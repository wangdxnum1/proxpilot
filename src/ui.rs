use colored::Colorize;

pub fn banner() {
    println!("{}", "ProxPilot · 代理辅助工具 v0.1".cyan().bold());
    println!("{}", "节点检测 · 优选切换 · 守护运行（Clash/mihomo 系）".dimmed());
    println!();
}

pub fn ok(msg: &str) {
    println!("  {} {}", "✔".green().bold(), msg);
}

pub fn fail(msg: &str) {
    println!("  {} {}", "✘".red().bold(), msg);
}

pub fn warn(msg: &str) {
    println!("  {} {}", "▲".yellow().bold(), msg);
}

pub fn info(msg: &str) {
    println!("  {}", msg.cyan());
}

pub fn dim(msg: &str) {
    println!("  {}", msg.dimmed());
}
