mod adb;
mod scrcpy;

use adb::{Device, Sessions};
use scrcpy::{Mirror, MirrorOptions, Mirrors};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

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
        let msgs = [
            scrcpy::msg_keycode(scrcpy::ACTION_DOWN, keycode, 0, 0),
            scrcpy::msg_keycode(scrcpy::ACTION_UP, keycode, 0, 0),
        ];
        if mirrors.try_send(&serial, &msgs)? {
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
    if mirrors.try_send(&serial, &[msg])? {
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

#[tauri::command(async)]
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
        .try_send(&serial, &[scrcpy::finger_touch(action, x, y, width, height)])
        .map(|_| ())
}

#[tauri::command(async)]
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
        .try_send(&serial, &[scrcpy::msg_scroll(x, y, width, height, dx, dy, 0)])
        .map(|_| ())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Sessions::default())
        .manage(Mirrors::default())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            connect,
            disconnect,
            send_key,
            send_text,
            start_mirror,
            stop_mirror,
            mirror_touch,
            mirror_scroll
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
