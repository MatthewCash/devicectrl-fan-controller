use anyhow::{Context, Result, bail};
use devicectrl_common::{
    DeviceState, DeviceStateUpdate,
    device_types::ceiling_fan::CeilingFanStateUpdate,
    protocol::simple::{DeviceBoundSimpleMessage, SIGNATURE_LEN, ServerBoundSimpleMessage},
};
use futures::{SinkExt, StreamExt, TryStreamExt};
use p256::ecdsa::{
    Signature,
    signature::{SignerMut, Verifier},
};
use tokio::{net::TcpStream, sync::broadcast};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::config::Config;

pub async fn connect_to_server(
    config: &Config,
    command_sender: &broadcast::Sender<CeilingFanStateUpdate>,
) -> Result<()> {
    let socket = TcpStream::connect(config.server_addr).await?;
    let (mut write, mut read) = Framed::new(socket, LengthDelimitedCodec::new()).split();

    log::info!("Connected to server");

    // send identify message
    write
        .send(serde_json::to_vec(&ServerBoundSimpleMessage::Identify(config.device_id))?.into())
        .await?;

    let mut send_message = async |request: &ServerBoundSimpleMessage| {
        let mut data = serde_json::to_vec(request)?;

        let sig: Signature = config.private_key.clone().try_sign(&data)?;
        data.splice(0..0, sig.to_bytes());

        write.send(data.into()).await?;

        Result::<()>::Ok(())
    };

    while let Some(buf) = read.try_next().await? {
        let sig: &[u8; SIGNATURE_LEN] = &buf
            .get(..SIGNATURE_LEN)
            .context("message is not long enough for signature")?
            .try_into()?;
        let data = &buf.get(SIGNATURE_LEN..).context("message is too short")?;

        config
            .server_public_key
            .verify(data, &Signature::from_slice(sig)?)?;

        let message: DeviceBoundSimpleMessage = serde_json::from_slice(data)?;
        log::debug!("received message: {message:?}");

        match message {
            DeviceBoundSimpleMessage::UpdateCommand(update) => {
                if update.device_id != config.device_id {
                    bail!("Update notification does not match this device id!")
                }

                let DeviceStateUpdate::CeilingFan(new_state) = update.change_to else {
                    bail!("Requested state is not a ceiling fan state!")
                };

                if let Err(err) = command_sender
                    .send(new_state)
                    .context("failed to enqueue fan state update")
                {
                    log::error!("{err:?}");
                }

                send_message(&ServerBoundSimpleMessage::UpdateNotification(
                    devicectrl_common::UpdateNotification {
                        device_id: config.device_id,
                        reachable: true,
                        new_state: DeviceState::Unknown,
                    },
                ))
                .await?;
            }
            DeviceBoundSimpleMessage::StateQuery { device_id } => {
                if device_id != config.device_id {
                    bail!("State query notification does not match this device id!")
                }

                send_message(&ServerBoundSimpleMessage::UpdateNotification(
                    devicectrl_common::UpdateNotification {
                        device_id: config.device_id,
                        reachable: true,
                        new_state: DeviceState::Unknown,
                    },
                ))
                .await?;
            }
            _ => log::error!("Unknown command received!"),
        }
    }

    bail!("connection closed")
}
