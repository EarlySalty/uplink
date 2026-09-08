use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::process::Command;
use tower::ServiceExt;
use uplink_service::{
    api::{ServiceState, router},
    config::Config,
    crypto::Secret,
    registry::Registry,
    secrets::ServiceSecrets,
    store::Store,
};
#[path = "support/rtmp.rs"]
mod rtmp;
#[path = "../../uplink-ingest/tests/support/tls.rs"]
mod tls;

struct Collector(Arc<std::sync::Mutex<Vec<uplink_ingest::TrackIdentity>>>);
impl uplink_service::runtime::SessionProcessor for Collector {
    async fn process(
        &self,
        first: uplink_ingest::MediaEvent,
        mut events: tokio::sync::mpsc::Receiver<uplink_ingest::MediaEvent>,
    ) -> Result<(), &'static str> {
        self.0.lock().unwrap().push(first.identity);
        while let Some(event) = events.recv().await {
            self.0.lock().unwrap().push(event.identity);
        }
        Ok(())
    }
}

struct Database {
    child: tokio::process::Child,
    directory: PathBuf,
}
impl Database {
    async fn start() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let directory = PathBuf::from("/tmp").join(format!("uplink-db-{}", hex::encode(random)));
        std::fs::create_dir(&directory).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let initialized = Command::new("/usr/lib/postgresql/16/bin/initdb")
            .args([
                "--auth=trust",
                "--username=uplink_test",
                "--no-locale",
                "-D",
            ])
            .arg(directory.join("data"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status();
        assert!(
            tokio::time::timeout(Duration::from_secs(20), initialized)
                .await
                .unwrap()
                .unwrap()
                .success(),
            "PostgreSQL-16-Testwerkzeuge sind erforderlich"
        );
        let child = Command::new("/usr/lib/postgresql/16/bin/postgres")
            .arg("-D")
            .arg(directory.join("data"))
            .args(["-h", "", "-k"])
            .arg(&directory)
            .args(["-c", "fsync=off", "-c", "max_connections=10"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self { child, directory }
    }
    async fn connect(&self) -> Store {
        let url = Secret::new(
            format!(
                "host={} user=uplink_test dbname=postgres",
                self.directory.display()
            )
            .into_bytes(),
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(store) = Store::connect(&url, 4).await {
                    return store;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn stop(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

async fn executable_smoke(database: &Database) {
    use std::{
        io::{Seek, SeekFrom, Write},
        os::fd::AsRawFd,
    };
    use tokio::io::AsyncBufReadExt;
    let identity = memfd::MemfdOptions::default()
        .close_on_exec(false)
        .create("synthetic-service-bootstrap")
        .unwrap();
    identity
        .as_file()
        .write_all(b"synthetic-bootstrap")
        .unwrap();
    identity.as_file().seek(SeekFrom::Start(0)).unwrap();
    let certificates = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let reply = serde_json::json!({"secrets":[
        {"secretKey":"RS_RELAY_API_SECRET","secretValue":"synthetic-api"},
        {"secretKey":"RS_RELAY_ADMIN_SECRET","secretValue":"synthetic-admin"},
        {"secretKey":"RS_RELAY_KEY_ENC","secretValue":"0707070707070707070707070707070707070707070707070707070707070707"},
        {"secretKey":"RS_RELAY_DATABASE_URL","secretValue":format!("host={} user=uplink_test dbname=postgres",database.directory.display())},
        {"secretKey":"RS_RELAY_BOT_INTERNAL_TOKEN","secretValue":"synthetic-broker"},
        {"secretKey":"UPLINK_TLS_CERTIFICATE","secretValue":certificates.cert.pem()},
        {"secretKey":"UPLINK_TLS_PRIVATE_KEY","secretValue":certificates.signing_key.serialize_pem()}
    ]});
    let app = axum::Router::new().route(
        "/api/v4/secrets/",
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let reply = reply.clone();
            async move {
                assert_eq!(
                    headers.get("Authorization").unwrap(),
                    "Bearer synthetic-bootstrap"
                );
                axum::Json(reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let config = include_str!("../../../config/uplink-beispiel.toml")
        .replace("127.0.0.1:8892", "127.0.0.1:0")
        .replace("127.0.0.1:8893", "127.0.0.1:0")
        .replace("http://127.0.0.1:8080", &format!("http://{address}"))
        .replace(
            "credential_fd = 5",
            &format!("credential_fd = {}", identity.as_raw_fd()),
        )
        .replace("/opt/uplink/media/ffmpeg", "/usr/bin/ffmpeg")
        .replace("/opt/uplink/media/ffprobe", "/usr/bin/ffprobe")
        .replace(
            "/run/user/1000/uplink-media",
            database.directory.join("media").to_str().unwrap(),
        );
    let config_path = database.directory.join("service.toml");
    std::fs::write(&config_path, config).unwrap();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("uplink-service");
    let mut daemon = Command::new(binary)
        .arg("--config")
        .arg(config_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = tokio::io::BufReader::new(daemon.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(15), output.next_line())
        .await
        .unwrap()
        .unwrap()
        .expect("Dienst muss gestartete Listener melden");
    assert!(line.len() < 256);
    let address: std::net::SocketAddr = line.split_whitespace().nth(2).unwrap().parse().unwrap();
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        Command::new("/usr/bin/kill")
            .arg("-TERM")
            .arg(daemon.id().unwrap().to_string())
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(15), daemon.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    provider.abort();
    let _ = provider.await;
}

#[tokio::test]
#[ignore = "Expliziter Integrationslauf benötigt lokale PostgreSQL-16-Binaries, keine Produktionsdaten."]
async fn controlplane_preserves_credentials_and_rejects_unauthorized_changes() {
    let database = Database::start().await;
    let store = Arc::new(database.connect().await);
    for statement in [
        "CREATE SCHEMA relay",
        "CREATE TABLE relay.users(streamer_id bigint PRIMARY KEY,enabled boolean NOT NULL,ingest_key_enc bytea,dock_token_enc bytea,ingest_key_hash text,reconnect_wait_s integer NOT NULL DEFAULT 0)",
        "CREATE TABLE relay.destinations(streamer_id bigint REFERENCES relay.users(streamer_id),platform text NOT NULL,rtmp_url text NOT NULL,stream_key_enc bytea NOT NULL,enabled boolean NOT NULL,width integer,height integer,fps integer,bitrate_kbps integer,UNIQUE(streamer_id,platform))",
        "CREATE TABLE relay.waitlist(streamer_id bigint PRIMARY KEY)",
    ] {
        store.query(statement, &[]).await.unwrap();
    }
    let encryption = Secret::new(vec![7; 32]);
    let key = encryption
        .seal(b"rsr_00000000000000000000000000000000", "ingest_key:11")
        .unwrap();
    let hash = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(
            b"rsr_00000000000000000000000000000000",
        ))
    };
    store.query("INSERT INTO relay.users(streamer_id,enabled,ingest_key_enc,ingest_key_hash) VALUES(11,true,$1,$2),(12,false,NULL,NULL)",&[&key,&hash]).await.unwrap();
    let state = Arc::new(ServiceState {
        config: Config::parse(include_str!("../../../config/uplink-beispiel.toml")).unwrap(),
        store: store.clone(),
        secrets: Arc::new(ServiceSecrets {
            api: Secret::new(b"synthetic-api".to_vec()),
            admin: Secret::new(b"synthetic-admin".to_vec()),
            database: Secret::new(Vec::new()),
            bot_internal: Secret::new(Vec::new()),
            encryption,
            tls_material: None,
        }),
        registry: Registry::new(2, 1).unwrap(),
    });
    let app = router(state.clone());
    let bad = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/me?streamer_id=11")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    for payload in [
        r#"{"streamer_id":11,"destinations":[{"platform":"twitch","rtmp_url":"rtmps://live.twitch.tv/app","stream_key":"synthetic-target","width":1920,"height":1080}]}"#,
        r#"{"streamer_id":11,"destinations":[{"platform":"twitch","stream_key":"","width":1280,"height":720}]}"#,
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/me/destinations")
                    .header("X-Relay-Auth", "synthetic-api")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let rows=store.query("SELECT stream_key_enc,width,height FROM relay.destinations WHERE streamer_id=11 AND platform='twitch'",&[]).await.unwrap();
    assert_eq!(
        state
            .secrets
            .encryption
            .open(&rows[0].get::<_, Vec<u8>>(0), "destination:11:twitch")
            .unwrap()
            .expose(),
        b"synthetic-target"
    );
    assert_eq!(rows[0].get::<_, i32>(1), 1280);
    assert_eq!(rows[0].get::<_, i32>(2), 720);
    assert_eq!(
        store
            .authenticate_ingest("rsr_00000000000000000000000000000000")
            .await
            .unwrap(),
        11
    );
    let response=app.clone().oneshot(Request::builder().method("PUT").uri("/v1/me/destinations").header("X-Relay-Auth","synthetic-api").header("content-type","application/json").body(Body::from(r#"{"streamer_id":11,"destinations":[{"platform":"twitch","width":640},{"platform":"kick","rtmp_url":"rtmps://user:secret@invalid/"}]}"#)).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        store
            .query(
                "SELECT width FROM relay.destinations WHERE streamer_id=11",
                &[]
            )
            .await
            .unwrap()[0]
            .get::<_, i32>(0),
        1280
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/me?streamer_id=11")
                .header("X-Relay-Auth", "synthetic-api")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["ingest_key"], "rsr_00000000000000000000000000000000");
    assert_eq!(value["srt_hint"], "");
    let mut runtime_config = state.config.clone();
    runtime_config.api_bind = "127.0.0.1:0".parse().unwrap();
    runtime_config.ingest_bind = "127.0.0.1:0".parse().unwrap();
    let runtime_state = Arc::new(ServiceState {
        config: runtime_config,
        store: store.clone(),
        secrets: state.secrets.clone(),
        registry: Registry::new(2, 1).unwrap(),
    });
    let certificates = tls::test_tls();
    assert!(
        certificates
            .certificate_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let identities = Arc::new(std::sync::Mutex::new(Vec::new()));
    let task = tokio::spawn(uplink_service::runtime::serve_with_ready(
        runtime_state.clone(),
        certificates.server,
        Arc::new(Collector(identities.clone())),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    let (api_address, ingest_address) = bound.await.unwrap();
    for iteration in 0..2 {
        let socket = tokio::net::TcpStream::connect(ingest_address)
            .await
            .unwrap();
        let mut peer = tokio_rustls::TlsConnector::from(certificates.client.clone())
            .connect("localhost".try_into().unwrap(), socket)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            rtmp::publish(&mut peer, "rsr_00000000000000000000000000000000").await;
            rtmp::message(&mut peer, 8, 1, &[0xaf, 0, 0x12, 0x10]).await;
            rtmp::stop(&mut peer).await;
            loop {
                if identities.lock().unwrap().len() > iteration
                    && runtime_state.registry.active_count() == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    {
        let ids = identities.lock().unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].session.tenant_id(), 11);
        assert_eq!(ids[1].session.tenant_id(), 11);
        assert_ne!(ids[0].generation, ids[1].generation);
    }
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{api_address}/v1/me/status?streamer_id=11"))
        .header("X-Relay-Auth", "synthetic-api")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(12), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    executable_smoke(&database).await;
    database.stop().await;
}
