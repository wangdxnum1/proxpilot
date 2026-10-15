//! Cooked-line input; no raw/alternate-screen mode. Capture and restore console settings.
use crate::{
    diagnostics::{AppError, AppResult, ErrorCode, Stage},
    operation::Cancellation,
};
use std::sync::mpsc::{self, Receiver};
pub struct Input(Receiver<Option<String>>);
impl Input {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || loop {
            let mut line = String::new();
            let result = std::io::stdin().read_line(&mut line);
            if !matches!(result,Ok(n) if n>0) {
                let _ = tx.send(None);
                break;
            }
            if tx
                .send(Some(line.trim_end_matches(['\r', '\n']).to_string()))
                .is_err()
            {
                break;
            }
        });
        Self(rx)
    }
    pub fn next(&self) -> AppResult<Option<String>> {
        loop {
            Cancellation::global().check()?;
            match self.0.recv_timeout(std::time::Duration::from_millis(25)) {
                Ok(line) => return Ok(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}
pub fn literal(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
pub struct ConsoleGuard {
    input_cp: u32,
    output_cp: u32,
    modes: Vec<(usize, u32)>,
}
impl ConsoleGuard {
    pub fn capture() -> Self {
        use windows_sys::Win32::System::Console::*;
        let mut modes = Vec::new();
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE] {
            let handle = unsafe { GetStdHandle(id) };
            let mut mode = 0;
            if unsafe { GetConsoleMode(handle, &mut mode) } != 0 {
                modes.push((handle as usize, mode));
            }
        }
        Self {
            input_cp: unsafe { GetConsoleCP() },
            output_cp: unsafe { GetConsoleOutputCP() },
            modes,
        }
    }
    pub fn restore(&self) {
        use windows_sys::Win32::System::Console::*;
        unsafe {
            if self.input_cp != 0 {
                SetConsoleCP(self.input_cp);
            }
            if self.output_cp != 0 {
                SetConsoleOutputCP(self.output_cp);
            }
            for (handle, mode) in &self.modes {
                SetConsoleMode(*handle as _, *mode);
            }
        }
    }
}
pub fn invalid() -> AppError {
    AppError::new(ErrorCode::InvalidInput, Stage::Configuration)
}
