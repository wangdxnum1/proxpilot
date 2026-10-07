//! 后端自动探测：寻找本机运行的 Clash/mihomo 系客户端，多个时默认优先 CuteCloud。

use std::time::Duration;

use crate::procinfo;
use crate::sysproxy;
use crate::Args;

#[derive(Clone)]
pub struct Backend {
    pub client: String,
    pub api: String,
    pub secret: Option<String>,
    pub proxy: String,
    pub source: String,
    pub version: Option<String>,
    /// 同时发现的其他客户端（名称, API 地址）
    pub alternatives: Vec<(String, String)>,
}

const COMMON_PORTS: [u16; 6] = [9090, 9097, 9091, 9094, 19090, 28090];
/// 多客户端同时在线时优先使用的客户端（暂定 CuteCloud）
const PREFERRED: &str = "CuteCloud";

pub fn detect(agent: &ureq::Agent, args: &Args) -> Result<Backend, String> {
    let (api, client, source, alternatives) = if let Some(a) = &args.api {
        let a = a.trim_end_matches('/').to_string();
        let port = crate::checker::parse_addr(&a).1;
        let info = procinfo::identify_client(port);
        (a, info.name, "--api 参数".to_string(), Vec::new())
    } else if !args.detect {
        // 快速路径：不做任何探测，直接使用 CuteCloud 的默认地址
        (
            "http://127.0.0.1:9090".to_string(),
            "CuteCloud".to_string(),
            "默认".to_string(),
            Vec::new(),
        )
    } else {
        let spinner = crate::ui::Spinner::start("探测本机运行的客户端...");
        let mut responders: Vec<(u16, String)> = Vec::new();
        let mut need_secret: Vec<(u16, String)> = Vec::new();
        for port in COMMON_PORTS {
            spinner.set_text(format!("探测 127.0.0.1:{} ...", port));
            let base = format!("http://127.0.0.1:{}", port);
            let mut req = agent
                .get(&format!("{}/version", base))
                .timeout(Duration::from_secs(2));
            if let Some(s) = &args.secret {
                req = req.set("Authorization", &format!("Bearer {}", s));
            }
            match req.call() {
                Ok(r) => {
                    let body = r.into_string().unwrap_or_default();
                    if body.contains("version") || body.contains("meta") {
                        let info = procinfo::identify_client(port);
                        responders.push((port, info.name));
                    }
                }
                Err(ureq::Error::Status(401, _)) => {
                    let info = procinfo::identify_client(port);
                    need_secret.push((port, info.name));
                }
                Err(_) => {}
            }
        }
        if responders.is_empty() {
            if let Some((port, name)) = need_secret.first() {
                return Err(format!(
                    "发现 {} 内核（127.0.0.1:{}），但 API 设置了 secret，请加 --secret 提供",
                    name, port
                ));
            }
            return Err(
                "未发现运行中的 Clash/mihomo 系客户端（CuteCloud / FlClash / Clash Verge 等），\
                 请先启动客户端，或用 --api 指定地址"
                    .to_string(),
            );
        }
        // 默认优先 CuteCloud，其余按端口顺序
        responders.sort_by_key(|(port, name)| {
            (
                if name.eq_ignore_ascii_case(PREFERRED) { 0 } else { 1 },
                *port,
            )
        });
        let (port, name) = responders.remove(0);
        let alts = responders
            .iter()
            .map(|(p, c)| (c.clone(), format!("http://127.0.0.1:{}", p)))
            .collect();
        (
            format!("http://127.0.0.1:{}", port),
            name,
            "自动探测".to_string(),
            alts,
        )
    };

    let mut be = Backend {
        client,
        api,
        secret: args.secret.clone(),
        proxy: String::new(),
        source,
        version: None,
        alternatives,
    };
    be.proxy = if let Some(p) = &args.proxy {
        p.clone()
    } else if let Some(p) = sysproxy::registry_proxy_server() {
        if p.contains("://") {
            p
        } else {
            format!("http://{}", p)
        }
    } else {
        "http://127.0.0.1:7890".to_string()
    };
    be.version = crate::mihomo::get_version(&be, agent).ok();
    Ok(be)
}
