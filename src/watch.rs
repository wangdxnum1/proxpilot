//! A watch iteration emits typed events; the same execution can feed text or NDJSON.
use crate::{
    appstate, checker, clients,
    core_api::CoreApi,
    detect::Backend,
    diagnostics::{AppError, Availability, ErrorCode, NodeFailure, Stage, VerificationReport},
    mihomo, nodeinfo,
    operation::Cancellation,
    switching::LiveState,
    sysproxy,
    watch_policy::{Action, Health, Observed, WatchPolicy},
    workflow, Args,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Event {
    Started {
        client: String,
        interval_secs: u64,
        optimize: bool,
        reopt_secs: u64,
        settings: crate::watch_policy::WatchSettings,
    },
    Waiting {
        cycle: u64,
        error: AppError,
    },
    Observed {
        cycle: u64,
        change: Observed,
        group: String,
        selection: String,
        manual_hold_remaining_secs: u64,
    },
    Health {
        cycle: u64,
        verification: VerificationReport,
        manual_hold_remaining_secs: u64,
    },
    Recheck {
        cycle: u64,
        after_secs: u64,
        error: AppError,
    },
    CooldownWait {
        cycle: u64,
        next_retry_after_secs: Option<u64>,
    },
    Committed {
        cycle: u64,
        result: crate::switching::CommitReport,
    },
    Unchanged {
        cycle: u64,
        result: workflow::UnchangedReport,
    },
    OperationFailed {
        cycle: u64,
        failure: workflow::OperationFailure,
    },
    Stopped {
        exit_code: i32,
        cancelled: bool,
    },
}
fn seconds(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
}
fn scope_key(state: &LiveState, group: &str) -> String {
    let mut members = state.members.clone();
    members.sort();
    members.dedup();
    crate::history::fingerprint(
        &serde_json::json!([state.identity, state.profile, group, members]).to_string(),
    )
}
fn record_failures(policy: &mut WatchPolicy, failures: &[NodeFailure], now: Duration) {
    for failure in failures
        .iter()
        .filter(|failure| crate::watch_policy::cooldown_failure(failure))
    {
        policy.failed(&failure.node, now);
    }
}

pub fn run(
    initial: &Backend,
    api: &CoreApi,
    args: &Args,
    emit: &mut dyn FnMut(Event) -> bool,
) -> i32 {
    if args.dry_run {
        let _ = emit(Event::Waiting {
            cycle: 0,
            error: AppError::new(ErrorCode::InvalidInput, Stage::Configuration),
        });
        let _ = emit(Event::Stopped {
            exit_code: 1,
            cancelled: false,
        });
        return 1;
    }
    let code = if !emit(Event::Started {
        client: initial.client.clone(),
        interval_secs: args.interval,
        optimize: args.optimize,
        reopt_secs: args.reopt,
        settings: args.watch.clone(),
    }) {
        0
    } else {
        run_loop(initial, api, args, emit)
    };
    let _ = emit(Event::Stopped {
        exit_code: code,
        cancelled: code == 130,
    });
    code
}

fn run_loop(
    initial: &Backend,
    api: &CoreApi,
    args: &Args,
    emit: &mut dyn FnMut(Event) -> bool,
) -> i32 {
    let cancel = Cancellation::global();
    let clock = Instant::now();
    let mut policy = WatchPolicy::new(args.watch.clone());
    let mut current = initial.clone();
    let mut cycle = 0;
    'cycles: loop {
        if cancel.is_cancelled() {
            return 130;
        }
        cycle += 1;
        match clients::refresh(&current, args, api) {
            Ok(next) => current = next,
            Err(_) => {
                policy.environment_wait();
                if !emit(Event::Waiting {
                    cycle,
                    error: AppError::new(ErrorCode::ClientUnavailable, Stage::Discovery),
                }) {
                    return 0;
                }
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue;
            }
        }
        let be = &current;
        let proxies = match mihomo::get_proxies_detailed(be, api) {
            Ok((proxies, _)) => proxies,
            Err(error) => {
                policy.environment_wait();
                if !emit(Event::Waiting { cycle, error }) {
                    return 0;
                }
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue;
            }
        };
        let saved = if args.group.is_empty() {
            appstate::current_group(be)
        } else {
            None
        };
        let resolved = match crate::group_args_from_proxies(&proxies, args, saved.as_deref()) {
            Ok(resolved) => resolved,
            Err(message) => {
                let mut error = AppError::new(ErrorCode::InvalidInput, Stage::Configuration);
                error.message = message;
                if !emit(Event::Waiting { cycle, error }) {
                    return 0;
                }
                if cycle == 1 {
                    return 1;
                }
                policy.environment_wait();
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue;
            }
        };
        let args = &resolved;
        if !checker::core_alive(&be.proxy) {
            policy.environment_wait();
            if !emit(Event::Waiting {
                cycle,
                error: AppError::new(ErrorCode::ClientUnavailable, Stage::Discovery),
            }) {
                return 0;
            }
            if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                return 130;
            }
            continue;
        }
        if args.manage_system_proxy && !sysproxy::matches(&be.proxy) {
            let control = crate::runtime_switch::KernelSwitchControl { backend: be, api };
            if let Err(failure) = workflow::repair_system_proxy(&control) {
                let cancelled = failure.error.code == ErrorCode::Cancelled;
                if !emit(Event::OperationFailed { cycle, failure }) {
                    return 0;
                }
                if cancelled {
                    return 130;
                }
                policy.environment_wait();
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue;
            }
        }
        let (state, verification, broken) = loop {
            let check = checker::verify_current_report(
                be,
                api,
                &args.group,
                &args.url,
                1,
                args.verify_timeout_ms,
            );
            if cancel.is_cancelled()
                || check
                    .verification
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == ErrorCode::Cancelled)
            {
                return 130;
            }
            if let Some(error) = check.controller_error {
                policy.environment_wait();
                if !emit(Event::Waiting { cycle, error }) {
                    return 0;
                }
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue 'cycles;
            }
            let Some(state) = check.state else {
                policy.environment_wait();
                if !emit(Event::Waiting {
                    cycle,
                    error: AppError::new(ErrorCode::ProfileChanged, Stage::Switching),
                }) {
                    return 0;
                }
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue 'cycles;
            };
            let Some(selected) = state.selection.as_deref() else {
                policy.environment_wait();
                if !emit(Event::Waiting {
                    cycle,
                    error: AppError::new(ErrorCode::StateUnavailable, Stage::Controller),
                }) {
                    return 0;
                }
                if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                    return 130;
                }
                continue 'cycles;
            };
            let now = clock.elapsed();
            let observed = policy.observe(&scope_key(&state, &args.group), selected, now);
            if !emit(Event::Observed {
                cycle,
                change: observed,
                group: args.group.clone(),
                selection: selected.into(),
                manual_hold_remaining_secs: seconds(policy.manual_remaining(now)),
            }) {
                return 0;
            }
            if !check.stable || observed == Observed::ScopeChange {
                policy.environment_wait();
                if cancel.wait(Duration::from_secs(1)).is_err() {
                    return 130;
                }
                continue 'cycles;
            }
            let failed = check.verification.availability == Availability::Failed;
            if failed {
                let error = check.verification.error.clone().unwrap_or_else(|| {
                    AppError::new(ErrorCode::TransportUnknown, Stage::Verification)
                });
                let failure = NodeFailure {
                    node: selected.into(),
                    phase: Stage::Verification,
                    error: error.clone(),
                };
                if !crate::watch_policy::cooldown_failure(&failure) {
                    let invalid = error.code == ErrorCode::InvalidInput;
                    policy.environment_wait();
                    if !emit(Event::Waiting { cycle, error }) {
                        return 0;
                    }
                    if invalid {
                        return 1;
                    }
                    if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                        return 130;
                    }
                    continue 'cycles;
                }
            }
            match policy.health(failed, now) {
                Health::Recheck(after) => {
                    let error = check.verification.error.unwrap_or_else(|| {
                        AppError::new(ErrorCode::TransportUnknown, Stage::Verification)
                    });
                    if !emit(Event::Recheck {
                        cycle,
                        after_secs: seconds(after),
                        error,
                    }) {
                        return 0;
                    }
                    if cancel.wait(after).is_err() {
                        return 130;
                    }
                }
                Health::Healthy => break (state, check.verification, false),
                Health::ConfirmedBroken => break (state, check.verification, true),
            }
        };
        if !emit(Event::Health {
            cycle,
            verification: verification.clone(),
            manual_hold_remaining_secs: seconds(policy.manual_remaining(clock.elapsed())),
        }) {
            return 0;
        }
        let action = policy.action(clock.elapsed(), args.optimize, args.reopt, broken);
        if action == Action::Wait {
            if cancel.wait(Duration::from_secs(args.interval)).is_err() {
                return 130;
            }
            continue;
        }
        let selected = state.selection.clone().unwrap_or_default();
        if broken {
            policy.failed(&selected, clock.elapsed());
        }
        // Fresh node membership and billing are checked again inside the operation.
        let billing = nodeinfo::BillingPolicy::for_backend(be, &proxies);
        let mut candidates =
            nodeinfo::candidates_with_policy(&proxies, &state.members, args.max_rate, &billing)
                .names;
        if action == Action::Optimize {
            candidates.retain(|node| node != &selected);
        }
        let eligibility = policy.eligible(&candidates, clock.elapsed());
        if eligibility.names.is_empty() {
            if !emit(Event::CooldownWait {
                cycle,
                next_retry_after_secs: eligibility.next_retry_after.map(seconds),
            }) {
                return 0;
            }
            if eligibility.next_retry_after.is_none() && action == Action::Optimize {
                policy.optimization_finished(clock.elapsed());
            }
            policy.environment_wait();
            let wait = eligibility
                .next_retry_after
                .map(|duration| duration.min(Duration::from_secs(args.interval)))
                .unwrap_or(Duration::from_secs(args.interval));
            if cancel.wait(wait).is_err() {
                return 130;
            }
            continue;
        }
        let outcome = if action == Action::Optimize {
            workflow::optimize_flow(be, api, args, &eligibility.names, &state)
        } else {
            workflow::fix_flow_allowed(be, api, args, Some(&eligibility.names), Some(&state))
                .map(|result| workflow::OptimizationOutcome::Committed { result })
        };
        let now = clock.elapsed();
        match outcome {
            Ok(workflow::OptimizationOutcome::Committed { result }) => {
                record_failures(&mut policy, &result.failures, now);
                policy.committed(&result.original, &result.selected, now);
                if action == Action::Optimize {
                    policy.optimization_finished(now);
                }
                if !emit(Event::Committed { cycle, result }) {
                    return 0;
                }
            }
            Ok(workflow::OptimizationOutcome::Unchanged { result }) => {
                record_failures(&mut policy, &result.failures, now);
                policy.committed(&result.original, &result.original, now);
                policy.optimization_finished(now);
                if !emit(Event::Unchanged { cycle, result }) {
                    return 0;
                }
            }
            Err(failure) => {
                record_failures(&mut policy, &failure.failures, now);
                let cancelled = failure.error.code == ErrorCode::Cancelled;
                if action == Action::Optimize && !failure.failures.is_empty() {
                    policy.optimization_finished(now);
                }
                policy.environment_wait();
                if !emit(Event::OperationFailed { cycle, failure }) {
                    return 0;
                }
                if cancelled {
                    return 130;
                }
            }
        }
        if cancel.wait(Duration::from_secs(args.interval)).is_err() {
            return 130;
        }
    }
}

pub fn render_text(event: Event) {
    use crate::ui;
    match event {
        Event::Started{interval_secs,optimize,reopt_secs,settings,..}=>{
            ui::info(&format!("守护模式：每 {interval_secs} 秒检查；{}；Ctrl+C 退出",if optimize{format!("主动优化周期 {reopt_secs} 秒")}else{"仅确认故障后修复".into()}));
            if optimize {ui::dim(&format!("最短正式切换 {}s · 失败冷却 {}s · 手动选择保护 {}s；同等级优化需参考分+5且提速≥20%与100ms",settings.min_switch_interval_secs,settings.failure_cooldown_secs,settings.manual_hold_secs));}
        }
        Event::Waiting{cycle,error}=>ui::warn(&format!("[周期 {cycle}] 等待目标环境恢复：{error}；可用 clients 查看控制接口状态")),
        Event::Observed{change:Observed::ManualChange,selection,manual_hold_remaining_secs,..}=>ui::info(&format!("检测到操作外选择 → {selection}；暂停主动优化 {manual_hold_remaining_secs}s，明确故障仍可修复")),
        Event::Observed{change:Observed::ScopeChange,..}=>ui::info("客户端实例、订阅或策略组已变化；已重置旧冷却并保护新的选择"),
        Event::Observed{..}=>{},
        Event::Health{cycle,verification,manual_hold_remaining_secs}=>{
            let state=match verification.availability{Availability::Webpage=>"网页验证通过",Availability::IpOnly=>"仅 IP 交叉检查通过，浏览器仍需确认",Availability::Failed=>"已确认当前出口验证失败"};
            ui::dim(&format!("[周期 {cycle}] {state}{}",if manual_hold_remaining_secs>0{format!(" · 手动保护剩余 {manual_hold_remaining_secs}s")}else{String::new()}));
        }
        Event::Recheck{cycle,after_secs,error}=>ui::warn(&format!("[周期 {cycle}] {error}；{after_secs}s 后复测确认")),
        Event::CooldownWait{next_retry_after_secs:Some(seconds),..}=>ui::dim(&format!("全部候选仍在冷却，约 {seconds}s 后可再次尝试；保持现状")),
        Event::CooldownWait{next_retry_after_secs:None,..}=>ui::dim("没有符合倍率限制的替代候选，保持现状"),
        Event::Committed{result,..}=>crate::print_commit(&result),
        Event::Unchanged{result,..}=>{
            ui::info(&format!("{}，{}「{}」",match result.reason{workflow::KeepReason::InsufficientBenefit=>"优化收益不足",workflow::KeepReason::NoVerifiedAlternative=>"没有验证通过的替代节点"},if result.rollback.status==crate::switching::RollbackStatus::Restored{"已恢复原选择"}else{"保持原选择"},result.original));
            for warning in result.warnings {ui::warn(&warning.to_string());}
        }
        Event::OperationFailed{failure,..}=>ui::warn(&failure.to_string()),
        Event::Stopped{cancelled:true,..}=>ui::dim("守护已取消"),
        Event::Stopped{exit_code:1,..}=>ui::warn("守护已停止，请先处理环境或参数问题"),
        Event::Stopped{..}=>{},
    }
}
