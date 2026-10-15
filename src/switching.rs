//! Explicit switching transaction; Drop retains evidence, never hides network cleanup.

use crate::sysproxy::ProxySnapshot;
use crate::{
    diagnostics::{AppError, AppResult, Availability, ErrorCode, Stage, VerificationReport},
    operation::{state_directory, Cancellation, MutationLock, RecoveryJournal},
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveState {
    pub identity: String,
    pub profile: Option<i64>,
    pub members: Vec<String>,
    pub selection: Option<String>,
}
impl LiveState {
    fn normalize(&mut self) {
        self.members.sort();
        self.members.dedup();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwitchSnapshot {
    pub group: String,
    pub state: LiveState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySync {
    Synced,
    ClientManaged,
    NotAvailable,
    NoMatchingGroup,
}

pub trait SwitchControl {
    fn state(&self, group: &str) -> AppResult<LiveState>;
    fn select(&self, group: &str, target: &str) -> AppResult<()>;
    fn proxy_snapshot(&self) -> AppResult<Option<ProxySnapshot>> {
        Ok(None)
    }
    fn target_proxy_snapshot(&self) -> AppResult<Option<ProxySnapshot>> {
        Ok(None)
    }
    fn enable_proxy(&self) -> AppResult<()> {
        Ok(())
    }
    fn restore_proxy(&self, _: &ProxySnapshot, _: &ProxySnapshot) -> AppResult<bool> {
        Ok(true)
    }
    fn persist(&self, _: &SwitchSnapshot, _: &str) -> AppResult<MemorySync> {
        Ok(MemorySync::NotAvailable)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProxyChange {
    before: ProxySnapshot,
    written: ProxySnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecoveryRecord {
    schema_version: u32,
    snapshot: SwitchSnapshot,
    expected: String,
    pending: Option<String>,
    committed: bool,
    #[serde(default)]
    proxy: Option<ProxyChange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackStatus {
    Unchanged,
    Restored,
    SkippedConflict,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RollbackReport {
    pub status: RollbackStatus,
    pub error: Option<AppError>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommitReport {
    pub group: String,
    pub original: String,
    pub selected: String,
    pub verification: VerificationReport,
    pub memory: Option<MemorySync>,
    pub warnings: Vec<AppError>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecoverySummary {
    pub group: String,
    pub original: Option<String>,
    pub expected: String,
    pub pending: Option<String>,
    pub committed: bool,
    pub profile: Option<i64>,
}

pub fn inspect_recovery() -> AppResult<Option<RecoverySummary>> {
    RecoveryJournal::current()?
        .load::<RecoveryRecord>()?
        .map(|record| {
            if record.schema_version != 1 {
                return Err(AppError::new(ErrorCode::StateUnavailable, Stage::State));
            }
            Ok(RecoverySummary {
                group: record.snapshot.group,
                original: record.snapshot.state.selection,
                expected: record.expected,
                pending: record.pending,
                committed: record.committed,
                profile: record.snapshot.state.profile,
            })
        })
        .transpose()
}

pub fn discard_recovery() -> AppResult<()> {
    let _ownership = MutationLock::acquire()?;
    RecoveryJournal::current()?.remove()
}

pub fn retry_recovery<C: SwitchControl + ?Sized>(
    control: &C,
    cancellation: &Cancellation,
) -> AppResult<RollbackReport> {
    cancellation.check()?;
    let _ownership = MutationLock::acquire()?;
    let journal = RecoveryJournal::current()?;
    match journal.load::<RecoveryRecord>()? {
        Some(record) => Ok(restore_record(control, &journal, &record)),
        None => Ok(RollbackReport {
            status: RollbackStatus::Unchanged,
            error: None,
        }),
    }
}

pub struct SwitchTransaction<'a, C: SwitchControl + ?Sized> {
    control: &'a C,
    record: RecoveryRecord,
    journal: RecoveryJournal,
    cancellation: Cancellation,
    finalized: bool,
    _lock: MutationLock,
}

impl<'a, C: SwitchControl + ?Sized> SwitchTransaction<'a, C> {
    pub fn begin(control: &'a C, group: &str, cancellation: Cancellation) -> AppResult<Self> {
        Self::begin_at(control, group, cancellation, &state_directory()?)
    }

    pub fn begin_at(
        control: &'a C,
        group: &str,
        cancellation: Cancellation,
        directory: &Path,
    ) -> AppResult<Self> {
        cancellation.check()?;
        let lock = MutationLock::acquire_at(directory)?;
        let journal = RecoveryJournal::new(directory.join("recovery.bin"));
        if let Some(previous) = journal.load::<RecoveryRecord>()? {
            let recovered = restore_record(control, &journal, &previous);
            if let Some(error) = recovered.error {
                return Err(error);
            }
            if matches!(
                recovered.status,
                RollbackStatus::Failed | RollbackStatus::SkippedConflict
            ) {
                return Err(AppError::new(ErrorCode::RollbackFailed, Stage::Rollback));
            }
        }
        let mut state = control.state(group)?;
        state.normalize();
        let original = state
            .selection
            .as_ref()
            .filter(|name| !name.is_empty() && state.members.contains(name))
            .cloned()
            .ok_or_else(|| AppError::new(ErrorCode::InvalidResponse, Stage::Switching))?;
        if group.is_empty() || state.identity.is_empty() {
            return Err(AppError::new(ErrorCode::InvalidResponse, Stage::Switching));
        }
        let record = RecoveryRecord {
            schema_version: 1,
            snapshot: SwitchSnapshot {
                group: group.into(),
                state,
            },
            expected: original,
            pending: None,
            committed: false,
            proxy: None,
        };
        journal.store(&record)?;
        Ok(Self {
            control,
            record,
            journal,
            cancellation,
            finalized: false,
            _lock: lock,
        })
    }

    pub fn snapshot(&self) -> &SwitchSnapshot {
        &self.record.snapshot
    }

    pub fn repair_proxy(&mut self) -> AppResult<bool> {
        self.cancellation.check()?;
        self.ensure_active()?;
        checked_state(self.control, &self.record, false)?;
        let Some(before) = self.control.proxy_snapshot()? else {
            return Ok(false);
        };
        let Some(written) = self.control.target_proxy_snapshot()? else {
            return Ok(false);
        };
        if let Some(owned) = &self.record.proxy {
            return if before == owned.written {
                Ok(false)
            } else {
                Err(AppError::new(
                    ErrorCode::SelectionConflict,
                    Stage::Switching,
                ))
            };
        }
        if before == written {
            return Ok(false);
        }
        self.record.proxy = Some(ProxyChange {
            before,
            written: written.clone(),
        });
        self.journal.store(&self.record)?;
        self.control.enable_proxy()?;
        if self.control.proxy_snapshot()? != Some(written) {
            return Err(AppError::new(
                ErrorCode::SelectionConflict,
                Stage::Switching,
            ));
        }
        Ok(true)
    }

    pub fn select(&mut self, target: &str) -> AppResult<()> {
        self.cancellation.check()?;
        self.ensure_active()?;
        let current = checked_state(self.control, &self.record, false)?;
        if !self
            .record
            .snapshot
            .state
            .members
            .iter()
            .any(|name| name == target)
        {
            return Err(AppError::new(ErrorCode::InvalidInput, Stage::Switching));
        }
        self.record.pending = Some(target.into());
        self.journal.store(&self.record)?;
        if current.selection.as_deref() != Some(target) {
            self.control.select(&self.record.snapshot.group, target)?;
        }
        let after = checked_state(self.control, &self.record, false)?;
        if after.selection.as_deref() != Some(target) {
            return Err(AppError::new(ErrorCode::InvalidResponse, Stage::Switching));
        }
        self.record.expected = target.into();
        self.record.pending = None;
        self.journal.store(&self.record)
    }

    pub fn commit(&mut self, verification: &VerificationReport) -> AppResult<CommitReport> {
        self.cancellation.check()?;
        self.ensure_active()?;
        if verification.availability == Availability::Failed {
            return Err(verification.error.clone().unwrap_or_else(|| {
                AppError::new(ErrorCode::TransportUnknown, Stage::Verification)
            }));
        }
        if verification.availability == Availability::Webpage
            && (verification.samples_requested == 0
                || verification.samples_passed != verification.samples_requested
                || !verification
                    .mean_ms
                    .is_some_and(|ms| ms.is_finite() && ms >= 0.0))
        {
            return Err(AppError::new(
                ErrorCode::InvalidResponse,
                Stage::Verification,
            ));
        }
        let state = checked_state(self.control, &self.record, false)?;
        if self.record.pending.is_some()
            || state.selection.as_deref() != Some(&self.record.expected)
        {
            return Err(AppError::new(
                ErrorCode::SelectionConflict,
                Stage::Switching,
            ));
        }
        // Persist the commit decision first: a failed journal deletion must never
        // cause the next invocation to undo an already verified committed choice.
        let mut committed = self.record.clone();
        committed.committed = true;
        self.journal.store(&committed)?;
        self.record = committed;
        self.finalized = true;
        let mut warnings = Vec::new();
        let memory = match self
            .control
            .persist(&self.record.snapshot, &self.record.expected)
        {
            Ok(result) => Some(result),
            Err(error) => {
                warnings.push(error);
                None
            }
        };
        if let Err(error) = self.journal.remove() {
            warnings.push(error);
        }
        Ok(CommitReport {
            group: self.record.snapshot.group.clone(),
            original: self
                .record
                .snapshot
                .state
                .selection
                .clone()
                .unwrap_or_default(),
            selected: self.record.expected.clone(),
            verification: verification.clone(),
            memory,
            warnings,
        })
    }

    pub fn rollback(&mut self) -> RollbackReport {
        if self.finalized {
            return RollbackReport {
                status: RollbackStatus::Unchanged,
                error: None,
            };
        }
        let report = restore_record(self.control, &self.journal, &self.record);
        if matches!(
            report.status,
            RollbackStatus::Unchanged | RollbackStatus::Restored
        ) {
            self.finalized = true;
        }
        report
    }

    fn ensure_active(&self) -> AppResult<()> {
        if self.finalized {
            Err(AppError::new(ErrorCode::InvalidInput, Stage::Switching))
        } else {
            Ok(())
        }
    }
}

fn checked_state<C: SwitchControl + ?Sized>(
    control: &C,
    record: &RecoveryRecord,
    allow_original: bool,
) -> AppResult<LiveState> {
    if record.schema_version != 1 {
        return Err(AppError::new(ErrorCode::StateUnavailable, Stage::State));
    }
    let mut state = control.state(&record.snapshot.group)?;
    state.normalize();
    let captured = &record.snapshot.state;
    if state.identity != captured.identity {
        return Err(AppError::new(ErrorCode::IdentityMismatch, Stage::Switching));
    }
    if state.profile != captured.profile || state.members != captured.members {
        return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
    }
    let selected = state
        .selection
        .as_deref()
        .ok_or_else(|| AppError::new(ErrorCode::InvalidResponse, Stage::Switching))?;
    let known = selected == record.expected
        || record.pending.as_deref() == Some(selected)
        || (allow_original && captured.selection.as_deref() == Some(selected));
    if !known {
        return Err(AppError::new(
            ErrorCode::SelectionConflict,
            Stage::Switching,
        ));
    }
    Ok(state)
}

fn restore_record<C: SwitchControl + ?Sized>(
    control: &C,
    journal: &RecoveryJournal,
    record: &RecoveryRecord,
) -> RollbackReport {
    let failure = |error: AppError| {
        let conflict = matches!(
            error.code,
            ErrorCode::IdentityMismatch | ErrorCode::ProfileChanged | ErrorCode::SelectionConflict
        );
        RollbackReport {
            status: if conflict {
                RollbackStatus::SkippedConflict
            } else {
                RollbackStatus::Failed
            },
            error: Some(error),
        }
    };
    if record.schema_version != 1 {
        return failure(AppError::new(ErrorCode::StateUnavailable, Stage::State));
    }
    if record.committed {
        return RollbackReport {
            status: RollbackStatus::Unchanged,
            error: journal.remove().err(),
        };
    }
    let original = match record.snapshot.state.selection.as_deref() {
        Some(original)
            if record
                .snapshot
                .state
                .members
                .iter()
                .any(|name| name == original) =>
        {
            original
        }
        _ => return failure(AppError::new(ErrorCode::InvalidResponse, Stage::Rollback)),
    };
    let current = match checked_state(control, record, true) {
        Ok(state) => state,
        Err(error) => return failure(error),
    };
    if current.selection.as_deref() == Some(original) {
        return finish_restore(control, journal, record, RollbackStatus::Unchanged);
    }
    let attempted = control.select(&record.snapshot.group, original);
    match checked_state(control, record, true) {
        Ok(state) if state.selection.as_deref() == Some(original) => {
            finish_restore(control, journal, record, RollbackStatus::Restored)
        }
        after => {
            let error = match attempted {
                Err(error) => error,
                Ok(()) => after
                    .err()
                    .unwrap_or_else(|| AppError::new(ErrorCode::InvalidResponse, Stage::Rollback)),
            };
            failure(error)
        }
    }
}

fn finish_restore<C: SwitchControl + ?Sized>(
    control: &C,
    journal: &RecoveryJournal,
    record: &RecoveryRecord,
    status: RollbackStatus,
) -> RollbackReport {
    if let Some(proxy) = &record.proxy {
        match control.restore_proxy(&proxy.before, &proxy.written) {
            Ok(true) => {}
            Ok(false) => {
                let mut error = AppError::new(ErrorCode::SelectionConflict, Stage::Rollback);
                error.message = "系统代理已被外部修改，保留新的配置".into();
                return RollbackReport {
                    status: RollbackStatus::SkippedConflict,
                    error: Some(error),
                };
            }
            Err(mut error) => {
                error.stage = Stage::Rollback;
                error.message = format!("恢复系统代理失败：{}", error.message);
                return RollbackReport {
                    status: RollbackStatus::Failed,
                    error: Some(error),
                };
            }
        }
    }
    RollbackReport {
        status,
        error: journal.remove().err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diagnostics::{Availability, ErrorCode, Stage, VerificationReport},
        operation::Cancellation,
    };
    use std::{
        cell::{Cell, RefCell},
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Core {
        selected: RefCell<Option<String>>,
        profile: Cell<Option<i64>>,
        writes: RefCell<Vec<String>>,
        persisted: RefCell<Vec<(Option<i64>, String)>>,
        lose_response: Cell<bool>,
        fail_restore: Cell<bool>,
        proxy: RefCell<Option<ProxySnapshot>>,
        fail_proxy_write: Cell<bool>,
        hold_journal: RefCell<Option<std::path::PathBuf>>,
        held_file: RefCell<Option<std::fs::File>>,
        fail_memory: Cell<bool>,
    }
    impl SwitchControl for Core {
        fn proxy_snapshot(&self) -> AppResult<Option<ProxySnapshot>> {
            Ok(self.proxy.borrow().clone())
        }
        fn target_proxy_snapshot(&self) -> AppResult<Option<ProxySnapshot>> {
            Ok(self.proxy.borrow().as_ref().map(|_| ProxySnapshot {
                enabled: Some(1),
                server: Some("127.0.0.1:7897".into()),
            }))
        }
        fn enable_proxy(&self) -> AppResult<()> {
            *self.proxy.borrow_mut() = self.target_proxy_snapshot()?;
            if self.fail_proxy_write.get() {
                Err(AppError::new(ErrorCode::StateUnavailable, Stage::Switching))
            } else {
                Ok(())
            }
        }
        fn restore_proxy(
            &self,
            before: &ProxySnapshot,
            written: &ProxySnapshot,
        ) -> AppResult<bool> {
            if self.proxy.borrow().as_ref() != Some(written)
                && self.proxy.borrow().as_ref() != Some(before)
            {
                return Ok(false);
            }
            *self.proxy.borrow_mut() = Some(before.clone());
            Ok(true)
        }
        fn state(&self, _: &str) -> crate::diagnostics::AppResult<LiveState> {
            Ok(LiveState {
                identity: "instance-a".into(),
                profile: self.profile.get(),
                members: ["auto", "good", "bad", "user"].map(str::to_string).to_vec(),
                selection: self.selected.borrow().clone(),
            })
        }
        fn select(&self, _: &str, target: &str) -> crate::diagnostics::AppResult<()> {
            self.writes.borrow_mut().push(target.into());
            if target == "auto" && self.fail_restore.get() {
                return Err(crate::diagnostics::AppError::controller_status(503));
            }
            *self.selected.borrow_mut() = Some(target.into());
            if self.lose_response.replace(false) {
                return Err(crate::diagnostics::AppError::new(
                    ErrorCode::ControllerTimeout,
                    Stage::Controller,
                ));
            }
            Ok(())
        }
        fn persist(
            &self,
            snapshot: &SwitchSnapshot,
            target: &str,
        ) -> crate::diagnostics::AppResult<MemorySync> {
            if let Some(path) = self.hold_journal.borrow().as_ref() {
                use std::os::windows::fs::OpenOptionsExt;
                *self.held_file.borrow_mut() = Some(
                    std::fs::OpenOptions::new()
                        .read(true)
                        .share_mode(3)
                        .open(path)
                        .unwrap(),
                );
            }
            if self.fail_memory.get() {
                return Err(AppError::new(ErrorCode::StateUnavailable, Stage::State));
            }
            self.persisted
                .borrow_mut()
                .push((snapshot.state.profile, target.into()));
            Ok(MemorySync::Synced)
        }
    }
    fn scenario(run: impl FnOnce(&Core, &Path)) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "proxpilot-switch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let core = Core {
            selected: RefCell::new(Some("auto".into())),
            profile: Cell::new(Some(1)),
            writes: RefCell::new(vec![]),
            persisted: RefCell::new(vec![]),
            lose_response: Cell::new(false),
            fail_restore: Cell::new(false),
            proxy: RefCell::new(None),
            fail_proxy_write: Cell::new(false),
            hold_journal: RefCell::new(None),
            held_file: RefCell::new(None),
            fail_memory: Cell::new(false),
        };
        run(&core, &root);
        std::fs::remove_dir_all(root).unwrap();
    }
    fn passed() -> VerificationReport {
        VerificationReport {
            availability: Availability::Webpage,
            mean_ms: Some(100.0),
            samples_requested: 2,
            samples_passed: 2,
            error: None,
        }
    }

    #[test]
    fn committed_selection_survives_failed_memory_and_journal_deletion() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("good").unwrap();
            *core.hold_journal.borrow_mut() = Some(root.join("recovery.bin"));
            core.fail_memory.set(true);
            let report = txn.commit(&passed()).unwrap();
            assert_eq!(report.warnings.len(), 2);
            assert!(report.memory.is_none());
            assert!(root.join("recovery.bin").exists());
            assert_eq!(core.selected.borrow().as_deref(), Some("good"));
            drop(txn);
            core.held_file.borrow_mut().take();
            let mut next =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            assert_eq!(core.selected.borrow().as_deref(), Some("good"));
            assert_eq!(&*core.writes.borrow(), &["good"]);
            assert_eq!(next.rollback().status, RollbackStatus::Unchanged);
        });
    }

    #[test]
    fn proxy_and_node_restore_together_after_failure() {
        scenario(|core, root| {
            let before = ProxySnapshot {
                enabled: Some(0),
                server: Some("old-proxy:1234".into()),
            };
            *core.proxy.borrow_mut() = Some(before.clone());
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            assert!(txn.repair_proxy().unwrap());
            txn.select("bad").unwrap();
            assert_eq!(txn.rollback().status, RollbackStatus::Restored);
            assert_eq!(*core.proxy.borrow(), Some(before));
            assert_eq!(core.selected.borrow().as_deref(), Some("auto"));
        });
    }

    #[test]
    fn failed_proxy_write_is_logged_before_mutation_and_can_be_restored() {
        scenario(|core, root| {
            let before = ProxySnapshot {
                enabled: Some(0),
                server: None,
            };
            *core.proxy.borrow_mut() = Some(before.clone());
            core.fail_proxy_write.set(true);
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            assert!(txn.repair_proxy().is_err());
            assert!(root.join("recovery.bin").exists());
            assert_eq!(txn.rollback().status, RollbackStatus::Unchanged);
            assert_eq!(*core.proxy.borrow(), Some(before));
            assert!(core.writes.borrow().is_empty());
        });
    }

    #[test]
    fn external_proxy_update_is_preserved_and_not_repaired_twice() {
        scenario(|core, root| {
            *core.proxy.borrow_mut() = Some(ProxySnapshot {
                enabled: Some(0),
                server: None,
            });
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.repair_proxy().unwrap();
            let external = ProxySnapshot {
                enabled: Some(1),
                server: Some("127.0.0.1:7890".into()),
            };
            *core.proxy.borrow_mut() = Some(external.clone());
            assert_eq!(
                txn.repair_proxy().unwrap_err().code,
                ErrorCode::SelectionConflict
            );
            assert_eq!(txn.rollback().status, RollbackStatus::SkippedConflict);
            assert_eq!(*core.proxy.borrow(), Some(external));
            assert!(root.join("recovery.bin").exists());
        });
    }

    #[test]
    fn only_verified_commit_persists_captured_profile() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("good").unwrap();
            assert!(core.persisted.borrow().is_empty());
            assert_eq!(txn.commit(&passed()).unwrap().selected, "good");
            assert_eq!(&*core.persisted.borrow(), &[(Some(1), "good".into())]);
            assert!(!root.join("recovery.bin").exists());
        });
    }

    #[test]
    fn rollback_restores_original_direct_group_not_guessed_leaf() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("bad").unwrap();
            assert_eq!(txn.rollback().status, RollbackStatus::Restored);
            assert_eq!(core.selected.borrow().as_deref(), Some("auto"));
            assert_eq!(&*core.writes.borrow(), &["bad", "auto"]);
            assert!(core.persisted.borrow().is_empty());
        });
    }

    #[test]
    fn lost_put_response_still_allows_safe_rollback() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            core.lose_response.set(true);
            assert_eq!(
                txn.select("good").unwrap_err().code,
                ErrorCode::ControllerTimeout
            );
            assert_eq!(core.selected.borrow().as_deref(), Some("good"));
            assert_eq!(txn.rollback().status, RollbackStatus::Restored);
        });
    }

    #[test]
    fn external_selection_is_not_overwritten_by_rollback() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("good").unwrap();
            *core.selected.borrow_mut() = Some("user".into());
            assert_eq!(txn.rollback().status, RollbackStatus::SkippedConflict);
            assert_eq!(core.selected.borrow().as_deref(), Some("user"));
            assert_eq!(&*core.writes.borrow(), &["good"]);
            assert!(root.join("recovery.bin").exists());
        });
    }

    #[test]
    fn changed_profile_prevents_commit_and_old_profile_rollback() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("good").unwrap();
            core.profile.set(Some(2));
            assert_eq!(
                txn.commit(&passed()).unwrap_err().code,
                ErrorCode::ProfileChanged
            );
            assert_eq!(txn.rollback().status, RollbackStatus::SkippedConflict);
            assert!(core.persisted.borrow().is_empty());
            assert_eq!(&*core.writes.borrow(), &["good"]);
        });
    }

    #[test]
    fn failed_final_verification_never_commits_memory() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("good").unwrap();
            let failed = VerificationReport {
                availability: Availability::Failed,
                mean_ms: None,
                samples_requested: 2,
                samples_passed: 1,
                error: Some(crate::diagnostics::AppError::website_status(403)),
            };
            assert_eq!(txn.commit(&failed).unwrap_err().http_status, Some(403));
            assert!(core.persisted.borrow().is_empty());
            assert_eq!(txn.rollback().status, RollbackStatus::Restored);
        });
    }

    #[test]
    fn rollback_failure_retains_journal_and_real_http_status() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("bad").unwrap();
            core.fail_restore.set(true);
            let result = txn.rollback();
            assert_eq!(result.status, RollbackStatus::Failed);
            assert_eq!(result.error.unwrap().http_status, Some(503));
            assert!(root.join("recovery.bin").exists());
            assert!(core.persisted.borrow().is_empty());
        });
    }

    #[test]
    fn cancellation_prevents_commit_but_does_not_cancel_cleanup() {
        scenario(|core, root| {
            let cancel = Cancellation::new();
            let mut txn = SwitchTransaction::begin_at(core, "group", cancel.clone(), root).unwrap();
            txn.select("good").unwrap();
            cancel.cancel();
            assert_eq!(
                txn.commit(&passed()).unwrap_err().code,
                ErrorCode::Cancelled
            );
            assert_eq!(txn.rollback().status, RollbackStatus::Restored);
            assert!(core.persisted.borrow().is_empty());
        });
    }

    #[test]
    fn pending_journal_is_recovered_before_next_transaction() {
        scenario(|core, root| {
            let mut txn =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            txn.select("bad").unwrap();
            drop(txn);
            assert!(root.join("recovery.bin").exists());
            let mut next =
                SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).unwrap();
            assert_eq!(core.selected.borrow().as_deref(), Some("auto"));
            assert_eq!(next.rollback().status, RollbackStatus::Unchanged);
        });
    }

    #[test]
    fn missing_original_or_invalid_journal_refuses_to_guess() {
        scenario(|core, root| {
            *core.selected.borrow_mut() = None;
            assert!(SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).is_err());
            *core.selected.borrow_mut() = Some("auto".into());
            std::fs::write(root.join("recovery.bin"), b"broken").unwrap();
            assert!(SwitchTransaction::begin_at(core, "group", Cancellation::new(), root).is_err());
            assert!(core.writes.borrow().is_empty());
            assert_eq!(std::fs::read(root.join("recovery.bin")).unwrap(), b"broken");
        });
    }
}
