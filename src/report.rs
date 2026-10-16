//! One output boundary for business evidence and safe terminal/pipe presentation.
use crate::diagnostics::{AppError, ErrorCode, Stage};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

static JSON: AtomicBool = AtomicBool::new(false);
static CLOSED: AtomicBool = AtomicBool::new(false);
static OUTPUT_FAILED: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicBool = AtomicBool::new(false);
static DATA: Mutex<Value> = Mutex::new(Value::Null);
static ERROR: Mutex<Option<AppError>> = Mutex::new(None);
static WARNINGS: Mutex<Vec<AppError>> = Mutex::new(Vec::new());

macro_rules! println {
    () => { crate::report::text(format_args!("\n")) };
    ($($arg:tt)*) => { crate::report::text(format_args!("{}\n", format_args!($($arg)*))) };
}
macro_rules! print {
    ($($arg:tt)*) => { crate::report::text(format_args!($($arg)*)) };
}

pub fn initialize(json: bool) {
    JSON.store(json, Ordering::Release);
    if json {
        colored::control::set_override(false);
    }
}
pub fn machine() -> bool {
    JSON.load(Ordering::Acquire)
}
pub fn closed() -> bool {
    CLOSED.load(Ordering::Acquire)
}
fn write(bytes: &[u8]) -> bool {
    if closed() {
        return false;
    }
    let result = io::stdout()
        .lock()
        .write_all(bytes)
        .and_then(|_| io::stdout().flush());
    if let Err(error) = result {
        if error.kind() != io::ErrorKind::BrokenPipe
            && !matches!(error.raw_os_error(), Some(109 | 232 | 233))
        {
            OUTPUT_FAILED.store(true, Ordering::Release);
        }
        CLOSED.store(true, Ordering::Release);
        return false;
    }
    true
}
pub fn text(arguments: std::fmt::Arguments<'_>) {
    if !machine() {
        write(arguments.to_string().as_bytes());
    }
}
pub fn data(value: impl Serialize) {
    *DATA.lock().unwrap() = serde_json::to_value(value).unwrap_or(Value::Null);
}
pub fn error(value: &AppError) {
    *ERROR.lock().unwrap() = Some(value.clone());
}
pub fn warning(value: &AppError) {
    WARNINGS.lock().unwrap().push(value.clone());
}
pub fn finish(command: &str, code: i32) -> i32 {
    if command == "watch" && EVENTS.load(Ordering::Acquire) {
        return output_exit(code);
    }
    if machine() {
        let error = if code == 0 {
            None
        } else if code == 130 {
            Some(AppError::new(ErrorCode::Cancelled, Stage::State))
        } else {
            ERROR.lock().unwrap().clone().or_else(|| {
                Some(AppError::new(
                    if code == 130 {
                        ErrorCode::Cancelled
                    } else if code == 2 {
                        ErrorCode::ProbeFailed
                    } else {
                        ErrorCode::ClientUnavailable
                    },
                    if code == 2 {
                        Stage::Verification
                    } else {
                        Stage::Configuration
                    },
                ))
            })
        };
        let result = CommandReport {
            schema_version: 1,
            command,
            success: code == 0,
            data: DATA.lock().unwrap().clone(),
            warnings: WARNINGS.lock().unwrap().clone(),
            error,
            cancelled: code == 130,
        };
        if command == "watch" {
            let mut writer = EventWriter::new();
            writer.value("result", serde_json::to_value(result).expect("report"));
            writer.value("stopped", json!({"exit_code":code,"cancelled":code==130}));
        } else {
            let mut bytes = serde_json::to_vec(&result).expect("finite report");
            bytes.push(b'\n');
            write(&bytes);
        }
    }
    output_exit(code)
}
fn output_exit(code: i32) -> i32 {
    if OUTPUT_FAILED.load(Ordering::Acquire) {
        1
    } else if closed() {
        0
    } else {
        code
    }
}
#[derive(Serialize)]
pub struct CommandReport<'a> {
    schema_version: u32,
    command: &'a str,
    success: bool,
    data: Value,
    warnings: Vec<AppError>,
    error: Option<AppError>,
    cancelled: bool,
}
pub struct EventWriter {
    run_id: String,
    sequence: u64,
}
impl EventWriter {
    pub fn new() -> Self {
        Self {
            run_id: format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            sequence: 0,
        }
    }
    pub fn emit(&mut self, event: crate::watch::Event) -> bool {
        if !machine() {
            crate::watch::render_text(event);
            return !closed();
        }
        let event = serde_json::to_value(event).expect("watch event");
        self.value(
            event["type"].as_str().unwrap_or("unknown"),
            event["payload"].clone(),
        )
    }
    fn value(&mut self, kind: &str, payload: Value) -> bool {
        EVENTS.store(true, Ordering::Release);
        self.sequence += 1;
        let value = json!({"schema_version":1,"run_id":self.run_id,"sequence":self.sequence,"time":crate::history::now(),"type":kind,"payload":payload});
        let mut bytes = serde_json::to_vec(&value).expect("event JSON");
        bytes.push(b'\n');
        write(&bytes)
    }
}
