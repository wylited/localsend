use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

use axum::{
    Json,
    body::Bytes,
    extract::{ConnectInfo, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use julid::Julid;
use log::{debug, error, info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::{
    LocalEvent, LocalService, Peers, ReceiveDialog, ReceiveRequest, SendingType, Sessions,
    error::{LocalSendError, Result},
    models::{Device, FileMetadata},
};

#[derive(Deserialize, Serialize, Clone)]
pub struct Session {
    pub session_id: String,
    pub files: BTreeMap<String, FileMetadata>,
    pub file_tokens: BTreeMap<String, String>,
    pub receiver: Device,
    pub sender: Device,
    pub status: SessionStatus,
    pub addr: SocketAddr,
}

#[derive(PartialEq, Deserialize, Serialize, Clone, Copy)]
pub enum SessionStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadRequest {
    pub info: Device,
    pub files: BTreeMap<String, FileMetadata>,
}

impl LocalService {
    pub async fn send_file(&self, peer: &str, file_path: PathBuf) -> Result<()> {
        let content = SendingType::File(file_path);
        self.send_content(peer, content).await
    }

    pub async fn send_text(&self, peer: &str, text: &str) -> Result<()> {
        let content = SendingType::Text(text.to_owned());
        self.send_content(peer, content).await
    }

    pub async fn cancel_upload(&self, session_id: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or(LocalSendError::SessionNotFound)?;

        let request = self
            .client
            .post(format!(
                "{}://{}/api/localsend/v2/cancel?sessionId={session_id}",
                session.receiver.protocol, session.addr
            ))
            .send()
            .await?;

        if request.status() != 200 {
            return Err(LocalSendError::CancelFailed);
        }

        Ok(())
    }

    // spawns a tokio task to wait for responses
    async fn send_content(&self, peer: &str, content: SendingType) -> Result<()> {
        let (metadata, bytes) = match content {
            SendingType::File(path) => {
                let contents = tokio::fs::read(&path).await?;
                let bytes = Bytes::from(contents);
                (FileMetadata::from_path(&path)?, bytes)
            }
            SendingType::Text(text) => (FileMetadata::from_text(&text)?, Bytes::from(text)),
        };

        let mut files = BTreeMap::new();
        files.insert(metadata.id.clone(), metadata.clone());

        let ourself = self.config.device.clone();
        let client = self.client.clone();
        let tx = self.transfer_event_tx.clone();
        let peer = peer.to_string();
        let sessions = self.sessions.clone();
        let peers = self.peers.clone();

        tokio::task::spawn(async move {
            fn send_tx(msg: LocalEvent, tx: &UnboundedSender<LocalEvent>) {
                if let Err(e) = tx.send(msg.clone()) {
                    log::error!("got error sending {msg:?} to frontend: {e:?}");
                }
            }

            let prepare_response =
                do_prepare_upload(ourself, &client, &peer, &peers, &sessions, files).await;

            let prepare_response = match prepare_response {
                Ok(r) => r,
                Err(e) => {
                    log::debug!("got error from remote receiver: {e:?}");
                    send_tx(LocalEvent::SendDenied, &tx);
                    return;
                }
            };

            send_tx(LocalEvent::SendApproved(metadata.id.clone()), &tx);

            let token = match prepare_response.files.get(&metadata.id) {
                Some(t) => t,
                None => {
                    send_tx(
                        LocalEvent::SendFailed {
                            error: "missing token in prepare response from remote".into(),
                        },
                        &tx,
                    );
                    return;
                }
            };

            let content_id = &metadata.id;
            let session_id = prepare_response.session_id;
            log::info!(
                "sending {content_id} to {}",
                peers
                    .read()
                    .await
                    .get(&peer)
                    .map(|(_, peer)| peer.alias.as_str())
                    .unwrap_or("unknown peer")
            );
            let resp = do_send_bytes(sessions, client, &session_id, content_id, token, bytes).await;

            match resp {
                Ok(_) => {
                    send_tx(
                        LocalEvent::SendSuccess {
                            content: content_id.to_owned(),
                            session: session_id,
                        },
                        &tx,
                    );
                }
                Err(e) => {
                    send_tx(
                        LocalEvent::SendFailed {
                            error: format!("{e:?}"),
                        },
                        &tx,
                    );
                }
            }
        });
        Ok(())
    }
}

pub async fn handle_prepare_upload(
    State(service): State<LocalService>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<PrepareUploadRequest>,
) -> impl IntoResponse {
    info!(
        "Received upload request from {} at {addr:?}",
        req.info.alias
    );

    let id = Julid::new();
    let (tx, mut rx) = unbounded_channel();
    let request = ReceiveRequest {
        alias: req.info.alias.clone(),
        files: req.files.clone(),
        tx,
    };

    match service
        .transfer_event_tx
        .send(LocalEvent::ReceiveRequest { id, request })
    {
        Ok(_) => {}
        Err(e) => {
            error!("error sending transfer event to app: {e:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let Some(confirmation) = rx.recv().await else {
        // the frontend must have dropped the tx before trying to send a reply back
        warn!("could not read content receive response from the frontend");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    if confirmation != ReceiveDialog::Approve {
        return StatusCode::FORBIDDEN.into_response();
    }

    let session_id = id.as_string();

    let file_tokens: BTreeMap<String, String> = req
        .files
        .keys()
        .map(|id| (id.clone(), Julid::new().to_string())) // Replace with actual token logic
        .collect();

    let session = Session {
        session_id: session_id.clone(),
        files: req.files.clone(),
        file_tokens: file_tokens.clone(),
        receiver: service.config.device.clone(),
        sender: req.info.clone(),
        status: SessionStatus::Active,
        addr,
    };

    service
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);

    (
        StatusCode::OK,
        Json(PrepareUploadResponse {
            session_id,
            files: file_tokens,
        }),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadParams {
    session_id: String,
    file_id: String,
    token: String,
}

pub async fn handle_receive_upload(
    Query(params): Query<UploadParams>,
    State(service): State<LocalService>,
    body: Bytes,
) -> impl IntoResponse {
    // Extract query parameters
    let session_id = &params.session_id;
    let file_id = &params.file_id;
    let token = &params.token;

    // Get session and validate

    let session = {
        let lock = service.sessions.read().await;
        match lock.get(session_id).cloned() {
            Some(session) => session,
            None => return StatusCode::BAD_REQUEST.into_response(),
        }
    };

    if session.status != SessionStatus::Active {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // Validate token
    if session.file_tokens.get(file_id) != Some(&token.to_string()) {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Get file metadata
    let file_metadata = match session.files.get(file_id) {
        Some(metadata) => metadata,
        None => {
            return (StatusCode::BAD_REQUEST, "File not found".to_string()).into_response();
        }
    };

    let download_dir = &service.config.download_dir;

    // Create directory if it doesn't exist
    if let Err(e) = tokio::fs::create_dir_all(download_dir).await {
        log::error!("could not create download directory '{download_dir:?}', got {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not save content").into_response();
    }

    // Create file path
    let file_path = service.config.download_dir.join(&file_metadata.file_name);

    // Write file
    if let Err(e) = tokio::fs::write(&file_path, body).await {
        log::warn!("could not save content to {file_path:?}, got {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not save content").into_response();
    }

    log::info!(
        "saved content from {} to {file_path:?}",
        &session.sender.alias
    );
    if let Ok(id) = Julid::from_str(session_id) {
        service.send_event(LocalEvent::ReceivedInbound(id));
    };

    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams {
    session_id: String,
}

pub async fn handle_cancel(
    Query(params): Query<CancelParams>,
    State(service): State<LocalService>,
) -> impl IntoResponse {
    // it's OK to hold this lock for the whole body here, we hardly ever have to
    // handle cancels and none of these ops are slow.
    let mut sessions_lock = service.sessions.write().await;
    let session = match sessions_lock.get_mut(&params.session_id) {
        Some(session) => session,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    info!(
        "{} cancelled the transfer of {}",
        &session.sender.alias,
        session
            .files
            .values()
            .map(|f| f.file_name.clone())
            .collect::<Vec<_>>()
            .join(",")
    );

    session.status = SessionStatus::Cancelled;

    if let Ok(id) = Julid::from_str(&params.session_id) {
        service.send_event(LocalEvent::Cancelled { session_id: id });
        StatusCode::OK.into_response()
    } else {
        StatusCode::BAD_REQUEST.into_response()
    }
}

// free function that can be called inside a future in tokio::task::spawn()
async fn do_send_bytes(
    sessions: Sessions,
    client: Client,
    session_id: &str,
    content_id: &str,
    token: &str,
    body: Bytes,
) -> Result<()> {
    let session = sessions
        .read()
        .await
        .get(session_id)
        .cloned()
        .ok_or(LocalSendError::SessionNotFound)?;

    if session.status != SessionStatus::Active {
        return Err(LocalSendError::SessionInactive);
    }

    if session.file_tokens.get(content_id).map(|t| t.as_str()) != Some(token) {
        return Err(LocalSendError::InvalidToken);
    }

    let request = client
                          .post(format!(
                              "{}://{}/api/localsend/v2/upload?sessionId={session_id}&fileId={content_id}&token={token}",
                              session.receiver.protocol, session.addr))
                          .body(body);

    debug!("Uploading bytes: {request:?}");
    let response = request.send().await?;

    if response.status() != 200 {
        log::warn!("non-200 remote response: {response:?}");
        Err(LocalSendError::UploadFailed)
    } else {
        Ok(())
    }
}

// free function that can be called inside a future in tokio::task::spawn()
async fn do_prepare_upload(
    ourself: Device,
    client: &reqwest::Client,
    peer: &str,
    peers: &Peers,
    sessions: &Sessions,
    files: BTreeMap<String, FileMetadata>,
) -> Result<PrepareUploadResponse> {
    let Some((addr, device)) = peers.read().await.get(peer).cloned() else {
        return Err(LocalSendError::PeerNotFound);
    };

    log::debug!("preparing upload request");

    let request = client
        .post(format!(
            "{}://{}/api/localsend/v2/prepare-upload",
            device.protocol, addr
        ))
        .json(&PrepareUploadRequest {
            info: ourself.clone(),
            files: files.clone(),
        })
        .timeout(Duration::from_secs(30));

    debug!("sending '{request:?}' to peer at {addr:?}");

    // tokio::spawn(future);
    let response = request.send().await?;

    debug!("Response: {response:?}");

    let response: PrepareUploadResponse = match response.json().await {
        Err(e) => {
            error!("got error deserializing response: {e:?}");
            return Err(LocalSendError::RequestError(e));
        }
        Ok(r) => r,
    };

    debug!("decoded response: {response:?}");

    let session = Session {
        session_id: response.session_id.clone(),
        files,
        file_tokens: response.files.clone(),
        receiver: device,
        sender: ourself.clone(),
        status: SessionStatus::Active,
        addr,
    };

    sessions
        .write()
        .await
        .insert(response.session_id.clone(), session);

    Ok(response)
}
