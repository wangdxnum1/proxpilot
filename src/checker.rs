//! 节点探测、真实访问验证与优选切换。

use std::net::{TcpStream, ToSocketAddrs};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::detect::Backend;
use crate::diagnostics::{
    AppError, Availability, ErrorCode, Stage, StreamHealth, StreamOptions, StreamReport,
    VerificationReport,
};
use crate::mihomo::{self, ProxyInfo};
use crate::ui;

/// Pure attribution of one node's stream samples. Kept free of I/O so the
/// gitpilot §5 decision table can be unit-tested exhaustively.
pub fn classify_stream(
    samples: &[crate::diagnostics::StreamSample],
    options: StreamOptions,
) -> StreamReport {
    let requested = samples.len();
    let mut completed = 0usize;
    let mut cuts = 0usize;
    let mut slow = 0usize;
    let mut unknown = 0usize;
    let mut min_ratio = f64::INFINITY;
    let mut rates = Vec::new();
    for sample in samples {
        let ratio = if sample.expected_bytes == 0 {
            0.0
        } else {
            (sample.received_bytes as f64 / sample.expected_bytes as f64).min(1.0)
        };
        min_ratio = min_ratio.min(ratio);
        // Environment/transport setup errors are NOT node-quality failures.
        // They must never be counted as an early cut.
        let setup_failure = sample.error.as_ref().is_some_and(|e| {
            matches!(
                e.code,
                ErrorCode::Tls
                    | ErrorCode::Dns
                    | ErrorCode::Connect
                    | ErrorCode::ProxyConnect
                    | ErrorCode::InvalidInput
                    | ErrorCode::Cancelled
            )
        });
        if setup_failure {
            unknown += 1;
            continue;
        }
        if sample.completed {
            completed += 1;
            rates.push(sample.throughput_kbps);
            if sample.throughput_kbps < options.min_throughput_kbps as f64 {
                slow += 1;
            }
        } else if sample
            .error
            .as_ref()
            .is_some_and(|e| e.code == ErrorCode::Timeout)
            && sample.received_bytes == 0
        {
            // No body bytes and we hit the overall budget: cannot distinguish
            // a stalled node from a stalled target. Do not blame the node.
            unknown += 1;
        } else {
            cuts += 1;
        }
    }
    rates.sort_by(f64::total_cmp);
    let median = if rates.is_empty() {
        0.0
    } else if rates.len() % 2 == 0 {
        (rates[rates.len() / 2 - 1] + rates[rates.len() / 2]) / 2.0
    } else {
        rates[rates.len() / 2]
    };
    let health = if cuts > 0 {
        StreamHealth::Cut
    } else if unknown == requested || requested == 0 {
        StreamHealth::Unknown
    } else if completed == requested && slow == 0 {
        StreamHealth::Healthy
    } else {
        StreamHealth::Degraded
    };
    StreamReport {
        samples_requested: requested,
        samples_completed: completed,
        early_cuts: cuts,
        slow_samples: slow,
        min_received_ratio: if min_ratio.is_finite() {
            min_ratio
        } else {
            0.0
        },
        median_throughput_kbps: median,
        health,
        samples: samples.to_vec(),
        represents: "proxy_link".into(),
    }
}

/// One node's concurrent stream samples. Returns `None` when the stream
/// dimension is disabled or the probe could not be constructed.
pub fn run_stream(proxy: &str, url: &str, options: Option<StreamOptions>) -> Option<StreamReport> {
    let options = options?;
    let probe = match crate::http::StreamProbe::new(proxy) {
        Ok(probe) => probe,
        Err(error) => {
            ui::warn(&format!("长连接检测不可用，跳过该维度：{error}"));
            return None;
        }
    };
    let bytes = options.bytes;
    let spinner = ui::Spinner::start(&format!(
        "长连接检测：并发 {} 次采样（每次 {} KiB）...",
        options.samples,
        bytes / 1024
    ));
    let result = probe.test_stream_with_progress(
        url,
        options,
        &crate::operation::Cancellation::global(),
        |done, total| {
            spinner.set_text(format!(
                "长连接检测：并发 {} 次采样，已完成 {}/{}（每次 {} KiB）",
                total,
                done,
                total,
                bytes / 1024
            ));
        },
    );
    drop(spinner);
    match result {
        Ok(samples) => Some(classify_stream(&samples, options)),
        Err(error) => {
            ui::warn(&format!("长连接检测未完成，跳过该维度：{error}"));
            None
        }
    }
}

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
pub fn verify_access_report_with_timeout(
    proxy: &str,
    url: &str,
    samples: usize,
    timeout_ms: u32,
) -> VerificationReport {
    let n = samples.max(1);
    let mut report = VerificationReport {
        availability: Availability::Failed,
        mean_ms: None,
        samples_requested: n,
        samples_passed: 0,
        error: None,
        stream: None,
    };
    let probe = match crate::http::BrowserProbe::new_with_timeout(proxy, timeout_ms) {
        Ok(probe) => probe,
        Err(error) => {
            report.error = Some(error);
            return report;
        }
    };
    let spinner = ui::Spinner::start(&format!("实测 {}（第 1/{} 次）", ui::display_url(url), n));
    let mut total = 0.0;
    let mut transport_failed = false;
    for sample in 0..n {
        spinner.set_text(format!(
            "实测 {}（第 {}/{} 次）",
            ui::display_url(url),
            sample + 1,
            n
        ));
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

/// Same evidence as `verify_access_report_with_timeout`, plus the optional
/// git long-connection dimension. Existing callers keep their old signature.
pub fn verify_access_with_stream(
    proxy: &str,
    url: &str,
    samples: usize,
    timeout_ms: u32,
    stream: Option<(String, StreamOptions)>,
) -> VerificationReport {
    let mut report = verify_access_report_with_timeout(proxy, url, samples, timeout_ms);
    if let Some((stream_url, options)) = stream {
        report.stream = run_stream(proxy, &stream_url, Some(options));
    }
    report
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CurrentVerification {
    pub verification: VerificationReport,
    pub state: Option<crate::switching::LiveState>,
    pub stable: bool,
    pub controller_error: Option<AppError>,
    pub history_warning: Option<AppError>,
}

pub fn verify_current_report(
    be: &Backend,
    api: &crate::core_api::CoreApi,
    group: &str,
    url: &str,
    samples: usize,
    timeout_ms: u32,
) -> CurrentVerification {
    verify_current_with_stream(be, api, group, url, samples, timeout_ms, None)
}

/// Adds the optional stream dimension to the current-exit check.
///
/// The stream samples lengthen the window between the before/after state
/// snapshots, so the result is only attributed to the node when the state is
/// still stable afterwards (see `stable` handling below).
pub fn verify_current_with_stream(
    be: &Backend,
    api: &crate::core_api::CoreApi,
    group: &str,
    url: &str,
    samples: usize,
    timeout_ms: u32,
    stream: Option<(String, StreamOptions)>,
) -> CurrentVerification {
    use crate::switching::SwitchControl;
    let control = crate::runtime_switch::KernelSwitchControl { backend: be, api };
    let mut controller_error = None;
    let before = if group.is_empty() {
        None
    } else {
        match control.state(group) {
            Ok(state) => Some(state),
            Err(error) => {
                controller_error = Some(error);
                None
            }
        }
    };
    let report = verify_access_with_stream(&be.proxy, url, samples, timeout_ms, stream);
    let mut result = CurrentVerification {
        verification: report,
        state: before.clone(),
        stable: false,
        controller_error,
        history_warning: None,
    };
    if result
        .verification
        .error
        .as_ref()
        .is_some_and(|e| e.code == ErrorCode::Cancelled)
        || group.is_empty()
    {
        return result;
    }
    match control.state(group) {
        Ok(mut after) => {
            after.members.sort();
            if let Some(mut before) = before {
                before.members.sort();
                result.stable = before.identity == after.identity
                    && before.profile == after.profile
                    && before.members == after.members
                    && before.selection == after.selection;
                if result.stable {
                    if let Some(node) = before.selection {
                        let mut history =
                            crate::history::HistorySession::new(be, before.profile, group, url);
                        history.record_verification(be, &node, &result.verification);
                        result.history_warning = history.warning;
                    }
                }
            }
            result.state = Some(after);
        }
        Err(error) => result.controller_error = Some(error),
    }
    // The stream window is longer than a webpage sample. If the selection,
    // subscription or members changed during it, the evidence cannot be
    // attributed to the node we measured -- report Unknown, never a verdict.
    if !result.stable {
        if let Some(stream) = result.verification.stream.as_mut() {
            stream.health = StreamHealth::Unknown;
        }
    }
    result
}

pub fn is_real_node(info: &ProxyInfo, name: &str) -> bool {
    info.all.is_none()
        && !SKIP_TYPES.contains(&info.ptype.as_str())
        && !FAKE_NODES.iter().any(|f| name.contains(f))
}

pub type ScanReport = crate::probe_scheduler::ProbeRun;

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
        self.controller_error.as_ref()
    }
}

pub fn scan_report(
    be: &Backend,
    agent: &crate::core_api::CoreApi,
    members: &[String],
    url: &str,
    options: &crate::probe_scheduler::ProbeOptions,
    group: &str,
) -> ScanReport {
    let mut history = crate::history::HistorySession::new(
        be,
        crate::appstate::current_profile_id(be),
        group,
        url,
    );
    let mut report = scan_network_report(be, agent, members, url, options);
    history.record_probes(be, &report);
    report.history_warning = history.warning;
    report
}

fn scan_network_report(
    be: &Backend,
    agent: &crate::core_api::CoreApi,
    members: &[String],
    url: &str,
    options: &crate::probe_scheduler::ProbeOptions,
) -> ScanReport {
    let cancel = crate::operation::Cancellation::global();
    let cpu = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let spinner = ui::Spinner::start(&format!(
        "已测 0/{} · 可达 0 · 未通过 0 · 重试 0（并发上限 {}）",
        members.len(),
        options.limit(cpu, members.len())
    ));
    let mut report = crate::probe_scheduler::run_with_progress(
        members,
        options,
        cpu,
        &cancel,
        |name, timeout| mihomo::probe_delay(be, agent, name, url, timeout),
        |progress| {
            spinner.set_text(format!(
                "已测 {}/{} · 可达 {} · 未通过 {} · 重试 {}（并发上限 {}）",
                progress.completed,
                progress.total,
                progress.reachable,
                progress.failed,
                progress.retries,
                options.limit(cpu, members.len()),
            ));
        },
    );
    drop(spinner);
    if !report.cancelled
        && report.controller_error.is_none()
        && !report.measurements.is_empty()
        && report.measurements.iter().all(|m| m.error.is_some())
    {
        let health = agent
            .request(
                be,
                reqwest::Method::GET,
                "/version",
                None,
                Duration::from_secs(2),
            )
            .and_then(|response| {
                let value: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|_| AppError::new(ErrorCode::InvalidResponse, Stage::Controller))?;
                if value
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .is_none()
                {
                    return Err(AppError::new(ErrorCode::InvalidResponse, Stage::Controller));
                }
                Ok(())
            });
        if let Err(error) = health {
            report.controller_error = Some(error);
        } else if let Some(error) = report
            .measurements
            .iter()
            .filter_map(|measurement| measurement.error.as_ref())
            .find(|error| {
                error.stage == Stage::Controller || error.code == ErrorCode::InvalidResponse
            })
        {
            let mut error = error.clone();
            error.stage = Stage::Controller;
            report.controller_error = Some(error);
        }
    }
    ui::dim(&format!(
        "已完成 {} 个唯一节点；重试 {} 次；请求并发峰值 {}（上限 {}）",
        report.measurements.len(),
        report.retries,
        report.peak_concurrency,
        report.concurrency_limit
    ));
    report
}

#[cfg(test)]
fn scan_with_concurrency(
    be: &Backend,
    agent: &crate::core_api::CoreApi,
    members: &[String],
    url: &str,
    concurrency: usize,
) -> Vec<(i64, String)> {
    scan_network_report(
        be,
        agent,
        members,
        url,
        &crate::probe_scheduler::ProbeOptions {
            concurrency: Some(concurrency),
            retries: 0,
            ..Default::default()
        },
    )
    .reachable()
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

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::diagnostics::{StreamHealth, StreamSample};

    fn options() -> StreamOptions {
        StreamOptions {
            bytes: 1024,
            samples: 5,
            timeout_ms: 15000,
            min_throughput_kbps: 200,
            first_byte_timeout_ms: 5000,
        }
    }
    fn completed(rate: f64) -> StreamSample {
        StreamSample {
            completed: true,
            received_bytes: 1024,
            expected_bytes: 1024,
            first_byte_ms: Some(120.0),
            elapsed_ms: 800.0,
            throughput_kbps: rate,
            error: None,
        }
    }
    fn cut(received: u64, code: ErrorCode) -> StreamSample {
        StreamSample {
            completed: false,
            received_bytes: received,
            expected_bytes: 1024,
            first_byte_ms: if received > 0 { Some(300.0) } else { None },
            elapsed_ms: 3160.0,
            throughput_kbps: 0.0,
            error: Some(AppError::new(code, Stage::Stream)),
        }
    }

    #[test]
    fn all_fast_complete_is_healthy() {
        let samples: Vec<_> = (0..5).map(|_| completed(3000.0)).collect();
        let report = classify_stream(&samples, options());
        assert_eq!(report.health, StreamHealth::Healthy);
        assert_eq!(report.samples_completed, 5);
        assert_eq!(report.early_cuts, 0);
        assert_eq!(report.min_received_ratio, 1.0);
    }

    #[test]
    fn one_early_cut_is_enough_to_reject_the_node() {
        // gitpilot 2026-11-11 measured 5/9: a single sample hides it.
        let mut samples = vec![completed(3000.0); 4];
        samples.push(cut(0, ErrorCode::ResponseTransfer));
        let report = classify_stream(&samples, options());
        assert_eq!(report.health, StreamHealth::Cut);
        assert_eq!(report.early_cuts, 1);
        assert_eq!(report.min_received_ratio, 0.0);
    }

    #[test]
    fn slow_but_complete_is_degraded_not_cut() {
        let samples: Vec<_> = (0..5).map(|_| completed(100.0)).collect();
        let report = classify_stream(&samples, options());
        assert_eq!(report.health, StreamHealth::Degraded);
        assert_eq!(report.slow_samples, 5);
        assert_eq!(report.early_cuts, 0);
    }

    #[test]
    fn tls_and_timeout_setup_failures_are_unknown_not_node_faults() {
        let samples = vec![
            cut(0, ErrorCode::Tls),
            cut(0, ErrorCode::Connect),
            cut(0, ErrorCode::ProxyConnect),
            cut(0, ErrorCode::Timeout), // 0 bytes + timeout => not attributable
        ];
        let report = classify_stream(&samples, options());
        assert_eq!(report.health, StreamHealth::Unknown);
        assert_eq!(report.early_cuts, 0);
    }

    #[test]
    fn empty_and_cancelled_samples_are_unknown() {
        assert_eq!(
            classify_stream(&[], options()).health,
            StreamHealth::Unknown
        );
        let report = classify_stream(&[cut(0, ErrorCode::Cancelled)], options());
        assert_eq!(report.health, StreamHealth::Unknown);
    }

    #[test]
    fn report_never_exposes_the_target_url() {
        let report = classify_stream(&[completed(3000.0)], options());
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("http"));
        assert_eq!(report.represents, "proxy_link");
    }
}
