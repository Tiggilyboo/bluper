use std::collections::BTreeSet;
use tokio::{select, sync::mpsc};
use uuid::Uuid;

use ble_peripheral_rust::{
    Peripheral, PeripheralImpl,
    gatt::peripheral_event::{
        PeripheralEvent, ReadRequestResponse, RequestResponse, WriteRequestResponse,
    },
    uuid::ShortUuid,
};

use crate::consts::*;
use crate::hid::{
    build_hid_service, build_keyboard_report, build_mouse_report, keyboard_usage_to_modifier,
};
use crate::ui::AppCmd;

pub async fn ble_owner_task(
    mut cmd_rx: mpsc::Receiver<AppCmd>,
    mut evt_rx: mpsc::Receiver<PeripheralEvent>,
    evt_tx: mpsc::Sender<PeripheralEvent>,
    device_name: String,
    appearance: Option<u16>,
) -> anyhow::Result<()> {
    let (hid_service, input_uuid) = build_hid_service();

    let bas_service = ble_peripheral_rust::gatt::service::Service {
        uuid: Uuid::from_short(UUID_BAS_SERVICE),
        primary: true,
        characteristics: vec![ble_peripheral_rust::gatt::characteristic::Characteristic {
            uuid: Uuid::from_short(UUID_BATTERY_LEVEL),
            properties: vec![
                ble_peripheral_rust::gatt::properties::CharacteristicProperty::Read,
                ble_peripheral_rust::gatt::properties::CharacteristicProperty::NotifyEncryptionRequired,
            ],
            permissions: vec![ble_peripheral_rust::gatt::properties::AttributePermission::ReadEncryptionRequired],
            value: Some(vec![0]),
            ..Default::default()
        }],
    };

    let dis_service = ble_peripheral_rust::gatt::service::Service {
        uuid: Uuid::from_short(UUID_DIS_SERVICE),
        primary: true,
        characteristics: vec![
            ble_peripheral_rust::gatt::characteristic::Characteristic {
                uuid: Uuid::from_short(UUID_MFG_NAME),
                properties: vec![
                    ble_peripheral_rust::gatt::properties::CharacteristicProperty::Read,
                ],
                permissions: vec![
                    ble_peripheral_rust::gatt::properties::AttributePermission::Readable,
                ],
                value: Some(device_name.as_bytes().to_vec()),
                ..Default::default()
            },
            ble_peripheral_rust::gatt::characteristic::Characteristic {
                uuid: Uuid::from_short(UUID_MODEL_NUM),
                properties: vec![
                    ble_peripheral_rust::gatt::properties::CharacteristicProperty::Read,
                ],
                permissions: vec![
                    ble_peripheral_rust::gatt::properties::AttributePermission::Readable,
                ],
                value: Some(device_name.as_bytes().to_vec()),
                ..Default::default()
            },
        ],
    };

    let mut peripheral = Peripheral::new(evt_tx).await?;

    // Backoff until powered
    let mut delay_ms = 50u64;
    loop {
        if peripheral.is_powered().await? {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        delay_ms = (delay_ms * 2).min(1000);
    }

    peripheral.add_service(&hid_service).await?;
    peripheral.add_service(&bas_service).await?;
    peripheral.add_service(&dis_service).await?;

    let mut advertising = false;
    if !advertising {
        peripheral
            .start_advertising(
                &device_name,
                &[
                    Uuid::from_short(UUID_HID_SERVICE),
                    Uuid::from_short(UUID_BAS_SERVICE),
                    Uuid::from_short(UUID_DIS_SERVICE),
                ],
                appearance,
            )
            .await?;
        advertising = true;
        tracing::info!("Advertising {}", &device_name);
    }

    let mut modifiers: u8 = 0;
    let mut pressed: BTreeSet<u8> = BTreeSet::new();
    let mut input_notify = false;
    let mut battery_notify = false;
    let mut last_battery: u8 = 95;

    loop {
        select! {
            ev = evt_rx.recv() => {
                match ev {
                    Some(PeripheralEvent::StateUpdate{ is_powered }) => {
                        tracing::info!(%is_powered, "Adapter powered");
                        if is_powered {
                            if !advertising {
                                if let Err(e) = peripheral.start_advertising(
                                    &device_name,
                                    &[
                                        Uuid::from_short(UUID_HID_SERVICE),
                                        Uuid::from_short(UUID_BAS_SERVICE),
                                        Uuid::from_short(UUID_DIS_SERVICE),
                                    ],
                                    appearance,
                                ).await {
                                    tracing::error!(error = %format!("{e:#}"), "advertise start error");
                                } else {
                                    advertising = true;
                                }
                            }
                        } else {
                            if advertising {
                                if let Err(e) = peripheral.stop_advertising().await { tracing::error!(error = %format!("{e:#}"), "advertise stop error"); }
                                advertising = false;
                            }
                        }
                    }
                    Some(PeripheralEvent::CharacteristicSubscriptionUpdate { request, subscribed }) => {
                        if request.characteristic == input_uuid {
                            input_notify = subscribed;
                            tracing::info!(%subscribed, "Report notify INPUT");
                        } else if request.characteristic == Uuid::from_short(UUID_BATTERY_LEVEL) {
                            battery_notify = subscribed;
                            tracing::info!(%subscribed, "Report notify BATTERY");
                        } else {
                            tracing::debug!(%subscribed, ?request, "Other subscription");
                        }
                    }
                    Some(PeripheralEvent::ReadRequest{ request, offset, responder }) => {
                        tracing::debug!(?request, %offset, "ReadRequest");
                        let value = if request.characteristic == Uuid::from_short(UUID_BATTERY_LEVEL) {
                            vec![last_battery]
                        } else {
                            Vec::<u8>::new()
                        };
                        let _ = responder.send(ReadRequestResponse{
                            value: value.into(),
                            response: RequestResponse::Success
                        });
                    }
                    Some(PeripheralEvent::WriteRequest{ request, offset, value, responder }) => {
                        tracing::debug!(?request, %offset, ?value, "WriteRequest");
                        let _ = responder.send(WriteRequestResponse{ response: RequestResponse::Success });
                    }
                    None => break,
                }
            }
            cmd = cmd_rx.recv() => {
                tracing::trace!(?cmd, "Received command");
                match cmd {
                    Some(AppCmd::Mouse { buttons, dx, dy, wheel }) if input_notify => {
                        let pkt = build_mouse_report(buttons, dx, dy, wheel);
                        tracing::trace!(buttons = %format!("{buttons:#04b}"), %dx, %dy, %wheel, "TX mouse");
                        peripheral.update_characteristic(input_uuid, pkt.to_vec().into()).await?;
                    }
                    Some(AppCmd::KeyDown(usage)) if input_notify => {
                        if let Some(m) = keyboard_usage_to_modifier(usage) { modifiers |= m; }
                        else {
                            pressed.insert(usage);
                            while pressed.len() > 6 { let first = *pressed.iter().next().unwrap(); pressed.remove(&first); }
                        }
                        let pkt = build_keyboard_report(modifiers, &pressed);
                        tracing::trace!(mods = %format!("{modifiers:#010b}"), ?pressed, "TX keybd DOWN");
                        peripheral.update_characteristic(input_uuid, pkt.to_vec().into()).await?;
                    }
                    Some(AppCmd::KeyUp(usage)) if input_notify => {
                        if let Some(m) = keyboard_usage_to_modifier(usage) { modifiers &= !m; }
                        else { pressed.remove(&usage); }
                        let pkt = build_keyboard_report(modifiers, &pressed);
                        tracing::trace!(mods = %format!("{modifiers:#010b}"), ?pressed, "TX keybd UP");
                        peripheral.update_characteristic(input_uuid, pkt.to_vec().into()).await?;
                    }
                    Some(AppCmd::Battery(level)) => {
                        if level != last_battery {
                            last_battery = level;
                            if battery_notify {
                                peripheral.update_characteristic(Uuid::from_short(UUID_BATTERY_LEVEL), vec![level].into()).await?;
                            }
                            tracing::info!(%level, "Battery set");
                        }
                    }
                    Some(AppCmd::Exit) => break,
                    None => break,
                    Some(_) => {}
                }
            }
        }
    }

    peripheral.stop_advertising().await?;
    Ok(())
}
