use std::{collections::BTreeSet, num::NonZeroU32};
use tokio::{select, sync::mpsc};
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
use softbuffer::{Context as SbContext, Surface as SbSurface};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseScrollDelta, WindowEvent},
    event_loop,
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::Window,
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
const PERIPHERAL_NAME: &str = "Bluper";
const PERIPHERAL_APPEARANCE: u16 = 0x03C0;

/// === Report IDs ===
const RID_MOUSE: u8 = 0x01;
const RID_KEYBD: u8 = 0x02;

#[derive(Debug)]
enum AppCmd {
    Mouse {
        buttons: u8,
        dx: i8,
        dy: i8,
        wheel: i8,
    },
    KeyDown(u8),
    KeyUp(u8),
    Battery(u8),
}

struct App {
    window: Option<Window>,
    cmd_tx: mpsc::Sender<AppCmd>, // bounded (uses try_send in callbacks)
    mouse_buttons: u8,            // bit0..2
    cursor_last: Option<(f64, f64)>,
    wheel_px_accum: f64,
    mods_winit: ModifiersState,
    hid_mod_mask: u8,
    size: PhysicalSize<u32>,
    exiting: bool,
}

impl App {
    fn new(cmd_tx: mpsc::Sender<AppCmd>) -> Self {
        Self {
            window: None,
            cmd_tx,
            mouse_buttons: 0,
            cursor_last: None,
            wheel_px_accum: 0.0,
            mods_winit: ModifiersState::empty(),
            hid_mod_mask: 0,
            size: PhysicalSize::new(800, 600),
            exiting: false,
        }
    }

    #[inline]
    fn send(&self, cmd: AppCmd) {
        let _ = self.cmd_tx.try_send(cmd); // never block the window thread
    }

    fn set_button(&mut self, button: winit::event::MouseButton, pressed: bool) {
        let bit = match button {
            winit::event::MouseButton::Left => 0,
            winit::event::MouseButton::Middle => 1,
            winit::event::MouseButton::Right => 2,
            _ => return,
        };
        if pressed {
            self.mouse_buttons |= 1 << bit;
        } else {
            self.mouse_buttons &= !(1 << bit);
        }
    }

    fn send_mouse(&self, dx: f64, dy: f64, wheel: i32) {
        let clamp = |v: f64| v.clamp(i8::MIN as f64, i8::MAX as f64) as i8;
        self.send(AppCmd::Mouse {
            buttons: self.mouse_buttons,
            dx: clamp(dx),
            dy: clamp(dy),
            wheel: wheel.clamp(i8::MIN as i32, i8::MAX as i32) as i8,
        });
    }

    fn note_modifier_physical_transition(&mut self, usage: u8, down: bool) {
        let bit = match usage {
            0xE0 => 0,
            0xE1 => 1,
            0xE2 => 2,
            0xE3 => 3,
            0xE4 => 4,
            0xE5 => 5,
            0xE6 => 6,
            0xE7 => 7,
            _ => return,
        };
        if down {
            self.hid_mod_mask |= 1 << bit;
        } else {
            self.hid_mod_mask &= !(1 << bit);
        }
    }

    fn mods_to_hid_mask(mods: ModifiersState) -> u8 {
        let mut m = 0u8;
        if mods.control_key() {
            m |= 1 << 0;
        } // LCtrl
        if mods.shift_key() {
            m |= 1 << 1;
        } // LShift
        if mods.alt_key() {
            m |= 1 << 2;
        } // LAlt
        if mods.super_key() {
            m |= 1 << 3;
        } // LGUI
        m
    }

    fn reconcile_mods(&mut self, want: u8) {
        const USAGE_FOR_BIT: [u8; 8] = [0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7];
        let have = self.hid_mod_mask;
        let to_release = have & !want;
        let to_press = want & !have;
        for bit in 0..8 {
            if (to_release & (1 << bit)) != 0 {
                self.send(AppCmd::KeyUp(USAGE_FOR_BIT[bit]));
            }
        }
        for bit in 0..8 {
            if (to_press & (1 << bit)) != 0 {
                self.send(AppCmd::KeyDown(USAGE_FOR_BIT[bit]));
            }
        }
        self.hid_mod_mask = want;
    }

    fn draw_once_black(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };

        // Create context/surface as locals so we don't store borrows.
        // Context<D>::new takes any D: HasDisplayHandle (e.g. &Window). :contentReference[oaicite:0]{index=0}
        let ctx = match SbContext::new(window) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("softbuffer context error: {e}");
                return;
            }
        };

        // Surface<D,W>::new(&Context<D>, W) with W: HasWindowHandle (e.g. &Window). :contentReference[oaicite:1]{index=1}
        let mut surf = match SbSurface::new(&ctx, window) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("softbuffer surface error: {e}");
                return;
            }
        };

        // softbuffer::Surface::resize requires NonZeroU32 sizes. :contentReference[oaicite:2]{index=2}
        let w = NonZeroU32::new(self.size.width.max(1)).unwrap();
        let h = NonZeroU32::new(self.size.height.max(1)).unwrap();
        if let Err(e) = surf.resize(w, h) {
            eprintln!("softbuffer resize error: {e}");
            return;
        }

        // Fill with opaque black (0xAARRGGBB) and present once.
        if let Ok(mut buf) = surf.buffer_mut() {
            buf.fill(0xFF00_0000);
            if let Err(e) = buf.present() {
                eprintln!("softbuffer present error: {e}");
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &winit::event_loop::ActiveEventLoop) {
        if self.exiting {
            return;
        }

        let win = el.create_window(Window::default_attributes()).unwrap();
        self.size = win.inner_size();
        self.window = Some(win);
        // Ask for the first frame so Wayland maps the toplevel.
        self.window.as_ref().unwrap().request_redraw();
        eprintln!("[winit] resumed -> window created");
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        eprintln!("[winit] {:?}", event);

        match event {
            WindowEvent::CloseRequested => {
                eprintln!("[winit] CloseRequested -> exit()");
                // Drop cmd_tx when App is dropped (after run_app returns)
                self.exiting = true;
                event_loop.exit();
            }
            WindowEvent::Destroyed => {
                eprintln!("[winit] Destroyed -> exit()");
                self.exiting = true;
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let usage = match &event.physical_key {
                    PhysicalKey::Code(code) => keycode_to_hid(*code), // your mapping fn
                    _ => None,
                };
                if let Some(u) = usage {
                    let down = matches!(event.state, ElementState::Pressed);
                    self.send(if down {
                        AppCmd::KeyDown(u)
                    } else {
                        AppCmd::KeyUp(u)
                    });
                    self.note_modifier_physical_transition(u, down);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods_winit = m.state();
                let want = App::mods_to_hid_mask(self.mods_winit);
                self.reconcile_mods(want);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.set_button(button, matches!(state, ElementState::Pressed));
                self.send_mouse(0.0, 0.0, 0);
            }
            WindowEvent::CursorEntered { .. } => {
                self.cursor_last = None;
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_last = None;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x, position.y);
                if let Some((px, py)) = self.cursor_last.replace((x, y)) {
                    self.send_mouse(x - px, y - py, 0);
                } else {
                    self.send_mouse(0.0, 0.0, 0);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                const PX_PER_NOTCH: f64 = 120.0;
                let mut notches = 0i32;
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        notches = y.round() as i32;
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        self.wheel_px_accum += p.y;
                        while self.wheel_px_accum.abs() >= PX_PER_NOTCH {
                            if self.wheel_px_accum > 0.0 {
                                notches += 1;
                                self.wheel_px_accum -= PX_PER_NOTCH;
                            } else {
                                notches -= 1;
                                self.wheel_px_accum += PX_PER_NOTCH;
                            }
                        }
                    }
                }
                if notches != 0 {
                    self.send_mouse(0.0, 0.0, notches);
                }
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    // Clear modifiers when focus is lost
                    self.reconcile_mods(0);
                }
                eprintln!("Focused = {focused}");
            }
            WindowEvent::Resized(sz) => {
                self.size = sz;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw_once_black(); // ← presents one frame
            }
            _ => {}
        }
    }
}

// Your mapping table (same as earlier)
fn keycode_to_hid(code: KeyCode) -> Option<u8> {
    use KeyCode::*;
    Some(match code {
        KeyA => 0x04,
        KeyB => 0x05,
        KeyC => 0x06,
        KeyD => 0x07,
        KeyE => 0x08,
        KeyF => 0x09,
        KeyG => 0x0A,
        KeyH => 0x0B,
        KeyI => 0x0C,
        KeyJ => 0x0D,
        KeyK => 0x0E,
        KeyL => 0x0F,
        KeyM => 0x10,
        KeyN => 0x11,
        KeyO => 0x12,
        KeyP => 0x13,
        KeyQ => 0x14,
        KeyR => 0x15,
        KeyS => 0x16,
        KeyT => 0x17,
        KeyU => 0x18,
        KeyV => 0x19,
        KeyW => 0x1A,
        KeyX => 0x1B,
        KeyY => 0x1C,
        KeyZ => 0x1D,
        Digit1 => 0x1E,
        Digit2 => 0x1F,
        Digit3 => 0x20,
        Digit4 => 0x21,
        Digit5 => 0x22,
        Digit6 => 0x23,
        Digit7 => 0x24,
        Digit8 => 0x25,
        Digit9 => 0x26,
        Digit0 => 0x27,
        Enter => 0x28,
        Escape => 0x29,
        Backspace => 0x2A,
        Tab => 0x2B,
        Space => 0x2C,
        Minus => 0x2D,
        Equal => 0x2E,
        BracketLeft => 0x2F,
        BracketRight => 0x30,
        Backslash => 0x31,
        IntlBackslash => 0x32,
        Semicolon => 0x33,
        Quote => 0x34,
        Backquote => 0x35,
        Comma => 0x36,
        Period => 0x37,
        Slash => 0x38,
        CapsLock => 0x39,
        F1 => 0x3A,
        F2 => 0x3B,
        F3 => 0x3C,
        F4 => 0x3D,
        F5 => 0x3E,
        F6 => 0x3F,
        F7 => 0x40,
        F8 => 0x41,
        F9 => 0x42,
        F10 => 0x43,
        F11 => 0x44,
        F12 => 0x45,
        PrintScreen => 0x46,
        ScrollLock => 0x47,
        Pause => 0x48,
        Insert => 0x49,
        Home => 0x4A,
        PageUp => 0x4B,
        Delete => 0x4C,
        End => 0x4D,
        PageDown => 0x4E,
        ArrowRight => 0x4F,
        ArrowLeft => 0x50,
        ArrowDown => 0x51,
        ArrowUp => 0x52,
        NumLock => 0x53,
        NumpadDivide => 0x54,
        NumpadMultiply => 0x55,
        NumpadSubtract => 0x56,
        NumpadAdd => 0x57,
        NumpadEnter => 0x58,
        Numpad1 => 0x59,
        Numpad2 => 0x5A,
        Numpad3 => 0x5B,
        Numpad4 => 0x5C,
        Numpad5 => 0x5D,
        Numpad6 => 0x5E,
        Numpad7 => 0x5F,
        Numpad8 => 0x60,
        Numpad9 => 0x61,
        Numpad0 => 0x62,
        NumpadDecimal => 0x63,
        ControlLeft => 0xE0,
        ShiftLeft => 0xE1,
        AltLeft => 0xE2,
        SuperLeft => 0xE3,
        ControlRight => 0xE4,
        ShiftRight => 0xE5,
        AltRight => 0xE6,
        SuperRight => 0xE7,
        _ => return None,
    })
}

// ------------------- BLE owner task (your logic) -------------------

async fn ble_owner_task(
    mut cmd_rx: mpsc::Receiver<AppCmd>,
    mut evt_rx: mpsc::Receiver<PeripheralEvent>,
    evt_tx: mpsc::Sender<PeripheralEvent>,
) -> anyhow::Result<()> {
    let (hid_service, mouse_in_uuid, keybd_in_uuid) = build_hid_services();

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
                value: Some(PERIPHERAL_NAME.as_bytes().to_vec()),
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_MODEL_NUM),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(PERIPHERAL_NAME.as_bytes().to_vec()),
                ..Default::default()
            },
        ],
    };

    // Peripheral setup
    // Create the Peripheral in this task so ownership is localized
    let mut peripheral = Peripheral::new(evt_tx).await?;
    while !peripheral.is_powered().await? {}
    peripheral.add_service(&hid_service).await?;
    peripheral.add_service(&bas_service).await?;
    peripheral.add_service(&dis_service).await?;
    peripheral
        .start_advertising(
            PERIPHERAL_NAME,
            &[
                Uuid::from_short(UUID_HID_SERVICE),
                Uuid::from_short(UUID_BAS_SERVICE),
                Uuid::from_short(UUID_DIS_SERVICE),
            ],
            Some(PERIPHERAL_APPEARANCE),
        )
        .await?;

    // Keyboard 6KRO state
    let mut modifiers: u8 = 0;
    let mut pressed: BTreeSet<u8> = BTreeSet::new();
    let mut notify = false;

    // Drive BLE with both BLE events and UI commands
    loop {
        select! {
            ev = evt_rx.recv() => {
                match ev {
                    Some(PeripheralEvent::StateUpdate{ is_powered }) => {
                        println!("Adapter powered: {is_powered}");
                    }
                    Some(PeripheralEvent::CharacteristicSubscriptionUpdate { request, subscribed }) => {
                        if request.characteristic == mouse_in_uuid {
                            notify = subscribed;
                            println!("Report notify MOUSE: {subscribed}");
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
            cmd = cmd_rx.recv() => {
                println!("Received command: {cmd:?}");
                match cmd {
                    Some(AppCmd::Mouse { buttons, dx, dy, wheel }) if notify => {
                        let pkt = build_mouse_report(buttons, dx, dy, wheel);
                        println!("TX mouse: btn={buttons:#04b} dx={dx} dy={dy} wheel={wheel}");
                        peripheral.update_characteristic(mouse_in_uuid, pkt.into()).await?;
                    }
                    Some(AppCmd::KeyDown(usage)) if notify => {
                        if let Some(m) = keyboard_usage_to_modifier(usage) {
                            modifiers |= m;
                        } else {
                            pressed.insert(usage);
                            while pressed.len() > 6 {
                                let first = *pressed.iter().next().unwrap();
                                pressed.remove(&first);
                            }
                        }
                        let pkt = build_keyboard_report(modifiers, &pressed);
                        println!("TX keybd DOWN: mods={:#010b} keys={:?}", modifiers, pressed);
                        peripheral.update_characteristic(keybd_in_uuid, pkt.into()).await?;
                    }
                    Some(AppCmd::KeyUp(usage)) if notify => {
                        if let Some(m) = keyboard_usage_to_modifier(usage) {
                            modifiers &= !m;
                        } else {
                            pressed.remove(&usage);
                        }
                        let pkt = build_keyboard_report(modifiers, &pressed);
                        println!("TX keybd UP: mods={:#010b} keys={:?}", modifiers, pressed);
                        peripheral.update_characteristic(keybd_in_uuid, pkt.into()).await?;
                    }
                    Some(AppCmd::Battery(level)) => {
                        peripheral.update_characteristic(Uuid::from_short(UUID_BATTERY_LEVEL), vec![level].into()).await?;
                        println!("Battery set to {}%", level);
                    }
                    Some(_) => {}
                    None => break, // UI dropped the sender (window closed) → exit cleanly
                }
            }
        }
    }

    peripheral.stop_advertising().await?;
    Ok(())
}

// ------------------- main: spawn BLE, run winit -------------------

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Channels
    let (cmd_tx, cmd_rx) = mpsc::channel::<AppCmd>(512);
    let (evt_tx, evt_rx) = mpsc::channel::<PeripheralEvent>(512);

    // Spawn BLE owner on Tokio. Move the receivers and evt_tx in as needed by your Peripheral::new(evt_tx).
    let ble_handle = tokio::spawn(async move {
        // Inside task, construct Peripheral with evt_tx (moved), then run loop with cmd_rx, evt_rx
        // Because we need evt_tx for Peripheral::new, pass it through with evt_rx:
        // If your Peripheral::new(evt_tx) signature requires evt_tx at construction,
        // move evt_tx in here and keep evt_rx in this task as we do.
        // For demonstration, assume Peripheral::new(evt_tx) is called inside ble_owner_task:
        if let Err(e) = ble_owner_task(cmd_rx, evt_rx, evt_tx).await {
            eprintln!("BLE task error: {e:#}");
        }
    });

    // Build & run winit app on the main thread (blocking)
    let mut app = App::new(cmd_tx.clone());
    let event_loop = event_loop::EventLoop::new()?;
    eprintln!("about to run_app");
    event_loop.run_app(&mut app)?;

    // After the window exits: drop the last sender to close the BLE task
    drop(cmd_tx);

    // Wait for BLE to finish cleanup
    let _ = ble_handle.await;

    Ok(())
}

// --- new: return two UUIDs (mouse_in, keybd_in) ---
fn build_hid_services() -> (Service, Uuid, Uuid) {
    let report_map: Vec<u8> = vec![
        // ----- Mouse, Report ID 1 -----
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xA1, 0x01, // Collection (Application)
        0x85, RID_MOUSE, //   Report ID (1)
        0x09, 0x01, //   Usage (Pointer)
        0xA1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page (Buttons)
        0x19, 0x01, //     Usage Minimum (Button 1)
        0x29, 0x03, //     Usage Maximum (Button 3)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x95, 0x03, //     Report Count (3)
        0x75, 0x01, //     Report Size (1)
        0x81, 0x02, //     Input (Data,Var,Abs)
        0x95, 0x01, //     Report Count (1)
        0x75, 0x05, //     Report Size (5)
        0x81, 0x03, //     Input (Const,Var,Abs)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x09, 0x38, //     Usage (Wheel)
        0x15, 0x81, //     Logical Minimum (-127)
        0x25, 0x7F, //     Logical Maximum (127)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x03, //     Report Count (3)
        0x81, 0x06, //     Input (Data,Var,Rel)
        0xC0, //   End Collection
        0xC0, // End Collection
        // ----- Keyboard, Report ID 2 -----
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x06, // Usage (Keyboard)
        0xA1, 0x01, // Collection (Application)
        0x85, RID_KEYBD, //   Report ID (2)
        0x05, 0x07, //   Usage Page (Keyboard/Keypad)
        // Modifier byte
        0x19, 0xE0, //   Usage Minimum (Left Ctrl)
        0x29, 0xE7, //   Usage Maximum (Right GUI)
        0x15, 0x00, //   Logical Minimum (0)
        0x25, 0x01, //   Logical Maximum (1)
        0x75, 0x01, //   Report Size (1)
        0x95, 0x08, //   Report Count (8)
        0x81, 0x02, //   Input (Data,Var,Abs)
        // Reserved byte
        0x75, 0x08, //   Report Size (8)
        0x95, 0x01, //   Report Count (1)
        0x81, 0x03, //   Input (Const,Var,Abs)
        // 6 Keycode array
        0x15, 0x00, //   Logical Minimum (0)
        0x25, 0x65, //   Logical Maximum (101)
        0x19, 0x00, //   Usage Minimum (0)
        0x29, 0x65, //   Usage Maximum (101)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x06, //   Report Count (6)
        0x81, 0x00, //   Input (Data,Array)
        0xC0, // End Collection
    ];

    let uuid_report_map = Uuid::from_short(UUID_HID_REPORT_MAP);
    let uuid_report = Uuid::from_short(UUID_HID_REPORT);

    // Distinct characteristic UUIDs (same 16-bit type) so you can address each:
    let mouse_in_uuid = uuid_report; // 0x2A4D
    let keybd_in_uuid = uuid_report;

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
                uuid: uuid_report_map,
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(report_map),
                ..Default::default()
            },
            // --- Mouse Input Report (RID 1) ---
            Characteristic {
                uuid: mouse_in_uuid,
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
                permissions: vec![AttributePermission::Readable],
                // Many stacks add CCCD automatically for Notify; descriptor below is the Report Reference:
                descriptors: vec![Descriptor {
                    uuid: Uuid::from_short(UUID_REPORT_REF_DESC),
                    // [Report ID, Report Type(Input=1, Output=2, Feature=3)]
                    value: Some(vec![RID_MOUSE, 0x01]),
                    ..Default::default()
                }],
                ..Default::default()
            },
            // --- Keyboard Input Report (RID 2) ---
            Characteristic {
                uuid: keybd_in_uuid,
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
                permissions: vec![AttributePermission::Readable],
                descriptors: vec![Descriptor {
                    uuid: Uuid::from_short(UUID_REPORT_REF_DESC),
                    value: Some(vec![RID_KEYBD, 0x01]),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
    };

    (hid_service, mouse_in_uuid, keybd_in_uuid)
}

/// 5-byte mouse: [RID, buttons, dx, dy, wheel]
fn build_mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8) -> Vec<u8> {
    vec![RID_MOUSE, buttons, dx as u8, dy as u8, wheel as u8]
}

/// 9-byte keyboard: [RID, mods, reserved, k0..k5]
fn build_keyboard_report(mods: u8, pressed: &BTreeSet<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 8);
    out.push(RID_KEYBD);
    out.push(mods);
    out.push(0x00); // reserved
    for &k in pressed.iter().take(6) {
        out.push(k);
    }
    while out.len() < 1 + 1 + 1 + 6 {
        out.push(0);
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
