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
