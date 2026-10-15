//! scan 的列布局与终端宽度适配；先计算显示宽度，再应用 ANSI 样式。

use colored::Colorize;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const DELAY_WIDTH: usize = 8;
const RANK_WIDTH: usize = 2;

#[derive(Clone, Copy, Default)]
enum Style {
    #[default]
    Plain,
    Dim,
    Fast,
    Economical,
    Current,
    Dedicated,
    Residential,
}

impl Style {
    fn paint(self, text: &str) -> String {
        match self {
            Self::Plain => text.normal(),
            Self::Dim => text.dimmed(),
            Self::Fast => text.green().bold(),
            Self::Economical => text.green(),
            Self::Current => text.yellow().bold(),
            Self::Dedicated => text.magenta().bold(),
            Self::Residential => text.blue(),
        }
        .to_string()
    }
}

struct Span(String, Style);

fn span(text: impl Into<String>, style: Style) -> Span {
    Span(text.into(), style)
}

/// 不截断名称，也不拆开旗帜、组合音标或 ZWJ emoji。
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if used > 0 && used + size > width {
            lines.push(String::new());
            used = 0;
        }
        lines.last_mut().unwrap().push_str(grapheme);
        used += size;
    }
    lines
}

fn wrapped_spans(spans: &[Span], width: usize, indent: usize) -> String {
    let mut result = " ".repeat(indent);
    let mut used = indent;
    for Span(text, style) in spans {
        // 短标签整体换行，避免把“当前”“家宽”等拆成两行。
        let span_width = text.width();
        if used > indent && span_width <= width.saturating_sub(indent) && used + span_width > width
        {
            result.push('\n');
            result.push_str(&" ".repeat(indent));
            used = indent;
        }
        let mut chunk = String::new();
        for grapheme in text.graphemes(true) {
            let size = UnicodeWidthStr::width(grapheme);
            if used > indent && used + size > width {
                result.push_str(&style.paint(&chunk));
                chunk.clear();
                result.push('\n');
                result.push_str(&" ".repeat(indent));
                used = indent;
            }
            chunk.push_str(grapheme);
            used += size;
        }
        result.push_str(&style.paint(&chunk));
    }
    result.push('\n');
    result
}

fn pad(text: &str, width: usize) -> String {
    format!("{}{}", text, " ".repeat(width.saturating_sub(text.width())))
}

fn pad_start(text: &str, width: usize) -> String {
    format!("{}{}", " ".repeat(width.saturating_sub(text.width())), text)
}

pub fn terminal_width() -> usize {
    use std::io::{self, IsTerminal};
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
    };
    if io::stdout().is_terminal() {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } != 0 {
            // 留一列，避免控制台恰好填满后自动换行产生空行。
            return (info.srWindow.Right - info.srWindow.Left).max(1) as usize;
        }
    }
    100
}

pub struct Row<'a> {
    pub delay: i64,
    pub name: &'a str,
    pub current: bool,
    pub rank: Option<usize>,
    pub rate: Option<(f64, bool)>,
    pub protocol: &'a str,
}

fn protocol_name(protocol: &str) -> &str {
    match protocol.trim() {
        "Shadowsocks" => "SS",
        "ShadowsocksR" => "SSR",
        "" => "?",
        other => other,
    }
}

pub fn header(width: usize, economical: bool) -> String {
    // 单位只在表头出现，表头与数值共享列宽及右边界。
    let delay = pad_start("延迟(ms)", DELAY_WIDTH);
    let text = if width < 76 && economical {
        "节点 / 延迟 · 协议".into()
    } else if width < 76 {
        "排名 · 节点 / 延迟 · 倍率 · 协议".into()
    } else if economical {
        format!("  {}  {}  节点", delay, pad("协议", 10))
    } else {
        format!(
            "  {}  {}  {}  {}  节点",
            pad_start("#", RANK_WIDTH),
            delay,
            pad("倍率", 6),
            pad("协议", 10)
        )
    };
    wrapped_spans(&[span(text, Style::Dim)], width, 2)
}

pub fn row(row: &Row<'_>, width: usize, economical: bool) -> String {
    let protocol = protocol_name(row.protocol);
    let rate = row
        .rate
        .map(|(r, _)| format!("{r}×"))
        .unwrap_or_else(|| "?".into());
    let rate_style = if row.rate.is_some_and(|(r, _)| r == 1.0) {
        Style::Economical
    } else {
        Style::Plain
    };
    let delay_style = if row.delay <= 200 {
        Style::Fast
    } else {
        Style::Plain
    };
    let name_style = if row.current {
        Style::Current
    } else {
        Style::Plain
    };
    let marker = if row.current { "▶" } else { " " };
    let rank = row.rank.map(|n| n.to_string()).unwrap_or_default();
    let compact = width < 76 || protocol.width() > 10 || rate.width() > 6;
    let mut result = String::new();
    let name_indent;
    if compact {
        let prefix = format!("  {marker} {rank:>RANK_WIDTH$}  ");
        name_indent = prefix.width();
        result.push_str(&name_style.paint(&prefix));
    } else {
        let mut prefix = vec![span(format!("  {marker} "), name_style)];
        if !economical {
            prefix.push(span(format!("{rank:>RANK_WIDTH$}  "), Style::Dim));
        }
        prefix.push(span(format!("{:>DELAY_WIDTH$}  ", row.delay), delay_style));
        if !economical {
            prefix.push(span(format!("{}  ", pad(&rate, 6)), rate_style));
        }
        prefix.push(span(format!("{}  ", pad(protocol, 10)), Style::Dim));
        name_indent = prefix.iter().map(|s| s.0.width()).sum();
        for Span(text, style) in prefix {
            result.push_str(&style.paint(&text));
        }
    }
    let name_lines = wrap_text(row.name, width.saturating_sub(name_indent));
    for (index, line) in name_lines.iter().enumerate() {
        if index > 0 {
            result.push_str(&" ".repeat(name_indent));
        }
        result.push_str(&name_style.paint(line));
        result.push('\n');
    }

    let mut details = Vec::new();
    let mut add = |text: String, style| {
        if !details.is_empty() {
            details.push(span(" · ", Style::Dim));
        }
        details.push(span(text, style));
    };
    if compact {
        add(format!("{} ms", row.delay), delay_style);
        if !economical {
            add(rate, rate_style);
        }
        add(protocol.into(), Style::Dim);
    }
    for label in crate::nodeinfo::route_labels(row.name) {
        let style = match label {
            "专线" => Style::Dedicated,
            "家宽" => Style::Residential,
            _ => Style::Dim,
        };
        add(label.into(), style);
    }
    if row.rate.is_some_and(|(_, hint)| hint) {
        add("倍率来源：订阅提示".into(), Style::Dim);
    }
    if row.current {
        add("当前".into(), Style::Current);
    }
    if !details.is_empty() {
        let last_name_width = name_lines.last().map_or(0, |line| line.width());
        let details_width: usize = details.iter().map(|s| s.0.width()).sum();
        if !compact && name_indent + last_name_width + 2 + details_width <= width {
            result.pop(); // 将标签接到名称的最后一行，仅在容纳不下时另起一行。
            result.push_str("  ");
            for Span(text, style) in &details {
                result.push_str(&style.paint(text));
            }
            result.push('\n');
        } else {
            result.push_str(&wrapped_spans(&details, width, name_indent));
        }
    }
    result
}

pub fn current(group: &str, name: Option<&str>, status: &str, width: usize) -> String {
    let mut result = wrapped_spans(
        &[
            span("▶ 当前节点", Style::Current),
            span(format!(" · {group}"), Style::Dim),
        ],
        width,
        2,
    );
    if let Some(name) = name {
        result.push_str(&wrapped_spans(&[span(name, Style::Current)], width, 4));
    }
    result.push_str(&wrapped_spans(&[span(status, Style::Dim)], width, 4));
    result.push('\n');
    result
}

pub fn note(text: &str, width: usize) {
    print!("{}", wrapped_spans(&[span(text, Style::Dim)], width, 2));
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn wrapping_preserves_full_names_and_graphemes_within_terminal_width() {
        let name = "🇭🇰香港👨‍👩‍👧‍👦-极纯家宽IP-e\u{301}-1倍率";
        let lines = wrap_text(name, 12);
        assert!(
            lines.len() > 1,
            "a long name must wrap in a narrow terminal"
        );
        assert_eq!(lines.concat(), name, "wrapping must not truncate names");
        for line in &lines {
            assert!(UnicodeWidthStr::width(line.as_str()) <= 12);
        }
        assert!(lines.iter().any(|line| line.contains("👨‍👩‍👧‍👦")));
        assert!(lines.iter().any(|line| line.contains("e\u{301}")));
    }

    #[test]
    fn narrow_and_wide_rows_keep_names_current_and_billing_provenance() {
        let name = "🇭🇰专线A1-香港4-极纯家宽IP支持Cityline流媒体全解锁-1倍率";
        let sample = Row {
            delay: 50,
            name,
            current: true,
            rank: Some(34),
            rate: Some((1.0, true)),
            protocol: "Shadowsocks",
        };
        for width in [40, 60, 76, 100, 160] {
            for economical in [false, true] {
                let output = row(&sample, width, economical);
                // 测试兼容外部强制开启颜色的环境。
                let mut plain = String::new();
                let mut escape = false;
                for c in output.chars() {
                    if c == '\x1b' {
                        escape = true;
                    } else if escape {
                        if c == 'm' {
                            escape = false;
                        }
                    } else {
                        plain.push(c);
                    }
                }
                for line in plain.lines() {
                    assert!(line.width() <= width, "width={width}: {line}");
                }
                let joined: String = plain.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(joined.contains(name), "full name must survive wrapping");
                assert!(plain.contains("当前"));
                assert!(joined.contains("倍率来源：订阅提示"));
                assert!(plain.contains("专线"));
                assert!(plain.contains("家宽"));
                assert!(plain.contains("SS"));
            }
        }
    }
}
