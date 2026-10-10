//! Safe node metadata and name-based billing policy. Credentials are never deserialized.
use crate::{detect::Backend, mihomo::ProxyInfo};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

fn adjacent_numeric_expression(before: &str) -> bool {
    let mut chars = before.trim_end().chars().rev().peekable();
    let mut separators = String::new();
    while chars
        .peek()
        .is_some_and(|c| c.is_whitespace() || "/\\~-+eExX×～至–—".contains(*c))
    {
        separators.push(chars.next().unwrap());
    }
    if separators.is_empty()
        || !chars.peek().is_some_and(|c| {
            c.is_ascii_digit() || *c == '.' || "零一二三四五六七八九十百千万".contains(*c)
        })
    {
        return false;
    }
    // 分式、指数等表达式一律拒绝；单个连字符可能只是“日本2-3倍率”的节点编号分隔符。
    if separators
        .chars()
        .any(|c| !c.is_whitespace() && !"-–—".contains(c))
    {
        return true;
    }
    while chars.peek().is_some_and(|c| {
        c.is_ascii_digit() || *c == '.' || "零一二三四五六七八九十百千万".contains(*c)
    }) {
        chars.next();
    }
    chars
        .next()
        .is_none_or(|c| c.is_whitespace() || "-–—([（【".contains(c))
}

pub fn rate_from_name(name: &str) -> Option<f64> {
    let mut result = None;
    for (index, _) in name.match_indices("倍率") {
        let prefix = name[..index].trim_end();
        let rate = if let Some(before) = prefix.strip_suffix('一') {
            if adjacent_numeric_expression(before) {
                return None;
            }
            if before
                .chars()
                .next_back()
                .is_some_and(|c| "零一二三四五六七八九十百千万".contains(c))
            {
                return None;
            }
            1.0
        } else {
            let number: String = prefix
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if adjacent_numeric_expression(prefix.strip_suffix(&number).unwrap_or(prefix)) {
                return None;
            }
            number.parse::<f64>().ok()?
        };
        if !rate.is_finite() || rate <= 0.0 || result.is_some_and(|old| old != rate) {
            return None;
        }
        result = Some(rate);
    }
    result
}
#[cfg(test)]
pub fn permitted(name: &str, max_rate: Option<f64>) -> bool {
    max_rate.is_none_or(|max| rate_from_name(name).is_some_and(|rate| rate <= max))
}

#[cfg(test)]
mod ambiguous_rate_tests {
    #[test]
    fn ratio_range_and_exponent_labels_are_unknown() {
        for name in [
            "node-3/1倍率",
            "node-1e3倍率",
            "node-1e-3倍率",
            "node-3~1倍率",
            "node-3至1倍率",
            "node-3-1倍率",
            "node-3/一倍率",
        ] {
            assert_eq!(super::rate_from_name(name), None, "{}", name);
            assert!(!super::permitted(name, Some(3.0)), "{}", name);
        }
    }
}
#[derive(Default)]
pub struct BillingPolicy {
    rules: HashMap<String, Option<f64>>,
}

impl BillingPolicy {
    pub fn for_backend(be: &Backend, proxies: &HashMap<String, ProxyInfo>) -> Self {
        if be.kind != Some(crate::client_config::ClientKind::CuteCloud) {
            return Self::default();
        }
        Self::from_hints(
            proxies
                .iter()
                .filter(|(name, info)| info.all.is_none() && name.contains("倍率提示"))
                .map(|(name, _)| name.as_str()),
        )
    }

    fn from_hints<'a>(hints: impl IntoIterator<Item = &'a str>) -> Self {
        let mut policy = Self::default();
        for hint in hints {
            for part in hint.split(['|', '｜']).skip(1) {
                let part = part.trim();
                let Some((route, number)) = part.split_once(['x', 'X', '×']) else {
                    continue;
                };
                let route = route.trim();
                if !matches!(route, "直连" | "中转" | "专线") {
                    continue;
                }
                let number = number.trim();
                let rate = if !number.is_empty()
                    && number.chars().all(|c| c.is_ascii_digit() || c == '.')
                {
                    number
                        .parse::<f64>()
                        .ok()
                        .filter(|r| r.is_finite() && *r > 0.0)
                } else {
                    None
                };
                policy
                    .rules
                    .entry(route.into())
                    .and_modify(|old| {
                        if *old != rate {
                            *old = None;
                        }
                    })
                    .or_insert(rate);
            }
        }
        policy
    }

    /// The bool identifies subscription-derived rates, so UI can show provenance.
    pub fn resolve(&self, name: &str) -> Option<(f64, bool)> {
        if let Some(rate) = rate_from_name(name) {
            return Some((rate, false));
        }
        // An invalid/conflicting explicit rate must not be masked by a route hint.
        if name.contains("倍率") {
            return None;
        }
        let mut routes = std::collections::HashSet::new();
        for part in name.split(['|', '｜']).skip(1).map(str::trim) {
            if matches!(part, "直连" | "中转" | "专线") {
                routes.insert(part);
            }
        }
        if routes.len() != 1 {
            return None;
        }
        self.rules
            .get(*routes.iter().next()?)?
            .map(|rate| (rate, true))
    }

    pub fn permitted(&self, name: &str, max_rate: Option<f64>) -> bool {
        max_rate.is_none_or(|max| self.resolve(name).is_some_and(|(rate, _)| rate <= max))
    }

    pub fn label(&self, name: &str) -> String {
        self.resolve(name)
            .map(|(rate, hint)| {
                format!(
                    "{}倍率（{}）",
                    rate,
                    if hint {
                        "订阅倍率提示，按线路标签匹配"
                    } else {
                        "名称"
                    }
                )
            })
            .unwrap_or_else(|| "倍率未知".into())
    }

    pub fn report(&self) {
        if self.rules.is_empty() {
            return;
        }
        let mut rules: Vec<_> = self
            .rules
            .iter()
            .map(|(route, rate)| {
                format!(
                    "{} {}",
                    route,
                    rate.map(|r| format!("{}倍率", r))
                        .unwrap_or("未知/冲突".into())
                )
            })
            .collect();
        rules.sort();
        crate::ui::dim(&format!(
            "CuteCloud 订阅倍率提示：{}；仅匹配明确线路标签，未注明类型保持未知",
            rules.join("、")
        ));
    }
}

pub struct Candidates {
    pub names: Vec<String>,
    pub unknown: usize,
    pub over_limit: usize,
    pub placeholders: usize,
}
#[cfg(test)]
pub fn candidates(
    proxies: &HashMap<String, ProxyInfo>,
    members: &[String],
    max_rate: Option<f64>,
) -> Candidates {
    candidates_with_policy(proxies, members, max_rate, &BillingPolicy::default())
}
pub fn candidates_with_policy(
    proxies: &HashMap<String, ProxyInfo>,
    members: &[String],
    max_rate: Option<f64>,
    policy: &BillingPolicy,
) -> Candidates {
    let mut result = Candidates {
        names: vec![],
        unknown: 0,
        over_limit: 0,
        placeholders: 0,
    };
    let mut seen = std::collections::HashSet::new();
    for name in members {
        if !seen.insert(name) {
            continue;
        }
        let Some(info) = proxies.get(name) else {
            continue;
        };
        if !crate::checker::is_real_node(info, name) {
            if info.all.is_none() {
                result.placeholders += 1;
            }
            continue;
        }
        if let Some(max) = max_rate {
            match policy.resolve(name).map(|(rate, _)| rate) {
                None => {
                    result.unknown += 1;
                    continue;
                }
                Some(rate) if rate > max => {
                    result.over_limit += 1;
                    continue;
                }
                _ => {}
            }
        }
        result.names.push(name.clone());
    }
    result
}
pub fn report_candidates(selected: &Candidates, max_rate: Option<f64>) {
    if let Some(max) = max_rate {
        crate::ui::info(&format!(
            "倍率上限 {}（依据名称或已识别的订阅提示）：候选 {}；排除超限 {}、未知倍率 {}",
            max,
            selected.names.len(),
            selected.over_limit,
            selected.unknown
        ));
    }
    if selected.placeholders > 0 {
        crate::ui::dim(&format!(
            "已排除 {} 个订阅/套餐信息条目",
            selected.placeholders
        ));
    }
}

#[derive(Clone, Default, Deserialize)]
pub struct NodeConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub protocol: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub tls: Option<bool>,
    #[serde(default)]
    pub udp: Option<bool>,
    #[serde(default)]
    pub cipher: Option<String>,
    #[serde(default, rename = "servername", alias = "sni")]
    pub sni: Option<String>,
}
#[derive(Deserialize, Default)]
struct Provider {
    #[serde(default)]
    path: Option<String>,
}
#[derive(Deserialize, Default)]
struct Document {
    #[serde(default)]
    proxies: Vec<NodeConfig>,
    #[serde(default, rename = "proxy-providers")]
    providers: HashMap<String, Provider>,
}
pub struct Metadata {
    pub nodes: HashMap<String, (NodeConfig, String)>,
    pub notices: Vec<String>,
}
fn read_document(path: &Path) -> Result<Document, String> {
    let bytes = std::fs::read(path).map_err(|_| format!("无法读取节点配置 {}", path.display()))?;
    serde_yaml_ng::from_slice(&bytes)
        .map_err(|_| format!("节点配置 {} 无法解析（内容未输出）", path.display()))
}
fn safe_provider_path(base: &Path, path: &str) -> Option<PathBuf> {
    let root = base.canonicalize().ok()?;
    let candidate = base.join(path).canonicalize().ok()?;
    if candidate.starts_with(root) && candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}
pub fn load_metadata(be: &Backend) -> Metadata {
    let mut result = Metadata {
        nodes: HashMap::new(),
        notices: vec![],
    };
    let Some(kind) = be.kind.filter(|_| be.source != "--api") else {
        result
            .notices
            .push("显式 API 或未知客户端：仅展示内核已提供的信息，缺失配置字段保持未知".into());
        return result;
    };
    let paths = match crate::clients::DiscoveryPaths::current() {
        Ok(paths) => paths,
        Err(e) => {
            result.notices.push(e);
            return result;
        }
    };
    let path = if kind == crate::client_config::ClientKind::VvCloud {
        // VVCloud 的托管订阅可能加密，不解密账户或凭据文件。
        let runtime = paths.runtime(kind);
        if runtime.exists() {
            runtime
        } else {
            paths.vvcloud.join("profiles/-1.yaml")
        }
    } else {
        paths.runtime(kind)
    };
    load_from_path(&path, &mut result);
    if kind == crate::client_config::ClientKind::VvCloud && result.nodes.is_empty() {
        result.notices.push("VVCloud 未提供可读的明文节点配置（托管订阅可能加密）；仅展示内核信息，服务器等字段保持未知".into());
    }
    result
}
fn load_from_path(path: &Path, result: &mut Metadata) {
    let document = match read_document(path) {
        Ok(doc) => doc,
        Err(e) => {
            result.notices.push(e);
            return;
        }
    };
    for node in document.proxies {
        result
            .nodes
            .insert(node.name.clone(), (node, path.display().to_string()));
    }
    let mut provider_nodes: HashMap<String, (NodeConfig, String)> = HashMap::new();
    let mut duplicated = std::collections::HashSet::new();
    for (name, provider) in document.providers {
        let Some(file) = provider.path else {
            continue;
        };
        let Some(file) = safe_provider_path(path.parent().unwrap_or(Path::new(".")), &file) else {
            result.notices.push(format!(
                "订阅缓存 {} 不可读或位于客户端目录之外，未加载",
                name
            ));
            continue;
        };
        match read_document(&file) {
            Ok(doc) => {
                for node in doc.proxies {
                    if provider_nodes.contains_key(&node.name) {
                        duplicated.insert(node.name.clone());
                    }
                    provider_nodes.insert(
                        node.name.clone(),
                        (node, format!("订阅缓存 {}：{}", name, file.display())),
                    );
                }
            }
            Err(e) => result.notices.push(e),
        }
    }
    for (name, record) in provider_nodes {
        if !duplicated.contains(&name) {
            result.nodes.entry(name).or_insert(record);
        }
    }
    if !duplicated.is_empty() {
        result
            .notices
            .push("多个订阅缓存存在同名节点；冲突详情保持未知，避免关联错误".into());
    }
}
pub fn bool_text(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "开启",
        Some(false) => "关闭",
        None => "未知",
    }
}
/// Provider-declared route/egress labels, not measured network properties.
pub fn route_labels(name: &str) -> Vec<&'static str> {
    let mut labels = vec![];
    for label in ["直连", "中转", "专线"] {
        if name.contains(label) {
            labels.push(label);
        }
    }
    if name.contains("家宽") || name.contains("住宅") {
        labels.push("家宽");
    }
    labels
}

pub fn name_labels(name: &str) -> Vec<&'static str> {
    let mut labels = route_labels(name);
    if name.contains("流媒体") {
        labels.push("流媒体");
    }
    if name.contains("AI") || name.contains("Claude") || name.contains("ChatGPT") {
        labels.push("AI用途");
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_unknown_and_ambiguous_rates_are_safe() {
        for (name, rate) in [
            ("node-0.5倍率", 0.5),
            ("node-1倍率", 1.0),
            ("node-1.0倍率", 1.0),
            ("node-一倍率", 1.0),
            ("node-11倍率", 11.0),
            ("专线A1-日本2-3倍率", 3.0),
            ("专线A1-新加坡3-3倍率", 3.0),
        ] {
            assert_eq!(rate_from_name(name), Some(rate));
        }
        for name in [
            "node",
            "node-十一倍率",
            "node-0倍率",
            "node-1倍率-3倍率",
            "node-1..0倍率",
        ] {
            assert_eq!(rate_from_name(name), None);
            assert!(!permitted(name, Some(1.0)));
        }
        assert!(permitted("node-0.5倍率", Some(1.0)));
        assert!(!permitted("node-3倍率", Some(1.0)));
    }
    #[test]
    fn filtering_precedes_network_probes_and_excludes_placeholders() {
        let proxies:HashMap<String,ProxyInfo>=serde_json::from_value(serde_json::json!({"one-1倍率":{"type":"SS"},"half-0.5倍率":{"type":"SS"},"three-3倍率":{"type":"SS"},"unknown":{"type":"SS"},"✅续费网址:https://getvv.cloud":{"type":"SS"},"group":{"type":"Selector","all":["one-1倍率"]}})).unwrap();
        let members = proxies.keys().cloned().collect::<Vec<_>>();
        let selected = candidates(&proxies, &members, Some(1.0));
        assert_eq!(selected.names.len(), 2);
        assert_eq!(selected.unknown, 1);
        assert_eq!(selected.over_limit, 1);
        assert_eq!(selected.placeholders, 1);
        assert!(selected.names.iter().all(|n| permitted(n, Some(1.0))));
        assert_eq!(candidates(&proxies, &members, None).names.len(), 4);
    }
    #[test]
    fn metadata_parses_whitelist_only_and_loads_provider_cache() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-meta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("providers")).unwrap();
        let path = root.join("runtime.yaml");
        std::fs::write(&path,"proxies:\n  - name: 日本🛰-1倍率\n    type: trojan\n    server: entry.example\n    port: 443\n    password: MUST-NOT-DISPLAY\n    uuid: MUST-NOT-DISPLAY\n    network: ws\n    tls: true\n    udp: true\nproxy-providers:\n  test:\n    path: providers/cache.yaml\n").unwrap();
        std::fs::write(root.join("providers/cache.yaml"),"proxies:\n  - name: provider-node\n    type: ss\n    cipher: aes-128-gcm\n    password: MUST-NOT-DISPLAY\n").unwrap();
        let mut result = Metadata {
            nodes: HashMap::new(),
            notices: vec![],
        };
        load_from_path(&path, &mut result);
        assert_eq!(
            result.nodes["日本🛰-1倍率"].0.protocol.as_deref(),
            Some("trojan")
        );
        assert_eq!(result.nodes["日本🛰-1倍率"].0.tls, Some(true));
        assert_eq!(
            result.nodes["provider-node"].0.cipher.as_deref(),
            Some("aes-128-gcm")
        );
        assert!(result.notices.is_empty());
        assert!(safe_provider_path(&root, "../outside.yaml").is_none());
        std::fs::write(&path, "password: MUST-NOT-DISPLAY: invalid:\n").unwrap();
        assert!(!read_document(&path)
            .err()
            .unwrap()
            .contains("MUST-NOT-DISPLAY"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod billing_policy_tests {
    use super::*;

    const HINT: &str = "🇨🇳 倍率提示|直连x1|中转x1|专线x2";

    #[test]
    fn route_hints_require_explicit_route_labels_and_preserve_explicit_rates() {
        let policy = BillingPolicy::from_hints([HINT]);
        for name in ["香港 A1 | 中转", "日本.H2 | 直连 | Trojan"] {
            assert_eq!(policy.resolve(name), Some((1.0, true)));
            assert!(policy.permitted(name, Some(1.0)));
        }
        assert_eq!(policy.resolve("香港 X1 | 专线"), Some((2.0, true)));
        assert!(!policy.permitted("香港 X1 | 专线", Some(1.0)));
        assert_eq!(
            policy.resolve("香港 X1 | 专线 | 0.5倍率"),
            Some((0.5, false))
        );
        for name in [
            "台湾 X1 | 原生",
            "新加坡 X1",
            "专线A1-日本2",
            "台湾 | 非专线",
            "香港 | 直连 | 专线",
            "香港 | 中转 | 1倍率-3倍率",
            "香港 | 中转 | 1e3倍率",
        ] {
            assert_eq!(policy.resolve(name), None, "{name}");
            assert!(!policy.permitted(name, Some(1.0)), "{name}");
        }
        assert!(policy.label("香港 X1 | 专线").contains("订阅倍率提示"));
    }

    #[test]
    fn conflicting_and_malformed_hints_remain_unknown_without_hardcoded_rates() {
        let policy = BillingPolicy::from_hints([HINT, "倍率提示|直连x2|中转x1|专线x2"]);
        assert_eq!(policy.resolve("日本 | 直连"), None);
        assert_eq!(policy.resolve("香港 | 中转"), Some((1.0, true)));
        let changed = BillingPolicy::from_hints(["倍率提示｜直连 X 0.5｜专线×3"]);
        assert_eq!(changed.resolve("香港｜专线"), Some((3.0, true)));
        assert_eq!(changed.resolve("日本 | 直连"), Some((0.5, true)));
        for malformed in [
            "倍率提示|专线x0",
            "倍率提示|专线x-1",
            "倍率提示|专线x1/2",
            "倍率提示|专线x1e2",
            "倍率提示|专线xNaN",
            "倍率提示|专线x1..2",
        ] {
            assert_eq!(
                BillingPolicy::from_hints([malformed]).resolve("香港 | 专线"),
                None
            );
        }
        assert_eq!(BillingPolicy::default().resolve("香港 | 专线"), None);
    }

    #[test]
    fn live_hints_are_cutecloud_only_and_placeholder_never_becomes_candidate() {
        let proxies: HashMap<String, ProxyInfo> = serde_json::from_value(serde_json::json!({
            (HINT): {"type":"Shadowsocks"},
            "香港 A1 | 中转": {"type":"Shadowsocks"},
            "日本.H2 | 直连 | Trojan": {"type":"Trojan"},
            "香港 X1 | 专线": {"type":"Vmess"},
            "台湾 X1 | 原生": {"type":"Vmess"}
        }))
        .unwrap();
        let mut be = Backend {
            kind: Some(crate::client_config::ClientKind::CuteCloud),
            client: "CuteCloud".into(),
            api: "http://127.0.0.1:9090".to_string().into(),
            secret: None,
            proxy: "http://127.0.0.1:7890".into(),
            source: "test".into(),
            version: None,
            alternatives: vec![],
        };
        let policy = BillingPolicy::for_backend(&be, &proxies);
        let members = vec![
            HINT.into(),
            "香港 A1 | 中转".into(),
            "香港 A1 | 中转".into(),
            "日本.H2 | 直连 | Trojan".into(),
            "香港 X1 | 专线".into(),
            "台湾 X1 | 原生".into(),
        ];
        let selected = candidates_with_policy(&proxies, &members, Some(1.0), &policy);
        assert_eq!(
            selected.names,
            vec!["香港 A1 | 中转", "日本.H2 | 直连 | Trojan"]
        );
        assert_eq!(
            (selected.unknown, selected.over_limit, selected.placeholders),
            (1, 1, 1)
        );
        assert_eq!(
            candidates_with_policy(&proxies, &members, None, &policy)
                .names
                .len(),
            4
        );
        for kind in [
            Some(crate::client_config::ClientKind::VvCloud),
            Some(crate::client_config::ClientKind::ClashVerge),
            None,
        ] {
            be.kind = kind;
            assert_eq!(
                BillingPolicy::for_backend(&be, &proxies).resolve("香港 A1 | 中转"),
                None
            );
        }
    }
}
