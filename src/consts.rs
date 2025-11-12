// Common constants for HID over GATT profile and device identity

pub const UUID_HID_SERVICE: u16 = 0x1812;
pub const UUID_BAS_SERVICE: u16 = 0x180F;
pub const UUID_DIS_SERVICE: u16 = 0x180A;

pub const UUID_HID_INFO: u16 = 0x2A4A;
pub const UUID_HID_CONTROL_POINT: u16 = 0x2A4C;
pub const UUID_HID_PROTOCOL_MODE: u16 = 0x2A4E;
pub const UUID_HID_REPORT_MAP: u16 = 0x2A4B;
pub const UUID_HID_REPORT: u16 = 0x2A4D;

pub const UUID_BATTERY_LEVEL: u16 = 0x2A19;
pub const UUID_MFG_NAME: u16 = 0x2A29;
pub const UUID_MODEL_NUM: u16 = 0x2A24;

pub const PERIPHERAL_APPEARANCE: u16 = 0x03C0;

// Report IDs
pub const RID_MOUSE: u8 = 0x01;
pub const RID_KEYBD: u8 = 0x02;
