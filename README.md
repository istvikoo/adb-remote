# ADB Remote

A desktop remote control for Android TV boxes, projectors and other Android devices, driven over ADB. It can also show the device screen live and control it with low latency using an embedded [scrcpy](https://github.com/Genymobile/scrcpy) server.

Built with Tauri 2 (Rust) and SvelteKit (TypeScript).

## Features

- Connect to a device by IP address (ADB over network, port 5555 by default); recent addresses are remembered
- Remote buttons: D-pad, OK, Back, Home, Menu, volume, mute, power, media controls, Delete, Enter, Settings
- PC keyboard control: arrows, Enter = OK, Esc = Back, Space = play/pause, +/− volume, Backspace = delete
- Text input
- Live screen view (H.264 via scrcpy, decoded with WebCodecs), with click/drag as touch and mouse wheel as scroll
- Three screen quality presets (size, bitrate, frame rate)

While the screen view is on, key presses go through the scrcpy control channel and are near-instant, and non-Latin text is typed via the device clipboard. With the screen view off, keys are sent with `input keyevent` over a persistent shell, which is slower on weak devices and limited to ASCII text.

## Requirements

- macOS 13.3 or later (Apple Silicon or Intel)
- `adb` installed on the computer, for example `brew install android-platform-tools`
- On the device: Developer options → network (ADB) debugging enabled. Confirm the "Allow debugging" prompt on first connection.

Pairing-code wireless debugging (Android 11+ phones) is not supported yet; the device must accept classic ADB connections on a TCP port.

## Install (macOS)

Download the `.dmg` from the [Releases](https://github.com/istvikoo/adb-remote/releases) page and drag the app to Applications.

The app is not signed with an Apple Developer ID, so macOS will refuse to open it on first launch. Either right-click the app and choose Open, or run:

```bash
xattr -dr com.apple.quarantine "/Applications/ADB Remote.app"
```

## Development

```bash
npm install
npm run tauri dev
```

Tests:

```bash
npm run check
cd src-tauri && cargo test
```

Build a release bundle:

```bash
npm run tauri build
```

## Notes

- DRM-protected apps (Netflix and similar) show a black picture in the screen view. This is an Android restriction.
- Starting the screen view wakes the device.
- Windows is planned but not built or tested yet.

## License

MIT, see [LICENSE](LICENSE). The bundled scrcpy server keeps its own license, described below.

## Third-party software

This repository includes the prebuilt `scrcpy-server` v5.0.1 binary (`src-tauri/resources/scrcpy-server`) from [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy), licensed under the Apache License 2.0. It is pushed to the device at runtime to provide screen mirroring and input injection.
