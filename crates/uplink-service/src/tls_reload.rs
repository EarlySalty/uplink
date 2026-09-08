use crate::crypto::Secret;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use uplink_ingest::rustls::{
    self,
    client::danger::ServerCertVerifier,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};

pub struct ReloadingTls {
    host: ServerName<'static>,
    verifier: Arc<rustls::client::WebPkiServerVerifier>,
    current: RwLock<Option<Arc<CertifiedKey>>>,
    refresh_failed: AtomicBool,
}
impl std::fmt::Debug for ReloadingTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReloadingTls { material: [geschützt] }")
    }
}
impl ReloadingTls {
    pub fn new(host: String, roots: rustls::RootCertStore) -> Result<Arc<Self>, &'static str> {
        let host = ServerName::try_from(host).map_err(|_| "TLS-Hostname ist ungültig.")?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|_| "TLS-Vertrauensanker fehlen.")?;
        Ok(Arc::new(Self {
            host,
            verifier,
            current: RwLock::new(None),
            refresh_failed: AtomicBool::new(false),
        }))
    }
    pub fn install(&self, certificate: &Secret, private_key: &Secret) -> Result<(), &'static str> {
        let result = self.install_at(certificate, private_key, UnixTime::now());
        self.mark_refresh_failure(result.is_err());
        result
    }
    pub fn mark_refresh_failure(&self, failed: bool) {
        self.refresh_failed.store(failed, Ordering::Relaxed);
    }
    pub fn refresh_failed(&self) -> bool {
        self.refresh_failed.load(Ordering::Relaxed)
    }
    pub fn ready(&self) -> bool {
        self.at(UnixTime::now()).is_some()
    }
    fn install_at(
        &self,
        certificate: &Secret,
        private_key: &Secret,
        now: UnixTime,
    ) -> Result<(), &'static str> {
        if certificate.expose().len() > 64 * 1024 || private_key.expose().len() > 16 * 1024 {
            return Err("TLS-Material ist zu groß.");
        }
        let certs = CertificateDer::pem_slice_iter(certificate.expose())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "TLS-Zertifikatskette ist ungültig.")?;
        let key = PrivateKeyDer::from_pem_slice(private_key.expose())
            .map_err(|_| "TLS-Schlüssel ist ungültig.")?;
        let candidate =
            CertifiedKey::from_der(certs, key, &rustls::crypto::ring::default_provider())
                .map_err(|_| "TLS-Zertifikat und Schlüssel passen nicht zusammen.")?;
        candidate
            .keys_match()
            .map_err(|_| "TLS-Schlüsselpaar konnte nicht bestätigt werden.")?;
        self.validate(&candidate, now)?;
        *self
            .current
            .write()
            .map_err(|_| "TLS-Zustand ist nicht verfügbar.")? = Some(Arc::new(candidate));
        Ok(())
    }
    fn validate(&self, key: &CertifiedKey, now: UnixTime) -> Result<(), &'static str> {
        let leaf = key.cert.first().ok_or("TLS-Zertifikatskette fehlt.")?;
        self.verifier
            .verify_server_cert(leaf, &key.cert[1..], &self.host, &[], now)
            .map_err(
                |_| "TLS-Zertifikat ist nicht gültig oder vertrauenswürdig für diesen Host.",
            )?;
        Ok(())
    }
    fn at(&self, now: UnixTime) -> Option<Arc<CertifiedKey>> {
        let key = self.current.read().ok()?.clone()?;
        self.validate(&key, now).ok()?;
        Some(key)
    }
    pub fn server_config(self: &Arc<Self>) -> Result<Arc<rustls::ServerConfig>, &'static str> {
        if self.at(UnixTime::now()).is_none() {
            return Err("Kein gültiges TLS-Schlüsselpaar geladen.");
        }
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS-Protokollkonfiguration ist ungültig.")?
        .with_no_client_auth()
        .with_cert_resolver(self.clone());
        // Every new connection must consult the current, still-valid certificate.
        // Resumed sessions would bypass the resolver and its expiration check.
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        config.send_tls13_tickets = 0;
        Ok(Arc::new(config))
    }
}
impl ResolvesServerCert for ReloadingTls {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.at(UnixTime::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn material(host: &str) -> (Secret, Secret, CertificateDer<'static>) {
        let generated = rcgen::generate_simple_self_signed(vec![host.into()]).unwrap();
        (
            Secret::new(generated.cert.pem().into_bytes()),
            Secret::new(generated.signing_key.serialize_pem().into_bytes()),
            generated.cert.der().clone(),
        )
    }
    #[test]
    fn restarted_resolver_rejects_partial_pair_until_provider_repairs_it() {
        let (old_certificate, old_key, old_der) = material("localhost");
        let (new_certificate, new_key, new_der) = material("localhost");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(old_der.clone()).unwrap();
        roots.add(new_der.clone()).unwrap();
        let running = ReloadingTls::new("localhost".into(), roots.clone()).unwrap();
        running.install(&old_certificate, &old_key).unwrap();
        // Only the certificate PATCH succeeded; a running process keeps its old pair.
        assert!(running.install(&new_certificate, &old_key).is_err());
        assert_eq!(running.at(UnixTime::now()).unwrap().cert[0], old_der);
        drop(running);
        // A fresh resolver has no persisted fallback and cannot start a TLS listener.
        let restarted = ReloadingTls::new("localhost".into(), roots).unwrap();
        assert!(restarted.install(&new_certificate, &old_key).is_err());
        assert!(restarted.refresh_failed());
        assert!(!restarted.ready());
        assert!(restarted.server_config().is_err());
        // After the provider retries the missing key, startup can succeed.
        restarted.install(&new_certificate, &new_key).unwrap();
        assert!(!restarted.refresh_failed());
        assert_eq!(restarted.at(UnixTime::now()).unwrap().cert[0], new_der);
        assert!(restarted.server_config().is_ok());
    }
    #[tokio::test]
    async fn valid_tls_rotation_keeps_listener_and_rejects_wrong_pair_host_and_expiration() {
        let (first, first_key, first_der) = material("localhost");
        let (second, second_key, second_der) = material("localhost");
        let (wrong, wrong_key, wrong_der) = material("wrong.invalid");
        let mut roots = rustls::RootCertStore::empty();
        for cert in [&first_der, &second_der, &wrong_der] {
            roots.add(cert.clone()).unwrap();
        }
        let reload = ReloadingTls::new("localhost".into(), roots.clone()).unwrap();
        assert!(reload.server_config().is_err());
        reload.install(&first, &first_key).unwrap();
        assert!(reload.install(&second, &first_key).is_err());
        assert!(reload.install(&wrong, &wrong_key).is_err());
        assert_eq!(reload.at(UnixTime::now()).unwrap().cert[0], first_der);
        let expired = UnixTime::since_unix_epoch(std::time::Duration::from_secs(253402300799));
        assert!(reload.install_at(&second, &second_key, expired).is_err());
        assert!(reload.at(expired).is_none());
        let server = reload.server_config().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let acceptor = tokio_rustls::TlsAcceptor::from(server);
            for _ in 0..2 {
                let (socket, _) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(socket).await.unwrap();
                tokio::io::AsyncWriteExt::write_all(&mut tls, &[1])
                    .await
                    .unwrap();
            }
        });
        let client = Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        );
        for (certificate, key, expected) in [
            (&first, &first_key, &first_der),
            (&second, &second_key, &second_der),
        ] {
            reload.install(certificate, key).unwrap();
            let socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let mut tls = tokio_rustls::TlsConnector::from(client.clone())
                .connect("localhost".try_into().unwrap(), socket)
                .await
                .unwrap();
            let mut byte = [0];
            tokio::io::AsyncReadExt::read_exact(&mut tls, &mut byte)
                .await
                .unwrap();
            assert_eq!(&tls.get_ref().1.peer_certificates().unwrap()[0], expected);
        }
        task.await.unwrap();
    }
}
