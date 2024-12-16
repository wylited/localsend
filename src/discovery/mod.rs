use std::net::SocketAddr;

use crate::{models::device::DeviceInfo, Client};

pub mod http;
pub mod multicast;

impl Client {
    pub async fn announce(&self, socket: Option<SocketAddr>) -> crate::error::Result<()> {
        self.announce_http(socket).await?;
        self.announce_multicast().await?;
        Ok(())
    }

    async fn process_device(&self, message: &str, src: SocketAddr ) {
        if let Ok(device) = serde_json::from_str::<DeviceInfo>(message) {
            if device.fingerprint == self.device.fingerprint {
                return;
            }

            let mut peers = self.peers.lock().await;
            peers.insert(device.fingerprint.clone(), device.clone());

            if device.announce != Some(true) {
                return;
            }

            // Announce in return upon receiving a valid device message and it wants announcements
            if let Err(e) = self.announce_multicast().await {
                eprintln!("Error during multicast announcement: {}", e);
            }
            if let Err(e) = self.announce_http(Some(src)).await {
                eprintln!("Error during HTTP announcement: {}", e);
            };
        } else {
            eprintln!("Received invalid message: {}", message);
        }
    }
}
