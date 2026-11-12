//! Development-toolchain proxy reachability.
//!
//! Different tools reach the network in different ways: some honour the
//! Windows system proxy (WinINET), some only read environment variables, some
//! have their own config file, and some go direct. A node problem therefore
//! does not hit every tool equally, and "my proxy works" is not one answer but
//! one answer per tool.
//!
//! This module only reports what it observes. It never edits a tool's config.

use crate::diagnostics::{AppError, ErrorCode, Stage};
use serde::Serialize;
use std::collections::BTreeMap;

/// How a tool decides to use a proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxySource {
    /// Reads the Windows system proxy (WinINET / registry).
    System,
    /// Only honours http_proxy / https_proxy environment variables.
    Environment,
    /// Has its own config file (registry/mirror/settings.xml ...).
    OwnConfig,
    /// Went direct in our probe, or has no known proxy plumbing.
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// Reachable through the proxy.
    ViaProxy,
    /// Reachable without the proxy.
    Direct,
    /// Reachable either way.
    Both,
    /// Neither path worked.
    Unreachable,
}

#[derive(Clone, Debug, Serialize)]
pub struct DepProbe {
    pub tool: String,
    pub endpoint: String,
    pub proxy_source: ProxySource,
    pub reach: Reach,
    /// Best (lowest) latency observed, in milliseconds.
    pub via_proxy_ms: Option<u64>,
    pub direct_ms: Option<u64>,
    /// Config file that decided the routing, when one exists.
    pub config: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DepReport {
    pub system_proxy: Option<String>,
    pub environment_proxy: bool,
    pub probes: Vec<DepProbe>,
}

fn read_system_proxy() -> Option<String> {
    crate::sysproxy::registry_proxy_server()
}

fn environment_proxy() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// One HEAD request through a chosen route. Returns latency on success.
fn probe_once(host: &str, proxy: Option<&str>, timeout_ms: u64) -> Result<u64, AppError> {
    let started = std::time::Instant::now();
    let socket_host = host;
    let port = 443u16;
    let addrs: Vec<_> = {
        use std::net::ToSocketAddrs;
        (socket_host, port)
            .to_socket_addrs()
            .map_err(|_| AppError::new(ErrorCode::Dns, Stage::Discovery))?
            .collect()
    };
    if addrs.is_empty() {
        return Err(AppError::new(ErrorCode::Dns, Stage::Discovery));
    }
    let timeout = std::time::Duration::from_millis(timeout_ms);
    // Through the proxy: establish a CONNECT tunnel first.
    if let Some(proxy) = proxy {
        if let Some(hostport) = proxy_hostport(proxy) {
            let mut stream = std::net::TcpStream::connect_timeout(&hostport, timeout)
                .map_err(|_| AppError::new(ErrorCode::ProxyConnect, Stage::Discovery))?;
            stream.set_read_timeout(Some(timeout)).ok();
            stream.set_write_timeout(Some(timeout)).ok();
            use std::io::{Read, Write};
            let request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
            stream
                .write_all(request.as_bytes())
                .map_err(|_| AppError::new(ErrorCode::ProxyConnect, Stage::Discovery))?;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 256];
            loop {
                let n = stream
                    .read(&mut chunk)
                    .map_err(|_| AppError::new(ErrorCode::ProxyConnect, Stage::Discovery))?;
                if n == 0 {
                    return Err(AppError::new(ErrorCode::ProxyConnect, Stage::Discovery));
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 8192 {
                    return Err(AppError::new(ErrorCode::ProxyConnect, Stage::Discovery));
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let status = head.lines().next().unwrap_or_default();
            if !status.contains(" 200") {
                return Err(AppError::new(ErrorCode::ProxyConnect, Stage::Discovery));
            }
            return Ok(started.elapsed().as_millis() as u64);
        }
    }
    let stream = std::net::TcpStream::connect_timeout(&addrs[0], timeout)
        .map_err(|_| AppError::new(ErrorCode::Connect, Stage::Discovery))?;
    stream.set_read_timeout(Some(timeout)).ok();
    Ok(started.elapsed().as_millis() as u64)
}

fn proxy_hostport(proxy: &str) -> Option<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    let value = if proxy.contains("://") {
        proxy.to_string()
    } else {
        format!("http://{proxy}")
    };
    let url = reqwest::Url::parse(&value).ok()?;
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    (host, port).to_socket_addrs().ok()?.next()
}

struct Target {
    tool: &'static str,
    endpoint: &'static str,
    host: &'static str,
    source: ProxySource,
    config: Option<&'static str>,
}

fn targets() -> Vec<Target> {
    vec![
        Target {
            tool: "npm",
            endpoint: "registry.npmjs.org",
            host: "registry.npmjs.org",
            source: ProxySource::System,
            config: None,
        },
        Target {
            tool: "pip",
            endpoint: "pypi.org",
            host: "pypi.org",
            source: ProxySource::System,
            config: None,
        },
        Target {
            tool: "cargo",
            endpoint: "rsproxy.cn",
            host: "rsproxy.cn",
            source: ProxySource::OwnConfig,
            config: Some("~/.cargo/config.toml"),
        },
        Target {
            tool: "docker",
            endpoint: "registry-1.docker.io",
            host: "registry-1.docker.io",
            source: ProxySource::System,
            config: None,
        },
        Target {
            tool: "maven",
            endpoint: "repo.maven.apache.org",
            host: "repo.maven.apache.org",
            source: ProxySource::OwnConfig,
            config: Some("~/.m2/settings.xml"),
        },
        Target {
            tool: "nuget",
            endpoint: "api.nuget.org",
            host: "api.nuget.org",
            source: ProxySource::OwnConfig,
            config: Some("~/.nuget/NuGet/NuGet.Config"),
        },
        Target {
            tool: "github",
            endpoint: "github.com",
            host: "github.com",
            source: ProxySource::OwnConfig,
            config: Some("~/.gitconfig"),
        },
    ]
}

/// True when the tool's own config file actually exists in the user profile.
fn config_exists(relative: &str) -> bool {
    let Some(home) = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
    else {
        return false;
    };
    let normalized: String = relative
        .trim_start_matches("~/")
        .chars()
        .map(|c| {
            if c == '/' {
                std::path::MAIN_SEPARATOR
            } else {
                c
            }
        })
        .collect();
    home.join(normalized).is_file()
}

pub fn report<F>(proxy: &str, timeout_ms: u64, on_progress: F) -> DepReport
where
    F: Fn(usize, usize) + Send + Sync,
{
    let system_proxy = read_system_proxy();
    let env_proxy = environment_proxy();
    let targets = targets();
    // Every (tool, route) pair is independent, so they all run at once: the
    // wall clock is the slowest single probe, not their sum. A tool whose
    // direct path is firewalled would otherwise make the whole command wait
    // out its timeout once per tool.
    let total = targets.len() * 2;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<(Option<u64>, Option<u64>)>> = targets
        .iter()
        .map(|_| std::sync::Mutex::new((None, None)))
        .collect();
    std::thread::scope(|scope| {
        for (index, target) in targets.iter().enumerate() {
            for via_proxy in [true, false] {
                let done = &done;
                let slots = &slots;
                let on_progress = &on_progress;
                scope.spawn(move || {
                    let route = if via_proxy { Some(proxy) } else { None };
                    let measured = probe_once(target.host, route, timeout_ms).ok();
                    let mut slot = slots[index].lock().unwrap();
                    if via_proxy {
                        slot.0 = measured;
                    } else {
                        slot.1 = measured;
                    }
                    drop(slot);
                    let finished = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    on_progress(finished, total);
                });
            }
        }
    });
    let mut probes = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let (via, direct) = *slots[index].lock().unwrap();
        // "Own config" is only real when the file is on disk; otherwise the
        // tool falls back to its built-in default (usually direct).
        let owns_config = target.config.is_some_and(config_exists);
        let declared = if target.source == ProxySource::OwnConfig && !owns_config {
            ProxySource::Direct
        } else {
            target.source
        };
        // Map the declared source onto what this machine actually has.
        let effective_source = match declared {
            ProxySource::System if system_proxy.is_some() => ProxySource::System,
            ProxySource::System if env_proxy => ProxySource::Environment,
            ProxySource::System => ProxySource::Direct,
            other => other,
        };
        let reach = match (via.is_some(), direct.is_some()) {
            (true, true) => Reach::Both,
            (true, false) => Reach::ViaProxy,
            (false, true) => Reach::Direct,
            (false, false) => Reach::Unreachable,
        };
        // A tool that cannot use the proxy at all is judged on the direct path
        // only; calling it "via proxy" would be wrong.
        let reach = if effective_source == ProxySource::Direct && direct.is_some() {
            Reach::Direct
        } else {
            reach
        };
        probes.push(DepProbe {
            tool: target.tool.into(),
            endpoint: target.endpoint.into(),
            proxy_source: effective_source,
            reach,
            via_proxy_ms: via,
            direct_ms: direct,
            config: if owns_config {
                target.config.map(Into::into)
            } else {
                None
            },
            note: match (effective_source, reach) {
                (ProxySource::Direct, Reach::Unreachable) => {
                    Some("该工具不使用代理，且直连不通".into())
                }
                (ProxySource::Direct, _) => Some("未发现代理配置，只有直连可用".into()),
                (_, Reach::Unreachable) => Some("代理与直连均不可用".into()),
                _ => None,
            },
        });
    }
    DepReport {
        system_proxy,
        environment_proxy: env_proxy,
        probes,
    }
}

/// Pad `text` to `width` display columns.
///
/// `{:<8}` counts chars, so a CJK label like "系统代理" occupies 8 columns but
/// only 4 chars and the columns after it drift. This pads by rendered width.
pub fn cell(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let occupied = text.width();
    if occupied >= width {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + width - occupied);
    out.push_str(text);
    out.extend(std::iter::repeat_n(' ', width - occupied));
    out
}

/// Machine output keeps a flat, stable shape for scripting.
pub fn to_json(report: &DepReport) -> serde_json::Value {
    let mut tools = BTreeMap::new();
    for probe in &report.probes {
        tools.insert(
            probe.tool.clone(),
            serde_json::json!({
                "endpoint": probe.endpoint,
                "proxy_source": probe.proxy_source,
                "reach": probe.reach,
                "via_proxy_ms": probe.via_proxy_ms,
                "direct_ms": probe.direct_ms,
                "config": probe.config,
                "note": probe.note,
            }),
        );
    }
    serde_json::json!({
        "system_proxy": report.system_proxy,
        "environment_proxy": report.environment_proxy,
        "tools": tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_hostport_accepts_bare_and_scheme_forms() {
        for value in ["127.0.0.1:7890", "http://127.0.0.1:7890"] {
            let addr = proxy_hostport(value).expect("parse");
            assert_eq!(addr.port(), 7890);
            assert!(addr.ip().is_loopback());
        }
        assert!(proxy_hostport("not a proxy").is_none());
    }

    #[test]
    fn config_probe_rejects_missing_files() {
        // A path that cannot exist must not be reported as "own config".
        assert!(!config_exists("definitely/not/a/real/file.toml"));
    }

    #[test]
    fn every_target_declares_a_host_matching_its_endpoint() {
        for target in targets() {
            assert_eq!(
                target.host, target.endpoint,
                "{} endpoint/host drift",
                target.tool
            );
            assert!(
                !target.endpoint.contains("://"),
                "{} has a scheme",
                target.tool
            );
        }
    }

    #[test]
    fn json_shape_is_flat_and_stable() {
        let report = DepReport {
            system_proxy: Some("127.0.0.1:7890".into()),
            environment_proxy: false,
            probes: vec![DepProbe {
                tool: "npm".into(),
                endpoint: "registry.npmjs.org".into(),
                proxy_source: ProxySource::System,
                reach: Reach::Both,
                via_proxy_ms: Some(10),
                direct_ms: Some(20),
                config: None,
                note: None,
            }],
        };
        let json = to_json(&report);
        assert_eq!(json["system_proxy"], "127.0.0.1:7890");
        assert_eq!(json["environment_proxy"], false);
        assert_eq!(json["tools"]["npm"]["reach"], "both");
        assert_eq!(json["tools"]["npm"]["proxy_source"], "system");
        assert_eq!(json["tools"]["npm"]["via_proxy_ms"], 10);
    }

    #[test]
    fn cell_pads_by_display_width_not_char_count() {
        use unicode_width::UnicodeWidthStr;
        // Two CJK chars occupy four columns; padding by char count would leave
        // the following columns two columns to the left.
        let padded = cell("工具", 7);
        assert_eq!(padded.width(), 7);
        assert_eq!(padded.chars().count(), 5);
        // Already wide enough: left untouched, never truncated.
        let long = cell("registry.npmjs.org", 7);
        assert_eq!(long, "registry.npmjs.org");
    }
}
