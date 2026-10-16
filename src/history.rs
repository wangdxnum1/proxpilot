//! Independent, bounded history. Arbitrary names/paths/URLs are persisted only as hashes.
use crate::{
    detect::Backend,
    diagnostics::{AppError, AppResult, Availability, ErrorCode, Stage, VerificationReport},
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
const APPLICATION_ID: i64 = 0x50504831;
fn unavailable() -> AppError {
    AppError::new(ErrorCode::StateUnavailable, Stage::State)
}
pub fn fingerprint(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryScope {
    pub identity: String,
    pub target: String,
    pub profile: Option<i64>,
}
impl HistoryScope {
    pub fn new(be: &Backend, profile: Option<i64>, group: &str, url: &str) -> Self {
        let source = if be.kind.is_some() && Path::new(&be.source).is_absolute() {
            be.source.replace('\\', "/").to_lowercase()
        } else {
            match &be.api {
                crate::core_api::ApiEndpoint::NamedPipe(path) => {
                    path.to_string_lossy().to_lowercase()
                }
                crate::core_api::ApiEndpoint::Http(endpoint) => reqwest::Url::parse(endpoint)
                    .map(|mut parsed| {
                        let _ = parsed.set_username("");
                        let _ = parsed.set_password(None);
                        parsed.set_query(None);
                        parsed.set_fragment(None);
                        parsed.to_string()
                    })
                    .unwrap_or_else(|_| endpoint.clone()),
            }
        };
        let identity = fingerprint(
            &serde_json::json!([
                be.kind.map(|kind| kind.name()).unwrap_or("anonymous"),
                source,
                profile,
                group
            ])
            .to_string(),
        );
        Self {
            identity,
            target: fingerprint(url),
            profile,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub stage: Stage,
    pub availability: Option<Availability>,
    pub delay_ms: Option<f64>,
    pub error_code: Option<ErrorCode>,
    pub http_status: Option<u16>,
    pub samples_requested: usize,
    pub samples_passed: usize,
}
impl Measurement {
    pub fn verification(report: &VerificationReport) -> Self {
        Self {
            stage: Stage::Verification,
            availability: Some(report.availability),
            delay_ms: report.mean_ms,
            error_code: report.error.as_ref().map(|e| e.code),
            http_status: if report.availability == Availability::Webpage {
                Some(200)
            } else {
                report.error.as_ref().and_then(|e| e.http_status)
            },
            samples_requested: report.samples_requested,
            samples_passed: report.samples_passed,
        }
    }
    fn valid(&self) -> bool {
        matches!(self.stage, Stage::Probe | Stage::Verification)
            && self.delay_ms.is_none_or(|ms| ms.is_finite() && ms >= 0.0)
            && self.samples_passed <= self.samples_requested
            && self
                .http_status
                .is_none_or(|status| (100..=999).contains(&status))
            && match self.stage {
                Stage::Probe => self.availability.is_none() && self.samples_requested == 0,
                Stage::Verification => {
                    self.availability.is_some()
                        && self.samples_requested > 0
                        && (self.availability != Some(Availability::Webpage)
                            || (self.samples_requested == self.samples_passed
                                && self.delay_ms.is_some()))
                }
                _ => false,
            }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct HistoryEntry {
    pub scope: String,
    pub target: String,
    pub node: String,
    pub timestamp: i64,
    pub measurement: Measurement,
}
pub struct HistoryStore {
    connection: Connection,
}

pub struct HistorySession {
    pub scope: HistoryScope,
    store: Option<HistoryStore>,
    pub warning: Option<AppError>,
}
impl HistorySession {
    pub fn new(be: &Backend, profile: Option<i64>, group: &str, url: &str) -> Self {
        let (store, warning) = match HistoryStore::current() {
            Ok(store) => (Some(store), None),
            Err(error) => (None, Some(error)),
        };
        let result = Self {
            scope: HistoryScope::new(be, profile, group, url),
            store,
            warning,
        };
        if let Some(error) = &result.warning {
            crate::ui::warn(&format!("测量历史不可用，本轮网络测试继续：{error}"));
        }
        result
    }
    fn record_many(&mut self, be: &Backend, measurements: &[(String, Measurement)]) {
        if self.warning.is_some() || measurements.is_empty() {
            return;
        }
        let result = if self.scope.profile != crate::appstate::current_profile_id(be) {
            Err(AppError::new(ErrorCode::ProfileChanged, Stage::State))
        } else {
            self.store
                .as_mut()
                .ok_or_else(unavailable)
                .and_then(|store| store.record_many(&self.scope, measurements, now()))
        };
        if let Err(error) = result {
            crate::ui::warn(&format!("测量历史未写入，本轮网络测试继续：{error}"));
            self.warning = Some(error);
        }
    }
    pub fn record_probes(&mut self, be: &Backend, report: &crate::probe_scheduler::ProbeRun) {
        if report.controller_error.is_some() {
            return;
        }
        let measurements = report
            .measurements
            .iter()
            .filter(|m| {
                m.error
                    .as_ref()
                    .is_none_or(|e| e.code != ErrorCode::Cancelled)
            })
            .map(|m| {
                (
                    m.node.clone(),
                    Measurement {
                        stage: Stage::Probe,
                        availability: None,
                        delay_ms: m.delay_ms.map(|ms| ms as f64),
                        error_code: m.error.as_ref().map(|e| e.code),
                        http_status: m.error.as_ref().and_then(|e| e.http_status),
                        samples_requested: 0,
                        samples_passed: 0,
                    },
                )
            })
            .collect::<Vec<_>>();
        self.record_many(be, &measurements);
    }
    pub fn record_verification(&mut self, be: &Backend, node: &str, report: &VerificationReport) {
        if report.error.as_ref().is_some_and(|error| {
            matches!(
                error.code,
                ErrorCode::Cancelled
                    | ErrorCode::ControllerAuth
                    | ErrorCode::IdentityMismatch
                    | ErrorCode::InvalidInput
                    | ErrorCode::StateUnavailable
                    | ErrorCode::ProfileChanged
                    | ErrorCode::SelectionConflict
                    | ErrorCode::ClientUnavailable
                    | ErrorCode::ControllerHttp
                    | ErrorCode::ControllerTimeout
                    | ErrorCode::RollbackFailed
            )
        }) {
            return;
        }
        self.record_many(be, &[(node.into(), Measurement::verification(report))]);
    }
    pub fn recent(&self, node: &str) -> AppResult<Vec<HistoryEntry>> {
        self.store
            .as_ref()
            .ok_or_else(unavailable)?
            .recent(&self.scope, node, now())
    }
    pub fn prior(&mut self, node: &str) -> Vec<HistoryEntry> {
        if self.warning.is_some() {
            return vec![];
        }
        match self.recent(node) {
            Ok(rows) => rows,
            Err(error) => {
                crate::ui::warn(&format!("测量历史读取失败，评分采用冷启动先验：{error}"));
                self.warning = Some(error);
                vec![]
            }
        }
    }
}
impl HistoryStore {
    pub fn current() -> AppResult<Self> {
        Self::open(&crate::operation::state_directory()?.join("history.sqlite3"))
    }
    pub fn open(path: &Path) -> AppResult<Self> {
        if path.exists()
            && std::fs::metadata(path)
                .map_err(|_| unavailable())?
                .permissions()
                .readonly()
        {
            return Err(unavailable());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| unavailable())?;
        }
        let mut connection = Connection::open(path).map_err(|_| unavailable())?;
        connection
            .busy_timeout(std::time::Duration::from_millis(100))
            .map_err(|_| unavailable())?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| unavailable())?;
        if version == 0 {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            let version: i64 = transaction
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .map_err(|_| unavailable())?;
            if version == 0 {
                let tables:i64=transaction.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|row|row.get(0)).map_err(|_|unavailable())?;
                if tables != 0 {
                    return Err(unavailable());
                }
                transaction.execute_batch("CREATE TABLE measurements(id INTEGER PRIMARY KEY,scope TEXT NOT NULL,target TEXT NOT NULL,node TEXT NOT NULL,timestamp INTEGER NOT NULL,evidence TEXT NOT NULL);CREATE INDEX measurements_lookup ON measurements(scope,target,node,timestamp);CREATE INDEX measurements_time ON measurements(timestamp);").map_err(|_|unavailable())?;
                transaction
                    .pragma_update(None, "application_id", APPLICATION_ID)
                    .map_err(|_| unavailable())?;
                transaction
                    .pragma_update(None, "user_version", 1)
                    .map_err(|_| unavailable())?;
            }
            transaction.commit().map_err(|_| unavailable())?;
        } else if version != 1 {
            return Err(unavailable());
        }
        let application: i64 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(|_| unavailable())?;
        let final_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| unavailable())?;
        if application != APPLICATION_ID || final_version != 1 {
            return Err(unavailable());
        }
        Ok(Self { connection })
    }
    #[cfg(test)]
    pub fn record(
        &mut self,
        scope: &HistoryScope,
        node: &str,
        measurement: &Measurement,
        time: i64,
    ) -> AppResult<()> {
        self.record_many(scope, &[(node.into(), measurement.clone())], time)
    }
    pub fn record_many(
        &mut self,
        scope: &HistoryScope,
        measurements: &[(String, Measurement)],
        time: i64,
    ) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| unavailable())?;
        for (node, measurement) in measurements {
            if !measurement.valid() {
                return Err(unavailable());
            }
            transaction.execute("INSERT INTO measurements(scope,target,node,timestamp,evidence) VALUES(?1,?2,?3,?4,?5)",params![scope.identity,scope.target,fingerprint(node),time,serde_json::to_string(measurement).map_err(|_|unavailable())?]).map_err(|_|unavailable())?;
        }
        transaction
            .execute(
                "DELETE FROM measurements WHERE timestamp < ?1",
                [time.saturating_sub(RETENTION_SECONDS)],
            )
            .map_err(|_| unavailable())?;
        transaction.execute("DELETE FROM measurements WHERE id NOT IN (SELECT id FROM measurements ORDER BY timestamp DESC,id DESC LIMIT 10000)",[]).map_err(|_|unavailable())?;
        transaction.commit().map_err(|_| unavailable())
    }
    fn query(
        &self,
        scope: Option<&HistoryScope>,
        node: Option<&str>,
        time: i64,
        limit: usize,
    ) -> AppResult<Vec<HistoryEntry>> {
        let mut statement=self.connection.prepare("SELECT scope,target,node,timestamp,evidence FROM measurements WHERE timestamp>=?1 AND timestamp<=?2 AND (?3 IS NULL OR scope=?3) AND (?4 IS NULL OR target=?4) AND (?5 IS NULL OR node=?5) ORDER BY timestamp DESC,id DESC LIMIT ?6").map_err(|_|unavailable())?;
        let mut rows = statement
            .query(params![
                time.saturating_sub(RETENTION_SECONDS),
                time,
                scope.map(|s| &s.identity),
                scope.map(|s| &s.target),
                node,
                limit as i64
            ])
            .map_err(|_| unavailable())?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().map_err(|_| unavailable())? {
            let evidence: String = row.get(4).map_err(|_| unavailable())?;
            let measurement: Measurement =
                serde_json::from_str(&evidence).map_err(|_| unavailable())?;
            if !measurement.valid() {
                return Err(unavailable());
            }
            let entry = HistoryEntry {
                scope: row.get(0).map_err(|_| unavailable())?,
                target: row.get(1).map_err(|_| unavailable())?,
                node: row.get(2).map_err(|_| unavailable())?,
                timestamp: row.get(3).map_err(|_| unavailable())?,
                measurement,
            };
            if [&entry.scope, &entry.target, &entry.node]
                .iter()
                .any(|key| key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(unavailable());
            }
            results.push(entry);
        }
        Ok(results)
    }
    pub fn recent(
        &self,
        scope: &HistoryScope,
        node: &str,
        time: i64,
    ) -> AppResult<Vec<HistoryEntry>> {
        self.query(Some(scope), Some(&fingerprint(node)), time, 10000)
    }
    pub fn recent_entry(&self, entry: &HistoryEntry, time: i64) -> AppResult<Vec<HistoryEntry>> {
        let scope = HistoryScope {
            identity: entry.scope.clone(),
            target: entry.target.clone(),
            profile: None,
        };
        self.query(Some(&scope), Some(&entry.node), time, 10000)
    }
    pub fn list(&self, time: i64) -> AppResult<Vec<HistoryEntry>> {
        self.query(None, None, time, 100)
    }
    pub fn clear(&mut self) -> AppResult<()> {
        self.connection
            .execute("DELETE FROM measurements", [])
            .map(|_| ())
            .map_err(|_| unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client_config::ClientKind,
        core_api::ApiEndpoint,
        diagnostics::{Availability, ErrorCode, Stage},
    };
    use std::path::Path;
    fn fixture(run: impl FnOnce(&Path)) {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        run(&root);
        std::fs::remove_dir_all(root).unwrap();
    }
    fn backend() -> crate::Backend {
        crate::Backend {
            kind: Some(ClientKind::ClashVerge),
            client: "test".into(),
            api: ApiEndpoint::NamedPipe(r"\\.\pipe\one".into()),
            secret: Some("test-secret-token".into()),
            proxy: "http://user:proxy-password@127.0.0.1:7897".into(),
            source: r"C:\Users\someone\configuration.yaml".into(),
            version: None,
            alternatives: vec![],
        }
    }
    fn evidence() -> Measurement {
        Measurement {
            stage: Stage::Verification,
            availability: Some(Availability::Webpage),
            delay_ms: Some(100.0),
            error_code: None,
            http_status: Some(200),
            samples_requested: 2,
            samples_passed: 2,
        }
    }
    #[test]
    fn identities_targets_nodes_and_profiles_are_isolated_without_secrets() {
        fixture(|root| {
            let path = root.join("history.sqlite3");
            let mut store = HistoryStore::open(&path).unwrap();
            let mut be = backend();
            let scope = HistoryScope::new(
                &be,
                Some(1),
                "group-secret",
                "https://user:password@host.invalid/path?token=private",
            );
            store
                .record(&scope, "node-uuid-secret", &evidence(), 1_000_000)
                .unwrap();
            assert_eq!(
                store
                    .recent(&scope, "node-uuid-secret", 1_000_000)
                    .unwrap()
                    .len(),
                1
            );
            assert!(store.recent(&scope, "other", 1_000_000).unwrap().is_empty());
            be.api = ApiEndpoint::NamedPipe(r"\\.\pipe\two".into());
            assert_eq!(
                scope,
                HistoryScope::new(
                    &be,
                    Some(1),
                    "group-secret",
                    "https://user:password@host.invalid/path?token=private"
                )
            );
            let mut other_source = be.clone();
            other_source.source = r"C:\Users\someone\other.yaml".into();
            assert_ne!(
                scope,
                HistoryScope::new(
                    &other_source,
                    Some(1),
                    "group-secret",
                    "https://user:password@host.invalid/path?token=private"
                )
            );
            for other in [
                HistoryScope::new(
                    &be,
                    Some(2),
                    "group-secret",
                    "https://user:password@host.invalid/path?token=private",
                ),
                HistoryScope::new(
                    &be,
                    Some(1),
                    "other",
                    "https://user:password@host.invalid/path?token=private",
                ),
                HistoryScope::new(&be, Some(1), "group-secret", "https://host.invalid/other"),
            ] {
                assert!(store
                    .recent(&other, "node-uuid-secret", 1_000_000)
                    .unwrap()
                    .is_empty());
            }
            be.kind = Some(ClientKind::CuteCloud);
            assert_ne!(
                scope,
                HistoryScope::new(
                    &be,
                    Some(1),
                    "group-secret",
                    "https://user:password@host.invalid/path?token=private"
                )
            );
            be.kind = None;
            be.api = ApiEndpoint::Http("http://127.0.0.1:9001".into());
            let one = HistoryScope::new(&be, None, "g", "https://host.invalid/");
            be.api = ApiEndpoint::Http("http://127.0.0.1:9002".into());
            assert_ne!(
                one,
                HistoryScope::new(&be, None, "g", "https://host.invalid/")
            );
            drop(store);
            for file in std::fs::read_dir(root).unwrap() {
                let bytes = std::fs::read(file.unwrap().path()).unwrap();
                for secret in [
                    "test-secret-token",
                    "proxy-password",
                    "group-secret",
                    "node-uuid-secret",
                    "password",
                    "token=private",
                    "someone",
                    "host.invalid",
                ] {
                    assert!(
                        !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                        "leaked {secret}"
                    );
                }
            }
        });
    }
    #[test]
    fn retention_size_limits_and_stages_remain_distinct() {
        fixture(|root| {
            let mut store = HistoryStore::open(&root.join("history.sqlite3")).unwrap();
            let scope = HistoryScope::new(&backend(), Some(1), "g", "https://test.invalid");
            let now = 4_000_000;
            store
                .record(&scope, "n", &evidence(), now - RETENTION_SECONDS - 1)
                .unwrap();
            let probe = Measurement {
                stage: Stage::Probe,
                availability: None,
                delay_ms: Some(10.0),
                error_code: None,
                http_status: None,
                samples_requested: 0,
                samples_passed: 0,
            };
            store.record(&scope, "n", &probe, now).unwrap();
            let rows = store.recent(&scope, "n", now).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].measurement.stage, Stage::Probe);
            assert_eq!(rows[0].measurement.availability, None);
            store
                .record_many(
                    &scope,
                    &(0..10_005)
                        .map(|_| ("n".into(), evidence()))
                        .collect::<Vec<_>>(),
                    now,
                )
                .unwrap();
            assert_eq!(store.recent(&scope, "n", now).unwrap().len(), 10_000);
            let latest = store.list(now).unwrap().remove(0);
            assert_eq!(store.recent_entry(&latest, now).unwrap().len(), 10_000);
            assert_eq!(
                store
                    .connection
                    .query_row("SELECT count(*) FROM measurements", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                10_000
            );
            assert!(store
                .recent(&scope, "n", now + RETENTION_SECONDS + 1)
                .unwrap()
                .is_empty());
            store
                .record(&scope, "n", &evidence(), now + RETENTION_SECONDS + 1)
                .unwrap();
            assert_eq!(
                store
                    .connection
                    .query_row("SELECT count(*) FROM measurements", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            store.clear().unwrap();
            assert!(store.list(now + RETENTION_SECONDS + 1).unwrap().is_empty());
        });
    }
    #[test]
    fn damaged_unknown_schema_locked_and_readonly_databases_degrade_safely() {
        fixture(|root| {
            let broken = root.join("broken.sqlite3");
            std::fs::write(&broken, b"damaged-evidence").unwrap();
            assert!(HistoryStore::open(&broken).is_err());
            assert_eq!(std::fs::read(&broken).unwrap(), b"damaged-evidence");
            let unknown = root.join("unknown.sqlite3");
            let db = rusqlite::Connection::open(&unknown).unwrap();
            db.pragma_update(None, "user_version", 99).unwrap();
            drop(db);
            assert!(HistoryStore::open(&unknown).is_err());
            let path = root.join("history.sqlite3");
            let mut store = HistoryStore::open(&path).unwrap();
            let lock = rusqlite::Connection::open(&path).unwrap();
            lock.execute_batch("BEGIN IMMEDIATE").unwrap();
            let scope = HistoryScope::new(&backend(), None, "g", "https://test.invalid");
            let started = std::time::Instant::now();
            assert!(store.record(&scope, "n", &evidence(), 1).is_err());
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
            lock.execute_batch("ROLLBACK").unwrap();
            drop(lock);
            drop(store);
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&path, permissions).unwrap();
            assert!(HistoryStore::open(&path).is_err());
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions).unwrap();
            let error = Measurement {
                stage: Stage::Verification,
                availability: Some(Availability::Failed),
                delay_ms: None,
                error_code: Some(ErrorCode::Tls),
                http_status: None,
                samples_requested: 2,
                samples_passed: 0,
            };
            assert!(error.delay_ms.is_none());
        });
    }
    #[test]
    fn invalid_measurement_batch_rolls_back_without_corrupting_prior_history() {
        fixture(|root| {
            let mut store = HistoryStore::open(&root.join("history.sqlite3")).unwrap();
            let scope = HistoryScope::new(&backend(), None, "g", "https://test.invalid");
            store.record(&scope, "n", &evidence(), 10).unwrap();
            let mut invalid = evidence();
            invalid.delay_ms = Some(f64::NAN);
            assert!(store
                .record_many(
                    &scope,
                    &[("n".into(), evidence()), ("n".into(), invalid)],
                    11
                )
                .is_err());
            assert_eq!(store.recent(&scope, "n", 11).unwrap().len(), 1);
            let mut invalid = evidence();
            invalid.http_status = Some(0);
            assert!(store.record(&scope, "n", &invalid, 11).is_err());
        });
    }
}
