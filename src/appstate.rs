//! 同步客户端自己的选择记忆（CuteCloud：database.sqlite profiles.selected_map）。
//!
//! 客户端 GUI 显示的"当前节点"和它反向覆盖内核选择的依据都来自这份记忆，
//! 切换内核后把它一并更新，界面与内核才不会长期脱节。

use rusqlite::Connection;
use std::path::PathBuf;

use crate::ui;

fn db_path() -> Option<PathBuf> {
    let base = std::env::var("APPDATA").ok()?;
    let p = PathBuf::from(base)
        .join("CuteCloud")
        .join("CuteCloud")
        .join("database.sqlite");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// 把指定组在客户端记忆里的选中节点同步为 node（best-effort，失败不影响切换本身）
pub fn sync_selection(group: &str, node: &str) {
    let Some(path) = db_path() else {
        return; // 非 CuteCloud 客户端或未安装，静默跳过
    };
    let res = (|| -> Result<(), String> {
        let con = Connection::open(&path).map_err(|e| e.to_string())?;
        con.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        let mut stmt = con
            .prepare("SELECT id, selected_map FROM profiles")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        for (id, sm) in rows {
            let Ok(mut map) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&sm)
            else {
                continue;
            };
            // 只更新确实记录了这个组的 profile（定位到正确的订阅）
            if !map.contains_key(group) {
                continue;
            }
            map.insert(
                group.to_string(),
                serde_json::Value::String(node.to_string()),
            );
            let new_sm = serde_json::Value::Object(map).to_string();
            con.execute(
                "UPDATE profiles SET selected_map = ?1, current_group_name = ?2 WHERE id = ?3",
                rusqlite::params![new_sm, group, id],
            )
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
        Ok(())
    })();
    match res {
        Ok(()) => ui::dim(
            "已同步客户端的选择记忆；CuteCloud 重启后界面显示将与实际一致，且不会再用旧节点覆盖",
        ),
        Err(e) => ui::dim(&format!("同步客户端记忆失败（不影响切换本身）：{}", e)),
    }
}
