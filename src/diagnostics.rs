//! Stable evidence for text, JSON and diagnostics. Raw dependency errors may contain URLs/secrets.
use serde::{Deserialize, Serialize};
use std::fmt;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Configuration,
    Discovery,
    Controller,
    Probe,
    Verification,
    Switching,
    Rollback,
    State,
    Build,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    ClientUnavailable,
    IdentityMismatch,
    ControllerAuth,
    ControllerHttp,
    ControllerTimeout,
    ProxyConnect,
    Connect,
    Timeout,
    Dns,
    Tls,
    ResponseTransfer,
    InvalidResponse,
    TransportUnknown,
    WebsiteHttp,
    ProbeFailed,
    Cancelled,
    Busy,
    SelectionConflict,
    ProfileChanged,
    RollbackFailed,
    StateUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub stage: Stage,
    pub http_status: Option<u16>,
    pub retryable: bool,
    pub message: String,
}

impl AppError {
    pub fn new(code: ErrorCode, stage: Stage) -> Self {
        use ErrorCode::*;
        let (message, retryable) = match code {
            InvalidInput => ("参数、URL 或请求配置无效", false),
            ClientUnavailable => ("目标客户端或控制接口不可用", true),
            IdentityMismatch => ("控制接口身份不匹配或无法确认", false),
            ControllerAuth => ("控制接口鉴权失败，请检查 secret", false),
            ControllerHttp => ("控制接口返回错误状态", false),
            ControllerTimeout => ("内核 API 操作超时", true),
            ProxyConnect => ("连接指定代理或建立代理隧道失败", true),
            Connect => ("连接目标失败", true),
            Timeout => ("连接或请求超过时间限制", true),
            Dns => ("DNS 解析失败", true),
            Tls => ("TLS 握手或证书验证失败", false),
            ResponseTransfer => ("响应传输中断或响应体不完整", true),
            InvalidResponse => ("控制接口响应格式无效", false),
            TransportUnknown => ("传输失败，现有证据不足以确认具体原因", true),
            WebsiteHttp => ("测试网站返回非成功状态", false),
            ProbeFailed => ("节点探测未返回有效延迟", true),
            Cancelled => ("操作已取消", false),
            Busy => ("另一个 ProxPilot 修改操作正在运行", true),
            SelectionConflict => ("当前选择已被外部修改，保留新的选择", false),
            ProfileChanged => ("当前订阅或策略组已改变，停止本次操作", false),
            RollbackFailed => ("恢复原选择失败，保留恢复记录", true),
            StateUnavailable => ("本地状态无法安全读取或写入", false),
        };
        Self {
            code,
            stage,
            http_status: None,
            retryable,
            message: message.into(),
        }
    }

    pub fn controller_status(status: u16) -> Self {
        let mut error = Self::new(
            if matches!(status, 401 | 403) {
                ErrorCode::ControllerAuth
            } else {
                ErrorCode::ControllerHttp
            },
            Stage::Controller,
        );
        error.http_status = Some(status);
        error.retryable = status == 429 || status >= 500;
        error
    }

    pub fn website_status(status: u16) -> Self {
        let mut error = Self::new(ErrorCode::WebsiteHttp, Stage::Verification);
        error.http_status = Some(status);
        error.retryable = status == 429 || status >= 500;
        error
    }

    pub fn from_wreq(error: &wreq::Error, stage: Stage) -> Self {
        use ErrorCode::*;
        let code = if error.is_timeout() {
            Timeout
        } else if error.is_dns() {
            Dns
        } else if error.is_tls() || has_tls_cause(error) {
            Tls
        } else if error.is_proxy_connect() {
            ProxyConnect
        } else if error.is_connect() {
            Connect
        } else if error.is_body() || error.is_decode() {
            ResponseTransfer
        } else if error.is_builder() {
            InvalidInput
        } else {
            TransportUnknown
        };
        Self::new(code, stage)
    }

    pub fn from_reqwest(error: &reqwest::Error, stage: Stage) -> Self {
        use std::error::Error;
        let mut cause = error.source();
        let mut tls = false;
        while let Some(current) = cause {
            if current.is::<rustls::Error>() {
                tls = true;
                break;
            }
            cause = current.source();
        }
        let code = if error.is_timeout() {
            if stage == Stage::Controller {
                ErrorCode::ControllerTimeout
            } else {
                ErrorCode::Timeout
            }
        } else if tls {
            ErrorCode::Tls
        } else if error.is_connect() {
            ErrorCode::Connect
        } else if error.is_body() || error.is_decode() {
            ErrorCode::ResponseTransfer
        } else if error.is_builder() {
            ErrorCode::InvalidInput
        } else {
            ErrorCode::TransportUnknown
        };
        Self::new(code, stage)
    }
}

fn has_tls_cause(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(error);
    for _ in 0..32 {
        let Some(current) = cause else {
            break;
        };
        if current.is::<btls::ssl::Error>()
            || current.is::<btls::error::ErrorStack>()
            || current.is::<rustls::Error>()
        {
            return true;
        }
        cause = current.source();
    }
    false
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(status) = self.http_status {
            write!(f, "（HTTP {status}）")?;
        }
        Ok(())
    }
}
impl std::error::Error for AppError {}
impl From<AppError> for String {
    fn from(error: AppError) -> Self {
        error.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Webpage,
    IpOnly,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationReport {
    pub availability: Availability,
    pub mean_ms: Option<f64>,
    pub samples_requested: usize,
    pub samples_passed: usize,
    pub error: Option<AppError>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeFailure {
    pub node: String,
    pub phase: Stage,
    pub error: AppError,
}
