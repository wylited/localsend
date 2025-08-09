use std::{net::SocketAddr, path::Path, time::Duration};

use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Json, Router,
};
use axum_server::{tls_rustls::RustlsConfig, Handle};
use tokio::sync::mpsc;
use tokio_rustls::rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use tower_http::limit::RequestBodyLimitLayer;

use crate::{
    discovery::register_device,
    transfer::{prepare_upload, receive_upload},
    LocalService,
};

impl LocalService {
    pub async fn start_http_server(&self, stop_rx: mpsc::Receiver<()>) -> crate::error::Result<()> {
        let app = self.create_router();
        // TODO: make addr config
        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.device.port));
        let (key, cert) = self.config.ssl();

        let ssl_config = rustls_server_config(key, cert);

        let handle = Handle::new();

        tokio::spawn(shutdown(handle.clone(), stop_rx));

        axum_server::bind_rustls(addr, ssl_config)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;

        Ok(())
    }

    fn create_router(&self) -> Router {
        let device = self.config.device.clone();
        let d2 = device.clone();
        Router::new()
            .route("/api/localsend/v2/register", post(register_device))
            .route(
                "/api/localsend/v2/info",
                get(move || async move { Json(device) }),
            )
            .route(
                // the mobile client is trying to hit this one
                "/api/localsend/v1/info",
                get(move || async move { Json(d2) }),
            )
            .route("/api/localsend/v2/prepare-upload", post(prepare_upload))
            .route("/api/localsend/v2/upload", post(receive_upload))
            .layer(DefaultBodyLimit::disable())
            .layer(RequestBodyLimitLayer::new(1024 * 1024 * 1024))
            .with_state(self.clone())
    }
}

fn rustls_server_config(key: impl AsRef<Path>, cert: impl AsRef<Path>) -> RustlsConfig {
    let key = match PrivateKeyDer::from_pem_file(&key) {
        Ok(k) => k,
        Err(e) => {
            let path = key.as_ref().display();
            log::error!("could not open {path} for reading; got {e:?}");
            panic!()
        }
    };

    let certs = CertificateDer::pem_file_iter(cert)
        .unwrap()
        .map(|cert| cert.unwrap())
        .collect();

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("bad certificate/key");

    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    RustlsConfig::from_config(config.into())
}

async fn shutdown(handle: Handle, mut rx: mpsc::Receiver<()>) {
    let _ = rx.recv().await;
    log::info!("shutting down http server");
    handle.graceful_shutdown(Some(Duration::from_secs(5)));
}
