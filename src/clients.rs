use crate::{
    client_config::{self, ClientKind, ClientSelection},
    core_api::{ApiEndpoint, CoreApi},
    detect::Backend,
    Args,
};
use std::path::{Path, PathBuf};

pub struct DiscoveryPaths {
    pub cutecloud: PathBuf,
    pub clash_verge: PathBuf,
}
impl DiscoveryPaths {
    pub fn current() -> Result<Self, String> {
        let base = PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA 未设置")?);
        Ok(Self {
            cutecloud: base.join("CuteCloud/CuteCloud"),
            clash_verge: base.join("io.github.clash-verge-rev.clash-verge-rev"),
        })
    }
    pub(crate) fn runtime(&self, kind: ClientKind) -> PathBuf {
        match kind {
            ClientKind::CuteCloud => self.cutecloud.join("config.yaml"),
            ClientKind::ClashVerge => {
                let runtime = self.clash_verge.join("clash-verge.yaml");
                if runtime.exists() {
                    runtime
                } else {
                    self.clash_verge.join("config.yaml")
                }
            }
        }
    }
}
pub struct DiscoveryRecord {
    pub name: String,
    pub backend: Option<Backend>,
    pub error: Option<String>,
}
fn kind_of(info: crate::procinfo::ClientInfo) -> Option<ClientKind> {
    match info.name.as_str() {
        "CuteCloud" => Some(ClientKind::CuteCloud),
        "Clash Verge" => Some(ClientKind::ClashVerge),
        _ => None,
    }
}
fn identify(port: u16) -> Option<ClientKind> {
    kind_of(crate::procinfo::identify_client(port))
}
fn identify_endpoint(url: &str) -> Option<ClientKind> {
    let url = reqwest::Url::parse(url).ok()?;
    let pid = crate::procinfo::find_listener_pid_at(url.host_str()?, url.port_or_known_default()?);
    kind_of(crate::procinfo::identify_pid(pid))
}
pub fn validate_endpoint_identity(kind: Option<ClientKind>, url: &str) -> Result<(), String> {
    let (_, local, _) = http_endpoint(url)?;
    validate_explicit_identity(
        kind,
        local,
        if local { identify_endpoint(url) } else { None },
    )
}
fn http_endpoint(value: &str) -> Result<(String, bool, u16), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "API 地址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("API 必须是无用户名密码的 HTTP(S) 地址".into());
    }
    let host = url.host_str().ok_or("API 缺少主机")?;
    let local = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    Ok((
        value.trim_end_matches('/').into(),
        local,
        url.port_or_known_default().ok_or("API 缺少端口")?,
    ))
}
pub fn parse_runtime(kind: ClientKind, text: &str) -> Result<Backend, String> {
    #[derive(serde::Deserialize)]
    struct Runtime {
        #[serde(default, rename = "external-controller")]
        controller: String,
        #[serde(default, rename = "external-controller-pipe")]
        pipe: String,
        #[serde(default)]
        secret: Option<String>,
    }
    let cfg: Runtime =
        serde_yaml_ng::from_str(text).map_err(|_| "运行配置 YAML 无效（内容未输出）")?;
    let api = if !cfg.pipe.trim().is_empty() {
        ApiEndpoint::NamedPipe(PathBuf::from(cfg.pipe))
    } else {
        if cfg.controller.trim().is_empty() {
            return Err("运行配置没有启用控制接口".into());
        }
        let controller = cfg
            .controller
            .replace("0.0.0.0:", "127.0.0.1:")
            .replace("[::]:", "[::1]:");
        let value = if controller.contains("://") {
            controller
        } else {
            format!("http://{}", controller)
        };
        let (url, local, _) = http_endpoint(&value)?;
        if !local {
            return Err("客户端运行配置控制接口必须为本机地址".into());
        }
        ApiEndpoint::Http(url)
    };
    Ok(Backend {
        kind: Some(kind),
        client: kind.display_name().into(),
        api,
        secret: cfg.secret.filter(|s| !s.is_empty()),
        proxy: String::new(),
        source: "运行配置".into(),
        version: None,
        alternatives: vec![],
    })
}
fn configured(kind: ClientKind, paths: &DiscoveryPaths) -> Result<Backend, String> {
    let path = paths.runtime(kind);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 {} 失败：{}", path.display(), e))?;
    let mut be = parse_runtime(kind, &text)?;
    be.source = path.display().to_string();
    Ok(be)
}
pub fn validate_explicit_identity(
    expected: Option<ClientKind>,
    local: bool,
    actual: Option<ClientKind>,
) -> Result<(), String> {
    if let Some(expected) = expected {
        if !local || actual != Some(expected) {
            return Err(format!(
                "控制接口身份不匹配或无法确认；目标为 {}",
                expected.name()
            ));
        }
    }
    Ok(())
}
fn verify(be: Backend, api: &CoreApi) -> Result<Backend, String> {
    verify_with_proxy(be, api, None)
}
fn verify_with_proxy(
    mut be: Backend,
    api: &CoreApi,
    proxy: Option<&str>,
) -> Result<Backend, String> {
    if let ApiEndpoint::Http(url) = &be.api {
        validate_endpoint_identity(be.kind, url)?;
    }
    be.version = Some(crate::mihomo::get_version(&be, api)?);
    be.proxy = proxy
        .map(|p| Ok(p.to_string()))
        .unwrap_or_else(|| crate::mihomo::get_runtime_proxy(&be, api))?;
    Ok(be)
}
pub fn discover(api: &CoreApi, paths: &DiscoveryPaths) -> Vec<DiscoveryRecord> {
    discover_with_secret(api, paths, None)
}
fn discover_with_secret(
    api: &CoreApi,
    paths: &DiscoveryPaths,
    secret: Option<&str>,
) -> Vec<DiscoveryRecord> {
    let mut records = Vec::new();
    for kind in [ClientKind::CuteCloud, ClientKind::ClashVerge] {
        match configured(kind, paths).and_then(|mut be| {
            if let Some(secret) = secret {
                be.secret = Some(secret.into());
            }
            verify(be, api)
        }) {
            Ok(be) => records.push(DiscoveryRecord {
                name: kind.display_name().into(),
                backend: Some(be),
                error: None,
            }),
            Err(e) => records.push(DiscoveryRecord {
                name: kind.display_name().into(),
                backend: None,
                error: Some(e),
            }),
        }
    }
    for port in [9090, 9097, 9091, 9094, 19090, 28090] {
        if crate::procinfo::find_listener_pid(port).is_none() {
            continue;
        }
        let kind = identify(port);
        if kind.is_some_and(|k| {
            records
                .iter()
                .any(|r| r.backend.as_ref().is_some_and(|b| b.kind == Some(k)))
        }) {
            continue;
        }
        let info = crate::procinfo::identify_client(port);
        let be = Backend {
            kind,
            client: info.name.clone(),
            api: format!("http://127.0.0.1:{}", port).into(),
            secret: secret.map(str::to_string).or_else(|| {
                kind.and_then(|k| configured(k, paths).ok())
                    .and_then(|b| b.secret)
            }),
            proxy: String::new(),
            source: "扫描监听端口".into(),
            version: None,
            alternatives: vec![],
        };
        if let Ok(be) = verify(be, api) {
            records.push(DiscoveryRecord {
                name: info.name,
                backend: Some(be),
                error: None,
            });
        }
    }
    deduplicate(&mut records);
    records
}
pub fn deduplicate(records: &mut Vec<DiscoveryRecord>) {
    let mut seen = std::collections::HashSet::new();
    records.retain(|r| {
        r.backend
            .as_ref()
            .is_none_or(|b| seen.insert((b.kind.map(|k| k.name()), b.proxy.clone())))
    });
}
pub fn proxy_matches(proxy: &str, registry: &str) -> bool {
    if proxy.starts_with("socks") {
        return false;
    }
    fn normalized(value: &str) -> Option<(String, u16)> {
        let value = value.trim();
        let value = if value.contains("://") {
            value.to_string()
        } else {
            format!("http://{}", value)
        };
        let url = reqwest::Url::parse(&value).ok()?;
        let host = url.host_str()?.trim_matches(['[', ']']);
        let host = if host.eq_ignore_ascii_case("localhost") {
            "127.0.0.1".into()
        } else {
            host.to_lowercase()
        };
        Some((host, url.port_or_known_default()?))
    }
    let keyed = registry.contains('=');
    if keyed {
        for required in ["http", "https"] {
            if !registry.split(';').any(|entry| {
                entry
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case(required))
            }) {
                return false;
            }
        }
    }
    let entries: Vec<&str> = registry
        .split(';')
        .filter_map(|v| {
            if keyed {
                let (protocol, address) = v.trim().split_once('=')?;
                if matches!(
                    protocol.trim().to_ascii_lowercase().as_str(),
                    "http" | "https"
                ) {
                    Some(address)
                } else {
                    None
                }
            } else {
                Some(v)
            }
        })
        .collect();
    normalized(proxy).is_some_and(|target| {
        !entries.is_empty()
            && entries
                .iter()
                .all(|v| normalized(v).as_ref() == Some(&target))
    })
}
pub fn select_auto(records: &[DiscoveryRecord], active: Option<&str>) -> Result<Backend, String> {
    let available: Vec<&Backend> = records.iter().filter_map(|r| r.backend.as_ref()).collect();
    if let Some(active) = active {
        let matched: Vec<_> = available
            .iter()
            .filter(|b| proxy_matches(&b.proxy, active))
            .collect();
        if matched.len() == 1 {
            return Ok((*matched[0]).clone());
        }
    }
    match available.as_slice() {
        [be] => Ok((*be).clone()),
        [] => Err("未发现可用内核；用 clients 查看详情".into()),
        _ => Err("多个客户端同时可用，无法唯一选择；请用 --client 或 --api 指定".into()),
    }
}
#[cfg(test)]
pub fn select_explicit(records: &[DiscoveryRecord], kind: ClientKind) -> Result<Backend, String> {
    let found: Vec<_> = records
        .iter()
        .filter_map(|r| r.backend.as_ref())
        .filter(|b| b.kind == Some(kind))
        .collect();
    match found.as_slice() {
        [be] => Ok((*be).clone()),
        _ => Err(format!(
            "{} 没有唯一可用内核；请确认启动或用 --api 指定",
            kind.name()
        )),
    }
}
pub fn choose_selection(
    explicit: Option<ClientSelection>,
    detect: bool,
    path: &Path,
) -> Result<ClientSelection, String> {
    if let Some(selection) = explicit {
        return Ok(selection);
    }
    if detect {
        return Ok(ClientSelection::Auto);
    }
    Ok(client_config::load_default(path)?
        .unwrap_or(ClientSelection::Explicit(ClientKind::CuteCloud)))
}
pub fn resolve_backend(
    args: &Args,
    api: &CoreApi,
    paths: &DiscoveryPaths,
) -> Result<Backend, String> {
    if let Some(url) = &args.api {
        let (url, local, _) = http_endpoint(url)?;
        let kind = if local { identify_endpoint(&url) } else { None };
        let expected = match args.client {
            Some(ClientSelection::Explicit(k)) => Some(k),
            _ => None,
        };
        validate_explicit_identity(expected, local, kind)?;
        let mut be = Backend {
            kind,
            client: kind
                .map(|k| k.display_name().to_string())
                .unwrap_or_else(|| "指定的 mihomo 内核".into()),
            api: url.into(),
            secret: args.secret.clone().or_else(|| {
                kind.and_then(|k| configured(k, paths).ok())
                    .and_then(|b| b.secret)
            }),
            proxy: String::new(),
            source: "--api".into(),
            version: None,
            alternatives: vec![],
        };
        be.version = Some(crate::mihomo::get_version(&be, api)?);
        be.proxy = args
            .proxy
            .clone()
            .map(Ok)
            .unwrap_or_else(|| crate::mihomo::get_runtime_proxy(&be, api))?;
        return Ok(be);
    }
    let selection = choose_selection(args.client, args.detect, &client_config::config_path()?)?;
    let mut be = match selection {
        ClientSelection::Explicit(kind) => {
            let mut be = configured(kind, paths)?;
            if args.secret.is_some() {
                be.secret = args.secret.clone();
            }
            verify_with_proxy(be, api, args.proxy.as_deref())?
        }
        ClientSelection::Auto => {
            let records = discover_with_secret(api, paths, args.secret.as_deref());
            let active = if crate::sysproxy::proxy_enabled() {
                crate::sysproxy::registry_proxy_server()
            } else {
                None
            };
            select_auto(&records, active.as_deref())?
        }
    };
    if let Some(proxy) = &args.proxy {
        be.proxy = proxy.clone();
    }
    Ok(be)
}
pub fn refresh(be: &Backend, args: &Args, api: &CoreApi) -> Result<Backend, String> {
    if args.api.is_some() || be.kind.is_none() {
        return verify_with_proxy(be.clone(), api, args.proxy.as_deref());
    }
    let next = refresh_candidate(be, args, &DiscoveryPaths::current()?)?;
    let mut next = verify_with_proxy(next, api, args.proxy.as_deref())?;
    if let Some(proxy) = &args.proxy {
        next.proxy = proxy.clone();
    }
    Ok(next)
}

fn refresh_candidate(be: &Backend, args: &Args, paths: &DiscoveryPaths) -> Result<Backend, String> {
    let mut next = if args.api.is_some() || be.kind.is_none() {
        be.clone()
    } else {
        configured(be.kind.unwrap(), paths)?
    };
    if args.secret.is_some() {
        next.secret = args.secret.clone();
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_config::{ClientKind, ClientSelection};

    fn record(kind: ClientKind, port: u16) -> DiscoveryRecord {
        DiscoveryRecord {
            name: kind.display_name().into(),
            error: None,
            backend: Some(Backend {
                kind: Some(kind),
                client: kind.display_name().into(),
                api: format!("http://127.0.0.1:{}", port + 1000).into(),
                secret: None,
                proxy: format!("http://127.0.0.1:{}", port),
                source: "test".into(),
                version: Some("test".into()),
                alternatives: vec![],
            }),
        }
    }

    #[test]
    fn verge_runtime_pipe_and_proxy_override_stale_tcp() {
        let be = parse_runtime(ClientKind::ClashVerge, "mixed-port: 7897\nexternal-controller: ''\nexternal-controller-pipe: '\\\\.\\pipe\\test-runtime'\nsecret: 'test-secret'\n").unwrap();
        assert_eq!(
            be.api,
            ApiEndpoint::NamedPipe(std::path::PathBuf::from(r"\\.\pipe\test-runtime"))
        );
        assert_eq!(be.secret.as_deref(), Some("test-secret"));
        assert!(parse_runtime(ClientKind::ClashVerge, "external-controller: ''\n").is_err());
    }

    #[test]
    fn auto_selects_unique_active_proxy_or_reports_ambiguity() {
        let records = vec![
            record(ClientKind::CuteCloud, 7890),
            record(ClientKind::ClashVerge, 7897),
        ];
        assert_eq!(
            select_auto(&records, Some("localhost:7897")).unwrap().kind,
            Some(ClientKind::ClashVerge)
        );
        assert!(select_auto(&records, None).is_err());
        assert_eq!(
            select_auto(&records[1..], None).unwrap().kind,
            Some(ClientKind::ClashVerge)
        );
        assert!(select_auto(&[], None).is_err());
    }

    #[test]
    fn explicit_client_and_api_never_cross_clients() {
        let records = vec![record(ClientKind::CuteCloud, 7890)];
        assert!(select_explicit(&records, ClientKind::ClashVerge).is_err());
        assert!(validate_explicit_identity(
            Some(ClientKind::ClashVerge),
            true,
            Some(ClientKind::CuteCloud)
        )
        .is_err());
        assert!(validate_explicit_identity(Some(ClientKind::ClashVerge), false, None).is_err());
        assert!(validate_explicit_identity(None, false, None).is_ok());
    }

    #[test]
    fn explicit_target_ignores_broken_default_config() {
        let path = std::env::temp_dir().join(format!(
            "proxpilot-broken-default-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "broken JSON").unwrap();
        let missing = path.as_path();
        assert_eq!(
            choose_selection(
                Some(ClientSelection::Explicit(ClientKind::ClashVerge)),
                true,
                missing
            )
            .unwrap(),
            ClientSelection::Explicit(ClientKind::ClashVerge)
        );
        assert_eq!(
            choose_selection(None, true, missing).unwrap(),
            ClientSelection::Auto
        );
        assert!(choose_selection(None, false, missing).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn discovery_deduplicates_instances_and_reports_unreadable_config() {
        let mut records = vec![
            record(ClientKind::ClashVerge, 7897),
            record(ClientKind::ClashVerge, 7897),
            DiscoveryRecord {
                name: "bad client".into(),
                backend: None,
                error: Some("配置不可读".into()),
            },
        ];
        deduplicate(&mut records);
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|r| r.error.is_some()));
        let error = parse_runtime(ClientKind::ClashVerge, "secret: private-leak: malformed:\n")
            .err()
            .unwrap();
        assert!(!error.contains("private-leak"));
    }
}

#[cfg(test)]
mod proxy_tests {
    #[test]
    fn per_protocol_proxy_must_match_http_and_https() {
        assert!(!super::proxy_matches(
            "http://127.0.0.1:7897",
            "http=127.0.0.1:7897;https=127.0.0.1:7890"
        ));
        assert!(super::proxy_matches(
            "http://127.0.0.1:7897",
            "http=localhost:7897;https=127.0.0.1:7897;ftp=127.0.0.1:1234"
        ));
        assert!(!super::proxy_matches(
            "http://127.0.0.1:7897",
            "socks=127.0.0.1:7897"
        ));
        assert!(!super::proxy_matches(
            "http://127.0.0.1:7897",
            "http=127.0.0.1:7897"
        ));
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    #[test]
    fn watch_reads_changed_pipe_without_cross_client_fallback() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = DiscoveryPaths {
            cutecloud: root.join("cute"),
            clash_verge: root.join("verge"),
        };
        std::fs::create_dir_all(&paths.clash_verge).unwrap();
        std::fs::create_dir_all(&paths.cutecloud).unwrap();
        let file = paths.clash_verge.join("clash-verge.yaml");
        std::fs::write(&file, "external-controller-pipe: 'pipe-A'\n").unwrap();
        std::fs::write(
            paths.cutecloud.join("config.yaml"),
            "external-controller: 127.0.0.1:9090\n",
        )
        .unwrap();
        let pinned = configured(ClientKind::ClashVerge, &paths).unwrap();
        let args = crate::parse_args_from(["watch".to_string()]).unwrap();
        std::fs::write(&file, "external-controller-pipe: 'pipe-B'\n").unwrap();
        let next = refresh_candidate(&pinned, &args, &paths).unwrap();
        assert_eq!(next.api, ApiEndpoint::NamedPipe(PathBuf::from("pipe-B")));
        assert_eq!(next.kind, Some(ClientKind::ClashVerge));
        std::fs::remove_file(&file).unwrap();
        assert!(refresh_candidate(&pinned, &args, &paths).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn loopback_addresses_are_distinct() {
        assert!(!proxy_matches("http://127.0.0.1:7897", "127.0.0.2:7897"));
        assert!(!proxy_matches("http://127.0.0.1:7897", "[::1]:7897"));
        assert!(proxy_matches("http://127.0.0.1:7897", "localhost:7897"));
    }
}
