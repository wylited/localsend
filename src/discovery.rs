use std::{
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{ConnectInfo, State},
    Json,
};
use log::{debug, error, trace, warn};
use tokio::net::UdpSocket;

use crate::{models::Device, Config, LocalService, RunningState};

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

        let mut timeout = tokio::time::interval(Duration::from_secs(5));
        timeout.tick().await;

        loop {
            tokio::select! {
                _ = timeout.tick() => {
                    let rstate = {
                        *self.running_state.lock().await
                    };
                    if rstate == RunningState::Stopping
                    {
                        debug!("stopping multicast listen");
                        break;
                    }
                },
                r = self.socket.recv_from(&mut buf) => {
                    trace!("received multicast datagram");
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
            if device.fingerprint == self.config.device.fingerprint {
                return;
            }

            let mut src = src;
            src.set_port(device.port); // Update the port to the one the device sent

            {
                let mut peers = self.peers.lock().await;
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
) -> Json<Device> {
    let mut addr = addr;
    addr.set_port(service.config.device.port);
    service
        .peers
        .lock()
        .await
        .insert(device.fingerprint.clone(), (addr, device.clone()));
    Json(device)
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

/*
async fn announce_unicast(
    device: &Device,
    ip: Option<SocketAddr>,
    client: reqwest::Client,
) -> crate::error::Result<()> {
    // for enumerating subnet peers when multicast fails (https://github.com/localsend/protocol?tab=readme-ov-file#32-http-legacy-mode)
    let std::net::IpAddr::V4(ip) = local_ip_address::local_ip()? else {
        unreachable!()
    };

    let mut _network_ip = ip;
    let nifs = NetworkInterface::show()?;
    for addr in nifs.into_iter().flat_map(|i| i.addr) {
        if let Addr::V4(V4IfAddr {
            ip: ifip,
            netmask: Some(netmask),
            ..
        }) = addr
            && ip == ifip
        {
            _network_ip = ip & netmask;
            break;
        }
    }

    todo!()
}
*/
