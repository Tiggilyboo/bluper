//! BluePointer: BLE HID passthrough for mouse + keyboard
//! - BLE owner task owns the Peripheral
//! - stdin task (crossterm) -> AppCmd
//! - evdev task (reads /dev/input/event* devices) -> AppCmd

use std::{collections::BTreeSet, time::Duration};

use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use evdev::{EventType, KeyCode, RelativeAxisCode};
use tokio::{select, sync::mpsc, task, time::sleep};
use uuid::Uuid;

use ble_peripheral_rust::{
    Peripheral, PeripheralImpl,
    gatt::{
        characteristic::Characteristic,
        descriptor::Descriptor,
        peripheral_event::{
            PeripheralEvent, ReadRequestResponse, RequestResponse, WriteRequestResponse,
        },
        properties::{AttributePermission, CharacteristicProperty},
        service::Service,
    },
    uuid::ShortUuid,
};

/// === UUIDs ===
const UUID_HID_SERVICE: u16 = 0x1812;
const UUID_BAS_SERVICE: u16 = 0x180F;
const UUID_DIS_SERVICE: u16 = 0x180A;

const UUID_HID_INFO: u16 = 0x2A4A;
const UUID_HID_CONTROL_POINT: u16 = 0x2A4C;
const UUID_HID_PROTOCOL_MODE: u16 = 0x2A4E;
const UUID_HID_REPORT_MAP: u16 = 0x2A4B;
const UUID_HID_REPORT: u16 = 0x2A4D; // we’ll create 2 characteristics with this UUID (mouse & keyboard)
const UUID_REPORT_REF_DESC: u16 = 0x2908;

const UUID_BATTERY_LEVEL: u16 = 0x2A19;
const UUID_MFG_NAME: u16 = 0x2A29;
const UUID_MODEL_NUM: u16 = 0x2A24;

/// === Report IDs ===
const RID_MOUSE: u8 = 0x01;
const RID_KEYBD: u8 = 0x02;

/// === App->BLE owner commands ===
#[derive(Debug)]
enum AppCmd {
    /// mouse: buttons bitfield (bit0..2 left/middle/right), dx, dy, wheel
    Mouse {
        buttons: u8,
        dx: i8,
        dy: i8,
        wheel: i8,
    },
    /// keyboard key down/up (HID usage)
    KeyDown(u8),
    KeyUp(u8),
    /// set battery level (0-100)
    Battery(u8),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Channel for app commands -> BLE owner
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<AppCmd>(512);
    // Channel for BLE events
    let (evt_tx, mut evt_rx) = mpsc::channel::<PeripheralEvent>(512);

    // ---------- Build GATT ----------
    let (hid_service, report_char_uuid) = build_hid_services();
    let bas_service = Service {
        uuid: Uuid::from_short(UUID_BAS_SERVICE),
        primary: true,
        characteristics: vec![Characteristic {
            uuid: Uuid::from_short(UUID_BATTERY_LEVEL),
            properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
            permissions: vec![AttributePermission::Readable],
            value: Some(vec![95]),
            ..Default::default()
        }],
    };
    let dis_service = Service {
        uuid: Uuid::from_short(UUID_DIS_SERVICE),
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: Uuid::from_short(UUID_MFG_NAME),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(b"BluePointer Labs".to_vec()),
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_MODEL_NUM),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(b"BluePointer-1".to_vec()),
                ..Default::default()
            },
        ],
    };

    // ---------- BLE owner task ----------
    let mut peripheral = Peripheral::new(evt_tx).await?;
    while !peripheral.is_powered().await? {}
    peripheral.add_service(&hid_service).await?;
    peripheral.add_service(&bas_service).await?;
    peripheral.add_service(&dis_service).await?;
    peripheral
        .start_advertising(
            "BluePointer",
            &[
                Uuid::from_short(UUID_HID_SERVICE),
                Uuid::from_short(UUID_BAS_SERVICE),
                Uuid::from_short(UUID_DIS_SERVICE),
            ],
            Some(0x03C0),
        )
        .await?;

    // Keyboard state for 6KRO report (modifiers + 6 keys)
    let mut modifiers: u8 = 0;
    let mut pressed: BTreeSet<u8> = BTreeSet::new();
    let mut report_notify_enabled = false;

    // ---------- Producers ----------
    // 1) stdin (crossterm) demo input
    let stdin_tx = cmd_tx.clone();
    task::spawn(async move { read_stdin(stdin_tx).await });

    // 2) evdev system devices (mouse + keyboard)
    let evdev_tx = cmd_tx.clone();
    task::spawn(async move { read_evdev(evdev_tx).await });

    // ---------- Main loop: drive BLE with both BLE events and AppCmd ----------
    loop {
        select! {
            // BLE stack → us
            ev = evt_rx.recv() => {
                match ev {
                    Some(PeripheralEvent::StateUpdate{ is_powered }) => {
                        println!("Adapter powered: {is_powered}");
                    }
                    Some(PeripheralEvent::CharacteristicSubscriptionUpdate { request, subscribed }) => {
                        if request.characteristic == report_char_uuid {
                            report_notify_enabled = subscribed;
                            println!("Report notify: {subscribed}");
                        } else {
                            println!("Other subscription: {subscribed} for {request:?}");
                        }
                    }
                    Some(PeripheralEvent::ReadRequest{ request, offset, responder }) => {
                        println!("ReadRequest: {:?} off={}", request, offset);
                        let _ = responder.send(ReadRequestResponse{
                            value: Vec::<u8>::new().into(),
                            response: RequestResponse::Success
                        });
                    }
                    Some(PeripheralEvent::WriteRequest{ request, offset, value, responder }) => {
                        println!("WriteRequest: {:?} off={} val={:?}", request, offset, value);
                        let _ = responder.send(WriteRequestResponse{ response: RequestResponse::Success });
                    }
                    None => break,
                }
            }

            // App-produced input → send HID reports
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AppCmd::Mouse { buttons, dx, dy, wheel }) if report_notify_enabled => {
                        let pkt = vec![RID_MOUSE, buttons, dx as u8, dy as u8, wheel as u8];
                        println!("TX mouse: btn={buttons:#04b} dx={dx} dy={dy} wheel={wheel}");
                        peripheral.update_characteristic(report_char_uuid, pkt.into()).await?;
                    }
                    Some(AppCmd::KeyDown(usage)) if report_notify_enabled => {
                        if let Some(m) = keyboard_usage_to_modifier(usage) {
                            modifiers |= m;
                        } else {
                            pressed.insert(usage);
                            while pressed.len() > 6 { // keep 6KRO
                                let first = *pressed.iter().next().unwrap();
                                pressed.remove(&first);
                            }
                        }
                        println!("TX keybd: mods={modifiers:#010b} keys={:?}", pressed);
                        let pkt = build_keyboard_report(modifiers, &pressed);
                        peripheral.update_characteristic(report_char_uuid, pkt.into()).await?;
                    }
                    Some(AppCmd::KeyUp(usage)) if report_notify_enabled => {
                        if let Some(m) = keyboard_usage_to_modifier(usage) {
                            modifiers &= !m;
                        } else {
                            pressed.remove(&usage);
                        }
                        let pkt = build_keyboard_report(modifiers, &pressed);
                        peripheral.update_characteristic(report_char_uuid, pkt.into()).await?;
                    }
                    Some(AppCmd::Battery(level)) => {
                        peripheral.update_characteristic(Uuid::from_short(UUID_BATTERY_LEVEL), vec![level].into()).await?;
                        println!("Battery set to {}%", level);
                    }
                    Some(_) | None => {}
                }
            }
        }
    }

    peripheral.stop_advertising().await?;

    Ok(())
}

/// Build HID service with:
/// - Report Map (mouse RID=1, keyboard RID=2)
/// - Report (Input) x2 (mouse & keyboard) with proper Report Reference
fn build_hid_services() -> (Service, Uuid) {
    // --- Report Map combining mouse + keyboard ---
    let mut report_map: Vec<u8> = Vec::new();

    // Mouse (RID 1)
    report_map.extend_from_slice(&[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, RID_MOUSE, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09,
        0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95,
        0x01, 0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81,
        0x25, 0x7F, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06, 0xC0, 0xC0,
    ]);

    // Keyboard (RID 2): 8-byte report (mods, reserved, 6 keycodes) + LED OUT report
    report_map.extend_from_slice(&[
        0x05, 0x01, // Generic Desktop
        0x09, 0x06, // Keyboard
        0xA1, 0x01, // Collection (Application)
        0x85, RID_KEYBD, 0x05, 0x07, // Usage Page (Keyboard/Keypad)
        // Modifiers (8 bits)
        0x19, 0xE0, 0x29, 0xE7, // Usage Min/Max (LeftCtrl..RightGUI)
        0x15, 0x00, 0x25, 0x01, // Logical 0..1
        0x75, 0x01, 0x95, 0x08, 0x81, 0x02, // Input (Data,Var,Abs)
        // Reserved
        0x75, 0x08, 0x95, 0x01, 0x81, 0x03, // Input (Const)
        // 6 keycodes
        0x15, 0x00, 0x25, 0x65, // 0..101 (enough for common keys)
        0x19, 0x00, 0x29, 0x65, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00, // Input (Data,Array)
        // --- Keyboard LEDs (OUT report bits) ---
        0x05, 0x08, // Usage Page (LEDs)
        0x19, 0x01, // Usage Minimum (Num Lock)
        0x29, 0x05, // Usage Maximum (Kana)
        0x95, 0x05, // Report Count (5)
        0x75, 0x01, // Report Size (1)
        0x91, 0x02, // Output (Data,Var,Abs)
        0x95, 0x01, // Report Count (1)
        0x75, 0x03, // Report Size (3) ; padding
        0x91, 0x03, // Output (Const,Var,Abs)
        0xC0, // End Collection
    ]);

    let report_char_uuid = Uuid::from_short(UUID_HID_REPORT);

    let hid_service = Service {
        uuid: Uuid::from_short(UUID_HID_SERVICE),
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_INFO),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(vec![0x11, 0x01, 0x00, 0x00]),
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_CONTROL_POINT),
                properties: vec![CharacteristicProperty::Write],
                permissions: vec![AttributePermission::Writeable],
                value: None,
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_PROTOCOL_MODE),
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Write],
                permissions: vec![
                    AttributePermission::Readable,
                    AttributePermission::Writeable,
                ],
                value: Some(vec![0x01]), // Report Protocol
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_REPORT_MAP),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(report_map),
                ..Default::default()
            },
            Characteristic {
                uuid: report_char_uuid,
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
                permissions: vec![AttributePermission::Readable],
                value: None,
                // descriptor value can be RID_MOUSE or RID_KEYBD; the payload's Report ID is what matters
                descriptors: vec![Descriptor {
                    uuid: Uuid::from_short(UUID_REPORT_REF_DESC),
                    value: Some(vec![RID_MOUSE, 0x01]),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Characteristic {
                uuid: report_char_uuid,
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
                permissions: vec![AttributePermission::Readable],
                value: None,
                // descriptor value can be RID_MOUSE or RID_KEYBD; the payload's Report ID is what matters
                descriptors: vec![Descriptor {
                    uuid: Uuid::from_short(UUID_REPORT_REF_DESC),
                    value: Some(vec![RID_KEYBD, 0x01]),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
    };

    (hid_service, report_char_uuid)
}

/// Build an 8-byte keyboard input report prefixed with Report ID
fn build_keyboard_report(mods: u8, pressed: &BTreeSet<u8>) -> Vec<u8> {
    let mut out = vec![RID_KEYBD, mods, 0x00 /* reserved */];
    // up to 6 keys
    for &k in pressed.iter().take(6) {
        out.push(k);
    }
    while out.len() < 1 + 1 + 1 + 6 {
        out.push(0); // fill remaining slots
    }
    out
}

/// Map HID usage to modifier bit (if it is a modifier)
fn keyboard_usage_to_modifier(usage: u8) -> Option<u8> {
    match usage {
        0xE0 => Some(1 << 0), // LCtrl
        0xE1 => Some(1 << 1), // LShift
        0xE2 => Some(1 << 2), // LAlt
        0xE3 => Some(1 << 3), // LGUI
        0xE4 => Some(1 << 4), // RCtrl
        0xE5 => Some(1 << 5), // RShift
        0xE6 => Some(1 << 6), // RAlt
        0xE7 => Some(1 << 7), // RGUI
        _ => None,
    }
}

/// =====================
/// INPUT PRODUCERS
/// =====================

/// 1) stdin producer (crossterm) — good for quick testing without evdev permissions.
/// - Press `q` to quit.
/// - Arrow keys move the mouse.
/// - `a..z` send keyboard presses.
/// - Hold Shift/Ctrl/Alt/GUI to test modifiers.
/// - Optional: enable mouse capture to translate terminal mouse drags → HID mouse.
async fn read_stdin(tx: mpsc::Sender<AppCmd>) -> anyhow::Result<()> {
    use crossterm::{
        ExecutableCommand,
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
            KeyModifiers, MouseEventKind,
        },
        terminal,
    };

    let _ = std::io::stdout().execute(terminal::EnterAlternateScreen);
    let _ = enable_raw_mode();
    let _ = execute!(std::io::stdout(), EnableMouseCapture)?;

    println!("stdin: press 'q' to quit, arrows move mouse, letters send keypresses");

    loop {
        // non-blocking poll so we can periodically yield
        if event::poll(Duration::from_millis(25)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(k)) => {
                    // key press / release
                    let is_press = matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat);
                    // modifiers
                    let mut mods = Vec::new();
                    if k.modifiers.contains(KeyModifiers::CONTROL) {
                        mods.extend([0xE0]);
                    } // LCtrl
                    if k.modifiers.contains(KeyModifiers::SHIFT) {
                        mods.extend([0xE1]);
                    } // LShift
                    if k.modifiers.contains(KeyModifiers::ALT) {
                        mods.extend([0xE2]);
                    } // LAlt
                    if k.modifiers.contains(KeyModifiers::SUPER) {
                        mods.extend([0xE3]);
                    } // LGUI

                    for m in mods {
                        let _ = if is_press {
                            tx.send(AppCmd::KeyDown(m)).await
                        } else {
                            tx.send(AppCmd::KeyUp(m)).await
                        };
                    }

                    // basic alphanumerics + ESC + ENTER + SPACE
                    if let Some(usage) = keycode_to_hid_usage(&k.code) {
                        let _ = if is_press {
                            tx.send(AppCmd::KeyDown(usage)).await
                        } else {
                            tx.send(AppCmd::KeyUp(usage)).await
                        };
                    }

                    if k.code == KeyCode::Char('q') && is_press && k.modifiers.is_empty() {
                        break;
                    }
                }
                Ok(Event::Mouse(m)) => {
                    match m.kind {
                        MouseEventKind::Down(btn) | MouseEventKind::Up(btn) => {
                            // simple: map left/middle/right only
                            let mask = match btn {
                                crossterm::event::MouseButton::Left => 1 << 0,
                                crossterm::event::MouseButton::Middle => 1 << 2,
                                crossterm::event::MouseButton::Right => 1 << 1,
                            };
                            // we can’t read current button state from crossterm easily; just emit press OR release deltas
                            let _ = tx
                                .send(AppCmd::Mouse {
                                    buttons: mask,
                                    dx: 0,
                                    dy: 0,
                                    wheel: 0,
                                })
                                .await;
                        }
                        MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                            // terminal only gives absolute cell coords; approximate small deltas
                            let _ = tx
                                .send(AppCmd::Mouse {
                                    buttons: 0,
                                    dx: 1,
                                    dy: 1,
                                    wheel: 0,
                                })
                                .await;
                        }
                        MouseEventKind::ScrollDown => {
                            let _ = tx
                                .send(AppCmd::Mouse {
                                    buttons: 0,
                                    dx: 0,
                                    dy: 0,
                                    wheel: -1,
                                })
                                .await;
                        }
                        MouseEventKind::ScrollUp => {
                            let _ = tx
                                .send(AppCmd::Mouse {
                                    buttons: 0,
                                    dx: 0,
                                    dy: 0,
                                    wheel: 1,
                                })
                                .await;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        } else {
            sleep(Duration::from_millis(5)).await;
        }
    }

    execute!(std::io::stdout(), DisableMouseCapture)?;
    disable_raw_mode()?;

    Ok(())
}

/// Minimal mapping for demo (extend as needed).
fn keycode_to_hid_usage(code: &crossterm::event::KeyCode) -> Option<u8> {
    use crossterm::event::KeyCode::*;
    Some(match code {
        Char('a') => 0x04,
        Char('b') => 0x05,
        Char('c') => 0x06,
        Char('d') => 0x07,
        Char('e') => 0x08,
        Char('f') => 0x09,
        Char('g') => 0x0A,
        Char('h') => 0x0B,
        Char('i') => 0x0C,
        Char('j') => 0x0D,
        Char('k') => 0x0E,
        Char('l') => 0x0F,
        Char('m') => 0x10,
        Char('n') => 0x11,
        Char('o') => 0x12,
        Char('p') => 0x13,
        Char('q') => 0x14,
        Char('r') => 0x15,
        Char('s') => 0x16,
        Char('t') => 0x17,
        Char('u') => 0x18,
        Char('v') => 0x19,
        Char('w') => 0x1A,
        Char('x') => 0x1B,
        Char('y') => 0x1C,
        Char('z') => 0x1D,
        Char('1') => 0x1E,
        Char('2') => 0x1F,
        Char('3') => 0x20,
        Char('4') => 0x21,
        Char('5') => 0x22,
        Char('6') => 0x23,
        Char('7') => 0x24,
        Char('8') => 0x25,
        Char('9') => 0x26,
        Char('0') => 0x27,
        Enter => 0x28,
        Esc => 0x29,
        Backspace => 0x2A,
        Tab => 0x2B,
        Space => 0x2C,
        Left => 0x50,
        Right => 0x4F,
        Up => 0x52,
        Down => 0x51,
        _ => return None,
    })
}

/// 2) evdev producer (system-wide devices)
async fn read_evdev(tx: mpsc::Sender<AppCmd>) {
    // You’ll need read access to /dev/input/event* (run as root or udev rule).
    // We attach to the first keyboard & mouse we find. Extend to handle hotplug/multiple devices.
    let mut kb: Option<evdev::Device> = None;
    let mut ms: Option<evdev::Device> = None;

    // scan devices
    for (path, mut d) in evdev::enumerate() {
        let name = d.name().unwrap_or("unknown").to_string();
        if d.supported_keys()
            .map(|k| k.iter().len() > 0)
            .unwrap_or(false)
            && kb.is_none()
        {
            kb = Some(d);
            println!("Using keyboard: {name} @ {}", path.display());
        } else if d.supported_relative_axes().is_some() && ms.is_none() {
            ms = Some(d);
            println!("Using mouse: {name} @ {}", path.display());
        }
    }

    // Spawn blocking readers (evdev is blocking)
    if let Some(mut dev) = kb {
        let tx2 = tx.clone();
        task::spawn_blocking(move || {
            loop {
                for ev in dev.fetch_events().unwrap() {
                    if ev.event_type() == EventType::KEY {
                        let code = evdev::KeyCode::new(ev.code());
                        let usage = evdev_key_to_hid(code);
                        if let Some(u) = usage {
                            let cmd = if ev.value() != 0 {
                                AppCmd::KeyDown(u)
                            } else {
                                AppCmd::KeyUp(u)
                            };
                            if tx2.blocking_send(cmd).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
    }
    if let Some(mut dev) = ms {
        task::spawn_blocking(move || {
            let mut buttons: u8 = 0;
            loop {
                let mut dx = 0i32;
                let mut dy = 0i32;
                let mut wheel = 0i32;
                for ev in dev.fetch_events().unwrap() {
                    match ev.event_type() {
                        EventType::RELATIVE => match RelativeAxisCode(ev.code()) {
                            RelativeAxisCode::REL_X => dx += ev.value(),
                            RelativeAxisCode::REL_Y => dy += ev.value(),
                            RelativeAxisCode::REL_WHEEL => wheel += ev.value(),
                            _ => {}
                        },
                        EventType::KEY => {
                            // map BTN_LEFT/RIGHT/MIDDLE
                            let code = KeyCode::new(ev.code());
                            let mask = match code {
                                KeyCode::BTN_LEFT => 1 << 0,
                                KeyCode::BTN_RIGHT => 1 << 1,
                                KeyCode::BTN_MIDDLE => 1 << 2,
                                _ => 0,
                            };
                            if mask != 0 {
                                if ev.value() != 0 {
                                    buttons |= mask;
                                } else {
                                    buttons &= !mask;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if dx != 0 || dy != 0 || wheel != 0 {
                    let _ = tx.blocking_send(AppCmd::Mouse {
                        buttons,
                        dx: dx.clamp(-127, 127) as i8,
                        dy: dy.clamp(-127, 127) as i8,
                        wheel: wheel.clamp(-127, 127) as i8,
                    });
                }
            }
        });
    }
}

/// Very small evdev->HID usage mapping (extend as needed)
fn evdev_key_to_hid(k: evdev::KeyCode) -> Option<u8> {
    Some(match k {
        KEY_A => 0x04,
        KEY_B => 0x05,
        KEY_C => 0x06,
        KEY_D => 0x07,
        KEY_E => 0x08,
        KEY_F => 0x09,
        KEY_G => 0x0A,
        KEY_H => 0x0B,
        KEY_I => 0x0C,
        KEY_J => 0x0D,
        KEY_K => 0x0E,
        KEY_L => 0x0F,
        KEY_M => 0x10,
        KEY_N => 0x11,
        KEY_O => 0x12,
        KEY_P => 0x13,
        KEY_Q => 0x14,
        KEY_R => 0x15,
        KEY_S => 0x16,
        KEY_T => 0x17,
        KEY_U => 0x18,
        KEY_V => 0x19,
        KEY_W => 0x1A,
        KEY_X => 0x1B,
        KEY_Y => 0x1C,
        KEY_Z => 0x1D,
        KEY_1 => 0x1E,
        KEY_2 => 0x1F,
        KEY_3 => 0x20,
        KEY_4 => 0x21,
        KEY_5 => 0x22,
        KEY_6 => 0x23,
        KEY_7 => 0x24,
        KEY_8 => 0x25,
        KEY_9 => 0x26,
        KEY_0 => 0x27,
        KEY_ENTER => 0x28,
        KEY_ESC => 0x29,
        KEY_BACKSPACE => 0x2A,
        KEY_TAB => 0x2B,
        KEY_SPACE => 0x2C,
        KEY_MINUS => 0x2D,
        KEY_EQUAL => 0x2E,
        KEY_LEFTBRACE => 0x2F,
        KEY_RIGHTBRACE => 0x30,
        KEY_BACKSLASH => 0x31,
        KEY_SEMICOLON => 0x33,
        KEY_APOSTROPHE => 0x34,
        KEY_GRAVE => 0x35,
        KEY_COMMA => 0x36,
        KEY_DOT => 0x37,
        KEY_SLASH => 0x38,
        KEY_CAPSLOCK => 0x39,
        KEY_F1 => 0x3A,
        KEY_F2 => 0x3B,
        KEY_F3 => 0x3C,
        KEY_F4 => 0x3D,
        KEY_F5 => 0x3E,
        KEY_F6 => 0x3F,
        KEY_F7 => 0x40,
        KEY_F8 => 0x41,
        KEY_F9 => 0x42,
        KEY_F10 => 0x43,
        KEY_F11 => 0x44,
        KEY_F12 => 0x45,
        KEY_SYSRQ => 0x46,
        KEY_SCROLLLOCK => 0x47,
        KEY_PAUSE => 0x48,
        KEY_INSERT => 0x49,
        KEY_HOME => 0x4A,
        KEY_PAGEUP => 0x4B,
        KEY_DELETE => 0x4C,
        KEY_END => 0x4D,
        KEY_PAGEDOWN => 0x4E,
        KEY_RIGHT => 0x4F,
        KEY_LEFT => 0x50,
        KEY_DOWN => 0x51,
        KEY_UP => 0x52,
        KEY_LEFTCTRL => 0xE0,
        KEY_LEFTSHIFT => 0xE1,
        KEY_LEFTALT => 0xE2,
        KEY_LEFTMETA => 0xE3,
        KEY_RIGHTCTRL => 0xE4,
        KEY_RIGHTSHIFT => 0xE5,
        KEY_RIGHTALT => 0xE6,
        KEY_RIGHTMETA => 0xE7,
    })
}
