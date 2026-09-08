use std::sync::Arc;
use uplink_ingest::rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};

pub struct TestTls {
    pub server: Arc<ServerConfig>,
    pub client: Arc<ClientConfig>,
    pub certificate_pem: String,
}

pub fn test_tls() -> TestTls {
    test_tls_for_names(vec!["localhost".into(), "127.0.0.1".into()])
}

pub fn test_tls_for_names(subject_alt_names: Vec<String>) -> TestTls {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(subject_alt_names).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let client = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    // Privater Schlüssel verlässt nur als In-Memory-DER den Generator.
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
    let server = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap();
    TestTls {
        server: Arc::new(server),
        client: Arc::new(client),
        certificate_pem: cert.pem(),
    }
}
