use anyhow::Result;
use devicectrl_common::DeviceId;
use p256::{
    ecdsa::{SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};
use serde::{Deserialize, de};
use serde_derive::Deserialize;
use std::{net::SocketAddr, path::Path};
use tokio::fs;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub device_id: DeviceId,
    pub server_addr: SocketAddr,
    #[serde(
        rename = "server_public_key_path",
        deserialize_with = "deserialize_verifying_key"
    )]
    pub server_public_key: VerifyingKey,
    #[serde(
        rename = "private_key_path",
        deserialize_with = "deserialize_signing_key"
    )]
    pub private_key: SigningKey,
    pub hci_device: u16,
}

fn deserialize_verifying_key<'de, D>(deserializer: D) -> Result<VerifyingKey, D::Error>
where
    D: de::Deserializer<'de>,
{
    let der_bytes = std::fs::read(String::deserialize(deserializer)?).map_err(de::Error::custom)?;
    VerifyingKey::from_public_key_der(&der_bytes).map_err(de::Error::custom)
}

pub fn deserialize_signing_key<'de, D>(deserializer: D) -> Result<SigningKey, D::Error>
where
    D: de::Deserializer<'de>,
{
    let der_bytes = std::fs::read(String::deserialize(deserializer)?).map_err(de::Error::custom)?;
    SigningKey::from_pkcs8_der(&der_bytes).map_err(de::Error::custom)
}

pub async fn load_config(path: &Path) -> Result<Config> {
    Ok(serde_json::from_slice(&fs::read(path).await?)?)
}
