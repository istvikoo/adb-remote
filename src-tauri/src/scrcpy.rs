//! Клієнт scrcpy-server: трансляція екрана й швидке керування.
//!
//! Протокол внутрішній для scrcpy і прив'язаний до версії сервера, тому
//! сервер вбудовано в програму і версія зафіксована.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::adb::{self, Result};

const SERVER_JAR: &[u8] = include_bytes!("../resources/scrcpy-server");
const SERVER_VERSION: &str = "5.0.1";
const DEVICE_PATH: &str = "/data/local/tmp/adb-remote-scrcpy-server.jar";

const CODEC_H264: u32 = 0x6832_3634;
const FLAG_SESSION: u64 = 1 << 63;
const FLAG_CONFIG: u64 = 1 << 62;
const FLAG_KEY: u64 = 1 << 61;
const MAX_PACKET: usize = 32 * 1024 * 1024;

/// Типи повідомлень, що йдуть в інтерфейс (перший байт).
pub const OUT_SESSION: u8 = 0;
pub const OUT_CONFIG: u8 = 1;
pub const OUT_KEY_FRAME: u8 = 2;
pub const OUT_DELTA_FRAME: u8 = 3;
pub const OUT_ENDED: u8 = 255;

// ---------- відеопотік ----------

#[derive(Debug, PartialEq)]
pub enum Packet {
    Session { width: u32, height: u32 },
    Config(Vec<u8>),
    Frame { pts: u64, key: bool, data: Vec<u8> },
}

fn read_err(e: std::io::Error) -> String {
    format!("The video stream was interrupted: {e}")
}

pub fn read_packet(r: &mut impl Read) -> Result<Packet> {
    let mut h = [0u8; 12];
    r.read_exact(&mut h).map_err(read_err)?;
    let head = u64::from_be_bytes(h[..8].try_into().unwrap());
    let tail = u32::from_be_bytes(h[8..].try_into().unwrap());

    if head & FLAG_SESSION != 0 {
        return Ok(Packet::Session {
            width: head as u32,
            height: tail,
        });
    }

    let size = tail as usize;
    if size > MAX_PACKET {
        return Err(format!("Invalid video packet size: {size}"));
    }
    let mut data = vec![0u8; size];
    r.read_exact(&mut data).map_err(read_err)?;

    if head & FLAG_CONFIG != 0 {
        Ok(Packet::Config(data))
    } else {
        Ok(Packet::Frame {
            pts: head & !(FLAG_SESSION | FLAG_CONFIG | FLAG_KEY),
            key: head & FLAG_KEY != 0,
            data,
        })
    }
}

/// Читає початок відеосокета: службовий байт, назву пристрою, кодек.
pub fn read_video_header(r: &mut impl Read) -> Result<String> {
    let mut dummy = [0u8; 1];
    r.read_exact(&mut dummy).map_err(read_err)?;
    let mut name = [0u8; 64];
    r.read_exact(&mut name).map_err(read_err)?;
    let mut codec = [0u8; 4];
    r.read_exact(&mut codec).map_err(read_err)?;
    match u32::from_be_bytes(codec) {
        CODEC_H264 => {}
        0 | 1 => return Err("The device could not start screen capture".into()),
        other => return Err(format!("Unsupported video codec: {other:#x}")),
    }
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    Ok(String::from_utf8_lossy(&name[..end]).into_owned())
}

/// Рядок кодека для WebCodecs (`avc1.PPCCLL`) з SPS у пакеті конфігурації.
pub fn avc_codec_string(config: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 3 < config.len() {
        if config[i] == 0 && config[i + 1] == 0 && config[i + 2] == 1 {
            let nal = &config[i + 3..];
            if nal.len() >= 4 && nal[0] & 0x1f == 7 {
                return Some(format!("avc1.{:02x}{:02x}{:02x}", nal[1], nal[2], nal[3]));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    None
}

/// Перетворює пакети сервера на повідомлення для інтерфейсу.
/// Конфігурацію (SPS/PPS) приклеює до наступного кадру, як це робить scrcpy.
#[derive(Default)]
pub struct Framer {
    pending_config: Vec<u8>,
}

impl Framer {
    pub fn convert(&mut self, packet: Packet) -> Result<Vec<u8>> {
        match packet {
            Packet::Session { width, height } => {
                let mut m = vec![OUT_SESSION];
                m.extend_from_slice(&width.to_be_bytes());
                m.extend_from_slice(&height.to_be_bytes());
                Ok(m)
            }
            Packet::Config(data) => {
                let codec = avc_codec_string(&data)
                    .ok_or("H.264 parameters not found in the video stream")?;
                self.pending_config = data;
                let mut m = vec![OUT_CONFIG];
                m.extend_from_slice(codec.as_bytes());
                Ok(m)
            }
            Packet::Frame { pts, key, data } => {
                let config = std::mem::take(&mut self.pending_config);
                let mut m = Vec::with_capacity(9 + config.len() + data.len());
                m.push(if key { OUT_KEY_FRAME } else { OUT_DELTA_FRAME });
                m.extend_from_slice(&pts.to_be_bytes());
                m.extend_from_slice(&config);
                m.extend_from_slice(&data);
                Ok(m)
            }
        }
    }
}

pub fn ended_message(reason: &str) -> Vec<u8> {
    let mut m = vec![OUT_ENDED];
    m.extend_from_slice(reason.as_bytes());
    m
}

// ---------- повідомлення керування ----------

pub const ACTION_DOWN: u8 = 0;
pub const ACTION_UP: u8 = 1;
/// Ідентифікатор «пальця» (SC_POINTER_ID_GENERIC_FINGER = -2).
const POINTER_FINGER: u64 = u64::MAX - 1;

pub fn msg_keycode(action: u8, keycode: u32, repeat: u32, metastate: u32) -> Vec<u8> {
    let mut m = vec![0, action];
    m.extend_from_slice(&keycode.to_be_bytes());
    m.extend_from_slice(&repeat.to_be_bytes());
    m.extend_from_slice(&metastate.to_be_bytes());
    m
}

pub fn msg_text(text: &str) -> Vec<u8> {
    let mut m = vec![1];
    m.extend_from_slice(&(text.len() as u32).to_be_bytes());
    m.extend_from_slice(text.as_bytes());
    m
}

fn push_position(m: &mut Vec<u8>, x: i32, y: i32, width: u16, height: u16) {
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    m.extend_from_slice(&width.to_be_bytes());
    m.extend_from_slice(&height.to_be_bytes());
}

#[allow(clippy::too_many_arguments)]
pub fn msg_touch(
    action: u8,
    pointer_id: u64,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    pressure: u16,
    action_button: u32,
    buttons: u32,
) -> Vec<u8> {
    let mut m = vec![2, action];
    m.extend_from_slice(&pointer_id.to_be_bytes());
    push_position(&mut m, x, y, width, height);
    m.extend_from_slice(&pressure.to_be_bytes());
    m.extend_from_slice(&action_button.to_be_bytes());
    m.extend_from_slice(&buttons.to_be_bytes());
    m
}

/// Значення прокрутки в діапазоні [-16, 16] кодується як i16 з фіксованою комою.
fn scroll_fixed(v: f32) -> i16 {
    let n = (v / 16.0).clamp(-1.0, 1.0);
    if n >= 1.0 {
        i16::MAX
    } else {
        (n * 32768.0) as i16
    }
}

pub fn msg_scroll(x: i32, y: i32, width: u16, height: u16, h: f32, v: f32, buttons: u32) -> Vec<u8> {
    let mut m = vec![3];
    push_position(&mut m, x, y, width, height);
    m.extend_from_slice(&scroll_fixed(h).to_be_bytes());
    m.extend_from_slice(&scroll_fixed(v).to_be_bytes());
    m.extend_from_slice(&buttons.to_be_bytes());
    m
}

/// Кладе текст у буфер обміну пристрою і (за бажанням) одразу вставляє.
pub fn msg_set_clipboard(sequence: u64, paste: bool, text: &str) -> Vec<u8> {
    let mut m = vec![9];
    m.extend_from_slice(&sequence.to_be_bytes());
    m.push(paste as u8);
    m.extend_from_slice(&(text.len() as u32).to_be_bytes());
    m.extend_from_slice(text.as_bytes());
    m
}

// ---------- сесія трансляції ----------

pub struct MirrorOptions {
    pub max_size: u16,
    pub bit_rate: u32,
    pub max_fps: u8,
}

fn server_command(scid: u32, o: &MirrorOptions) -> String {
    format!(
        "shell:CLASSPATH={DEVICE_PATH} app_process / com.genymobile.scrcpy.Server {SERVER_VERSION} \
         scid={scid:08x} log_level=info audio=false tunnel_forward=true clipboard_autosync=false \
         max_size={} video_bit_rate={} max_fps={}",
        o.max_size, o.bit_rate, o.max_fps
    )
}

fn drain(mut stream: TcpStream, alive: Arc<AtomicBool>, log: Option<Arc<Mutex<String>>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(n) if n > 0 => {
                    if let Some(log) = &log {
                        if let Ok(mut l) = log.lock() {
                            l.push_str(&String::from_utf8_lossy(&buf[..n]));
                            if l.len() > 8192 {
                                let cut = l.len() - 4096;
                                let cut = (cut..l.len()).find(|&i| l.is_char_boundary(i)).unwrap_or(0);
                                l.drain(..cut);
                            }
                        }
                    }
                }
                _ => break,
            }
        }
        alive.store(false, Ordering::SeqCst);
    });
}

/// Останні рядки журналу сервера, щоб повідомлення про помилку лишалось коротким.
fn log_tail(log: &Mutex<String>) -> String {
    let text = log.lock().map(|l| l.trim().to_string()).unwrap_or_default();
    let mut lines: Vec<&str> = text.lines().rev().take(3).collect();
    lines.reverse();
    let mut tail = lines.join(" | ");
    if tail.len() > 400 {
        let cut = (tail.len() - 400..tail.len())
            .find(|&i| tail.is_char_boundary(i))
            .unwrap_or(0);
        tail.drain(..cut);
    }
    tail
}

fn with_log(msg: &str, log: &Mutex<String>) -> String {
    let tail = log_tail(log);
    if tail.is_empty() {
        msg.to_string()
    } else {
        format!("{msg} ({tail})")
    }
}

/// Усі з'єднання з сервером, відкриті й перевірені до запуску потоків.
struct Sockets {
    video: TcpStream,
    video_reader: TcpStream,
    control: TcpStream,
    control_reader: TcpStream,
    control_writer: TcpStream,
}

/// Під'єднується до сокета сервера. При помилці всі відкриті тут з'єднання
/// закриваються самі (копій у потоках ще немає).
fn attach(serial: &str, scid: u32, server_alive: &AtomicBool) -> Result<Sockets> {
    let io = |e: std::io::Error| e.to_string();
    let socket = format!("localabstract:scrcpy_{scid:08x}");

    // Сервер стартує не миттєво: пробуємо під'єднатися до його сокета.
    let mut video = None;
    for _ in 0..100 {
        if !server_alive.load(Ordering::SeqCst) {
            break;
        }
        if let Ok(s) = adb::open_service(serial, &socket) {
            video = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut video = video.ok_or("The mirroring server did not start")?;

    let control = adb::open_service(serial, &socket)
        .map_err(|e| format!("Could not open the control channel ({e})"))?;
    let _ = control.set_nodelay(true);

    video.set_read_timeout(Some(Duration::from_secs(10))).map_err(io)?;
    read_video_header(&mut video)?;
    video.set_read_timeout(None).map_err(io)?;

    Ok(Sockets {
        video_reader: video.try_clone().map_err(io)?,
        control_reader: control.try_clone().map_err(io)?,
        control_writer: control.try_clone().map_err(io)?,
        video,
        control,
    })
}

pub struct Mirror {
    server: TcpStream,
    video: TcpStream,
    control: TcpStream,
    /// Черга повідомлень керування: один потік пише їх у сокет по порядку,
    /// тож відправники ніколи не блокуються на мережі.
    tx: mpsc::Sender<Vec<u8>>,
    alive: Arc<AtomicBool>,
}

impl Mirror {
    /// Запускає сервер на пристрої й під'єднується до нього.
    /// `on_message` викликається з окремого потоку для кожного повідомлення
    /// інтерфейсу; якщо повертає false, трансляція зупиняється.
    pub fn start(
        serial: &str,
        options: &MirrorOptions,
        mut on_message: impl FnMut(Vec<u8>) -> bool + Send + 'static,
    ) -> Result<Self> {
        adb::push(serial, SERVER_JAR, DEVICE_PATH)?;

        let scid = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
            .unwrap_or(1)
            & 0x7fff_ffff;

        let server = adb::open_service(serial, &server_command(scid, options))?;
        let server_alive = Arc::new(AtomicBool::new(true));
        let log = Arc::new(Mutex::new(String::new()));
        // Якщо копію зробити не вдалось, `server` закриється при виході.
        let server_reader = server.try_clone().map_err(|e| e.to_string())?;
        drain(server_reader, server_alive.clone(), Some(log.clone()));

        let sockets = match attach(serial, scid, &server_alive) {
            Ok(s) => s,
            Err(e) => {
                // Копію з'єднання тримає потік журналу, тому закриваємо явно:
                // це завершує сервер на пристрої.
                let _ = server.shutdown(Shutdown::Both);
                return Err(with_log(&e, &log));
            }
        };

        let alive = Arc::new(AtomicBool::new(true));

        // Повідомлення від пристрою (буфер обміну тощо) нам не потрібні,
        // але сокет треба вичитувати.
        drain(sockets.control_reader, Arc::new(AtomicBool::new(true)), None);

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = sockets.control_writer;
        let writer_flag = alive.clone();
        std::thread::spawn(move || {
            for msg in rx {
                if writer.write_all(&msg).is_err() {
                    writer_flag.store(false, Ordering::SeqCst);
                    let _ = writer.shutdown(Shutdown::Both);
                    break;
                }
            }
        });

        let mut reader = sockets.video_reader;
        let flag = alive.clone();
        std::thread::spawn(move || {
            let mut framer = Framer::default();
            let reason = loop {
                let msg = match read_packet(&mut reader).and_then(|p| framer.convert(p)) {
                    Ok(m) => m,
                    Err(e) => break e,
                };
                if !on_message(msg) {
                    break String::new();
                }
            };
            let stopped_by_us = !flag.swap(false, Ordering::SeqCst);
            if !stopped_by_us && !reason.is_empty() {
                on_message(ended_message(&with_log(&reason, &log)));
            } else {
                on_message(ended_message(""));
            }
            let _ = reader.shutdown(Shutdown::Both);
        });

        Ok(Mirror {
            server,
            video: sockets.video,
            control: sockets.control,
            tx,
            alive,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Ставить повідомлення керування в чергу; не блокується.
    pub fn send(&self, msg: Vec<u8>) -> Result<()> {
        self.tx
            .send(msg)
            .map_err(|_| "The control channel was interrupted".to_string())
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        // Закриття сокетів завершує сервер на пристрої.
        let _ = self.control.shutdown(Shutdown::Both);
        let _ = self.video.shutdown(Shutdown::Both);
        let _ = self.server.shutdown(Shutdown::Both);
    }
}

#[derive(Default)]
pub struct Mirrors(Mutex<HashMap<String, Mirror>>);

impl Mirrors {
    pub fn insert(&self, serial: &str, mirror: Mirror) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(serial.to_string(), mirror);
        }
    }

    pub fn stop(&self, serial: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(serial);
        }
    }

    pub fn stop_all(&self) {
        if let Ok(mut map) = self.0.lock() {
            map.clear();
        }
    }

    /// Надсилає повідомлення керування, якщо для пристрою йде трансляція.
    /// `Ok(false)` — трансляції немає, викликач має скористатися shell.
    pub fn try_send(&self, serial: &str, msgs: Vec<Vec<u8>>) -> Result<bool> {
        let mut map = self.0.lock().map_err(|_| "Internal state error")?;
        let Some(mirror) = map.get(serial) else {
            return Ok(false);
        };
        if !mirror.is_alive() {
            map.remove(serial);
            return Ok(false);
        }
        for m in msgs {
            if mirror.send(m).is_err() {
                map.remove(serial);
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub fn finger_touch(action: u8, x: i32, y: i32, width: u16, height: u16) -> Vec<u8> {
    let pressure = if action == ACTION_UP { 0 } else { 0xffff };
    msg_touch(action, POINTER_FINGER, x, y, width, height, pressure, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Еталонні байти взято з тестів scrcpy (app/tests/test_control_msg_serialize.c).

    #[test]
    fn serializes_keycode() {
        assert_eq!(
            msg_keycode(ACTION_UP, 66, 5, 0x41),
            [0, 0x01, 0, 0, 0, 0x42, 0, 0, 0, 0x05, 0, 0, 0, 0x41]
        );
    }

    #[test]
    fn serializes_text() {
        let mut expected = vec![1, 0, 0, 0, 0x0d];
        expected.extend_from_slice(b"hello, world!");
        assert_eq!(msg_text("hello, world!"), expected);
    }

    #[test]
    fn serializes_touch() {
        assert_eq!(
            msg_touch(ACTION_DOWN, 0x1234567887654321, 100, 200, 1080, 1920, 0xffff, 1, 1),
            [
                2, 0x00, 0x12, 0x34, 0x56, 0x78, 0x87, 0x65, 0x43, 0x21, 0x00, 0x00, 0x00, 0x64,
                0x00, 0x00, 0x00, 0xc8, 0x04, 0x38, 0x07, 0x80, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
                0x00, 0x00, 0x00, 0x01,
            ]
        );
    }

    #[test]
    fn serializes_scroll() {
        assert_eq!(
            msg_scroll(260, 1026, 1080, 1920, 16.0, -16.0, 1),
            [
                3, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00, 0x04, 0x02, 0x04, 0x38, 0x07, 0x80, 0x7F,
                0xFF, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01,
            ]
        );
    }

    #[test]
    fn serializes_set_clipboard() {
        let mut expected = vec![9, 1, 2, 3, 4, 5, 6, 7, 8, 1, 0, 0, 0, 0x0d];
        expected.extend_from_slice(b"hello, world!");
        assert_eq!(msg_set_clipboard(0x0102030405060708, true, "hello, world!"), expected);
    }

    const SPS_PPS: &[u8] = &[
        0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x29, 0x8d, 0x68, // SPS (baseline, level 4.1)
        0, 0, 0, 1, 0x68, 0xce, 0x01, 0xa8, 0x35, 0xc8, // PPS
    ];

    fn stream() -> Vec<u8> {
        let mut s = vec![0u8]; // службовий байт
        let mut name = [0u8; 64];
        name[..6].copy_from_slice(b"TV Box");
        s.extend_from_slice(&name);
        s.extend_from_slice(b"h264");
        // сесія 1280x720
        s.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 0x05, 0x00, 0, 0, 0x02, 0xd0]);
        // конфігурація
        s.extend_from_slice(&FLAG_CONFIG.to_be_bytes());
        s.extend_from_slice(&(SPS_PPS.len() as u32).to_be_bytes());
        s.extend_from_slice(SPS_PPS);
        // ключовий кадр, pts = 1000
        s.extend_from_slice(&(FLAG_KEY | 1000).to_be_bytes());
        s.extend_from_slice(&5u32.to_be_bytes());
        s.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        // звичайний кадр, pts = 2000
        s.extend_from_slice(&2000u64.to_be_bytes());
        s.extend_from_slice(&5u32.to_be_bytes());
        s.extend_from_slice(&[0, 0, 0, 1, 0x41]);
        s
    }

    #[test]
    fn parses_video_stream() {
        let data = stream();
        let mut r = &data[..];
        assert_eq!(read_video_header(&mut r).unwrap(), "TV Box");
        assert_eq!(read_packet(&mut r).unwrap(), Packet::Session { width: 1280, height: 720 });
        assert_eq!(read_packet(&mut r).unwrap(), Packet::Config(SPS_PPS.to_vec()));
        assert_eq!(
            read_packet(&mut r).unwrap(),
            Packet::Frame { pts: 1000, key: true, data: vec![0, 0, 0, 1, 0x65] }
        );
        assert_eq!(
            read_packet(&mut r).unwrap(),
            Packet::Frame { pts: 2000, key: false, data: vec![0, 0, 0, 1, 0x41] }
        );
        assert!(read_packet(&mut r).is_err());
    }

    #[test]
    fn frames_messages_for_ui() {
        let data = stream();
        let mut r = &data[..];
        read_video_header(&mut r).unwrap();
        let mut f = Framer::default();

        let session = f.convert(read_packet(&mut r).unwrap()).unwrap();
        assert_eq!(session, [OUT_SESSION, 0, 0, 5, 0, 0, 0, 2, 0xd0]);

        let config = f.convert(read_packet(&mut r).unwrap()).unwrap();
        assert_eq!(config[0], OUT_CONFIG);
        assert_eq!(&config[1..], b"avc1.42c029");

        // Конфігурація приклеюється до першого кадру після неї.
        let key = f.convert(read_packet(&mut r).unwrap()).unwrap();
        assert_eq!(key[0], OUT_KEY_FRAME);
        assert_eq!(&key[1..9], &1000u64.to_be_bytes());
        assert_eq!(&key[9..9 + SPS_PPS.len()], SPS_PPS);
        assert_eq!(&key[9 + SPS_PPS.len()..], &[0, 0, 0, 1, 0x65]);

        let delta = f.convert(read_packet(&mut r).unwrap()).unwrap();
        assert_eq!(delta[0], OUT_DELTA_FRAME);
        assert_eq!(&delta[9..], &[0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn rejects_bad_streams() {
        let mut bad_codec = vec![0u8; 65];
        bad_codec.extend_from_slice(&[0, 0, 0, 1]);
        assert!(read_video_header(&mut &bad_codec[..]).is_err());
        assert!(avc_codec_string(&[0, 0, 0, 1, 0x68, 1, 2, 3]).is_none());
        assert!(Framer::default().convert(Packet::Config(vec![1, 2, 3])).is_err());
    }

    /// Справжній пристрій: `ADB_REMOTE_TEST_SERIAL=ip:5555 cargo test -- --ignored live_mirror --nocapture`.
    #[test]
    #[ignore]
    fn live_mirror() {
        let serial = std::env::var("ADB_REMOTE_TEST_SERIAL").expect("вкажіть ADB_REMOTE_TEST_SERIAL");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let opts = MirrorOptions { max_size: 1280, bit_rate: 4_000_000, max_fps: 30 };
        let started = std::time::Instant::now();
        let mirror = Mirror::start(&serial, &opts, move |m| tx.send(m).is_ok()).expect("трансляція має стартувати");
        println!("старт за {:?}", started.elapsed());

        let (mut session, mut codec, mut keys, mut deltas, mut bytes) = (None, None, 0, 0, 0usize);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok(m) = rx.recv_timeout(Duration::from_millis(500)) else { continue };
            match m[0] {
                OUT_SESSION => session = Some((u32::from_be_bytes(m[1..5].try_into().unwrap()), u32::from_be_bytes(m[5..9].try_into().unwrap()))),
                OUT_CONFIG => codec = Some(String::from_utf8_lossy(&m[1..]).into_owned()),
                OUT_KEY_FRAME => { keys += 1; bytes += m.len() }
                OUT_DELTA_FRAME => { deltas += 1; bytes += m.len() }
                OUT_ENDED => panic!("потік завершився: {}", String::from_utf8_lossy(&m[1..])),
                _ => {}
            }
        }
        println!("сесія={session:?} кодек={codec:?} ключових={keys} звичайних={deltas} байт={bytes}");
        // Канал керування: код 0 (KEYCODE_UNKNOWN) нічого не робить на пристрої.
        mirror.send(msg_keycode(ACTION_DOWN, 0, 0, 0)).unwrap();
        mirror.send(msg_keycode(ACTION_UP, 0, 0, 0)).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(mirror.is_alive());
        assert!(session.is_some() && codec.is_some() && keys >= 1);
        drop(mirror);
    }

    #[test]
    fn keeps_error_log_short() {
        let empty = Mutex::new(String::new());
        assert_eq!(with_log("failed", &empty), "failed");

        let log = Mutex::new("line 1\nline 2\nline 3\nline 4\n".to_string());
        assert_eq!(with_log("failed", &log), "failed (line 2 | line 3 | line 4)");

        let long = Mutex::new("x".repeat(5000));
        assert!(with_log("failed", &long).len() <= 420);
    }

    #[test]
    fn builds_server_command() {
        let cmd = server_command(0x1234, &MirrorOptions { max_size: 1280, bit_rate: 4_000_000, max_fps: 30 });
        assert!(cmd.starts_with("shell:CLASSPATH=/data/local/tmp/adb-remote-scrcpy-server.jar app_process / com.genymobile.scrcpy.Server 5.0.1 "));
        assert!(cmd.contains(" scid=00001234 "));
        assert!(cmd.contains("tunnel_forward=true"));
        assert!(cmd.ends_with("max_size=1280 video_bit_rate=4000000 max_fps=30"));
    }
}
