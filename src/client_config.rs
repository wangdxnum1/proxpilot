use serde_json::{Map, Value};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    CuteCloud,
    ClashVerge,
    VvCloud,
}

impl ClientKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::CuteCloud => "cutecloud",
            Self::ClashVerge => "clash-verge",
            Self::VvCloud => "vvcloud",
        }
    }
    pub fn display_name(self) -> &'static str {
        match self {
            Self::CuteCloud => "CuteCloud",
            Self::ClashVerge => "Clash Verge",
            Self::VvCloud => "VVCloud",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientSelection {
    Explicit(ClientKind),
    Auto,
}

impl ClientSelection {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "cutecloud" => Ok(Self::Explicit(ClientKind::CuteCloud)),
            "clash-verge" => Ok(Self::Explicit(ClientKind::ClashVerge)),
            "vvcloud" => Ok(Self::Explicit(ClientKind::VvCloud)),
            "auto" => Ok(Self::Auto),
            _ => Err(
                "客户端类型无效；支持 cutecloud、clash-verge、vvcloud、auto（用 clients --supported 查看）"
                    .into(),
            ),
        }
    }
}

impl fmt::Display for ClientSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Explicit(kind) => kind.name(),
            Self::Auto => "auto",
        })
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(|p| PathBuf::from(p).join("ProxPilot").join("config.json"))
        .ok_or_else(|| "无法确定配置路径：APPDATA 未设置".into())
}

fn read_document(path: &Path) -> Result<Map<String, Value>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Map::from_iter([("schema_version".into(), Value::from(1))]))
        }
        Err(e) => return Err(format!("读取配置 {} 失败：{}", path.display(), e)),
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        format!(
            "配置 {} 不是有效 JSON；请修复或使用 --client 明确指定",
            path.display()
        )
    })?;
    let map = value.as_object().cloned().ok_or("配置必须是 JSON 对象")?;
    if map.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("不支持的配置版本：schema_version 必须为 1".into());
    }
    if let Some(value) = map.get("default_client") {
        ClientSelection::parse(value.as_str().ok_or("default_client 必须是字符串")?)?;
    }
    Ok(map)
}

pub fn load_default(path: &Path) -> Result<Option<ClientSelection>, String> {
    read_document(path)?
        .get("default_client")
        .map(|v| ClientSelection::parse(v.as_str().unwrap()))
        .transpose()
}

pub fn save_default(path: &Path, client: Option<ClientSelection>) -> Result<(), String> {
    let mut document = read_document(path)?;
    match client {
        Some(client) => {
            document.insert("default_client".into(), Value::String(client.to_string()));
        }
        None => {
            document.remove("default_client");
        }
    }
    let parent = path.parent().ok_or("配置路径没有父目录")?;
    fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败：{}", e))?;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let tmp = parent.join(format!(
        ".config-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| format!("保存配置失败（原配置已保留）：{}", e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn show_config(path: &Path) -> Result<String, String> {
    let saved = load_default(path)?;
    Ok(format!(
        "配置文件：{}\n保存的默认客户端：{}\n运行时选择方式：{}",
        path.display(),
        saved
            .map(|v| v.to_string())
            .unwrap_or_else(|| "未设置（空）".into()),
        saved
            .map(|v| v.to_string())
            .unwrap_or_else(|| "auto（未设置默认客户端，自动探测）".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "proxpilot-config-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("config.json")
    }

    #[test]
    fn client_names_are_strict_and_case_insensitive() {
        assert_eq!(
            ClientSelection::parse("CLASH-VERGE").unwrap(),
            ClientSelection::Explicit(ClientKind::ClashVerge)
        );
        assert_eq!(
            ClientSelection::parse("auto").unwrap(),
            ClientSelection::Auto
        );
        assert_eq!(
            ClientSelection::parse("VVCLOUD").unwrap(),
            ClientSelection::Explicit(ClientKind::VvCloud)
        );
        assert!(ClientSelection::parse("verge").is_err());
        assert!(ClientSelection::parse("unknown").is_err());
    }

    #[test]
    fn config_round_trip_preserves_unknown_fields() {
        let p = temp_path();
        assert_eq!(load_default(&p).unwrap(), None);
        let shown = show_config(&p).unwrap();
        assert!(shown.contains("未设置（空）"));
        assert!(shown.contains("auto"));
        assert!(!shown.contains("cutecloud"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, r#"{"schema_version":1,"future":{"value":true}}"#).unwrap();
        save_default(&p, Some(ClientSelection::Explicit(ClientKind::ClashVerge))).unwrap();
        assert_eq!(
            load_default(&p).unwrap(),
            Some(ClientSelection::Explicit(ClientKind::ClashVerge))
        );
        let v: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["future"]["value"], true);
        assert!(show_config(&p).unwrap().contains("clash-verge"));
        save_default(&p, Some(ClientSelection::Explicit(ClientKind::VvCloud))).unwrap();
        assert_eq!(
            load_default(&p).unwrap(),
            Some(ClientSelection::Explicit(ClientKind::VvCloud))
        );
        save_default(&p, None).unwrap();
        assert_eq!(load_default(&p).unwrap(), None);
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn config_rejects_invalid_data_without_overwrite() {
        let p = temp_path();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        for bad in [
            "broken",
            r#"{"schema_version":2}"#,
            r#"{"schema_version":1,"default_client":"unknown"}"#,
            r#"{"schema_version":1,"default_client":false}"#,
        ] {
            fs::write(&p, bad).unwrap();
            assert!(load_default(&p).is_err());
            assert!(save_default(&p, Some(ClientSelection::Auto)).is_err());
            assert_eq!(fs::read_to_string(&p).unwrap(), bad);
        }
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
}
