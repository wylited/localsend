use std::{collections::HashMap, net::SocketAddr};

use crate::models::{device::DeviceInfo, file::FileMetadata};

pub struct Session {
    pub session_id: String,
    pub files: HashMap<String, FileMetadata>,
    pub file_tokens: HashMap<String, String>,
    pub receiver: DeviceInfo,
    pub sender: DeviceInfo,
    pub status: SessionStatus,
    pub addr: SocketAddr,
}

#[derive(PartialEq)]
pub enum SessionStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Cancelled,
}
