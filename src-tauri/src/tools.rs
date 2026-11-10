//! Інструменти поверх shell: застосунки, відомості про пристрій, швидкі дії.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::Serialize;

use crate::adb::{self, Result};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Разова команда з виводом для внутрішніх потреб (без скасування).
fn run(serial: &str, cmd: &str) -> Result<String> {
    let never = AtomicBool::new(false);
    let result = adb::shell_output(serial, cmd, TIMEOUT, &never)?;
    match result.note {
        Some(note) if result.output.trim().is_empty() => Err(format!("The device did not answer ({note})")),
        _ => Ok(result.output),
    }
}

/// Бере рядок в одинарні лапки для shell пристрою.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------- застосунки ----------

pub fn parse_packages(raw: &str) -> Vec<String> {
    let mut list: Vec<String> = raw
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:"))
        .map(|p| p.trim().to_string())
        .filter(|p| valid_package(p))
        .collect();
    list.sort_by_key(|p| p.to_lowercase());
    list.dedup();
    list
}

pub fn valid_package(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 255
        && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

fn checked_package(p: &str) -> Result<&str> {
    if valid_package(p) {
        Ok(p)
    } else {
        Err("Invalid package name".into())
    }
}

pub fn list_apps(serial: &str, all: bool) -> Result<Vec<String>> {
    let cmd = if all { "pm list packages" } else { "pm list packages -3" };
    Ok(parse_packages(&run(serial, cmd)?))
}

fn monkey_failed(output: &str) -> bool {
    output.contains("No activities found") || output.contains("monkey aborted")
}

/// Запускає застосунок: спершу як ТВ-застосунок (LEANBACK), потім як звичайний.
pub fn launch_app(serial: &str, package: &str) -> Result<()> {
    let package = checked_package(package)?;
    for category in ["LEANBACK_LAUNCHER", "LAUNCHER"] {
        let out = run(
            serial,
            &format!("monkey -p {package} -c android.intent.category.{category} 1 2>&1"),
        )?;
        if !monkey_failed(&out) {
            return Ok(());
        }
    }
    Err(format!("{package} has no screen that can be opened"))
}

pub fn stop_app(serial: &str, package: &str) -> Result<()> {
    let package = checked_package(package)?;
    let out = run(serial, &format!("am force-stop {package} 2>&1"))?;
    if out.trim().is_empty() {
        Ok(())
    } else {
        Err(out.trim().to_string())
    }
}

// ---------- швидкі дії ----------

/// Готує адресу: додає `https://`, якщо схеми немає; відкидає керівні символи.
pub fn normalize_url(input: &str) -> Result<String> {
    let url = input.trim();
    if url.is_empty() {
        return Err("Enter a link".into());
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("The link contains invalid characters".into());
    }
    if url.contains("://") {
        Ok(url.to_string())
    } else {
        Ok(format!("https://{url}"))
    }
}

pub fn open_url(serial: &str, url: &str) -> Result<()> {
    let url = normalize_url(url)?;
    let out = run(
        serial,
        &format!("am start -a android.intent.action.VIEW -d {} 2>&1", sh_quote(&url)),
    )?;
    match out.lines().find(|l| l.contains("Error") || l.contains("Exception")) {
        Some(line) => Err(format!("The device could not open the link: {}", line.trim())),
        None => Ok(()),
    }
}

pub fn reboot(serial: &str) -> Result<()> {
    adb::open_service(serial, "reboot:").map(|_| ())
}

// ---------- відомості про пристрій ----------

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct DeviceInfo {
    pub model: String,
    pub manufacturer: String,
    pub android: String,
    pub sdk: String,
    pub ip: String,
    pub resolution: String,
    pub storage_total_kb: Option<u64>,
    pub storage_free_kb: Option<u64>,
}

const INFO_CMD: &str = "echo @@model; getprop ro.product.model; \
    echo @@manufacturer; getprop ro.product.manufacturer; \
    echo @@android; getprop ro.build.version.release; \
    echo @@sdk; getprop ro.build.version.sdk; \
    echo @@ip; ip -4 -o addr show scope global 2>/dev/null; \
    echo @@size; wm size 2>/dev/null; \
    echo @@df; df -k /data 2>/dev/null";

pub fn parse_device_info(raw: &str) -> DeviceInfo {
    let mut info = DeviceInfo::default();
    let mut section = "";
    let mut ips: Vec<String> = Vec::new();

    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(name) = line.strip_prefix("@@") {
            section = name;
            continue;
        }
        match section {
            "model" if info.model.is_empty() => info.model = line.to_string(),
            "manufacturer" if info.manufacturer.is_empty() => info.manufacturer = line.to_string(),
            "android" if info.android.is_empty() => info.android = line.to_string(),
            "sdk" if info.sdk.is_empty() => info.sdk = line.to_string(),
            "ip" => {
                let mut parts = line.split_whitespace();
                if parts.any(|p| p == "inet") {
                    if let Some(addr) = parts.next() {
                        let addr = addr.split('/').next().unwrap_or(addr).to_string();
                        if !ips.contains(&addr) {
                            ips.push(addr);
                        }
                    }
                }
            }
            // "Override size" йде після "Physical size" і є актуальним.
            "size" => {
                if let Some((_, size)) = line.split_once(':') {
                    info.resolution = size.trim().to_string();
                }
            }
            "df" => {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() >= 4 {
                    if let (Ok(total), Ok(free)) = (f[1].parse::<u64>(), f[3].parse::<u64>()) {
                        info.storage_total_kb = Some(total);
                        info.storage_free_kb = Some(free);
                    }
                }
            }
            _ => {}
        }
    }
    info.ip = ips.join(", ");
    info
}

pub fn device_info(serial: &str) -> Result<DeviceInfo> {
    Ok(parse_device_info(&run(serial, INFO_CMD)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_packages() {
        let raw = "package:org.videolan.vlc\r\npackage:com.Zeta.app\npackage:com.alpha.app\n\njunk\npackage:bad name\npackage:com.alpha.app\n";
        assert_eq!(
            parse_packages(raw),
            ["com.alpha.app", "com.Zeta.app", "org.videolan.vlc"]
        );
        assert!(parse_packages("").is_empty());
    }

    #[test]
    fn validates_packages() {
        assert!(valid_package("com.example.app_1"));
        assert!(!valid_package(""));
        assert!(!valid_package("com.example; reboot"));
        assert!(!valid_package("com.example$(id)"));
        assert!(launch_app("x", "a b").is_err());
        assert!(stop_app("x", "a;b").is_err());
    }

    #[test]
    fn detects_monkey_failure() {
        assert!(monkey_failed("** No activities found to run, monkey aborted."));
        assert!(!monkey_failed("Events injected: 1\n## Network stats: elapsed time=20ms"));
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(normalize_url(" example.com/a?b=1 ").unwrap(), "https://example.com/a?b=1");
        assert_eq!(normalize_url("http://x.y").unwrap(), "http://x.y");
        assert_eq!(normalize_url("vnd.youtube://abc").unwrap(), "vnd.youtube://abc");
        assert!(normalize_url("").is_err());
        assert!(normalize_url("a b").is_err());
        assert!(normalize_url("a\nb").is_err());
    }

    #[test]
    fn quotes_for_shell() {
        assert_eq!(sh_quote("a'b"), r"'a'\''b'");
        assert_eq!(sh_quote("$(reboot);`x`"), "'$(reboot);`x`'");
    }

    #[test]
    fn parses_device_info() {
        let raw = "@@model\nXK03H\n@@manufacturer\nXGIMI\n@@android\n11\n@@sdk\n30\n@@ip\n\
            3: wlan0    inet 192.168.1.50/24 brd 192.168.1.255 scope global wlan0\\       valid_lft forever\n\
            7: tailscale0    inet 100.65.1.2/32 scope global tailscale0\n\
            @@size\nPhysical size: 3840x2160\nOverride size: 1920x1080\n\
            @@df\nFilesystem     1K-blocks    Used Available Use% Mounted on\n/dev/block/dm-5 24000000 9000000 15000000  38% /data\n";
        assert_eq!(
            parse_device_info(raw),
            DeviceInfo {
                model: "XK03H".into(),
                manufacturer: "XGIMI".into(),
                android: "11".into(),
                sdk: "30".into(),
                ip: "192.168.1.50, 100.65.1.2".into(),
                resolution: "1920x1080".into(),
                storage_total_kb: Some(24_000_000),
                storage_free_kb: Some(15_000_000),
            }
        );
        // Порожній або неповний вивід не ламає розбір.
        assert_eq!(parse_device_info(""), DeviceInfo::default());
        assert_eq!(parse_device_info("@@model\n@@df\ngarbage\n").model, "");
    }
}
