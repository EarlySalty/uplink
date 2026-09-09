use super::{ProbeResult, config::Config, source::MAX_TAG};
use std::sync::Arc;
use uplink_ingest::{
    AuthorizedSession, Authorizer, IngestLimits, IngestServer,
    rustls::{
        self,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use uplink_media::{PublishSecret, PublishTarget};

pub struct LocalAuth;

impl Authorizer for LocalAuth {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        if app == "synthetic" && stream == "local-load" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

pub async fn endpoint(
    config: &Config,
    id: &str,
) -> ProbeResult<(Arc<IngestServer<LocalAuth>>, PublishTarget)> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .map_err(|_| "Lokales TLS-Zertifikat konnte nicht erzeugt werden")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(cert.der().clone())
        .map_err(|_| "Lokales TLS-Zertifikat ist ungültig")?;
    let client = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| "Lokale TLS-Version ist ungültig")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "Lokale TLS-Version ist ungültig")?
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der())),
        )
        .map_err(|_| "Lokale TLS-Konfiguration ist ungültig")?;
    let mut limits = IngestLimits::local_probe();
    limits.max_queued_events = config.queue_events;
    limits.max_queued_bytes = config.queue_bytes;
    limits.max_event_bytes = MAX_TAG;
    limits.rtmp.chunk.max_message_bytes = MAX_TAG;
    limits.rtmp.chunk.max_partial_bytes = config.queue_bytes;
    limits.media_idle_timeout = std::time::Duration::from_secs(30);
    let server = Arc::new(
        IngestServer::bind_loopback(0, Arc::new(server_tls), Arc::new(LocalAuth), limits)
            .await
            .map_err(|_| "Lokaler RTMPS-Eingang konnte nicht starten")?,
    );
    let port = server
        .local_addr()
        .map_err(|_| "Lokaler RTMPS-Eingang hat keine Adresse")?
        .port();
    let target = PublishTarget {
        id: id.into(),
        endpoint: format!("rtmps://localhost:{port}/synthetic"),
        playpath: PublishSecret::new(b"local-load".to_vec())
            .map_err(|_| "Lokale Testautorisierung ist ungültig")?,
        tls: Some(Arc::new(client)),
        allowed_hosts: vec!["localhost".into()],
        allow_loopback: true,
        allow_unencrypted: false,
    };
    Ok((server, target))
}
