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
    pub vvcloud: PathBuf,
}
impl DiscoveryPaths {
    pub fn current() -> Result<Self, String> {
        let base = PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA 未设置")?);
        Ok(Self {
            vvcloud: base.join("VVCloud/VVCloud"),
            cutecloud: base.join("CuteCloud/CuteCloud"),
            clash_verge: base.join("io.github.clash-verge-rev.clash-verge-rev"),
        })
    }
    pub(crate) fn runtime(&self, kind: ClientKind) -> PathBuf {
        match kind {
            ClientKind::VvCloud => self.vvcloud.join("config.yaml"),
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
        "VVCloud" => Some(ClientKind::VvCloud),
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
const CONTROLLER_PORTS: [u16; 6] = [9090, 9097, 9091, 9094, 19090, 28090];

fn unique_controller(
    kind: ClientKind,
    candidates: &[(u16, Option<ClientKind>)],
) -> Result<u16, String> {
    let ports: Vec<_> = candidates
        .iter()
        .filter(|(_, actual)| *actual == Some(kind))
        .map(|(port, _)| *port)
        .collect();
    match ports.as_slice() {
        [port] => Ok(*port),
        [] => Err(format!(
            "未发现 {} 的控制接口；请确认启动或用 --api 指定",
            kind.display_name()
        )),
        _ => Err(format!(
            "{} 存在多个控制接口，请用 --api 指定",
            kind.display_name()
        )),
    }
}

fn configured(kind: ClientKind, paths: &DiscoveryPaths) -> Result<Backend, String> {
    let path = paths.runtime(kind);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if kind == ClientKind::VvCloud && e.kind() == std::io::ErrorKind::NotFound => {
            // VVCloud 管理加密配置；只探测经进程身份确认的本机控制端口。
            let candidates: Vec<_> = CONTROLLER_PORTS
                .iter()
                .map(|port| {
                    (
                        *port,
                        identify_endpoint(&format!("http://127.0.0.1:{}", port)),
                    )
                })
                .collect();
            let port = unique_controller(kind, &candidates)?;
            return Ok(Backend {
                kind: Some(kind),
                client: kind.display_name().into(),
                api: format!("http://127.0.0.1:{}", port).into(),
                secret: None,
                proxy: String::new(),
                source: "VVCloud 进程监听端口".into(),
                version: None,
                alternatives: vec![],
            });
        }
        Err(e) => return Err(format!("读取 {} 失败：{}", path.display(), e)),
    };
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
    for kind in [
        ClientKind::CuteCloud,
        ClientKind::ClashVerge,
        ClientKind::VvCloud,
    ] {
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
    for port in CONTROLLER_PORTS {
        if crate::procinfo::find_listener_pid(port).is_none() {
            continue;
        }
        let kind = identify(port);
        // VVCloud 的正式发现已处理唯一性与配置错误，通用扫描不能绕过它。
        if kind == Some(ClientKind::VvCloud) {
            continue;
        }
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
fn active_proxy_owner(registry: &str) -> Option<String> {
    let mut owners = std::collections::HashSet::new();
    for entry in registry.split(';') {
        let address = if registry.contains('=') {
            let (protocol, address) = entry.split_once('=')?;
            if !matches!(
                protocol.trim().to_ascii_lowercase().as_str(),
                "http" | "https"
            ) {
                continue;
            }
            address.trim()
        } else {
            entry.trim()
        };
        let value = if address.contains("://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };
        let url = reqwest::Url::parse(&value).ok()?;
        let host = url.host_str()?.trim_matches(['[', ']']);
        if host != "localhost"
            && !host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        {
            return None;
        }
        let pid = crate::procinfo::find_listener_pid_at(host, url.port_or_known_default()?);
        let owner = crate::procinfo::identify_pid(pid).name;
        if owner == "未知客户端" {
            return None;
        }
        owners.insert(owner);
    }
    if owners.len() == 1 {
        owners.into_iter().next()
    } else {
        None
    }
}

fn cutecloud_managed_profile(paths: &DiscoveryPaths) -> Option<bool> {
    let prefs: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(paths.cutecloud.join("shared_preferences.json")).ok()?,
    )
    .ok()?;
    let cfg: serde_json::Value =
        serde_json::from_str(prefs.get("flutter.config")?.as_str()?).ok()?;
    let profile = cfg.get("currentProfileId")?.as_i64()?;
    let db = rusqlite::Connection::open_with_flags(
        paths.cutecloud.join("database.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    db.query_row(
        "SELECT managed FROM profiles WHERE id = ?1",
        [profile],
        |row| row.get::<_, bool>(0),
    )
    .ok()
}

fn cutecloud_control_guidance(managed: Option<bool>) -> String {
    let reason = match managed {
        Some(true) => "\n  当前使用托管订阅，该模式会隐藏外部控制器并关闭控制 API。请在 CuteCloud 的“配置”页切换到普通（非托管）订阅。",
        Some(false) => "\n  当前使用普通（非托管）配置，请检查外部控制器是否已开启。",
        None => "\n  请检查 CuteCloud 当前配置：托管订阅模式可能隐藏外部控制器并关闭控制 API；如果没有开关，切换到普通（非托管）订阅后再检查。",
    };
    format!("{reason}\n  在普通配置下，打开“设置 → 基本配置 → 外部控制器”（默认端口 9090）；必要时重新启动内核。\n  运行 proxpilot.exe clients 确认可用，再运行 proxpilot.exe scan；扫描的是当前普通配置的节点。\n  如果改为扫描 Clash Verge，可运行 proxpilot.exe --client clash-verge scan（扫描的是 Clash Verge 节点）。")
}

fn unavailable_proxy_message(owner: Option<&str>) -> String {
    let mut message = match owner {
        Some(owner) => format!("系统代理当前由 {owner} 提供，但没有匹配的可用控制接口。"),
        None => "系统代理已开启，但没有匹配的可用控制接口，且无法确认所属客户端。".into(),
    };
    match owner {
        Some("CuteCloud") => {
            let managed = DiscoveryPaths::current().ok().and_then(|paths| cutecloud_managed_profile(&paths));
            message.push_str(&cutecloud_control_guidance(managed));
        },
        Some("FlClash") => message.push_str(
            "\n  请打开客户端设置，找到“外部控制器（External controller）”并开启（通常在基本配置中，默认端口 9090）。\n  如果已开启仍不可用，请重新启动内核或客户端，让控制接口设置生效；不要只开启系统代理。\n  然后运行 proxpilot.exe clients 确认可用，再运行 proxpilot.exe scan。"
        ),
        Some("Clash Verge") => message.push_str(
            "\n  请在 Clash Verge 设置中检查外部控制/API 配置，启用本机 HTTP 控制接口或命名管道，并重新启动内核。\n  然后运行 proxpilot.exe clients 确认可用，再运行 proxpilot.exe scan。"
        ),
        _ => message.push_str(
            "\n  请在当前代理客户端中开启本机外部控制/API 接口，并重新启动内核；运行 proxpilot.exe clients 查看发现结果。"
        ),
    }
    message.push_str("\n  如需选择其他客户端，用 --client 显式指定；已知控制地址时也可用 --api / --secret 指定接口。程序不会自动切换到其他客户端。");
    message
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
        return if matched.is_empty() {
            Err(unavailable_proxy_message(
                active_proxy_owner(active).as_deref(),
            ))
        } else {
            Err("系统代理匹配多个可用内核，无法唯一选择；请用 clients 查看详情，或用 --client / --api 显式指定目标".into())
        };
    }
    match available.as_slice() {
        [be] => Ok((*be).clone()),
        [] => Err("自动探测未发现可用内核；请确认客户端已启动，用 clients 查看详情，或用 --client / --api 指定".into()),
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
    Ok(client_config::load_default(path)?.unwrap_or(ClientSelection::Auto))
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
                Some(crate::sysproxy::registry_proxy_server().ok_or(
                    "系统代理已开启，但无法读取代理服务器地址；请检查系统代理设置，或用 --client / --api 显式指定目标",
                )?)
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
    let mut next = match verify_with_proxy(next, api, args.proxy.as_deref()) {
        Ok(next) => next,
        Err(_) if be.kind == Some(ClientKind::CuteCloud) => {
            // The GUI can hot-update the controller without writing config.yaml.
            // Revalidate the pinned endpoint's current owner and live API; never
            // substitute another client's endpoint or treat a closed API as alive.
            let mut pinned = be.clone();
            if args.secret.is_some() {
                pinned.secret = args.secret.clone();
            }
            pinned.source = "已验证的 CuteCloud 监听接口（运行配置暂不可用）".into();
            verify_with_proxy(pinned, api, args.proxy.as_deref())?
        }
        Err(e) => return Err(e),
    };
    if let Some(proxy) = &args.proxy {
        next.proxy = proxy.clone();
    }
    Ok(next)
}

fn refresh_candidate(be: &Backend, args: &Args, paths: &DiscoveryPaths) -> Result<Backend, String> {
    let mut next = if args.api.is_some() || be.kind.is_none() {
        be.clone()
    } else {
        match configured(be.kind.unwrap(), paths) {
            Ok(next) => next,
            Err(_) if be.kind == Some(ClientKind::CuteCloud) => {
                let mut pinned = be.clone();
                pinned.source = "CuteCloud 上轮监听接口（等待实时验证）".into();
                pinned
            }
            Err(e) => return Err(e),
        }
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
    fn vvcloud_controller_requires_unique_matching_identity() {
        let candidates = [
            (9090, Some(ClientKind::CuteCloud)),
            (9091, Some(ClientKind::VvCloud)),
            (9097, Some(ClientKind::ClashVerge)),
        ];
        assert_eq!(
            unique_controller(ClientKind::VvCloud, &candidates).unwrap(),
            9091
        );
        assert!(unique_controller(ClientKind::VvCloud, &candidates[..1]).is_err());
        assert!(unique_controller(
            ClientKind::VvCloud,
            &[
                (9090, Some(ClientKind::VvCloud)),
                (9091, Some(ClientKind::VvCloud))
            ]
        )
        .is_err());
        assert!(validate_explicit_identity(
            Some(ClientKind::VvCloud),
            true,
            Some(ClientKind::CuteCloud)
        )
        .is_err());
        let records = vec![
            record(ClientKind::VvCloud, 7890),
            record(ClientKind::ClashVerge, 7897),
        ];
        assert_eq!(
            select_auto(&records, Some("127.0.0.1:7890")).unwrap().kind,
            Some(ClientKind::VvCloud)
        );
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
    fn unset_default_uses_auto_without_persisting_a_client() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-default-auto-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("config.json");
        assert_eq!(
            choose_selection(None, false, &path).unwrap(),
            ClientSelection::Auto
        );
        assert!(!path.exists());
        client_config::save_default(&path, Some(ClientSelection::Explicit(ClientKind::VvCloud)))
            .unwrap();
        assert_eq!(
            choose_selection(None, false, &path).unwrap(),
            ClientSelection::Explicit(ClientKind::VvCloud)
        );
        assert_eq!(
            choose_selection(
                Some(ClientSelection::Explicit(ClientKind::ClashVerge)),
                false,
                &path
            )
            .unwrap(),
            ClientSelection::Explicit(ClientKind::ClashVerge)
        );
        assert_eq!(
            choose_selection(None, true, &path).unwrap(),
            ClientSelection::Auto
        );
        client_config::save_default(&path, None).unwrap();
        assert_eq!(
            choose_selection(None, false, &path).unwrap(),
            ClientSelection::Auto
        );
        assert_eq!(client_config::load_default(&path).unwrap(), None);
        assert!(select_auto(&[], None).err().unwrap().contains("--client"));
        let records = [
            record(ClientKind::CuteCloud, 7890),
            record(ClientKind::VvCloud, 7891),
        ];
        assert!(select_auto(&records, None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn auto_does_not_fall_back_when_active_proxy_has_no_available_controller() {
        let records = [
            DiscoveryRecord {
                name: "CuteCloud".into(),
                backend: None,
                error: Some("运行配置没有启用控制接口".into()),
            },
            record(ClientKind::ClashVerge, 7897),
        ];
        let error = select_auto(&records, Some("127.0.0.1:7890"))
            .err()
            .expect("must not scan Clash Verge while the system proxy points to CuteCloud");
        assert!(error.contains("系统代理"));
        assert!(error.contains("控制接口"));
        assert!(error.contains("clients"));
        assert!(error.contains("--client"));
        assert_eq!(
            select_auto(&records, None).unwrap().kind,
            Some(ClientKind::ClashVerge)
        );
        assert_eq!(
            select_explicit(&records, ClientKind::ClashVerge)
                .unwrap()
                .kind,
            Some(ClientKind::ClashVerge)
        );
    }

    #[test]
    fn auto_requires_one_match_when_system_proxy_is_active() {
        let records = [
            record(ClientKind::CuteCloud, 7890),
            record(ClientKind::ClashVerge, 7890),
        ];
        assert!(select_auto(&records, Some("127.0.0.1:7890")).is_err());
        assert!(select_auto(&records, Some("127.0.0.1:8888")).is_err());
        assert!(select_auto(&[], Some("127.0.0.1:7890")).is_err());
    }

    #[test]
    fn unavailable_proxy_names_owner_and_explains_how_to_enable_control() {
        let message = unavailable_proxy_message(Some("CuteCloud"));
        assert!(message.contains("CuteCloud"));
        assert!(message.contains("普通"));
        assert!(message.contains("外部控制器"));
        assert!(message.contains("--client clash-verge scan"));
        assert!(message.contains("proxpilot.exe scan"));
        let unknown = unavailable_proxy_message(None);
        assert!(unknown.contains("无法确认"));
        assert!(!unknown.contains("CuteCloud"));
        assert!(unavailable_proxy_message(Some("Clash Verge")).contains("Clash Verge"));
    }

    #[test]
    fn cutecloud_managed_profile_gets_specific_control_guidance() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-managed-controller-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let paths = DiscoveryPaths {
            cutecloud: root.clone(),
            clash_verge: root.join("verge"),
            vvcloud: root.join("vv"),
        };
        let cfg = serde_json::json!({"flutter.config": "{\"currentProfileId\":42}"});
        std::fs::write(root.join("shared_preferences.json"), cfg.to_string()).unwrap();
        let db = rusqlite::Connection::open(root.join("database.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE profiles(id INTEGER PRIMARY KEY, managed INTEGER); INSERT INTO profiles VALUES(42,1);").unwrap();
        assert_eq!(cutecloud_managed_profile(&paths), Some(true));
        let message = cutecloud_control_guidance(Some(true));
        assert!(message.contains("托管订阅"));
        assert!(message.contains("普通"));
        assert!(message.contains("外部控制器"));
        assert!(message.contains("Clash Verge"));
        db.execute("UPDATE profiles SET managed=0 WHERE id=42", [])
            .unwrap();
        assert_eq!(cutecloud_managed_profile(&paths), Some(false));
        assert!(!cutecloud_control_guidance(Some(false)).contains("当前使用托管订阅"));
        drop(db);
        std::fs::remove_file(root.join("database.sqlite")).unwrap();
        assert_eq!(cutecloud_managed_profile(&paths), None);
        assert!(!root.join("database.sqlite").exists());
        std::fs::remove_dir_all(root).unwrap();
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
    fn cutecloud_refresh_preserves_pinned_endpoint_when_runtime_is_stale() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-cute-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = DiscoveryPaths {
            vvcloud: root.join("vv"),
            cutecloud: root.join("cute"),
            clash_verge: root.join("verge"),
        };
        std::fs::create_dir_all(&paths.cutecloud).unwrap();
        std::fs::create_dir_all(&paths.clash_verge).unwrap();
        let file = paths.runtime(ClientKind::CuteCloud);
        std::fs::write(
            &file,
            "external-controller: 127.0.0.1:9090\nsecret: pinned-secret\n",
        )
        .unwrap();
        let pinned = configured(ClientKind::CuteCloud, &paths).unwrap();
        std::fs::write(
            paths.runtime(ClientKind::ClashVerge),
            "external-controller: 127.0.0.1:9097\n",
        )
        .unwrap();
        let args =
            crate::parse_args_from(["watch".into(), "--secret".into(), "override".into()]).unwrap();
        std::fs::write(&file, "external-controller: ''\n").unwrap();
        let next = refresh_candidate(&pinned, &args, &paths).unwrap();
        assert_eq!(next.kind, Some(ClientKind::CuteCloud));
        assert_eq!(next.api, pinned.api);
        assert_eq!(next.secret.as_deref(), Some("override"));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            refresh_candidate(&pinned, &args, &paths).unwrap().api,
            pinned.api
        );
        std::fs::write(
            &file,
            "external-controller: 127.0.0.1:9091\nsecret: changed\n",
        )
        .unwrap();
        let updated = refresh_candidate(&pinned, &args, &paths).unwrap();
        assert_eq!(
            updated.api,
            ApiEndpoint::Http("http://127.0.0.1:9091".into())
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires a running CuteCloud selected by the system proxy; read-only"]
    fn live_cutecloud_watch_refresh_revalidates_hot_updated_controller() {
        let args =
            crate::parse_args_from(["watch".into(), "--client".into(), "auto".into()]).unwrap();
        let api = CoreApi::new().unwrap();
        let pinned = crate::detect::detect(&api, &args).unwrap();
        assert_eq!(pinned.kind, Some(ClientKind::CuteCloud));
        let updated = refresh(&pinned, &args, &api).unwrap();
        assert_eq!(updated.kind, Some(ClientKind::CuteCloud));
        assert_eq!(updated.api, pinned.api);
        assert!(updated.version.is_some());
        assert!(!updated.proxy.is_empty());
    }

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
            vvcloud: root.join("vv"),
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
    fn vvcloud_watch_refreshes_its_own_runtime_and_rejects_invalid_config() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-vv-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = DiscoveryPaths {
            cutecloud: root.join("cute"),
            clash_verge: root.join("verge"),
            vvcloud: root.join("vv"),
        };
        std::fs::create_dir_all(&paths.vvcloud).unwrap();
        let file = paths.runtime(ClientKind::VvCloud);
        std::fs::write(&file, "external-controller: 127.0.0.1:19090\nsecret: old\n").unwrap();
        let pinned = configured(ClientKind::VvCloud, &paths).unwrap();
        let args = crate::parse_args_from([
            "watch".to_string(),
            "--secret".to_string(),
            "override".to_string(),
        ])
        .unwrap();
        std::fs::write(&file, "external-controller: 127.0.0.1:9091\nsecret: new\n").unwrap();
        let next = refresh_candidate(&pinned, &args, &paths).unwrap();
        assert_eq!(next.kind, Some(ClientKind::VvCloud));
        assert_eq!(next.api, ApiEndpoint::Http("http://127.0.0.1:9091".into()));
        assert_eq!(next.secret.as_deref(), Some("override"));
        std::fs::write(&file, "external-controller: ''\n").unwrap();
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
