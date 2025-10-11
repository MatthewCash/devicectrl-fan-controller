use anyhow::{Context, Result, bail};
use devicectrl_common::{
    DeviceState,
    protocol::simple::{DeviceBoundSimpleMessage, SIGNATURE_LEN, ServerBoundSimpleMessage},
    updates::AttributeUpdate,
};
use p256::ecdsa::{
    Signature,
    signature::{SignerMut, Verifier},
};
use p256::elliptic_curve::rand_core::{OsRng, RngCore};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::broadcast,
};

use crate::config::Config;

pub async fn connect_to_server(
    config: &Config,
    command_sender: &broadcast::Sender<AttributeUpdate>,
) -> Result<()> {
    let mut socket = TcpStream::connect(config.server_addr)
        .await
        .context("failed to connect to server")?;

    let mut expected_recv_nonce = OsRng.next_u32();
    socket
        .write_u32(expected_recv_nonce)
        .await
        .context("failed to send client nonce")?;

    let mut send_nonce = socket
        .read_u32()
        .await
        .context("failed to read server nonce")?;

    send_identify_message(&mut socket, config).await?;

    log::info!("Connected to server!");

    loop {
        let recv_nonce = match socket.read_u32().await {
            Ok(n) => n,
            Err(e) => bail!("connection closed or failed reading server nonce: {e:?}"),
        };

        expected_recv_nonce = expected_recv_nonce.wrapping_add(1);
        if recv_nonce != expected_recv_nonce {
            bail!(
                "Server nonce mismatch: expected {}, got {}",
                expected_recv_nonce,
                recv_nonce
            );
        }

        let payload_len = socket
            .read_u32()
            .await
            .context("failed to read payload length")? as usize;

        let mut payload = vec![0u8; payload_len];
        socket
            .read_exact(&mut payload)
            .await
            .context("failed to read payload")?;

        let mut sig_buf = [0u8; SIGNATURE_LEN];
        socket
            .read_exact(&mut sig_buf)
            .await
            .context("failed to read signature")?;

        let mut signed_region = Vec::with_capacity(
            core::mem::size_of::<u32>() + core::mem::size_of::<u32>() + payload.len(),
        );
        signed_region.extend_from_slice(&recv_nonce.to_be_bytes());
        signed_region.extend_from_slice(&(payload_len as u32).to_be_bytes());
        signed_region.extend_from_slice(&payload);

        config
            .server_public_key
            .verify(&signed_region, &Signature::from_slice(&sig_buf)?)
            .context("signature verification failed")?;

        let message: DeviceBoundSimpleMessage =
            serde_json::from_slice(&payload).context("failed to decode server message")?;
        log::debug!("received message: {message:?}");

        match message {
            DeviceBoundSimpleMessage::UpdateCommand(command) => {
                if command.device_id != config.device_id {
                    bail!("Update command device id does not match this device id");
                }

                if let Err(err) = command_sender
                    .send(command.update)
                    .context("failed to enqueue device update")
                {
                    log::error!("{err:?}");
                }

                send_signed_message(
                    &mut socket,
                    &mut send_nonce,
                    config,
                    &ServerBoundSimpleMessage::UpdateNotification(
                        devicectrl_common::UpdateNotification {
                            device_id: config.device_id,
                            reachable: true,
                            new_state: DeviceState::Unknown,
                        },
                    ),
                )
                .await?;
            }
            DeviceBoundSimpleMessage::StateQuery { device_id } => {
                if device_id != config.device_id {
                    bail!("State query device id does not match this device id");
                }

                send_signed_message(
                    &mut socket,
                    &mut send_nonce,
                    config,
                    &ServerBoundSimpleMessage::UpdateNotification(
                        devicectrl_common::UpdateNotification {
                            device_id: config.device_id,
                            reachable: true,
                            new_state: DeviceState::Unknown,
                        },
                    ),
                )
                .await?;
            }
            _ => {
                log::error!("Unknown command received");
            }
        }
    }
}

async fn send_identify_message(socket: &mut TcpStream, config: &Config) -> Result<()> {
    // [ u32 len | data ]
    let data = serde_json::to_vec(&ServerBoundSimpleMessage::Identify(config.device_id))
        .context("failed to encode identify message")?;
    let len_be = (data.len() as u32).to_be_bytes();

    socket
        .write_all(&len_be)
        .await
        .context("failed to write identify length")?;
    socket
        .write_all(&data)
        .await
        .context("failed to write identify payload")?;

    Ok(())
}

async fn send_signed_message(
    socket: &mut TcpStream,
    send_nonce: &mut u32,
    config: &Config,
    message: &ServerBoundSimpleMessage,
) -> Result<()> {
    let payload = serde_json::to_vec(message).context("failed to encode outbound message")?;
    let payload_len = payload.len() as u32;

    // Build [ nonce | len | payload ]
    *send_nonce = send_nonce.wrapping_add(1);
    let mut to_sign = Vec::with_capacity(
        core::mem::size_of::<u32>() + core::mem::size_of::<u32>() + payload.len(),
    );
    to_sign.extend_from_slice(&send_nonce.to_be_bytes());
    to_sign.extend_from_slice(&payload_len.to_be_bytes());
    to_sign.extend_from_slice(&payload);

    let sig: Signature = config
        .private_key
        .clone()
        .try_sign(&to_sign)
        .context("failed to sign outbound message")?;

    // Send [ nonce | len | payload | signature ]
    socket
        .write_all(&to_sign)
        .await
        .context("failed to write framed payload")?;
    socket
        .write_all(&sig.to_bytes())
        .await
        .context("failed to write signature")?;

    Ok(())
}
