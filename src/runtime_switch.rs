//! The live-core implementation of the switching transaction's small control interface.
use crate::{
    appstate,
    core_api::CoreApi,
    detect::Backend,
    diagnostics::{AppError, AppResult, ErrorCode, Stage},
    switching::{LiveState, MemorySync, SwitchControl, SwitchSnapshot},
};

pub struct KernelSwitchControl<'a> {
    pub backend: &'a Backend,
    pub api: &'a CoreApi,
}

impl KernelSwitchControl<'_> {
    pub fn identity(&self, pid: Option<u32>) -> String {
        crate::history::fingerprint(
            &serde_json::json!([
                self.backend.kind.map(|k| k.name()).unwrap_or("explicit"),
                self.backend.source,
                self.backend.api.to_string(),
                pid
            ])
            .to_string(),
        )
    }
}

impl SwitchControl for KernelSwitchControl<'_> {
    fn proxy_snapshot(&self) -> AppResult<Option<crate::sysproxy::ProxySnapshot>> {
        crate::sysproxy::snapshot().map(Some)
    }
    fn target_proxy_snapshot(&self) -> AppResult<Option<crate::sysproxy::ProxySnapshot>> {
        crate::sysproxy::target_snapshot(&self.backend.proxy).map(Some)
    }
    fn enable_proxy(&self) -> AppResult<()> {
        crate::sysproxy::enable_for(&self.backend.proxy)
            .map_err(|_| AppError::new(ErrorCode::StateUnavailable, Stage::Switching))
    }
    fn restore_proxy(
        &self,
        before: &crate::sysproxy::ProxySnapshot,
        written: &crate::sysproxy::ProxySnapshot,
    ) -> AppResult<bool> {
        crate::sysproxy::restore_if_ours(before, written)
    }
    fn state(&self, group: &str) -> AppResult<LiveState> {
        let before = appstate::current_profile_id(self.backend);
        let (proxies, pid) = crate::mihomo::get_proxies_detailed(self.backend, self.api)?;
        let after = appstate::current_profile_id(self.backend);
        if before != after {
            return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
        }
        let group_info = proxies
            .get(group)
            .filter(|p| p.all.is_some())
            .ok_or_else(|| AppError::new(ErrorCode::ProfileChanged, Stage::Switching))?;
        Ok(LiveState {
            identity: self.identity(pid),
            profile: before,
            members: group_info.all.clone().unwrap_or_default(),
            selection: group_info.now.clone(),
        })
    }
    fn select(&self, group: &str, target: &str) -> AppResult<()> {
        let path = format!("/proxies/{}", crate::mihomo::enc_path(group));
        let body = serde_json::to_vec(&serde_json::json!({"name":target}))
            .map_err(|_| AppError::new(ErrorCode::InvalidInput, Stage::Switching))?;
        self.api.request_detailed(
            self.backend,
            reqwest::Method::PUT,
            &path,
            Some(body),
            std::time::Duration::from_secs(10),
        )?;
        Ok(())
    }
    fn persist(&self, snapshot: &SwitchSnapshot, target: &str) -> AppResult<MemorySync> {
        appstate::sync_selection_captured(
            self.backend,
            snapshot.state.profile,
            &snapshot.group,
            target,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diagnostics::{Availability, VerificationReport},
        http::test_support::{accept, listener, read_request},
        operation::Cancellation,
        switching::SwitchTransaction,
    };
    use std::io::Write;
    #[test]
    fn runtime_identity_does_not_serialize_credentials_or_user_paths() {
        let api = CoreApi::new().unwrap();
        let mut be = Backend {
            kind: None,
            client: "test".into(),
            api: "http://user:password@127.0.0.1:9000/private?token=secret"
                .to_string()
                .into(),
            secret: Some("bearer-secret".into()),
            proxy: String::new(),
            source: r"C:\Users\private-user\config.yaml".into(),
            version: None,
            alternatives: vec![],
        };
        let one = KernelSwitchControl {
            backend: &be,
            api: &api,
        }
        .identity(Some(1));
        assert_eq!(one.len(), 64);
        for value in ["password", "token", "private-user", "bearer-secret"] {
            assert!(!one.contains(value));
        }
        assert_ne!(
            one,
            KernelSwitchControl {
                backend: &be,
                api: &api
            }
            .identity(Some(2))
        );
        be.source = r"C:\other\config.yaml".into();
        assert_ne!(
            one,
            KernelSwitchControl {
                backend: &be,
                api: &api
            }
            .identity(Some(1))
        );
    }

    #[test]
    fn live_http_adapter_checks_state_and_commits_without_client_memory() {
        let listener = listener();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (finished, shutdown) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for selected in [
                Some("original"),
                Some("original"),
                None,
                Some("target"),
                Some("target"),
            ] {
                let mut stream = accept(&listener);
                requests.push(read_request(&mut stream));
                if let Some(selected) = selected {
                    let body = serde_json::json!({"proxies":{"test-group":{"type":"Selector","all":["original","target"],"now":selected},"original":{"type":"SS"},"target":{"type":"SS"}}}).to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                } else {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .unwrap();
                }
            }
            // Keep the listening process alive until the last response identity is read.
            let _ = shutdown.recv_timeout(std::time::Duration::from_secs(5));
            requests
        });
        let backend = Backend {
            kind: None,
            client: "fixture".into(),
            api: endpoint.into(),
            secret: None,
            proxy: "http://127.0.0.1:1".into(),
            source: "fixture".into(),
            version: None,
            alternatives: vec![],
        };
        let api = CoreApi::new().unwrap();
        let control = KernelSwitchControl {
            backend: &backend,
            api: &api,
        };
        let root = std::env::temp_dir().join(format!(
            "proxpilot-runtime-txn-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut transaction =
            SwitchTransaction::begin_at(&control, "test-group", Cancellation::new(), &root)
                .unwrap();
        assert_eq!(
            transaction.snapshot().state.identity,
            control.identity(Some(std::process::id()))
        );
        transaction.select("target").unwrap();
        let result = transaction
            .commit(&VerificationReport {
                availability: Availability::Webpage,
                mean_ms: Some(25.0),
                samples_requested: 2,
                samples_passed: 2,
                error: None,
            })
            .unwrap();
        assert_eq!(result.memory, Some(MemorySync::NotAvailable));
        assert!(result.warnings.is_empty());
        finished.send(()).unwrap();
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("PUT "))
                .count(),
            1
        );
        assert!(requests[2].starts_with("PUT /proxies/test-group "));
        assert!(!root.join("recovery.bin").exists());
        drop(transaction);
        std::fs::remove_dir_all(root).unwrap();
    }
}
