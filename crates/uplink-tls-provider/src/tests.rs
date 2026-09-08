use super::*;
use std::{collections::HashMap, sync::Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
};

fn config(port: u16) -> Config {
    Config {
        hostname: "test.example".into(),
        certificate_file: "/certificate.pem".into(),
        private_key_file: "/private.pem".into(),
        source_owner_uid: 0,
        identity_file: "/identity.json".into(),
        identity_owner_uid: 0,
        project_id: "00000000-0000-0000-0000-000000000001".into(),
        environment: "prod".into(),
        secret_path: "/".into(),
        infisical_port: port,
    }
}
fn material() -> (Material, Arc<rustls::RootCertStore>) {
    let cert = rcgen::generate_simple_self_signed(vec!["test.example".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    (
        Material {
            certificate: Secret::new(cert.cert.pem().into_bytes()),
            private_key: Secret::new(cert.signing_key.serialize_pem().into_bytes()),
        },
        Arc::new(roots),
    )
}
#[derive(Default)]
struct State {
    values: HashMap<String, String>,
    requests: Vec<(String, String)>,
    fail: Option<(String, String, u16)>,
    discard_writes: bool,
}
struct Server {
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
    port: u16,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut block = [0; 8192];
                let header_end = loop {
                    let count = socket.read(&mut block).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&block[..count]);
                    assert!(request.len() <= MAX_RESPONSE);
                    if let Some(index) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let head = std::str::from_utf8(&request[..header_end]).unwrap();
                let mut line = head.lines().next().unwrap().split_whitespace();
                let method = line.next().unwrap().to_owned();
                let path = line.next().unwrap().split('?').next().unwrap().to_owned();
                let size = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                assert!(size <= MAX_RESPONSE);
                while request.len() < header_end + size {
                    let count = socket.read(&mut block).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&block[..count]);
                    assert!(request.len() <= MAX_RESPONSE);
                }
                let (status, body) = {
                    let mut state = shared.lock().unwrap();
                    let name = path.rsplit('/').next().unwrap().to_owned();
                    assert!([CERTIFICATE_SECRET, PRIVATE_KEY_SECRET].contains(&name.as_str()));
                    state.requests.push((method.clone(), name.clone()));
                    if let Some((expected_method, expected_name, status)) = &state.fail
                        && *expected_method == method
                        && *expected_name == name
                    {
                        (*status, b"sensitive-reflected-error".to_vec())
                    } else if method == "GET" {
                        assert!(path.starts_with("/api/v4/secrets/"));
                        match state.values.get(&name){Some(value)=>(200,serde_json::to_vec(&serde_json::json!({"secret":{"secretKey":name,"secretValue":value}})).unwrap()),None=>(404,Vec::new())}
                    } else {
                        assert!(path.starts_with("/api/v3/secrets/raw/"));
                        assert!(method == "PATCH" || method == "POST");
                        if method == "PATCH" && !state.values.contains_key(&name) {
                            (404, Vec::new())
                        } else {
                            let value: serde_json::Value =
                                serde_json::from_slice(&request[header_end..header_end + size])
                                    .unwrap();
                            assert_eq!(value["workspaceId"], config(port).project_id);
                            assert_eq!(value["environment"], "prod");
                            assert_eq!(value["secretPath"], "/");
                            assert_eq!(value["type"], "shared");
                            if !state.discard_writes {
                                state
                                    .values
                                    .insert(name, value["secretValue"].as_str().unwrap().into());
                            }
                            (200, Vec::new())
                        }
                    }
                };
                let header = format!(
                    "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        Self { state, task, port }
    }
    fn vault(&self) -> Vault {
        Vault::new(
            &config(self.port),
            &Secret::new(b"synthetic-test-identity".to_vec()),
        )
        .unwrap()
    }
    fn set(&self, source: &Material) {
        let mut state = self.state.lock().unwrap();
        state.values.insert(
            CERTIFICATE_SECRET.into(),
            String::from_utf8(source.certificate.0.to_vec()).unwrap(),
        );
        state.values.insert(
            PRIVATE_KEY_SECRET.into(),
            String::from_utf8(source.private_key.0.to_vec()).unwrap(),
        );
    }
    fn writes(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(method, _)| method != "GET")
            .cloned()
            .collect()
    }
}

#[tokio::test]
async fn unchanged_pair_never_writes_and_missing_pair_is_created_then_verified() {
    let server = Server::start().await;
    let (source, roots) = material();
    let vault = server.vault();
    assert_eq!(
        sync_material(&vault, &source, "test.example", roots.clone()).await,
        Ok(SyncStatus::Updated)
    );
    assert_eq!(
        server.writes(),
        vec![
            ("PATCH".into(), CERTIFICATE_SECRET.into()),
            ("POST".into(), CERTIFICATE_SECRET.into()),
            ("PATCH".into(), PRIVATE_KEY_SECRET.into()),
            ("POST".into(), PRIVATE_KEY_SECRET.into())
        ]
    );
    server.state.lock().unwrap().requests.clear();
    assert_eq!(
        sync_material(&vault, &source, "test.example", roots).await,
        Ok(SyncStatus::Unchanged)
    );
    assert!(server.writes().is_empty());
}

#[tokio::test]
async fn partial_write_is_visible_and_next_attempt_updates_only_unfinished_key() {
    let server = Server::start().await;
    let (old, _) = material();
    server.set(&old);
    let (new, roots) = material();
    let vault = server.vault();
    server.state.lock().unwrap().fail = Some(("PATCH".into(), PRIVATE_KEY_SECRET.into(), 500));
    assert_eq!(
        sync_material(&vault, &new, "test.example", roots.clone()).await,
        Err(Error::Rejected(500))
    );
    assert_eq!(server.writes().len(), 2);
    {
        let mut state = server.state.lock().unwrap();
        state.fail = None;
        state.requests.clear();
    }
    assert_eq!(
        sync_material(&vault, &new, "test.example", roots).await,
        Ok(SyncStatus::Updated)
    );
    assert_eq!(
        server.writes(),
        vec![("PATCH".into(), PRIVATE_KEY_SECRET.into())]
    );
}

#[tokio::test]
async fn access_rejection_never_becomes_creation_or_reflected_output() {
    for status in [401, 403, 409, 500] {
        let server = Server::start().await;
        let (old, _) = material();
        server.set(&old);
        let (new, roots) = material();
        server.state.lock().unwrap().fail =
            Some(("PATCH".into(), CERTIFICATE_SECRET.into(), status));
        let result = sync_material(&server.vault(), &new, "test.example", roots).await;
        assert_eq!(result, Err(Error::Rejected(status)));
        assert_eq!(
            server.writes(),
            vec![("PATCH".into(), CERTIFICATE_SECRET.into())]
        );
        assert!(!format!("{result:?}").contains("sensitive-reflected-error"));
    }
}

#[tokio::test]
async fn accepted_http_write_is_not_a_verified_sync_and_invalid_source_never_contacts_vault() {
    let server = Server::start().await;
    let (old, _) = material();
    server.set(&old);
    let (new, roots) = material();
    server.state.lock().unwrap().discard_writes = true;
    assert_eq!(
        sync_material(&server.vault(), &new, "test.example", roots.clone()).await,
        Err(Error::Verification)
    );
    server.state.lock().unwrap().requests.clear();
    assert_eq!(
        sync_material(&server.vault(), &new, "wrong.example", roots).await,
        Err(Error::Certificate)
    );
    assert!(server.state.lock().unwrap().requests.is_empty());
}

#[test]
fn identity_and_configuration_reject_untrusted_input_without_disclosing_it() {
    assert!(identity(br#"{"serviceToken":"synthetic-test-identity"}"#).is_ok());
    for source in [
        b"not-json-sensitive-sentinel".as_slice(),
        br#"{"serviceToken":""}"#,
        br#"{"serviceToken":"bad\nheader"}"#,
    ] {
        let error = identity(source).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("sentinel"));
    }
    let mut value = config(8080);
    assert!(value.validate().is_ok());
    value.secret_path = "/../foreign".into();
    assert_eq!(value.validate(), Err(Error::Configuration));
    value = config(8080);
    value.hostname = "127.0.0.1".into();
    assert_eq!(value.validate(), Err(Error::Configuration));
    value = config(8080);
    value.private_key_file = "/foreign/../secret".into();
    assert_eq!(value.validate(), Err(Error::Configuration));
}

#[test]
fn expired_certificate_and_multiple_private_keys_are_rejected() {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["test.example".into()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2010, 1, 1);
    params.not_after = rcgen::date_time_ymd(2020, 1, 1);
    let cert = params.self_signed(&key).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let expired = Material {
        certificate: Secret::new(cert.pem().into_bytes()),
        private_key: Secret::new(key.serialize_pem().into_bytes()),
    };
    assert_eq!(
        validate_material(&expired, "test.example", Arc::new(roots), UnixTime::now()),
        Err(Error::Certificate)
    );
    let (mut valid, roots) = material();
    let duplicate = valid.private_key.0.to_vec();
    valid.private_key.0.extend_from_slice(&duplicate);
    assert_eq!(
        validate_material(&valid, "test.example", roots, UnixTime::now()),
        Err(Error::KeyMismatch)
    );
}

#[tokio::test]
async fn response_limit_and_transport_redirect_never_allow_sync() {
    let server = Server::start().await;
    let (source, roots) = material();
    server.state.lock().unwrap().fail = Some(("GET".into(), CERTIFICATE_SECRET.into(), 302));
    assert_eq!(
        sync_material(&server.vault(), &source, "test.example", roots.clone()).await,
        Err(Error::Rejected(302))
    );
    assert!(server.writes().is_empty());
    {
        let mut state = server.state.lock().unwrap();
        state.fail = None;
        state
            .values
            .insert(CERTIFICATE_SECRET.into(), "x".repeat(MAX_RESPONSE + 1));
    }
    assert_eq!(
        sync_material(&server.vault(), &source, "test.example", roots).await,
        Err(Error::Response)
    );
    assert!(server.writes().is_empty());
}

#[tokio::test]
async fn bounded_file_reader_checks_uid_permissions_symlink_and_size() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let path =
        std::env::temp_dir().join(format!("uplink-public-reader-test-{}", std::process::id()));
    let link = path.with_extension("link");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    use std::io::Write;
    file.write_all(b"public test input").unwrap();
    drop(file);
    let uid = std::fs::metadata(&path).unwrap().uid();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read_file(&path, uid, true, 64).await.is_ok());
    assert_eq!(
        read_file(&path, uid, true, 4).await.unwrap_err(),
        Error::InputTooLarge
    );
    assert_eq!(
        read_file(&path, uid.saturating_add(1), true, 64)
            .await
            .unwrap_err(),
        Error::FilePermissions
    );
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(
        read_file(&link, uid, true, 64).await.unwrap_err(),
        Error::File
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        read_file(&path, uid, true, 64).await.unwrap_err(),
        Error::FilePermissions
    );
    std::fs::remove_file(link).unwrap();
    std::fs::remove_file(path).unwrap();
}
