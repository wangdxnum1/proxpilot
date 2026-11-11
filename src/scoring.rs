//! Human-chosen weights; current verification grade always outranks these scores.
use crate::{
    diagnostics::{Availability, ErrorCode, Stage, VerificationReport},
    history::{HistoryEntry, RETENTION_SECONDS},
};
use std::cmp::Ordering;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ScoreBreakdown {
    pub total: f64,
    pub success: f64,
    pub latency: f64,
    pub stability: f64,
    pub cost: f64,
    pub samples: usize,
}
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        values[middle - 1] / 2.0 + values[middle] / 2.0
    } else {
        values[middle]
    })
}
fn valid_history(row: &HistoryEntry, now: i64) -> bool {
    let measurement = &row.measurement;
    measurement.stage == Stage::Verification
        && measurement.samples_requested > 0
        && measurement.samples_passed <= measurement.samples_requested
        && row.timestamp >= now.saturating_sub(RETENTION_SECONDS)
        && row.timestamp <= now
        && measurement.availability.is_some()
        && measurement
            .delay_ms
            .is_none_or(|ms| ms.is_finite() && ms >= 0.0)
        && measurement.error_code.is_none_or(|code| {
            !matches!(
                code,
                ErrorCode::Cancelled
                    | ErrorCode::ControllerAuth
                    | ErrorCode::IdentityMismatch
                    | ErrorCode::ClientUnavailable
                    | ErrorCode::InvalidInput
                    | ErrorCode::StateUnavailable
                    | ErrorCode::ProfileChanged
                    | ErrorCode::SelectionConflict
                    | ErrorCode::RollbackFailed
                    | ErrorCode::ControllerHttp
                    | ErrorCode::ControllerTimeout
            )
        })
        && (measurement.availability != Some(Availability::Webpage)
            || (measurement.delay_ms.is_some()
                && measurement.samples_passed == measurement.samples_requested))
}
pub fn score(
    history: &[HistoryEntry],
    current: &VerificationReport,
    rate: Option<f64>,
) -> ScoreBreakdown {
    score_at(history, current, rate, crate::history::now())
}
pub fn score_at(
    history: &[HistoryEntry],
    current: &VerificationReport,
    rate: Option<f64>,
    now: i64,
) -> ScoreBreakdown {
    let history: Vec<_> = history
        .iter()
        .filter(|row| valid_history(row, now))
        .collect();
    let samples = history.len();
    let successes = history
        .iter()
        .filter(|row| row.measurement.availability == Some(Availability::Webpage))
        .count();
    let success = 50.0
        * if samples < 3 {
            0.5
        } else {
            (successes as f64 + 1.0) / (samples as f64 + 2.0)
        };
    let mut times: Vec<_> = history
        .iter()
        .filter(|row| row.measurement.availability == Some(Availability::Webpage))
        .filter_map(|row| row.measurement.delay_ms)
        .collect();
    let historical_median = median(&mut times);
    let current_ms = if current.availability == Availability::Webpage {
        current.mean_ms.filter(|ms| ms.is_finite() && *ms >= 0.0)
    } else {
        None
    };
    let latency = 25.0
        * historical_median
            .or(current_ms)
            .map(|ms| 1.0 / (1.0 + ms / 500.0))
            .unwrap_or(0.5);
    let stability = 15.0
        * if times.len() < 3 {
            0.5
        } else {
            let center = historical_median.unwrap();
            let mut deviations: Vec<_> = times.iter().map(|ms| (ms - center).abs()).collect();
            1.0 / (1.0 + median(&mut deviations).unwrap() / center.max(1.0))
        };
    let cost = 10.0
        * rate
            .filter(|rate| rate.is_finite() && *rate > 0.0)
            .map(|rate| 1.0 / (1.0 + rate))
            .unwrap_or(0.0);
    ScoreBreakdown {
        total: (success + latency + stability + cost).clamp(0.0, 100.0),
        success,
        latency,
        stability,
        cost,
        samples,
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Candidate {
    pub node: String,
    pub verification: VerificationReport,
    pub score: ScoreBreakdown,
}
impl Candidate {
    pub fn verified(
        node: String,
        history: &[HistoryEntry],
        verification: VerificationReport,
        rate: Option<f64>,
    ) -> Option<Self> {
        let valid = verification.samples_requested > 0
            && verification.samples_passed <= verification.samples_requested
            && verification
                .mean_ms
                .is_none_or(|ms| ms.is_finite() && ms >= 0.0)
            && match verification.availability {
                Availability::Webpage => {
                    verification.samples_passed == verification.samples_requested
                        && verification.mean_ms.is_some()
                }
                Availability::IpOnly => {
                    verification.samples_passed < verification.samples_requested
                }
                Availability::Failed => false,
            };
        if !valid {
            return None;
        }
        let score = score(history, &verification, rate);
        Some(Self {
            node,
            verification,
            score,
        })
    }
}
pub fn grade(availability: Availability) -> u8 {
    match availability {
        Availability::Webpage => 0,
        Availability::IpOnly => 1,
        Availability::Failed => 2,
    }
}
/// Orthogonal tie-breaker inside one availability grade. When the stream
/// dimension is disabled every candidate is `None`, so this key is equal for
/// all of them and `compare_candidates` is byte-for-byte identical to the
/// pre-existing ordering.
fn stream_rank(stream: Option<&crate::diagnostics::StreamReport>) -> (u8, u32) {
    use crate::diagnostics::StreamHealth;
    match stream.map(|s| s.health) {
        Some(StreamHealth::Healthy) => (0, 0),
        Some(StreamHealth::Degraded) => (1, 0),
        Some(StreamHealth::Unknown) | None => (2, 0), // not measured = neutral
        Some(StreamHealth::Cut) => (3, stream.map_or(0, |s| s.early_cuts as u32)),
    }
}

pub fn compare_candidates(a: &Candidate, b: &Candidate) -> Ordering {
    grade(a.verification.availability)
        .cmp(&grade(b.verification.availability))
        .then_with(|| {
            stream_rank(a.verification.stream.as_ref())
                .cmp(&stream_rank(b.verification.stream.as_ref()))
        })
        .then_with(|| b.score.total.total_cmp(&a.score.total))
        .then_with(|| {
            a.verification
                .mean_ms
                .unwrap_or(f64::MAX)
                .total_cmp(&b.verification.mean_ms.unwrap_or(f64::MAX))
        })
        .then_with(|| a.node.cmp(&b.node))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diagnostics::{Availability, Stage, VerificationReport},
        history::{HistoryEntry, Measurement},
    };
    fn current(grade: Availability, ms: Option<f64>) -> VerificationReport {
        VerificationReport {
            availability: grade,
            mean_ms: ms,
            samples_requested: 2,
            samples_passed: if grade == Availability::Webpage { 2 } else { 0 },
            error: None,
            stream: None,
        }
    }
    fn row(grade: Availability, ms: Option<f64>) -> HistoryEntry {
        HistoryEntry {
            scope: String::new(),
            target: String::new(),
            node: String::new(),
            timestamp: crate::history::now(),
            measurement: Measurement {
                stage: Stage::Verification,
                availability: Some(grade),
                delay_ms: ms,
                error_code: None,
                http_status: None,
                samples_requested: 2,
                samples_passed: if grade == Availability::Webpage { 2 } else { 0 },
            },
        }
    }
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }
    #[test]
    fn configuration_and_local_state_errors_do_not_lower_node_success_rate() {
        for code in [
            ErrorCode::InvalidInput,
            ErrorCode::StateUnavailable,
            ErrorCode::ProfileChanged,
        ] {
            let mut rows = vec![row(Availability::Failed, None); 3];
            for row in &mut rows {
                row.measurement.error_code = Some(code);
            }
            assert_eq!(
                super::score(&rows, &current(Availability::Webpage, Some(500.0)), None).samples,
                0
            );
        }
    }
    #[test]
    fn cold_start_and_exact_weights() {
        let score = super::score(&[], &current(Availability::Webpage, Some(500.0)), Some(1.0));
        close(score.success, 25.0);
        close(score.latency, 12.5);
        close(score.stability, 7.5);
        close(score.cost, 5.0);
        close(score.total, 50.0);
        assert_eq!(score.samples, 0);
        let rows = vec![row(Availability::Webpage, Some(500.0)); 3];
        let score = super::score(
            &rows,
            &current(Availability::Webpage, Some(500.0)),
            Some(1.0),
        );
        close(score.success, 40.0);
        close(score.stability, 15.0);
        close(score.total, 72.5);
        assert_eq!(score.samples, 3);
    }
    #[test]
    fn median_mad_expiration_and_separate_probe_stage() {
        let mut rows = vec![
            row(Availability::Webpage, Some(100.0)),
            row(Availability::Webpage, Some(300.0)),
            row(Availability::Webpage, Some(500.0)),
        ];
        let score = super::score(&rows, &current(Availability::Webpage, Some(1.0)), Some(1.0));
        close(score.latency, 15.625);
        close(score.stability, 9.0);
        close(score.total, 69.625);
        rows[0].measurement.stage = Stage::Probe;
        rows[1].timestamp = 0;
        rows[2].timestamp = crate::history::now() + 100;
        let score = score_at(
            &rows,
            &current(Availability::Webpage, Some(500.0)),
            None,
            crate::history::now(),
        );
        assert_eq!(score.samples, 0);
        close(score.success, 25.0);
        close(score.stability, 7.5);
        close(score.cost, 0.0);
    }
    #[test]
    fn ip_only_unknown_cost_extremes_remain_finite() {
        let evidence = current(Availability::IpOnly, None);
        let score = super::score(&[], &evidence, None);
        close(score.latency, 12.5);
        close(score.total, 45.0);
        for rate in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            close(super::score(&[], &evidence, Some(rate)).cost, 0.0);
        }
        assert!(super::score(
            &vec![row(Availability::Webpage, Some(f64::MAX)); 3],
            &current(Availability::Webpage, Some(f64::MAX)),
            Some(f64::MAX)
        )
        .total
        .is_finite());
        assert!(super::score(
            &[row(Availability::Webpage, Some(f64::NAN))],
            &current(Availability::IpOnly, Some(f64::INFINITY)),
            None
        )
        .total
        .is_finite());
    }
    #[test]
    fn grade_dominates_history_failed_current_rejected_and_ties_stable() {
        let ip = Candidate::verified(
            "ip".into(),
            &vec![row(Availability::Webpage, Some(1.0)); 20],
            current(Availability::IpOnly, None),
            Some(0.1),
        )
        .unwrap();
        let web = Candidate::verified(
            "web".into(),
            &[],
            current(Availability::Webpage, Some(9000.0)),
            None,
        )
        .unwrap();
        assert!(compare_candidates(&web, &ip).is_lt());
        assert!(Candidate::verified(
            "failed".into(),
            &vec![row(Availability::Webpage, Some(1.0)); 20],
            current(Availability::Failed, None),
            Some(1.0)
        )
        .is_none());
        assert!(Candidate::verified(
            "invalid".into(),
            &[],
            current(Availability::Webpage, Some(f64::NAN)),
            None
        )
        .is_none());
        let a = Candidate::verified(
            "a".into(),
            &[],
            current(Availability::Webpage, Some(500.0)),
            None,
        )
        .unwrap();
        let b = Candidate::verified(
            "b".into(),
            &[],
            current(Availability::Webpage, Some(500.0)),
            None,
        )
        .unwrap();
        assert!(compare_candidates(&a, &b).is_lt());
    }

    fn with_stream(
        grade: Availability,
        health: crate::diagnostics::StreamHealth,
        node: &str,
    ) -> Candidate {
        let mut v = current(grade, Some(500.0));
        v.stream = Some(crate::diagnostics::StreamReport {
            samples_requested: 5,
            samples_completed: usize::from(health == crate::diagnostics::StreamHealth::Healthy),
            early_cuts: usize::from(health == crate::diagnostics::StreamHealth::Cut),
            slow_samples: 0,
            min_received_ratio: 1.0,
            median_throughput_kbps: 3000.0,
            health,
            samples: vec![],
            represents: "proxy_link".into(),
        });
        Candidate::verified(node.into(), &[], v, None).unwrap()
    }

    #[test]
    fn disabled_stream_dimension_leaves_ordering_exactly_unchanged() {
        // All `None` => equal tie-breaker => grade, score, mean_ms, name only.
        let a = Candidate::verified(
            "a".into(),
            &[],
            current(Availability::Webpage, Some(500.0)),
            None,
        )
        .unwrap();
        let b = Candidate::verified(
            "b".into(),
            &[],
            current(Availability::Webpage, Some(500.0)),
            None,
        )
        .unwrap();
        assert!(a.verification.stream.is_none());
        assert!(b.verification.stream.is_none());
        assert!(
            compare_candidates(&a, &b).is_lt(),
            "name must decide when scores tie"
        );
        assert_eq!(a.score.total, b.score.total);
    }

    #[test]
    fn grade_still_outranks_stream() {
        let web_cut = with_stream(
            Availability::Webpage,
            crate::diagnostics::StreamHealth::Cut,
            "z",
        );
        let ip_healthy = with_stream(
            Availability::IpOnly,
            crate::diagnostics::StreamHealth::Healthy,
            "a",
        );
        assert!(compare_candidates(&web_cut, &ip_healthy).is_lt());
    }

    #[test]
    fn stream_breaks_ties_only_inside_the_same_grade() {
        use crate::diagnostics::StreamHealth as H;
        let healthy = with_stream(Availability::Webpage, H::Healthy, "z");
        let degraded = with_stream(Availability::Webpage, H::Degraded, "a");
        let unknown = with_stream(Availability::Webpage, H::Unknown, "a");
        let cut = with_stream(Availability::Webpage, H::Cut, "a");
        assert!(compare_candidates(&healthy, &degraded).is_lt());
        assert!(compare_candidates(&degraded, &unknown).is_lt());
        assert!(compare_candidates(&unknown, &cut).is_lt());
    }
}
