use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use colored::Colorize;

const SPIN_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 动态进度指示器：终端里原地刷新（旋转符 + 当前步骤 + 已耗时），
/// 输出被重定向/管道时自动禁用，保持日志干净。Drop 时自动清屏收尾。
pub struct Spinner {
    done: Arc<AtomicBool>,
    text: Arc<Mutex<String>>,
    handle: Option<thread::JoinHandle<()>>,
    enabled: bool,
}

impl Spinner {
    pub fn start(msg: &str) -> Spinner {
        // TTY 下启用；设 PROXPILOT_SPINNER=1 可强制开启（特殊终端/调试用）
        let enabled = io::stdout().is_terminal()
            || std::env::var("PROXPILOT_SPINNER").map(|v| v == "1").unwrap_or(false);
        let mut s = Spinner {
            done: Arc::new(AtomicBool::new(false)),
            text: Arc::new(Mutex::new(msg.to_string())),
            handle: None,
            enabled,
        };
        if !enabled {
            return s;
        }
        let done = s.done.clone();
        let text = s.text.clone();
        s.handle = Some(thread::spawn(move || {
            let start = Instant::now();
            let mut out = io::stdout();
            let mut i = 0usize;
            while !done.load(Ordering::Relaxed) {
                let frame = SPIN_FRAMES[i % SPIN_FRAMES.len()];
                let _ = write!(
                    out,
                    "\r\x1b[K  {} {} ({:.0}s)",
                    frame.cyan(),
                    text.lock().unwrap(),
                    start.elapsed().as_secs_f64()
                );
                let _ = out.flush();
                i += 1;
                thread::sleep(Duration::from_millis(100));
            }
            let _ = write!(out, "\r\x1b[K");
            let _ = out.flush();
        }));
        s
    }

    pub fn set_text(&self, msg: impl AsRef<str>) {
        if self.enabled {
            *self.text.lock().unwrap() = msg.as_ref().to_string();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if self.enabled {
            self.done.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

pub fn banner() {
    println!("{}", format!("ProxPilot · 代理领航员 v{}", env!("CARGO_PKG_VERSION")).cyan().bold());
    println!("{}", "节点检测 · 智能优选 · 自动切换 · 全时守护（Clash/mihomo 系）".dimmed());
    println!();
}

pub fn ok(msg: &str) {
    println!("  {}", format!("✔ {}", msg).green());
}

pub fn fail(msg: &str) {
    println!("  {}", format!("✘ {}", msg).red());
}

pub fn warn(msg: &str) {
    println!("  {}", format!("▲ {}", msg).yellow());
}

pub fn info(msg: &str) {
    println!("  {}", msg.cyan());
}

pub fn dim(msg: &str) {
    println!("  {}", msg.dimmed());
}
