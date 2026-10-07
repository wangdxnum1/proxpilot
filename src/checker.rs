//! 节点探测、真实访问验证与优选切换。

use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use colored::Colorize;

use crate::detect::Backend;
use crate::mihomo::{self, ProxyInfo};
use crate::ui;
use crate::Args;

const CURL: &str = r"C:\Windows\System32\curl.exe";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// 机场放在节点列表里的假节点（套餐信息占位）
const FAKE_NODES: [&str; 6] = ["剩余流量", "套餐到期", "到期", "剩余", "官网", "流量"];
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
/// 最多深度验证多少个通过探测的节点
const MAX_VERIFY: usize = 5;

pub fn parse_addr(addr: &str) -> (String, u16) {
    let a = addr
        .strip_prefix("http://")
        .or_else(|| addr.strip_prefix("https://"))
        .unwrap_or(addr);
    let mut it = a.split(':');
    let host = it.next().unwrap_or("127.0.0.1").to_string();
    let port = it.next().and_then(|p| p.parse().ok()).unwrap_or(7890);
    (host, port)
}

pub fn core_alive(proxy: &str) -> bool {
    let (host, port) = parse_addr(proxy);
    match format!("{}:{}", host, port).to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(sa) => TcpStream::connect_timeout(&sa, Duration::from_secs(2)).is_ok(),
            None => false,
        },
        Err(_) => false,
    }
}

/// 用 curl（schannel TLS 指纹，贴近真实浏览器）经代理实测网址，返回 (HTTP 码, 耗时秒)
pub fn curl_test(proxy: &str, url: &str) -> (u16, f64) {
    let out = Command::new(CURL)
        .args([
            "-x", proxy, "-A", UA, "-s", "-o", "NUL", "-w", "%{http_code} %{time_total}",
            "--max-time", "12", url,
        ])
        .output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            let mut it = s.trim().split_whitespace();
            let code = it
                .next()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(0);
            let t = it
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(99.0);
            (code, t)
        }
        Err(_) => (0, 99.0),
    }
}

/// 连续 samples 次全部 200 才算通过，返回平均耗时
pub fn curl_avg(proxy: &str, url: &str, samples: usize) -> Result<f64, u16> {
    let mut total = 0.0;
    for _ in 0..samples {
        let (code, t) = curl_test(proxy, url);
        if code != 200 {
            return Err(code);
        }
        total += t;
    }
    Ok(total / samples.max(1) as f64)
}

pub fn is_real_node(info: &ProxyInfo, name: &str) -> bool {
    !SKIP_TYPES.contains(&info.ptype.as_str()) && !FAKE_NODES.iter().any(|f| name.contains(f))
}

/// 并发（8 路）探测节点可达性，返回 (探测延迟, 节点名) 升序
pub fn scan_reachable(
    be: &Backend,
    agent: &ureq::Agent,
    members: &[String],
    url: &str,
) -> Vec<(i64, String)> {
    let reachable: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());
    thread::scope(|s| {
        for chunk in members.chunks(8) {
            let reachable = &reachable;
            s.spawn(move || {
                for name in chunk {
                    if let Some(d) = mihomo::probe_delay(be, agent, name, url, 4000) {
                        reachable.lock().unwrap().push((d, name.clone()));
                    }
                }
            });
        }
    });
    let mut v = reachable.into_inner().unwrap();
    v.sort();
    v
}

/// 完整优选：探测 → 逐个切换实测 → 停在实测最快的节点。全部失败则恢复原节点。
pub fn fix_flow(be: &Backend, agent: &ureq::Agent, args: &Args) -> Result<String, String> {
    let proxies = mihomo::get_proxies(be, agent)?;
    let group = proxies
        .get(&args.group)
        .ok_or_else(|| format!("找不到组「{}」", args.group))?;
    let orig = group.now.clone().unwrap_or_else(|| "未知".into());
    ui::info(&format!("当前组「{}」→ {}", args.group, orig));

    let mut members: Vec<String> = Vec::new();
    for name in group.all.clone().unwrap_or_default() {
        if let Some(info) = proxies.get(&name) {
            if is_real_node(info, &name) {
                members.push(name);
            }
        }
    }
    let total = members.len();
    // 按最近一次测速延迟排序，快的先测
    members.sort_by_key(|n: &String| {
        let d = proxies
            .get(n)
            .and_then(|p| p.history.last())
            .map(|h| h.delay)
            .unwrap_or(0);
        if d > 0 {
            d
        } else {
            90_000
        }
    });
    ui::info(&format!(
        "组内真实节点 {} 个，8 路并发探测可达性...",
        total
    ));

    let reachable = scan_reachable(be, agent, &members, &args.url);
    ui::info(&format!("探测完成：可达 {}/{}", reachable.len(), total));
    if reachable.is_empty() {
        return Err("所有节点都无法访问测试网址".into());
    }

    let verify_n = args.top.min(reachable.len());
    ui::info(&format!(
        "对探测最快的 {} 个节点做真实访问验证（每个测 {} 次，全部 200 才算通过）...",
        verify_n, args.samples
    ));
    let mut verified: Vec<(f64, String)> = Vec::new();
    let mut last_code = 0u16;
    for (d, name) in reachable.iter().take(args.top) {
        if let Err(e) = mihomo::switch_group(be, agent, &args.group, name) {
            ui::warn(&format!("跳过 {}（{}）", name, e));
            continue;
        }
        match curl_avg(&be.proxy, &args.url, args.samples) {
            Ok(t) => {
                ui::ok(&format!("{} · 探测 {}ms · 实测平均 {:.2}s", name.green(), d, t));
                verified.push((t, name.clone()));
            }
            Err(code) => {
                last_code = code;
                ui::fail(&format!(
                    "{} · 探测 {}ms · 验证未通过（HTTP {}）",
                    name, d, code
                ));
            }
        }
        if verified.len() >= MAX_VERIFY {
            break;
        }
    }

    if verified.is_empty() {
        let _ = mihomo::switch_group(be, agent, &args.group, &orig);
        return Err(format!(
            "前 {} 个候选全部无法打开网页（最后 HTTP {}）——多半整批 IP 被风控，稍后再试或问机场",
            verify_n, last_code
        ));
    }

    verified.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let (best_t, best_name) = verified[0].clone();
    // 验证过程把组切到了最后一个候选，最终停在最优节点上
    mihomo::switch_group(be, agent, &args.group, &best_name)?;

    println!();
    ui::info("实测通过的节点（按实测延迟排序）：");
    for (t, n) in &verified {
        println!("      {} {:>6.2}s  {}", "✔".green(), t, n);
    }
    println!();
    if best_name == orig {
        ui::ok(&format!(
            "当前节点即最优：{}（实测 {:.2}s），未改动",
            best_name.green().bold(),
            best_t
        ));
    } else {
        ui::ok(&format!(
            "已切换 {} → {}（实测 {:.2}s）",
            args.group,
            best_name.green().bold(),
            best_t
        ));
    }
    Ok(best_name)
}
