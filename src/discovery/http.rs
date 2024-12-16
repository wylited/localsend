use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;

use axum::{extract::State, Extension, Json};

use crate::{models::device::DeviceInfo, Client};

impl Client {
    pub async fn announce_http(&self, ip: Option<SocketAddr>) -> crate::error::Result<()> {
        if let Some(ip) = ip {
            let url = format!("http://{}/api/localsend/v2/register", ip);
            let client = reqwest::Client::new();
            client.post(&url).json(&self.device).send().await?;
        }
        Ok(())
    }

    pub async fn announce_http_legacy(&self) -> crate::error::Result<()> {
        // send the reqwest to all local ip addresses from 192.168.0.0 to 192.168.255.255
        let mut address_list = Vec::new();
        for j in 0..256 {
            for k in 0..256 {
                address_list.push(format!("192.168.{:03}.{}", j, k));
            }
        }

        for ip in address_list {
            let url = format!("http://{}/api/localsend/v2/register", ip);
            self.http_client.post(&url).json(&self.device).send().await?;
        }
        Ok(())
    }
}

pub async fn register_device(
    State(peers): State<Arc<Mutex<HashMap<String, DeviceInfo>>>>,
    Extension(client): Extension<DeviceInfo>,
    Json(device): Json<DeviceInfo>,
) -> Json<DeviceInfo> {
    peers.lock().await.insert(device.fingerprint.clone(), device.clone());
    Json(client)
}
