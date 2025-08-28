pub mod config;
pub mod discovery;
pub mod error;
pub mod http_server;
pub mod models;
pub mod transfer;

use std::{
    collections::BTreeMap,
    fmt::Debug,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, OnceLock},
};

pub use config::Config;
use julid::Julid;
use log::error;
use models::{Device, FileMetadata};
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{self, UnboundedSender},
        Mutex,
    },
    task::JoinSet,
};
use transfer::Session;

pub const DEFAULT_PORT: u16 = 53317;
pub const MULTICAST_IP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const LISTENING_SOCKET_ADDR: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::from_bits(0), DEFAULT_PORT);

pub type ShutdownSender = mpsc::Sender<()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listeners {
    Udp,
    Http,
    Multicast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveDialog {
    Approve,
    Deny,
}

#[derive(Debug, Clone)]
pub enum TransferEvent {
    Received(Julid),
    Cancelled(Julid),
    ReceiveRequest { id: Julid, request: ReceiveRequest },
}

#[derive(Clone)]
pub struct ReceiveRequest {
    pub alias: String,
    pub files: BTreeMap<String, FileMetadata>,
    pub tx: UnboundedSender<ReceiveDialog>,
}

impl Debug for ReceiveRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiveRequest")
            .field("alias", &self.alias)
            .field("files", &self.files)
            .finish()
    }
}

/// Contains the main network and backend state for an application session.
#[derive(Clone)]
pub struct LocalService {
    pub peers: Arc<Mutex<BTreeMap<String, (SocketAddr, Device)>>>,
    pub sessions: Arc<Mutex<BTreeMap<String, Session>>>, // Session ID to Session
    pub running_state: Arc<Mutex<RunningState>>,
    pub socket: Arc<UdpSocket>,
    pub client: reqwest::Client,
    pub config: Config,
    shutdown_sender: OnceLock<ShutdownSender>,
    // the receiving end will be held by the application so it can update the UI based on backend
    // events
    transfer_event_tx: UnboundedSender<TransferEvent>,
}

impl LocalService {
    pub async fn new(
        config: Config,
        transfer_event_tx: UnboundedSender<TransferEvent>,
    ) -> crate::error::Result<Self> {
        let socket = UdpSocket::bind(LISTENING_SOCKET_ADDR).await?;
        socket.set_multicast_loop_v4(true)?;
        socket.set_multicast_ttl_v4(8)?; // 8 hops out from localnet
        socket.join_multicast_v4(MULTICAST_IP, Ipv4Addr::from_bits(0))?;

        let client = reqwest::ClientBuilder::new()
            // localsend certs are self-signed
            .danger_accept_invalid_certs(true)
            .build()?;

        Ok(Self {
            config,
            client,
            socket: socket.into(),
            transfer_event_tx,
            peers: Default::default(),
            sessions: Default::default(),
            running_state: Default::default(),
            shutdown_sender: Default::default(),
        })
    }

    pub async fn start(&self, handles: &mut JoinSet<Listeners>) {
        let service = self.clone();

        handles.spawn({
            let (tx, shutdown_rx) = mpsc::channel(1);
            let _ = self.shutdown_sender.set(tx);
            async move {
                if let Err(e) = service.start_http_server(shutdown_rx).await {
                    error!("HTTP server error: {e}");
                }
                Listeners::Http
            }
        });
        let service = self.clone();

        handles.spawn({
            async move {
                if let Err(e) = service.listen_multicast().await {
                    error!("UDP listener error: {e}");
                }
                Listeners::Multicast
            }
        });

        let service = self.clone();
        handles.spawn({
            async move {
                loop {
                    let rstate = service.running_state.lock().await;
                    if *rstate == RunningState::Stopping {
                        break;
                    }
                    if let Err(e) = service.announce(None).await {
                        error!("Announcement error: {e}");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                Listeners::Udp
            }
        });
    }

    pub async fn stop(&self) {
        let mut rstate = self.running_state.lock().await;
        *rstate = RunningState::Stopping;
        let _ = self
            .shutdown_sender
            .get()
            .expect("Could not get stop signal transmitter")
            .send(())
            .await;
    }

    pub async fn refresh_peers(&self) {
        let mut peers = self.peers.lock().await;
        peers.clear();
    }

    pub fn send_event(&self, event: TransferEvent) {
        if let Err(e) = self.transfer_event_tx.send(event.clone()) {
            error!("got error sending transfer event '{event:?}': {e:?}");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningState {
    Running,
    Stopping,
}

impl Default for RunningState {
    fn default() -> Self {
        Self::Running
    }
}
