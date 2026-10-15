use crate::diagnostics::{AppError, AppResult, ErrorCode, Stage};
use windows_sys::Win32::Networking::WinInet::{
    InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
use winreg::RegKey;

const INET_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProxySnapshot {
    pub enabled: Option<u32>,
    pub server: Option<String>,
}

fn state_error() -> AppError {
    AppError::new(ErrorCode::StateUnavailable, Stage::State)
}

fn optional_value<T: winreg::types::FromRegValue>(
    key: &RegKey,
    name: &str,
) -> AppResult<Option<T>> {
    match key.get_value(name) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(state_error()),
    }
}

fn snapshot_from_key(key: &RegKey) -> AppResult<ProxySnapshot> {
    Ok(ProxySnapshot {
        enabled: optional_value(key, "ProxyEnable")?,
        server: optional_value(key, "ProxyServer")?,
    })
}

pub fn snapshot() -> AppResult<ProxySnapshot> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(INET_KEY, KEY_QUERY_VALUE)
        .map_err(|_| state_error())?;
    snapshot_from_key(&key)
}

pub fn target_snapshot(proxy: &str) -> AppResult<ProxySnapshot> {
    let invalid = || AppError::new(ErrorCode::InvalidInput, Stage::Configuration);
    let url = reqwest::Url::parse(proxy).map_err(|_| invalid())?;
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid());
    }
    let host = url.host_str().ok_or_else(invalid)?.trim_matches(['[', ']']);
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.into()
    };
    let port = url.port_or_known_default().ok_or_else(invalid)?;
    Ok(ProxySnapshot {
        enabled: Some(1),
        server: Some(format!("{host}:{port}")),
    })
}

fn restore_from_key(
    key: &RegKey,
    before: &ProxySnapshot,
    written: &ProxySnapshot,
) -> AppResult<bool> {
    let current = snapshot_from_key(key)?;
    if (current.enabled != before.enabled && current.enabled != written.enabled)
        || (current.server != before.server && current.server != written.server)
    {
        return Ok(false);
    }
    if current.server != before.server {
        match &before.server {
            Some(value) => key.set_value("ProxyServer", value),
            None => key.delete_value("ProxyServer"),
        }
        .map_err(|_| state_error())?;
    }
    if current.enabled != before.enabled {
        match before.enabled {
            Some(value) => key.set_value("ProxyEnable", &value),
            None => key.delete_value("ProxyEnable"),
        }
        .map_err(|_| state_error())?;
    }
    Ok(true)
}

pub fn restore_if_ours(before: &ProxySnapshot, written: &ProxySnapshot) -> AppResult<bool> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(INET_KEY, KEY_QUERY_VALUE | KEY_SET_VALUE)
        .map_err(|_| state_error())?;
    let restored = restore_from_key(&key, before, written)?;
    if restored {
        notify();
    }
    Ok(restored)
}

pub fn proxy_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey_with_flags(INET_KEY, KEY_QUERY_VALUE) {
        Ok(k) => k.get_value::<u32, _>("ProxyEnable").unwrap_or(0) == 1,
        Err(_) => false,
    }
}

/// 读取注册表里记录的代理服务器地址（如 127.0.0.1:7890），不带协议前缀
pub fn registry_proxy_server() -> Option<String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let k = hkcu
        .open_subkey_with_flags(INET_KEY, KEY_QUERY_VALUE)
        .ok()?;
    let v: String = k.get_value("ProxyServer").ok()?;
    let v = v.trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 打开系统代理（含 WinINET 广播刷新，浏览器即时生效）
pub fn matches(proxy: &str) -> bool {
    proxy_enabled()
        && registry_proxy_server()
            .as_deref()
            .is_some_and(|r| crate::clients::proxy_matches(proxy, r))
}
fn write_proxy(k: &RegKey, proxy: &str) -> Result<(), String> {
    let target = target_snapshot(proxy)?;
    k.set_value("ProxyServer", &target.server.ok_or("代理地址无效")?)
        .map_err(|e| e.to_string())?;
    k.set_value("ProxyEnable", &1u32)
        .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn enable_for(proxy: &str) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let k = hkcu
        .open_subkey_with_flags(INET_KEY, KEY_SET_VALUE)
        .map_err(|e| format!("打开注册表失败: {}", e))?;
    write_proxy(&k, proxy)?;
    notify();
    Ok(())
}

fn notify() {
    unsafe {
        InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_SETTINGS_CHANGED,
            std::ptr::null(),
            0,
        );
        InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_REFRESH,
            std::ptr::null(),
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_restore_preserves_external_changes_and_handles_partial_writes() {
        let path = format!(
            r"Software\ProxPilotTests\restore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&path).unwrap();
        key.set_value("ProxyOverride", &"<local>;localhost")
            .unwrap();
        let before = snapshot_from_key(&key).unwrap();
        let written = target_snapshot("http://127.0.0.1:7897").unwrap();
        // Simulate a failure between writing ProxyServer and ProxyEnable.
        key.set_value("ProxyServer", &"127.0.0.1:7897").unwrap();
        assert!(restore_from_key(&key, &before, &written).unwrap());
        assert_eq!(snapshot_from_key(&key).unwrap(), before);
        write_proxy(&key, "http://127.0.0.1:7897").unwrap();
        key.set_value("ProxyServer", &"127.0.0.1:7890").unwrap();
        assert!(!restore_from_key(&key, &before, &written).unwrap());
        assert_eq!(
            key.get_value::<String, _>("ProxyServer").unwrap(),
            "127.0.0.1:7890"
        );
        assert_eq!(key.get_value::<u32, _>("ProxyEnable").unwrap(), 1);
        assert_eq!(
            key.get_value::<String, _>("ProxyOverride").unwrap(),
            "<local>;localhost"
        );
        drop(key);
        hkcu.delete_subkey_all(path).unwrap();
    }
    #[test]
    fn repair_sets_target_and_preserves_bypass() {
        let path = format!(
            r"Software\ProxPilotTests\{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&path).unwrap();
        key.set_value("ProxyOverride", &"localhost;*.internal")
            .unwrap();
        write_proxy(&key, "http://127.0.0.1:7897").unwrap();
        assert_eq!(
            key.get_value::<String, _>("ProxyServer").unwrap(),
            "127.0.0.1:7897"
        );
        assert_eq!(key.get_value::<u32, _>("ProxyEnable").unwrap(), 1);
        assert_eq!(
            key.get_value::<String, _>("ProxyOverride").unwrap(),
            "localhost;*.internal"
        );
        assert!(write_proxy(&key, "socks5h://127.0.0.1:1080").is_err());
        drop(key);
        hkcu.delete_subkey_all(&path).unwrap();
    }
}
