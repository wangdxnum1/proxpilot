//! 节点探测、真实访问验证与优选切换。

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::detect::Backend;
use crate::diagnostics::{AppError, Availability, ErrorCode, Stage, VerificationReport};
use crate::mihomo::{self, ProxyInfo};
use crate::ui;

/// 机场放在节点列表里的假节点（套餐信息占位）
const FAKE_NODES: [&str; 13] = [
    "倍率提示",
    "剩余流量",
    "套餐到期",
    "到期",
    "剩余",
    "官网",
    "流量",
    "续费",
    "订阅地址",
    "订阅网址",
    "更新订阅",
    "http://",
    "https://",
];
/// 组类型（不是真实节点）
const SKIP_TYPES: [&str; 7] = [
    "Selector",
    "URLTest",
    "Fallback",
    "Direct",
    "Reject",
    "Compatible",
    "Pass",
];

pub fn parse_addr(addr: &str) -> (String, u16) {
    let value = if addr.contains("://") {
        addr.to_string()
    } else {
        format!("http://{}", addr)
    };
    match reqwest::Url::parse(&value) {
        Ok(url) => (
            url.host_str()
                .unwrap_or("127.0.0.1")
                .trim_matches(['[', ']'])
                .to_string(),
            url.port_or_known_default().unwrap_or(7890),
        ),
        Err(_) => ("127.0.0.1".into(), 7890),
    }
}

pub fn core_alive(proxy: &str) -> bool {
    let (host, port) = parse_addr(proxy);
    match (host.as_str(), port).to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(sa) => TcpStream::connect_timeout(&sa, Duration::from_secs(2)).is_ok(),
            None => false,
        },
        Err(_) => false,
    }
}

/// 访问判定结果
#[derive(Debug, Clone)]
pub enum Verdict {
    /// 全部 200，平均耗时（秒）
    Pass(f64),
    /// chatgpt.com 返回 403，但 api.openai.com IP 检查通过（401）：
    /// 仅作为备选信号，不能保证浏览器通过 Cloudflare 验证
    PassIpOnly,
    /// 不可访问；保留传输类别，HTTP 状态可为空。
    Fail(AppError),
}

impl Verdict {
    pub fn is_ok(&self) -> bool {
        !matches!(self, Verdict::Fail(_))
    }
}

fn is_openai_url(url: &str) -> bool {
    reqwest::Url::parse(url).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| {
                let host = host.trim_end_matches('.');
                host == "chatgpt.com"
                    || host.ends_with(".chatgpt.com")
                    || host == "openai.com"
                    || host.ends_with(".openai.com")
            })
    })
}

/// 实测报告保留状态与失败类别；IP-only 不能冒充网页通过。
pub fn verify_access_report(proxy: &str, url: &str, samples: usize) -> VerificationReport {
    let n = samples.max(1);
    let mut report = VerificationReport {
        availability: Availability::Failed,
        mean_ms: None,
        samples_requested: n,
        samples_passed: 0,
        error: None,
    };
    let probe = match crate::http::BrowserProbe::new(proxy) {
        Ok(probe) => probe,
        Err(error) => {
            report.error = Some(error);
            return report;
        }
    };
    let spinner = ui::Spinner::start(&format!("实测 {}（第 1/{} 次）", url, n));
    let mut total = 0.0;
    let mut transport_failed = false;
    for sample in 0..n {
        spinner.set_text(format!("实测 {}（第 {}/{} 次）", url, sample + 1, n));
        match probe.test_detailed(url) {
            Ok((200, seconds)) => {
                total += seconds;
                report.samples_passed += 1;
            }
            Ok((status, _)) => report.error = Some(AppError::website_status(status)),
            Err(error) => {
                if error.code == ErrorCode::Cancelled {
                    report.error = Some(error);
                    return report;
                }
                transport_failed = true;
                report.error = Some(error);
            }
        }
    }
    if report.samples_passed == n {
        report.availability = Availability::Webpage;
        report.mean_ms = Some(total * 1000.0 / n as f64);
        return report;
    }
    let may_cross_check = is_openai_url(url)
        && !transport_failed
        && report.error.as_ref().and_then(|error| error.http_status) == Some(403);
    if may_cross_check {
        spinner.set_text("网页返回 403，交叉检查 OpenAI API 响应...");
        let cross_check = probe.test_detailed("https://api.openai.com/v1/models");
        if let Err(error) = &cross_check {
            if error.code == ErrorCode::Cancelled {
                report.error = Some(error.clone());
                return report;
            }
        }
        if matches!(cross_check, Ok((401, _))) {
            for k in 0..2 {
                spinner.set_text(format!(
                    "API 返回未授权响应，复测隧道稳定性（{}/2）...",
                    k + 1
                ));
                if let Err(error) = probe.test_detailed(url) {
                    report.error = Some(error);
                    return report;
                }
            }
            report.availability = Availability::IpOnly;
        }
    }
    report
}

pub fn verify_access(proxy: &str, url: &str, samples: usize) -> Verdict {
    let report = verify_access_report(proxy, url, samples);
    match report.availability {
        Availability::Webpage => Verdict::Pass(report.mean_ms.unwrap_or_default() / 1000.0),
        Availability::IpOnly => Verdict::PassIpOnly,
        Availability::Failed => Verdict::Fail(
            report
                .error
                .unwrap_or_else(|| AppError::new(ErrorCode::TransportUnknown, Stage::Verification)),
        ),
    }
}

pub fn is_real_node(info: &ProxyInfo, name: &str) -> bool {
    info.all.is_none()
        && !SKIP_TYPES.contains(&info.ptype.as_str())
        && !FAKE_NODES.iter().any(|f| name.contains(f))
}

/// 可用逻辑 CPU 数量的两倍，线程数不超过待测节点数。
pub fn scan_concurrency(node_count: usize) -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_mul(2)
        .min(node_count)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeMeasurement {
    pub node: String,
    pub delay_ms: Option<i64>,
    pub error: Option<AppError>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ScanReport {
    pub measurements: Vec<ProbeMeasurement>,
    pub cancelled: bool,
}

impl ScanReport {
    pub fn reachable(&self) -> Vec<(i64, String)> {
        let mut reachable: Vec<_> = self
            .measurements
            .iter()
            .filter_map(|m| m.delay_ms.map(|delay| (delay, m.node.clone())))
            .collect();
        reachable.sort();
        reachable
    }

    pub fn controller_failure(&self) -> Option<&AppError> {
        self.measurements
            .iter()
            .filter_map(|m| m.error.as_ref())
            .find(|error| {
                matches!(
                    error.code,
                    ErrorCode::ControllerAuth | ErrorCode::IdentityMismatch
                )
            })
    }
}

pub fn scan_reachable_report(
    be: &Backend,
    agent: &crate::core_api::CoreApi,
    members: &[String],
    url: &str,
) -> ScanReport {
    scan_report_with_concurrency(be, agent, members, url, scan_concurrency(members.len()))
}

#[cfg(test)]
fn scan_with_concurrency(
    be: &Backend,
    agent: &crate::core_api::CoreApi,
    members: &[String],
    url: &str,
    concurrency: usize,
) -> Vec<(i64, String)> {
    scan_report_with_concurrency(be, agent, members, url, concurrency).reachable()
}

fn scan_report_with_concurrency(
    be: &Backend,
    agent: &crate::core_api::CoreApi,
    members: &[String],
    url: &str,
    concurrency: usize,
) -> ScanReport {
    let cancellation = crate::operation::Cancellation::global();
    let total = members.len();
    let next = AtomicUsize::new(0);
    let done = Arc::new(AtomicUsize::new(0));
    let spinner = ui::Spinner::start(&format!("{} 路并发探测 {} 个节点...", concurrency, total));
    let measurements = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..concurrency {
            let next = &next;
            let measurements = &measurements;
            let done = &done;
            let spinner = &spinner;
            let cancellation = &cancellation;
            s.spawn(move || {
                while !cancellation.is_cancelled() {
                    let Some(name) = members.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    if cancellation.is_cancelled() {
                        break;
                    }
                    let (delay_ms, error) = match mihomo::probe_delay(be, agent, name, url, 4000) {
                        Ok(delay) => (Some(delay), None),
                        Err(error) => (None, Some(error)),
                    };
                    measurements.lock().unwrap().push(ProbeMeasurement {
                        node: name.clone(),
                        delay_ms,
                        error,
                    });
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    spinner.set_text(format!("并发探测节点... 已测 {}/{}", n, total));
                }
            });
        }
    });
    drop(spinner);
    let mut measurements = measurements.into_inner().unwrap();
    measurements.sort_by(|a, b| a.node.cmp(&b.node));
    ScanReport {
        measurements,
        cancelled: cancellation.is_cancelled(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::{accept, listener, read_request};
    use std::io::Write;
    use std::time::Instant;

    #[test]
    fn openai_cross_check_only_applies_to_real_target_hosts() {
        for url in [
            "https://openai.com.example.org/",
            "https://example.org/?next=chatgpt.com",
            "https://openai.com@evil.example/",
        ] {
            assert!(
                !is_openai_url(url),
                "must not cross-check unrelated host: {url}"
            );
        }
        for url in [
            "https://CHATGPT.COM/",
            "https://api.openai.com/v1/models",
            "https://chatgpt.com/",
        ] {
            assert!(is_openai_url(url), "legitimate host: {url}");
        }
    }

    #[test]
    fn scan_workers_obey_limit_and_process_every_node_once() {
        let listener = listener();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let first_pair = AtomicUsize::new(0);
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                let mut requests = Vec::new();
                thread::scope(|handlers| {
                    let mut tasks = Vec::new();
                    for index in 0..5 {
                        let mut stream = accept(&listener);
                        let active = &active;
                        let peak = &peak;
                        let first_pair = &first_pair;
                        tasks.push(handlers.spawn(move || {
                            let request = read_request(&mut stream);
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(current, Ordering::SeqCst);
                            if index < 2 {
                                first_pair.fetch_add(1, Ordering::SeqCst);
                                let start = Instant::now();
                                while first_pair.load(Ordering::SeqCst) < 2 {
                                    assert!(start.elapsed() < Duration::from_secs(5), "second concurrent request did not arrive");
                                    thread::sleep(Duration::from_millis(1));
                                }
                            }
                            thread::sleep(Duration::from_millis(20));
                            // 第三个响应是不可达节点，其余节点按延迟排序。
                            let delay = if index == 2 { 0 } else { 5 - index };
                            let body = format!(r#"{{"delay":{delay}}}"#);
                            active.fetch_sub(1, Ordering::SeqCst);
                            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                            request
                        }));
                    }
                    for task in tasks {
                        requests.push(task.join().unwrap());
                    }
                });
                requests
            });
            let backend = Backend {
                kind: None,
                client: "test".into(),
                api: api.into(),
                secret: None,
                proxy: String::new(),
                source: "test".into(),
                version: None,
                alternatives: Vec::new(),
            };
            let members: Vec<_> = (0..5).map(|i| format!("node{i}")).collect();
            let results = scan_with_concurrency(
                &backend,
                &crate::core_api::CoreApi::new().unwrap(),
                &members,
                "http://test.invalid/",
                2,
            );
            assert_eq!(peak.load(Ordering::SeqCst), 2);
            assert_eq!(results.len(), 4);
            assert!(results.windows(2).all(|pair| pair[0] <= pair[1]));
            let requests = server.join().unwrap();
            for member in members {
                assert_eq!(
                    requests
                        .iter()
                        .filter(|r| r.contains(&format!("/proxies/{member}/delay?")))
                        .count(),
                    1
                );
            }
        });
    }
}

#[cfg(test)]
mod rate_policy_tests {
    use super::*;
    use crate::http::test_support::{accept, listener, read_request};
    use std::io::Write;
    #[test]
    fn no_compliant_candidates_never_probe_or_switch() {
        let listener = listener();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let request = read_request(&mut stream);
            let body=serde_json::json!({"proxies":{"test":{"type":"Selector","now":"expensive-3倍率","all":["expensive-3倍率","unknown","✅续费网址:https://example.com"]},"expensive-3倍率":{"type":"SS"},"unknown":{"type":"SS"},"✅续费网址:https://example.com":{"type":"SS"}}}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            drop(stream);
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                listener.accept().is_err(),
                "unexpected probe or switch after no eligible candidates"
            );
            request
        });
        let be = Backend {
            kind: None,
            client: "mock".into(),
            api: url.into(),
            secret: None,
            proxy: "http://127.0.0.1:1".into(),
            source: "test".into(),
            version: None,
            alternatives: vec![],
        };
        let args = crate::parse_args_from(
            ["fix", "--group", "test", "--max-rate", "1"].map(str::to_string),
        )
        .unwrap();
        assert!(
            crate::workflow::fix_flow(&be, &crate::core_api::CoreApi::new().unwrap(), &args)
                .unwrap_err()
                .to_string()
                .contains("未进行节点切换")
        );
        assert!(server.join().unwrap().starts_with("GET /proxies "));
    }
}
