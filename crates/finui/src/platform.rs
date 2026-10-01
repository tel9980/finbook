//! 设备身份采集（登录设备绑定用）
//!
//! 需要一个跨平台、稳定、零额外依赖的机器指纹：
//! - Windows 读注册表 `MachineGuid`（装系统时生成，重装才变）
//! - Linux 读 `/etc/machine-id`（systemd 机器标识）
//! - macOS 读 `IOPlatformUUID`
//! - 都拿不到时退化为「主机名 + 用户名」的 FNV 哈希——不完美，
//!   但比"任何机器都能登录"强；管理员随时可以重置绑定。

use std::path::Path;

use findb::security::DeviceIdentity;

/// 采集当前设备身份。id 稳定不变；name 供管理员在安全中心辨认。
pub fn device_identity() -> DeviceIdentity {
    let id = platform_uid().unwrap_or_else(|| format!("fbx-{fallback:016x}", fallback = fallback_hash()));
    DeviceIdentity::new(&id, &host_label())
}

/// 操作系统级机器标识
#[cfg(windows)]
fn platform_uid() -> Option<String> {
    let out = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    // 形如 "    MachineGuid    REG_SZ    1234abcd-..."
    let val = s
        .lines()
        .find(|l| l.contains("MachineGuid"))?
        .split_whitespace()
        .last()?
        .to_string();
    (!val.is_empty()).then(|| format!("win-{val}"))
}

#[cfg(target_os = "macos")]
fn platform_uid() -> Option<String> {
    let out = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    // 形如 "IOPlatformUUID" = "XXXX-XXXX"
    for line in s.lines() {
        if line.contains("IOPlatformUUID") {
            let parts: Vec<&str> = line.split('"').collect();
            if parts.len() >= 4 {
                return Some(format!("mac-{}", parts[3]));
            }
        }
    }
    None
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_uid() -> Option<String> {
    for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(format!("lnx-{t}"));
            }
        }
    }
    None
}

/// 展示名：优先主机名，让管理员一眼认出是哪台机器
fn host_label() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty());
    match host {
        Some(h) => h.trim().to_string(),
        None => "本机".to_string(),
    }
}

/// 兜底指纹：主机名 + 用户名的 FNV-1a 64 位哈希
fn fallback_hash() -> u64 {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    let raw = format!("{}|{}", host_label(), user);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 用系统默认程序打开一个文件（跨平台）。
///
/// 打印预览、附件"打开"都用它：写到一个临时文件再用关联程序打开。
/// Linux 用 `xdg-open`，macOS 用 `open`，Windows 用 `cmd /c start`。
pub fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &path.to_string_lossy()])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("无法打开文件：{e}"))
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("无法打开文件：{e}"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("无法打开文件：{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_and_named() {
        let a = device_identity();
        let b = device_identity();
        assert_eq!(a.id, b.id, "同一台机器两次采集应一致");
        assert!(!a.id.is_empty());
        assert!(!a.name.is_empty());
    }
}
