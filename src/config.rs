use std::{
    net::{IpAddr, Ipv4Addr, SocketAddrV4},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use local_ip_address::local_ip;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    error::{LocalSendError, Result},
    models::Device,
    DEFAULT_PORT, MULTICAST_IP,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub multicast_addr: SocketAddrV4,
    pub download_dir: PathBuf,
    pub data_dir: PathBuf,
    #[serde(deserialize_with = "deserialize_local_addr")]
    pub local_ip_addr: Ipv4Addr,
    pub device: Device,
}

// we ask the OS for our IP every time at start, we don't actually store a real one.
fn deserialize_local_addr<'de, D: Deserializer<'de>>(
    _: D,
) -> std::result::Result<Ipv4Addr, D::Error> {
    Ok(get_local_ip_addr())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            multicast_addr: SocketAddrV4::new(MULTICAST_IP, DEFAULT_PORT),
            local_ip_addr: get_local_ip_addr(),
            download_dir: Default::default(),
            data_dir: Default::default(),
            device: Default::default(),
        }
    }
}

impl Config {
    pub fn new() -> Result<Self> {
        let dirs = directories::BaseDirs::new().ok_or(LocalSendError::NoHomeDir)?;

        let download_dir = dirs.home_dir().join("localsend-downloads");
        let config_file = dirs.config_dir().join("localsend.toml");
        let data_dir = dirs.data_local_dir().join("localsend");

        let key = data_dir.join("key.pem");
        let cert = data_dir.join("cert.pem");
        let fingerprint = if data_dir.exists() {
            if !(key.exists() && cert.exists()) {
                gen_ssl(&key, &cert)?
            } else {
                let key = std::fs::read(key)?;
                sha256::digest(key)
            }
        } else {
            std::fs::create_dir_all(data_dir.as_path())?;
            gen_ssl(&key, &cert)?
        };

        let config = Self {
            multicast_addr: SocketAddrV4::new(MULTICAST_IP, DEFAULT_PORT),
            local_ip_addr: get_local_ip_addr(),
            download_dir,
            data_dir,
            device: Device {
                alias: rustix::system::uname()
                    .nodename()
                    .to_string_lossy()
                    .to_string(),
                fingerprint,
                ..Default::default()
            },
        };

        let config = if !config_file.exists() {
            log::info!("creating config file at {config_file:?}");
            std::fs::write(&config_file, toml::to_string(&config)?)?;
            config
        } else {
            log::info!("reading config from {config_file:?}");
            Figment::from(Serialized::defaults(config))
                .merge(Toml::file(config_file))
                .extract()
                .map_err(Box::new)? // boxed because the error size from figment is large
        };

        log::info!("using config: {config:?}");

        Ok(config)
    }

    /// Returns (key, cert) paths
    pub fn ssl(&self) -> (PathBuf, PathBuf) {
        let key = self.data_dir.join("key.pem");
        let cert = self.data_dir.join("cert.pem");
        (key, cert)
    }
}

fn gen_ssl(key: &Path, cert: &Path) -> Result<String> {
    let cert_key = rcgen::generate_simple_self_signed(vec!["*".into()])?;
    let cert_text = cert_key.cert.pem();
    let key_text = cert_key.signing_key.serialize_pem();
    std::fs::write(key, key_text.clone())?;
    std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o400u32))?;
    std::fs::write(cert, cert_text)?;
    Ok(sha256::digest(key_text))
}

fn get_local_ip_addr() -> Ipv4Addr {
    let IpAddr::V4(local_ip_addr) = local_ip().unwrap_or(IpAddr::V4(Ipv4Addr::from_bits(0))) else {
        panic!("IPv6 is unsupported");
    };
    local_ip_addr
}
