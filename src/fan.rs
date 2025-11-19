use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use anyhow::{Result, bail};
use crc::{CRC_16_XMODEM, Crc};
use devicectrl_common::{
    device_types::{NumericProperties, ceiling_fan::FanDirection},
    updates::AttributeUpdate,
};
use hciraw::HciSocket;

use crate::ble::advertise_ble_message;

const BRIGHTNESS_PROPS: NumericProperties = NumericProperties {
    min: 0,
    max: 255,
    step: 1,
};

const COLOR_TEMP_PROPS: NumericProperties = NumericProperties {
    min: 0,
    max: 255,
    step: 1,
};

const SPEED_PROPS: NumericProperties = NumericProperties {
    min: 0,
    max: 6,
    step: 1,
};

// Values and algorithms derived from https://github.com/NicoIIT/ha-ble-adv

const PACKET_LEN: usize = 19;
pub const ENCRYPTED_PACKET_LEN: usize = PACKET_LEN + 5 + FRAME_HEADER.len();

const PACKET_HEADER: [u8; 3] = [0x20, 0x82, 0x00];
const FRAME_HEADER: [u8; 2] = [0xF0, 0x08];

const XOR_LUT: [u8; 128] = [
    0xB7, 0xFD, 0x93, 0x26, 0x36, 0x3F, 0xF7, 0xCC, 0x34, 0xA5, 0xE5, 0xF1, 0x71, 0xD8, 0x31, 0x15,
    0x04, 0xC7, 0x23, 0xC3, 0x18, 0x96, 0x05, 0x9A, 0x07, 0x12, 0x80, 0xE2, 0xEB, 0x27, 0xB2, 0x75,
    0xD0, 0xEF, 0xAA, 0xFB, 0x43, 0x4D, 0x33, 0x85, 0x45, 0xF9, 0x02, 0x7F, 0x50, 0x3C, 0x9F, 0xA8,
    0x51, 0xA3, 0x40, 0x8F, 0x92, 0x9D, 0x38, 0xF5, 0xBC, 0xB6, 0xDA, 0x21, 0x10, 0xFF, 0xF3, 0xD2,
    0xE0, 0x32, 0x3A, 0x0A, 0x49, 0x06, 0x24, 0x5C, 0xC2, 0xD3, 0xAC, 0x62, 0x91, 0x95, 0xE4, 0x79,
    0xE7, 0xC8, 0x37, 0x6D, 0x8D, 0xD5, 0x4E, 0xA9, 0x6C, 0x56, 0xF4, 0xEA, 0x65, 0x7A, 0xAE, 0x08,
    0xE1, 0xF8, 0x98, 0x11, 0x69, 0xD9, 0x8E, 0x94, 0x9B, 0x1E, 0x87, 0xE9, 0xCE, 0x55, 0x28, 0xDF,
    0x8C, 0xA1, 0x89, 0x0D, 0xBF, 0xE6, 0x42, 0x68, 0x41, 0x99, 0x2D, 0x0F, 0xB0, 0x54, 0xBB, 0x16,
];

const SEED: u16 = 0x2B53;
const INDEX: u8 = 0;
const DEVICE_TYPE: u16 = 1024;

// Because the fan uses the same command for brightness and color temperature,
// we need to cache the state of the fan to remember the last brightness and temperature
// values, so we can send the correct command when only one of them changes.
// TODO: maybe use Options to represent the initial unknown state?
#[derive(Debug)]
pub struct CachedFanState {
    pub tx_count: u8,
    pub power: bool,
    pub color_temp: u8,
    pub brightness: u8,
    pub speed: u8,

    pub remote_uid: u32, // not actually fan state, but convenient to store here
}

#[repr(u8)]
enum Cmd {
    Direction = 0x15,
    FanSpeed = 0x31,
    LightOn = 0x10,
    LightOff = 0x11,
    LightBrightnessTemperature = 0x21,
    Pair = 0x28,
}

#[derive(Debug)]
struct SerializedPacket(pub [u8; PACKET_LEN]);

#[derive(Debug)]
pub struct EncryptedPacket(pub [u8; ENCRYPTED_PACKET_LEN]);

#[derive(Debug)]
pub struct WrappedPacket(pub [u8; ENCRYPTED_PACKET_LEN + 5]);

pub fn wrap_packet(packet: &EncryptedPacket) -> WrappedPacket {
    let mut buf = [0u8; size_of::<WrappedPacket>()];

    buf[0..5].copy_from_slice(&[0x02, 0x01, 0x19, ENCRYPTED_PACKET_LEN as u8 + 1, 0x03]);
    buf[5..].copy_from_slice(&packet.0);

    WrappedPacket(buf)
}

#[derive(Debug)]
struct PacketData {
    // PACKET_HEADER here
    tx_count: u8,
    device_type: u16,
    uid: u32,
    index: u8,
    cmd: u8,
    // 2 zero bytes here
    arg0: u8,
    arg1: u8,
    arg2: u8,
    seed: u16,
}

impl PacketData {
    fn from_command(update: &AttributeUpdate, fan_state: &mut CachedFanState) -> Vec<Self> {
        let mut packets = Vec::new();

        if let AttributeUpdate::Brightness(brightness) = &update {
            let brightness =
                brightness.apply_to(&BRIGHTNESS_PROPS.to_state(fan_state.brightness as u32)) as u8;

            fan_state.brightness = brightness;

            // the fan has a power state, so we need to send a command to turn it on or off
            // because the api does not have a separate power state, it just has brightness
            if (brightness != 0) != fan_state.power {
                fan_state.power = brightness != 0;
                packets.push(Self::new(
                    fan_state.tx_count,
                    fan_state.remote_uid,
                    match brightness {
                        0 => Cmd::LightOff,
                        _ => Cmd::LightOn,
                    },
                    [0, 0, 0],
                ));

                fan_state.tx_count = fan_state.tx_count.wrapping_add(1);
            }
        }

        if let AttributeUpdate::ColorTemp(color_temp) = update {
            fan_state.color_temp =
                color_temp.apply_to(&COLOR_TEMP_PROPS.to_state(fan_state.color_temp as u32)) as u8;
        }

        if matches!(
            update,
            AttributeUpdate::Brightness(_) | AttributeUpdate::ColorTemp(_)
        ) {
            let brightness = fan_state.brightness as f32;
            let temperature = fan_state.color_temp as f32;

            packets.push(Self::new(
                fan_state.tx_count,
                fan_state.remote_uid,
                Cmd::LightBrightnessTemperature,
                [
                    0,
                    (brightness * ((255. - temperature).min(127.) / 127.)).ceil() as u8,
                    (brightness * temperature.min(127.) / 127.).ceil() as u8,
                ],
            ));
            fan_state.tx_count = fan_state.tx_count.wrapping_add(1);
        }

        if let AttributeUpdate::FanDirection(fan_direction) = &update {
            packets.push(Self::new(
                fan_state.tx_count,
                fan_state.remote_uid,
                Cmd::Direction,
                [
                    match fan_direction {
                        FanDirection::Forward => 0,
                        FanDirection::Reverse => 1,
                    },
                    0,
                    0,
                ],
            ));
            fan_state.tx_count = fan_state.tx_count.wrapping_add(1);
        }

        if let AttributeUpdate::FanSpeed(fan_speed) = &update {
            let fan_speed = fan_speed.apply_to(&SPEED_PROPS.to_state(fan_state.speed as u32)) as u8;

            packets.push(Self::new(
                fan_state.tx_count,
                fan_state.remote_uid,
                Cmd::FanSpeed,
                [32, fan_speed, 0],
            ));
            fan_state.tx_count = fan_state.tx_count.wrapping_add(1);
        }

        packets
    }
    fn new(tx_count: u8, uid: u32, cmd: Cmd, args: [u8; 3]) -> Self {
        Self {
            tx_count,
            device_type: DEVICE_TYPE,
            uid,
            index: INDEX,
            cmd: cmd as u8,
            arg0: args[0],
            arg1: args[1],
            arg2: args[2],
            seed: SEED,
        }
    }
    fn serialize(&self) -> SerializedPacket {
        let mut buf = [0u8; 19];

        buf[0..=2].copy_from_slice(&PACKET_HEADER);
        buf[3] = self.tx_count;
        buf[4..=5].copy_from_slice(&self.device_type.to_le_bytes());
        buf[6..=9].copy_from_slice(&self.uid.to_le_bytes());
        buf[10] = self.index;
        buf[11] = self.cmd;
        buf[14] = self.arg0;
        buf[15] = self.arg1;
        buf[16] = self.arg2;
        buf[17..=18].copy_from_slice(&self.seed.to_le_bytes());

        SerializedPacket(buf)
    }
    #[allow(dead_code)] // this function is just for testing
    fn deserialize(packet: &SerializedPacket) -> Result<Self> {
        let buf = packet.0;
        if buf[0..3] != PACKET_HEADER {
            bail!("Packet header does not match!");
        }

        Ok(Self {
            tx_count: buf[3],
            device_type: u16::from_le_bytes([buf[4], buf[5]]),
            uid: u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]),
            index: buf[10],
            cmd: buf[11],
            arg0: buf[14],
            arg1: buf[15],
            arg2: buf[16],
            seed: u16::from_le_bytes([buf[17], buf[18]]),
        })
    }
}

fn whiten<const N: usize>(buffer: &[u8; N], seed: u8) -> [u8; N] {
    let salt = (PACKET_HEADER[1] & 0x3) << 5;
    let mut result = [0u8; N];
    for (i, &val) in buffer.iter().enumerate() {
        let idx = ((seed as usize + i + 9) & 0x1F) + salt as usize;
        result[i] = XOR_LUT[idx] ^ seed ^ val;
    }
    result
}

fn sign(buffer: &[u8], tx_count: u8, seed: u16) -> u16 {
    let key = [
        (seed & 0xFF) as u8,
        (seed >> 8) as u8,
        tx_count,
        0x0D,
        0xBF,
        0xE6,
        0x42,
        0x68,
        0x41,
        0x99,
        0x2D,
        0x0F,
        0xB0,
        0x54,
        0xBB,
        0x16,
    ];

    let mut block = GenericArray::from([0u8; 16]);
    block.copy_from_slice(&buffer[0..16]);

    let cipher = Aes128::new((&key).into());
    cipher.encrypt_block(&mut block);

    let sign = u16::from_le_bytes([block[0], block[1]]);
    if sign != 0 { sign } else { 0xFFFF }
}

fn encrypt(decoded: &SerializedPacket) -> EncryptedPacket {
    let buf = decoded.0;
    let seed = u16::from_le_bytes([buf[PACKET_LEN - 2], buf[PACKET_LEN - 1]]);

    let mut msg_buf = [0u8; PACKET_LEN + 1];
    msg_buf[..(PACKET_LEN - 2)].copy_from_slice(&buf[..(PACKET_LEN - 2)]);

    let sign = sign(&msg_buf[1..17], msg_buf[3], seed);
    msg_buf[PACKET_LEN - 2..PACKET_LEN].copy_from_slice(&sign.to_le_bytes());
    msg_buf[PACKET_LEN] = 0;

    let mut result = [0u8; ENCRYPTED_PACKET_LEN];
    result[..2].copy_from_slice(&FRAME_HEADER);

    result[2..4].copy_from_slice(&msg_buf[..2]);
    let whitened = whiten::<{ PACKET_LEN - 1 }>(&msg_buf[2..].try_into().unwrap(), seed as u8);
    result[4..PACKET_LEN + 3].copy_from_slice(&whitened);

    result[PACKET_LEN + 3..PACKET_LEN + 5].copy_from_slice(&seed.to_le_bytes());

    let crc = Crc::<u16>::new(&CRC_16_XMODEM);
    let mut digest = crc.digest_with_initial(!seed);
    digest.update(&result[FRAME_HEADER.len()..PACKET_LEN + 5]);

    result[PACKET_LEN + 5..PACKET_LEN + 7].copy_from_slice(&digest.finalize().to_le_bytes());

    EncryptedPacket(result)
}

pub async fn send_update_to_fan(
    update: AttributeUpdate,
    fan_state: &mut CachedFanState,
    hci_socket: &HciSocket,
) -> Result<()> {
    let packets = PacketData::from_command(&update, fan_state);

    for packet in packets {
        send_packet_to_fan(packet, hci_socket).await?;
    }

    Ok(())
}

pub async fn send_keepalive_to_fan(
    fan_state: &mut CachedFanState,
    hci_socket: &HciSocket,
) -> Result<()> {
    let packet = PacketData::new(
        fan_state.tx_count,
        fan_state.remote_uid,
        Cmd::Pair,
        [0, 0, 0],
    );
    fan_state.tx_count = fan_state.tx_count.wrapping_add(1);

    send_packet_to_fan(packet, hci_socket).await
}

async fn send_packet_to_fan(packet: PacketData, hci_socket: &HciSocket) -> Result<()> {
    log::debug!("sending packet: {packet:?}");

    let serialized = packet.serialize();
    let encrypted = encrypt(&serialized);
    let wrapped = wrap_packet(&encrypted);

    advertise_ble_message(hci_socket, &wrapped).await
}
