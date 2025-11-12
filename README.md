# Bluper — BLE HID Keyboard + Mouse Peripheral

Bluper exposes a Bluetooth LE HID peripheral (Keyboard + Mouse) and passes through local input from a small winit app. It sends HID over GATT reports to the connected host.

## Features
- HID over GATT service with a single Input Report characteristic using Report IDs
- Keyboard 6KRO + modifier byte (E0..E7)
- Mouse buttons + relative X/Y + wheel
- Battery Service and Device Information Service
- BLE startup (power-on backoff) and re-advertising on power changes
- Windowed input via winit
- Structured logging via `tracing`
- Small CLI to configure name, appearance, log-level, and headless mode

## Architecture
- `src/main.rs`: CLI + tracing init; spawns BLE task; runs winit app unless `--headless`
- `src/ble.rs`: Owns `Peripheral`, builds services, handles BLE events + App commands
- `src/ui.rs`: Winit `ApplicationHandler`; translates keyboard/mouse to `AppCmd`
- `src/hid.rs`: HID descriptor/report builders, keycode mapping, helper utilities
- `src/consts.rs`: UUIDs, Report IDs, defaults

HID structure:
- A single Input Report characteristic (0x2A4D) carries both mouse (RID 1) and keyboard (RID 2). Hosts parse the Report Map and demux by Report ID.

## CLI
```
bluper [--name <string>] [--appearance <u16>] [--log-level <level>] [--headless]
```
- `--name`: Device name advertised and used in DIS (default: "Bluper")
- `--appearance`: BLE appearance (default: 0x03C0 Generic HID)
- `--log-level`: `trace|debug|info|warn|error` (default: `info`). Overridden by `RUST_LOG` if set
- `--headless`: Do not create a window; run BLE only

Examples:
- `RUST_LOG=debug cargo run`
- `cargo run -- --name "KBM-Bridge" --log-level trace`
- `cargo run -- --headless --name "KBM-Headless"`

## Logging
- Uses `tracing` with `EnvFilter` + `fmt` subscriber
- Environment: `RUST_LOG=bluper=trace,winit=info` or use `--log-level`

## Build & Run
- Build: `cargo build`
- Run: `cargo run`
