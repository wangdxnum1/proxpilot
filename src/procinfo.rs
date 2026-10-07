//! 通过端口反查进程，识别客户端是谁。

use std::process::Command;

pub struct ClientInfo {
    pub name: String,
    #[allow(dead_code)]
    pub process: String,
    #[allow(dead_code)]
    pub path: String,
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, pid: u32) -> isize;
    fn QueryFullProcessImageNameW(
        process: isize,
        flags: u32,
        exe_name: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn CloseHandle(handle: isize) -> i32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// 找到监听指定端口的进程 PID
pub fn find_listener_pid(port: u16) -> Option<u32> {
    let out = Command::new("netstat").args(["-ano", "-p", "TCP"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let suffix = format!(":{}", port);
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() >= 5 && t[3].eq_ignore_ascii_case("LISTENING") && t[1].ends_with(&suffix) {
            if let Ok(pid) = t[4].parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn process_name(pid: u32) -> Option<String> {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?.trim().to_string();
    if !first.starts_with('"') {
        return None; // 没有匹配任务的提示信息
    }
    Some(first.trim_start_matches('"').split('"').next()?.to_string())
}

/// 识别监听该端口的是哪家客户端
pub fn identify_client(port: u16) -> ClientInfo {
    let pid = find_listener_pid(port);
    let (path, process) = match pid {
        Some(p) => (
            image_path(p).unwrap_or_default(),
            process_name(p).unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    };
    let name = client_display_name(&path, &process);
    ClientInfo { name, process, path }
}

pub fn client_display_name(path: &str, process: &str) -> String {
    let l = format!("{} {}", path, process).to_lowercase();
    if l.contains("cutecloud") {
        "CuteCloud".into()
    } else if l.contains("flclash") {
        "FlClash".into()
    } else if l.contains("verge") {
        "Clash Verge".into()
    } else if l.contains("clash for windows") || l.contains("clash-win64") {
        "Clash for Windows".into()
    } else if l.contains("mihomo") {
        "mihomo".into()
    } else if l.contains("clash") {
        "Clash 内核".into()
    } else if !process.is_empty() {
        process.into()
    } else {
        "未知客户端".into()
    }
}
