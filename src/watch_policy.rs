//! Monotonic watch policy; no network, sleep or persistent wall-clock deadlines.
use crate::{
    diagnostics::{Availability, VerificationReport},
    scoring::{Candidate, ScoreBreakdown},
};
use std::{collections::HashMap, time::Duration};

#[derive(Clone, Debug, serde::Serialize)]
pub struct WatchSettings {
    pub min_switch_interval_secs: u64,
    pub failure_cooldown_secs: u64,
    pub manual_hold_secs: u64,
}
impl Default for WatchSettings {
    fn default() -> Self {
        Self {
            min_switch_interval_secs: 120,
            failure_cooldown_secs: 600,
            manual_hold_secs: 900,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observed {
    Initial,
    Unchanged,
    ManualChange,
    ScopeChange,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Wait,
    Optimize,
    Repair,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Recheck(Duration),
    ConfirmedBroken,
}
pub struct Eligibility {
    pub names: Vec<String>,
    pub next_retry_after: Option<Duration>,
}
pub struct WatchPolicy {
    settings: WatchSettings,
    scope: Option<String>,
    selection: Option<String>,
    last_commit: Option<Duration>,
    last_optimization: Option<Duration>,
    manual_until: Option<Duration>,
    failure_since: Option<Duration>,
    cooldowns: HashMap<String, Duration>,
}
impl WatchPolicy {
    pub fn new(settings: WatchSettings) -> Self {
        Self {
            settings,
            scope: None,
            selection: None,
            last_commit: None,
            last_optimization: None,
            manual_until: None,
            failure_since: None,
            cooldowns: HashMap::new(),
        }
    }
    pub fn observe(&mut self, scope: &str, selection: &str, now: Duration) -> Observed {
        let observed = match (&self.scope, &self.selection) {
            (None, _) => Observed::Initial,
            (Some(previous), _) if previous != scope => Observed::ScopeChange,
            (_, Some(previous)) if previous != selection => Observed::ManualChange,
            _ => Observed::Unchanged,
        };
        if observed == Observed::ScopeChange {
            self.cooldowns.clear();
            self.last_commit = None;
        }
        if matches!(observed, Observed::ScopeChange | Observed::ManualChange) {
            self.manual_until =
                Some(now.saturating_add(Duration::from_secs(self.settings.manual_hold_secs)));
            self.last_optimization = Some(now);
            self.failure_since = None;
        }
        self.scope = Some(scope.into());
        self.selection = Some(selection.into());
        observed
    }
    pub fn health(&mut self, failed: bool, now: Duration) -> Health {
        if !failed {
            self.failure_since = None;
            return Health::Healthy;
        }
        let start = *self.failure_since.get_or_insert(now);
        let remaining = Duration::from_secs(5).saturating_sub(now.saturating_sub(start));
        if remaining.is_zero() {
            Health::ConfirmedBroken
        } else {
            Health::Recheck(remaining)
        }
    }
    pub fn environment_wait(&mut self) {
        self.failure_since = None;
    }
    pub fn action(
        &self,
        now: Duration,
        optimize: bool,
        reopt_secs: u64,
        confirmed_broken: bool,
    ) -> Action {
        if confirmed_broken {
            return Action::Repair;
        }
        let due = self
            .last_optimization
            .is_none_or(|previous| now.saturating_sub(previous) >= Duration::from_secs(reopt_secs));
        let protected = self.manual_until.is_some_and(|until| until > now);
        let interval = self.last_commit.is_none_or(|previous| {
            now.saturating_sub(previous)
                >= Duration::from_secs(self.settings.min_switch_interval_secs)
        });
        if optimize && due && !protected && interval {
            Action::Optimize
        } else {
            Action::Wait
        }
    }
    pub fn failed(&mut self, node: &str, now: Duration) {
        if self.settings.failure_cooldown_secs == 0 {
            return;
        }
        let until = now.saturating_add(Duration::from_secs(self.settings.failure_cooldown_secs));
        self.cooldowns
            .entry(node.into())
            .and_modify(|prior| {
                if *prior <= now {
                    *prior = until;
                }
            })
            .or_insert(until);
    }
    pub fn eligible(&self, nodes: &[String], now: Duration) -> Eligibility {
        let mut names = Vec::new();
        let mut next_retry_after = None;
        for name in nodes {
            if let Some(until) = self.cooldowns.get(name).filter(|until| **until > now) {
                let remaining = until.saturating_sub(now);
                next_retry_after = Some(
                    next_retry_after
                        .map(|previous: Duration| previous.min(remaining))
                        .unwrap_or(remaining),
                );
            } else {
                names.push(name.clone());
            }
        }
        Eligibility {
            names,
            next_retry_after,
        }
    }
    pub fn committed(&mut self, original: &str, selected: &str, now: Duration) {
        if original != selected {
            self.last_commit = Some(now);
        }
        self.selection = Some(selected.into());
        self.failure_since = None;
    }
    pub fn optimization_finished(&mut self, now: Duration) {
        self.last_optimization = Some(now);
        self.failure_since = None;
    }
    pub fn manual_remaining(&self, now: Duration) -> Duration {
        self.manual_until
            .map(|until| until.saturating_sub(now))
            .unwrap_or_default()
    }
}
pub fn cooldown_failure(failure: &crate::diagnostics::NodeFailure) -> bool {
    use crate::diagnostics::{ErrorCode, Stage};
    match failure.phase {
        Stage::Probe => failure.error.code == ErrorCode::ProbeFailed,
        Stage::Verification => matches!(
            failure.error.code,
            ErrorCode::WebsiteHttp
                | ErrorCode::ProxyConnect
                | ErrorCode::Connect
                | ErrorCode::Timeout
                | ErrorCode::Dns
                | ErrorCode::Tls
                | ErrorCode::ResponseTransfer
                | ErrorCode::TransportUnknown
        ),
        _ => false,
    }
}
pub fn has_benefit(
    current: &VerificationReport,
    current_score: &ScoreBreakdown,
    candidate: &Candidate,
) -> bool {
    if !current_score.total.is_finite()
        || !candidate.score.total.is_finite()
        || candidate
            .verification
            .mean_ms
            .is_some_and(|ms| !ms.is_finite() || ms < 0.0)
        || (candidate.verification.availability == Availability::Webpage
            && candidate.verification.mean_ms.is_none())
    {
        return false;
    }
    let current_grade = crate::scoring::grade(current.availability);
    let candidate_grade = crate::scoring::grade(candidate.verification.availability);
    if candidate_grade < current_grade {
        return true;
    }
    if candidate_grade != current_grade
        || current.availability != Availability::Webpage
        || !current_score.total.is_finite()
        || !candidate.score.total.is_finite()
        || candidate.score.total < current_score.total + 5.0
    {
        return false;
    }
    match (current.mean_ms, candidate.verification.mean_ms) {
        (Some(before), Some(after))
            if before.is_finite() && after.is_finite() && before >= 0.0 && after >= 0.0 =>
        {
            before - after >= 100.0 && after <= before * 0.8
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diagnostics::{Availability, VerificationReport},
        scoring::{Candidate, ScoreBreakdown},
    };
    fn t(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }
    #[test]
    fn explicit_zero_disables_each_protection_without_losing_manual_schedule_reset() {
        let mut policy = WatchPolicy::new(WatchSettings {
            min_switch_interval_secs: 0,
            failure_cooldown_secs: 0,
            manual_hold_secs: 0,
        });
        policy.observe("s", "old", t(0));
        policy.failed("old", t(0));
        assert_eq!(policy.eligible(&["old".into()], t(0)).names.len(), 1);
        policy.committed("old", "new", t(0));
        policy.optimization_finished(t(0));
        assert_eq!(policy.observe("s", "user", t(1)), Observed::ManualChange);
        assert_eq!(policy.action(t(2), true, 1, false), Action::Optimize);
    }
    #[test]
    fn control_and_configuration_failures_never_start_node_cooldown() {
        use crate::diagnostics::{AppError, ErrorCode, NodeFailure, Stage};
        for code in [
            ErrorCode::ControllerAuth,
            ErrorCode::IdentityMismatch,
            ErrorCode::InvalidInput,
            ErrorCode::StateUnavailable,
            ErrorCode::ControllerTimeout,
            ErrorCode::Cancelled,
        ] {
            assert!(!cooldown_failure(&NodeFailure {
                node: "n".into(),
                phase: Stage::Verification,
                error: AppError::new(code, Stage::Controller)
            }));
        }
        assert!(cooldown_failure(&NodeFailure {
            node: "n".into(),
            phase: Stage::Verification,
            error: AppError::website_status(403)
        }));
        assert!(!cooldown_failure(&NodeFailure {
            node: "n".into(),
            phase: Stage::Probe,
            error: AppError::controller_status(503)
        }));
        assert!(!cooldown_failure(&NodeFailure {
            node: "n".into(),
            phase: Stage::Probe,
            error: AppError::controller_status(504)
        }));
    }
    fn report(grade: Availability, ms: Option<f64>) -> VerificationReport {
        VerificationReport {
            availability: grade,
            mean_ms: ms,
            samples_requested: 1,
            samples_passed: usize::from(grade == Availability::Webpage),
            error: None,
        }
    }
    fn score(total: f64) -> ScoreBreakdown {
        ScoreBreakdown {
            total,
            success: 0.0,
            latency: 0.0,
            stability: 0.0,
            cost: 0.0,
            samples: 3,
        }
    }
    fn candidate(grade: Availability, ms: Option<f64>, total: f64) -> Candidate {
        Candidate {
            node: "new".into(),
            verification: report(grade, ms),
            score: score(total),
        }
    }

    #[test]
    fn default_fault_only_and_explicit_optimization_schedule() {
        let mut policy = WatchPolicy::new(WatchSettings::default());
        assert_eq!(policy.observe("scope", "original", t(0)), Observed::Initial);
        assert_eq!(policy.action(t(0), false, 100, false), Action::Wait);
        assert_eq!(policy.action(t(0), true, 100, false), Action::Optimize);
        policy.optimization_finished(t(0));
        assert_eq!(policy.action(t(99), true, 100, false), Action::Wait);
        assert_eq!(policy.action(t(100), true, 100, false), Action::Optimize);
        assert_eq!(policy.action(t(100), false, 100, true), Action::Repair);
    }
    #[test]
    fn confirmation_requires_five_seconds_and_health_or_environment_resets_it() {
        let mut policy = WatchPolicy::new(WatchSettings::default());
        assert_eq!(policy.health(false, t(0)), Health::Healthy);
        assert_eq!(policy.health(true, t(1)), Health::Recheck(t(5)));
        assert_eq!(policy.health(true, t(5)), Health::Recheck(t(1)));
        assert_eq!(policy.health(true, t(6)), Health::ConfirmedBroken);
        policy.environment_wait();
        assert_eq!(policy.health(true, t(7)), Health::Recheck(t(5)));
        assert_eq!(policy.health(false, t(8)), Health::Healthy);
        assert_eq!(policy.health(true, t(9)), Health::Recheck(t(5)));
    }
    #[test]
    fn manual_choice_is_protected_but_confirmed_failure_can_repair() {
        let mut policy = WatchPolicy::new(WatchSettings::default());
        policy.observe("scope", "old", t(0));
        policy.optimization_finished(t(0));
        assert_eq!(
            policy.observe("scope", "user", t(10)),
            Observed::ManualChange
        );
        assert_eq!(policy.action(t(909), true, 100, false), Action::Wait);
        assert_eq!(policy.action(t(910), true, 100, false), Action::Optimize);
        assert_eq!(policy.action(t(11), true, 100, true), Action::Repair);
        policy.committed("user", "repaired", t(11));
        assert_eq!(
            policy.observe("scope", "repaired", t(12)),
            Observed::Unchanged
        );
        assert_eq!(policy.action(t(132), true, 100, false), Action::Wait);
    }
    #[test]
    fn minimum_interval_only_follows_actual_commits_and_failure_can_override() {
        let mut policy = WatchPolicy::new(WatchSettings::default());
        policy.observe("s", "old", t(0));
        policy.committed("old", "new", t(10));
        policy.optimization_finished(t(10));
        assert_eq!(policy.action(t(129), true, 1, false), Action::Wait);
        assert_eq!(policy.action(t(130), true, 1, false), Action::Optimize);
        assert_eq!(policy.action(t(11), false, 100, true), Action::Repair);
        let mut unchanged = WatchPolicy::new(WatchSettings::default());
        unchanged.observe("s", "old", t(0));
        unchanged.committed("old", "old", t(10));
        unchanged.optimization_finished(t(10));
        assert_eq!(unchanged.action(t(11), true, 1, false), Action::Optimize);
    }
    #[test]
    fn cooldown_exhaustion_reports_next_time_without_erasing_failures_and_scope_resets() {
        let mut policy = WatchPolicy::new(WatchSettings::default());
        policy.observe("s", "a", t(0));
        policy.failed("a", t(1));
        policy.failed("b", t(11));
        let nodes = vec!["a".into(), "b".into()];
        let eligibility = policy.eligible(&nodes, t(12));
        assert!(eligibility.names.is_empty());
        assert_eq!(eligibility.next_retry_after, Some(t(589)));
        policy.environment_wait();
        assert!(policy.eligible(&nodes, t(13)).names.is_empty());
        assert_eq!(policy.eligible(&nodes, t(601)).names, vec!["a"]);
        assert_eq!(
            policy.observe("new-profile", "b", t(20)),
            Observed::ScopeChange
        );
        assert_eq!(policy.eligible(&nodes, t(20)).names.len(), 2);
        assert_eq!(policy.action(t(21), true, 1, false), Action::Wait);
    }
    #[test]
    fn benefit_requires_all_thresholds_and_uses_current_grade() {
        let current = report(Availability::Webpage, Some(1000.0));
        let base = score(60.0);
        assert!(has_benefit(
            &current,
            &base,
            &candidate(Availability::Webpage, Some(800.0), 65.0)
        ));
        assert!(!has_benefit(
            &current,
            &base,
            &candidate(Availability::Webpage, Some(850.0), 90.0)
        ));
        assert!(!has_benefit(
            &current,
            &base,
            &candidate(Availability::Webpage, Some(700.0), 64.99)
        ));
        assert!(!has_benefit(
            &report(Availability::Webpage, Some(400.0)),
            &base,
            &candidate(Availability::Webpage, Some(300.1), 90.0)
        ));
        assert!(has_benefit(
            &report(Availability::IpOnly, None),
            &score(99.0),
            &candidate(Availability::Webpage, Some(9000.0), 1.0)
        ));
        assert!(!has_benefit(
            &current,
            &base,
            &candidate(Availability::IpOnly, None, 99.0)
        ));
        assert!(!has_benefit(
            &report(Availability::IpOnly, None),
            &base,
            &candidate(Availability::IpOnly, None, 99.0)
        ));
        assert!(!has_benefit(
            &current,
            &base,
            &candidate(Availability::Webpage, Some(f64::NAN), 99.0)
        ));
        assert!(!has_benefit(
            &report(Availability::IpOnly, None),
            &base,
            &candidate(Availability::Webpage, Some(f64::NAN), 99.0)
        ));
    }
}
