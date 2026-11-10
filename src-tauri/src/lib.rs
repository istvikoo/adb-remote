mod adb;
mod scrcpy;
mod tools;

use adb::{Device, Sessions};
use scrcpy::{Mirror, MirrorOptions, Mirrors};
use tauri::ipc::{Channel, InvokeResponseBody};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::State;

/// Прапорець зупинки команди, що зараз виконується в консолі.
#[derive(Default)]
struct ConsoleCancel(Mutex<Arc<AtomicBool>>);

#[tauri::command(async)]
fn list_devices() -> Result<Vec<Device>, String> {
    adb::devices()
}

#[tauri::command(async)]
fn connect(address: String) -> Result<String, String> {
    adb::connect(&address)
}

#[tauri::command(async)]
fn disconnect(
    serial: String,
    sessions: State<'_, Sessions>,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    mirrors.stop(&serial);
    sessions.close(&serial);
    adb::disconnect(&serial)
}

/// Натискання клавіші: через канал scrcpy, якщо йде трансляція (миттєво),
/// інакше через `input keyevent` у shell.
#[tauri::command(async)]
fn send_key(
    serial: String,
    keycode: u32,
    longpress: bool,
    sessions: State<'_, Sessions>,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    if !longpress {
        let msgs = vec![
            scrcpy::msg_keycode(scrcpy::ACTION_DOWN, keycode, 0, 0),
            scrcpy::msg_keycode(scrcpy::ACTION_UP, keycode, 0, 0),
        ];
        if mirrors.try_send(&serial, msgs)? {
            return Ok(());
        }
    }
    sessions.shell(&serial, &adb::key_command(keycode, longpress))
}

#[tauri::command(async)]
fn send_text(
    serial: String,
    text: String,
    sessions: State<'_, Sessions>,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    if text.is_empty() {
        return Err("Text is empty".into());
    }
    // Не-ASCII (кирилиця) вводиться лише вставкою з буфера обміну.
    let msg = if text.is_ascii() {
        scrcpy::msg_text(&text)
    } else {
        scrcpy::msg_set_clipboard(0, true, &text)
    };
    if mirrors.try_send(&serial, vec![msg])? {
        return Ok(());
    }
    sessions.shell(&serial, &adb::text_command(&text)?)
}

#[tauri::command(async)]
fn start_mirror(
    serial: String,
    max_size: u16,
    bit_rate: u32,
    max_fps: u8,
    on_data: Channel<InvokeResponseBody>,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    mirrors.stop(&serial);
    let options = MirrorOptions {
        max_size: max_size.clamp(320, 3840),
        bit_rate: bit_rate.clamp(500_000, 50_000_000),
        max_fps: max_fps.clamp(5, 120),
    };
    let mirror = Mirror::start(&serial, &options, move |msg| {
        on_data.send(InvokeResponseBody::Raw(msg)).is_ok()
    })?;
    mirrors.insert(&serial, mirror);
    Ok(())
}

#[tauri::command(async)]
fn stop_mirror(serial: String, mirrors: State<'_, Mirrors>) {
    mirrors.stop(&serial);
}

/// Зупиняє всі трансляції. Інтерфейс викликає це при завантаженні, щоб після
/// перезавантаження сторінки не лишалось трансляцій без глядача.
#[tauri::command(async)]
fn stop_all_mirrors(mirrors: State<'_, Mirrors>) {
    mirrors.stop_all();
}

// Дотик і прокрутка лише ставлять повідомлення в чергу (без мережі),
// тому виконуються синхронно, у порядку надходження.
#[tauri::command]
fn mirror_touch(
    serial: String,
    action: u8,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    mirrors
        .try_send(&serial, vec![scrcpy::finger_touch(action, x, y, width, height)])
        .map(|_| ())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn mirror_scroll(
    serial: String,
    x: i32,
    y: i32,
    width: u16,
    height: u16,
    dx: f32,
    dy: f32,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    mirrors
        .try_send(&serial, vec![scrcpy::msg_scroll(x, y, width, height, dx, dy, 0)])
        .map(|_| ())
}

/// Натискання або відпускання клавіші через канал scrcpy (для справжнього
/// утримання). Повертає false, якщо трансляції немає.
#[tauri::command]
fn key_event(
    serial: String,
    keycode: u32,
    action: u8,
    repeat: u32,
    mirrors: State<'_, Mirrors>,
) -> Result<bool, String> {
    let action = if action == 0 { scrcpy::ACTION_DOWN } else { scrcpy::ACTION_UP };
    mirrors.try_send(&serial, vec![scrcpy::msg_keycode(action, keycode, repeat, 0)])
}

// ---------- консоль ----------

#[tauri::command(async)]
fn run_shell(
    serial: String,
    command: String,
    cancel: State<'_, ConsoleCancel>,
) -> Result<adb::ExecResult, String> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Ok(mut current) = cancel.0.lock() {
        *current = flag.clone();
    }
    adb::shell_output(&serial, command.trim(), Duration::from_secs(15), &flag)
}

#[tauri::command]
fn cancel_shell(cancel: State<'_, ConsoleCancel>) {
    if let Ok(current) = cancel.0.lock() {
        current.store(true, Ordering::SeqCst);
    }
}

// ---------- застосунки та швидкі дії ----------

#[tauri::command(async)]
fn list_apps(serial: String, all: bool) -> Result<Vec<String>, String> {
    tools::list_apps(&serial, all)
}

#[tauri::command(async)]
fn launch_app(serial: String, package: String) -> Result<(), String> {
    tools::launch_app(&serial, &package)
}

#[tauri::command(async)]
fn stop_app(serial: String, package: String) -> Result<(), String> {
    tools::stop_app(&serial, &package)
}

#[tauri::command(async)]
fn device_info(serial: String) -> Result<tools::DeviceInfo, String> {
    tools::device_info(&serial)
}

#[tauri::command(async)]
fn open_url(serial: String, url: String) -> Result<(), String> {
    tools::open_url(&serial, &url)
}

#[tauri::command(async)]
fn reboot_device(
    serial: String,
    sessions: State<'_, Sessions>,
    mirrors: State<'_, Mirrors>,
) -> Result<(), String> {
    mirrors.stop(&serial);
    sessions.close(&serial);
    tools::reboot(&serial)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Sessions::default())
        .manage(Mirrors::default())
        .manage(ConsoleCancel::default())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            connect,
            disconnect,
            send_key,
            send_text,
            start_mirror,
            stop_mirror,
            stop_all_mirrors,
            mirror_touch,
            mirror_scroll,
            key_event,
            run_shell,
            cancel_shell,
            list_apps,
            launch_app,
            stop_app,
            device_info,
            open_url,
            reboot_device
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
