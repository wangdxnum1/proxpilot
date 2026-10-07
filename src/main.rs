mod checker;
mod detect;
mod mihomo;
mod procinfo;
mod sysproxy;
mod ui;

use std::process::exit;
use std::thread;
use std::time::{Duration, Instant};

use colored::Colorize;

use detect::Backend;

pub struct Args {
    pub cmd: Cmd,
    pub api: Option<String>,
    pub secret: Option<String>,
    pub proxy: Option<String>,
    pub group: String,
    pub url: String,
    pub top: usize,
    pub samples: usize,
    pub dry_run: bool,
    pub interval: u64,
    pub reopt: u64,
}

pub enum Cmd {
    Check,
    Scan,
    Fix,
    Use(String),
    Watch,
    Help,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut cmd = Cmd::Help;
    let mut got_pos = false;
    let mut api = None;
    let mut secret = None;
    let mut proxy = None;
    let mut group = "AI服务".to_string();
    let mut url = "https://chatgpt.com/".to_string();
    let mut top = 12usize;
    let mut samples = 2usize;
    let mut dry_run = false;
    let mut interval = 300u64;
    let mut reopt = 7200u64;

    while let Some(a) = it.next() {
        match a.as_str() {
            "--api" => api = it.next(),
            "--secret" => secret = it.next(),
            "--proxy" => proxy = it.next(),
            "--group" => group = it.next().unwrap_or(group),
            "--url" => url = it.next().unwrap_or(url),
            "--top" => top = it.next().and_then(|v| v.parse().ok()).unwrap_or(top),
            "--samples" => samples = it.next().and_then(|v| v.parse().ok()).unwrap_or(samples),
            "--interval" => interval = it.next().and_then(|v| v.parse().ok()).unwrap_or(interval),
            "--reopt" => reopt = it.next().and_then(|v| v.parse().ok()).unwrap_or(reopt),
            "--dry-run" => dry_run = true,
            "-h" | "--help" => cmd = Cmd::Help,
            "check" | "scan" | "fix" | "watch" | "use" | "help" => {
                if !got_pos {
                    got_pos = true;
                    cmd = match a.as_str() {
                        "check" => Cmd::Check,
                        "scan" => Cmd::Scan,
                        "fix" => Cmd::Fix,
                        "watch" => Cmd::Watch,
                        "use" => Cmd::Use(it.next().unwrap_or_default()),
                        _ => Cmd::Help,
                    };
                }
            }
            _ => {}
        }
    }

    Args {
        cmd,
        api,
        secret,
        proxy,
        group,
        url,
        top,
        samples,
        dry_run,
        interval,
        reopt,
    }
}

fn print_help() {
    println!("{}", "ProxPilot · 代理辅助工具 v0.1".cyan().bold());
    println!();
    println!("用法: proxpilot <命令> [选项]");
    println!();
    println!("命令:");
    println!("  check          体检：内核 / 系统代理 / 当前节点能否访问测试网址");
    println!("  scan           探测组内所有节点的可达性（不切换）");
    println!("  fix            优选：探测 → 真实验证 → 切到实测最快的节点");
    println!("  use <节点>     手动切换到指定节点并验证");
    println!("  watch          守护模式：定时检查，坏了自动修，定期重新优选");
    println!();
    println!("选项:");
    println!("  --group <名称>   策略组名称（默认 AI服务）");
    println!("  --url <地址>     测试网址（默认 https://chatgpt.com/）");
    println!("  --api <地址>     内核 API（默认自动探测 9090/9097 等常见端口）");
    println!("  --secret <值>    内核 API 的 secret");
    println!("  --proxy <地址>   代理出口（默认读系统代理，如 http://127.0.0.1:7890）");
    println!("  --top <N>        优选时验证前 N 个候选（默认 12）");
    println!("  --samples <N>    每个节点实测次数（默认 2，全部 200 才通过）");
    println!("  --interval <秒>  watch 检查间隔（默认 300）");
    println!("  --reopt <秒>     watch 定时优选间隔（默认 7200）");
    println!("  --dry-run        只探测报告，不切换");
    println!();
    println!("客户端识别:");
    println!("  自动探测本机运行的 Clash/mihomo 系客户端并显示名称，");
    println!("  多个同时在线时默认优先 CuteCloud，用 --api 可指定其他实例。");
    println!();
    println!("示例:");
    println!("  proxpilot check");
    println!("  proxpilot fix");
    println!("  proxpilot fix --group 流媒体 --url https://www.youtube.com/");
    println!("  proxpilot watch --interval 180");
    println!("  proxpilot use \"香港 IEPL 01\"");
}

fn setup(args: &Args) -> Result<(Backend, ureq::Agent), i32> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let be = match detect::detect(&agent, args) {
        Ok(b) => b,
        Err(e) => {
            ui::fail(&e);
            return Err(1);
        }
    };
    ui::info(&format!("目标客户端：{}", be.client.green().bold()));
    ui::info(&format!("内核 API：{}（{}）", be.api, be.source));
    if let Some(v) = &be.version {
        ui::dim(&format!("内核版本 {}", v));
    }
    if !be.alternatives.is_empty() {
        for (c, a) in &be.alternatives {
            ui::dim(&format!("同时发现：{}（{}）—— 用 --api 可指定", c, a));
        }
    }
    ui::info(&format!("代理出口：{}", be.proxy));
    println!();
    Ok((be, agent))
}

fn cmd_check(be: &Backend, agent: &ureq::Agent, args: &Args) -> i32 {
    if !checker::core_alive(&be.proxy) {
        ui::fail("内核代理端口不通，请确认客户端已启动");
        return 1;
    }
    ui::ok("内核运行中，代理端口可用");

    if sysproxy::proxy_enabled() {
        ui::ok("系统代理已开启");
    } else {
        ui::warn("系统代理未开启（浏览器不走代理）——其他代理软件退出时常干这事");
        if args.dry_run {
            ui::dim("dry-run：不自动修复");
        } else {
            match sysproxy::enable() {
                Ok(()) => ui::ok("已自动重新打开系统代理"),
                Err(e) => ui::fail(&format!("自动打开失败：{}", e)),
            }
        }
    }

    match mihomo::get_proxies(be, agent) {
        Ok(proxies) => {
            if let Some(g) = proxies.get(&args.group) {
                let now = g.now.clone().unwrap_or_else(|| "未知".into());
                ui::info(&format!("当前组「{}」→ {}", args.group, now));
            }
            match checker::curl_avg(&be.proxy, &args.url, args.samples) {
                Ok(t) => {
                    ui::ok(&format!("{} 可访问（平均 {:.2}s）", args.url, t));
                    0
                }
                Err(c) => {
                    let cs = if c == 0 { "超时".to_string() } else { format!("HTTP {}", c) };
                    ui::fail(&format!(
                        "{} 不可访问（{}）—— 运行 proxpilot fix 自动优选切换",
                        args.url, cs
                    ));
                    2
                }
            }
        }
        Err(e) => {
            ui::fail(&e);
            1
        }
    }
}

fn cmd_scan(be: &Backend, agent: &ureq::Agent, args: &Args) -> i32 {
    let proxies = match mihomo::get_proxies(be, agent) {
        Ok(p) => p,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    let group = match proxies.get(&args.group) {
        Some(g) => g,
        None => {
            ui::fail(&format!("找不到组「{}」", args.group));
            return 1;
        }
    };
    let mut members: Vec<String> = Vec::new();
    for name in group.all.clone().unwrap_or_default() {
        if let Some(info) = proxies.get(&name) {
            if checker::is_real_node(info, &name) {
                members.push(name);
            }
        }
    }
    ui::info(&format!(
        "组「{}」共 {} 个真实节点，8 路并发探测...",
        args.group,
        members.len()
    ));
    let reachable = checker::scan_reachable(be, agent, &members, &args.url);
    println!();
    if reachable.is_empty() {
        ui::fail("没有任何节点可达");
        return 2;
    }
    ui::ok(&format!(
        "可达 {}/{}（注意：任何 HTTP 响应都算可达，IP 被风控的 403 也包含）",
        reachable.len(),
        members.len()
    ));
    for (d, n) in reachable.iter().take(30) {
        println!("      {} {:>5}ms  {}", "✔".green(), d, n);
    }
    ui::dim("提示：可达 ≠ 能打开网页，IP 是否被风控要用 proxpilot fix 实测验证");
    0
}

fn cmd_fix(be: &Backend, agent: &ureq::Agent, args: &Args) -> i32 {
    if args.dry_run {
        ui::warn("dry-run 模式：只探测报告，不切换");
        return cmd_scan(be, agent, args);
    }
    if !checker::core_alive(&be.proxy) {
        ui::fail("内核代理端口不通，请确认客户端已启动");
        return 1;
    }
    if sysproxy::proxy_enabled() {
        ui::ok("系统代理已开启");
    } else {
        ui::warn("系统代理未开启，自动重新打开");
        match sysproxy::enable() {
            Ok(()) => ui::ok("系统代理已开启"),
            Err(e) => ui::fail(&format!("打开失败：{}", e)),
        }
    }
    println!();
    match checker::fix_flow(be, agent, args) {
        Ok(_) => {
            match checker::curl_avg(&be.proxy, &args.url, 1) {
                Ok(t) => ui::ok(&format!("最终确认：{} → HTTP 200（{:.2}s）", args.url, t)),
                Err(c) => ui::warn(&format!("最终确认异常（HTTP {}），可重跑 proxpilot fix", c)),
            }
            0
        }
        Err(e) => {
            ui::fail(&e);
            2
        }
    }
}

fn cmd_use(be: &Backend, agent: &ureq::Agent, args: &Args, node: &str) -> i32 {
    if node.is_empty() {
        ui::fail("用法: proxpilot use <节点名>（节点名含空格请加引号）");
        return 1;
    }
    match mihomo::switch_group(be, agent, &args.group, node) {
        Ok(()) => {
            ui::ok(&format!("已切换 {} → {}", args.group, node.green().bold()));
            match checker::curl_avg(&be.proxy, &args.url, args.samples) {
                Ok(t) => {
                    ui::ok(&format!("验证通过：{} 平均 {:.2}s", args.url, t));
                    0
                }
                Err(c) => {
                    ui::warn(&format!(
                        "切换成功但 {} 未通过验证（HTTP {}）",
                        args.url, c
                    ));
                    2
                }
            }
        }
        Err(e) => {
            ui::fail(&e);
            1
        }
    }
}

fn cmd_watch(be: &Backend, agent: &ureq::Agent, args: &Args) -> i32 {
    ui::info(&format!(
        "守护模式：每 {} 秒检查一次，每 {} 秒重新优选，Ctrl+C 退出",
        args.interval, args.reopt
    ));
    let mut last_opt = Instant::now() - Duration::from_secs(args.reopt + 1);
    let mut cycle: u64 = 0;
    loop {
        cycle += 1;
        if !checker::core_alive(&be.proxy) {
            ui::warn("内核未运行，等待中...");
            thread::sleep(Duration::from_secs(args.interval));
            continue;
        }
        if !sysproxy::proxy_enabled() {
            ui::warn("系统代理被关闭，自动重新打开");
            let _ = sysproxy::enable();
        }
        let (code, _) = checker::curl_test(&be.proxy, &args.url);
        let mut broken = code != 200;
        if broken {
            // 5 秒后复测一次，避免 Cloudflare 偶发误报
            thread::sleep(Duration::from_secs(5));
            broken = checker::curl_test(&be.proxy, &args.url).0 != 200;
        }
        let due = last_opt.elapsed().as_secs() >= args.reopt;
        if broken || due {
            let reason = if broken {
                format!("检测到 {} 不可访问（HTTP {}）", args.url, code)
            } else {
                "到达定时优选时间".to_string()
            };
            ui::warn(&format!("[周期 {}] {}，开始优选...", cycle, reason));
            if let Err(e) = checker::fix_flow(be, agent, args) {
                ui::fail(&e);
            }
            last_opt = Instant::now();
        } else {
            ui::dim(&format!("[周期 {}] 正常（HTTP 200）", cycle));
        }
        thread::sleep(Duration::from_secs(args.interval));
    }
}

/// 控制台默认 GBK 代码页会显示乱码，切到 UTF-8（对 mintty 等无副作用）
#[cfg(windows)]
fn set_console_utf8() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleOutputCP(cp: u32) -> i32;
        fn SetConsoleCP(cp: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}

fn main() {
    #[cfg(windows)]
    set_console_utf8();
    let args = parse_args();
    if matches!(&args.cmd, Cmd::Help) {
        print_help();
        return;
    }
    ui::banner();
    let (be, agent) = match setup(&args) {
        Ok(x) => x,
        Err(c) => exit(c),
    };
    let code = match &args.cmd {
        Cmd::Check => cmd_check(&be, &agent, &args),
        Cmd::Scan => cmd_scan(&be, &agent, &args),
        Cmd::Fix => cmd_fix(&be, &agent, &args),
        Cmd::Use(n) => cmd_use(&be, &agent, &args, n),
        Cmd::Watch => cmd_watch(&be, &agent, &args),
        Cmd::Help => {
            print_help();
            0
        }
    };
    exit(code);
}
