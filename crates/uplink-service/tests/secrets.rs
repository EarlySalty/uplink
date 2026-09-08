use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use std::{
    io::{Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use uplink_service::{config::Config, secrets::fetch};

#[tokio::test]
async fn bootstrap_protects_all_injected_fds_before_the_first_read() {
    let descriptors: Vec<_> = (0..3)
        .map(|_| {
            memfd::MemfdOptions::default()
                .close_on_exec(false)
                .create("synthetic-bootstrap-and-tls")
                .unwrap()
        })
        .collect();
    let mut config = Config::parse(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    config.infisical.credential_fd = descriptors[0].as_raw_fd() as u32;
    config.tls = uplink_service::config::TlsConfig::Fds {
        certificate_fd: descriptors[1].as_raw_fd() as u32,
        private_key_fd: descriptors[2].as_raw_fd() as u32,
    };
    uplink_service::secrets::protect_configured_fds(&config).unwrap();
    for descriptor in descriptors {
        let status = tokio::process::Command::new("/usr/bin/test")
            .arg("-e")
            .arg(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))
            .status()
            .await
            .unwrap();
        assert!(!status.success());
    }
}

#[tokio::test]
async fn inherited_bootstrap_and_tls_fds_do_not_reach_child_processes() {
    for material in [
        b"synthetic-bootstrap".as_slice(),
        b"synthetic-tls-certificate",
        b"synthetic-tls-private-key",
    ] {
        let memory = memfd::MemfdOptions::default()
            .close_on_exec(false)
            .create("synthetic-private-material")
            .unwrap();
        memory.as_file().write_all(material).unwrap();
        memory.as_file().seek(SeekFrom::Start(0)).unwrap();
        let fd = memory.as_raw_fd();
        let loaded = uplink_service::secrets::read_fd(fd as u32, 1024)
            .await
            .unwrap();
        assert!(loaded.matches(material));
        let status = tokio::process::Command::new("/usr/bin/test")
            .arg("-e")
            .arg(format!("/proc/self/fd/{fd}"))
            .status()
            .await
            .unwrap();
        assert!(
            !status.success(),
            "Autorisierter Original-FD wurde an Kindprozess vererbt"
        );
    }
}

#[tokio::test]
async fn infisical_reads_only_supplied_memory_fd_and_never_follows_redirects() {
    let memory = memfd::MemfdOptions::default()
        .create("synthetic-infisical-test")
        .unwrap();
    memory
        .as_file()
        .write_all(b"synthetic-service-identity\n")
        .unwrap();
    memory.as_file().seek(SeekFrom::Start(0)).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let router=Router::new().route("/api/v4/secrets/",get(move|headers:HeaderMap|{
        let count=count.clone();
        async move{
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(headers.get("Authorization").unwrap(),"Bearer synthetic-service-identity");
            Json(serde_json::json!({"secrets":[
                {"secretKey":"RS_RELAY_API_SECRET","secretValue":"synthetic-api"},
                {"secretKey":"RS_RELAY_ADMIN_SECRET","secretValue":"synthetic-admin"},
                {"secretKey":"RS_RELAY_KEY_ENC","secretValue":"0707070707070707070707070707070707070707070707070707070707070707"},
                {"secretKey":"RS_RELAY_DATABASE_URL","secretValue":"synthetic-database"},
                {"secretKey":"RS_RELAY_BOT_INTERNAL_TOKEN","secretValue":"synthetic-broker"},
                {"secretKey":"UPLINK_TLS_CERTIFICATE","secretValue":"synthetic-certificate"},
                {"secretKey":"UPLINK_TLS_PRIVATE_KEY","secretValue":"synthetic-key"}
            ]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
    let mut config = Config::parse(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    config.infisical.credential_fd = memory.as_raw_fd() as u32;
    config.infisical.base_url = format!("http://{address}");
    let secrets = fetch(&config).await.unwrap();
    assert!(secrets.api.matches(b"synthetic-api"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
    let target_count = Arc::new(AtomicUsize::new(0));
    let count = target_count.clone();
    let app = Router::new()
        .route(
            "/api/v4/secrets/",
            get(|| async { (StatusCode::FOUND, [("Location", "/forbidden")]) }),
        )
        .route(
            "/forbidden",
            get(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.infisical.base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    assert!(fetch(&config).await.is_err());
    assert_eq!(target_count.load(Ordering::SeqCst), 0);
    server.abort();
    let _ = server.await;
}
