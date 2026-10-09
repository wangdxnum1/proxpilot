mod appstate;
mod checker;
mod client_config;
mod clients;
mod core_api;
mod detect;
mod http;
mod mihomo;
mod nodeinfo;
mod procinfo;
mod sysproxy;
mod ui;

use std::process::exit;
use std::thread;
use std::time::{Duration, Instant};

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
    pub dry_run: bool,
    pub interval: u64,
    pub reopt: u64,
    pub optimize: bool,
    pub detect: bool,
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
        dry_run: false,
        interval: 300,
        reopt: 7200,
        optimize: false,
        detect: false,
    };
    let mut command = false;
    let mut supported = false;
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
            "--dry-run" => args.dry_run = true,
            "--detect" => args.detect = true,
            "--supported" => supported = true,
            "--help" | "-h" | "help" => {
                args.cmd = Cmd::Help;
                return Ok(args);
            }
            "check" | "scan" | "fix" | "use" | "watch" | "clients" | "config" | "nodes"
            | "info"
                if !command =>
            {
                command = true;
                args.cmd = match arg.as_str() {
                    "nodes" => Cmd::Nodes,
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
            _ => return Err(format!("未知命令或参数：{}", arg)),
        }
    }
    if supported {
        if matches!(args.cmd, Cmd::Clients(_)) {
            args.cmd = Cmd::Clients(true);
        } else {
            return Err("--supported 仅用于 clients 命令".into());
        }
    }
    if args.samples == 0 || args.interval == 0 || args.reopt == 0 {
        return Err("samples、interval、reopt 必须大于 0".into());
    }
    Ok(args)
}

fn cmd_clients(supported: bool) -> Result<(), String> {
    if supported {
        println!("cutecloud    CuteCloud    HTTP API；支持选择记忆同步");
        println!("clash-verge  Clash Verge  命名管道 / HTTP API；仅同步当前内核选择");
        println!("vvcloud     VVCloud      HTTP API；支持选择记忆同步");
        println!("auto         自动选择    系统代理开启时要求唯一匹配；关闭时要求唯一可用内核");
        return Ok(());
    }
    let api = core_api::CoreApi::new()?;
    let records = clients::discover(&api, &clients::DiscoveryPaths::current()?);
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
fn cmd_config(command: &ConfigCommand) -> Result<(), String> {
    let path = client_config::config_path()?;
    match command {
        ConfigCommand::Show => {}
        ConfigCommand::Set(c) => client_config::save_default(&path, Some(*c))?,
        ConfigCommand::Unset => client_config::save_default(&path, None)?,
    }
    println!("{}", client_config::show_config(&path)?);
    Ok(())
}
#[cfg(test)]
mod cli_tests {
    use super::*;
    fn parse(values: &[&str]) -> Result<Args, String> {
        parse_args_from(values.iter().map(|v| v.to_string()))
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
    println!("  clients [--supported]  查看发现的客户端 / 支持的类型");
    println!("  config show | set default-client <类型> | unset default-client");
    println!("  nodes          列出组内真实节点的协议、倍率和名称标签（不测速）");
    println!("  info <节点>    查看节点配置详情及信息来源（不显示凭据）");
    println!("  check          体检：内核 / 系统代理 / 当前节点能否访问测试网址");
    println!("  scan           探测组内所有节点的可达性（不切换）");
    println!("  fix            优选：探测 → 真实验证 → 切到实测最快的节点");
    println!("  use <节点>     手动切换到指定节点并验证");
    println!("  watch          守护模式：保持可用节点，坏了自动修；加 --optimize 主动优选");
    println!();
    println!("选项:");
    println!("  --group <名称>   指定策略组（默认优先读取客户端当前组并校验内核）");
    println!("  --url <地址>     测试网址（默认 https://chatgpt.com/）");
    println!("  --client <类型>  cutecloud / clash-verge / vvcloud / auto；覆盖保存的默认客户端");
    println!("  --api <地址>     显式指定 HTTP 内核 API（优先级最高）");
    println!("  --secret <值>    内核 API 的 secret");
    println!("  --proxy <地址>   代理出口（默认读目标内核运行端口）");
    println!("  --max-rate <N>  倍率上限；未知倍率排除，scan/fix/watch/nodes/use 生效");
    println!("  --top <N>        优选时验证前 N 个候选（默认 12）");
    println!("  --samples <N>    每个节点实测次数（默认 2，全部 200 才通过）");
    println!("  --interval <秒>  watch 检查间隔（默认 300）");
    println!("  --optimize      watch 启动时及定时主动优选（默认关闭）");
    println!("  --reopt <秒>     --optimize 的定时优选间隔（默认 7200）");
    println!("  --dry-run        只探测报告，不切换");
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
        for (group, selected) in &self.current {
            ui::info(&format!(
                "组「{}」{}",
                group,
                scan_current_status(selected.as_deref(), members, reachable)
            ));
        }
        if let Some(notice) = &self.current_notice {
            ui::dim(notice);
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

    if sysproxy::matches(&be.proxy) {
        ui::ok("系统代理已开启");
    } else {
        ui::warn("系统代理未开启或指向其他客户端（浏览器不走代理）——其他代理软件退出时常干这事");
        if args.dry_run {
            ui::dim("dry-run：不自动修复");
        } else {
            match sysproxy::enable_for(&be.proxy) {
                Ok(()) => ui::ok("已自动重新打开系统代理"),
                Err(e) => ui::fail(&format!("自动打开失败：{}", e)),
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
            match checker::verify_access(&be.proxy, &args.url, args.samples) {
                checker::Verdict::Pass(t) => {
                    ui::ok(&format!("{} 可访问（平均 {:.2}s）", args.url, t));
                    0
                }
                checker::Verdict::PassIpOnly => {
                    ui::ok(&format!("{} 可访问（IP 检查通过）", args.url));
                    ui::dim("说明：网页返回 HTTP 403；API 检查通过，但浏览器访问仍需确认");
                    0
                }
                checker::Verdict::Fail(c) => {
                    let cs = if c == 0 {
                        "连接或传输失败".to_string()
                    } else {
                        format!("HTTP {}", c)
                    };
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
        "{}共 {} 个真实节点（去重后），{} 路并发探测...",
        scope.label,
        members.len(),
        checker::scan_concurrency(members.len())
    ));
    let reachable = checker::scan_reachable(be, agent, &members, &args.url);
    println!();
    scope.print_current(&members, &reachable);
    if reachable.is_empty() {
        ui::fail("没有任何节点可达");
        return 2;
    }
    ui::ok(&format!(
        "可达 {}/{}（注意：任何 HTTP 响应都算可达，IP 被风控的 403 也包含）",
        reachable.len(),
        members.len()
    ));
    ui::info(&format!(
        "可达排行：显示前 {} / {} 个（按延迟排序）",
        reachable.len().min(30),
        reachable.len()
    ));
    for (d, n) in reachable.iter().take(30) {
        print_scan_row(*d, n, scope.is_current(n).then_some(n.as_str()), &policy);
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
    for (delay, name) in economical {
        print_scan_row(
            *delay,
            name,
            scope.is_current(name).then_some(name.as_str()),
            &policy,
        );
    }
    println!(
        "  标识：{} = 探测延迟 ≤ 200ms；{} = 名称或订阅提示识别为1倍率；{} = 当前选中节点",
        "[低延迟]".cyan().bold(),
        "[1倍率·省流量]".green().bold(),
        "[当前]".yellow()
    );
    ui::dim("提示：可达 ≠ 能打开网页，IP 是否被风控要用 proxpilot fix 实测验证");
    0
}

fn scan_current_status(
    current: Option<&str>,
    members: &[String],
    reachable: &[(i64, String)],
) -> String {
    let Some(name) = current else {
        return "[当前] 内核未提供当前选择".into();
    };
    let status = if let Some((rank, (delay, _))) =
        reachable.iter().enumerate().find(|(_, (_, n))| n == name)
    {
        format!("{}ms · 可达排名 {}/{}", delay, rank + 1, reachable.len())
    } else if members.iter().any(|n| n == name) {
        "本轮探测未成功（超时或请求失败，不等于客户端一定不可用）".into()
    } else {
        "未参与本轮探测（不在筛选后的真实节点候选中）".into()
    };
    format!("[当前] {} · {}（扫描开始时的选择）", name, status)
}

#[cfg(test)]
mod scan_current_tests {
    use super::*;
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

fn print_scan_row(delay: i64, name: &str, current: Option<&str>, policy: &nodeinfo::BillingPolicy) {
    let row = format!(
        "      ✔ {:>5}ms  {}{}",
        delay,
        name,
        if delay <= 200 { "  [低延迟]" } else { "" }
    );
    let row = if delay <= 200 {
        row.cyan().bold()
    } else {
        row.normal()
    };
    let economical = if policy.resolve(name).is_some_and(|(rate, _)| rate == 1.0) {
        format!("  {}", "[1倍率·省流量]".green().bold())
    } else {
        String::new()
    };
    let selected = if current == Some(name) {
        format!("  {}", "[当前]".yellow())
    } else {
        String::new()
    };
    let provenance = match policy.resolve(name) {
        Some((rate, true)) => format!("  [{}倍率·订阅提示]", rate),
        _ => String::new(),
    };
    println!("{}{}{}{}", row, economical, provenance, selected);
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
        ui::warn("dry-run 模式：只探测报告，不切换");
        return cmd_scan(be, agent, args);
    }
    if !checker::core_alive(&be.proxy) {
        ui::fail("内核代理端口不通，请确认客户端已启动；若使用的不是 CuteCloud，加 --detect 自动探测或用 --api 指定");
        return 1;
    }
    if sysproxy::matches(&be.proxy) {
        ui::ok("系统代理已开启");
    } else {
        ui::warn("系统代理未开启或指向其他客户端，自动重新打开");
        match sysproxy::enable_for(&be.proxy) {
            Ok(()) => ui::ok("系统代理已开启"),
            Err(e) => ui::fail(&format!("打开失败：{}", e)),
        }
    }
    println!();
    match checker::fix_flow(be, agent, args) {
        Ok(_) => {
            match checker::verify_access(&be.proxy, &args.url, 1) {
                checker::Verdict::Pass(t) => {
                    ui::ok(&format!("最终确认：{} → HTTP 200（{:.2}s）", args.url, t))
                }
                checker::Verdict::PassIpOnly => {
                    ui::ok(&format!("最终确认：{} 可访问（IP 检查通过）", args.url))
                }
                checker::Verdict::Fail(c) => {
                    ui::warn(&format!("最终确认异常（HTTP {}），可重跑 proxpilot fix", c))
                }
            }
            0
        }
        Err(e) => {
            ui::fail(&e);
            2
        }
    }
}

fn cmd_use(be: &Backend, agent: &crate::core_api::CoreApi, args: &Args, node: &str) -> i32 {
    if node.is_empty() {
        ui::fail("用法: proxpilot use <节点名>（节点名含空格请加引号）");
        return 1;
    }
    let proxies = match mihomo::get_proxies(be, agent) {
        Ok(p) => p,
        Err(e) => {
            ui::fail(&e);
            return 1;
        }
    };
    if !proxies
        .get(&args.group)
        .and_then(|p| p.all.as_ref())
        .is_some_and(|members| members.iter().any(|n| n == node))
        || !proxies
            .get(node)
            .is_some_and(|p| checker::is_real_node(p, node))
    {
        ui::fail("指定节点不是该策略组中的真实节点");
        return 1;
    }
    let policy = nodeinfo::BillingPolicy::for_backend(be, &proxies);
    policy.report();
    if !policy.permitted(node, args.max_rate) {
        ui::fail("指定节点超过倍率上限或倍率未知；未切换");
        return 1;
    }
    if args.dry_run {
        ui::info(&format!("dry-run：将切换 {} → {}", args.group, node));
        return 0;
    }
    match mihomo::switch_group(be, agent, &args.group, node) {
        Ok(()) => {
            ui::ok(&format!("已切换 {} → {}", args.group, node));
            appstate::sync_selection(be, &args.group, node);
            match checker::verify_access(&be.proxy, &args.url, args.samples) {
                checker::Verdict::Pass(t) => {
                    ui::ok(&format!("验证通过：{} 平均 {:.2}s", args.url, t));
                    0
                }
                checker::Verdict::PassIpOnly => {
                    ui::ok(&format!(
                        "验证通过：{} IP 检查通过（浏览器访问仍需确认）",
                        args.url
                    ));
                    0
                }
                checker::Verdict::Fail(c) => {
                    ui::warn(&format!("切换成功但 {} 未通过验证（HTTP {}）", args.url, c));
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

fn reoptimization_due(enabled: bool, elapsed: Option<Duration>, interval: u64) -> bool {
    enabled && elapsed.map_or(true, |elapsed| elapsed.as_secs() >= interval)
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    #[test]
    fn default_watch_never_proactively_optimizes() {
        for elapsed in [
            None,
            Some(Duration::ZERO),
            Some(Duration::from_secs(86_400)),
        ] {
            assert!(!reoptimization_due(false, elapsed, 7200));
            assert!(!reoptimization_due(false, elapsed, 0));
        }
    }

    #[test]
    fn enabled_watch_optimizes_at_startup_and_after_interval() {
        assert!(reoptimization_due(true, None, 7200));
        assert!(!reoptimization_due(true, Some(Duration::ZERO), 7200));
        assert!(!reoptimization_due(
            true,
            Some(Duration::from_secs(7199)),
            7200
        ));
        assert!(reoptimization_due(
            true,
            Some(Duration::from_secs(7200)),
            7200
        ));
        assert!(reoptimization_due(
            true,
            Some(Duration::from_secs(7201)),
            7200
        ));
    }
}

fn cmd_watch(initial: &Backend, agent: &crate::core_api::CoreApi, args: &Args) -> i32 {
    if args.optimize {
        ui::info(&format!(
            "守护模式：每 {} 秒检查一次，启动时及每 {} 秒主动优选，Ctrl+C 退出",
            args.interval, args.reopt
        ));
    } else {
        ui::info(&format!(
            "守护模式：每 {} 秒检查一次，保持可用节点，仅故障时优选，Ctrl+C 退出",
            args.interval
        ));
    }
    if args.dry_run {
        return match runtime_group_args(initial, agent, args) {
            Ok(resolved) => cmd_scan(initial, agent, &resolved),
            Err(e) => {
                ui::fail(&e);
                1
            }
        };
    }
    let mut current = initial.clone();
    let mut last_opt: Option<Instant> = None;
    let mut cycle: u64 = 0;
    loop {
        cycle += 1;
        match clients::refresh(&current, args, agent) {
            Ok(next) => current = next,
            Err(e) => {
                ui::warn(&format!("目标客户端暂不可用：{}", e));
                thread::sleep(Duration::from_secs(args.interval));
                continue;
            }
        }
        let be = &current;
        let proxies = match mihomo::get_proxies(be, agent) {
            Ok(proxies) => proxies,
            Err(e) => {
                ui::warn(&format!("读取实时策略组失败，等待重试：{}", e));
                thread::sleep(Duration::from_secs(args.interval));
                continue;
            }
        };
        let saved = if args.group.is_empty() {
            appstate::current_group(be)
        } else {
            None
        };
        let cycle_args = match group_args_from_proxies(&proxies, args, saved.as_deref()) {
            Ok(resolved) => resolved,
            Err(e) => {
                ui::fail(&e);
                return 1;
            }
        };
        let args = &cycle_args;
        if !checker::core_alive(&be.proxy) {
            ui::warn("内核未运行，等待中...");
            thread::sleep(Duration::from_secs(args.interval));
            continue;
        }
        let policy = nodeinfo::BillingPolicy::for_backend(be, &proxies);
        if let Some(current_node) = proxies.get(&args.group).and_then(|p| p.now.as_deref()) {
            if !policy.permitted(current_node, args.max_rate) {
                ui::warn("当前选择不符合倍率上限；默认保持可用节点，下次故障修复或主动优选只使用合规候选");
            }
        }
        if !sysproxy::matches(&be.proxy) {
            ui::warn("系统代理被关闭或指向其他客户端，自动重新打开");
            let _ = sysproxy::enable_for(&be.proxy);
        }
        let v1 = checker::verify_access(&be.proxy, &args.url, 1);
        let mut broken = !v1.is_ok();
        let mut last_code = match &v1 {
            checker::Verdict::Fail(c) => *c,
            _ => 200,
        };
        if broken {
            // 5 秒后复测一次，避免偶发误报
            thread::sleep(Duration::from_secs(5));
            let v2 = checker::verify_access(&be.proxy, &args.url, 1);
            broken = !v2.is_ok();
            if let checker::Verdict::Fail(c) = v2 {
                last_code = c;
            }
        }
        let due = reoptimization_due(args.optimize, last_opt.map(|t| t.elapsed()), args.reopt);
        if broken || due {
            let reason = if broken {
                format!("检测到 {} 不可访问（HTTP {}）", args.url, last_code)
            } else {
                if last_opt.is_none() {
                    "启动主动优选".to_string()
                } else {
                    "到达定时优选时间".to_string()
                }
            };
            ui::warn(&format!("[周期 {}] {}，开始优选...", cycle, reason));
            if let Err(e) = checker::fix_flow(be, agent, args) {
                ui::fail(&e);
            }
            last_opt = Some(Instant::now());
        } else {
            ui::dim(&format!("[周期 {}] 正常（HTTP 200）", cycle));
        }
        thread::sleep(Duration::from_secs(args.interval));
    }
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
    #[cfg(windows)]
    set_console_utf8();
    let args = parse_args_from(std::env::args().skip(1)).unwrap_or_else(|e| {
        ui::fail(&e);
        exit(1)
    });
    let static_result = match &args.cmd {
        Cmd::Clients(s) => Some(cmd_clients(*s)),
        Cmd::Config(c) => Some(cmd_config(c)),
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
        Err(c) => exit(c),
    };
    let args = if matches!(args.cmd, Cmd::Watch | Cmd::Info(_)) {
        args
    } else {
        runtime_group_args(&be, &agent, &args).unwrap_or_else(|e| {
            ui::fail(&e);
            exit(1)
        })
    };
    let code = match &args.cmd {
        Cmd::Nodes => cmd_nodes(&be, &agent, &args),
        Cmd::Info(name) => cmd_info(&be, &agent, &args, name),
        Cmd::Check => cmd_check(&be, &agent, &args),
        Cmd::Scan => cmd_scan(&be, &agent, &args),
        Cmd::Fix => cmd_fix(&be, &agent, &args),
        Cmd::Use(n) => cmd_use(&be, &agent, &args, n),
        Cmd::Watch => cmd_watch(&be, &agent, &args),
        Cmd::Clients(_) | Cmd::Config(_) => unreachable!(),
        Cmd::Help => {
            print_help();
            0
        }
    };
    exit(code);
}
