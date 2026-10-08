use crate::detect::Backend;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use std::{fmt, path::PathBuf, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiEndpoint {
    Http(String),
    NamedPipe(PathBuf),
}

impl From<String> for ApiEndpoint {
    fn from(value: String) -> Self {
        Self::Http(value)
    }
}

impl fmt::Display for ApiEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(url) => f.write_str(url),
            Self::NamedPipe(path) => write!(f, "命名管道 {}", path.display()),
        }
    }
}

pub struct CoreResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct CoreApi {
    http: reqwest::blocking::Client,
}

impl CoreApi {
    pub fn new() -> Result<Self, String> {
        crate::http::api_client()
            .map(|http| Self { http })
            .map_err(|e| e.to_string())
    }

    pub fn request(
        &self,
        be: &Backend,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<CoreResponse, String> {
        let response = match &be.api {
            ApiEndpoint::Http(base) => {
                crate::clients::validate_endpoint_identity(be.kind, base)?;
                let mut request = self
                    .http
                    .request(method, format!("{}{}", base.trim_end_matches('/'), path))
                    .timeout(timeout);
                if let Some(secret) = &be.secret {
                    request = request.bearer_auth(secret);
                }
                if let Some(body) = body {
                    request = request
                        .header("Content-Type", "application/json")
                        .body(body);
                }
                let response = request
                    .send()
                    .map_err(|e| format!("API 请求失败：{}", e.without_url()))?;
                let status = response.status().as_u16();
                // 错误响应无需读取可能含私密诊断信息的响应体。
                if !(200..300).contains(&status) {
                    return Err(format!("API 请求失败：HTTP {}", status));
                }
                let body = response
                    .bytes()
                    .map_err(|e| format!("API 响应传输失败：{}", e.without_url()))?
                    .to_vec();
                CoreResponse { status, body }
            }
            ApiEndpoint::NamedPipe(pipe) => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async {
                    tokio::time::timeout(timeout, async {
                        let stream = loop {
                            match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe) {
                                Ok(stream) => break stream,
                                Err(e) if e.raw_os_error() == Some(231) => {
                                    tokio::time::sleep(Duration::from_millis(10)).await
                                }
                                Err(e) => return Err(format!("连接内核命名管道失败：{}", e)),
                            }
                        };
                        if let Some(kind) = be.kind {
                            use std::os::windows::io::AsRawHandle;
                            let mut pid = 0;
                            let success = unsafe {
                                windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
                                    stream.as_raw_handle(),
                                    &mut pid,
                                )
                            };
                            if success == 0
                                || crate::procinfo::identify_pid(Some(pid)).name
                                    != kind.display_name()
                            {
                                return Err("命名管道服务端身份不匹配或无法确认".into());
                            }
                        }
                        let (mut sender, connection) =
                            hyper::client::conn::http1::handshake(TokioIo::new(stream))
                                .await
                                .map_err(|e| format!("管道 HTTP 握手失败：{}", e))?;
                        let connection = tokio::spawn(connection);
                        let mut builder = hyper::Request::builder()
                            .method(method.as_str())
                            .uri(path)
                            .header("Host", "localhost")
                            .header("Connection", "close");
                        if let Some(secret) = &be.secret {
                            builder = builder.header("Authorization", format!("Bearer {}", secret));
                        }
                        if body.is_some() {
                            builder = builder.header("Content-Type", "application/json");
                        }
                        let request = builder
                            .body(Full::new(Bytes::from(body.unwrap_or_default())))
                            .map_err(|_| "API 请求参数或鉴权头无效".to_string())?;
                        let result = async {
                            let response = sender
                                .send_request(request)
                                .await
                                .map_err(|e| format!("管道 API 请求失败：{}", e))?;
                            let status = response.status().as_u16();
                            if !(200..300).contains(&status) {
                                return Err(format!("API 请求失败：HTTP {}", status));
                            }
                            let body = response
                                .into_body()
                                .collect()
                                .await
                                .map_err(|e| format!("管道响应传输失败：{}", e))?
                                .to_bytes()
                                .to_vec();
                            Ok(CoreResponse { status, body })
                        }
                        .await;
                        connection.abort();
                        result
                    })
                    .await
                    .map_err(|_| "内核 API 操作超时".to_string())?
                })?
            }
        };
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Backend;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn backend(api: ApiEndpoint) -> Backend {
        Backend {
            kind: None,
            client: "test".into(),
            api,
            secret: Some("private-test-secret".into()),
            proxy: String::new(),
            source: "test".into(),
            version: None,
            alternatives: vec![],
        }
    }

    pub fn pipe_server(
        response: Vec<u8>,
        pause: Duration,
    ) -> (ApiEndpoint, std::thread::JoinHandle<String>) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::path::PathBuf::from(format!(
            r"\\.\pipe\proxpilot-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let copy = path.clone();
        let (ready, wait) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut pipe = tokio::net::windows::named_pipe::ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(&copy)
                    .unwrap();
                ready.send(()).unwrap();
                pipe.connect().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = pipe.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..n]);
                }
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let length = String::from_utf8_lossy(&request[..end])
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while request.len() < end + 4 + length {
                        let n = pipe.read(&mut buffer).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..n]);
                    }
                }
                tokio::time::sleep(pause).await;
                let _ = pipe.write_all(&response).await;
                String::from_utf8(request).unwrap()
            })
        });
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        (ApiEndpoint::NamedPipe(path), handle)
    }

    #[test]
    fn named_pipe_handles_chunked_authenticated_response() {
        let first = "{\"version\":";
        let second = "\"test\"}";
        let response = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n", first.len(), first, second.len(), second);
        let (endpoint, server) = pipe_server(response.into_bytes(), Duration::ZERO);
        let result = CoreApi::new()
            .unwrap()
            .request(
                &backend(endpoint),
                reqwest::Method::GET,
                "/version",
                None,
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(result.body, br#"{"version":"test"}"#);
        assert!(server
            .join()
            .unwrap()
            .to_lowercase()
            .contains("authorization: bearer private-test-secret"));
    }

    #[test]
    fn named_pipe_switch_preserves_unicode_body_and_encodes_group_segment() {
        let (endpoint, server) =
            pipe_server(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(), Duration::ZERO);
        crate::mihomo::switch_group(
            &backend(endpoint),
            &CoreApi::new().unwrap(),
            "组/🛰",
            "日本 节点🛰",
        )
        .unwrap();
        let request = server.join().unwrap();
        assert!(request.starts_with(&format!(
            "PUT /proxies/{} HTTP/1.1",
            crate::mihomo::enc_path("组/🛰")
        )));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap()["name"],
            "日本 节点🛰"
        );
        assert!(crate::mihomo::enc_path("组/🛰").contains("%2F"));
    }

    #[test]
    fn named_pipe_reports_truncation_timeout_and_http_error() {
        for (response, pause, succeeds) in [
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec(),
                Duration::ZERO,
                false,
            ),
            (
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec(),
                Duration::ZERO,
                false,
            ),
            (
                b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
                Duration::ZERO,
                true,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
                Duration::from_millis(250),
                false,
            ),
        ] {
            let (endpoint, server) = pipe_server(response, pause);
            let result = CoreApi::new().unwrap().request(
                &backend(endpoint),
                reqwest::Method::GET,
                "/test",
                None,
                Duration::from_millis(100),
            );
            assert_eq!(result.is_ok(), succeeds);
            if let Err(error) = result {
                assert!(!error.contains("private-test-secret"));
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn named_pipe_requests_remain_concurrent() {
        let api = CoreApi::new().unwrap();
        let first = pipe_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
            Duration::from_millis(250),
        );
        let second = pipe_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
            Duration::ZERO,
        );
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                api.request(
                    &backend(first.0),
                    reqwest::Method::GET,
                    "/test",
                    None,
                    Duration::from_secs(2),
                )
                .unwrap();
            });
            scope.spawn(|| {
                api.request(
                    &backend(second.0),
                    reqwest::Method::GET,
                    "/test",
                    None,
                    Duration::from_secs(2),
                )
                .unwrap();
                sent.send(()).unwrap();
            });
            received
                .recv_timeout(Duration::from_millis(180))
                .expect("fast pipe request serialized behind slow request");
        });
        first.1.join().unwrap();
        second.1.join().unwrap();
    }
}
