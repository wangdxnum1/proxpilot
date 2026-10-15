//! Share-safe whitelist. Never serialize backend/config/error strings into this report.
use crate::{
    diagnostics::{AppError, AppResult, ErrorCode, Stage},
    Args,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Write, path::Path};
#[derive(Default)]
struct Aliases(BTreeMap<String, String>);
impl Aliases {
    fn get(&mut self, kind: &str, value: &str) -> String {
        let key = format!("{kind}:{value}");
        if let Some(alias) = self.0.get(&key) {
            return alias.clone();
        }
        let alias = format!(
            "{kind}-{}",
            self.0
                .values()
                .filter(|a| a.starts_with(&format!("{kind}-")))
                .count()
                + 1
        );
        self.0.insert(key, alias.clone());
        alias
    }
}
fn safe_version(value: &str) -> Option<&str> {
    let numeric = value.strip_prefix('v').unwrap_or(value);
    (numeric.len() <= 32
        && numeric.split('.').count() >= 2
        && numeric
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
    .then_some(value)
}
fn history_summary() -> AppResult<Value> {
    let path = crate::operation::state_directory()?.join("history.sqlite3");
    if !path.exists() {
        return Ok(json!({"available":false,"reason":"not_created"}));
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| unavailable())?;
    db.busy_timeout(std::time::Duration::from_millis(100))
        .map_err(|_| unavailable())?;
    let app: i64 = db
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(|_| unavailable())?;
    let version: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| unavailable())?;
    if app != 0x50504831 || version != 1 {
        return Err(unavailable());
    }
    let count: i64 = db
        .query_row("SELECT count(*) FROM measurements", [], |r| r.get(0))
        .map_err(|_| unavailable())?;
    let mut query = db
        .prepare("SELECT evidence FROM measurements ORDER BY id DESC LIMIT 100")
        .map_err(|_| unavailable())?;
    let mut errors = Vec::new();
    for bytes in query
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| unavailable())?
    {
        if let Ok(evidence) =
            serde_json::from_str::<crate::history::Measurement>(&bytes.map_err(|_| unavailable())?)
        {
            if let Some(code) = evidence.error_code {
                let value = serde_json::to_value(code).unwrap();
                if !errors.contains(&value) {
                    errors.push(value);
                }
            }
        }
    }
    Ok(
        json!({"available":true,"schema_version":version,"records":count,"recent_error_categories":errors}),
    )
}
fn unavailable() -> AppError {
    AppError::new(ErrorCode::StateUnavailable, Stage::State)
}
pub fn collect(args: &Args) -> Value {
    let mut aliases = Aliases::default();
    let mut warnings = Vec::<AppError>::new();
    let dependencies: Value =
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/provenance.json")))
            .expect("embedded provenance");
    let source_integrity = source_integrity(&dependencies, Path::new("."));
    let api = crate::core_api::CoreApi::new().ok();
    let paths = crate::clients::DiscoveryPaths::current().ok();
    let records = if let (Some(api), Some(paths)) = (&api, &paths) {
        if args.api.is_some() {
            vec![crate::clients::DiscoveryRecord {
                name: "explicit".into(),
                backend: crate::detect::detect(api, args).ok(),
                error: None,
            }]
        } else {
            crate::clients::discover(api, paths)
        }
    } else {
        vec![]
    };
    let active = crate::sysproxy::registry_proxy_server();
    let enabled = crate::sysproxy::proxy_enabled();
    let mut clients = Vec::new();
    for record in records {
        if let (Some(be), Some(api)) = (record.backend, api.as_ref()) {
            let identity = be.kind.map(|k| k.name()).unwrap_or("mihomo");
            let endpoint = aliases.get("endpoint", &be.api.to_string());
            let mut groups = Vec::new();
            match crate::mihomo::get_proxies_detailed(&be, api) {
                Ok((proxies, _)) => {
                    let mut names: Vec<_> = proxies.keys().collect();
                    names.sort();
                    for name in names {
                        let info = &proxies[name];
                        if let Some(members) = &info.all {
                            groups.push(json!({"group":aliases.get("group",name),"member_count":members.len(),"selected":info.now.as_deref().map(|n|aliases.get("node",n)),"nodes":members.iter().map(|n|aliases.get("node",n)).collect::<Vec<_>>()}));
                        }
                    }
                }
                Err(error) => warnings.push(AppError::new(error.code, error.stage)),
            }
            clients.push(json!({"client":identity,"available":true,"endpoint":endpoint,"controller_type":match be.api {crate::core_api::ApiEndpoint::Http(_)=>"http",_=>"named_pipe"},"version":be.version.as_deref().and_then(safe_version),"system_proxy_matches":enabled&&active.as_deref().is_some_and(|s|crate::clients::proxy_matches(&be.proxy,s)),"profile":crate::appstate::current_profile_id(&be).map(|p|aliases.get("profile",&p.to_string())),"managed_subscription":if be.kind==Some(crate::client_config::ClientKind::CuteCloud){paths.as_ref().and_then(crate::clients::cutecloud_managed_profile)}else{None},"groups":groups}));
        } else {
            clients.push(json!({"available":false,"reason":"controller_unavailable"}));
        }
    }
    let recovery = match crate::switching::inspect_recovery() {
        Ok(Some(r)) => {
            json!({"pending":true,"committed":r.committed,"group":aliases.get("group",&r.group),"original":r.original.map(|n|aliases.get("node",&n)),"expected":aliases.get("node",&r.expected),"profile":r.profile.map(|p|aliases.get("profile",&p.to_string()))})
        }
        Ok(None) => json!({"pending":false}),
        Err(error) => {
            warnings.push(AppError::new(error.code, error.stage));
            json!({"available":false})
        }
    };
    let history = history_summary().unwrap_or_else(|e| {
        warnings.push(e);
        json!({"available":false})
    });
    let partial = clients.is_empty()
        || clients.iter().any(|c| c["available"] != true)
        || !warnings.is_empty();
    json!({"schema_version":1,"partial":partial,"program":{"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"dependencies":dependencies,"source_integrity":source_integrity,"clients":clients,"system_proxy":{"enabled":enabled},"recovery":recovery,"history":history,"warnings":warnings})
}
pub fn source_integrity(dependencies: &Value, root: &Path) -> Value {
    let package = root.join("vendor/btls-sys/prebuilt/x86_64-pc-windows-msvc");
    if !package.join("manifest.json").is_file() {
        return json!({"available":false,"scope":"source_files","reason":"not_present"});
    }
    use sha2::{Digest, Sha256};
    let manifest = std::fs::read(package.join("manifest.json")).ok();
    let manifest_matches = manifest.as_ref().is_some_and(|b| {
        Some(format!("{:x}", Sha256::digest(b)))
            == dependencies["manifest_sha256"].as_str().map(str::to_owned)
    });
    let verified = manifest_matches
        && dependencies["files"].as_object().is_some_and(|files| {
            files.iter().all(|(name, e)| {
                matches!(
                    name.as_str(),
                    "lib/crypto.lib" | "lib/ssl.lib" | "bindings.rs" | "LICENSE-BORINGSSL"
                ) && std::fs::read(package.join(name)).ok().is_some_and(|b| {
                    Some(b.len() as u64) == e["size"].as_u64()
                        && Some(format!("{:x}", Sha256::digest(&b)))
                            == e["sha256"].as_str().map(str::to_owned)
                })
            })
        });
    json!({"available":true,"scope":"source_files","verified":verified})
}
pub fn write_report(path: &Path, overwrite: bool, value: &Value) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| unavailable())?;
    bytes.push(b'\n');
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options.open(path).map_err(|_| unavailable())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| unavailable())
}
#[cfg(test)]
mod tests {
    #[test]
    fn version_whitelist_discards_arbitrary_names_and_urls() {
        for s in [
            "secret",
            "v1.2.secret",
            "v1.2.3?token=abc",
            "v1.2.3\nCookie:x",
        ] {
            assert_eq!(super::safe_version(s), None);
        }
        assert_eq!(super::safe_version("v1.19.32"), Some("v1.19.32"));
    }
    #[test]
    fn aliases_never_contain_input_and_are_report_stable() {
        let mut a = super::Aliases::default();
        let first = a.get("node", "password");
        assert!(!first.contains("password"));
        assert_eq!(first, a.get("node", "password"));
        assert_ne!(first, a.get("node", "another"));
    }
}
