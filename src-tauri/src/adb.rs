//! Клієнт до локального adb-сервера (сокет 127.0.0.1:5037).
//!
//! Замість запуску `adb shell ...` на кожне натискання тримаємо одну
//! постійну shell-сесію на пристрій і пишемо команди в неї.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

pub type Result<T> = std::result::Result<T, String>;

const DEFAULT_PORT: u16 = 5555;

fn server_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 5037))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Device {
    pub serial: String,
    /// `device` (готовий), `unauthorized`, `offline` тощо.
    pub state: String,
    pub model: Option<String>,
}

// ---------- пошук і запуск adb ----------

fn adb_file_name() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

/// Шукає бінарник adb: змінна середовища, поруч із програмою, типові місця, PATH.
/// GUI-програми на macOS не успадковують PATH з термінала, тому типові
/// місця перевіряємо явно.
fn find_adb() -> PathBuf {
    let name = adb_file_name();
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(p) = std::env::var("ADB_REMOTE_ADB") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            candidates.push(dir.join("platform-tools").join(name));
            candidates.push(dir.join("../Resources/platform-tools").join(name));
        }
    }
    if cfg!(windows) {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join("Android/Sdk/platform-tools").join(name));
        }
    } else {
        candidates.push(PathBuf::from("/opt/homebrew/bin/adb"));
        candidates.push(PathBuf::from("/usr/local/bin/adb"));
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(&home).join("Library/Android/sdk/platform-tools/adb"));
            candidates.push(PathBuf::from(&home).join("Android/Sdk/platform-tools/adb"));
        }
    }

    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn start_server() -> Result<()> {
    let adb = find_adb();
    let mut cmd = Command::new(&adb);
    cmd.arg("start-server");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd
        .output()
        .map_err(|e| format!("Could not start adb ({}): {e}", adb.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "adb start-server failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// ---------- протокол adb-сервера ----------

fn open() -> Result<TcpStream> {
    let addr = server_addr();
    let timeout = Duration::from_secs(2);
    let stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(_) => {
            start_server()?;
            TcpStream::connect_timeout(&addr, timeout)
                .map_err(|e| format!("The adb server is not responding: {e}"))?
        }
    };
    let _ = stream.set_nodelay(true);
    // Запис не має висіти вічно, якщо зв'язок із пристроєм завис.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    Ok(stream)
}

fn io_err(e: std::io::Error) -> String {
    format!("adb connection error: {e}")
}

fn read_prefixed(s: &mut TcpStream) -> Result<String> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).map_err(io_err)?;
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|h| usize::from_str_radix(h, 16).ok())
        .ok_or("Invalid response from the adb server")?;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).map_err(io_err)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn send_request(s: &mut TcpStream, payload: &str) -> Result<()> {
    // Довжина передається чотирма шістнадцятковими цифрами.
    if payload.len() > 0xffff {
        return Err("The command is too long".into());
    }
    s.write_all(format!("{:04x}{payload}", payload.len()).as_bytes())
        .map_err(io_err)?;
    let mut status = [0u8; 4];
    s.read_exact(&mut status).map_err(io_err)?;
    match &status {
        b"OKAY" => Ok(()),
        b"FAIL" => Err(humanize(&read_prefixed(s)?)),
        _ => Err("Invalid response from the adb server".into()),
    }
}

fn host_query(payload: &str, timeout: Duration) -> Result<String> {
    let mut s = open()?;
    s.set_read_timeout(Some(timeout)).map_err(io_err)?;
    send_request(&mut s, payload)?;
    read_prefixed(&mut s)
}

fn humanize(msg: &str) -> String {
    let m = msg.trim();
    if m.contains("unauthorized") {
        "Device not authorized: confirm \"Allow debugging\" on the device screen".into()
    } else if m.contains("offline") {
        "Device is offline".into()
    } else if m.contains("not found") {
        "Device not found. Connect again".into()
    } else {
        m.to_string()
    }
}

// ---------- публічні операції ----------

pub fn parse_devices(raw: &str) -> Vec<Device> {
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts
                .find_map(|p| p.strip_prefix("model:"))
                .map(|m| m.replace('_', " "));
            Some(Device { serial, state, model })
        })
        .collect()
}

pub fn devices() -> Result<Vec<Device>> {
    host_query("host:devices-l", Duration::from_secs(5)).map(|raw| parse_devices(&raw))
}

/// `192.168.1.50` -> `192.168.1.50:5555`; перевіряє допустимі символи.
pub fn normalize_addr(input: &str) -> Result<String> {
    let addr = input.trim();
    if addr.is_empty() {
        return Err("Enter the device IP address".into());
    }
    if !addr
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
    {
        return Err("The address contains invalid characters".into());
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => Ok(addr.to_string()),
        Some(_) => Err("Invalid port in the address".into()),
        None => Ok(format!("{addr}:{DEFAULT_PORT}")),
    }
}

/// Підключає пристрій по мережі. Повертає його serial (`ip:port`).
pub fn connect(input: &str) -> Result<String> {
    let addr = normalize_addr(input)?;
    let reply = host_query(&format!("host:connect:{addr}"), Duration::from_secs(15))?;
    if reply.starts_with("connected") || reply.starts_with("already connected") {
        Ok(addr)
    } else if reply.contains("authenticate") {
        Err(humanize("unauthorized"))
    } else {
        Err(format!("Could not connect to {addr}: {}", reply.trim()))
    }
}

pub fn disconnect(serial: &str) -> Result<()> {
    let addr = normalize_addr(serial)?;
    host_query(&format!("host:disconnect:{addr}"), Duration::from_secs(5)).map(|_| ())
}

/// Формує команду `input text` з екрануванням для shell.
pub fn text_command(text: &str) -> Result<String> {
    if text.is_empty() {
        return Err("Text is empty".into());
    }
    if !text.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        return Err(
            "Without screen view only Latin letters, digits and basic symbols can be typed. Turn on screen view to type other alphabets"
                .into(),
        );
    }
    let escaped = text.replace(' ', "%s").replace('\'', r"'\''");
    Ok(format!("input text '{escaped}'"))
}

pub fn key_command(keycode: u32, longpress: bool) -> String {
    if longpress {
        format!("input keyevent --longpress {keycode}")
    } else {
        format!("input keyevent {keycode}")
    }
}

// ---------- сервіси пристрою ----------

/// Відкриває потік до сервісу на пристрої (`shell:...`, `localabstract:...`, `sync:`).
pub fn open_service(serial: &str, service: &str) -> Result<TcpStream> {
    let mut stream = open()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(io_err)?;
    send_request(&mut stream, &format!("host:transport:{serial}"))?;
    send_request(&mut stream, service)?;
    stream.set_read_timeout(None).map_err(io_err)?;
    Ok(stream)
}

/// Копіює дані у файл на пристрої (протокол sync, аналог `adb push`).
pub fn push(serial: &str, data: &[u8], remote_path: &str) -> Result<()> {
    let mut s = open_service(serial, "sync:")?;
    s.set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(io_err)?;

    let mut out = Vec::with_capacity(data.len() + 256);
    let target = format!("{remote_path},33188"); // 0100644
    out.extend_from_slice(b"SEND");
    out.extend_from_slice(&(target.len() as u32).to_le_bytes());
    out.extend_from_slice(target.as_bytes());
    for chunk in data.chunks(64 * 1024) {
        out.extend_from_slice(b"DATA");
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    out.extend_from_slice(b"DONE");
    out.extend_from_slice(&mtime.to_le_bytes());
    s.write_all(&out).map_err(io_err)?;

    let mut reply = [0u8; 8];
    s.read_exact(&mut reply).map_err(io_err)?;
    let len = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]) as usize;
    match &reply[..4] {
        b"OKAY" => Ok(()),
        b"FAIL" => {
            let mut msg = vec![0u8; len.min(4096)];
            s.read_exact(&mut msg).map_err(io_err)?;
            Err(format!(
                "Could not copy the file to the device: {}",
                String::from_utf8_lossy(&msg)
            ))
        }
        _ => Err("Invalid response while copying the file".into()),
    }
}

// ---------- разові команди з виводом ----------

#[derive(Debug, Serialize, PartialEq)]
pub struct ExecResult {
    pub output: String,
    /// Чому вивід неповний: зупинено, тайм-аут або обрізано.
    pub note: Option<String>,
}

const MAX_OUTPUT: usize = 256 * 1024;

/// Виконує shell-команду й повертає її вивід (stdout разом зі stderr).
/// Зупиняється за тайм-аутом, за прапорцем `cancel` або коли виводу забагато;
/// закриття з'єднання завершує команду на пристрої.
pub fn shell_output(
    serial: &str,
    cmd: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<ExecResult> {
    if cmd.trim().is_empty() {
        return Err("Enter a command".into());
    }
    let mut stream = open_service(serial, &format!("shell:{cmd}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(io_err)?;

    let deadline = std::time::Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let note = loop {
        if cancel.load(Ordering::SeqCst) {
            break Some("stopped".to_string());
        }
        if std::time::Instant::now() >= deadline {
            break Some(format!("timed out after {} s", timeout.as_secs()));
        }
        match stream.read(&mut buf) {
            Ok(0) => break None,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_OUTPUT {
                    out.truncate(MAX_OUTPUT);
                    break Some("output truncated".to_string());
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(io_err(e)),
        }
    };
    let _ = stream.shutdown(std::net::Shutdown::Both);

    Ok(ExecResult {
        output: String::from_utf8_lossy(&out).replace("\r\n", "\n"),
        note,
    })
}

// ---------- постійні shell-сесії ----------

struct Session {
    stream: TcpStream,
    alive: Arc<AtomicBool>,
}

impl Session {
    fn open(serial: &str) -> Result<Self> {
        // `shell:sh` — сирий (без PTY) інтерпретатор, що читає команди зі stdin.
        let stream = open_service(serial, "shell:sh")?;

        let alive = Arc::new(AtomicBool::new(true));
        let mut reader = stream.try_clone().map_err(io_err)?;
        let flag = alive.clone();
        std::thread::spawn(move || {
            // Вичитуємо вивід, щоб буфер не переповнився, і ловимо обрив.
            let mut buf = [0u8; 4096];
            while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
            flag.store(false, Ordering::SeqCst);
        });

        Ok(Session { stream, alive })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn run(&mut self, cmd: &str) -> Result<()> {
        self.stream
            .write_all(format!("{cmd}\n").as_bytes())
            .and_then(|_| self.stream.flush())
            .map_err(io_err)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Сесії за пристроями. Загальний замок береться лише щоб знайти комірку
/// пристрою, тож завислий пристрій не блокує решту.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Arc<Mutex<Option<Session>>>>>);

impl Sessions {
    /// Виконує команду в постійній сесії пристрою; при обриві перепідключається один раз.
    pub fn shell(&self, serial: &str, cmd: &str) -> Result<()> {
        let slot = {
            let mut map = self.0.lock().map_err(|_| "Internal state error")?;
            map.entry(serial.to_string()).or_default().clone()
        };
        let mut slot = slot.lock().map_err(|_| "Internal state error")?;
        let mut last_err = String::new();
        for _ in 0..2 {
            if !slot.as_ref().is_some_and(Session::is_alive) {
                *slot = Some(Session::open(serial)?);
            }
            match slot.as_mut().expect("just set").run(cmd) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    *slot = None;
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    pub fn close(&self, serial: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(serial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_list() {
        let raw = "192.168.1.50:5555     device product:sabrina model:Chromecast_HD device:boreal transport_id:3\n\
                   ABC123                 unauthorized transport_id:4\n\
                   10.0.0.2:5555          offline\n";
        let d = parse_devices(raw);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].serial, "192.168.1.50:5555");
        assert_eq!(d[0].state, "device");
        assert_eq!(d[0].model.as_deref(), Some("Chromecast HD"));
        assert_eq!(d[1].state, "unauthorized");
        assert_eq!(d[1].model, None);
        assert_eq!(d[2].state, "offline");
        assert!(parse_devices("").is_empty());
    }

    #[test]
    fn normalizes_addresses() {
        assert_eq!(normalize_addr(" 192.168.1.50 ").unwrap(), "192.168.1.50:5555");
        assert_eq!(normalize_addr("192.168.1.50:5556").unwrap(), "192.168.1.50:5556");
        assert_eq!(normalize_addr("tv-box.local").unwrap(), "tv-box.local:5555");
        assert!(normalize_addr("").is_err());
        assert!(normalize_addr("1.2.3.4:abc").is_err());
        assert!(normalize_addr("1.2.3.4; rm -rf /").is_err());
    }

    #[test]
    fn escapes_text() {
        assert_eq!(text_command("hello world").unwrap(), "input text 'hello%sworld'");
        assert_eq!(text_command("it's").unwrap(), r"input text 'it'\''s'");
        assert_eq!(text_command("$(reboot);`x`").unwrap(), "input text '$(reboot);`x`'");
        assert!(text_command("привіт").is_err());
        assert!(text_command("a\nb").is_err());
        assert!(text_command("").is_err());
    }

    #[test]
    fn builds_key_commands() {
        assert_eq!(key_command(23, false), "input keyevent 23");
        assert_eq!(key_command(26, true), "input keyevent --longpress 26");
    }

    #[test]
    fn rejects_bad_commands() {
        let never = AtomicBool::new(false);
        assert!(shell_output("x", "  ", Duration::from_secs(1), &never).is_err());
    }

    /// Справжній пристрій: `ADB_REMOTE_TEST_SERIAL=ip:5555 cargo test -- --ignored live_shell --nocapture`.
    #[test]
    #[ignore]
    fn live_shell() {
        let serial = std::env::var("ADB_REMOTE_TEST_SERIAL").expect("ADB_REMOTE_TEST_SERIAL");
        let never = AtomicBool::new(false);

        let r = shell_output(&serial, "echo hello; echo err 1>&2", Duration::from_secs(5), &never).unwrap();
        assert_eq!(r.output, "hello\nerr\n");
        assert_eq!(r.note, None);

        let started = std::time::Instant::now();
        let r = shell_output(&serial, "echo start; sleep 30", Duration::from_secs(1), &never).unwrap();
        assert_eq!(r.output, "start\n");
        assert_eq!(r.note.as_deref(), Some("timed out after 1 s"));
        assert!(started.elapsed() < Duration::from_secs(3));

        let cancelled = AtomicBool::new(true);
        let r = shell_output(&serial, "sleep 30", Duration::from_secs(5), &cancelled).unwrap();
        assert_eq!(r.note.as_deref(), Some("stopped"));

        let info = crate::tools::device_info(&serial).unwrap();
        println!("{info:?}");
        assert!(!info.model.is_empty() && !info.android.is_empty());
        let apps = crate::tools::list_apps(&serial, false).unwrap();
        let all = crate::tools::list_apps(&serial, true).unwrap();
        println!("apps: {} third-party, {} total; first: {:?}", apps.len(), all.len(), apps.first());
        assert!(all.len() >= apps.len() && !all.is_empty());
    }

    /// Потребує встановленого adb: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn talks_to_real_server() {
        devices().expect("adb-сервер має відповісти");
        assert!(connect("127.0.0.1:1").is_err());
    }
}
