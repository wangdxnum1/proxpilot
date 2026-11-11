//! 内核 API 直连；网站实测明确走指定代理，每次请求使用全新的连接。

use std::cell::Cell;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use crate::diagnostics::{AppError, AppResult, ErrorCode, Stage, StreamOptions, StreamSample};
use http_body_util::BodyExt;
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::redirect;
use tokio::runtime::Runtime;
use wreq::cookie::Jar;
use wreq_util::{Emulation, Platform, Profile};

fn client_builder() -> ClientBuilder {
    static CRYPTO: Once = Once::new();
    CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    Client::builder().tls_backend_rustls()
}

pub fn api_client() -> Result<Client, reqwest::Error> {
    client_builder()
        .no_proxy()
        .redirect(redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    pub fn listener() -> TcpListener {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }

    pub fn accept(listener: &TcpListener) -> TcpStream {
        let start = Instant::now();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    return stream;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        start.elapsed() < Duration::from_secs(10),
                        "request did not arrive"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        }
    }

    pub fn read_request(stream: &mut TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            request.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        request
    }

    pub fn serve(responses: Vec<Vec<u8>>) -> (String, JoinHandle<Vec<String>>) {
        let listener = listener();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            responses
                .into_iter()
                .map(|response| {
                    let mut stream = accept(&listener);
                    let request = read_request(&mut stream);
                    stream.write_all(&response).unwrap();
                    request
                })
                .collect()
        });
        (url, handle)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::io::{Read, Write};
    use std::thread;

    #[test]
    fn proxy_receives_browser_headers_and_target_url() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno".to_vec(),
        ]);
        let result = BrowserProbe::new(&proxy)
            .unwrap()
            .test_detailed("http://browser-test.invalid/page")
            .unwrap();
        assert_eq!(result.0, 403);
        let request = server.join().unwrap().remove(0).to_lowercase();
        assert!(request.starts_with("get http://browser-test.invalid/page http/1.1\r\n"));
        for header in [
            "user-agent: mozilla/5.0",
            "chrome/149.",
            "sec-ch-ua-platform: \"windows\"",
            "sec-fetch-mode: navigate",
            "accept-language: zh-cn",
            "accept-encoding:",
        ] {
            assert!(request.contains(header), "missing {header}");
        }
    }

    #[test]
    fn cloudflare_challenge_switches_profile_and_keeps_cookies_separate() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 403 Forbidden\r\ncf-mitigated: challenge\r\nSet-Cookie: chrome=first; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nSet-Cookie: safari=second; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ]);
        let probe = BrowserProbe::new(&proxy).unwrap();
        for _ in 0..2 {
            assert_eq!(
                probe
                    .test_detailed("http://browser-test.invalid/")
                    .unwrap()
                    .0,
                200
            );
        }
        let requests: Vec<_> = server
            .join()
            .unwrap()
            .into_iter()
            .map(|r| r.to_lowercase())
            .collect();
        assert!(requests[0].contains("chrome/149."));
        assert!(requests[1].contains("safari/"));
        assert!(!requests[1].contains("cookie:"));
        assert!(requests[2].contains("safari/"));
        assert!(requests[2].contains("cookie: safari=second"));
        assert!(!requests[2].contains("chrome=first"));
    }

    #[test]
    fn failed_alternative_profile_does_not_replace_preferred_profile() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 403 Forbidden\r\ncf-mitigated: challenge\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ]);
        let probe = BrowserProbe::new(&proxy).unwrap();
        assert_eq!(
            probe
                .test_detailed("http://browser-test.invalid/")
                .unwrap()
                .0,
            403
        );
        assert_eq!(
            probe
                .test_detailed("http://browser-test.invalid/")
                .unwrap()
                .0,
            200
        );
        let requests = server.join().unwrap();
        assert!(requests[2].to_lowercase().contains("chrome/149."));
    }

    #[test]
    fn redirects_share_cookies_within_one_verification() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: http://browser-test.invalid/final\r\nSet-Cookie: session=abc; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
        ]);
        assert_eq!(
            BrowserProbe::new(&proxy)
                .unwrap()
                .test_detailed("http://browser-test.invalid/start")
                .unwrap()
                .0,
            200
        );
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("GET http://browser-test.invalid/final "));
        assert!(requests[1].to_lowercase().contains("cookie: session=abc"));
    }

    #[test]
    fn incomplete_response_body_is_a_failure() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort".to_vec(),
        ]);
        let error = BrowserProbe::new(&proxy)
            .unwrap()
            .test_detailed("http://browser-test.invalid/")
            .unwrap_err();
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "response_transfer");
        assert!(value["http_status"].is_null());
        server.join().unwrap();
    }

    #[test]
    fn gzip_body_is_decoded_and_read_to_completion() {
        let mut response = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 25\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(&[
            31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 203, 72, 205, 201, 201, 7, 0, 134, 166, 16, 54, 5, 0,
            0, 0,
        ]);
        let (proxy, server) = serve(vec![response]);
        assert_eq!(
            BrowserProbe::new(&proxy)
                .unwrap()
                .test_detailed("http://browser-test.invalid/")
                .unwrap()
                .0,
            200
        );
        server.join().unwrap();
    }

    #[test]
    fn successive_samples_use_new_tcp_connections() {
        let listener = listener();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut first = accept(&listener);
            read_request(&mut first);
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            // 服务器允许 keep-alive，客户端仍须主动关闭旧隧道。
            assert_eq!(first.read(&mut [0u8; 1]).unwrap(), 0);
            let mut second = accept(&listener);
            read_request(&mut second);
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let probe = BrowserProbe::new(&proxy).unwrap();
        for _ in 0..2 {
            assert_eq!(
                probe
                    .test_detailed("http://browser-test.invalid/")
                    .unwrap()
                    .0,
                200
            );
        }
        server.join().unwrap();
    }

    #[test]
    fn api_client_does_not_follow_redirects() {
        let (url, server) = serve(vec![b"HTTP/1.1 302 Found\r\nLocation: http://unreachable.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()]);
        assert_eq!(api_client().unwrap().get(url).send().unwrap().status(), 302);
        server.join().unwrap();
    }

    #[test]
    fn https_uses_proxy_connect_and_rejects_a_failed_tunnel() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ]);
        let error = BrowserProbe::new(&proxy)
            .unwrap()
            .test_detailed("https://browser-test.invalid/")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProxyConnect);
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("CONNECT browser-test.invalid:443 HTTP/1.1\r\n"));
    }

    #[test]
    fn tls_handshake_error_keeps_category_without_secret_url() {
        let listener = listener();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let request = read_request(&mut stream);
            assert!(request.starts_with("CONNECT "));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut hello = [0u8; 2048];
            assert!(stream.read(&mut hello).unwrap() > 0);
            stream.write_all(b"not-a-TLS-record").unwrap();
        });
        let error = BrowserProbe::new(&proxy)
            .unwrap()
            .test_detailed("https://tls-test.invalid/path?token=secret-test-value")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Tls);
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains("secret-test-value"));
        assert!(!serialized.contains("tls-test.invalid"));
        server.join().unwrap();
    }

    #[test]
    fn cookie_state_does_not_leak_between_node_verifications() {
        let (proxy, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nSet-Cookie: session=first-node; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ]);
        for _ in 0..2 {
            assert_eq!(
                BrowserProbe::new(&proxy)
                    .unwrap()
                    .test_detailed("http://browser-test.invalid/")
                    .unwrap()
                    .0,
                200
            );
        }
        let requests = server.join().unwrap();
        assert!(!requests[1].to_lowercase().contains("cookie:"));
    }

    #[test]
    fn chrome_client_hello_contains_grease_and_http2_alpn() {
        let listener = listener();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            assert!(read_request(&mut stream).starts_with("CONNECT browser-test.invalid:443 "));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut header = [0u8; 5];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(header[0], 22); // TLS handshake record
            let mut hello = vec![0u8; u16::from_be_bytes([header[3], header[4]]) as usize];
            stream.read_exact(&mut hello).unwrap();
            hello
        });
        assert!(BrowserProbe::new(&proxy)
            .unwrap()
            .test_detailed("https://browser-test.invalid/")
            .is_err());
        let hello = server.join().unwrap();
        assert_eq!(hello[0], 1); // ClientHello
        let grease = |v: u16| v & 0x0f0f == 0x0a0a && v >> 8 == v & 0xff;
        let mut pos = 4 + 2 + 32;
        pos += 1 + hello[pos] as usize; // session id
        let cipher_len = u16::from_be_bytes([hello[pos], hello[pos + 1]]) as usize;
        pos += 2;
        assert!(hello[pos..pos + cipher_len]
            .as_chunks::<2>()
            .0
            .iter()
            .any(|c| grease(u16::from_be_bytes([c[0], c[1]]))));
        pos += cipher_len;
        pos += 1 + hello[pos] as usize; // compression methods
        let extension_len = u16::from_be_bytes([hello[pos], hello[pos + 1]]) as usize;
        pos += 2;
        let end = pos + extension_len;
        let mut saw_grease = false;
        let mut saw_h2 = false;
        while pos < end {
            let id = u16::from_be_bytes([hello[pos], hello[pos + 1]]);
            let len = u16::from_be_bytes([hello[pos + 2], hello[pos + 3]]) as usize;
            pos += 4;
            saw_grease |= grease(id);
            if id == 16 {
                saw_h2 = hello[pos..pos + len].windows(2).any(|v| v == b"h2");
            }
            pos += len;
        }
        assert!(saw_grease, "Chrome GREASE extension missing");
        assert!(saw_h2, "HTTP/2 ALPN missing");
    }

    /// 手工运行：cargo test live_profile_comparison -- --ignored --nocapture
    /// 默认走本机 7890；可用 PROXPILOT_TEST_PROXY 指定测试代理。
    #[test]
    #[ignore = "requires a running local proxy and live network"]
    fn live_profile_comparison() {
        let proxy = std::env::var("PROXPILOT_TEST_PROXY")
            .unwrap_or_else(|_| "http://127.0.0.1:7890".into());
        let probe = BrowserProbe::new(&proxy).unwrap();
        for (profile, platform) in [
            (Profile::Chrome149, Platform::Windows),
            (Profile::Chrome140, Platform::Windows),
            (Profile::Chrome131, Platform::Windows),
            (Profile::Chrome120, Platform::Windows),
            (Profile::Safari26, Platform::MacOS),
        ] {
            probe.runtime.block_on(async {
                let client = probe
                    .builder_for(profile, platform)
                    .cookie_provider(Arc::new(Jar::default()))
                    .build()
                    .unwrap();
                let result = client
                    .get("https://chatgpt.com/")
                    .header(wreq::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
                    .send()
                    .await;
                match result {
                    Ok(response) => {
                        response.forbid_recycle();
                        println!(
                            "{profile:?}: HTTP {}, {:?}, cf-mitigated={:?}",
                            response.status(),
                            response.version(),
                            response.headers().get("cf-mitigated")
                        );
                        // 不输出网页、Cookie 或个人数据；只完成传输验证。
                        let mut body = wreq::Body::from(response);
                        while let Some(frame) = body.frame().await {
                            frame.unwrap();
                        }
                    }
                    Err(e) => println!("{profile:?}: {e}"),
                }
            });
        }
    }
}

/// Cookie 只在同一次节点验证中共享，避免跨节点带入上一个节点的状态。
pub struct BrowserProbe {
    proxy: wreq::Proxy,
    cookies: Arc<Jar>,
    safari_cookies: Arc<Jar>,
    prefer_safari: Cell<bool>,
    runtime: Runtime,
    timeout: Duration,
}

impl BrowserProbe {
    #[cfg(test)]
    pub fn new(proxy: &str) -> AppResult<Self> {
        Self::new_with_timeout(proxy, 12000)
    }

    pub fn new_with_timeout(proxy: &str, timeout_ms: u32) -> AppResult<Self> {
        Ok(Self {
            timeout: Duration::from_millis(u64::from(timeout_ms)),
            proxy: wreq::Proxy::all(proxy)
                .map_err(|e| AppError::from_wreq(&e, Stage::Configuration))?,
            cookies: Arc::new(Jar::default()),
            safari_cookies: Arc::new(Jar::default()),
            prefer_safari: Cell::new(false),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| AppError::new(ErrorCode::StateUnavailable, Stage::Verification))?,
        })
    }

    fn builder(&self) -> wreq::ClientBuilder {
        self.builder_for(Profile::Chrome149, Platform::Windows)
    }

    fn builder_for(&self, profile: Profile, platform: Platform) -> wreq::ClientBuilder {
        // 同一配置生成 UA、Client Hints、TLS ClientHello 和 HTTP/2 设置。
        wreq::Client::builder()
            .emulation(
                Emulation::builder()
                    .profile(profile)
                    .platform(platform)
                    .build(),
            )
            .no_proxy()
            .proxy(self.proxy.clone())
            .cookie_provider(if platform == Platform::MacOS {
                self.safari_cookies.clone()
            } else {
                self.cookies.clone()
            })
            .redirect(wreq::redirect::Policy::limited(10))
            .timeout(self.timeout)
            .connect_timeout(self.timeout.min(Duration::from_secs(8)))
    }

    async fn request(&self, url: &str, safari: bool) -> AppResult<(u16, bool)> {
        // 每次重建 Client，HTTP/1 和 HTTP/2 均不会复用前一个节点的隧道。
        let builder = if safari {
            self.builder_for(Profile::Safari26, Platform::MacOS)
        } else {
            self.builder()
        };
        let client = builder
            .build()
            .map_err(|e| AppError::from_wreq(&e, Stage::Verification))?;
        let response = client
            .get(url)
            .header(wreq::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .map_err(|e| AppError::from_wreq(&e, Stage::Verification))?;
        response.forbid_recycle();
        let code = response.status().as_u16();
        let challenge = response
            .headers()
            .get("cf-mitigated")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("challenge"));
        let mut body = wreq::Body::from(response);
        while let Some(frame) = body.frame().await {
            frame.map_err(|_| AppError::new(ErrorCode::ResponseTransfer, Stage::Verification))?;
        }
        Ok((code, challenge))
    }

    /// 最多两个浏览器配置共享配置的整体超时；耗时包含候补请求。
    pub fn test_detailed(&self, url: &str) -> AppResult<(u16, f64)> {
        self.test_with_cancellation(url, &crate::operation::Cancellation::global())
    }

    pub fn test_with_cancellation(
        &self,
        url: &str,
        cancellation: &crate::operation::Cancellation,
    ) -> AppResult<(u16, f64)> {
        cancellation.check()?;
        self.runtime.block_on(async {
            let start = Instant::now();
            let request = tokio::time::timeout(self.timeout, async {
                let preferred = self.prefer_safari.get();
                let (code, challenge) = self.request(url, preferred).await?;
                // 普通 403 或断连不重试；只对服务端明确标记的 Challenge 使用候补。
                if code == 403 && challenge {
                    let (alternative, _) = self.request(url, !preferred).await?;
                    if alternative == 200 {
                        self.prefer_safari.set(!preferred);
                    }
                    Ok::<u16, AppError>(alternative)
                } else {
                    Ok(code)
                }
            });
            let code = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AppError::new(ErrorCode::Cancelled, Stage::Verification)),
                result = request => result.map_err(|_| AppError::new(ErrorCode::Timeout, Stage::Verification))??,
            };
            Ok((code, start.elapsed().as_secs_f64()))
        })
    }
}

// --- Git long-connection / large-transfer dimension -----------------------
//
// Kept separate from `BrowserProbe`: webpage verification uses a short
// 12s budget and one short request per sample, while a stream sample needs a
// bounded but much larger body read. Nothing here changes existing behavior.
//
// Concurrency model: all samples for ONE node run concurrently (the kernel's
// selected node is global state, so different nodes must stay serial). N
// independent connections are N Bernoulli trials of "is this connection cut
// at the start of the body", which is exactly the failure mode described in
// gitpilot/doc/long-connection-probe-20261111.md. Wall time is ~one sample,
// not N samples.

fn failed_sample(options: StreamOptions, code: ErrorCode) -> StreamSample {
    StreamSample {
        completed: false,
        received_bytes: 0,
        expected_bytes: options.bytes.max(1),
        first_byte_ms: None,
        elapsed_ms: 0.0,
        throughput_kbps: 0.0,
        error: Some(AppError::new(code, Stage::Stream)),
    }
}

async fn stream_once(proxy: wreq::Proxy, url: String, options: StreamOptions) -> StreamSample {
    let start = Instant::now();
    let overall = Duration::from_millis(u64::from(options.timeout_ms.max(1)));
    let first_byte_budget = Duration::from_millis(u64::from(options.first_byte_timeout_ms.max(1)));
    let mut sample = StreamSample {
        completed: false,
        received_bytes: 0,
        expected_bytes: options.bytes.max(1),
        first_byte_ms: None,
        elapsed_ms: 0.0,
        throughput_kbps: 0.0,
        error: None,
    };
    let work = async {
        let client = wreq::Client::builder()
            .emulation(
                Emulation::builder()
                    .profile(Profile::Chrome149)
                    .platform(Platform::Windows)
                    .build(),
            )
            .no_proxy()
            .proxy(proxy)
            .redirect(wreq::redirect::Policy::limited(10))
            .timeout(overall)
            .connect_timeout(overall.min(Duration::from_secs(8)))
            .build()
            .map_err(|e| AppError::from_wreq(&e, Stage::Stream))?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::from_wreq(&e, Stage::Stream))?;
        // Never read more than --git-bytes. A declared Content-Length is
        // trusted only down to that cap; a smaller object is read in full.
        let declared = response
            .headers()
            .get(wreq::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let target = declared.unwrap_or(options.bytes).min(options.bytes).max(1);
        sample.expected_bytes = target;
        let mut body = wreq::Body::from(response);
        let mut got_first = false;
        loop {
            let frame = if got_first {
                body.frame().await
            } else {
                // Headers were already received; this budget isolates the
                // "200 OK then no pack stream" failure mode.
                match tokio::time::timeout(first_byte_budget, body.frame()).await {
                    Ok(frame) => frame,
                    Err(_) => {
                        sample.error = Some(AppError::new(ErrorCode::Timeout, Stage::Stream));
                        break;
                    }
                }
            };
            let Some(frame) = frame else { break };
            let frame = match frame {
                Ok(frame) => frame,
                Err(_) => {
                    sample.error = Some(AppError::new(ErrorCode::ResponseTransfer, Stage::Stream));
                    break;
                }
            };
            if let Some(data) = frame.data_ref() {
                if !got_first {
                    got_first = true;
                    sample.first_byte_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                }
                sample.received_bytes = sample.received_bytes.saturating_add(data.len() as u64);
                if sample.received_bytes >= target {
                    break;
                }
            }
        }
        sample.completed = sample.error.is_none() && sample.received_bytes >= target;
        if !sample.completed && sample.error.is_none() {
            // Response headers arrived, body ended early: early EOF, not HTTP 0.
            sample.error = Some(AppError::new(ErrorCode::ResponseTransfer, Stage::Stream));
        }
        Ok::<(), AppError>(())
    };
    match tokio::time::timeout(overall, work).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => sample.error = Some(error),
        Err(_) => {
            sample.completed = false;
            sample.error = Some(AppError::new(ErrorCode::Timeout, Stage::Stream));
        }
    }
    sample.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    sample.throughput_kbps = if sample.elapsed_ms > 0.0 {
        sample.received_bytes as f64 / (sample.elapsed_ms / 1000.0) / 1024.0
    } else {
        0.0
    };
    sample
}

pub struct StreamProbe {
    proxy: wreq::Proxy,
    runtime: Runtime,
}

impl StreamProbe {
    pub fn new(proxy: &str) -> AppResult<Self> {
        Ok(Self {
            proxy: wreq::Proxy::all(proxy)
                .map_err(|e| AppError::from_wreq(&e, Stage::Configuration))?,
            // Single-threaded runtime is enough: stream samples are IO-bound
            // and `JoinSet` overlaps them on the reactor. No rt-multi-thread
            // feature (and no extra OS threads) required.
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| AppError::new(ErrorCode::StateUnavailable, Stage::Stream))?,
        })
    }

    /// Same as `test_stream`, reporting `(completed, total)` after each sample
    /// finishes so the UI can show live progress.
    pub fn test_stream_with_progress(
        &self,
        url: &str,
        options: StreamOptions,
        cancellation: &crate::operation::Cancellation,
        on_progress: impl Fn(usize, usize) + Send + Sync,
    ) -> AppResult<Vec<StreamSample>> {
        cancellation.check()?;
        self.runtime.block_on(async {
            let total = options.samples.max(1);
            let mut set = tokio::task::JoinSet::new();
            for _ in 0..total {
                set.spawn(stream_once(self.proxy.clone(), url.to_string(), options));
            }
            let mut samples = Vec::new();
            loop {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        set.abort_all();
                        return Err(AppError::new(ErrorCode::Cancelled, Stage::Stream));
                    }
                    joined = set.join_next() => {
                        let Some(result) = joined else { break };
                        samples.push(match result {
                            Ok(sample) => sample,
                            Err(_) => failed_sample(options, ErrorCode::TransportUnknown),
                        });
                        on_progress(samples.len(), total);
                    }
                }
            }
            Ok(samples)
        })
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::http::test_support::{accept, listener, read_request};
    use std::io::Write;

    fn options(samples: usize, bytes: u64) -> StreamOptions {
        StreamOptions {
            bytes,
            samples,
            timeout_ms: 4000,
            min_throughput_kbps: 1,
            first_byte_timeout_ms: 2000,
        }
    }

    /// Minimal HTTP proxy: reply with a body of `body_len` bytes, or cut the
    /// connection right after the headers when `cut` is set (the exact failure
    /// mode in gitpilot/doc/long-connection-probe-20261111.md).
    fn proxy_server(
        connections: usize,
        body_len: usize,
        cut: bool,
    ) -> (String, std::thread::JoinHandle<usize>) {
        let listener = listener();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut served = 0;
            for _ in 0..connections {
                let mut stream = accept(&listener);
                let _ = read_request(&mut stream);
                if cut {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        body_len
                    )
                    .unwrap();
                    stream.flush().unwrap();
                    drop(stream); // early EOF, no close_notify equivalent
                } else {
                    let body = vec![b'x'; body_len];
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                    stream.flush().unwrap();
                }
                served += 1;
            }
            served
        });
        (url, handle)
    }

    #[test]
    fn concurrent_samples_all_complete() {
        let (proxy, server) = proxy_server(5, 4096, false);
        let probe = StreamProbe::new(&proxy).unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let samples = probe
            .test_stream_with_progress(
                "http://stream.invalid/data",
                options(5, 4096),
                &crate::operation::Cancellation::new(),
                |done, total| seen.lock().unwrap().push((done, total)),
            )
            .unwrap();
        assert_eq!(samples.len(), 5);
        assert!(samples.iter().all(|s| s.completed));
        assert!(samples.iter().all(|s| s.received_bytes == 4096));
        assert_eq!(server.join().unwrap(), 5, "all 5 connections were served");
        // Progress must advance to completion so the UI is never silent.
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 5);
        assert_eq!(seen.last(), Some(&(5, 5)));
    }

    #[test]
    fn early_eof_is_detected_as_incomplete_not_healthy() {
        let (proxy, server) = proxy_server(5, 4096, true);
        let probe = StreamProbe::new(&proxy).unwrap();
        let samples = probe
            .test_stream_with_progress(
                "http://stream.invalid/data",
                options(5, 4096),
                &crate::operation::Cancellation::new(),
                |_, _| {},
            )
            .unwrap();
        assert_eq!(samples.len(), 5);
        assert!(samples
            .iter()
            .all(|s| !s.completed && s.received_bytes == 0));
        assert_eq!(server.join().unwrap(), 5);
    }

    #[test]
    fn cancellation_aborts_pending_samples() {
        let (proxy, server) = proxy_server(1, 4096, false);
        let probe = StreamProbe::new(&proxy).unwrap();
        let cancellation = crate::operation::Cancellation::new();
        cancellation.cancel();
        let result = probe.test_stream_with_progress(
            "http://stream.invalid/data",
            options(1, 4096),
            &cancellation,
            |_, _| {},
        );
        assert!(result.is_err());
        // Server thread may never accept; drop without joining to avoid a hang.
        drop(server);
    }
}
