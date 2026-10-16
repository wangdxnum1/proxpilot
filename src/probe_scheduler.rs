//! Bounded windows with cancellation and retries counted separately from unique nodes.
use crate::{
    diagnostics::{AppError, AppResult, ErrorCode},
    operation::Cancellation,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
use std::time::Duration;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeOptions {
    pub concurrency: Option<usize>,
    pub timeout_ms: u32,
    pub retries: u8,
}
impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            concurrency: None,
            timeout_ms: 4000,
            retries: 1,
        }
    }
}
impl ProbeOptions {
    pub fn limit(&self, cpu: usize, count: usize) -> usize {
        self.concurrency
            .unwrap_or_else(|| cpu.max(1).saturating_mul(2).min(32))
            .min(count)
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeMeasurement {
    pub node: String,
    pub delay_ms: Option<i64>,
    pub error: Option<AppError>,
    pub attempts: u8,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeRun {
    pub measurements: Vec<ProbeMeasurement>,
    pub cancelled: bool,
    pub retries: usize,
    pub peak_concurrency: usize,
    pub concurrency_limit: usize,
    pub windows: Vec<usize>,
    pub controller_error: Option<AppError>,
    pub history_warning: Option<AppError>,
}
fn transient(error: &AppError) -> bool {
    error.retryable
        && matches!(
            error.code,
            ErrorCode::Timeout
                | ErrorCode::ControllerTimeout
                | ErrorCode::Connect
                | ErrorCode::ProxyConnect
                | ErrorCode::Dns
                | ErrorCode::ResponseTransfer
                | ErrorCode::TransportUnknown
        )
}
pub fn run(
    nodes: &[String],
    options: &ProbeOptions,
    cpu: usize,
    cancel: &Cancellation,
    probe: impl Fn(&str, u32) -> AppResult<i64> + Sync,
) -> ProbeRun {
    let limit = options.limit(cpu, nodes.len());
    let mut concurrency = if options.concurrency.is_some() {
        limit
    } else {
        limit.min(8)
    };
    let retries = AtomicUsize::new(0);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let fatal = AtomicBool::new(false);
    let controller_error = Mutex::new(None);
    let mut measurements = Vec::new();
    let mut windows = Vec::new();
    let mut cursor = 0;
    let mut clean_windows = 0;
    while cursor < nodes.len()
        && concurrency > 0
        && !cancel.is_cancelled()
        && !fatal.load(Ordering::Acquire)
    {
        let end = (cursor + concurrency).min(nodes.len());
        windows.push(end - cursor);
        let batch = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for name in &nodes[cursor..end] {
                if cancel.is_cancelled() || fatal.load(Ordering::Acquire) {
                    break;
                }
                let (probe, batch, retries, active, peak, fatal, controller_error) = (
                    &probe,
                    &batch,
                    &retries,
                    &active,
                    &peak,
                    &fatal,
                    &controller_error,
                );
                scope.spawn(move || {
                    let mut attempts = 0;
                    let mut had_transient = false;
                    let outcome = loop {
                        if let Err(error) = cancel.check() {
                            break Err(error);
                        }
                        if fatal.load(Ordering::Acquire) {
                            return;
                        }
                        if attempts > 0 {
                            retries.fetch_add(1, Ordering::Relaxed);
                        }
                        attempts += 1;
                        let in_flight = active.fetch_add(1, Ordering::AcqRel) + 1;
                        peak.fetch_max(in_flight, Ordering::AcqRel);
                        let result = probe(name, options.timeout_ms);
                        active.fetch_sub(1, Ordering::AcqRel);
                        let Err(error) = &result else {
                            break result;
                        };
                        if matches!(
                            error.code,
                            ErrorCode::ControllerAuth | ErrorCode::IdentityMismatch
                        ) {
                            fatal.store(true, Ordering::Release);
                            let mut stored = controller_error.lock().unwrap();
                            if stored.is_none() {
                                *stored = Some(error.clone());
                            }
                            break result;
                        }
                        had_transient |= transient(error);
                        if !transient(error) || attempts > options.retries {
                            break result;
                        }
                        if let Err(error) =
                            cancel.wait(Duration::from_millis(200u64 << (attempts - 1)))
                        {
                            break Err(error);
                        }
                        if fatal.load(Ordering::Acquire) {
                            break result;
                        }
                    };
                    let (delay_ms, error) = match outcome {
                        Ok(delay) => (Some(delay), None),
                        Err(error) => (None, Some(error)),
                    };
                    batch.lock().unwrap().push((
                        ProbeMeasurement {
                            node: name.clone(),
                            delay_ms,
                            error,
                            attempts,
                        },
                        had_transient,
                    ));
                });
            }
        });
        let batch = batch.into_inner().unwrap();
        if options.concurrency.is_none()
            && end - cursor == concurrency
            && batch.len() == concurrency
        {
            let failed = batch.iter().filter(|(_, transient)| *transient).count();
            if failed * 2 >= concurrency {
                concurrency = (concurrency / 2).max(1);
                clean_windows = 0;
            } else if failed == 0 {
                clean_windows += 1;
                if clean_windows == 2 {
                    concurrency = (concurrency + 2).min(limit);
                    clean_windows = 0;
                }
            } else {
                clean_windows = 0;
            }
        }
        measurements.extend(batch.into_iter().map(|(measurement, _)| measurement));
        cursor = end;
    }
    measurements.sort_by(|a, b| a.node.cmp(&b.node));
    ProbeRun {
        measurements,
        cancelled: cancel.is_cancelled(),
        retries: retries.into_inner(),
        peak_concurrency: peak.into_inner(),
        concurrency_limit: limit,
        windows,
        controller_error: controller_error.into_inner().unwrap(),
        history_warning: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{AppError, ErrorCode, Stage};
    use crate::operation::Cancellation;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|n| n.to_string()).collect()
    }
    #[test]
    fn limits_respect_cpu_global_cap_explicit_override_and_empty_input() {
        let auto = ProbeOptions::default();
        assert_eq!(auto.limit(1, 500), 2);
        assert_eq!(auto.limit(100, 500), 32);
        assert_eq!(auto.limit(100, 3), 3);
        assert_eq!(auto.limit(100, 0), 0);
        assert_eq!(
            ProbeOptions {
                concurrency: Some(256),
                ..Default::default()
            }
            .limit(1, 500),
            256
        );
        assert_eq!(
            run(&[], &auto, 1, &Cancellation::new(), |_, _| panic!(
                "empty input cannot probe"
            ))
            .peak_concurrency,
            0
        );
    }
    #[test]
    fn fatal_peer_during_backoff_does_not_count_an_unissued_retry() {
        let barrier = std::sync::Barrier::new(2);
        let report = run(
            &names(2),
            &ProbeOptions {
                concurrency: Some(2),
                retries: 1,
                ..Default::default()
            },
            2,
            &Cancellation::new(),
            |name, _| {
                barrier.wait();
                if name == "0" {
                    Err(AppError::new(ErrorCode::Timeout, Stage::Probe))
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    Err(AppError::controller_status(401))
                }
            },
        );
        assert_eq!(report.retries, 0);
    }
    #[test]
    fn semantic_http_and_tls_failures_are_not_retried() {
        for error in [
            AppError::website_status(429),
            AppError::controller_status(503),
            AppError::new(ErrorCode::InvalidInput, Stage::Probe),
            AppError::new(ErrorCode::Tls, Stage::Probe),
            AppError::new(ErrorCode::ProbeFailed, Stage::Probe),
        ] {
            let report = run(
                &names(1),
                &ProbeOptions {
                    retries: 3,
                    ..Default::default()
                },
                2,
                &Cancellation::new(),
                |_, _| Err(error.clone()),
            );
            assert_eq!(report.retries, 0);
            assert_eq!(report.measurements[0].attempts, 1);
        }
    }
    #[test]
    fn automatic_windows_shrink_grow_and_ignore_partial_tail() {
        let calls = AtomicUsize::new(0);
        let report = run(
            &names(31),
            &ProbeOptions {
                retries: 0,
                ..Default::default()
            },
            32,
            &Cancellation::new(),
            |_, _| {
                let index = calls.fetch_add(1, Ordering::SeqCst);
                if index < 12 {
                    Err(AppError::new(ErrorCode::Timeout, Stage::Probe))
                } else {
                    Ok(10)
                }
            },
        );
        assert_eq!(report.windows, [8, 4, 2, 2, 4, 4, 6, 1]);
        assert_eq!(report.measurements.len(), 31);
        assert!(report.peak_concurrency <= 8);
        assert_eq!(report.concurrency_limit, 31);
    }
    #[test]
    fn fixed_parallelism_caps_active_requests_and_retry_does_not_duplicate_nodes() {
        let attempts = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let report = run(
            &names(9),
            &ProbeOptions {
                concurrency: Some(3),
                retries: 1,
                timeout_ms: 777,
            },
            32,
            &Cancellation::new(),
            |_, timeout| {
                assert_eq!(timeout, 777);
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
                if attempts.fetch_add(1, Ordering::SeqCst) < 3 {
                    Err(AppError::new(ErrorCode::Connect, Stage::Probe))
                } else {
                    Ok(20)
                }
            },
        );
        assert_eq!(report.retries, 3);
        assert_eq!(report.measurements.len(), 9);
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert!(report.windows.iter().all(|n| *n <= 3));
    }
    #[test]
    fn authentication_stops_dispatch_and_is_never_retried() {
        let report = run(
            &names(10),
            &ProbeOptions {
                concurrency: Some(1),
                retries: 3,
                ..Default::default()
            },
            32,
            &Cancellation::new(),
            |_, _| Err(AppError::controller_status(401)),
        );
        assert_eq!(report.measurements.len(), 1);
        assert_eq!(report.retries, 0);
        assert_eq!(
            report.controller_error.unwrap().code,
            ErrorCode::ControllerAuth
        );
    }
    #[test]
    fn cancellation_stops_retry_and_later_nodes() {
        let cancel = Cancellation::new();
        let report = run(
            &names(10),
            &ProbeOptions {
                concurrency: Some(1),
                ..Default::default()
            },
            32,
            &cancel,
            |_, _| {
                cancel.cancel();
                Err(AppError::new(ErrorCode::Timeout, Stage::Probe))
            },
        );
        assert!(report.cancelled);
        assert_eq!(report.measurements.len(), 1);
        assert_eq!(report.retries, 0);
    }
}
