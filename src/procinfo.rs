//! 通过端口反查进程，识别客户端是谁。

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub struct ClientInfo {
    pub name: String,
    #[allow(dead_code)]
    pub process: String,
    #[allow(dead_code)]
    pub path: String,
}

/// Windows 表由 DWORD 和变长行组成；u32 缓冲区满足表的对齐要求。
fn listener_table(family: u32) -> Option<Vec<u32>> {
    let mut size = 0;
    let status = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != ERROR_INSUFFICIENT_BUFFER && status != NO_ERROR {
        return None;
    }
    for _ in 0..4 {
        let mut table = vec![0u32; (size as usize).div_ceil(4)];
        let status = unsafe {
            GetExtendedTcpTable(
                table.as_mut_ptr().cast(),
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == NO_ERROR {
            return Some(table);
        }
        if status != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
    }
    None
}

/// 找到监听指定端口的进程 PID
pub fn find_listener_pid(port: u16) -> Option<u32> {
    find_listener_for(port, None)
}
pub fn find_listener_pid_at(host: &str, port: u16) -> Option<u32> {
    use std::net::ToSocketAddrs;
    let targets: Vec<_> = (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .ok()?
        .map(|a| a.ip())
        .collect();
    let found: std::collections::HashSet<_> = targets
        .iter()
        .filter_map(|ip| {
            find_listener_for(port, Some(*ip)).or_else(|| {
                // 没有匹配的 IPv4 监听时，IPv4 连接仅可能由 IPv6 wildcard 双栈监听接收。
                // 后续直连 API 成功才视为可用，IPv6-only 接口的 IPv4 请求会失败。
                if ip.is_ipv4() {
                    find_listener_for(
                        port,
                        Some(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)),
                    )
                } else {
                    None
                }
            })
        })
        .collect();
    if found.len() == 1 {
        found.into_iter().next()
    } else {
        None
    }
}
fn find_listener_for(port: u16, target: Option<std::net::IpAddr>) -> Option<u32> {
    for family in [AF_INET, AF_INET6] {
        if target.is_some_and(|ip| ip.is_ipv4() != (family == AF_INET)) {
            continue;
        }
        let Some(table) = listener_table(family as u32) else {
            continue;
        };
        let count = *table.first()? as usize;
        let (offset, row_size) = if family == AF_INET {
            (
                std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table),
                std::mem::size_of::<MIB_TCPROW_OWNER_PID>(),
            )
        } else {
            (
                std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
                std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>(),
            )
        };
        let available = (table.len() * 4).saturating_sub(offset) / row_size;
        for i in 0..count.min(available) {
            // 已校验缓冲区边界，表行均为 POD 整数；不创建未对齐引用。
            let row = unsafe { table.as_ptr().cast::<u8>().add(offset + i * row_size) };
            let (local_port, pid, address) = unsafe {
                if family == AF_INET {
                    let row = row.cast::<MIB_TCPROW_OWNER_PID>().read_unaligned();
                    (
                        row.dwLocalPort,
                        row.dwOwningPid,
                        std::net::IpAddr::V4(std::net::Ipv4Addr::from(
                            row.dwLocalAddr.to_le_bytes(),
                        )),
                    )
                } else {
                    let row = row.cast::<MIB_TCP6ROW_OWNER_PID>().read_unaligned();
                    (
                        row.dwLocalPort,
                        row.dwOwningPid,
                        std::net::IpAddr::V6(std::net::Ipv6Addr::from(row.ucLocalAddr)),
                    )
                }
            };
            if u16::from_be(local_port as u16) == port
                && target.is_none_or(|ip| address.is_unspecified() || address == ip)
            {
                return Some(pid);
            }
        }
    }
    None
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// 识别监听该端口的是哪家客户端
pub fn identify_client(port: u16) -> ClientInfo {
    identify_pid(find_listener_pid(port))
}
pub fn identify_pid(pid: Option<u32>) -> ClientInfo {
    let (path, process) = match pid {
        Some(p) => {
            let path = image_path(p).unwrap_or_default();
            let process = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (path, process)
        }
        None => (String::new(), String::new()),
    };
    let name = client_display_name(&path, &process);
    ClientInfo {
        name,
        process,
        path,
    }
}

pub fn client_display_name(path: &str, process: &str) -> String {
    let l = format!("{} {}", path, process).to_lowercase();
    if l.contains("cutecloud") {
        "CuteCloud".into()
    } else if l.contains("flclash") {
        "FlClash".into()
    } else if l.contains("verge") {
        "Clash Verge".into()
    } else if l.contains("clash for windows") || l.contains("clash-win64") {
        "Clash for Windows".into()
    } else if l.contains("mihomo") {
        "mihomo".into()
    } else if l.contains("clash") {
        "Clash 内核".into()
    } else if !process.is_empty() {
        process.into()
    } else {
        "未知客户端".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn identifies_current_ipv4_listener_and_process_without_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(find_listener_pid(port), Some(std::process::id()));
        let client = identify_client(port);
        assert!(!client.path.is_empty());
        assert_eq!(
            client.process,
            std::env::current_exe()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
        );
    }

    #[test]
    fn identifies_current_ipv6_listener_without_commands() {
        let listener = TcpListener::bind("[::1]:0").unwrap();
        assert_eq!(
            find_listener_pid(listener.local_addr().unwrap().port()),
            Some(std::process::id())
        );
    }
}

#[cfg(test)]
mod address_tests {
    #[test]
    fn same_port_distinct_addresses_resolve_without_family_confusion() {
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        let _v6 = std::net::TcpListener::bind(("::1", port)).unwrap();
        assert_eq!(
            super::find_listener_pid_at("127.0.0.1", port),
            Some(std::process::id())
        );
        assert_eq!(
            super::find_listener_pid_at("::1", port),
            Some(std::process::id())
        );
        assert_eq!(super::find_listener_pid_at("127.0.0.2", port), None);
    }
}
