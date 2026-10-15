//! 同步 CuteCloud / VVCloud 的本地 database.sqlite profiles.selected_map。
//!
//! 切换内核后尽力更新已有记录；GUI 和重启持久化的实际行为由客户端管理。

use rusqlite::Connection;
use std::path::PathBuf;

use crate::ui;

fn db_path(kind: crate::client_config::ClientKind, base: &std::path::Path) -> Option<PathBuf> {
    use crate::client_config::ClientKind;
    let directory = match kind {
        ClientKind::CuteCloud => "CuteCloud/CuteCloud",
        ClientKind::VvCloud => "VVCloud/VVCloud",
        ClientKind::ClashVerge => return None,
    };
    let path = base.join(directory).join("database.sqlite");
    path.is_file().then_some(path)
}

fn active_profile_id(directory: &std::path::Path) -> Option<i64> {
    let prefs: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(directory.join("shared_preferences.json")).ok()?,
    )
    .ok()?;
    let config: serde_json::Value =
        serde_json::from_str(prefs.get("flutter.config")?.as_str()?).ok()?;
    config.get("currentProfileId")?.as_i64()
}

/// Read only the active subscription; the caller must validate this group
/// against the target kernel's live /proxies response before using it.
pub fn current_group(be: &crate::Backend) -> Option<String> {
    let base = std::env::var_os("APPDATA")?;
    let path = db_path(be.kind?, std::path::Path::new(&base))?;
    current_group_from_database(&path)
}

pub fn current_profile_id(be: &crate::Backend) -> Option<i64> {
    let base = std::env::var_os("APPDATA")?;
    let path = db_path(be.kind?, std::path::Path::new(&base))?;
    active_profile_id(path.parent()?)
}

pub fn sync_selection_captured(
    be: &crate::Backend,
    profile: Option<i64>,
    group: &str,
    node: &str,
) -> crate::diagnostics::AppResult<crate::switching::MemorySync> {
    use crate::{client_config::ClientKind, switching::MemorySync};
    let Some(kind) = be.kind else {
        return Ok(MemorySync::NotAvailable);
    };
    if kind == ClientKind::ClashVerge {
        return Ok(MemorySync::ClientManaged);
    }
    let (Some(profile), Some(base)) = (profile, std::env::var_os("APPDATA")) else {
        return Ok(MemorySync::NotAvailable);
    };
    let Some(path) = db_path(kind, std::path::Path::new(&base)) else {
        return Ok(MemorySync::NotAvailable);
    };
    sync_database_captured(&path, profile, group, node)
}

fn sync_database_captured(
    path: &std::path::Path,
    profile: i64,
    group: &str,
    node: &str,
) -> crate::diagnostics::AppResult<crate::switching::MemorySync> {
    use crate::{
        diagnostics::{AppError, ErrorCode, Stage},
        switching::MemorySync,
    };
    use rusqlite::OptionalExtension;
    let failure = || AppError::new(ErrorCode::StateUnavailable, Stage::State);
    let directory = path.parent().ok_or_else(failure)?;
    let check_profile = || {
        if active_profile_id(directory) == Some(profile) {
            Ok(())
        } else {
            Err(AppError::new(ErrorCode::ProfileChanged, Stage::State))
        }
    };
    check_profile()?;
    let mut con = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| failure())?;
    con.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| failure())?;
    let transaction = con
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| failure())?;
    let existing: Option<Option<String>> = transaction
        .query_row(
            "SELECT selected_map FROM profiles WHERE id=?1",
            [profile],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure())?;
    let Some(Some(original)) = existing else {
        return Ok(MemorySync::NoMatchingGroup);
    };
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&original).map_err(|_| failure())?;
    if !map.contains_key(group) {
        return Ok(MemorySync::NoMatchingGroup);
    }
    map.insert(group.into(), serde_json::Value::String(node.into()));
    check_profile()?;
    let count = transaction.execute(
        "UPDATE profiles SET selected_map=?1, current_group_name=?2 WHERE id=?3 AND selected_map=?4",
        rusqlite::params![serde_json::Value::Object(map).to_string(), group, profile, original],
    ).map_err(|_| failure())?;
    if count != 1 {
        return Err(failure());
    }
    transaction.commit().map_err(|_| failure())?;
    Ok(MemorySync::Synced)
}

fn current_group_from_database(path: &std::path::Path) -> Option<String> {
    let profile = active_profile_id(path.parent()?)?;
    let con = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    con.busy_timeout(std::time::Duration::from_millis(200))
        .ok()?;
    let group: Option<String> = con
        .query_row(
            "SELECT current_group_name FROM profiles WHERE id = ?1",
            [profile],
            |r| r.get(0),
        )
        .ok()?;
    group.filter(|g| !g.trim().is_empty())
}

/// 同步目标客户端的选择记录；失败不影响已完成的内核切换。
pub fn sync_selection(be: &crate::Backend, group: &str, node: &str) {
    let Some(kind) = be.kind else {
        return;
    };
    if kind == crate::client_config::ClientKind::ClashVerge {
        ui::dim("Clash Verge：当前内核选择已更新；重启后的持久化由客户端管理，不写 profiles.yaml");
        return;
    }
    let Some(base) = std::env::var_os("APPDATA") else {
        return;
    };
    let Some(path) = db_path(kind, std::path::Path::new(&base)) else {
        return;
    };
    match sync_database(&path, group, node) {
        Ok(true) => ui::dim(&format!(
            "已同步 {} 的本地选择记录；客户端重启后的行为由其自身管理",
            kind.display_name()
        )),
        Ok(false) => ui::dim(&format!(
            "未找到记录此策略组的 {} profile，未同步选择记忆",
            kind.display_name()
        )),
        Err(e) => ui::dim(&format!("同步客户端记忆失败（不影响切换本身）：{}", e)),
    }
}

fn sync_database(path: &std::path::Path, group: &str, node: &str) -> Result<bool, String> {
    let directory = path.parent().ok_or("数据库目录无效")?;
    let profile = active_profile_id(directory);
    // When preferences exist, never fall back to another subscription if
    // the active profile cannot be read.
    if directory.join("shared_preferences.json").exists() && profile.is_none() {
        return Ok(false);
    }
    let con = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| e.to_string())?;
    con.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    let mut stmt = con
        .prepare("SELECT id, selected_map FROM profiles WHERE (?1 IS NULL OR id = ?1)")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([profile], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    for (id, sm) in rows {
        let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&sm)
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
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_profile_cannot_write_into_a_new_subscription() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-captured-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("database.sqlite");
        let con = Connection::open(&path).unwrap();
        con.execute_batch(r#"CREATE TABLE profiles(id INTEGER PRIMARY KEY, selected_map TEXT, current_group_name TEXT);
            INSERT INTO profiles VALUES(1,'{"AI":"original-1"}','AI');
            INSERT INTO profiles VALUES(2,'{"AI":"original-2"}','AI');"#).unwrap();
        let write_profile = |id| {
            std::fs::write(root.join("shared_preferences.json"),
            serde_json::json!({"flutter.config":serde_json::json!({"currentProfileId":id}).to_string()}).to_string()).unwrap()
        };
        write_profile(2);
        assert_eq!(
            sync_database_captured(&path, 1, "AI", "verified")
                .unwrap_err()
                .code,
            crate::diagnostics::ErrorCode::ProfileChanged
        );
        let read = |id| {
            con.query_row(
                "SELECT selected_map FROM profiles WHERE id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        };
        assert!(read(1).contains("original-1"));
        assert!(read(2).contains("original-2"));
        write_profile(1);
        assert_eq!(
            sync_database_captured(&path, 1, "AI", "verified").unwrap(),
            crate::switching::MemorySync::Synced
        );
        assert!(read(1).contains("verified"));
        assert!(read(2).contains("original-2"));
        drop(con);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn current_group_and_sync_follow_active_profile_without_cross_subscription_writes() {
        let root = std::env::temp_dir().join(format!(
            "proxpilot-active-group-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("database.sqlite");
        let prefs = root.join("shared_preferences.json");
        let write_profile = |id| {
            std::fs::write(
                &prefs,
                serde_json::json!({
                    "flutter.config": serde_json::json!({"currentProfileId":id}).to_string()
                })
                .to_string(),
            )
            .unwrap()
        };
        let con = Connection::open(&path).unwrap();
        con.execute_batch("CREATE TABLE profiles(id INTEGER, current_group_name TEXT, selected_map TEXT); INSERT INTO profiles VALUES(1,'Other','{\"Shared\":\"old1\"}'); INSERT INTO profiles VALUES(2,'Shared','{\"Shared\":\"old2\"}');").unwrap();
        write_profile(2);
        assert_eq!(
            current_group_from_database(&path).as_deref(),
            Some("Shared")
        );
        assert!(sync_database(&path, "Shared", "new").unwrap());
        let maps: Vec<String> = con
            .prepare("SELECT selected_map FROM profiles ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&maps[0]).unwrap()["Shared"],
            "old1"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&maps[1]).unwrap()["Shared"],
            "new"
        );
        write_profile(1);
        assert_eq!(current_group_from_database(&path).as_deref(), Some("Other"));
        write_profile(999);
        assert!(current_group_from_database(&path).is_none());
        assert!(!sync_database(&path, "Shared", "wrong").unwrap());
        std::fs::write(&prefs, "invalid").unwrap();
        assert!(current_group_from_database(&path).is_none());
        assert!(!sync_database(&path, "Shared", "wrong").unwrap());
        assert!(current_group_from_database(&root.join("missing.sqlite")).is_none());
        assert!(!root.join("missing.sqlite").exists());
        drop(con);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn selection_database_is_isolated_by_client() {
        use crate::client_config::ClientKind;
        let root = std::env::temp_dir().join(format!(
            "proxpilot-client-db-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for dir in ["CuteCloud/CuteCloud", "VVCloud/VVCloud"] {
            let path = root.join(dir).join("database.sqlite");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let con = Connection::open(path).unwrap();
            con.execute_batch(r#"CREATE TABLE profiles(id INTEGER, selected_map TEXT, current_group_name TEXT); INSERT INTO profiles VALUES(1, '{"Proxy":"old"}', 'Proxy');"#).unwrap();
        }
        let vv = db_path(ClientKind::VvCloud, &root).unwrap();
        let cute = db_path(ClientKind::CuteCloud, &root).unwrap();
        assert_ne!(vv, cute);
        assert!(sync_database(&vv, "Proxy", "new").unwrap());
        let con = Connection::open(cute).unwrap();
        let saved: String = con
            .query_row("SELECT selected_map FROM profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&saved).unwrap()["Proxy"],
            "old"
        );
        assert!(db_path(ClientKind::ClashVerge, &root).is_none());
        drop(con);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn updates_only_the_profile_with_matching_group() {
        let path = std::env::temp_dir().join(format!(
            "proxpilot-db-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let con = Connection::open(&path).unwrap();
        con.execute_batch(
            "CREATE TABLE profiles(id INTEGER,selected_map TEXT,current_group_name TEXT);",
        )
        .unwrap();
        con.execute(
            "INSERT INTO profiles VALUES(1,?1,'x')",
            [r#"{"Other":"old"}"#],
        )
        .unwrap();
        con.execute(
            "INSERT INTO profiles VALUES(2,?1,'x')",
            [r#"{"VVCloud":"old"}"#],
        )
        .unwrap();
        assert!(sync_database(&path, "VVCloud", "日本 🛰").unwrap());
        assert!(!sync_database(&path, "Missing", "other").unwrap());
        let values: Vec<String> = con
            .prepare("SELECT selected_map FROM profiles ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(values[0], r#"{"Other":"old"}"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&values[1]).unwrap()["VVCloud"],
            "日本 🛰"
        );
        drop(con);
        std::fs::remove_file(path).unwrap();
    }
}
