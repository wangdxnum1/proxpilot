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
pub fn enable() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let k = hkcu
        .open_subkey_with_flags(INET_KEY, KEY_SET_VALUE)
        .map_err(|e| format!("打开注册表失败: {}", e))?;
    k.set_value("ProxyEnable", &1u32)
        .map_err(|e| format!("写入注册表失败: {}", e))?;
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
