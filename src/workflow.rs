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
}
impl From<AppError> for OperationFailure {
    fn from(error: AppError) -> Self {
        Self {
            error,
            rollback: None,
        }
    }
}
impl OperationFailure {
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
        OperationFailure {
            error,
            rollback: Some(rollback),
        }
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

fn in_transaction<C: SwitchControl + ?Sized>(
    control: &C,
    scope: &PreparedScope,
    args: &Args,
    execute: impl FnOnce(&mut SwitchTransaction<'_, C>) -> AppResult<VerificationReport>,
) -> Result<CommitReport, OperationFailure> {
    let mut transaction = SwitchTransaction::begin(control, &args.group, Cancellation::global())?;
    let result = (|| {
        let captured = &transaction.snapshot().state;
        if captured.identity != scope.identity {
            return Err(AppError::new(ErrorCode::IdentityMismatch, Stage::Switching));
        }
        if captured.profile != scope.profile || captured.members != scope.members {
            return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
        }
        let verification = execute(&mut transaction)?;
        transaction.commit(&verification)
    })();
    result.map_err(|error| OperationFailure {
        error,
        rollback: Some(transaction.rollback()),
    })
}

pub fn use_flow(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
    target: &str,
) -> Result<SelectionOutcome, OperationFailure> {
    let control = KernelSwitchControl { backend: be, api };
    let scope = prepare(&control, &args.group)?;
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
    let result = in_transaction(&control, &scope, args, |transaction| {
        let mut history =
            crate::history::HistorySession::new(be, scope.profile, &args.group, &args.url);
        transaction.select(target)?;
        let evidence = checker::verify_access_report_with_timeout(
            &be.proxy,
            &args.url,
            args.samples,
            args.verify_timeout_ms,
        );
        transaction.validate_current()?;
        history.record_verification(be, target, &evidence);
        Ok(evidence)
    })?;
    Ok(SelectionOutcome::Committed { result })
}

pub fn fix_flow(
    be: &Backend,
    api: &CoreApi,
    args: &Args,
) -> Result<CommitReport, OperationFailure> {
    use crate::diagnostics::Availability;
    if args.dry_run {
        return Err(AppError::new(ErrorCode::InvalidInput, Stage::Configuration).into());
    }
    let control = KernelSwitchControl { backend: be, api };
    let scope = prepare(&control, &args.group)?;
    let policy = nodeinfo::BillingPolicy::for_backend(be, &scope.proxies);
    policy.report();
    let candidates =
        nodeinfo::candidates_with_policy(&scope.proxies, &scope.members, args.max_rate, &policy);
    nodeinfo::report_candidates(&candidates, args.max_rate);
    if candidates.names.is_empty() || args.top == 0 {
        let mut error = AppError::new(ErrorCode::InvalidInput, Stage::Configuration);
        error.message = "没有符合条件的真实节点；未进行节点切换，请检查倍率限制或策略组".into();
        return Err(error.into());
    }
    if !checker::core_alive(&be.proxy) {
        return Err(AppError::new(ErrorCode::ClientUnavailable, Stage::Discovery).into());
    }
    in_transaction(&control, &scope, args, |transaction| {
        let mut history =
            crate::history::HistorySession::new(be, scope.profile, &args.group, &args.url);
        if args.manage_system_proxy {
            transaction.repair_proxy()?;
        }
        let scan = checker::scan_report(
            be,
            api,
            &candidates.names,
            &args.url,
            &args.probe,
            &args.group,
        );
        if let Some(error) = scan.controller_failure() {
            return Err(error.clone());
        }
        let reachable = scan.reachable();
        if reachable.is_empty() {
            return Err(scan
                .measurements
                .iter()
                .filter_map(|m| m.error.clone())
                .next()
                .unwrap_or_else(|| AppError::new(ErrorCode::ProbeFailed, Stage::Verification)));
        }
        crate::ui::info(&format!(
            "探测完成：可达 {}/{}，验证前 {} 个候选",
            reachable.len(),
            candidates.names.len(),
            args.top.min(reachable.len())
        ));
        let mut verified = Vec::new();
        let mut last_error = AppError::new(ErrorCode::ProbeFailed, Stage::Verification);
        for (_, name) in reachable.iter().take(args.top) {
            transaction.select(name)?;
            let evidence = checker::verify_access_report_with_timeout(
                &be.proxy,
                &args.url,
                args.samples,
                args.verify_timeout_ms,
            );
            transaction.validate_current()?;
            history.record_verification(be, name, &evidence);
            if evidence
                .error
                .as_ref()
                .is_some_and(|error| error.code == ErrorCode::Cancelled)
            {
                return Err(evidence.error.unwrap());
            }
            if evidence.availability == Availability::Failed {
                if let Some(error) = evidence.error {
                    last_error = error;
                }
                crate::ui::warn(&format!("{} · 验证未通过（{}）", name, last_error));
            } else {
                crate::ui::info(&format!(
                    "{} · {}（候选测试，尚未提交）",
                    name,
                    if evidence.availability == Availability::Webpage {
                        "网页验证通过"
                    } else {
                        "仅 IP 交叉检查通过"
                    }
                ));
                verified.push((name.clone(), evidence));
            }
            if verified.len() >= 5 {
                break;
            }
        }
        verified.sort_by(|a, b| {
            let grade = |e: &VerificationReport| {
                if e.availability == Availability::Webpage {
                    0
                } else {
                    1
                }
            };
            grade(&a.1)
                .cmp(&grade(&b.1))
                .then_with(|| {
                    a.1.mean_ms
                        .unwrap_or(f64::MAX)
                        .total_cmp(&b.1.mean_ms.unwrap_or(f64::MAX))
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        let Some((best, prior)) = verified.first() else {
            return Err(last_error);
        };
        transaction.select(best)?;
        let final_evidence = checker::verify_access_report_with_timeout(
            &be.proxy,
            &args.url,
            args.samples,
            args.verify_timeout_ms,
        );
        transaction.validate_current()?;
        history.record_verification(be, best, &final_evidence);
        if prior.availability == Availability::Webpage
            && final_evidence.availability != Availability::Webpage
        {
            return Err(final_evidence.error.unwrap_or_else(|| {
                AppError::new(ErrorCode::TransportUnknown, Stage::Verification)
            }));
        }
        Ok(final_evidence)
    })
}
