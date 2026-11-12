//! Fast probe for a connection lifetime cap on the proxy link.
//!
//! The streaming check (`--git-check`) downloads megabytes and looks at when
//! failures cluster. That answers the question from the data side. This probe
//! answers it from the other side: open a tunnel through the proxy, send
//! nothing, and watch how long the tunnel survives before the peer closes it.
//!
//! An idle tunnel that dies at a repeatable age is direct evidence of a
//! lifetime cap somewhere in the path: no bytes were in flight, so neither
//! throughput nor a transfer quota can explain the break. The probe reports the
//! pattern; it does not attribute the cause to the node, the relay or the
//! target.

use crate::diagnostics::{AppError, ErrorCode, Stage};
use serde::Serialize;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEFAULT_WINDOW_MS: u64 = 12_000;
pub const DEFAULT_SAMPLES: usize = 6;
pub const MAX_WINDOW_MS: u64 = 60_000;
pub const MAX_SAMPLES: usize = 16;
/// How often a waiting thread re-checks the deadline and the stop flag.
const POLL_MS: u64 = 100;
/// Ages closer together than this are treated as one repeatable moment.
const CLUSTER_TOLERANCE_MS: f64 = 400.0;

#[derive(Clone, Debug, Serialize)]
pub struct LifetimeSample {
    /// How long the tunnel stayed open before the peer closed it.
    pub survived_ms: f64,
    /// True when the peer closed the tunnel instead of the window expiring.
    pub closed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeVerdict {
    /// Trials closed at one repeatable age, so a fixed lifetime cap exists.
    CapObserved,
    /// Most trials closed, but at differing ages: a cap exists whose length
    /// varies. An idle tunnel still should not die inside the window, so the
    /// closes are evidence of a cap even without a single agreed age.
    CapVariable,
    /// Every tunnel was still open when the observation window ended.
    NoCapObserved,
    /// Too few closes to conclude anything.
    Inconclusive,
    /// No tunnel could be opened at all.
    Unreachable,
}

#[derive(Clone, Debug, Serialize)]
pub struct LifetimeReport {
    pub host: String,
    pub samples_requested: usize,
    pub samples_closed: usize,
    pub window_ms: u64,
    /// Median age at close, over the tunnels the peer closed.
    pub lifespan_ms: Option<f64>,
    /// Widest gap between two observed close ages.
    pub spread_ms: Option<f64>,
    pub verdict: LifetimeVerdict,
    pub samples: Vec<LifetimeSample>,
    /// `proxy_link` — this measures the link, not a real client.
    pub represents: String,
}

impl LifetimeReport {
    /// True when every observed close agrees closely enough that a random
    /// network failure cannot explain them.
    pub fn is_repeatable(&self) -> bool {
        self.verdict == LifetimeVerdict::CapObserved
    }
    /// True whenever the closes show a cap exists, whether its length is fixed
    /// or varies between trials.
    #[cfg(test)]
    fn has_cap(&self) -> bool {
        matches!(
            self.verdict,
            LifetimeVerdict::CapObserved | LifetimeVerdict::CapVariable
        )
    }
}

/// Open `samples` tunnels through `proxy` to `host`, send nothing, and record
/// when each one is closed.
pub fn probe(proxy: &str, host: &str, samples: usize, window_ms: u64) -> LifetimeReport {
    let samples = samples.clamp(1, MAX_SAMPLES);
    let window_ms = window_ms.clamp(500, MAX_WINDOW_MS);
    let base = LifetimeReport {
        host: host.to_string(),
        samples_requested: samples,
        samples_closed: 0,
        window_ms,
        lifespan_ms: None,
        spread_ms: None,
        verdict: LifetimeVerdict::Unreachable,
        samples: Vec::new(),
        represents: "proxy_link".into(),
    };
    let Some(addr) = proxy_addr(proxy) else {
        return base;
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel::<(usize, LifetimeSample)>();
    let mut handles = Vec::with_capacity(samples);
    for index in 0..samples {
        let sender = sender.clone();
        let stop = Arc::clone(&stop);
        let host = host.to_string();
        handles.push(std::thread::spawn(move || {
            let sample = watch_one(addr, &host, window_ms, &stop);
            let _ = sender.send((index, sample));
        }));
    }
    drop(sender);

    let mut collected = vec![None; samples];
    // Stop early once the verdict is already settled: either the closes agree,
    // or a majority has closed at differing ages. Waiting for the rest would
    // only stretch the command for a conclusion that cannot change.
    while let Ok((index, sample)) = receiver.recv() {
        collected[index] = Some(sample);
        let closed: Vec<f64> = collected
            .iter()
            .flatten()
            .filter(|s| s.closed && s.survived_ms > 0.0)
            .map(|s| s.survived_ms)
            .collect();
        if repeatable(&closed, samples) || closed.len() * 2 > samples {
            stop.store(true, Ordering::Relaxed);
        }
    }
    for handle in handles {
        let _ = handle.join();
    }

    let mut samples: Vec<LifetimeSample> = collected.into_iter().flatten().collect();
    samples.sort_by(|a, b| a.survived_ms.total_cmp(&b.survived_ms));
    summarise(base, samples)
}

fn summarise(mut report: LifetimeReport, samples: Vec<LifetimeSample>) -> LifetimeReport {
    let closed: Vec<f64> = samples
        .iter()
        .filter(|s| s.closed && s.survived_ms > 0.0)
        .map(|s| s.survived_ms)
        .collect();
    report.samples_closed = closed.len();
    let opened = samples
        .iter()
        .any(|s| s.error.is_none() || s.survived_ms > 0.0);
    report.lifespan_ms = median(&closed);
    report.spread_ms = spread(&closed);
    let requested = report.samples_requested;
    report.verdict = if !opened || samples.iter().all(|s| s.survived_ms == 0.0) {
        LifetimeVerdict::Unreachable
    } else if closed.is_empty() {
        LifetimeVerdict::NoCapObserved
    } else if repeatable(&closed, requested) {
        LifetimeVerdict::CapObserved
    } else if closed.len() >= 2 && closed.len() * 2 >= requested {
        // Most idle tunnels died inside the window but at differing ages: a cap
        // exists, its length just is not constant. Silently calling this
        // inconclusive would hide the very symptom being hunted.
        LifetimeVerdict::CapVariable
    } else {
        LifetimeVerdict::Inconclusive
    };
    report.samples = samples;
    report
}

/// A conclusion needs at least two agreeing closes and enough of the sample to
/// rule out a one-off.
fn repeatable(closed: &[f64], requested: usize) -> bool {
    if closed.len() < 2 || closed.len() * 2 < requested {
        return false;
    }
    let (Some(centre), Some(width)) = (median(closed), spread(closed)) else {
        return false;
    };
    width <= CLUSTER_TOLERANCE_MS.max(centre * 0.1)
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(if sorted.len() % 2 == 0 {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    })
}

fn spread(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let (min, max) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    Some(max - min)
}

/// Open the tunnel and report how long it lived.
fn watch_one(addr: SocketAddr, host: &str, window_ms: u64, stop: &AtomicBool) -> LifetimeSample {
    let mut stream = match open_tunnel(addr, host) {
        Ok(stream) => stream,
        Err(error) => {
            return LifetimeSample {
                survived_ms: 0.0,
                closed: false,
                error: Some(error),
            }
        }
    };
    let started = Instant::now();
    let window = Duration::from_millis(window_ms);
    let mut buffer = [0u8; 4096];
    loop {
        if stop.load(Ordering::Relaxed) {
            return LifetimeSample {
                survived_ms: started.elapsed().as_secs_f64() * 1000.0,
                closed: false,
                error: None,
            };
        }
        let remaining = window.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return LifetimeSample {
                survived_ms: window_ms as f64,
                closed: false,
                error: None,
            };
        }
        let slice = remaining.min(Duration::from_millis(POLL_MS));
        if stream.set_read_timeout(Some(slice)).is_err() {
            return LifetimeSample {
                survived_ms: started.elapsed().as_secs_f64() * 1000.0,
                closed: true,
                error: None,
            };
        }
        match stream.read(&mut buffer) {
            // The peer closed the tunnel at a moment of its choosing.
            Ok(0) => {
                return LifetimeSample {
                    survived_ms: started.elapsed().as_secs_f64() * 1000.0,
                    closed: true,
                    error: None,
                }
            }
            // Idle tunnels should stay silent; if the peer speaks we keep going.
            Ok(_) => continue,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                continue
            }
            Err(_) => {
                return LifetimeSample {
                    survived_ms: started.elapsed().as_secs_f64() * 1000.0,
                    closed: true,
                    error: None,
                }
            }
        }
    }
}

fn open_tunnel(addr: SocketAddr, host: &str) -> Result<TcpStream, AppError> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|_| AppError::new(ErrorCode::ProxyConnect, Stage::Stream))?;
    let _ = stream.set_nodelay(true);
    let request = format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|_| AppError::new(ErrorCode::ProxyConnect, Stage::Stream))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut response = Vec::new();
    let mut buffer = [0u8; 512];
    while !response.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buffer[..n]),
            Err(_) => return Err(AppError::new(ErrorCode::ProxyConnect, Stage::Stream)),
        }
        if response.len() > 8192 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&response);
    if !head.starts_with("HTTP/1.") || !head.contains(" 200") {
        return Err(AppError::new(ErrorCode::ProxyConnect, Stage::Stream));
    }
    Ok(stream)
}

fn proxy_addr(proxy: &str) -> Option<SocketAddr> {
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

pub fn to_json(report: &LifetimeReport) -> serde_json::Value {
    serde_json::json!({
        "host": report.host,
        "window_ms": report.window_ms,
        "samples_requested": report.samples_requested,
        "samples_closed": report.samples_closed,
        "lifespan_ms": report.lifespan_ms,
        "spread_ms": report.spread_ms,
        "verdict": report.verdict,
        "repeatable": report.is_repeatable(),
        "represents": report.represents,
        "samples": report.samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(survived_ms: f64, closed: bool) -> LifetimeSample {
        LifetimeSample {
            survived_ms,
            closed,
            error: None,
        }
    }

    fn report(samples: Vec<LifetimeSample>) -> LifetimeReport {
        let base = LifetimeReport {
            host: "example.test".into(),
            samples_requested: samples.len(),
            samples_closed: 0,
            window_ms: DEFAULT_WINDOW_MS,
            lifespan_ms: None,
            spread_ms: None,
            verdict: LifetimeVerdict::Unreachable,
            samples: Vec::new(),
            represents: "proxy_link".into(),
        };
        summarise(base, samples)
    }

    #[test]
    fn agreeing_closes_are_a_repeatable_cap() {
        let value = report(vec![
            sample(3006.0, true),
            sample(3004.0, true),
            sample(3007.0, true),
            sample(3005.0, true),
        ]);
        assert_eq!(value.verdict, LifetimeVerdict::CapObserved);
        assert!(value.is_repeatable());
        assert_eq!(value.samples_closed, 4);
        assert!((value.lifespan_ms.unwrap() - 3005.5).abs() < 0.01);
    }

    #[test]
    fn surviving_tunnels_mean_no_cap_within_the_window() {
        let value = report(vec![
            sample(DEFAULT_WINDOW_MS as f64, false),
            sample(DEFAULT_WINDOW_MS as f64, false),
            sample(DEFAULT_WINDOW_MS as f64, false),
        ]);
        assert_eq!(value.verdict, LifetimeVerdict::NoCapObserved);
        assert!(value.lifespan_ms.is_none());
    }

    #[test]
    fn scattered_closes_still_prove_a_cap_exists() {
        let value = report(vec![
            sample(302.0, true),
            sample(4100.0, true),
            sample(9000.0, true),
            sample(11000.0, true),
        ]);
        // Idle tunnels should never die; differing ages mean an unstable cap,
        // not "no signal".
        assert_eq!(value.verdict, LifetimeVerdict::CapVariable);
        assert!(value.has_cap());
        assert!(!value.is_repeatable());
    }

    #[test]
    fn a_single_close_is_never_a_conclusion() {
        let value = report(vec![sample(3005.0, true), sample(12000.0, false)]);
        assert_eq!(value.verdict, LifetimeVerdict::Inconclusive);
        assert!(!value.has_cap());
    }

    #[test]
    fn a_minority_of_closes_is_not_enough_to_conclude() {
        let value = report(vec![
            sample(3005.0, true),
            sample(12000.0, false),
            sample(12000.0, false),
            sample(12000.0, false),
        ]);
        assert_eq!(value.verdict, LifetimeVerdict::Inconclusive);
        assert!(!value.has_cap());
    }

    #[test]
    fn refused_tunnels_are_unreachable_not_a_cap() {
        let error = AppError::new(ErrorCode::ProxyConnect, Stage::Stream);
        let value = report(vec![
            LifetimeSample {
                survived_ms: 0.0,
                closed: false,
                error: Some(error.clone()),
            },
            LifetimeSample {
                survived_ms: 0.0,
                closed: false,
                error: Some(error),
            },
        ]);
        assert_eq!(value.verdict, LifetimeVerdict::Unreachable);
        assert!(value.lifespan_ms.is_none());
    }

    #[test]
    fn proxy_addr_accepts_bare_and_scheme_forms() {
        assert!(proxy_addr("127.0.0.1:7890").is_some());
        assert!(proxy_addr("http://127.0.0.1:7890").is_some());
        assert_eq!(
            proxy_addr("127.0.0.1:7890").map(|a| a.port()),
            proxy_addr("http://127.0.0.1:7890").map(|a| a.port())
        );
    }

    #[test]
    fn json_shape_is_flat_and_stable() {
        let value = to_json(&report(vec![sample(3005.0, true), sample(3006.0, true)]));
        assert_eq!(value["verdict"], "cap_observed");
        assert_eq!(value["repeatable"], true);
        assert_eq!(value["represents"], "proxy_link");
        assert!(value["lifespan_ms"].is_number());
        assert_eq!(value["samples"].as_array().map(|a| a.len()), Some(2));
    }
}
