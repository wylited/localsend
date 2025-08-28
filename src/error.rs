#[derive(Debug, thiserror::Error)]
pub enum LocalSendError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Request error: {0}")]
    RequestError(#[from] reqwest::Error),

    #[error("Async join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),

    #[error("Invalid PIN")]
    InvalidPin,

    #[error("Session blocked")]
    SessionBlocked,

    #[error("Too many requests")]
    TooManyRequests,

    #[error("Not a file")]
    NotAFile,

    #[error("Peer not found")]
    PeerNotFound,

    #[error("Upload failed")]
    UploadFailed,

    #[error("Invalid token")]
    InvalidToken,

    #[error("Session inactive")]
    SessionInactive,

    #[error("Session not found")]
    SessionNotFound,

    #[error("Cancel Failed")]
    CancelFailed,

    #[error("IPv6 is not supported")]
    IPv6Unsupported,

    #[error("Error getting local IP")]
    IpAddrError(#[from] local_ip_address::Error),

    #[error("Error getting network interface")]
    NetworkInterfaceError(#[from] network_interface::Error),

    #[error("Error: could not get $HOME value")]
    NoHomeDir,

    #[error("Could not generate SSL certs")]
    SslGenFail(#[from] rcgen::Error),

    #[error("Could not serialize config")]
    ConfigSerializationFail(#[from] toml::ser::Error),

    #[error("Could not parse config file")]
    ConfigParseError(#[from] Box<figment::Error>),
}

pub type Result<T> = std::result::Result<T, LocalSendError>;
