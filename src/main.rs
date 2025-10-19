use anyhow::{Context, Result};
use devicectrl_common::{
    DeviceState,
    protocol::simple::{
        DeviceBoundSimpleMessage, ServerBoundSimpleMessage,
        tokio::{CryptoContext, TransportEvent, make_transport_channels, transport_task},
    },
};
use hciraw::{HciChannel, HciSocket, HciSocketAddr};
use sd_notify::NotifyState;
use std::{env, path::PathBuf, time::Duration};
use tokio::{sync::Mutex, time::sleep};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

use crate::fan::{CachedFanState, send_keepalive_to_fan, send_update_to_fan};

mod ble;
mod config;
mod fan;

struct AppState {
    pub hci_socket: HciSocket,
    pub fan_state: Mutex<CachedFanState>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .without_time() // systemd logs already include timestamps
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .with_env_var("LOG_LEVEL")
                .from_env()?,
        )
        .init();

    let config = Box::leak(Box::new(
        config::load_config(&PathBuf::from(
            env::var("CONFIG_PATH").expect("CONFIG_PATH env var missing!"),
        ))
        .await
        .context("failed to load config")?,
    ));

    let app_state: &AppState = Box::leak(Box::new(AppState {
        hci_socket: HciSocket::bind(HciSocketAddr::new(Some(config.hci_device), HciChannel::Raw))?,
        fan_state: Mutex::new(CachedFanState {
            tx_count: 16, // this is what FanLampPro app initializes with
            power: true,
            temperature: 0,
            brightness: 255,

            remote_uid: config.remote_uid,
        }),
    }));

    let (mut client_channels, worker_channels) = make_transport_channels(16);

    let crypto = CryptoContext {
        server_public_key: config.server_public_key,
        private_key: config.private_key.clone(),
    };

    tokio::spawn(transport_task(
        config.server_addr,
        worker_channels,
        config.device_id,
        crypto,
    ));

    // Sometimes the fan ignores commands when it has not received one for a while.
    // I have not found anything documenting this, but sending a 'keepalive' seems to work. 🤷‍♂️
    tokio::spawn({
        async move {
            loop {
                sleep(Duration::from_secs(60 * 60)).await;

                let mut fan_state = app_state.fan_state.lock().await;
                if let Err(err) = send_keepalive_to_fan(&mut fan_state, &app_state.hci_socket).await
                {
                    log::error!("{:?}", err.context("Failed to send keepalive to fan"));
                }
            }
        }
    });

    let _ = sd_notify::notify(false, &[NotifyState::Ready]);

    loop {
        match client_channels
            .incoming
            .recv()
            .await
            .context("Failed to receive command")?
        {
            TransportEvent::Connected => {
                log::info!("Connected to server!");
            }
            TransportEvent::Error(err) => {
                log::error!("{:?}", err.context("failed to communicate with server"));
            }
            TransportEvent::Message(DeviceBoundSimpleMessage::UpdateCommand(update)) => {
                // since this takes 500ms the recv() call above may lag when under pressure
                let mut fan_state = app_state.fan_state.lock().await;
                send_update_to_fan(update.update, &mut fan_state, &app_state.hci_socket).await?;
            }
            TransportEvent::Message(DeviceBoundSimpleMessage::StateQuery { device_id }) => {
                client_channels
                    .outgoing
                    .send(ServerBoundSimpleMessage::UpdateNotification(
                        devicectrl_common::UpdateNotification {
                            device_id,
                            reachable: true,
                            new_state: DeviceState::Unknown,
                        },
                    ))
                    .await?;
            }
            _ => {}
        }
    }
}
