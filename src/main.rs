use anyhow::{Context, Result};
use devicectrl_common::device_types::ceiling_fan::CeilingFanStateUpdate;
use hciraw::{HciChannel, HciSocket, HciSocketAddr};
use sd_notify::NotifyState;
use std::{env, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::broadcast, time::sleep};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

use crate::{
    fan::{CachedFanState, send_update_to_fan},
    transport::connect_to_server,
};

mod ble;
mod config;
mod fan;
mod transport;

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

    let config = Arc::new(
        config::load_config(&PathBuf::from(
            env::var("CONFIG_PATH").expect("CONFIG_PATH env var missing!"),
        ))
        .await
        .context("failed to load config")?,
    );

    // only store about 2.5s worth of commands in the channel
    let (command_sender, mut command_receiver) = broadcast::channel::<CeilingFanStateUpdate>(5);

    let hci_socket = HciSocket::bind(HciSocketAddr::new(Some(config.hci_device), HciChannel::Raw))?;

    tokio::spawn(async move {
        loop {
            if let Err(err) = connect_to_server(&config, &command_sender).await {
                log::error!("{:?}", err.context("Failed to handle server loop"));
            }
            sleep(Duration::from_secs(5)).await;
        }
    });

    let _ = sd_notify::notify(false, &[NotifyState::Ready]);

    let mut fan_state = CachedFanState {
        tx_count: 16, // this is what FanLampPro app initializes with
        power: true,
        temperature: 0,
        brightness: 255,
    };

    loop {
        let new_state = command_receiver
            .recv()
            .await
            .context("Failed to receive command")?;

        // since this takes 500ms the recv() call above may lag when under pressure
        send_update_to_fan(new_state, &mut fan_state, &hci_socket).await?;
    }
}
