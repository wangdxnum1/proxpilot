//! Mutation workflows return evidence; presentation and process exit belong to the caller.
use crate::{
    appstate, checker,
    core_api::CoreApi,
    detect::Backend,
    diagnostics::{AppError, AppResult, ErrorCode, Stage, VerificationReport},
    mihomo, nodeinfo,
    operation::Cancellation,
    runtime_switch::KernelSwitchControl,
    switching::{CommitReport, RollbackReport, RollbackStatus, SwitchControl, SwitchTransaction},
    Args,
};
use std::{collections::HashMap, fmt};

#[derive(Clone, Debug, serde::Serialize)]
pub struct OperationFailure {
    pub error: AppError,
    pub rollback: Option<RollbackReport>,
    pub failures: Vec<crate::diagnostics::NodeFailure>,
}
impl From<AppError> for OperationFailure {
    fn from(error: AppError) -> Self {
        Self {
            error,
            rollback: None,
            failures: vec![],
        }
    }
}
impl OperationFailure {
    fn with_rollback(error: AppError, rollback: RollbackReport) -> Self {
        Self {
            error,
            rollback: Some(rollback),
            failures: vec![],
        }
    }
    pub fn exit_code(&self) -> i32 {
        if self.error.code == ErrorCode::Cancelled {
            130
        } else if self.rollback.as_ref().is_some_and(|r| {
            matches!(
                r.status,
                RollbackStatus::Failed | RollbackStatus::SkippedConflict
            )
        }) {
            1
        } else if self.error.stage == Stage::Verification {
            2
        } else {
            1
        }
    }
}
impl fmt::Display for OperationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)?;
        if let Some(rollback) = &self.rollback {
            let status = match rollback.status {
                RollbackStatus::Unchanged => "原状态未变化",
                RollbackStatus::Restored => "已恢复操作前的状态",
                RollbackStatus::SkippedConflict => "状态已被外部修改，跳过恢复并保留记录",
                RollbackStatus::Failed => "恢复失败，保留恢复记录",
            };
            write!(f, "；{status}")?;
            if let Some(error) = &rollback.error {
                write!(f, "（{error}）")?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SelectionOutcome {
    DryRun { group: String, target: String },
    Committed { result: CommitReport },
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    InsufficientBenefit,
    NoVerifiedAlternative,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct UnchangedReport {
    pub group: String,
    pub original: String,
    pub current: VerificationReport,
    pub current_score: crate::scoring::ScoreBreakdown,
    pub best: Option<crate::scoring::Candidate>,
    pub reason: KeepReason,
    pub rollback: RollbackReport,
    pub failures: Vec<crate::diagnostics::NodeFailure>,
    pub warnings: Vec<AppError>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OptimizationOutcome {
    Committed { result: CommitReport },
    Unchanged { result: UnchangedReport },
}
enum Decision {
    Commit(VerificationReport),
    Keep(UnchangedReport),
}

/// Independent proxy repair owns the same per-user lock as node transactions.
pub fn repair_system_proxy(control: &impl SwitchControl) -> Result<(), OperationFailure> {
    let _ownership = crate::operation::MutationLock::acquire()?;
    repair_proxy_owned(control, &Cancellation::global())
}

fn repair_proxy_owned(
    control: &impl SwitchControl,
    cancel: &Cancellation,
) -> Result<(), OperationFailure> {
    let unavailable = || AppError::new(ErrorCode::StateUnavailable, Stage::Switching);
    cancel.check()?;
    let before = control.proxy_snapshot()?.ok_or_else(unavailable)?;
    let target = control.target_proxy_snapshot()?.ok_or_else(unavailable)?;
    if before == target {
        return Ok(());
    }
    cancel.check()?;
    if control.proxy_snapshot()? != Some(before.clone()) {
        return Err(AppError::new(ErrorCode::SelectionConflict, Stage::Switching).into());
    }
    let result = control.enable_proxy().and_then(|()| {
        if control.proxy_snapshot()? == Some(target.clone()) {
            Ok(())
        } else {
            Err(unavailable())
        }
    });
    result.map_err(|error| {
        let rollback = match control.restore_proxy(&before, &target) {
            Ok(restored) => RollbackReport {
                status: if restored {
                    RollbackStatus::Restored
                } else {
                    RollbackStatus::SkippedConflict
                },
                error: None,
            },
            Err(error) => RollbackReport {
                status: RollbackStatus::Failed,
                error: Some(error),
            },
        };
        OperationFailure::with_rollback(error, rollback)
    })
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use crate::{switching::LiveState, sysproxy::ProxySnapshot};
    use std::cell::RefCell;

    struct Proxy {
        state: RefCell<ProxySnapshot>,
        fail_write: bool,
        external: bool,
    }
    fn before() -> ProxySnapshot {
        ProxySnapshot {
            enabled: Some(0),
            server: Some("old:80".into()),
        }
    }
    fn target() -> ProxySnapshot {
        ProxySnapshot {
            enabled: Some(1),
            server: Some("target:80".into()),
        }
    }
    impl SwitchControl for Proxy {
        fn state(&self, _: &str) -> AppResult<LiveState> {
            unreachable!()
        }
        fn select(&self, _: &str, _: &str) -> AppResult<()> {
            unreachable!()
        }
        fn proxy_snapshot(&self) -> AppResult<Option<ProxySnapshot>> {
            Ok(Some(self.state.borrow().clone()))
        }
        fn target_proxy_snapshot(&self) -> AppResult<Option<ProxySnapshot>> {
            Ok(Some(target()))
        }
        fn enable_proxy(&self) -> AppResult<()> {
            *self.state.borrow_mut() = target();
            if self.fail_write {
                self.state.borrow_mut().enabled = Some(0);
                if self.external {
                    self.state.borrow_mut().server = Some("external:80".into());
                }
                Err(AppError::new(ErrorCode::StateUnavailable, Stage::Switching))
            } else {
                Ok(())
            }
        }
        fn restore_proxy(&self, prior: &ProxySnapshot, written: &ProxySnapshot) -> AppResult<bool> {
            if self.state.borrow().server != written.server {
                return Ok(false);
            }
            *self.state.borrow_mut() = prior.clone();
            Ok(true)
        }
    }
    #[test]
    fn standalone_proxy_repair_reports_half_write_and_preserves_external_update() {
        for external in [false, true] {
            let proxy = Proxy {
                state: RefCell::new(before()),
                fail_write: true,
                external,
            };
            let failure = repair_proxy_owned(&proxy, &Cancellation::new()).unwrap_err();
            assert_eq!(
                failure.rollback.unwrap().status,
                if external {
                    RollbackStatus::SkippedConflict
                } else {
                    RollbackStatus::Restored
                }
            );
            assert_eq!(
                proxy.state.borrow().server.as_deref(),
                Some(if external { "external:80" } else { "old:80" })
            );
        }
        let proxy = Proxy {
            state: RefCell::new(before()),
            fail_write: false,
            external: false,
        };
        repair_proxy_owned(&proxy, &Cancellation::new()).unwrap();
        assert_eq!(*proxy.state.borrow(), target());
    }
}

struct PreparedScope {
    proxies: HashMap<String, mihomo::ProxyInfo>,
    profile: Option<i64>,
    identity: String,
    members: Vec<String>,
}

fn prepare(control: &KernelSwitchControl<'_>, group: &str) -> AppResult<PreparedScope> {
    let profile = appstate::current_profile_id(control.backend);
    let (proxies, pid) = mihomo::get_proxies_detailed(control.backend, control.api)?;
    if profile != appstate::current_profile_id(control.backend) {
        return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
    }
    let mut members = proxies
        .get(group)
        .and_then(|group| group.all.clone())
        .ok_or_else(|| AppError::new(ErrorCode::InvalidInput, Stage::Configuration))?;
    members.sort();
    members.dedup();
    Ok(PreparedScope {
        proxies,
        profile,
        identity: control.identity(pid),
        members,
    })
}

fn require_observed(
    current: &crate::switching::LiveState,
    expected: Option<&crate::switching::LiveState>,
) -> AppResult<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if current.identity != expected.identity {
        return Err(AppError::new(ErrorCode::IdentityMismatch, Stage::Switching));
    }
    let mut members = expected.members.clone();
    members.sort();
    members.dedup();
    if current.profile != expected.profile || current.members != members {
        return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
    }
    if current.selection != expected.selection {
        return Err(AppError::new(
            ErrorCode::SelectionConflict,
            Stage::Switching,
        ));
    }
    Ok(())
}
fn require_prepared_observed(
    scope: &PreparedScope,
    group: &str,
    expected: Option<&crate::switching::LiveState>,
) -> AppResult<()> {
    require_observed(
        &crate::switching::LiveState {
            identity: scope.identity.clone(),
            profile: scope.profile,
            members: scope.members.clone(),
            selection: scope.proxies.get(group).and_then(|group| group.now.clone()),
        },
        expected,
    )
}

fn in_transaction<C: SwitchControl + ?Sized>(
    control: &C,
    scope: &PreparedScope,
    args: &Args,
    execute: impl FnOnce(&mut SwitchTransaction<'_, C>) -> AppResult<VerificationReport>,
) -> Result<CommitReport, OperationFailure> {
    match in_transaction_deciding(control, scope, args, |transaction| {
        execute(transaction).map(Decision::Commit)
    })? {
        OptimizationOutcome::Committed { result } => Ok(result),
        OptimizationOutcome::Unchanged { .. } => {
            unreachable!("commit-only operation cannot request a keep decision")
        }
    }
}

fn in_transaction_deciding<C: SwitchControl + ?Sized>(
    control: &C,
    scope: &PreparedScope,
    args: &Args,
    execute: impl FnOnce(&mut SwitchTransaction<'_, C>) -> AppResult<Decision>,
) -> Result<OptimizationOutcome, OperationFailure> {
    let mut transaction = SwitchTransaction::begin(control, &args.group, Cancellation::global())?;
    let decision = (|| {
        let captured = &transaction.snapshot().state;
        if captured.identity != scope.identity {
            return Err(AppError::new(ErrorCode::IdentityMismatch, Stage::Switching));
        }
        if captured.profile != scope.profile || captured.members != scope.members {
            return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
        }
        execute(&mut transaction)
    })();
    match decision {
        Err(error) => Err(OperationFailure::with_rollback(
            error,
            transaction.rollback(),
        )),
        Ok(Decision::Commit(verification)) => match transaction.commit(&verification) {
            Ok(result) => Ok(OptimizationOutcome::Committed { result }),
            Err(error) => Err(OperationFailure::with_rollback(
                error,
                transaction.rollback(),
            )),
        },
        Ok(Decision::Keep(mut report)) => {
            let rollback = transaction.rollback();
            if matches!(
                rollback.status,
                RollbackStatus::Failed | RollbackStatus::SkippedConflict
            ) || rollback.error.is_some()
            {
                return Err(OperationFailure::with_rollback(
                    rollback.error.clone().unwrap_or_else(|| {
                        AppError::new(ErrorCode::RollbackFailed, Stage::Rollback)
                    }),
                    rollback,
                ));
            }
            report.rollback = rollback;
            Ok(OptimizationOutcome::Unchanged { result: report })
        }
    }
}

pub fn use_flow(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
    target: &str,
) -> Result<SelectionOutcome, OperationFailure> {
    use_flow_expected(be, api, args, target, None)
}
pub fn use_flow_expected(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
    target: &str,
    expected: Option<&crate::switching::LiveState>,
) -> Result<SelectionOutcome, OperationFailure> {
    let control = KernelSwitchControl { backend: be, api };
    let scope = prepare(&control, &args.group)?;
    require_prepared_observed(&scope, &args.group, expected)?;
    let real = scope
        .proxies
        .get(target)
        .is_some_and(|node| checker::is_real_node(node, target));
    if target.is_empty() || !real || !scope.members.iter().any(|name| name == target) {
        let mut error = AppError::new(ErrorCode::InvalidInput, Stage::Configuration);
        error.message = "指定节点不是该策略组中的真实节点；未切换".into();
        return Err(error.into());
    }
    let policy = nodeinfo::BillingPolicy::for_backend(be, &scope.proxies);
    policy.report();
    if !policy.permitted(target, args.max_rate) {
        let mut error = AppError::new(ErrorCode::InvalidInput, Stage::Configuration);
        error.message = "指定节点超过倍率上限或倍率未知；未切换".into();
        return Err(error.into());
    }
    if args.dry_run {
        return Ok(SelectionOutcome::DryRun {
            group: args.group.clone(),
            target: target.into(),
        });
    }
    let mut scored_candidate = None;
    let mut history_warning = None;
    let mut result = in_transaction(&control, &scope, args, |transaction| {
        require_observed(&transaction.snapshot().state, expected)?;
        let mut history =
            crate::history::HistorySession::new(be, scope.profile, &args.group, &args.url);
        let prior = history.prior(target);
        transaction.select(target)?;
        let evidence = checker::verify_access_report_with_timeout(
            &be.proxy,
            &args.url,
            args.samples,
            args.verify_timeout_ms,
        );
        transaction.validate_current()?;
        scored_candidate = crate::scoring::Candidate::verified(
            target.into(),
            &prior,
            evidence.clone(),
            policy.resolve(target).map(|(rate, _)| rate),
        );
        history.record_verification(be, target, &evidence);
        history_warning = history.warning;
        Ok(evidence)
    })?;
    if let Some(candidate) = scored_candidate {
        result.score = Some(candidate.score.clone());
        result.candidates.push(candidate);
    }
    if let Some(error) = history_warning {
        result.warnings.push(error);
    }
    Ok(SelectionOutcome::Committed { result })
}

struct MeasuredCandidate {
    result: crate::scoring::Candidate,
    prior: Vec<crate::history::HistoryEntry>,
}

fn measurement_error(report: &VerificationReport) -> AppError {
    report
        .error
        .clone()
        .unwrap_or_else(|| AppError::new(ErrorCode::TransportUnknown, Stage::Verification))
}

fn measure_node<C: SwitchControl + ?Sized>(
    transaction: &mut SwitchTransaction<'_, C>,
    be: &Backend,
    args: &Args,
    history: &mut crate::history::HistorySession,
    node: &str,
    failures: &mut Vec<crate::diagnostics::NodeFailure>,
) -> AppResult<VerificationReport> {
    let report = checker::verify_access_report_with_timeout(
        &be.proxy,
        &args.url,
        args.samples,
        args.verify_timeout_ms,
    );
    transaction.validate_current()?;
    if let Some(error) = report.error.as_ref().filter(|error| {
        matches!(
            error.code,
            ErrorCode::Cancelled
                | ErrorCode::InvalidInput
                | ErrorCode::StateUnavailable
                | ErrorCode::ControllerAuth
                | ErrorCode::IdentityMismatch
        )
    }) {
        return Err(error.clone());
    }
    history.record_verification(be, node, &report);
    if report.availability == crate::diagnostics::Availability::Failed {
        failures.push(crate::diagnostics::NodeFailure {
            node: node.into(),
            phase: Stage::Verification,
            error: measurement_error(&report),
        });
    }
    Ok(report)
}

fn selection_candidates(
    scope: &PreparedScope,
    args: &Args,
    policy: &nodeinfo::BillingPolicy,
    allowed: Option<&[String]>,
    skip: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut candidates =
        nodeinfo::candidates_with_policy(&scope.proxies, &scope.members, args.max_rate, policy);
    nodeinfo::report_candidates(&candidates, args.max_rate);
    candidates.names.retain(|name| {
        allowed.is_none_or(|allowed| allowed.contains(name)) && skip != Some(name.as_str())
    });
    if candidates.names.is_empty() || args.top == 0 {
        let mut error = AppError::new(ErrorCode::InvalidInput, Stage::Configuration);
        error.message = "没有符合条件且不在冷却中的真实候选；未进行节点切换".into();
        return Err(error);
    }
    Ok(candidates.names)
}

#[allow(clippy::too_many_arguments)]
fn verify_candidates<C: SwitchControl + ?Sized>(
    transaction: &mut SwitchTransaction<'_, C>,
    be: &Backend,
    api: &CoreApi,
    args: &Args,
    policy: &nodeinfo::BillingPolicy,
    names: &[String],
    history: &mut crate::history::HistorySession,
    failures: &mut Vec<crate::diagnostics::NodeFailure>,
    warnings: &mut Vec<AppError>,
) -> AppResult<(Vec<MeasuredCandidate>, AppError)> {
    let scan = checker::scan_report(be, api, names, &args.url, &args.probe, &args.group);
    if let Some(error) = scan.controller_failure() {
        return Err(error.clone());
    }
    if scan.cancelled {
        return Err(AppError::new(ErrorCode::Cancelled, Stage::Probe));
    }
    if let Some(error) = scan.history_warning.clone() {
        warnings.push(error);
    }
    for measurement in &scan.measurements {
        if let Some(error) = &measurement.error {
            failures.push(crate::diagnostics::NodeFailure {
                node: measurement.node.clone(),
                phase: Stage::Probe,
                error: error.clone(),
            });
        }
    }
    let reachable = scan.reachable();
    crate::ui::info(&format!(
        "探测完成：可达 {}/{}，验证前 {} 个候选",
        reachable.len(),
        names.len(),
        args.top.min(reachable.len())
    ));
    let mut verified = Vec::new();
    let mut last_error = AppError::new(ErrorCode::ProbeFailed, Stage::Verification);
    for (_, name) in reachable.iter().take(args.top) {
        let prior = history.prior(name);
        transaction.select(name)?;
        let report = measure_node(transaction, be, args, history, name, failures)?;
        if report.availability == crate::diagnostics::Availability::Failed {
            last_error = measurement_error(&report);
            crate::ui::warn(&format!("{} · 验证未通过（{}）", name, last_error));
        } else if let Some(candidate) = crate::scoring::Candidate::verified(
            name.clone(),
            &prior,
            report,
            policy.resolve(name).map(|(rate, _)| rate),
        ) {
            crate::ui::info(&format!(
                "{} · {}（候选测试，尚未提交）",
                name,
                if candidate.verification.availability == crate::diagnostics::Availability::Webpage
                {
                    "网页验证通过"
                } else {
                    "仅 IP 交叉检查通过"
                }
            ));
            verified.push(MeasuredCandidate {
                result: candidate,
                prior,
            });
        }
        if verified.len() >= 5 {
            break;
        }
    }
    verified.sort_by(|a, b| crate::scoring::compare_candidates(&a.result, &b.result));
    Ok((verified, last_error))
}

fn final_candidate<C: SwitchControl + ?Sized>(
    transaction: &mut SwitchTransaction<'_, C>,
    be: &Backend,
    args: &Args,
    policy: &nodeinfo::BillingPolicy,
    best: &MeasuredCandidate,
    history: &mut crate::history::HistorySession,
    failures: &mut Vec<crate::diagnostics::NodeFailure>,
) -> AppResult<crate::scoring::Candidate> {
    transaction.select(&best.result.node)?;
    let report = measure_node(transaction, be, args, history, &best.result.node, failures)?;
    if report.availability == crate::diagnostics::Availability::Failed
        || (best.result.verification.availability == crate::diagnostics::Availability::Webpage
            && report.availability != crate::diagnostics::Availability::Webpage)
    {
        return Err(measurement_error(&report));
    }
    crate::scoring::Candidate::verified(
        best.result.node.clone(),
        &best.prior,
        report,
        policy.resolve(&best.result.node).map(|(rate, _)| rate),
    )
    .ok_or_else(|| AppError::new(ErrorCode::InvalidResponse, Stage::Verification))
}

pub fn fix_flow(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
) -> Result<CommitReport, OperationFailure> {
    fix_flow_allowed(be, api, args, None, None)
}

pub fn fix_flow_allowed(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
    allowed: Option<&[String]>,
    expected: Option<&crate::switching::LiveState>,
) -> Result<CommitReport, OperationFailure> {
    if args.dry_run {
        return Err(AppError::new(ErrorCode::InvalidInput, Stage::Configuration).into());
    }
    let control = KernelSwitchControl { backend: be, api };
    let scope = prepare(&control, &args.group)?;
    require_prepared_observed(&scope, &args.group, expected)?;
    let policy = nodeinfo::BillingPolicy::for_backend(be, &scope.proxies);
    policy.report();
    let names = selection_candidates(&scope, args, &policy, allowed, None)?;
    if !checker::core_alive(&be.proxy) {
        return Err(AppError::new(ErrorCode::ClientUnavailable, Stage::Discovery).into());
    }
    let mut history =
        crate::history::HistorySession::new(be, scope.profile, &args.group, &args.url);
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut ranking = Vec::new();
    let mut chosen_score = None;
    let mut result = in_transaction(&control, &scope, args, |transaction| {
        require_observed(&transaction.snapshot().state, expected)?;
        if args.manage_system_proxy {
            transaction.repair_proxy()?;
        }
        let (verified, last_error) = verify_candidates(
            transaction,
            be,
            api,
            args,
            &policy,
            &names,
            &mut history,
            &mut failures,
            &mut warnings,
        )?;
        let Some(best) = verified.first() else {
            return Err(last_error);
        };
        let final_result = final_candidate(
            transaction,
            be,
            args,
            &policy,
            best,
            &mut history,
            &mut failures,
        )?;
        chosen_score = Some(final_result.score.clone());
        ranking = verified.into_iter().map(|entry| entry.result).collect();
        Ok(final_result.verification)
    });
    if let Some(error) = history.warning {
        warnings.push(error);
    }
    match &mut result {
        Ok(report) => {
            report.score = chosen_score;
            report.candidates = ranking;
            report.failures = failures;
            report.warnings.extend(warnings);
        }
        Err(error) => error.failures = failures,
    }
    result
}

pub fn optimize_flow(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
    allowed: &[String],
    expected: &crate::switching::LiveState,
) -> Result<OptimizationOutcome, OperationFailure> {
    use crate::diagnostics::Availability;
    if args.dry_run {
        return Err(AppError::new(ErrorCode::InvalidInput, Stage::Configuration).into());
    }
    let control = KernelSwitchControl { backend: be, api };
    let scope = prepare(&control, &args.group)?;
    require_prepared_observed(&scope, &args.group, Some(expected))?;
    let policy = nodeinfo::BillingPolicy::for_backend(be, &scope.proxies);
    policy.report();
    // Filter before any node traffic. Original is measured as the comparison, never as a replacement candidate.
    let selected = scope
        .proxies
        .get(&args.group)
        .and_then(|group| group.now.as_deref());
    let names = selection_candidates(&scope, args, &policy, Some(allowed), selected)?;
    if !checker::core_alive(&be.proxy) {
        return Err(AppError::new(ErrorCode::ClientUnavailable, Stage::Discovery).into());
    }
    let mut history =
        crate::history::HistorySession::new(be, scope.profile, &args.group, &args.url);
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut ranking = Vec::new();
    let mut chosen_score = None;
    let mut result = in_transaction_deciding(&control, &scope, args, |transaction| {
        require_observed(&transaction.snapshot().state, Some(expected))?;
        if args.manage_system_proxy {
            transaction.repair_proxy()?;
        }
        let original = transaction
            .snapshot()
            .state
            .selection
            .clone()
            .ok_or_else(|| AppError::new(ErrorCode::StateUnavailable, Stage::Switching))?;
        let prior = history.prior(&original);
        let current = measure_node(
            transaction,
            be,
            args,
            &mut history,
            &original,
            &mut failures,
        )?;
        let current_score = crate::scoring::score(
            &prior,
            &current,
            policy.resolve(&original).map(|(rate, _)| rate),
        );
        let (verified, last_error) = verify_candidates(
            transaction,
            be,
            api,
            args,
            &policy,
            &names,
            &mut history,
            &mut failures,
            &mut warnings,
        )?;
        let keep = |reason, best| UnchangedReport {
            group: args.group.clone(),
            original: original.clone(),
            current: current.clone(),
            current_score: current_score.clone(),
            best,
            reason,
            rollback: RollbackReport {
                status: RollbackStatus::Unchanged,
                error: None,
            },
            failures: vec![],
            warnings: vec![],
        };
        let Some(best) = verified.first() else {
            return if current.availability == Availability::Failed {
                Err(last_error)
            } else {
                Ok(Decision::Keep(keep(
                    KeepReason::NoVerifiedAlternative,
                    None,
                )))
            };
        };
        ranking = verified.iter().map(|entry| entry.result.clone()).collect();
        if best.result.node == original
            || !crate::watch_policy::has_benefit(&current, &current_score, &best.result)
        {
            return Ok(Decision::Keep(keep(
                KeepReason::InsufficientBenefit,
                Some(best.result.clone()),
            )));
        }
        let final_result = final_candidate(
            transaction,
            be,
            args,
            &policy,
            best,
            &mut history,
            &mut failures,
        )?;
        if !crate::watch_policy::has_benefit(&current, &current_score, &final_result) {
            return Ok(Decision::Keep(keep(
                KeepReason::InsufficientBenefit,
                Some(final_result),
            )));
        }
        chosen_score = Some(final_result.score.clone());
        Ok(Decision::Commit(final_result.verification))
    });
    if let Some(error) = history.warning {
        warnings.push(error);
    }
    match &mut result {
        Ok(OptimizationOutcome::Committed { result }) => {
            result.score = chosen_score;
            result.candidates = ranking;
            result.failures = failures;
            result.warnings.extend(warnings);
        }
        Ok(OptimizationOutcome::Unchanged { result }) => {
            result.failures = failures;
            result.warnings.extend(warnings);
        }
        Err(error) => error.failures = failures,
    }
    result
}
