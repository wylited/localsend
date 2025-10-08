use std::{
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    response::IntoResponse,
};
use log::{debug, error, trace, warn};
use reqwest::StatusCode;
use tokio::net::UdpSocket;

use crate::{Config, DEFAULT_INTERVAL, LocalService, RunningState, models::Device};

impl LocalService {
    pub async fn announce(&self, socket: Option<SocketAddr>) -> crate::error::Result<()> {
        trace!("announcing");
        announce_http(&self.config.device, socket, self.client.clone()).await?;
        announce_multicast(
            &self.config.device,
            self.config.multicast_addr,
            self.socket.clone(),
        )
        .await?;
        Ok(())
    }

    pub async fn listen_multicast(&self) -> crate::error::Result<()> {
        let mut buf = [0; 65536];

        let mut timeout = tokio::time::interval(DEFAULT_INTERVAL);
        timeout.tick().await;

        loop {
            tokio::select! {
                _ = timeout.tick() => {
                    let rstate = {
                        *self.running_state.read().await
                    };
                    if rstate == RunningState::Stopping
                    {
                        debug!("stopping multicast listen");
                        break;
                    }
                },
                r = self.socket.recv_from(&mut buf) => {
                    match r {
                        Ok((size, src)) => {
                            let received_msg = String::from_utf8_lossy(&buf[..size]);
                            self.process_device(&received_msg, src, &self.config).await;
                        }
                        Err(e) => {
                            error!("Error receiving message: {e}");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn process_device(&self, message: &str, src: SocketAddr, config: &Config) {
        if let Ok(device) = serde_json::from_str::<Device>(message) {
            if device == self.config.device {
                return;
            }

            let mut src = src;
            src.set_port(device.port); // Update the port to the one the device sent

            {
                let mut peers = self.peers.write().await;
                peers.insert(device.fingerprint.clone(), (src, device.clone()));
            }

            if device.announce != Some(true) {
                return;
            }

            // Announce in return upon receiving a valid device message and it wants
            // announcements
            if let Err(e) =
                announce_multicast(&device, config.multicast_addr, self.socket.clone()).await
            {
                warn!("Error during multicast announcement: {e}");
            }
            if let Err(e) = announce_http(&device, Some(src), self.client.clone()).await {
                warn!("Error during HTTP announcement: {e}");
            };
        } else {
            error!("Received invalid message: {message}");
        }
    }
}

/// Axum request handler for receiving other devices' registration requests.
pub async fn register_device(
    State(service): State<LocalService>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(device): Json<Device>,
) -> impl IntoResponse {
    if device == service.config.device {
        return StatusCode::ALREADY_REPORTED.into_response();
    }
    let mut addr = addr;
    addr.set_port(service.config.device.port);
    service
        .peers
        .write()
        .await
        .insert(device.fingerprint.clone(), (addr, device.clone()));
    Json(device).into_response()
}

//-************************************************************************
// private helpers
//-************************************************************************
async fn announce_http(
    device: &Device,
    ip: Option<SocketAddr>,
    client: reqwest::Client,
) -> crate::error::Result<()> {
    if let Some(ip) = ip {
        let url = format!("https://{ip}/api/localsend/v2/register");
        client.post(&url).json(device).send().await?;
    }
    Ok(())
}

async fn announce_multicast(
    device: &Device,
    addr: SocketAddrV4,
    socket: Arc<UdpSocket>,
) -> crate::error::Result<()> {
    let msg = device.to_json()?;
    socket.send_to(msg.as_bytes(), addr).await?;
    Ok(())
}
