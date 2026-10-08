use windows_sys::Win32::Networking::WinInet::{
    InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
use winreg::RegKey;

const INET_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

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
    let url = reqwest::Url::parse(proxy).map_err(|_| "代理地址无效")?;
    if url.scheme() != "http" {
        return Err("Windows 系统代理需要 HTTP/mixed 端口；SOCKS 出口不能自动配置".into());
    }
    let host = url.host_str().ok_or("代理缺少主机")?;
    let port = url.port_or_known_default().ok_or("代理缺少端口")?;
    k.set_value("ProxyServer", &format!("{}:{}", host, port))
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
