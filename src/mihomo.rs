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

pub fn get_proxies(be: &Backend, agent: &ureq::Agent) -> Result<HashMap<String, ProxyInfo>, String> {
    let mut req = agent.get(&format!("{}/proxies", be.api));
    if let Some(s) = &be.secret {
        req = req.set("Authorization", &format!("Bearer {}", s));
    }
    let resp = req.call().map_err(|e| format!("API 请求失败: {}", e))?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    let parsed: ProxiesMap = serde_json::from_str(&body).map_err(|e| format!("JSON 解析失败: {}", e))?;
    Ok(parsed.proxies)
}

pub fn get_version(be: &Backend, agent: &ureq::Agent) -> Result<String, String> {
    let resp = agent
        .get(&format!("{}/version", be.api))
        .call()
        .map_err(|e| format!("API 请求失败: {}", e))?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    Ok(v.get("version")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string())
}

/// 让内核对指定节点做一次延迟测试（任意 HTTP 响应都算成功，含 403）
pub fn probe_delay(be: &Backend, agent: &ureq::Agent, node: &str, url: &str, timeout_ms: u32) -> Option<i64> {
    let u = format!(
        "{}/proxies/{}/delay?url={}&timeout={}",
        be.api,
        enc_path(node),
        enc(url),
        timeout_ms
    );
    let mut req = agent.get(&u);
    if let Some(s) = &be.secret {
        req = req.set("Authorization", &format!("Bearer {}", s));
    }
    let resp = req.call().ok()?;
    let body = resp.into_string().ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    let d = v.get("delay")?.as_i64()?;
    if d > 0 {
        Some(d)
    } else {
        None
    }
}

pub fn switch_group(be: &Backend, agent: &ureq::Agent, group: &str, node: &str) -> Result<(), String> {
    let u = format!("{}/proxies/{}", be.api, enc_path(group));
    let body = serde_json::json!({ "name": node }).to_string();
    let mut req = agent
        .put(&u)
        .set("Content-Type", "application/json");
    if let Some(s) = &be.secret {
        req = req.set("Authorization", &format!("Bearer {}", s));
    }
    let resp = req.send(body.as_bytes()).map_err(|e| format!("切换失败: {}", e))?;
    let _ = resp.into_string();
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
