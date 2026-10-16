use crate::{
    diagnostics::{AppError, AppResult, ErrorCode, Stage},
    mihomo, nodeinfo,
    operation::Cancellation,
    runtime_switch::KernelSwitchControl,
    switching::{LiveState, SwitchControl},
    terminal::{literal, Input},
    Args, Backend,
};
use std::io::IsTerminal;
const PAGE: usize = 20;
#[derive(Clone)]
struct Entry {
    id: String,
    name: String,
    protocol: String,
    rate: Option<f64>,
    labels: Vec<String>,
}
#[derive(Default, Clone)]
struct Filter {
    keyword: String,
    protocol: String,
    rate: Option<f64>,
    line: String,
}
struct BrowseModel {
    entries: Vec<Entry>,
    filter: Filter,
    page: usize,
}
impl BrowseModel {
    fn visible(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| {
                e.name
                    .to_lowercase()
                    .contains(&self.filter.keyword.to_lowercase())
                    && (self.filter.protocol.is_empty()
                        || e.protocol.eq_ignore_ascii_case(&self.filter.protocol))
                    && self
                        .filter
                        .rate
                        .is_none_or(|max| e.rate.is_some_and(|r| r <= max))
                    && (self.filter.line.is_empty()
                        || e.labels.iter().any(|l| l.contains(&self.filter.line)))
            })
            .collect()
    }
    fn target(&self, id: &str) -> Option<&Entry> {
        let matches: Vec<_> = self.visible().into_iter().filter(|e| e.id == id).collect();
        (matches.len() == 1).then(|| matches[0])
    }
    fn render(&mut self, current: Option<&str>) {
        let visible = self.visible();
        let pages = visible.len().div_ceil(PAGE).max(1);
        let page = self.page.min(pages - 1);
        println!("第 {}/{} 页 · {} 个节点", page + 1, pages, visible.len());
        for e in visible.iter().skip(page * PAGE).take(PAGE) {
            println!(
                "{} id={}  {}  [{}] {}×  {}",
                if current == Some(e.name.as_str()) {
                    "▶"
                } else {
                    " "
                },
                e.id,
                literal(&e.name),
                literal(&e.protocol),
                e.rate.map(|r| r.to_string()).unwrap_or_else(|| "?".into()),
                e.labels.join(" · ")
            );
        }
        self.page = page;
    }
}
fn entry_id(state: &LiveState, group: &str, name: &str) -> String {
    let mut members = state.members.clone();
    members.sort();
    members.dedup();
    crate::history::fingerprint(
        &serde_json::json!([
            state.identity,
            state.profile,
            members,
            state.selection,
            group,
            name
        ])
        .to_string(),
    )[..12]
        .into()
}
fn load(
    be: &Backend,
    api: &crate::core_api::CoreApi,
    args: &Args,
) -> AppResult<(BrowseModel, LiveState)> {
    let profile = crate::appstate::current_profile_id(be);
    let (proxies, pid) = mihomo::get_proxies_detailed(be, api)?;
    if profile != crate::appstate::current_profile_id(be) {
        return Err(AppError::new(ErrorCode::ProfileChanged, Stage::Switching));
    }
    let group = proxies
        .get(&args.group)
        .filter(|p| legal(p))
        .ok_or_else(crate::terminal::invalid)?;
    let mut members = group.all.clone().unwrap_or_default();
    members.sort();
    members.dedup();
    let state = LiveState {
        identity: KernelSwitchControl { backend: be, api }.identity(pid),
        profile,
        members,
        selection: group.now.clone(),
    };
    let policy = nodeinfo::BillingPolicy::for_backend(be, &proxies);
    let selected =
        nodeinfo::candidates_with_policy(&proxies, &state.members, args.max_rate, &policy);
    let entries = selected
        .names
        .iter()
        .map(|name| Entry {
            id: entry_id(&state, &args.group, name),
            name: name.clone(),
            protocol: proxies[name].ptype.clone(),
            rate: policy.resolve(name).map(|r| r.0),
            labels: nodeinfo::name_labels(name)
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .collect();
    Ok((
        BrowseModel {
            entries,
            filter: Filter::default(),
            page: 0,
        },
        state,
    ))
}
fn legal(info: &mihomo::ProxyInfo) -> bool {
    info.all.is_some()
        && (info.ptype.eq_ignore_ascii_case("selector")
            || info.ptype.eq_ignore_ascii_case("select"))
}
fn choose_group(
    be: &Backend,
    api: &crate::core_api::CoreApi,
    args: &Args,
    input: &Input,
) -> AppResult<Option<String>> {
    let (proxies, _) = mihomo::get_proxies_detailed(be, api)?;
    let mut groups: Vec<_> = proxies
        .iter()
        .filter(|(_, p)| legal(p))
        .map(|(n, _)| n.clone())
        .collect();
    groups.sort();
    if !args.group.is_empty() {
        return if groups.contains(&args.group) {
            Ok(Some(args.group.clone()))
        } else {
            Err(crate::terminal::invalid())
        };
    }
    if groups.len() == 1 {
        return Ok(Some(groups.remove(0)));
    }
    if groups.is_empty() {
        return Err(crate::terminal::invalid());
    }
    for (i, name) in groups.iter().enumerate() {
        println!("{}  {}", i + 1, literal(name));
    }
    print!("group [编号/q]> ");
    let Some(value) = input.next()? else {
        return Ok(None);
    };
    if value == "q" || value.is_empty() {
        return Ok(None);
    }
    let index = value
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(crate::terminal::invalid)?
        - 1;
    groups
        .get(index)
        .cloned()
        .map(Some)
        .ok_or_else(crate::terminal::invalid)
}
pub fn run(initial: &Backend, api: &crate::core_api::CoreApi, args: &Args) -> i32 {
    if args.json || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        let e = crate::terminal::invalid();
        crate::report::error(&e);
        crate::ui::fail("browse 需要输入/输出终端，且不能与 --json 组合");
        return 1;
    }
    let input = Input::new();
    let mut args = args.clone();
    let result = (|| -> AppResult<()> {
        let mut be = initial.clone();
        let Some(group) = choose_group(&be, api, &args, &input)? else {
            return Ok(());
        };
        args.group = group;
        let (mut model, mut state) = load(&be, api, &args)?;
        loop {
            Cancellation::global().check()?;
            println!(
                "策略组: {} · 当前: {}",
                literal(&args.group),
                literal(state.selection.as_deref().unwrap_or("?"))
            );
            model.render(state.selection.as_deref());
            println!("filter <关键词> / protocol <协议|all> / rate <上限|all> / line <标签|all>");
            println!("next / prev / detail <ID> / use <ID> / refresh / scan / group / q");
            print!("browse> ");
            let Some(line) = input.next()? else { break };
            let (action, value) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            let value = value.trim();
            match action {
                "q" | "quit" => break,
                "filter" => {
                    model.filter.keyword = value.into();
                    model.page = 0;
                }
                "protocol" => {
                    model.filter.protocol = if value == "all" {
                        String::new()
                    } else {
                        value.into()
                    };
                    model.page = 0;
                }
                "line" => {
                    model.filter.line = if value == "all" {
                        String::new()
                    } else {
                        value.into()
                    };
                    model.page = 0;
                }
                "rate" => {
                    if value == "all" {
                        model.filter.rate = None;
                    } else if let Ok(rate) = value.parse::<f64>() {
                        if rate.is_finite() && rate > 0.0 {
                            model.filter.rate = Some(rate);
                        } else {
                            crate::ui::warn("倍率须为有限正数");
                        }
                    } else {
                        crate::ui::warn("倍率须为有限正数");
                    }
                    model.page = 0;
                }
                "next" => model.page = model.page.saturating_add(1),
                "prev" => model.page = model.page.saturating_sub(1),
                "detail" => {
                    if let Some(entry) = model.target(value) {
                        let metadata = nodeinfo::load_metadata(&be);
                        println!(
                            "{}",
                            serde_json::json!({"name":entry.name,"protocol":entry.protocol,"rate":entry.rate,"labels":entry.labels,"labels_source":"name_unverified","configuration":metadata.nodes.get(&entry.name).map(|(c,_)|c)})
                        );
                    } else {
                        crate::ui::warn("无效或过期 ID，请使用当前列表的 ID");
                    }
                }
                "use" => {
                    let Some(entry) = model.target(value).cloned() else {
                        crate::ui::warn("无效或过期 ID，请使用当前列表的 ID");
                        continue;
                    };
                    let current = KernelSwitchControl { backend: &be, api }.state(&args.group)?;
                    if !same_state(&current, &state) {
                        crate::ui::warn("客户端状态已改变，请 refresh 后重新选择");
                        continue;
                    }
                    println!(
                        "确认切换: 客户端 {} · 组 {} · {} → {} · {}× · 网页采样 {} 次（{}ms/次）{}",
                        literal(&be.client),
                        literal(&args.group),
                        literal(state.selection.as_deref().unwrap_or("?")),
                        literal(&entry.name),
                        entry
                            .rate
                            .map(|r| r.to_string())
                            .unwrap_or_else(|| "未知".into()),
                        args.samples,
                        args.verify_timeout_ms,
                        if args.dry_run { " · dry-run" } else { "" }
                    );
                    print!("confirm [y/N]> ");
                    let answer = input.next()?;
                    if !answer.as_deref().is_some_and(|s| {
                        s.eq_ignore_ascii_case("y") || s.eq_ignore_ascii_case("yes")
                    }) {
                        crate::ui::info("已取消切换");
                        continue;
                    }
                    match crate::workflow::use_flow_expected(
                        &be,
                        api,
                        &args,
                        &entry.name,
                        Some(&state),
                    ) {
                        Ok(crate::workflow::SelectionOutcome::Committed { result }) => {
                            crate::print_commit(&result)
                        }
                        Ok(crate::workflow::SelectionOutcome::DryRun { .. }) => {
                            crate::ui::info("dry-run：确认预览完成，未切换")
                        }
                        Err(failure) => {
                            crate::ui::fail(&failure.to_string());
                            if failure.exit_code() == 130 {
                                return Err(failure.error);
                            }
                        }
                    }
                    let fresh = load(&be, api, &args)?;
                    let filter = model.filter.clone();
                    model = fresh.0;
                    model.filter = filter;
                    state = fresh.1;
                }
                "refresh" | "scan" | "group" => {
                    be = crate::clients::refresh(&be, &args, api).map_err(|_| {
                        AppError::new(ErrorCode::ClientUnavailable, Stage::Discovery)
                    })?;
                    if action == "group" {
                        let mut selection = args.clone();
                        selection.group.clear();
                        let Some(group) = choose_group(&be, api, &selection, &input)? else {
                            continue;
                        };
                        args.group = group;
                    }
                    if action == "scan" {
                        let code = crate::cmd_scan(&be, api, &args);
                        if code == 130 {
                            return Err(AppError::new(ErrorCode::Cancelled, Stage::Probe));
                        }
                    }
                    let fresh = load(&be, api, &args)?;
                    let filter = model.filter.clone();
                    model = fresh.0;
                    model.filter = filter;
                    state = fresh.1;
                }
                _ => crate::ui::warn("未知操作，请使用菜单命令"),
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            crate::report::error(&e);
            crate::ui::fail(&e.to_string());
            if e.code == ErrorCode::Cancelled {
                130
            } else {
                1
            }
        }
    }
}
fn same_state(left: &LiveState, right: &LiveState) -> bool {
    let mut l = left.members.clone();
    let mut r = right.members.clone();
    l.sort();
    l.dedup();
    r.sort();
    r.dedup();
    left.identity == right.identity
        && left.profile == right.profile
        && left.selection == right.selection
        && l == r
}
#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> BrowseModel {
        let nodes = vec![
            Entry {
                id: "a".into(),
                name: "香港👩‍💻-1倍率-专线".into(),
                protocol: "SS".into(),
                rate: Some(1.0),
                labels: vec!["专线".into()],
            },
            Entry {
                id: "b".into(),
                name: "日本-3倍率".into(),
                protocol: "AnyTLS".into(),
                rate: Some(3.0),
                labels: vec![],
            },
            Entry {
                id: "c".into(),
                name: "未知".into(),
                protocol: "SS".into(),
                rate: None,
                labels: vec![],
            },
        ];
        BrowseModel {
            entries: nodes,
            filter: Filter::default(),
            page: 0,
        }
    }
    #[test]
    fn filters_are_combined_and_unknown_rate_never_passes_limit() {
        let mut m = model();
        assert_eq!(m.visible().len(), 3);
        m.filter.rate = Some(1.0);
        assert_eq!(m.visible().len(), 1);
        m.filter.protocol = "anytls".into();
        assert!(m.visible().is_empty());
        m.filter.rate = None;
        assert_eq!(m.visible()[0].id, "b");
        m.filter.protocol.clear();
        m.filter.line = "专线".into();
        m.filter.keyword = "👩‍💻".into();
        assert_eq!(m.visible()[0].name, "香港👩‍💻-1倍率-专线");
    }
    #[test]
    fn identity_changes_when_members_selection_or_scope_changes() {
        let state = crate::switching::LiveState {
            identity: "core".into(),
            profile: Some(1),
            members: vec!["a".into(), "b".into()],
            selection: Some("a".into()),
        };
        let id = entry_id(&state, "group", "b");
        let mut changed = state.clone();
        changed.selection = Some("b".into());
        assert_ne!(id, entry_id(&changed, "group", "b"));
        changed = state.clone();
        changed.members.push("c".into());
        assert_ne!(id, entry_id(&changed, "group", "b"));
        assert_ne!(id, entry_id(&state, "other", "b"));
        changed = state.clone();
        changed.members.reverse();
        assert_eq!(id, entry_id(&changed, "group", "b"));
    }
    #[test]
    fn ambiguous_id_and_old_ordinal_are_never_targets() {
        let m = model();
        assert!(m.target("1").is_none());
        assert!(m.target("unknown").is_none());
        assert_eq!(m.target("a").unwrap().name, "香港👩‍💻-1倍率-专线");
    }
}
