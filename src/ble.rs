use anyhow::Result;
use hciraw::HciSocket;
use std::time::Duration;
use tokio::time::sleep;

use crate::fan::WrappedPacket;

const HCI_COMMAND_PKT: u8 = 0x01;
const OGF_LE_CTL: u16 = 0x08;

const OCF_LE_SET_ADVERTISING_PARAMETERS: u16 = 0x06;
const OCF_LE_SET_ADVERTISING_DATA: u16 = 0x08;
const OCF_LE_SET_ADVERTISE_ENABLE: u16 = 0x0A;

fn create_hci_command(cmd_code: u16, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(data.len() + 4);

    buf.push(HCI_COMMAND_PKT);
    buf.extend((cmd_code + (OGF_LE_CTL << 10)).to_ne_bytes());
    buf.push(data.len() as u8);
    buf.extend_from_slice(data);

    buf
}

fn generate_advertising_params() -> [u8; 15] {
    [32, 0, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x7, 0]
}

// this whole thing sucks because it requires commands to be processed serially
// and can clog up the socket if commands are sent quickly
pub async fn advertise_ble_message(hci_socket: &HciSocket, data: &WrappedPacket) -> Result<()> {
    let mut buf: Vec<u8> = Vec::from(&data.0);
    buf.insert(0, data.0.len() as u8);

    hci_socket.send(&create_hci_command(OCF_LE_SET_ADVERTISE_ENABLE, &[0]))?;

    hci_socket.send(&create_hci_command(
        OCF_LE_SET_ADVERTISING_PARAMETERS,
        &generate_advertising_params(),
    ))?;

    hci_socket.send(&create_hci_command(OCF_LE_SET_ADVERTISING_DATA, &buf))?;

    hci_socket.send(&create_hci_command(OCF_LE_SET_ADVERTISE_ENABLE, &[1]))?;

    sleep(Duration::from_millis(500)).await;

    hci_socket.send(&create_hci_command(OCF_LE_SET_ADVERTISE_ENABLE, &[0]))?;

    Ok(())
}
