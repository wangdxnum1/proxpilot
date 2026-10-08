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
    let response = api.request(be, reqwest::Method::GET, path, None, std::time::Duration::from_secs(10))?;
    serde_json::from_slice(&response.body).map_err(|_| "API JSON 解析失败".into())
}

pub fn get_proxies(be: &Backend, api: &crate::core_api::CoreApi) -> Result<HashMap<String, ProxyInfo>, String> {
    let response = api.request(be, reqwest::Method::GET, "/proxies", None, std::time::Duration::from_secs(10))?;
    let parsed: ProxiesMap = serde_json::from_slice(&response.body).map_err(|_| "节点 API JSON 解析失败".to_string())?;
    Ok(parsed.proxies)
}

pub fn get_version(be: &Backend, api: &crate::core_api::CoreApi) -> Result<String, String> {
    api_json(be, api, "/version")?.get("version").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "响应不是有效的 mihomo 版本信息".into())
}

pub fn get_runtime_proxy(be: &Backend, api: &crate::core_api::CoreApi) -> Result<String, String> {
    let value = api_json(be, api, "/configs")?;
    for (name, scheme) in [("mixed-port", "http"), ("port", "http"), ("socks-port", "socks5h")] {
        if let Some(port) = value.get(name).and_then(Value::as_u64).filter(|p| *p > 0 && *p <= 65535) {
            return Ok(format!("{}://127.0.0.1:{}", scheme, port));
        }
    }
    Err("目标内核没有可用的 mixed/HTTP/SOCKS 代理端口，请用 --proxy 指定".into())
}

pub fn probe_delay(be: &Backend, api: &crate::core_api::CoreApi, node: &str, url: &str, timeout_ms: u32) -> Option<i64> {
    let path = format!("/proxies/{}/delay?url={}&timeout={}", enc_path(node), enc(url), timeout_ms);
    let response = api.request(be, reqwest::Method::GET, &path, None, std::time::Duration::from_millis(u64::from(timeout_ms) + 2000)).ok()?;
    let value: Value = serde_json::from_slice(&response.body).ok()?;
    value.get("delay")?.as_i64().filter(|d| *d > 0)
}

pub fn switch_group(be: &Backend, api: &crate::core_api::CoreApi, group: &str, node: &str) -> Result<(), String> {
    let path = format!("/proxies/{}", enc_path(group));
    let body = serde_json::to_vec(&serde_json::json!({"name": node})).map_err(|e| e.to_string())?;
    let response = api.request(be, reqwest::Method::PUT, &path, Some(body), std::time::Duration::from_secs(10))?;
    debug_assert!((200..300).contains(&response.status));
    Ok(())
}

/// 百分号编码（用于路径段，保留 /）
pub fn enc_path(s: &str) -> String {
    enc_impl(s, true)
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
    use crate::http::test_support::serve;
    use crate::core_api::CoreApi;

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
