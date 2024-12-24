use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query};
use axum::Extension;
use axum::{response::IntoResponse, Json};
use axum::http::StatusCode;

use native_dialog::MessageDialog;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;
use crate::error::{LocalSendError, Result};
use crate::transfer::session::{Session, SessionStatus};
use crate::{models::{device::DeviceInfo, file::FileMetadata}, Client};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    pub files: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadRequest {
    pub info: DeviceInfo,
    pub files: HashMap<String, FileMetadata>,
}

impl Client {
    pub async fn prepare_upload(&self, peer: String, files: HashMap<String, FileMetadata>) -> Result<PrepareUploadResponse> {
        if !self.peers.lock().await.contains_key(&peer) {
            return Err(LocalSendError::PeerNotFound);
        }

        let peer = self.peers.lock().await.get(&peer).unwrap().clone();
        println!("Peer: {:?}", peer);

        let response = self
            .http_client
            .post(&format!("{}://{}/api/localsend/v2/prepare-upload", peer.1.protocol, peer.0.clone()))
            .json(&PrepareUploadRequest {
                info: self.device.clone(),
                files: files.clone(),
            })
            .send()
            .await?;

        println!("Response: {:?}", response);

        let response: PrepareUploadResponse = response.json().await?;

        let session = Session {
            session_id: response.session_id.clone(),
            files,
            file_tokens: response.files.clone(),
            receiver: peer.1,
            sender: self.device.clone(),
            status: SessionStatus::Active,
            addr: peer.0,
        };

        self.sessions.lock().await.insert(response.session_id.clone(), session);

        Ok(response)
    }

    pub async fn upload(&self, session_id: String, file_id: String, token: String, body: Bytes) -> Result<()> {
        let sessions = self.sessions.lock().await;
        let session = sessions.get(&session_id).unwrap();

        if session.status != SessionStatus::Active {
            return Err(LocalSendError::SessionInactive);
        }

        if session.file_tokens.get(&file_id) != Some(&token) {
            return Err(LocalSendError::InvalidToken);
        }

        let request = self
            .http_client
            .post(&format!("{}://{}/api/localsend/v2/upload?sessionId={}&fileId={}&token={}", session.receiver.protocol, session.addr, session_id, file_id, token))
            //.post(&format!("https://webhook.site/2f23a529-b687-4375-ad5f-54906ab26ac7?session_id={}&file_id={}&token={}", session_id, file_id, token))
            .body(body);

        println!("Uploading file: {:?}", request);
        let response = request.send().await?;

        if response.status() != 200 {
            println!("Upload failed: {:?}", response);
            return Err(LocalSendError::UploadFailed);
        }

        Ok(())
    }

    pub async fn send_file(&self, peer: String, file_path: PathBuf) -> Result<()> {
        // Generate file metadata
        let file_metadata = FileMetadata::from_path(&file_path)?;

        // Prepare files map
        let mut files = HashMap::new();
        files.insert(file_metadata.id.clone(), file_metadata.clone());

        // Prepare upload
        let prepare_response = self.prepare_upload(peer, files).await?;

        // Get file token
        let token = prepare_response.files.get(&file_metadata.id)
            .ok_or(LocalSendError::InvalidToken)?;

        // Read file contents
        let file_contents = tokio::fs::read(&file_path).await?;
        let bytes = Bytes::from(file_contents);

        // Upload file
        self.upload(
            prepare_response.session_id,
            file_metadata.id,
            token.clone(),
            bytes
        ).await?;

        Ok(())
    }
}

pub async fn register_prepare_upload(
    Extension(client): Extension<DeviceInfo>,
    Extension(sessions): Extension<Arc<Mutex<HashMap<String, Session>>>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<PrepareUploadRequest>,
) -> impl IntoResponse {
    println!("Received upload request from alias: {}", req.info.alias);

    let result = MessageDialog::new()
        .set_title(&req.info.alias)
        .set_text("Do you want to receive files from this device?")
        .show_confirm()
        .unwrap();

    if result {
        let session_id = Uuid::new_v4().to_string();

        let file_tokens: HashMap<String, String> = req.files.iter()
                                                      .map(|(id, _)| (id.clone(), Uuid::new_v4().to_string())) // Replace with actual token logic
                                                      .collect();

        let session = Session {
            session_id: session_id.clone(),
            files: req.files.clone(),
            file_tokens: file_tokens.clone(),
            receiver: client.clone(),
            sender: req.info.clone(),
            status: SessionStatus::Active,
            addr,
        };

        sessions.lock().await.insert(session_id.clone(), session);

        return (StatusCode::OK,
                Json(PrepareUploadResponse {
                    session_id,
                    files: file_tokens,
                })).into_response();
    } else {
        return StatusCode::FORBIDDEN.into_response();
    }
}

pub async fn register_upload(
    Query(params): Query<UploadParams>,
    Extension(sessions): Extension<Arc<Mutex<HashMap<String, Session>>>>,
    body: Bytes,
) -> impl IntoResponse {
    // Extract query parameters
    let session_id = &params.session_id;
    let file_id = &params.file_id;
    let token = &params.token;
    let download_dir = PathBuf::from("/home/wyli/Downloads");

    // Get session and validate
    let mut sessions_lock = sessions.lock().await;
    let session = match sessions_lock.get_mut(session_id) {
        Some(session) => session,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };


    if session.status != SessionStatus::Active {
        return StatusCode::BAD_REQUEST.into_response()
    }

    // Validate token
    if session.file_tokens.get(file_id) != Some(&token.to_string()) {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Get file metadata
    let file_metadata = match session.files.get(file_id) {
        Some(metadata) => metadata,
        None => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "File not found".to_string(),
        )
            .into_response(),
    };

    // Create directory if it doesn't exist
    if let Err(e) = tokio::fs::create_dir_all(&*download_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create directory: {}", e),
        )
            .into_response();
    }

    // Create file path
    let file_path = download_dir.join(&file_metadata.file_name);

    // Write file
    if let Err(e) = tokio::fs::write(&file_path, body).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write file: {}", e),
        )
            .into_response();
    }

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

pub async fn register_cancel(
    Query(params): Query<CancelParams>,
    Extension(sessions): Extension<Arc<Mutex<HashMap<String, Session>>>>,
) -> impl IntoResponse {
    let mut sessions_lock = sessions.lock().await;
    let session = match sessions_lock.get_mut(&params.session_id) {
        Some(session) => session,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    session.status = SessionStatus::Cancelled;
    StatusCode::OK.into_response()
}

// Cancel parameters struct
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams {
    session_id: String,
}
