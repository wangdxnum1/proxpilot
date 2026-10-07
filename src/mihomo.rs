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

fn api_response(
    req: reqwest::blocking::RequestBuilder,
) -> Result<reqwest::blocking::Response, String> {
    let response = req.send().map_err(|e| format!("API 请求失败: {}", e))?;
    if !response.status().is_success() {
        return Err(format!("API 请求失败: HTTP {}", response.status()));
    }
    Ok(response)
}

pub fn get_proxies(
    be: &Backend,
    agent: &reqwest::blocking::Client,
) -> Result<HashMap<String, ProxyInfo>, String> {
    let mut req = agent.get(format!("{}/proxies", be.api));
    if let Some(s) = &be.secret {
        req = req.bearer_auth(s);
    }
    let resp = api_response(req)?;
    let body = resp.text().map_err(|e| e.to_string())?;
    let parsed: ProxiesMap =
        serde_json::from_str(&body).map_err(|e| format!("JSON 解析失败: {}", e))?;
    Ok(parsed.proxies)
}

pub fn get_version(be: &Backend, agent: &reqwest::blocking::Client) -> Result<String, String> {
    let mut req = agent.get(format!("{}/version", be.api));
    if let Some(s) = &be.secret {
        req = req.bearer_auth(s);
    }
    let resp = api_response(req)?;
    let body = resp.text().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(v.get("version")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string())
}

/// 让内核对指定节点做一次延迟测试（任意 HTTP 响应都算成功，含 403）
pub fn probe_delay(
    be: &Backend,
    agent: &reqwest::blocking::Client,
    node: &str,
    url: &str,
    timeout_ms: u32,
) -> Option<i64> {
    let u = format!(
        "{}/proxies/{}/delay?url={}&timeout={}",
        be.api,
        enc_path(node),
        enc(url),
        timeout_ms
    );
    let mut req = agent.get(&u);
    if let Some(s) = &be.secret {
        req = req.bearer_auth(s);
    }
    let resp = api_response(req).ok()?;
    let body = resp.text().ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    let d = v.get("delay")?.as_i64()?;
    if d > 0 {
        Some(d)
    } else {
        None
    }
}

pub fn switch_group(
    be: &Backend,
    agent: &reqwest::blocking::Client,
    group: &str,
    node: &str,
) -> Result<(), String> {
    let u = format!("{}/proxies/{}", be.api, enc_path(group));
    let body = serde_json::json!({ "name": node }).to_string();
    let mut req = agent.put(&u).header("Content-Type", "application/json");
    if let Some(s) = &be.secret {
        req = req.bearer_auth(s);
    }
    let resp = api_response(req.body(body)).map_err(|e| format!("切换失败: {}", e))?;
    let _ = resp.text();
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
    use crate::http::{api_client, test_support::serve};

    fn backend(api: String) -> Backend {
        Backend {
            client: "test".into(),
            api,
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
            get_version(&backend(url), &api_client().unwrap()).unwrap(),
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
        assert!(switch_group(&backend(url), &api_client().unwrap(), "AI服务", "node").is_err());
        server.join().unwrap();
    }

    #[test]
    fn redirected_switch_is_not_reported_as_success() {
        let (url, server) = serve(vec![b"HTTP/1.1 302 Found\r\nLocation: /other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()]);
        assert!(switch_group(&backend(url), &api_client().unwrap(), "AI服务", "node").is_err());
        server.join().unwrap();
    }
}
