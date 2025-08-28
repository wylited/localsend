use std::{path::Path, time::SystemTime};

use chrono::{DateTime, Utc};
use julid::Julid;
use serde::{Deserialize, Serialize};

use crate::error::LocalSendError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMetadata {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    pub file_type: String, // mime type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FileMetadataExt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadataExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessed: Option<String>,
}

impl FileMetadata {
    pub fn from_path(path: &Path) -> crate::error::Result<Self> {
        let metadata = path.metadata()?;
        if !metadata.is_file() {
            return Err(LocalSendError::NotAFile);
        }

        let id = path.to_str().unwrap().to_string();
        let file_name = path.file_name().unwrap().to_str().unwrap().to_string();
        let size = metadata.len();

        let file_type = mime_guess::from_path(path)
            .first()
            .map(|mime| mime.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string()); // Default type if none found

        let sha256 = Some(sha256::try_digest(path)?);

        let metadata = Some(FileMetadataExt {
            modified: metadata.modified().ok().map(format_datetime),
            accessed: metadata.accessed().ok().map(format_datetime),
        });

        Ok(FileMetadata {
            id,
            file_name,
            size,
            file_type,
            sha256,
            preview: None,
            metadata,
        })
    }

    pub fn from_text(text: &str) -> crate::error::Result<Self> {
        let size = text.len() as u64;
        let id = Julid::new().as_string();
        let file_type = "text/plain".into();
        let sha256 = Some(sha256::digest(text));

        Ok(FileMetadata {
            id: id.clone(),
            file_name: format!("{id}.txt"),
            size,
            file_type,
            sha256,
            preview: Some(text.to_string()),
            metadata: None,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Mobile,
    Desktop,
    Web,
    Headless,
    Server,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub alias: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<DeviceType>,
    pub fingerprint: String,
    pub port: u16,
    pub protocol: String,
    #[serde(default)]
    pub download: bool,
    #[serde(default)]
    pub announce: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Https,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            alias: "Localsend".to_string(),
            version: "2.1".to_string(),
            device_model: None,
            device_type: Some(DeviceType::Headless),
            fingerprint: Julid::new().to_string(),
            port: crate::DEFAULT_PORT,
            protocol: "https".to_string(),
            download: false,
            announce: Some(true),
        }
    }
}

impl Device {
    pub fn to_json(&self) -> crate::error::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

fn format_datetime(system_time: SystemTime) -> String {
    let datetime: DateTime<Utc> = system_time.into();
    datetime.to_rfc3339()
}
