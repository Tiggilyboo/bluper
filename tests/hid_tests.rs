use std::collections::BTreeSet;

use bluper::hid::{build_keyboard_report, build_mouse_report, keycode_to_hid};
use winit::keyboard::KeyCode;

#[test]
fn keyboard_report_length_and_padding() {
    let mut pressed = BTreeSet::new();
    // Push 7 keys; only first 6 should be included
    for k in [0x04u8, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A] { pressed.insert(k); }
    let mods = 0b0001_0010; // example mask
    let pkt = build_keyboard_report(mods, &pressed);
    assert_eq!(pkt.len(), 9);
    assert_eq!(pkt[0], 0x02); // RID keyboard
    assert_eq!(pkt[1], mods);
    assert_eq!(pkt[2], 0x00); // reserved
    // check first 6 keycodes
    assert_eq!(&pkt[3..9], &[0x04, 0x05, 0x06, 0x07, 0x08, 0x09]);
}

#[test]
fn mouse_report_layout() {
    let pkt = build_mouse_report(0b0000_0111, -10, 5, 1);
    assert_eq!(pkt, [0x01, 0b0000_0111, 246, 5, 1]); // 246 = -10u8
}

#[test]
fn keycode_mapping_basic() {
    assert_eq!(keycode_to_hid(KeyCode::KeyA), Some(0x04));
    assert_eq!(keycode_to_hid(KeyCode::Digit1), Some(0x1E));
    assert_eq!(keycode_to_hid(KeyCode::Enter), Some(0x28));
}
