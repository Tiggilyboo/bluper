use tokio::sync::mpsc;
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

// ---- UUIDs ----
const UUID_HID_SERVICE: u16 = 0x1812;
const UUID_BAS_SERVICE: u16 = 0x180F;
const UUID_DIS_SERVICE: u16 = 0x180A;

const UUID_HID_INFO: u16 = 0x2A4A;
const UUID_HID_CONTROL_POINT: u16 = 0x2A4C;
const UUID_HID_PROTOCOL_MODE: u16 = 0x2A4E;
const UUID_HID_REPORT_MAP: u16 = 0x2A4B;
const UUID_HID_REPORT: u16 = 0x2A4D;

const UUID_BATTERY_LEVEL: u16 = 0x2A19;
const UUID_MFG_NAME: u16 = 0x2A29;
const UUID_MODEL_NUM: u16 = 0x2A24;

const UUID_REPORT_REF_DESC: u16 = 0x2908; // [report_id, report_type=1(Input)]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // -------- HID Report Map: 3 buttons + X/Y + wheel; Report ID = 1 --------
    let report_map: Vec<u8> = vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xA1, 0x01, // Collection (Application)
        0x85, 0x01, //   Report ID (1)
        0x09, 0x01, //   Usage (Pointer)
        0xA1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page (Button)
        0x19, 0x01, //     Usage Minimum (1)
        0x29, 0x03, //     Usage Maximum (3)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x95, 0x03, //     Report Count (3)
        0x75, 0x01, //     Report Size (1)
        0x81, 0x02, //     Input (Data,Var,Abs) ; buttons
        0x95, 0x01, //     Report Count (1)
        0x75, 0x05, //     Report Size (5)
        0x81, 0x03, //     Input (Const,Var,Abs) ; padding
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x09, 0x38, //     Usage (Wheel)
        0x15, 0x81, //     Logical Minimum (-127)
        0x25, 0x7F, //     Logical Maximum (127)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x03, //     Report Count (3)
        0x81, 0x06, //     Input (Data,Var,Rel) ; dx,dy,wheel
        0xC0, 0xC0,
    ];

    // -------- Services --------
    let hid_service = Service {
        uuid: Uuid::from_short(UUID_HID_SERVICE),
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_INFO),
                properties: vec![CharacteristicProperty::Read],
                permissions: vec![AttributePermission::Readable],
                value: Some(vec![0x11, 0x01, 0x00, 0x00]), // bcdHID=0x0111, country=0, flags=0
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
            // Input Report (notify + read) with Report Reference descriptor
            Characteristic {
                uuid: Uuid::from_short(UUID_HID_REPORT),
                properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
                permissions: vec![AttributePermission::Readable],
                value: None,
                descriptors: vec![Descriptor {
                    uuid: Uuid::from_short(UUID_REPORT_REF_DESC),
                    value: Some(vec![0x01, 0x01]), // report_id=1, report_type=Input(1)
                    ..Default::default()
                }],
                ..Default::default()
            },
        ],
    };

    let bas_service = Service {
        uuid: Uuid::from_short(UUID_BAS_SERVICE),
        primary: true,
        characteristics: vec![Characteristic {
            uuid: Uuid::from_short(UUID_BATTERY_LEVEL),
            properties: vec![CharacteristicProperty::Read, CharacteristicProperty::Notify],
            permissions: vec![AttributePermission::Readable],
            value: Some(vec![95]), // 95%
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
                value: Some(b"Rusty HID Inc.".to_vec()),
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

    // -------- Peripheral bring-up --------
    let (tx, mut rx) = mpsc::channel::<PeripheralEvent>(256);
    let mut peripheral = Peripheral::new(tx).await?;

    // Wait for adapter, register services, then advertise
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
        )
        .await?;

    // -------- Event loop (NO globals; we hold & use `peripheral` here) --------
    loop {
        match rx.recv().await {
            Some(PeripheralEvent::StateUpdate { is_powered }) => {
                println!("PowerOn: {is_powered}");
            }
            Some(PeripheralEvent::CharacteristicSubscriptionUpdate {
                request,
                subscribed,
            }) => {
                println!("Subscription update: {subscribed} {:?}", request);
                if subscribed && request.service == Uuid::from_short(UUID_HID_REPORT) {
                    // Send a tiny mouse move: [report_id=1, buttons=0, dx=+5, dy=+3, wheel=0]
                    let pkt = vec![0x01, 0x00, 5i8 as u8, 3i8 as u8, 0u8];
                    peripheral
                        .update_characteristic(Uuid::from_short(UUID_HID_REPORT), pkt.into())
                        .await?;
                }
            }
            Some(PeripheralEvent::ReadRequest {
                request,
                offset,
                responder,
            }) => {
                println!("ReadRequest: {:?} offset={}", request, offset);
                responder
                    .send(ReadRequestResponse {
                        value: Vec::<u8>::new().into(),
                        response: RequestResponse::Success,
                    })
                    .ok();
            }
            Some(PeripheralEvent::WriteRequest {
                request,
                offset,
                value,
                responder,
            }) => {
                println!(
                    "WriteRequest: {:?} offset={} value={:?}",
                    request, offset, value
                );
                responder
                    .send(WriteRequestResponse {
                        response: RequestResponse::Success,
                    })
                    .ok();
            }
            None => break, // channel closed
        }
    }

    Ok(())
}
