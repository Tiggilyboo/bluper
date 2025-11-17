mod ble;
mod consts;
pub mod hid;
mod host_power;
mod ui;

use ble_peripheral_rust::gatt::peripheral_event::PeripheralEvent;
use tokio::sync::mpsc;
use winit::event_loop;

use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

use crate::ble::ble_owner_task;
use crate::ui::{App, AppCmd};

#[derive(Debug, Parser)]
#[command(name = "bluper", version, about = "BLE HID K+M peripheral")]
struct Cli {
    #[arg(long, default_value = "Bluper")]
    name: String,
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Init tracing with env override, else CLI level
    let env_filter = std::env::var("RUST_LOG").unwrap_or_else(|_| cli.log_level.clone());
    fmt().with_env_filter(EnvFilter::new(env_filter)).init();

    let (cmd_tx, cmd_rx) = mpsc::channel::<AppCmd>(512);
    let (evt_tx, evt_rx) = mpsc::channel::<PeripheralEvent>(512);

    // Spawn periodic battery poller
    {
        let cmd = cmd_tx.clone();
        tokio::spawn(async move {
            let mut last_sent: Option<u8> = None;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                if let Some(p) = crate::host_power::get_battery_percent() {
                    if last_sent != Some(p) {
                        if cmd.send(AppCmd::Battery(p)).await.is_err() {
                            break;
                        }
                        last_sent = Some(p);
                        tracing::debug!(%p, "Battery polled");
                    }
                }
            }
        });
    }

    let name = cli.name.clone();
    let appearance = Some(consts::PERIPHERAL_APPEARANCE);

    let ble_handle = tokio::spawn(async move {
        if let Err(e) = ble_owner_task(cmd_rx, evt_rx, evt_tx, name, appearance).await {
            tracing::error!(error = %format!("{e:#}"), "BLE task error");
        }
    });

    let mut app = App::new(cmd_tx.clone());
    let event_loop = event_loop::EventLoop::new()?;
    event_loop.run_app(&mut app)?;

    drop(cmd_tx);
    let _ = ble_handle.await;
    Ok(())
}
