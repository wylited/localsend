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
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

pub use config::Config;
use julid::Julid;
use log::error;
use models::{Device, FileMetadata};
use tokio::{
    net::UdpSocket,
    sync::{
        RwLock,
        mpsc::{self, UnboundedReceiver, UnboundedSender},
    },
    task::JoinSet,
};
use transfer::Session;

pub const DEFAULT_PORT: u16 = 53317;
pub const MULTICAST_IP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(100);

pub type Peers = Arc<RwLock<BTreeMap<String, (SocketAddr, Device)>>>;
pub type Sessions = Arc<RwLock<BTreeMap<String, Session>>>; // Session ID to Session

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTask {
    Udp,
    Http,
    Multicast,
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveDialog {
    Approve,
    Deny,
}

#[derive(Debug)]
pub enum SendingType {
    File(PathBuf),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEvent {
    ReceivedInbound(Julid),
    SendApproved(String),
    SendDenied,
    SendSuccess { content: String, session: String },
    SendFailed { error: String },
    Cancelled { session_id: Julid },
    ReceiveRequest { id: Julid, request: ReceiveRequest },
    Tick,
}

#[derive(Clone)]
pub struct ReceiveRequest {
    pub alias: String,
    pub files: BTreeMap<String, FileMetadata>,
    pub tx: UnboundedSender<ReceiveDialog>,
}

impl PartialEq for ReceiveRequest {
    fn eq(&self, other: &Self) -> bool {
        self.alias == other.alias && self.files == other.files
    }
}

impl Eq for ReceiveRequest {}

impl Debug for ReceiveRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiveRequest")
            .field("alias", &self.alias)
            .field("files", &self.files)
            .finish()
    }
}

impl ReceiveRequest {
    pub fn file_names(&self) -> Vec<String> {
        self.files.values().map(|f| f.file_name.clone()).collect()
    }
}

/// Contains the main network and backend state for an application session.
#[derive(Clone)]
pub struct LocalService {
    pub peers: Peers,
    pub sessions: Sessions,
    pub running_state: Arc<RwLock<RunningState>>,
    pub socket: Arc<UdpSocket>,
    pub client: reqwest::Client,
    pub config: Config,
    pub http_handle: Arc<OnceLock<axum_server::Handle>>,
    // the receiving end will be held by the application so it can update the UI based on backend
    // events
    transfer_event_tx: UnboundedSender<LocalEvent>,
}

impl LocalService {
    pub async fn new(
        config: Config,
    ) -> crate::error::Result<(Self, UnboundedReceiver<LocalEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let addr = SocketAddrV4::new(config.local_ip_addr, DEFAULT_PORT);
        let socket = UdpSocket::bind(addr).await?;
        socket.set_multicast_loop_v4(false)?;
        socket.set_multicast_ttl_v4(1)?; // local subnet only
        socket.join_multicast_v4(MULTICAST_IP, *addr.ip())?;

        let client = reqwest::ClientBuilder::new()
            // localsend certs are self-signed
            .danger_accept_invalid_certs(true)
            .build()?;

        Ok((
            Self {
                config,
                client,
                socket: socket.into(),
                transfer_event_tx: tx,
                peers: Default::default(),
                sessions: Default::default(),
                running_state: Default::default(),
                http_handle: Default::default(),
            },
            rx,
        ))
    }

    pub async fn start(&self) -> JoinSet<LocalTask> {
        let service = self.clone();

        let mut handles = JoinSet::new();

        handles.spawn(async move {
            if let Err(e) = service.start_http_server().await {
                error!("HTTP server error: {e}");
            }
            LocalTask::Http
        });
        let service = self.clone();

        handles.spawn(async move {
            if let Err(e) = service.listen_multicast().await {
                error!("UDP listener error: {e}");
            }
            LocalTask::Multicast
        });

        let service = self.clone();
        handles.spawn(async move {
            let service = &service;
            let mut tick = tokio::time::interval(DEFAULT_INTERVAL);

            loop {
                tick.tick().await;
                service
                    .transfer_event_tx
                    .send(LocalEvent::Tick)
                    .unwrap_or_else(|e| log::warn!("could not send tick event: {e:?}"));

                let rstate = service.running_state.read().await;
                if *rstate == RunningState::Stopping {
                    break;
                }
            }
            LocalTask::Tick
        });

        let service = self.clone();
        handles.spawn(async move {
            loop {
                if let Err(e) = service.announce(None).await {
                    error!("Announcement error: {e}");
                }

                tokio::time::sleep(Duration::from_secs(2)).await;

                let rstate = service.running_state.read().await;
                if *rstate == RunningState::Stopping {
                    break;
                }
            }
            LocalTask::Udp
        });

        handles
    }

    pub async fn stop(&self) {
        {
            let mut rstate = self.running_state.write().await;
            *rstate = RunningState::Stopping;
        }
        log::info!("shutting down http server");
        self.http_handle
            .get()
            .expect("missing http handle for shutdown")
            .graceful_shutdown(Some(Duration::from_secs(5)));
    }

    pub async fn clear_peers(&self) {
        let mut peers = self.peers.write().await;
        peers.clear();
    }

    pub fn send_event(&self, event: LocalEvent) {
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
