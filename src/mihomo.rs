//! Clash / mihomo 系内核的 RESTful API 客户端（external-controller）。
//! CuteCloud、FlClash、Clash Verge、Clash for Windows 等均兼容此 API。

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::Backend;

#[derive(Deserialize, Clone)]
pub struct ProxyInfo {
    #[serde(rename = "type", default)]
    pub ptype: String,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub all: Option<Vec<String>>,
    #[serde(default)]
    pub history: Vec<DelayRecord>,
    #[serde(default)]
    pub udp: Option<bool>,
}

#[derive(Deserialize, Clone, Copy)]
pub struct DelayRecord {
    #[serde(default)]
    pub delay: i64,
}

#[derive(Deserialize)]
struct ProxiesMap {
    #[serde(default)]
    proxies: HashMap<String, ProxyInfo>,
}

fn api_json(be: &Backend, api: &crate::core_api::CoreApi, path: &str) -> Result<Value, String> {
    let response = api.request(
        be,
        reqwest::Method::GET,
        path,
        None,
        std::time::Duration::from_secs(10),
    )?;
    serde_json::from_slice(&response.body).map_err(|_| "API JSON 解析失败".into())
}

pub fn get_proxies(
    be: &Backend,
    api: &crate::core_api::CoreApi,
) -> Result<HashMap<String, ProxyInfo>, String> {
    let response = api.request(
        be,
        reqwest::Method::GET,
        "/proxies",
        None,
        std::time::Duration::from_secs(10),
    )?;
    let parsed: ProxiesMap =
        serde_json::from_slice(&response.body).map_err(|_| "节点 API JSON 解析失败".to_string())?;
    Ok(parsed.proxies)
}

pub fn get_version(be: &Backend, api: &crate::core_api::CoreApi) -> Result<String, String> {
    api_json(be, api, "/version")?
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "响应不是有效的 mihomo 版本信息".into())
}

pub fn get_mode(be: &Backend, api: &crate::core_api::CoreApi) -> Result<String, String> {
    api_json(be, api, "/configs")?
        .get("mode")
        .and_then(Value::as_str)
        .map(|mode| mode.to_ascii_lowercase())
        .ok_or("内核未提供代理模式".into())
}

pub fn get_runtime_proxy(be: &Backend, api: &crate::core_api::CoreApi) -> Result<String, String> {
    let value = api_json(be, api, "/configs")?;
    for (name, scheme) in [
        ("mixed-port", "http"),
        ("port", "http"),
        ("socks-port", "socks5h"),
    ] {
        if let Some(port) = value
            .get(name)
            .and_then(Value::as_u64)
            .filter(|p| *p > 0 && *p <= 65535)
        {
            return Ok(format!("{}://127.0.0.1:{}", scheme, port));
        }
    }
    Err("目标内核没有可用的 mixed/HTTP/SOCKS 代理端口，请用 --proxy 指定".into())
}

pub fn probe_delay(
    be: &Backend,
    api: &crate::core_api::CoreApi,
    node: &str,
    url: &str,
    timeout_ms: u32,
) -> Option<i64> {
    let path = format!(
        "/proxies/{}/delay?url={}&timeout={}",
        enc_path(node),
        enc(url),
        timeout_ms
    );
    let response = api
        .request(
            be,
            reqwest::Method::GET,
            &path,
            None,
            std::time::Duration::from_millis(u64::from(timeout_ms) + 2000),
        )
        .ok()?;
    let value: Value = serde_json::from_slice(&response.body).ok()?;
    value.get("delay")?.as_i64().filter(|d| *d > 0)
}

pub fn switch_group(
    be: &Backend,
    api: &crate::core_api::CoreApi,
    group: &str,
    node: &str,
) -> Result<(), String> {
    let path = format!("/proxies/{}", enc_path(group));
    let body = serde_json::to_vec(&serde_json::json!({"name": node})).map_err(|e| e.to_string())?;
    let response = api.request(
        be,
        reqwest::Method::PUT,
        &path,
        Some(body),
        std::time::Duration::from_secs(10),
    )?;
    debug_assert!((200..300).contains(&response.status));
    Ok(())
}

/// 百分号编码（用于单个路径段）
pub fn enc_path(s: &str) -> String {
    enc_impl(s, false)
}

/// 百分号编码（用于查询值，编码所有特殊字符）
pub fn enc(s: &str) -> String {
    enc_impl(s, false)
}

fn enc_impl(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_api::CoreApi;
    use crate::http::test_support::serve;

    fn backend(api: String) -> Backend {
        Backend {
            kind: None,
            client: "test".into(),
            api: api.into(),
            secret: Some("test-secret".into()),
            proxy: "http://127.0.0.1:1".into(),
            source: "test".into(),
            version: None,
            alternatives: Vec::new(),
        }
    }

    #[test]
    fn version_request_includes_api_authentication() {
        let body = r#"{"version":"test-core"}"#;
        let (url, server) = serve(vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes()]);
        assert_eq!(
            get_version(&backend(url), &CoreApi::new().unwrap()).unwrap(),
            "test-core"
        );
        assert!(server.join().unwrap()[0]
            .to_lowercase()
            .contains("authorization: bearer test-secret"));
    }

    #[test]
    fn rejected_switch_is_an_error() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ]);
        assert!(switch_group(&backend(url), &CoreApi::new().unwrap(), "AI服务", "node").is_err());
        server.join().unwrap();
    }

    #[test]
    fn redirected_switch_is_not_reported_as_success() {
        let (url, server) = serve(vec![b"HTTP/1.1 302 Found\r\nLocation: /other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()]);
        assert!(switch_group(&backend(url), &CoreApi::new().unwrap(), "AI服务", "node").is_err());
        server.join().unwrap();
    }
}

pub fn missing_group(proxies: &HashMap<String, ProxyInfo>, name: &str) -> String {
    let mut groups: Vec<_> = proxies
        .iter()
        .filter(|(_, p)| p.all.is_some())
        .map(|(n, _)| n.as_str())
        .collect();
    groups.sort();
    format!(
        "找不到组「{}」；可用组：{}。请用 --group 指定",
        name,
        groups.join("、")
    )
}

/// 从实时组引用图选择唯一顶层业务组；GLOBAL 是合成总览，不参与业务组引用。
pub fn resolve_group(
    proxies: &HashMap<String, ProxyInfo>,
    explicit: &str,
) -> Result<String, String> {
    if !explicit.is_empty() {
        return if proxies.get(explicit).is_some_and(|p| p.all.is_some()) {
            Ok(explicit.into())
        } else {
            Err(missing_group(proxies, explicit))
        };
    }
    let mut groups: Vec<&str> = proxies
        .iter()
        .filter(|(name, p)| name.as_str() != "GLOBAL" && p.all.is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    if groups.is_empty() {
        return if proxies.get("GLOBAL").is_some_and(|p| p.all.is_some()) {
            Ok("GLOBAL".into())
        } else {
            Err("目标客户端没有可用策略组，请检查运行配置".into())
        };
    }
    groups.sort();
    let referenced: std::collections::HashSet<&str> = groups
        .iter()
        .flat_map(|name| {
            proxies[*name]
                .all
                .as_ref()
                .unwrap()
                .iter()
                .map(String::as_str)
        })
        .collect();
    let roots: Vec<&str> = groups
        .iter()
        .copied()
        .filter(|name| !referenced.contains(name))
        .collect();
    match roots.as_slice() {
        [name] => Ok((*name).into()),
        _ => Err(format!(
            "无法唯一确定目标策略组；可用组：{}。请用 --group 显式指定",
            groups.join("、")
        )),
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    fn groups(value: serde_json::Value) -> HashMap<String, ProxyInfo> {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn unique_business_root_ignores_global_and_nested_groups() {
        let proxies = groups(serde_json::json!({
            "GLOBAL":{"type":"Selector","all":["VVCloud","自动选择","故障转移"]},
            "VVCloud":{"type":"Selector","all":["自动选择","故障转移","日本 🛰"]},
            "自动选择":{"type":"URLTest","all":["日本 🛰"]},
            "故障转移":{"type":"Fallback","all":["日本 🛰"]},
            "日本 🛰":{"type":"SS"}
        }));
        assert_eq!(resolve_group(&proxies, "").unwrap(), "VVCloud");
        assert_eq!(resolve_group(&proxies, "自动选择").unwrap(), "自动选择");
        assert!(resolve_group(&proxies, "日本 🛰").is_err());
        assert!(resolve_group(&proxies, "AI服务").is_err());
    }
    #[test]
    fn multiple_roots_or_cycles_require_explicit_choice() {
        let mut proxies =
            groups(serde_json::json!({"AI服务":{"all":["node"]},"流媒体":{"all":["node"]}}));
        let error = resolve_group(&proxies, "").unwrap_err();
        assert!(error.contains("AI服务") && error.contains("流媒体"));
        assert_eq!(resolve_group(&proxies, "AI服务").unwrap(), "AI服务");
        proxies = groups(serde_json::json!({"A":{"all":["B"]},"B":{"all":["A"]}}));
        assert!(resolve_group(&proxies, "").is_err());
    }
    #[test]
    fn no_groups_and_global_only_are_handled() {
        assert!(resolve_group(&HashMap::new(), "").is_err());
        assert_eq!(
            resolve_group(
                &groups(serde_json::json!({"GLOBAL":{"all":["DIRECT"]}})),
                ""
            )
            .unwrap(),
            "GLOBAL"
        );
    }
    #[test]
    fn topology_is_recomputed_after_runtime_changes() {
        let first = groups(serde_json::json!({"Old":{"all":["node"]}}));
        let second = groups(serde_json::json!({"New":{"all":["node"]}}));
        assert_eq!(resolve_group(&first, "").unwrap(), "Old");
        assert_eq!(resolve_group(&second, "").unwrap(), "New");
        assert!(resolve_group(&second, "Old").is_err());
    }
}
