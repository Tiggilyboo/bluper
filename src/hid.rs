use std::collections::BTreeSet;

use uuid::Uuid;
use winit::keyboard::KeyCode;

use ble_peripheral_rust::{
    gatt::{
        characteristic::Characteristic,
        properties::{AttributePermission, CharacteristicProperty},
        service::Service,
    },
    uuid::ShortUuid,
};

use crate::consts::*;

pub fn keycode_to_hid(code: KeyCode) -> Option<u8> {
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

pub fn keyboard_usage_to_modifier(usage: u8) -> Option<u8> {
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

pub fn build_mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8) -> [u8; 5] {
    [RID_MOUSE, buttons, dx as u8, dy as u8, wheel as u8]
}

pub fn build_keyboard_report(mods: u8, pressed: &BTreeSet<u8>) -> [u8; 9] {
    let mut out = [0u8; 9];
    out[0] = RID_KEYBD;
    out[1] = mods;
    out[2] = 0x00; // reserved
    for (i, &k) in pressed.iter().take(6).enumerate() {
        out[3 + i] = k;
    }
    out
}

// Single Input Report characteristic carrying both mouse and keyboard via Report IDs
pub fn build_hid_service() -> (Service, Uuid) {
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

    let input_uuid = Uuid::from_short(UUID_HID_REPORT);

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
                permissions: vec![AttributePermission::WriteEncryptionRequired],
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_PROTOCOL_MODE),
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Write],
                permissions: vec![
                    AttributePermission::ReadEncryptionRequired,
                    AttributePermission::WriteEncryptionRequired,
                ],
                value: Some(vec![0x01]),
                ..Default::default()
            },
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_REPORT_MAP),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::ReadEncryptionRequired],
                value: Some(report_map),
                ..Default::default()
            },
            // Single Input Report characteristic carrying all RIDs
            Characteristic {
                uuid: input_uuid,
                properties: vec![
                    CharacteristicProperty::Read,
                    CharacteristicProperty::NotifyEncryptionRequired,
                ],
                permissions: vec![AttributePermission::ReadEncryptionRequired],
                ..Default::default()
            },
        ],
    };

    (hid_service, input_uuid)
}
