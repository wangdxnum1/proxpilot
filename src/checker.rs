//! 节点探测、真实访问验证与优选切换。

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use colored::Colorize;

use crate::detect::Backend;
use crate::mihomo::{self, ProxyInfo};
use crate::ui;
use crate::Args;

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

/// 访问判定结果
#[derive(Debug, Clone)]
pub enum Verdict {
    /// 全部 200，平均耗时（秒）
    Pass(f64),
    /// chatgpt.com 返回 403，但 api.openai.com IP 检查通过（401）：
    /// 仅作为备选信号，不能保证浏览器通过 Cloudflare 验证
    PassIpOnly,
    /// 不可访问（HTTP 码，0 = 连接或传输失败）
    Fail(u16),
}

impl Verdict {
    pub fn is_ok(&self) -> bool {
        !matches!(self, Verdict::Fail(_))
    }
}

fn is_openai_url(url: &str) -> bool {
    url.contains("chatgpt.com") || url.contains("openai.com")
}

/// 未授权 API 返回 401 是备选信号，不保证网页可通过 Cloudflare 验证。
fn openai_ip_status(probe: &crate::http::BrowserProbe) -> u16 {
    probe
        .test("https://api.openai.com/v1/models")
        .map(|r| r.0)
        .unwrap_or(0)
}

/// 访问判定：samples 次全部 200 → Pass；
/// OpenAI 域名遇 403 时用 api.openai.com 交叉验证 IP 信誉（401 → PassIpOnly）；
/// 否则 Fail
pub fn verify_access(proxy: &str, url: &str, samples: usize) -> Verdict {
    let probe = match crate::http::BrowserProbe::new(proxy) {
        Ok(p) => p,
        Err(e) => {
            ui::fail(&format!("实测客户端初始化失败：{}", e));
            return Verdict::Fail(0);
        }
    };
    let n = samples.max(1);
    let spinner = ui::Spinner::start(&format!("实测 {}（第 1/{} 次）", url, n));
    let mut total = 0.0;
    let mut ok = 0usize;
    let mut last_code = 0u16;
    let mut saw_timeout = false;
    for s in 0..n {
        if s > 0 {
            spinner.set_text(format!("实测 {}（第 {}/{} 次）", url, s + 1, n));
        }
        let (code, t) = probe.test(url).unwrap_or((0, 99.0));
        match code {
            200 => {
                total += t;
                ok += 1;
            }
            0 => {
                last_code = 0;
                saw_timeout = true;
            }
            _ => last_code = code,
        }
    }
    if ok == n {
        return Verdict::Pass(total / n as f64);
    }
    if is_openai_url(url) && last_code == 403 && !saw_timeout {
        spinner.set_text("chatgpt.com 返回 403，交叉验证 IP（api.openai.com）...");
        if openai_ip_status(&probe) == 401 {
            // IP 正常，但还要确认隧道稳定（UDP 节点可能抖动：小包探测能过、
            // 真实握手时断时续）。再测两次，任何一次断连/超时都判失败。
            for k in 0..2 {
                spinner.set_text(format!("IP 正常，复测隧道稳定性（{}/2）...", k + 1));
                let (code, _) = probe.test(url).unwrap_or((0, 99.0));
                if code == 0 {
                    return Verdict::Fail(0);
                }
            }
            return Verdict::PassIpOnly;
        }
    }
    Verdict::Fail(last_code)
}

pub fn is_real_node(info: &ProxyInfo, name: &str) -> bool {
    !SKIP_TYPES.contains(&info.ptype.as_str()) && !FAKE_NODES.iter().any(|f| name.contains(f))
}

/// 并发（8 路）探测节点可达性，返回 (探测延迟, 节点名) 升序
pub fn scan_reachable(
    be: &Backend,
    agent: &reqwest::blocking::Client,
    members: &[String],
    url: &str,
) -> Vec<(i64, String)> {
    let total = members.len();
    let done = Arc::new(AtomicUsize::new(0));
    let spinner = ui::Spinner::start(&format!("并发探测 {} 个节点...", total));
    let reachable: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());
    thread::scope(|s| {
        for chunk in members.chunks(8) {
            let reachable = &reachable;
            let done = &done;
            let spinner = &spinner;
            s.spawn(move || {
                for name in chunk {
                    if let Some(d) = mihomo::probe_delay(be, agent, name, url, 4000) {
                        reachable.lock().unwrap().push((d, name.clone()));
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    spinner.set_text(format!("并发探测节点... 已测 {}/{}", n, total));
                }
            });
        }
    });
    drop(spinner);
    let mut v = reachable.into_inner().unwrap();
    v.sort();
    v
}

/// 完整优选：探测 → 逐个切换实测 → 停在实测最快的节点。全部失败则恢复原节点。
pub fn fix_flow(
    be: &Backend,
    agent: &reqwest::blocking::Client,
    args: &Args,
) -> Result<String, String> {
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
    ui::info(&format!("组内真实节点 {} 个，8 路并发探测可达性...", total));

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
    let mut verified: Vec<(f64, String, bool)> = Vec::new(); // (耗时, 节点, 是否仅 IP 通过)
    let mut last_code = 0u16;
    for (d, name) in reachable.iter().take(args.top) {
        if let Err(e) = mihomo::switch_group(be, agent, &args.group, name) {
            ui::warn(&format!("跳过 {}（{}）", name, e));
            continue;
        }
        match verify_access(&be.proxy, &args.url, args.samples) {
            Verdict::Pass(t) => {
                ui::ok(&format!("{} · 探测 {}ms · 实测平均 {:.2}s", name, d, t));
                verified.push((t, name.clone(), false));
            }
            Verdict::PassIpOnly => {
                ui::ok(&format!(
                    "{} · 探测 {}ms · IP 检查通过（网页 HTTP 403，浏览器访问仍需确认）",
                    name, d
                ));
                // 信任级低于真实 200：排序永远排在实测通过节点之后
                verified.push((f64::MAX, name.clone(), true));
            }
            Verdict::Fail(code) => {
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
    let (best_t, best_name, best_soft) = verified[0].clone();
    // 验证过程把组切到了最后一个候选，最终停在最优节点上
    mihomo::switch_group(be, agent, &args.group, &best_name)?;
    crate::appstate::sync_selection(&args.group, &best_name);

    println!();
    ui::info("实测通过的节点（按实测延迟排序）：");
    for (t, n, soft) in &verified {
        if *soft {
            println!("      {} IP检查通过  {}", "≈".yellow(), n);
        } else {
            println!("      {} {:>6.2}s  {}", "✔".green(), t, n);
        }
    }
    println!();
    if best_name == orig {
        ui::ok(&format!(
            "当前节点即最优：{}，未改动",
            best_name.green().bold()
        ));
    } else {
        ui::ok(&format!(
            "已切换 {} → {}",
            args.group,
            best_name.green().bold()
        ));
    }
    if best_soft {
        ui::dim("该节点 IP 检查通过，但网页仍返回 HTTP 403；浏览器访问需实际确认");
    } else {
        ui::dim(&format!("最优实测延迟 {:.2}s", best_t));
    }
    Ok(best_name)
}
