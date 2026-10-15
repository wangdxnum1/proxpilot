#[macro_use]
mod report;
mod appstate;
mod browse;
mod checker;
mod client_config;
mod clients;
mod commands;
mod core_api;
mod detect;
mod diagnose;
mod diagnostics;
mod history;
mod http;
mod mihomo;
mod nodeinfo;
mod operation;
mod probe_scheduler;
mod procinfo;
mod runtime_switch;
mod scan_output;
mod scoring;
mod switching;
mod sysproxy;
mod terminal;
mod ui;
mod watch;
mod watch_policy;
mod workflow;

fn command_name() -> String {
    let mut values = std::env::args().skip(1);
    while let Some(value) = values.next() {
        if matches!(
            value.as_str(),
            "--client"
                | "--api"
                | "--secret"
                | "--proxy"
                | "--group"
                | "--url"
                | "--max-rate"
                | "--top"
                | "--samples"
                | "--concurrency"
                | "--probe-timeout-ms"
                | "--verify-timeout-ms"
                | "--retries"
                | "--interval"
                | "--reopt"
                | "--min-switch-interval"
                | "--failure-cooldown"
                | "--manual-hold"
                | "--output"
        ) {
            values.next();
            continue;
        }
        if matches!(
            value.as_str(),
            "check"
                | "scan"
                | "fix"
                | "use"
                | "watch"
                | "nodes"
                | "info"
                | "clients"
                | "config"
                | "history"
                | "recover"
                | "diagnose"
                | "browse"
                | "help"
        ) {
            return value;
        }
    }
    "help".into()
}
static CONSOLE: std::sync::OnceLock<terminal::ConsoleGuard> = std::sync::OnceLock::new();
fn exit(code: i32) -> ! {
    if let Some(console) = CONSOLE.get() {
        console.restore();
    }
    std::process::exit(report::finish(&command_name(), code))
}

use colored::Colorize;

use detect::Backend;

#[derive(Clone)]
pub struct Args {
    pub client: Option<client_config::ClientSelection>,
    pub cmd: Cmd,
    pub api: Option<String>,
    pub secret: Option<String>,
    pub proxy: Option<String>,
    pub group: String,
    pub max_rate: Option<f64>,
    pub url: String,
    pub top: usize,
    pub samples: usize,
    pub probe: probe_scheduler::ProbeOptions,
    pub verify_timeout_ms: u32,
    pub dry_run: bool,
    pub manage_system_proxy: bool,
    pub interval: u64,
    pub reopt: u64,
    pub optimize: bool,
    pub watch: watch_policy::WatchSettings,
    pub detect: bool,
    pub json: bool,
    pub output: Option<std::path::PathBuf>,
    pub overwrite: bool,
}

#[derive(Clone)]
pub enum Cmd {
    Check,
    Scan,
    Fix,
    Use(String),
    Watch,
    Help,
    Clients(bool),
    Config(ConfigCommand),
    Nodes,
    Info(String),
    Recover(RecoveryAction),
    History(bool),
    Diagnose,
    Browse,
}

#[derive(Clone, Copy)]
pub enum RecoveryAction {
    Inspect,
    Retry,
    Discard,
}

#[derive(Clone)]
pub enum ConfigCommand {
    Show,
    Set(client_config::ClientSelection),
    Unset,
}

fn parse_args_from(input: impl IntoIterator<Item = String>) -> Result<Args, String> {
    use client_config::ClientSelection;
    let mut it = input.into_iter();
    let mut args = Args {
        client: None,
        cmd: Cmd::Help,
        api: None,
        secret: None,
        proxy: None,
        group: String::new(),
        max_rate: None,
        url: "https://chatgpt.com/".into(),
        top: 12,
        samples: 2,
        probe: Default::default(),
        verify_timeout_ms: 12000,
        dry_run: false,
        manage_system_proxy: true,
        interval: 300,
        reopt: 7200,
        optimize: false,
        watch: Default::default(),
        detect: false,
        json: false,
        output: None,
        overwrite: false,
    };
    let mut command = false;
    let mut supported = false;
    let mut recovery_action = None;
    fn value(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
        let v = it.next().ok_or_else(|| format!("{} 缺少参数值", flag))?;
        if v.starts_with("--") {
            return Err(format!("{} 缺少参数值", flag));
        }
        Ok(v)
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--client" => args.client = Some(ClientSelection::parse(&value(&mut it, &arg)?)?),
            "--api" => args.api = Some(value(&mut it, &arg)?),
            "--secret" => args.secret = Some(value(&mut it, &arg)?),
            "--proxy" => args.proxy = Some(value(&mut it, &arg)?),
            "--group" => {
                args.group = value(&mut it, &arg)?;
                if args.group.trim().is_empty() {
                    return Err("--group 名称不能为空".into());
                }
            }
            "--max-rate" => {
                let limit: f64 = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| "--max-rate 必须为正数")?;
                if !limit.is_finite() || limit <= 0.0 {
                    return Err("--max-rate 必须为有限正数".into());
                }
                args.max_rate = Some(limit);
            }
            "--url" => args.url = value(&mut it, &arg)?,
            "--concurrency" | "--probe-timeout-ms" | "--verify-timeout-ms" | "--retries" => {
                let number: u32 = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| format!("{} 必须为非负整数", arg))?;
                match arg.as_str() {
                    "--concurrency" if (1..=256).contains(&number) => {
                        args.probe.concurrency = Some(number as usize)
                    }
                    "--probe-timeout-ms" if (100..=120000).contains(&number) => {
                        args.probe.timeout_ms = number
                    }
                    "--verify-timeout-ms" if (100..=120000).contains(&number) => {
                        args.verify_timeout_ms = number
                    }
                    "--retries" if number <= 3 => args.probe.retries = number as u8,
                    _ => return Err(format!("{} 超出允许范围", arg)),
                }
            }
            "--top" => {
                args.top = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| "--top 必须为整数")?
            }
            "--samples" => {
                args.samples = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| "--samples 必须为整数")?
            }
            "--interval" => {
                args.interval = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| "--interval 必须为整数")?
            }
            "--reopt" => {
                args.reopt = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| "--reopt 必须为整数")?
            }
            "--optimize" => args.optimize = true,
            "--min-switch-interval" | "--failure-cooldown" | "--manual-hold" => {
                let seconds: u64 = value(&mut it, &arg)?
                    .parse()
                    .map_err(|_| format!("{} 必须为非负整数秒", arg))?;
                match arg.as_str() {
                    "--min-switch-interval" => args.watch.min_switch_interval_secs = seconds,
                    "--failure-cooldown" => args.watch.failure_cooldown_secs = seconds,
                    _ => args.watch.manual_hold_secs = seconds,
                }
            }
            "--dry-run" => args.dry_run = true,
            "--no-system-proxy" => args.manage_system_proxy = false,
            "--detect" => args.detect = true,
            "--json" => args.json = true,
            "--output" => args.output = Some(value(&mut it, &arg)?.into()),
            "--overwrite" => args.overwrite = true,
            "--supported" => supported = true,
            "--retry" | "--discard" => {
                if recovery_action.is_some() {
                    return Err("--retry 与 --discard 不能重复或同时使用".into());
                }
                recovery_action = Some(if arg == "--retry" {
                    RecoveryAction::Retry
                } else {
                    RecoveryAction::Discard
                });
            }
            "--help" | "-h" | "help" => {
                args.cmd = Cmd::Help;
                return Ok(args);
            }
            "check" | "scan" | "fix" | "use" | "watch" | "clients" | "config" | "nodes"
            | "info" | "recover" | "history" | "diagnose" | "browse"
                if !command =>
            {
                command = true;
                args.cmd = match arg.as_str() {
                    "browse" => Cmd::Browse,
                    "diagnose" => Cmd::Diagnose,
                    "nodes" => Cmd::Nodes,
                    "recover" => Cmd::Recover(RecoveryAction::Inspect),
                    "history" => Cmd::History(false),
                    "info" => Cmd::Info(value(&mut it, "info")?),
                    "check" => Cmd::Check,
                    "scan" => Cmd::Scan,
                    "fix" => Cmd::Fix,
                    "watch" => Cmd::Watch,
                    "use" => Cmd::Use(value(&mut it, "use")?),
                    "clients" => Cmd::Clients(false),
                    "config" => {
                        let action = value(&mut it, "config")?;
                        let action=match action.as_str() {
                            "show"=>ConfigCommand::Show,
                            "set"|"unset"=>{
                                if value(&mut it,"config key")?!="default-client" {return Err("配置键仅支持 default-client".into());}
                                if action=="set" {ConfigCommand::Set(ClientSelection::parse(&value(&mut it,"default-client")?)?)} else {ConfigCommand::Unset}
                            },_=>return Err("用法：config show | set default-client <客户端> | unset default-client".into())
                        };
                        Cmd::Config(action)
                    }
                    _ => unreachable!(),
                }
            }
            "clear" if matches!(args.cmd, Cmd::History(false)) => args.cmd = Cmd::History(true),
            _ => return Err(format!("未知命令或参数：{}", arg)),
        }
    }
    if matches!(args.cmd, Cmd::Browse) && args.json {
        return Err("browse 不能与 --json 组合".into());
    }
    if (args.output.is_some() || args.overwrite) && !matches!(args.cmd, Cmd::Diagnose) {
        return Err("--output / --overwrite 仅用于 diagnose".into());
    }
    if supported {
        if matches!(args.cmd, Cmd::Clients(_)) {
            args.cmd = Cmd::Clients(true);
        } else {
            return Err("--supported 仅用于 clients 命令".into());
        }
    }
    if let Some(action) = recovery_action {
        if matches!(args.cmd, Cmd::Recover(_)) {
            args.cmd = Cmd::Recover(action);
        } else {
            return Err("--retry / --discard 仅用于 recover 命令".into());
        }
    }
    if args.samples == 0 || args.interval == 0 || args.reopt == 0 {
        return Err("samples、interval、reopt 必须大于 0".into());
    }
    if !reqwest::Url::parse(&args.url)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
    {
        return Err("测试网址必须是带主机名的 http(s) URL".into());
    }
    Ok(args)
}

fn cmd_clients(args: &Args, supported: bool) -> Result<(), String> {
    if supported {
        report::data(serde_json::json!({"supported":["cutecloud","clash-verge","vvcloud","auto"]}));
        println!("cutecloud    CuteCloud    HTTP API；支持选择记忆同步");
        println!("clash-verge  Clash Verge  命名管道 / HTTP API；仅同步当前内核选择");
        println!("vvcloud     VVCloud      HTTP API；支持选择记忆同步");
        println!("auto         自动选择    系统代理开启时要求唯一匹配；关闭时要求唯一可用内核");
        return Ok(());
    }
    let api = core_api::CoreApi::new()?;
    let records = if args.api.is_some() {
        vec![clients::DiscoveryRecord {
            name: "explicit".into(),
            backend: Some(detect::detect(&api, args)?),
            error: None,
        }]
    } else {
        clients::discover(&api, &clients::DiscoveryPaths::current()?)
    };
    report::data(
        serde_json::json!({"clients":records.iter().map(|r|serde_json::json!({"client":r.backend.as_ref().map(commands::backend),"available":r.backend.is_some(),"error":if r.backend.is_none(){Some(diagnostics::AppError::new(diagnostics::ErrorCode::ClientUnavailable,diagnostics::Stage::Discovery))}else{None}})).collect::<Vec<_>>()}),
    );
    let active = if sysproxy::proxy_enabled() {
        sysproxy::registry_proxy_server()
    } else {
        None
    };
    let mut available = false;
    for record in records {
        if let Some(be) = record.backend {
            available = true;
            println!(
                "{} [{}]  API={}  出口={}  {}",
                record.name,
                be.kind.map(|k| k.name()).unwrap_or("mihomo"),
                be.api,
                be.proxy,
                if active
                    .as_deref()
                    .is_some_and(|r| clients::proxy_matches(&be.proxy, r))
                {
                    "[系统代理]"
                } else {
                    ""
                }
            );
            println!("  配置来源：{}", be.source);
        } else {
            println!(
                "{}：不可用 — {}",
                record.name,
                record.error.unwrap_or_default()
            );
        }
    }
    if available {
        Ok(())
    } else {
        Err("未发现可用客户端".into())
    }
}
fn cmd_config(command: &ConfigCommand, dry_run: bool) -> Result<(), String> {
    let path = client_config::config_path()?;
    match command {
        ConfigCommand::Show => {}
        ConfigCommand::Set(c) if !dry_run => client_config::save_default(&path, Some(*c))?,
        ConfigCommand::Unset if !dry_run => client_config::save_default(&path, None)?,
        _ => {}
    }
    let value = serde_json::json!({"schema_version":1,"default_client":client_config::load_default(&path)?.map(|c|c.to_string()),"dry_run":dry_run});
    report::data(&value);
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    Ok(())
}

fn cmd_history(clear: bool, dry_run: bool) -> Result<(), String> {
    if clear && dry_run {
        report::data(serde_json::json!({"cleared":false,"dry_run":true}));
        ui::dim("dry-run：将清理本工具的测量历史；未执行");
        return Ok(());
    }
    let _ownership = if clear {
        Some(operation::MutationLock::acquire()?)
    } else {
        None
    };
    let mut store = history::HistoryStore::current()?;
    if clear {
        store.clear()?;
        report::data(serde_json::json!({"cleared":true}));
        ui::ok("已清理 ProxPilot 测量历史；客户端配置和当前节点保持现状");
        return Ok(());
    }
    let now = history::now();
    let rows = store.list(now)?;
    report::data(
        serde_json::json!({"measurements":rows,"retention_seconds":history::RETENTION_SECONDS}),
    );
    ui::info(&format!(
        "最近测量：{} 条（30天内，最多显示100条；节点/范围使用不可逆指纹别名）",
        rows.len()
    ));
    let mut shown = std::collections::HashSet::new();
    for row in rows {
        let stage = if row.measurement.stage == diagnostics::Stage::Probe {
            "初筛"
        } else {
            "网页验证"
        };
        let grade = match row.measurement.availability {
            Some(diagnostics::Availability::Webpage) => "网页通过",
            Some(diagnostics::Availability::IpOnly) => "仅IP通过",
            Some(diagnostics::Availability::Failed) => "未通过",
            None => {
                if row.measurement.delay_ms.is_some() {
                    "收到响应"
                } else {
                    "探测失败"
                }
            }
        };
        let delay = row
            .measurement
            .delay_ms
            .map(|ms| format!("{ms:.0}ms"))
            .unwrap_or_else(|| "—".into());
        println!(
            "  {}秒前  范围#{}  节点#{}  {} · {} · {}",
            now.saturating_sub(row.timestamp),
            &row.scope[..12],
            &row.node[..12],
            stage,
            grade,
            delay
        );
        if row.measurement.stage == diagnostics::Stage::Verification
            && shown.insert((row.scope.clone(), row.target.clone(), row.node.clone()))
        {
            let evidence = store.recent_entry(&row, now)?;
            let unverified = diagnostics::VerificationReport {
                availability: diagnostics::Availability::Failed,
                mean_ms: None,
                samples_requested: 0,
                samples_passed: 0,
                error: None,
            };
            let basis = scoring::score_at(&evidence, &unverified, None, now);
            ui::dim(&format!("历史依据（人为权重 50/25/15/10）：{} 次验证 · 成功率分 {:.1}/50 · 延迟分 {:.1}/25 · 稳定性分 {:.1}/15；倍率未知，成本分0；不代表当前可用",basis.samples,basis.success,basis.latency,basis.stability));
        }
    }
    Ok(())
}
#[cfg(test)]
mod cli_tests {
    use super::*;
    fn parse(values: &[&str]) -> Result<Args, String> {
        parse_args_from(values.iter().map(|v| v.to_string()))
    }
    #[test]
    fn watch_protection_controls_accept_explicit_zero_and_reject_negative_values() {
        for flag in [
            "--min-switch-interval",
            "--failure-cooldown",
            "--manual-hold",
        ] {
            assert!(parse(&["watch", flag, "0"]).is_ok());
            assert!(parse(&["watch", flag, "900"]).is_ok());
            assert!(parse(&["watch", flag, "-1"]).is_err());
        }
    }
    #[test]
    fn probe_controls_have_strict_bounds_and_valid_defaults() {
        for (flag, good, bad) in [
            ("--concurrency", vec!["1", "256"], vec!["0", "257", "-1"]),
            (
                "--probe-timeout-ms",
                vec!["100", "120000"],
                vec!["99", "120001"],
            ),
            (
                "--verify-timeout-ms",
                vec!["100", "120000"],
                vec!["99", "120001"],
            ),
            ("--retries", vec!["0", "3"], vec!["4", "-1"]),
        ] {
            for value in good {
                assert!(parse(&["scan", flag, value]).is_ok(), "{flag} {value}");
            }
            for value in bad {
                assert!(parse(&["scan", flag, value]).is_err(), "{flag} {value}");
            }
        }
    }
    #[test]
    fn client_commands_and_strict_errors() {
        assert!(matches!(
            parse(&["clients", "--supported"]).unwrap().cmd,
            Cmd::Clients(true)
        ));
        assert!(matches!(
            parse(&["config", "set", "default-client", "clash-verge"])
                .unwrap()
                .cmd,
            Cmd::Config(ConfigCommand::Set(_))
        ));
        assert!(parse(&["check", "--client", "verge"]).is_err());
        assert!(parse(&["check", "--client"]).is_err());
        assert!(parse(&["watch", "--interval", "0"]).is_err());
        assert!(parse(&["check", "--typo"]).is_err());
        assert!(matches!(
            parse(&["nodes", "--max-rate", "1"]).unwrap().cmd,
            Cmd::Nodes
        ));
        assert!(matches!(
            parse(&["info", "日本 🛰"]).unwrap().cmd,
            Cmd::Info(_)
        ));
        assert_eq!(
            parse(&["watch", "--max-rate", "0.5"]).unwrap().max_rate,
            Some(0.5)
        );
        for value in ["0", "-1", "NaN", "inf", "invalid"] {
            assert!(parse(&["scan", "--max-rate", value]).is_err());
        }
        assert!(parse(&["scan", "--max-rate"]).is_err());
        assert_eq!(
            parse(&["--client", "auto", "check"]).unwrap().client,
            Some(client_config::ClientSelection::Auto)
        );
    }
}

fn print_help() {
    println!(
        "{}",
        format!("ProxPilot · 代理优选助手 v{}", env!("CARGO_PKG_VERSION"))
            .cyan()
            .bold()
    );
    println!();
    println!("用法: proxpilot <命令> [选项]");
    println!();
    println!("命令:");
    println!("  browse         终端分页/筛选/详情/扫描；切换须确认，默认取消");
    println!("  diagnose       只读匿名诊断；--output <文件>，覆盖需 --overwrite；不测速/上传");
    println!("  history [clear] 查看最近测量记录或清理本工具历史（无需内核在线）");
    println!("  recover [--retry | --discard]  查看、重试恢复或仅清理恢复记录");
    println!("  clients [--supported]  查看发现的客户端 / 支持的类型");
    println!("  config show | set default-client <类型> | unset default-client");
    println!("  nodes          列出组内真实节点的协议、倍率和名称标签（不测速）");
    println!("  info <节点>    查看节点配置详情及信息来源（不显示凭据）");
    println!("  check          体检：内核 / 系统代理 / 当前节点能否访问测试网址");
    println!("  scan           探测组内所有节点的可达性（不切换）");
    println!("  fix            优选：探测 → 真实验证 → 按验证等级和参考分优选并确认");
    println!("  use <节点>     手动切换到指定节点并验证");
    println!("  watch          守护模式：保持可用节点，坏了自动修；加 --optimize 主动优选");
    println!();
    println!("选项:");
    println!("  --json           本地结果 JSON；watch 为逐行事件（诊断报告另行匿名）");
    println!("  --group <名称>   指定策略组（默认优先读取客户端当前组并校验内核）");
    println!("  --url <地址>     测试网址（默认 https://chatgpt.com/）");
    println!("  --client <类型>  cutecloud / clash-verge / vvcloud / auto；覆盖保存的默认客户端");
    println!("  --api <地址>     显式指定 HTTP 内核 API（优先级最高）");
    println!("  --secret <值>    内核 API 的 secret");
    println!("  --proxy <地址>   代理出口（默认读目标内核运行端口）");
    println!("  --max-rate <N>  倍率上限；未知倍率排除，scan/fix/watch/nodes/use 生效");
    println!("  --top <N>        优选时验证前 N 个候选（默认 12）");
    println!("  --samples <N>    每个节点实测次数（默认 2，全部 200 才通过）");
    println!("  --concurrency <N> 固定并发 1–256；默认自适应，上限 min(CPU×2,32,节点数)");
    println!("  --probe-timeout-ms <N>  初筛超时 100–120000ms（默认 4000）");
    println!("  --verify-timeout-ms <N> 网页单次采样整体超时 100–120000ms（默认 12000）");
    println!("  --retries <N>    初筛传输失败额外重试 0–3 次（默认 1），不重试网页失败采样");
    println!("  --interval <秒>  watch 检查间隔（默认 300）");
    println!("  --optimize      watch 启动时及定时主动优选（默认关闭）");
    println!("  --reopt <秒>     --optimize 的定时优选间隔（默认 7200）");
    println!("  --min-switch-interval <秒> 正式切换最短间隔（默认 120，0 关闭）");
    println!("  --failure-cooldown <秒>    失败候选冷却时间（默认 600，0 关闭）");
    println!("  --manual-hold <秒>         操作外选择变化后的主动优化保护（默认 900，0 关闭）");
    println!("  --dry-run        只探测报告，不切换");
    println!("  --no-system-proxy 保持系统代理现状（适用于 TUN 或手动代理）");
    println!("  --detect         自动探测（兼容 --client auto）");
    println!("默认：未配置时使用 auto；系统代理开启时要求唯一匹配，关闭时要求唯一可用内核。");
    println!();
    println!("示例:");
    println!("  proxpilot check");
    println!("  proxpilot fix");
    println!("  proxpilot fix --group 流媒体 --url https://www.youtube.com/");
    println!("  proxpilot watch --interval 180");
    println!("  proxpilot watch --optimize --reopt 7200");
    println!("  proxpilot use \"香港 IEPL 01\"");
}

fn setup(args: &Args) -> Result<(Backend, crate::core_api::CoreApi), i32> {
    let agent = core_api::CoreApi::new().map_err(|e| {
        ui::fail(&format!("HTTP 客户端初始化失败：{}", e));
        1
    })?;
    let be = match detect::detect(&agent, args) {
        Ok(b) => b,
        Err(e) => {
            ui::fail(&e);
            return Err(1);
        }
    };
    if matches!(args.cmd, Cmd::Scan) {
        scan_output::note(
            &format!(
                "{} · 内核 {} · {}",
                be.client,
                be.version.as_deref().unwrap_or("版本未知"),
                be.proxy
            ),
            scan_output::terminal_width(),
        );
    } else {
        ui::info(&format!("目标客户端：{}", be.client.green().bold()));
        ui::info(&format!("内核 API：{}（{}）", be.api, be.source));
        if let Some(v) = &be.version {
            ui::dim(&format!("内核版本 {}", v));
        }
        ui::info(&format!("代理出口：{}", be.proxy));
    }
    if !be.alternatives.is_empty() {
        for (c, a) in &be.alternatives {
            ui::dim(&format!("同时发现：{}（{}）—— 用 --api 可指定", c, a));
        }
    }
    println!();
    Ok((be, agent))
}

fn runtime_group_args(be: &Backend, api: &core_api::CoreApi, args: &Args) -> Result<Args, String> {
    let proxies = mihomo::get_proxies(be, api)?;
    let saved = if args.group.is_empty() {
        appstate::current_group(be)
    } else {
        None
    };
    group_args_from_proxies(&proxies, args, saved.as_deref())
}
fn group_args_from_proxies(
    proxies: &std::collections::HashMap<String, mihomo::ProxyInfo>,
    args: &Args,
    saved: Option<&str>,
) -> Result<Args, String> {
    let mut resolved = args.clone();
    if args.group.is_empty() {
        if let Some(group) =
            saved.filter(|name| proxies.get(*name).is_some_and(|p| p.all.is_some()))
        {
            resolved.group = group.into();
            ui::info(&format!(
                "客户端当前策略组：{}（本地记忆，已校验内核）",
                group
            ));
            return Ok(resolved);
        }
        if saved.is_some() {
            ui::warn("客户端保存的策略组已失效，按实时内核确定检测或切换范围");
        }
        if matches!(args.cmd, Cmd::Check | Cmd::Scan | Cmd::Nodes) {
            return Ok(resolved);
        }
    }
    resolved.group = mihomo::resolve_group(proxies, &args.group)?;
    if args.group.is_empty() {
        ui::info(&format!("实时识别策略组：{}", resolved.group));
    }
    Ok(resolved)
}

struct NodeScope {
    label: String,
    members: Vec<String>,
    current: Vec<(String, Option<String>)>,
    current_notice: Option<String>,
}

impl NodeScope {
    fn is_current(&self, name: &str) -> bool {
        self.current
            .iter()
            .any(|(_, selected)| selected.as_deref() == Some(name))
    }

    fn print_current(&self, members: &[String], reachable: &[(i64, String)]) {
        let width = scan_output::terminal_width();
        for (group, selected) in &self.current {
            print!(
                "{}",
                scan_output::current(
                    group,
                    selected.as_deref(),
                    &format!(
                        "{} · 扫描开始时的选择",
                        scan_current_detail(selected.as_deref(), members, reachable)
                    ),
                    width,
                )
            );
        }
        if let Some(notice) = &self.current_notice {
            scan_output::note(notice, width);
        }
    }
}

#[cfg(test)]
fn node_scope(
    proxies: &std::collections::HashMap<String, mihomo::ProxyInfo>,
    group: &str,
) -> Result<NodeScope, String> {
    node_scope_with_mode(proxies, group, Some("rule"))
}

fn selected_leaf(
    proxies: &std::collections::HashMap<String, mihomo::ProxyInfo>,
    group: &str,
) -> Option<String> {
    let mut name = group;
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(name) {
            return None;
        }
        let info = proxies.get(name)?;
        if info.all.is_none() {
            return checker::is_real_node(info, name).then(|| name.to_string());
        }
        name = info.now.as_deref()?;
    }
}

fn node_scope_with_mode(
    proxies: &std::collections::HashMap<String, mihomo::ProxyInfo>,
    group: &str,
    mode: Option<&str>,
) -> Result<NodeScope, String> {
    let (label, mut members) = if !group.is_empty() {
        let info = proxies
            .get(group)
            .filter(|p| p.all.is_some())
            .ok_or_else(|| mihomo::missing_group(proxies, group))?;
        (
            format!("组「{}」", group),
            info.all.clone().unwrap_or_default(),
        )
    } else {
        ("全内核".into(), proxies.keys().cloned().collect())
    };
    if group.is_empty() {
        members.sort();
    }
    let root = match mode {
        Some("global") => Some("GLOBAL".to_string()),
        Some("rule") => mihomo::resolve_group(proxies, group).ok(),
        _ => None,
    };
    let mut current = Vec::new();
    let current_notice = if let Some(root) = root {
        if let Some(leaf) = selected_leaf(proxies, &root) {
            current.push((root, Some(leaf)));
            None
        } else {
            Some(format!("组「{}」的选择链未指向可确认的真实节点（可能为直连、拒绝、信息条目或循环）；未标记 [当前]", root))
        }
    } else {
        Some(match mode {
            Some("direct") => "当前为直连模式，未使用代理节点".into(),
            Some("rule") => {
                "规则模式存在多个候选业务组，无法唯一确定当前节点；可用 --group 指定检查范围".into()
            }
            _ => "无法确认代理模式，未标记当前节点".into(),
        })
    };
    Ok(NodeScope {
        label,
        members,
        current,
        current_notice,
    })
}

fn cmd_nodes(be: &Backend, api: &core_api::CoreApi, args: &Args) -> i32 {
    let proxies = match mihomo::get_proxies(be, api) {
        Ok(p) => p,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    let mode = mihomo::get_mode(be, api).ok();
    let scope = match node_scope_with_mode(&proxies, &args.group, mode.as_deref()) {
        Ok(scope) => scope,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    let policy = nodeinfo::BillingPolicy::for_backend(be, &proxies);
    policy.report();
    let selected =
        nodeinfo::candidates_with_policy(&proxies, &scope.members, args.max_rate, &policy);
    nodeinfo::report_candidates(&selected, args.max_rate);
    ui::info(&format!(
        "{}真实节点 {} 个（不测速）",
        scope.label,
        selected.names.len()
    ));
    report::data(
        serde_json::json!({"backend":commands::backend(be),"nodes":selected.names.iter().map(|n|commands::node(n,&proxies[n],&policy,scope.is_current(n))).collect::<Vec<_>>()}),
    );
    for name in &selected.names {
        let info = &proxies[name];
        println!(
            "  {}  [{}]  {}  {}{}",
            name,
            info.ptype,
            policy.label(name),
            nodeinfo::name_labels(name).join(" / "),
            if scope.is_current(name) {
                " [当前]"
            } else {
                ""
            }
        );
    }
    ui::dim(
        "协议来自内核；倍率来自名称或已识别的订阅提示；线路、家宽和用途标签来自名称，未实测验证。",
    );
    if selected.names.is_empty() {
        1
    } else {
        0
    }
}
fn cmd_info(be: &Backend, api: &core_api::CoreApi, args: &Args, name: &str) -> i32 {
    let proxies = match mihomo::get_proxies(be, api) {
        Ok(p) => p,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    let Some(live) = proxies.get(name).filter(|p| checker::is_real_node(p, name)) else {
        ui::fail("未找到此真实节点；用 nodes 查看名称（含空格请加引号）");
        return 1;
    };
    if !args.group.is_empty()
        && !proxies
            .get(&args.group)
            .and_then(|p| p.all.as_ref())
            .is_some_and(|members| members.iter().any(|n| n == name))
    {
        ui::fail("节点不属于指定策略组");
        return 1;
    }
    let policy = nodeinfo::BillingPolicy::for_backend(be, &proxies);
    policy.report();
    let metadata = nodeinfo::load_metadata(be);
    let record = metadata.nodes.get(name);
    let config = record.map(|(config, _)| config);
    let mut evidence = commands::node(name, live, &policy, false);
    evidence["configuration"] = serde_json::to_value(config).unwrap();
    evidence["groups"] = serde_json::json!(proxies
        .iter()
        .filter(|(_, p)| p.all.as_ref().is_some_and(|a| a.iter().any(|n| n == name)))
        .map(|(n, _)| n)
        .collect::<Vec<_>>());
    report::data(evidence);
    ui::info(&format!("节点：{}", name));
    println!("  协议：{}（内核 API）", live.ptype);
    println!(
        "  配置协议：{}",
        config.and_then(|c| c.protocol.as_deref()).unwrap_or("未知")
    );
    println!(
        "  入口服务器：{}",
        config.and_then(|c| c.server.as_deref()).unwrap_or("未知")
    );
    println!(
        "  入口端口：{}",
        config
            .and_then(|c| c.port)
            .map(|p| p.to_string())
            .unwrap_or_else(|| "未知".into())
    );
    println!(
        "  传输方式：{}",
        config
            .and_then(|c| c.network.as_deref())
            .unwrap_or("未知/未显式配置")
    );
    println!("  TLS：{}", nodeinfo::bool_text(config.and_then(|c| c.tls)));
    println!(
        "  UDP 配置：{}",
        nodeinfo::bool_text(config.and_then(|c| c.udp))
    );
    println!("  UDP 内核能力：{}", nodeinfo::bool_text(live.udp));
    println!(
        "  加密算法：{}",
        config
            .and_then(|c| c.cipher.as_deref())
            .unwrap_or("未知/未显式配置")
    );
    println!(
        "  TLS 服务名：{}",
        config
            .and_then(|c| c.sni.as_deref())
            .unwrap_or("未知/未显式配置")
    );
    println!(
        "  配置来源：{}",
        record
            .map(|(_, source)| source.as_str())
            .unwrap_or("没有可关联的本机节点配置")
    );
    println!("  倍率：{}", policy.label(name));
    println!(
        "  名称标签：{}（未验证）",
        nodeinfo::name_labels(name).join(" / ")
    );
    let mut groups: Vec<_> = proxies
        .iter()
        .filter(|(_, p)| {
            p.all
                .as_ref()
                .is_some_and(|members| members.iter().any(|n| n == name))
        })
        .map(|(n, _)| n.as_str())
        .collect();
    groups.sort();
    println!("  所属策略组：{}（内核 API）", groups.join("、"));
    if !policy.permitted(name, args.max_rate) {
        ui::warn("此节点不符合指定倍率上限；详情命令仅展示，不切换");
    }
    for notice in metadata.notices {
        ui::dim(&notice);
    }
    ui::dim(
        "入口地址不等于最终出口 IP；本命令不测速、不切换，不显示密码、UUID、secret 或订阅 URL。",
    );
    0
}

fn cmd_check(be: &Backend, agent: &crate::core_api::CoreApi, args: &Args) -> i32 {
    if !checker::core_alive(&be.proxy) {
        ui::fail("内核代理端口不通，请确认客户端已启动；若使用的不是 CuteCloud，加 --detect 自动探测或用 --api 指定");
        return 1;
    }
    ui::ok("内核运行中，代理端口可用");

    if !args.manage_system_proxy {
        ui::dim("系统代理保持现状（--no-system-proxy）");
    } else if sysproxy::matches(&be.proxy) {
        ui::ok("系统代理已开启");
    } else {
        ui::warn("系统代理未开启或指向其他客户端（浏览器不走代理）——其他代理软件退出时常干这事");
        if args.dry_run {
            ui::dim("dry-run：不自动修复");
        } else {
            let control = runtime_switch::KernelSwitchControl {
                backend: be,
                api: agent,
            };
            match workflow::repair_system_proxy(&control) {
                Ok(()) => ui::ok("已自动重新打开系统代理"),
                Err(e) => {
                    ui::fail(&format!("自动打开失败：{}", e));
                    return e.exit_code();
                }
            }
        }
    }

    match mihomo::get_proxies(be, agent) {
        Ok(proxies) => {
            if !args.group.is_empty() && !proxies.contains_key(&args.group) {
                ui::fail(&mihomo::missing_group(&proxies, &args.group));
                return 1;
            }
            let display_group = if args.group.is_empty() {
                mihomo::resolve_group(&proxies, "").ok()
            } else {
                Some(args.group.clone())
            };
            if let Some((name, g)) = display_group
                .as_deref()
                .and_then(|name| proxies.get(name).map(|g| (name, g)))
            {
                let now = g.now.clone().unwrap_or_else(|| "未知".into());
                ui::info(&format!("当前组「{}」→ {}", name, now));
            }
            let current = checker::verify_current_report(
                be,
                agent,
                display_group.as_deref().unwrap_or(""),
                &args.url,
                args.samples,
                args.verify_timeout_ms,
            );
            report::data(&current);
            if let Some(e) = &current.history_warning {
                report::warning(e);
            }
            if let Some(e) = &current.controller_error {
                report::error(e);
                ui::fail(&e.to_string());
                return if e.code == diagnostics::ErrorCode::Cancelled {
                    130
                } else {
                    1
                };
            }
            match current.verification.availability {
                diagnostics::Availability::Webpage => {
                    let t = current.verification.mean_ms.unwrap_or_default() / 1000.0;
                    ui::ok(&format!("{} 可访问（平均 {:.2}s）", args.url, t));
                    0
                }
                diagnostics::Availability::IpOnly => {
                    ui::ok(&format!("{} 可访问（IP 检查通过）", args.url));
                    ui::dim("说明：网页返回 HTTP 403；API 检查通过，但浏览器访问仍需确认");
                    0
                }
                diagnostics::Availability::Failed => {
                    let c = current.verification.error.unwrap_or_else(|| {
                        diagnostics::AppError::new(
                            diagnostics::ErrorCode::ProbeFailed,
                            diagnostics::Stage::Verification,
                        )
                    });
                    report::error(&c);
                    if c.code == diagnostics::ErrorCode::Cancelled {
                        return 130;
                    }
                    let cs = c.to_string();
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

fn cmd_scan(be: &Backend, agent: &crate::core_api::CoreApi, args: &Args) -> i32 {
    match switching::inspect_recovery() {
        Ok(Some(_)) => ui::warn("存在尚未清理的操作记录；本次只读扫描保持现状，可用 recover 查看或显式处理"),
        Err(_) => ui::warn("操作恢复记录无法安全读取；本次只读扫描保持现状，可用 recover 查看，确认放弃后用 recover --discard 清理"),
        Ok(None) => {}
    }
    let proxies = match mihomo::get_proxies(be, agent) {
        Ok(p) => p,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    let mode = mihomo::get_mode(be, agent).ok();
    let scope = match node_scope_with_mode(&proxies, &args.group, mode.as_deref()) {
        Ok(scope) => scope,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    let policy = nodeinfo::BillingPolicy::for_backend(be, &proxies);
    policy.report();
    let selected =
        nodeinfo::candidates_with_policy(&proxies, &scope.members, args.max_rate, &policy);
    nodeinfo::report_candidates(&selected, args.max_rate);
    let members = selected.names;
    if members.is_empty() {
        scope.print_current(&members, &[]);
        ui::fail("没有符合条件的真实节点，请检查倍率限制或策略组");
        return 1;
    }
    ui::info(&format!(
        "{}共 {} 个真实节点（去重后），并发上限 {}...",
        scope.label,
        members.len(),
        args.probe.limit(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            members.len()
        )
    ));
    let report = checker::scan_report(be, agent, &members, &args.url, &args.probe, &args.group);
    let ranks = report.reachable();
    let nodes: Vec<_> = report
        .measurements
        .iter()
        .map(|m| {
            let mut n = commands::node(
                &m.node,
                &proxies[&m.node],
                &policy,
                scope.is_current(&m.node),
            );
            n["delay_ms"] = serde_json::json!(m.delay_ms);
            n["error"] = serde_json::json!(m.error);
            n["attempts"] = serde_json::json!(m.attempts);
            n["rank"] =
                serde_json::json!(ranks.iter().position(|(_, n)| n == &m.node).map(|i| i + 1));
            n
        })
        .collect();
    report::data(
        serde_json::json!({"backend":commands::backend(be),"nodes":nodes,"current":scope.current,"total":members.len(),"probe":report}),
    );
    if let Some(error) = &report.history_warning {
        report::warning(error);
    }
    if report.cancelled {
        ui::warn(&format!(
            "扫描已取消，已完成 {}/{}",
            report.measurements.len(),
            members.len()
        ));
        return 130;
    }
    let reachable = report.reachable();
    if let Some(error) = report.controller_failure() {
        report::error(&error);
        ui::fail(&error.to_string());
        return 1;
    }
    let mut failures = std::collections::BTreeMap::<String, usize>::new();
    for error in report.measurements.iter().filter_map(|m| m.error.as_ref()) {
        *failures.entry(error.to_string()).or_default() += 1;
    }
    for (reason, count) in failures {
        ui::dim(&format!("{count} 个节点探测未成功：{reason}"));
    }
    println!();
    scope.print_current(&members, &reachable);
    if reachable.is_empty() {
        ui::fail("没有任何节点可达");
        return 2;
    }
    let width = scan_output::terminal_width();
    println!(
        "  {}  {} 可达 / {} 已测",
        "扫描完成".bold(),
        reachable.len(),
        members.len()
    );
    println!();
    ui::info(&format!(
        "可达排行：显示前 {} / {} 个（按延迟排序）",
        reachable.len().min(30),
        reachable.len()
    ));
    print!("{}", scan_output::header(width, false));
    for (index, (d, n)) in reachable.iter().take(30).enumerate() {
        print!(
            "{}",
            scan_output::row(
                &scan_output::Row {
                    delay: *d,
                    name: n,
                    current: scope.is_current(n),
                    rank: Some(index + 1),
                    rate: policy.resolve(n),
                    protocol: proxies.get(n).map(|p| p.ptype.as_str()).unwrap_or(""),
                },
                width,
                false
            )
        );
    }
    let economical: Vec<_> = reachable
        .iter()
        .filter(|(_, name)| policy.resolve(name).is_some_and(|(rate, _)| rate == 1.0))
        .collect();
    println!();
    ui::info(&format!(
        "1倍率可达节点：{} 个（按延迟排序，完整列出）",
        economical.len()
    ));
    if !economical.is_empty() {
        print!("{}", scan_output::header(width, true));
    }
    for (delay, name) in economical {
        print!(
            "{}",
            scan_output::row(
                &scan_output::Row {
                    delay: *delay,
                    name,
                    current: scope.is_current(name),
                    rank: None,
                    rate: policy.resolve(name),
                    protocol: proxies.get(name).map(|p| p.ptype.as_str()).unwrap_or(""),
                },
                width,
                true
            )
        );
    }
    println!();
    scan_output::note(
        "延迟 ≤ 200ms 为低延迟；1× 为1倍率，? 为未知倍率；▶ 当前；SS = Shadowsocks。",
        width,
    );
    scan_output::note(
        "协议来自内核；倍率来自名称或标注的订阅提示；线路与家宽标签来自名称，未经实测验证。",
        width,
    );
    scan_output::note(
        "可达仅表示收到 HTTP 响应（含 403）；网站可用性请运行 proxpilot fix。",
        width,
    );
    0
}

fn scan_current_detail(
    current: Option<&str>,
    members: &[String],
    reachable: &[(i64, String)],
) -> String {
    let Some(name) = current else {
        return "内核未提供当前选择".into();
    };
    if let Some((rank, (delay, _))) = reachable.iter().enumerate().find(|(_, (_, n))| n == name) {
        format!("{}ms · 可达排名 {}/{}", delay, rank + 1, reachable.len())
    } else if members.iter().any(|n| n == name) {
        "本轮探测未成功（超时或请求失败，不等于客户端一定不可用）".into()
    } else {
        "未参与本轮探测（不在筛选后的真实节点候选中）".into()
    }
}

#[cfg(test)]
mod scan_current_tests {
    use super::*;
    fn scan_current_status(
        current: Option<&str>,
        members: &[String],
        reachable: &[(i64, String)],
    ) -> String {
        scan_output::current(
            "test",
            current,
            &scan_current_detail(current, members, reachable),
            100,
        )
    }
    #[test]
    fn current_node_outside_top_thirty_is_still_visible() {
        let mut reachable: Vec<_> = (0..30).map(|i| (i + 1, format!("node-{}", i))).collect();
        reachable.push((900, "selected-3倍率".into()));
        let members: Vec<_> = reachable.iter().map(|(_, n)| n.clone()).collect();
        let text = scan_current_status(Some("selected-3倍率"), &members, &reachable);
        assert!(text.contains("selected-3倍率"));
        assert!(text.contains("900ms"));
        assert!(text.contains("31/31"));
    }
    #[test]
    fn failed_and_filtered_current_nodes_have_distinct_statuses() {
        let members = vec!["selected-3倍率".into()];
        let failed = scan_current_status(Some("selected-3倍率"), &members, &[]);
        assert!(failed.contains("selected-3倍率"));
        assert!(failed.contains("探测未成功"));
        let filtered = scan_current_status(Some("selected-3倍率"), &[], &[]);
        assert!(filtered.contains("selected-3倍率"));
        assert!(filtered.contains("未参与"));
        assert!(!filtered.contains("探测未成功"));
        assert!(scan_current_status(None, &[], &[]).contains("未提供"));
    }
}

#[cfg(test)]
mod node_scope_tests {
    use super::*;

    fn proxies() -> std::collections::HashMap<String, mihomo::ProxyInfo> {
        serde_json::from_value(serde_json::json!({
            "AI服务": {"type":"Selector", "all":["自动选择", "共享-1倍率"], "now":"自动选择"},
            "流媒体": {"type":"Selector", "all":["共享-1倍率", "其他-3倍率", "共享-1倍率"], "now":"共享-1倍率"},
            "自动选择": {"type":"URLTest", "all":["共享-1倍率"], "now":"共享-1倍率"},
            "共享-1倍率": {"type":"Shadowsocks"},
            "其他-3倍率": {"type":"Trojan"},
            "未分组-1倍率": {"type":"Vmess"},
            "DIRECT": {"type":"Direct"},
            "REJECT": {"type":"Reject"},
            "续费网址:https://example.invalid": {"type":"Shadowsocks"}
        })).unwrap()
    }

    #[test]
    fn saved_group_is_used_by_detection_and_switching_commands_with_explicit_override() {
        let proxies = proxies();
        for command in ["check", "scan", "nodes", "fix", "use", "watch"] {
            let mut input = vec![command.to_string()];
            if command == "use" {
                input.push("共享-1倍率".into());
            }
            let args = parse_args_from(input.clone()).unwrap();
            assert_eq!(
                group_args_from_proxies(&proxies, &args, Some("流媒体"))
                    .unwrap()
                    .group,
                "流媒体"
            );
            // Re-resolving with the original arguments follows GUI changes,
            // as watch does on each cycle.
            assert_eq!(
                group_args_from_proxies(&proxies, &args, Some("AI服务"))
                    .unwrap()
                    .group,
                "AI服务"
            );
            input.extend(["--group".into(), "自动选择".into()]);
            let explicit = parse_args_from(input).unwrap();
            assert_eq!(
                group_args_from_proxies(&proxies, &explicit, Some("流媒体"))
                    .unwrap()
                    .group,
                "自动选择"
            );
            for saved in [None, Some("不存在"), Some("共享-1倍率")] {
                let resolved = group_args_from_proxies(&proxies, &args, saved);
                if matches!(args.cmd, Cmd::Check | Cmd::Scan | Cmd::Nodes) {
                    assert!(resolved.unwrap().group.is_empty());
                } else {
                    assert!(resolved.is_err());
                }
            }
        }
    }

    #[test]
    fn global_scan_deduplicates_shared_nodes_and_includes_ungrouped_nodes() {
        let proxies = proxies();
        assert!(mihomo::resolve_group(&proxies, "").is_err());
        let scope = node_scope(&proxies, "").unwrap();
        let selected = nodeinfo::candidates(&proxies, &scope.members, None);
        assert_eq!(
            selected.names,
            vec!["共享-1倍率", "其他-3倍率", "未分组-1倍率"]
        );
        let economical = nodeinfo::candidates(&proxies, &scope.members, Some(1.0));
        assert_eq!(economical.names, vec!["共享-1倍率", "未分组-1倍率"]);
        assert!(!scope.is_current("共享-1倍率"));
        assert!(scope.current.is_empty());
        assert!(scope
            .current_notice
            .as_deref()
            .unwrap()
            .contains("无法唯一"));
    }

    #[test]
    fn explicit_group_still_limits_candidates_and_rejects_node_or_missing_group() {
        let proxies = proxies();
        let scope = node_scope(&proxies, "流媒体").unwrap();
        let selected = nodeinfo::candidates(&proxies, &scope.members, None);
        assert_eq!(selected.names, vec!["共享-1倍率", "其他-3倍率"]);
        assert!(node_scope(&proxies, "不存在").is_err());
        assert!(node_scope(&proxies, "共享-1倍率").is_err());
    }

    #[test]
    fn global_scan_handles_group_cycles_and_no_groups() {
        let cyclic = serde_json::from_value(serde_json::json!({
            "A": {"all":["B"], "now":"B"},
            "B": {"all":["A"], "now":"A"},
            "node": {"type":"Trojan"}
        }))
        .unwrap();
        let scope = node_scope(&cyclic, "").unwrap();
        assert!(scope.current.iter().all(|(_, now)| now.is_none()));
        assert_eq!(
            nodeinfo::candidates(&cyclic, &scope.members, None).names,
            vec!["node"]
        );
        let empty = node_scope(&std::collections::HashMap::new(), "").unwrap();
        assert!(empty.members.is_empty());
        assert!(empty.current.is_empty());
    }

    #[test]
    fn only_the_selected_root_chain_is_marked_current() {
        let mut proxies: std::collections::HashMap<String, mihomo::ProxyInfo> = serde_json::from_value(serde_json::json!({
            "GLOBAL": {"all":["VVCloud", "自动选择", "vietnam", "japan"], "now":"自动选择"},
            "VVCloud": {"all":["自动选择", "故障转移", "vietnam", "japan"], "now":"vietnam"},
            "自动选择": {"type":"URLTest", "all":["vietnam", "japan"], "now":"japan"},
            "故障转移": {"type":"Fallback", "all":["剩余流量：495 GB", "vietnam"], "now":"剩余流量：495 GB"},
            "vietnam": {"type":"Trojan"}, "japan": {"type":"Trojan"},
            "剩余流量：495 GB": {"type":"Trojan"}, "DIRECT": {"type":"Direct"}
        })).unwrap();
        let scope = node_scope_with_mode(&proxies, "", Some("rule")).unwrap();
        assert!(scope.is_current("vietnam"));
        assert!(!scope.is_current("japan"));
        assert_eq!(scope.current.len(), 1);
        let global = node_scope_with_mode(&proxies, "VVCloud", Some("global")).unwrap();
        assert!(global.is_current("japan"));
        assert!(!global.is_current("vietnam"));
        proxies.get_mut("VVCloud").unwrap().now = Some("自动选择".into());
        assert!(node_scope(&proxies, "").unwrap().is_current("japan"));
        let direct = node_scope_with_mode(&proxies, "", Some("direct")).unwrap();
        assert!(direct.current.is_empty());
        assert!(direct.current_notice.unwrap().contains("直连模式"));
        assert!(node_scope_with_mode(&proxies, "", None)
            .unwrap()
            .current
            .is_empty());
        proxies.get_mut("VVCloud").unwrap().now = Some("故障转移".into());
        let placeholder = node_scope(&proxies, "").unwrap();
        assert!(placeholder.current.is_empty());
        assert!(!placeholder.is_current("剩余流量：495 GB"));
        proxies.get_mut("VVCloud").unwrap().now = Some("DIRECT".into());
        assert!(node_scope(&proxies, "").unwrap().current.is_empty());
    }
}

#[cfg(test)]
fn is_one_rate(name: &str) -> bool {
    nodeinfo::rate_from_name(name) == Some(1.0)
}

#[cfg(test)]
mod rate_tests {
    #[test]
    fn recognizes_one_rate_without_numeric_suffix_false_positives() {
        for name in [
            "D越南1-家宽住宅IP-1倍率",
            "D美国2-网页-视频浏览-1倍率",
            "日本-1.0倍率",
            "香港-一倍率",
            "台湾-1 倍率",
        ] {
            assert!(super::is_one_rate(name), "{}", name);
        }
        for name in [
            "日本-3倍率",
            "美国-11倍率",
            "香港-0.1倍率",
            "台湾-1.5倍率",
            "新加坡-十一倍率",
            "日本1",
            "续费网址:https://getvv.cloud",
        ] {
            assert!(!super::is_one_rate(name), "{}", name);
        }
    }
}

fn cmd_fix(be: &Backend, agent: &crate::core_api::CoreApi, args: &Args) -> i32 {
    if args.dry_run {
        ui::warn("dry-run：只探测报告，不切换");
        return cmd_scan(be, agent, args);
    }
    match workflow::fix_flow(be, agent, args) {
        Ok(result) => {
            report::data(&result);
            print_commit(&result);
            0
        }
        Err(failure) => {
            report::data(&failure);
            report::error(&failure.error);
            ui::fail(&failure.to_string());
            failure.exit_code()
        }
    }
}

fn print_commit(result: &switching::CommitReport) {
    ui::ok(&format!("已确认 {} → {}", result.group, result.selected));
    if let Some(score) = &result.score {
        ui::info(&format!(
            "参考分 {:.1}/100 · 历史网页验证 {} 次{}",
            score.total,
            score.samples,
            if score.samples < 3 {
                "（样本不足，成功率/波动采用中性先验）"
            } else {
                ""
            }
        ));
        ui::dim(&format!("人为权重 50/25/15/10：成功率 {:.1}/50 · 延迟 {:.1}/25 · 稳定性 {:.1}/15 · 成本 {:.1}/10；先比较本轮验证等级，再比较参考分",score.success,score.latency,score.stability,score.cost));
    }
    match result.verification.availability {
        diagnostics::Availability::Webpage => ui::ok(&format!(
            "网页验证通过（平均 {:.2}s）",
            result.verification.mean_ms.unwrap_or_default() / 1000.0
        )),
        diagnostics::Availability::IpOnly => {
            ui::warn("仅 IP 交叉检查通过；网页仍未通过验证，浏览器访问需实际确认")
        }
        diagnostics::Availability::Failed => ui::fail("网站验证失败"),
    }
    match result.memory {
        Some(switching::MemorySync::Synced) => ui::dim("已同步当前订阅的选择记忆"),
        Some(switching::MemorySync::ClientManaged) => ui::dim("重启后的选择持久化由客户端管理"),
        Some(switching::MemorySync::NotAvailable) => {
            ui::dim("来源或订阅不可确认，未同步客户端选择记忆")
        }
        Some(switching::MemorySync::NoMatchingGroup) => {
            ui::dim("当前订阅未记录该策略组，未写入选择记忆")
        }
        None => {}
    }
    for warning in &result.warnings {
        ui::warn(&warning.to_string());
    }
}

fn cmd_use(be: &Backend, agent: &crate::core_api::CoreApi, args: &Args, node: &str) -> i32 {
    match workflow::use_flow(be, agent, args, node) {
        Ok(workflow::SelectionOutcome::DryRun { group, target }) => {
            report::data(serde_json::json!({"status":"dry_run","group":group,"target":target}));
            ui::info(&format!("dry-run：将切换 {} → {}", group, target));
            0
        }
        Ok(workflow::SelectionOutcome::Committed { result }) => {
            report::data(&result);
            print_commit(&result);
            0
        }
        Err(failure) => {
            report::data(&failure);
            report::error(&failure.error);
            ui::fail(&failure.to_string());
            failure.exit_code()
        }
    }
}

fn cmd_recovery_record(action: RecoveryAction) -> Result<(), String> {
    match action {
        RecoveryAction::Inspect => match switching::inspect_recovery()? {
            Some(record) => {
                report::data(&record);
                ui::info(&format!(
                    "恢复记录：组「{}」；{}",
                    record.group,
                    if record.committed {
                        "已提交，仅需清理记录"
                    } else {
                        "尚未完成恢复"
                    }
                ));
                ui::info(&format!(
                    "原选择：{}；上次写入：{}",
                    record.original.as_deref().unwrap_or("不可确认"),
                    record.pending.as_deref().unwrap_or(&record.expected)
                ));
                ui::dim("recover --retry 校验原客户端后尝试恢复；recover --discard 仅删除本工具记录，不切换节点或修改系统代理。");
            }
            None => {
                report::data(serde_json::json!({"pending":false}));
                ui::info("没有待恢复记录");
            }
        },
        RecoveryAction::Discard => {
            switching::discard_recovery()?;
            report::data(serde_json::json!({"discarded":true}));
            ui::info("已清理恢复记录，节点与系统代理保持现状");
        }
        RecoveryAction::Retry => unreachable!(),
    }
    Ok(())
}

fn cmd_retry_recovery(be: &Backend, api: &core_api::CoreApi) -> i32 {
    let control = runtime_switch::KernelSwitchControl { backend: be, api };
    match switching::retry_recovery(&control, &operation::Cancellation::global()) {
        Ok(report) => {
            report::data(&report);
            if let Some(error) = report.error {
                report::error(&error);
                ui::warn(&format!("恢复未完成：{}；请查看 recover 记录", error));
                return 1;
            }
            match report.status {
                switching::RollbackStatus::Restored => {
                    ui::ok("已恢复原选择与本次操作改变的代理配置");
                    0
                }
                switching::RollbackStatus::Unchanged => {
                    ui::info("原选择未变化，恢复记录已处理");
                    0
                }
                switching::RollbackStatus::SkippedConflict => {
                    ui::warn("发现外部状态变化，保留现状和恢复记录");
                    1
                }
                switching::RollbackStatus::Failed => {
                    ui::fail("恢复失败，恢复记录已保留");
                    1
                }
            }
        }
        Err(error) => {
            ui::fail(&error.to_string());
            if error.code == diagnostics::ErrorCode::Cancelled {
                130
            } else {
                1
            }
        }
    }
}

fn cmd_watch(initial: &Backend, agent: &crate::core_api::CoreApi, args: &Args) -> i32 {
    if args.dry_run {
        return match runtime_group_args(initial, agent, args) {
            Ok(resolved) => cmd_scan(initial, agent, &resolved),
            Err(error) => {
                ui::fail(&error);
                if operation::Cancellation::global().is_cancelled() {
                    130
                } else {
                    1
                }
            }
        };
    }
    let mut writer = report::EventWriter::new();
    watch::run(initial, agent, args, &mut |event| writer.emit(event))
}

/// 控制台默认 GBK 代码页会显示乱码，切到 UTF-8（对 mintty 等无副作用）
#[cfg(windows)]
fn set_console_utf8() {
    use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}

fn main() {
    report::initialize(std::env::args().any(|v| v == "--json"));
    run_main();
    exit(0);
}
fn run_main() {
    #[cfg(windows)]
    {
        CONSOLE.get_or_init(terminal::ConsoleGuard::capture);
        set_console_utf8();
    }
    if let Err(error) = operation::install_console_handler() {
        ui::fail(&error.to_string());
        exit(1);
    }
    let args = parse_args_from(std::env::args().skip(1)).unwrap_or_else(|e| {
        report::error(&diagnostics::AppError::new(
            diagnostics::ErrorCode::InvalidInput,
            diagnostics::Stage::Configuration,
        ));
        ui::fail(&e);
        exit(1)
    });
    let static_result = match &args.cmd {
        Cmd::Diagnose => Some((|| -> Result<(), String> {
            let value = diagnose::collect(&args);
            if let Some(path) = &args.output {
                diagnose::write_report(path, args.overwrite, &value).map_err(|e| {
                    report::error(&e);
                    e.to_string()
                })?;
            }
            report::data(&value);
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            Ok(())
        })()),
        Cmd::Clients(s) => Some(cmd_clients(&args, *s)),
        Cmd::Config(c) => Some(cmd_config(c, args.dry_run)),
        Cmd::History(clear) => Some(cmd_history(*clear, args.dry_run)),
        Cmd::Recover(action @ (RecoveryAction::Inspect | RecoveryAction::Discard)) => {
            Some(cmd_recovery_record(*action))
        }
        _ => None,
    };
    if let Some(result) = static_result {
        if let Err(e) = result {
            ui::fail(&e);
            exit(1);
        }
        return;
    }
    if matches!(&args.cmd, Cmd::Help) {
        print_help();
        return;
    }
    ui::banner();
    let (be, agent) = match setup(&args) {
        Ok(x) => x,
        Err(c) => exit(if operation::Cancellation::global().is_cancelled() {
            130
        } else {
            c
        }),
    };
    let args = if matches!(
        args.cmd,
        Cmd::Watch | Cmd::Info(_) | Cmd::Recover(_) | Cmd::Browse
    ) {
        args
    } else {
        runtime_group_args(&be, &agent, &args).unwrap_or_else(|e| {
            ui::fail(&e);
            exit(if operation::Cancellation::global().is_cancelled() {
                130
            } else {
                1
            })
        })
    };
    if operation::Cancellation::global().is_cancelled() {
        exit(130);
    }
    let code = match &args.cmd {
        Cmd::Recover(RecoveryAction::Retry) => cmd_retry_recovery(&be, &agent),
        Cmd::Recover(_) => unreachable!(),
        Cmd::Browse => browse::run(&be, &agent, &args),
        Cmd::Nodes => cmd_nodes(&be, &agent, &args),
        Cmd::Info(name) => cmd_info(&be, &agent, &args, name),
        Cmd::Check => cmd_check(&be, &agent, &args),
        Cmd::Scan => cmd_scan(&be, &agent, &args),
        Cmd::Fix => cmd_fix(&be, &agent, &args),
        Cmd::Use(n) => cmd_use(&be, &agent, &args, n),
        Cmd::Watch => cmd_watch(&be, &agent, &args),
        Cmd::Clients(_) | Cmd::Config(_) | Cmd::History(_) | Cmd::Diagnose => unreachable!(),
        Cmd::Help => {
            print_help();
            0
        }
    };
    exit(code);
}
