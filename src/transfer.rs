use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use julid::Julid;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::unbounded_channel;

use crate::{
    error::{LocalSendError, Result},
    models::{Device, FileMetadata},
    LocalService, ReceiveDialog, ReceiveRequest, TransferEvent,
};

#[derive(Deserialize, Serialize)]
pub struct Session {
    pub session_id: String,
    pub files: BTreeMap<String, FileMetadata>,
    pub file_tokens: BTreeMap<String, String>,
    pub receiver: Device,
    pub sender: Device,
    pub status: SessionStatus,
    pub addr: SocketAddr,
}

#[derive(PartialEq, Deserialize, Serialize)]
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
    pub async fn prepare_upload(
        &self,
        peer: &str,
        files: BTreeMap<String, FileMetadata>,
    ) -> Result<PrepareUploadResponse> {
        let Some((addr, device)) = self.peers.lock().await.get(peer).cloned() else {
            return Err(LocalSendError::PeerNotFound);
        };

        let request = self
            .client
            .post(format!(
                "{}://{}/api/localsend/v2/prepare-upload",
                device.protocol, addr
            ))
            .json(&PrepareUploadRequest {
                info: self.config.device.clone(),
                files: files.clone(),
            });

        let r = request.timeout(Duration::from_secs(30)).build().unwrap();
        debug!("sending '{r:?}' to peer at {addr:?}");

        let response = self.client.execute(r).await?;

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
            sender: self.config.device.clone(),
            status: SessionStatus::Active,
            addr,
        };

        self.sessions
            .lock()
            .await
            .insert(response.session_id.clone(), session);

        Ok(response)
    }

    pub async fn send_file(&self, peer: &str, file_path: PathBuf) -> Result<()> {
        // Generate file metadata
        let file_metadata = FileMetadata::from_path(&file_path)?;

        // Prepare files map
        let mut files = BTreeMap::new();
        files.insert(file_metadata.id.clone(), file_metadata.clone());

        // Prepare upload
        let prepare_response = self.prepare_upload(peer, files).await?;

        // Get file token
        let token = prepare_response
            .files
            .get(&file_metadata.id)
            .ok_or(LocalSendError::InvalidToken)?;

        // Read file contents
        let file_contents = tokio::fs::read(&file_path).await?;
        let bytes = Bytes::from(file_contents);

        // Upload file
        self.send_bytes(
            &prepare_response.session_id,
            &file_metadata.id,
            token,
            bytes,
        )
        .await?;

        Ok(())
    }

    pub async fn send_text(&self, peer: &str, text: &str) -> Result<()> {
        // Generate file metadata
        let file_metadata = FileMetadata::from_text(text)?;

        // Prepare files map
        let mut files = BTreeMap::new();
        files.insert(file_metadata.id.clone(), file_metadata.clone());

        // Prepare upload
        let prepare_response = self.prepare_upload(peer, files).await?;

        // Get file token
        let token = prepare_response
            .files
            .get(&file_metadata.id)
            .ok_or(LocalSendError::InvalidToken)?;

        let bytes = Bytes::from(text.to_owned());

        // Upload file
        self.send_bytes(
            &prepare_response.session_id,
            &file_metadata.id,
            token,
            bytes,
        )
        .await?;

        Ok(())
    }

    pub async fn cancel_upload(&self, session_id: &str) -> Result<()> {
        let sessions = self.sessions.lock().await;
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

    async fn send_bytes(
        &self,
        session_id: &str,
        content_id: &str,
        token: &String,
        body: Bytes,
    ) -> Result<()> {
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id).unwrap();

        if session.status != SessionStatus::Active {
            return Err(LocalSendError::SessionInactive);
        }

        if session.file_tokens.get(content_id) != Some(token) {
            return Err(LocalSendError::InvalidToken);
        }

        let request = self.client
                          .post(format!(
                              "{}://{}/api/localsend/v2/upload?sessionId={session_id}&fileId={content_id}&token={token}",
                              session.receiver.protocol, session.addr))
                          .body(body).build()?;

        debug!("Uploading bytes: {request:?}");
        let response = self.client.execute(request).await?;

        if response.status() != 200 {
            warn!("Upload failed: {response:?}");
            return Err(LocalSendError::UploadFailed);
        }

        Ok(())
    }
}

pub async fn prepare_upload(
    State(service): State<LocalService>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<PrepareUploadRequest>,
) -> impl IntoResponse {
    info!("Received upload request from alias: {}", req.info.alias);

    let id = Julid::new();
    let (tx, mut rx) = unbounded_channel();
    let request = ReceiveRequest {
        alias: req.info.alias.clone(),
        files: req.files.clone(),
        tx,
    };

    match service
        .transfer_event_tx
        .send(TransferEvent::ReceiveRequest { id, request })
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
        .lock()
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

pub async fn receive_upload(
    Query(params): Query<UploadParams>,
    State(service): State<LocalService>,
    body: Bytes,
) -> impl IntoResponse {
    // Extract query parameters
    let session_id = &params.session_id;
    let file_id = &params.file_id;
    let token = &params.token;

    // Get session and validate
    let mut sessions_lock = service.sessions.lock().await;
    let session = match sessions_lock.get_mut(session_id) {
        Some(session) => session,
        None => return StatusCode::BAD_REQUEST.into_response(),
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
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "File not found".to_string(),
            )
                .into_response();
        }
    };

    let download_dir = &service.config.download_dir;

    // Create directory if it doesn't exist
    if let Err(e) = tokio::fs::create_dir_all(download_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create directory: {e}"),
        )
            .into_response();
    }

    // Create file path
    let file_path = service.config.download_dir.join(&file_metadata.file_name);

    // Write file
    if let Err(e) = tokio::fs::write(&file_path, body).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write file: {e}"),
        )
            .into_response();
    }

    if let Ok(id) = Julid::from_str(session_id) {
        service.send_event(TransferEvent::Received(id));
    };

    StatusCode::OK.into_response()
}

// Query parameters struct
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadParams {
    session_id: String,
    file_id: String,
    token: String,
}

pub async fn handle_cancel(
    Query(params): Query<CancelParams>,
    State(service): State<LocalService>,
) -> impl IntoResponse {
    let mut sessions_lock = service.sessions.lock().await;
    let session = match sessions_lock.get_mut(&params.session_id) {
        Some(session) => session,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    debug!("got cancel request for {}", params.session_id);

    session.status = SessionStatus::Cancelled;

    if let Ok(id) = Julid::from_str(&params.session_id) {
        service.send_event(TransferEvent::Cancelled(id));
    };

    StatusCode::OK.into_response()
}

// Cancel parameters struct
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams {
    session_id: String,
}
