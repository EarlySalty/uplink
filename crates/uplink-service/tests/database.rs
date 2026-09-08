use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::{sync::Arc, time::Duration};
use tokio::process::Command;
use tower::ServiceExt;
use uplink_service::{
    api::{ServiceState, router},
    config::Config,
    crypto::Secret,
    registry::Registry,
    secrets::ServiceSecrets,
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

#[path = "support/database.rs"]
mod database;
use database::{Database, fixture};
impl database::Database {
    pub(crate) async fn raw(&self) -> (tokio_postgres::Client, tokio::task::JoinHandle<()>) {
        let mut config = tokio_postgres::Config::new();
        config
            .host_path(&self.directory)
            .user("uplink_test")
            .dbname("postgres");
        let (client, driver) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let task = tokio::spawn(async move {
            let _ = driver.await;
        });
        (client, task)
    }
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn dashboard_never_turns_saved_destination_into_active_output() {
    let (database, state) = fixture().await;
    state.store.query("INSERT INTO relay.destinations VALUES(11,'twitch','rtmps://live.twitch.tv/app',$1,true,1920,1080,60,6000)", &[&vec![0u8]]).await.unwrap();
    let response = router(state.clone())
        .oneshot(request("GET", "/v1/me?streamer_id=11", ""))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["public_ingest_url"], state.config.public_ingest_url);
    assert_eq!(body["service_status"], "ready");
    assert!(body["session"].is_null());
    let response = router(state.clone())
        .oneshot(request("GET", "/v1/me/destinations?streamer_id=11", ""))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let output = &body["destinations"][0];
    assert_eq!(output["enabled"], true);
    assert_eq!(output["requested"]["width"], 1920);
    assert_eq!(output["output_state"], "unknown");
    assert!(output["active_profile"].is_null());
    assert_eq!(output["publication_confirmed"], false);
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn dock_rotation_persists_identity_and_preserves_ingest_key() {
    let (database, state) = fixture().await;
    state
        .store
        .query(
            "ALTER TABLE relay.users ADD COLUMN dock_token_hash text",
            &[],
        )
        .await
        .unwrap();
    let before: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let response = router(state.clone())
        .oneshot(request("POST", "/v1/me/dock/rotate?streamer_id=11", ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let row = &state.store.query("SELECT dock_token_hash,dock_token_enc,ingest_key_hash FROM relay.users WHERE streamer_id=11", &[]).await.unwrap()[0];
    let encrypted: Vec<u8> = row.get(1);
    let token = state
        .secrets
        .encryption
        .open(&encrypted, "dock_token:11")
        .unwrap();
    use sha2::Digest;
    assert_eq!(
        row.get::<_, String>(0),
        hex::encode(sha2::Sha256::digest(token.expose()))
    );
    assert_eq!(row.get::<_, String>(2), before);
    assert_eq!(token.expose().len(), 37);
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn admin_admission_is_atomic_preserves_keys_and_requires_distinct_auth() {
    let (database, mut state) = fixture().await;
    state.store.query("ALTER TABLE relay.waitlist ADD COLUMN requested_at timestamptz NOT NULL DEFAULT now(), ADD COLUMN note text",&[]).await.unwrap();
    Arc::get_mut(&mut Arc::get_mut(&mut state).unwrap().secrets)
        .unwrap()
        .admin = Secret::new(b"synthetic-admin".to_vec());
    state
        .store
        .query(
            "INSERT INTO relay.waitlist(streamer_id) VALUES(11),(12)",
            &[],
        )
        .await
        .unwrap();
    let old: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let app = router(state.clone());
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/v1/admin/waitlist", ""))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut listing = request("GET", "/v1/admin/waitlist", "");
    listing
        .headers_mut()
        .insert("X-Relay-Auth", "synthetic-admin".parse().unwrap());
    let response = app.clone().oneshot(listing).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listing: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
    assert_eq!(listing["entries"].as_array().unwrap().len(), 2);
    state
        .store
        .query("INSERT INTO relay.waitlist(streamer_id) VALUES(13)", &[])
        .await
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request("DELETE", "/v1/admin/waitlist/13", ""))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut reject = request("DELETE", "/v1/admin/waitlist/13", "");
    reject
        .headers_mut()
        .insert("X-Relay-Auth", "synthetic-admin".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(reject).await.unwrap().status(),
        StatusCode::OK
    );
    for id in [11, 12] {
        let mut req = request(
            "POST",
            "/v1/admin/users",
            format!("{{\"streamer_id\":{id}}}"),
        );
        req.headers_mut()
            .insert("X-Relay-Auth", "synthetic-admin".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::OK
        );
    }
    let count: i64 = state
        .store
        .query("SELECT count(*) FROM relay.waitlist", &[])
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(count, 0);
    assert_eq!(
        state
            .store
            .query(
                "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
                &[]
            )
            .await
            .unwrap()[0]
            .get::<_, String>(0),
        old
    );
    let rows=state.store.query("SELECT ingest_key_hash,ingest_key_enc FROM relay.users WHERE streamer_id=12 AND enabled=true",&[]).await.unwrap();
    let key = state
        .secrets
        .encryption
        .open(&rows[0].get::<_, Vec<u8>>(1), "ingest_key:12")
        .unwrap();
    use sha2::Digest;
    assert_eq!(
        rows[0].get::<_, String>(0),
        hex::encode(sha2::Sha256::digest(key.expose()))
    );
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn profile_disconnect_waits_for_source_end_and_reconnect_setting_stays_a_wish() {
    let (database, state) = fixture().await;
    state.store.query("INSERT INTO relay.destinations VALUES(11,'twitch','rtmps://live.twitch.tv/app',$1,true,1920,1080,60,6000)",&[&vec![0u8]]).await.unwrap();
    let app = router(state.clone());
    let active = state.registry.reserve(11).unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "DELETE",
                "/v1/me/destinations/twitch?streamer_id=11",
                ""
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    drop(active);
    assert_eq!(
        app.clone()
            .oneshot(request(
                "DELETE",
                "/v1/me/destinations/twitch?streamer_id=11",
                ""
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = app
        .oneshot(request(
            "PUT",
            "/v1/me/reconnect-wait?streamer_id=11",
            r#"{"reconnect_wait_s":123}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["reconnect_wait_s"], 123);
    assert_eq!(body["applied"], false);
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz und lokale HTTP-/Websocket-Verbindungen."]
async fn stored_docks_survive_hub_restart_and_rotation_revokes_open_socket() {
    use futures::{StreamExt, future::BoxFuture};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use uplink_chat::{BrokerError, ChatConfig, ChatHub, Grant, Platform, PlatformBroker};
    struct Disconnected;
    impl PlatformBroker for Disconnected {
        fn grant(&self, _: u64, _: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
            Box::pin(async { Err(BrokerError::Disconnected) })
        }
    }
    let (database, mut state) = fixture().await;
    state
        .store
        .query(
            "ALTER TABLE relay.users ADD COLUMN dock_token_hash text",
            &[],
        )
        .await
        .unwrap();
    let response = router(state.clone())
        .oneshot(request(
            "POST",
            "/v1/me/dock-token/rotate?streamer_id=11",
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let encrypted: Vec<u8> = state
        .store
        .query(
            "SELECT dock_token_enc FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let token = state
        .secrets
        .encryption
        .open(&encrypted, "dock_token:11")
        .unwrap();
    let token = std::str::from_utf8(token.expose()).unwrap();
    let make_hub = || {
        ChatHub::new(
            ChatConfig::default(),
            Arc::new(uplink_service::chat::StoredDockIdentity(
                state.store.clone(),
            )),
            Arc::new(Disconnected),
        )
        .unwrap()
    };
    let first_hub = make_hub();
    first_hub.shutdown().await;
    let hub = make_hub();
    Arc::get_mut(&mut state).unwrap().chat = Some(hub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .get(format!("http://{addr}/dock/chat"))
        .query(&[("t", token)])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["Cache-Control"], "no-store");
    let mut request = format!("ws://{addr}/v1/chat/ws?t={token}")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Origin",
        "https://deutsche-deadlock-community.de".parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert!(socket.next().await.unwrap().unwrap().is_text());
    let response = client
        .post(format!(
            "http://{addr}/v1/me/dock-token/rotate?streamer_id=11"
        ))
        .header("X-Relay-Auth", "synthetic-api")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(message)) = socket.next().await {
            if message.is_close() {
                break;
            }
        }
    })
    .await
    .expect("Rotation muss bereits offene Verbindung sofort schließen");
    assert_eq!(
        client
            .get(format!("http://{addr}/dock/chat"))
            .query(&[("t", token)])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Neuer persistenter Token wird nach einer Nutzersperre ebenfalls abgelehnt.
    state
        .store
        .query(
            "UPDATE relay.users SET enabled=false WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    let response = client
        .get(format!("http://{addr}/v1/me?streamer_id=11"))
        .header("X-Relay-Auth", "synthetic-api")
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["ingest_key"], "");
    assert!(body["dock_urls"].is_null());
    hub.shutdown().await;
    server.abort();
    let _ = server.await;
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz und lokales TLS."]
async fn isolated_ingest_accepts_only_test_identity_and_cannot_write_or_publish() {
    use uplink_ingest::Authorizer;
    let (database, mut state) = fixture().await;
    Arc::get_mut(&mut state).unwrap().config = Config::parse(&format!(
        "{}\n[test_ingest]\nallowed_streamer_ids = [11]\n",
        include_str!("../../../config/uplink-beispiel.toml")
    ))
    .unwrap();
    {
        let config = &mut Arc::get_mut(&mut state).unwrap().config;
        config.api_bind = "127.0.0.1:0".parse().unwrap();
        config.ingest_bind = "127.0.0.1:0".parse().unwrap();
        config.media.ffmpeg = "/usr/bin/ffmpeg".into();
        config.media.ffprobe = "/usr/bin/ffprobe".into();
        config.media.work_directory = database.directory.join("media");
    }
    use sha2::Digest;
    let other_key = "rsr_11111111111111111111111111111111";
    state
        .store
        .query(
            "INSERT INTO relay.users(streamer_id,enabled,ingest_key_hash) VALUES(12,true,$1)",
            &[&hex::encode(sha2::Sha256::digest(other_key))],
        )
        .await
        .unwrap();
    let authorizer = uplink_service::runtime::ServiceAuthorizer::new(state.clone());
    assert!(authorizer.authorize("live", other_key).await.is_err());
    for (method, path, body) in [
        ("POST", "/v1/me/key/rotate?streamer_id=11", ""),
        ("POST", "/v1/me/waitlist?streamer_id=11", ""),
        (
            "PUT",
            "/v1/me/destinations",
            r#"{"streamer_id":11,"destinations":[]}"#,
        ),
    ] {
        let response = router(state.clone())
            .oneshot(request(method, path, body))
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ));
    }
    let response = router(state.clone())
        .oneshot(request("GET", "/v1/me/status?streamer_id=12", ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let certificates = tls::test_tls();
    let processor = Arc::new(uplink_service::media::Coordinator::new(state.clone()).unwrap());
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        processor,
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    let (_, address) = bound.await.unwrap();
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut peer = tokio_rustls::TlsConnector::from(certificates.client)
        .connect("localhost".try_into().unwrap(), socket)
        .await
        .unwrap();
    rtmp::publish(&mut peer, "rsr_00000000000000000000000000000000").await;
    rtmp::message(&mut peer, 8, 1, &[0xaf, 0, 0x11, 0x90]).await;
    rtmp::message(&mut peer, 8, 1, &[0xaf, 1, 1, 2, 3]).await;
    rtmp::stop(&mut peer).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state.registry.active_count() == 0 && !state.registry.status(11).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = &state.registry.status(11)[0];
    assert!(status.error.is_none());
    assert_eq!(status.received_events, 2);
    assert_eq!(
        status.source_observation.as_ref().unwrap()["mode"],
        "ingest_test"
    );
    assert_eq!(status.outputs.as_ref().unwrap()["publishing"], false);
    assert_eq!(
        state
            .store
            .query("SELECT count(*) FROM relay.destinations", &[])
            .await
            .unwrap()[0]
            .get::<_, i64>(0),
        0
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    database.stop().await;
}

fn request(method: &str, uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Relay-Auth", "synthetic-api")
        .header("content-type", "application/json")
        .body(body.into())
        .unwrap()
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn opposite_multi_target_updates_use_one_lock_order() {
    let (database, state) = fixture().await;
    let key = state
        .secrets
        .encryption
        .seal(b"synthetic-key", "destination:11:twitch")
        .unwrap();
    state.store.query("INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled,width,height) VALUES(11,'twitch','rtmps://publish.invalid/app',$1,true,1920,1080),(11,'kick','rtmps://publish.invalid/app',$1,true,1920,1080)",&[&key]).await.unwrap();
    // Ein kurzer Trigger hält die jeweils erste Zeilensperre lange genug,
    // damit gegensinnige Reihenfolgen tatsächlich aufeinandertreffen.
    state.store.query("CREATE FUNCTION relay.hold_target_lock() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN PERFORM pg_sleep(0.1); RETURN NEW; END'",&[]).await.unwrap();
    state.store.query("CREATE TRIGGER hold_target_lock BEFORE UPDATE ON relay.destinations FOR EACH ROW EXECUTE FUNCTION relay.hold_target_lock()",&[]).await.unwrap();
    let (observer, driver) = database.raw().await;
    observer
        .batch_execute("ALTER DATABASE postgres SET deadlock_timeout='100ms'")
        .await
        .unwrap();
    let gate = Arc::new(tokio::sync::Barrier::new(3));
    let app = router(state.clone());
    let mut tasks = Vec::new();
    for payload in [
        r#"{"streamer_id":11,"destinations":[{"platform":"twitch","width":1280},{"platform":"kick","width":1280}]}"#,
        r#"{"streamer_id":11,"destinations":[{"platform":"kick","height":720},{"platform":"twitch","height":720}]}"#,
    ] {
        let gate = gate.clone();
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            gate.wait().await;
            app.oneshot(request("PUT", "/v1/me/destinations", payload))
                .await
                .unwrap()
                .status()
        }));
    }
    gate.wait().await;
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    let rows = state
        .store
        .query(
            "SELECT width,height FROM relay.destinations ORDER BY platform",
            &[],
        )
        .await
        .unwrap();
    let deadlocks: i64 = observer
        .query_one(
            "SELECT deadlocks FROM pg_stat_database WHERE datname=current_database()",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    drop(observer);
    driver.await.unwrap();
    database.stop().await;
    assert_eq!(
        statuses,
        vec![StatusCode::OK; 2],
        "Gegensinnige Mehrziel-Updates dürfen keinen Deadlock erzeugen"
    );
    assert_eq!(deadlocks, 0);
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.get::<_, i32>(0), 1280);
        assert_eq!(row.get::<_, i32>(1), 720);
    }
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz und lokales TCP."]
async fn closed_tcp_client_cannot_leave_a_late_key_rotation() {
    use tokio::io::AsyncWriteExt;
    let (database, state) = fixture().await;
    let before: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let (mut locker, driver) = database.raw().await;
    let transaction = locker.transaction().await.unwrap();
    transaction
        .query(
            "SELECT streamer_id FROM relay.users WHERE streamer_id=11 FOR UPDATE",
            &[],
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    client.write_all(b"POST /v1/me/key/rotate?streamer_id=11 HTTP/1.1\r\nHost: localhost\r\nX-Relay-Auth: synthetic-api\r\nContent-Length: 0\r\n\r\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            transaction.batch_execute("SELECT pg_stat_clear_snapshot()").await.unwrap();
            if transaction.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND pid<>pg_backend_pid())",&[]).await.unwrap().get::<_,bool>(0) {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    drop(client);
    // Der echte TCP-Abbruch muss spätestens mit Anfragefrist und Nachlauf die
    // wartende Datenbankarbeit beenden. Sofortige TCP-Cancellation ist damit
    // ausdrücklich nicht behauptet.
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            transaction.batch_execute("SELECT pg_stat_clear_snapshot()").await.unwrap();
            let active:i64=transaction.query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid()",&[]).await.unwrap().get(0);
            if active==0 {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    transaction.commit().await.unwrap();
    let after: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    server.abort();
    let _ = server.await;
    drop(locker);
    driver.await.unwrap();
    database.stop().await;
    assert_eq!(after, before);
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn concurrent_profile_patch_keeps_key_committed_while_waiting() {
    let (database, state) = fixture().await;
    let old = state
        .secrets
        .encryption
        .seal(b"old-key", "destination:11:twitch")
        .unwrap();
    let new = state
        .secrets
        .encryption
        .seal(b"new-key", "destination:11:twitch")
        .unwrap();
    state.store.query("INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled,width) VALUES(11,'twitch','rtmps://live.twitch.tv/app',$1,true,1920)", &[&old]).await.unwrap();
    let (mut locker, driver) = database.raw().await;
    let transaction = locker.transaction().await.unwrap();
    transaction
        .execute(
            "UPDATE relay.destinations SET stream_key_enc=$1 WHERE streamer_id=11",
            &[&new],
        )
        .await
        .unwrap();
    let app = router(state.clone());
    let patch = tokio::spawn(app.oneshot(request(
        "PUT",
        "/v1/me/destinations",
        r#"{"streamer_id":11,"destinations":[{"platform":"twitch","width":1280}]}"#,
    )));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            transaction.batch_execute("SELECT pg_stat_clear_snapshot()").await.unwrap();
            if transaction.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND pid<>pg_backend_pid())", &[]).await.unwrap().get::<_,bool>(0) {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(patch.await.unwrap().unwrap().status(), StatusCode::OK);
    let row = state
        .store
        .query(
            "SELECT stream_key_enc,width FROM relay.destinations WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .secrets
            .encryption
            .open(&row[0].get::<_, Vec<u8>>(0), "destination:11:twitch")
            .unwrap()
            .expose(),
        b"new-key"
    );
    assert_eq!(row[0].get::<_, i32>(1), 1280);
    drop(locker);
    driver.await.unwrap();
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn timed_out_key_rotation_cannot_commit_after_lock_release() {
    let (database, state) = fixture().await;
    let before: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let (mut locker, driver) = database.raw().await;
    let transaction = locker.transaction().await.unwrap();
    transaction
        .query(
            "SELECT streamer_id FROM relay.users WHERE streamer_id=11 FOR UPDATE",
            &[],
        )
        .await
        .unwrap();
    let response = router(state.clone())
        .oneshot(request(
            "POST",
            "/v1/me/key/rotate?streamer_id=11",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert!(response.status().is_server_error());
    transaction.commit().await.unwrap();
    let rows = tokio::time::timeout(
        Duration::from_millis(500),
        state.store.query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        ),
    )
    .await
    .expect("Keine festhängende vorherige Query")
    .unwrap();
    assert_eq!(
        rows[0].get::<_, String>(0),
        before,
        "Abgelaufene Rotation darf nicht später schreiben"
    );
    drop(locker);
    driver.await.unwrap();
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn cancelled_http_request_cannot_commit_a_late_rotation() {
    let (database, state) = fixture().await;
    let before: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let (mut locker, driver) = database.raw().await;
    let transaction = locker.transaction().await.unwrap();
    transaction
        .query(
            "SELECT streamer_id FROM relay.users WHERE streamer_id=11 FOR UPDATE",
            &[],
        )
        .await
        .unwrap();
    let pending = tokio::spawn(router(state.clone()).oneshot(request(
        "POST",
        "/v1/me/key/rotate?streamer_id=11",
        Body::empty(),
    )));
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            transaction.batch_execute("SELECT pg_stat_clear_snapshot()").await.unwrap();
            if transaction.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND pid<>pg_backend_pid())",&[]).await.unwrap().get::<_,bool>(0) {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    pending.abort();
    let _ = pending.await;
    transaction.commit().await.unwrap();
    // Warten bis die abgebrochene Transaktion abgewickelt wurde, nicht nur auf
    // einen MVCC-Snapshot schauen, während eine spätere Mutation noch wartet.
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            let count:i64=locker.query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid()",&[]).await.unwrap().get(0);
            if count==0 {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let after: String = state
        .store
        .query(
            "SELECT ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(after, before);
    drop(locker);
    driver.await.unwrap();
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn unsafe_legacy_endpoint_is_redacted_and_other_targets_remain_visible() {
    let (database, state) = fixture().await;
    let ciphertext = state
        .secrets
        .encryption
        .seal(b"synthetic-key", "destination:11:twitch")
        .unwrap();
    state.store.query("INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled) VALUES(11,'twitch','rtmps://publish.invalid/app/synthetic-private-key',$1,true),(11,'youtube','rtmps://a.rtmps.youtube.com/live2',$1,true)",&[&ciphertext]).await.unwrap();
    let response = router(state.clone())
        .oneshot(request(
            "GET",
            "/v1/me/destinations?streamer_id=11",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("synthetic-private-key"));
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["destinations"].as_array().unwrap().len(), 2);
    assert_eq!(value["destinations"][0]["platform"], "twitch");
    assert_eq!(value["destinations"][0]["blocked"], true);
    assert_eq!(value["destinations"][0]["rtmp_url"], "");
    assert!(value["destinations"][0]["error"].is_string());
    assert_eq!(value["destinations"][1]["blocked"], false);
    database.stop().await;
    let response = router(state.clone())
        .oneshot(request(
            "GET",
            "/v1/me/status?streamer_id=11",
            Body::empty(),
        ))
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["service_status"],
        "unavailable"
    );
    let response = router(state)
        .oneshot(request("GET", "/v1/health", Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["ok"],
        false
    );
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn waitlist_write_is_visible_for_new_and_existing_users() {
    let (database, state) = fixture().await;
    let app = router(state);
    for tenant in [11, 99] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/me/waitlist?streamer_id={tenant}"),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/v1/me?streamer_id={tenant}"),
                Body::empty(),
            ))
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
        assert_eq!(value["waitlisted"], true);
    }
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz und lokales TLS."]
async fn zero_media_sessions_keep_authorized_end_reasons_and_reject_anonymous_changes() {
    use tokio::io::AsyncReadExt;
    let (database, state) = fixture().await;
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let identities = Arc::new(std::sync::Mutex::new(Vec::new()));
    let task = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        Arc::new(Collector(identities.clone())),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    let (api, address) = bound.await.unwrap();
    let mut previous = 0;
    for (mode, expected, failed) in [
        ("truncated", "MediaRejected(Truncated)", true),
        ("stop", "ExplicitStop", false),
        ("disconnect", "PeerClosed", true),
    ] {
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut peer = tokio_rustls::TlsConnector::from(certificates.client.clone())
            .connect("localhost".try_into().unwrap(), socket)
            .await
            .unwrap();
        rtmp::publish(&mut peer, "rsr_00000000000000000000000000000000").await;
        match mode {
            "truncated" => rtmp::message(&mut peer, 8, 1, &[0xaf]).await,
            "stop" => rtmp::stop(&mut peer).await,
            _ => {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while state.registry.active_count() != 1 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
        }
        if mode != "disconnect" {
            let mut response = Vec::new();
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                (&mut peer).take(16 * 1024).read_to_end(&mut response),
            )
            .await
            .unwrap();
        }
        drop(peer);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let statuses = state.registry.status(11);
                if state.registry.active_count() == 0
                    && statuses.first().is_some_and(|status| status.id != previous)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{api}/v1/me/status?streamer_id=11"))
            .header("X-Relay-Auth", "synthetic-api")
            .send()
            .await
            .unwrap();
        let value: serde_json::Value = response.json().await.unwrap();
        let status = &value["sessions"][0];
        assert_eq!(status["received_events"], 0);
        assert_eq!(
            status["ingest_end_reason"], expected,
            "Abschluss ohne MediaEvent muss zugeordnet bleiben"
        );
        assert_eq!(status["error"].is_string(), failed);
        assert_eq!(status["state"], if failed { "Fehler" } else { "Beendet" });
        previous = status["id"].as_u64().unwrap();
    }
    let before = serde_json::to_value(state.registry.status(11)).unwrap();
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut peer = tokio_rustls::TlsConnector::from(certificates.client)
        .connect("localhost".try_into().unwrap(), socket)
        .await
        .unwrap();
    rtmp::publish(&mut peer, "rsr_11111111111111111111111111111111").await;
    let mut response = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        peer.take(16 * 1024).read_to_end(&mut response),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(state.registry.status(11)).unwrap(),
        before
    );
    assert!(state.registry.status(99).is_empty());
    assert_eq!(state.registry.active_count(), 0);
    assert!(identities.lock().unwrap().is_empty());
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz und lokales TLS."]
async fn rejected_media_after_valid_prelude_stays_failed_in_session_status() {
    let (database, state) = fixture().await;
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        Arc::new(Collector(Arc::new(std::sync::Mutex::new(Vec::new())))),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    let (_, address) = bound.await.unwrap();
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let mut peer = tokio_rustls::TlsConnector::from(certificates.client)
        .connect("localhost".try_into().unwrap(), socket)
        .await
        .unwrap();
    rtmp::publish(&mut peer, "rsr_00000000000000000000000000000000").await;
    rtmp::message(&mut peer, 8, 1, &[0xaf, 0, 0x12, 0x10]).await;
    rtmp::message(&mut peer, 8, 1, &[0xaf]).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state.registry.active_count() == 0 && !state.registry.status(11).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = state.registry.status(11);
    assert!(
        status[0].error.is_some(),
        "Ein Prozessor-Ok darf MediaRejected nicht überschreiben"
    );
    let value = serde_json::to_value(&status[0]).unwrap();
    assert_eq!(value["ingest_end_reason"], "MediaRejected(Truncated)");
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    database.stop().await;
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
    let public_ca = database.directory.join("public-test-ca.pem");
    std::fs::write(&public_ca, certificates.cert.pem()).unwrap();
    let reply = serde_json::json!({"secrets":[
        {"secretKey":"RS_RELAY_API_SECRET","secretValue":"synthetic-api"},
        {"secretKey":"RS_RELAY_ADMIN_SECRET","secretValue":"synthetic-admin"},
        {"secretKey":"RS_RELAY_KEY_ENC","secretValue":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7; 32])},
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
    let mut config: toml::Value =
        toml::from_str(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    config.as_table_mut().unwrap().insert(
        "loopback_test_ca".into(),
        toml::Value::String(public_ca.to_str().unwrap().into()),
    );
    config["api_bind"] = toml::Value::String("127.0.0.1:0".into());
    config["ingest_bind"] = toml::Value::String("127.0.0.1:0".into());
    config["public_ingest_url"] = toml::Value::String("rtmps://localhost/live".into());
    config["infisical"]["base_url"] = toml::Value::String(format!("http://{address}"));
    config["infisical"]["credential_fd"] = toml::Value::Integer(identity.as_raw_fd().into());
    config["media"]["ffmpeg"] = toml::Value::String("/usr/bin/ffmpeg".into());
    config["media"]["ffprobe"] = toml::Value::String("/usr/bin/ffprobe".into());
    config["media"]["work_directory"] =
        toml::Value::String(database.directory.join("media").to_str().unwrap().into());
    let config_path = database.directory.join("service.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
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
        chat: None,
        tls: None,
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
        chat: None,
        tls: None,
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

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn admission_repairs_absent_credentials_without_replacing_valid_keys() {
    let (database, mut state) = fixture().await;
    Arc::get_mut(&mut Arc::get_mut(&mut state).unwrap().secrets)
        .unwrap()
        .admin = Secret::new(b"synthetic-admin".to_vec());
    state
        .store
        .query(
            "INSERT INTO relay.users(streamer_id,enabled) VALUES(12,false)",
            &[],
        )
        .await
        .unwrap();
    state
        .store
        .query("INSERT INTO relay.waitlist VALUES(12)", &[])
        .await
        .unwrap();
    state
        .store
        .query(
            "UPDATE relay.users SET ingest_key_hash=NULL WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    let app = router(state.clone());
    for id in [12_i64, 11] {
        let mut req = request(
            "POST",
            "/v1/admin/users",
            format!("{{\"streamer_id\":{id}}}"),
        );
        req.headers_mut()
            .insert("X-Relay-Auth", "synthetic-admin".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::OK
        );
        let rows = state.store.query("SELECT enabled,ingest_key_hash,ingest_key_enc FROM relay.users WHERE streamer_id=$1", &[&id]).await.unwrap();
        let key = state
            .secrets
            .encryption
            .open(&rows[0].get::<_, Vec<u8>>(2), &format!("ingest_key:{id}"))
            .unwrap();
        use sha2::Digest;
        assert!(rows[0].get::<_, bool>(0));
        assert_eq!(
            rows[0].get::<_, String>(1),
            hex::encode(sha2::Sha256::digest(key.expose()))
        );
        if id == 11 {
            assert_eq!(key.expose(), b"rsr_00000000000000000000000000000000");
        }
    }
    assert_eq!(
        state
            .store
            .query("SELECT count(*) FROM relay.waitlist", &[])
            .await
            .unwrap()[0]
            .get::<_, i64>(0),
        0
    );
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn admission_rejects_corrupt_credentials_before_enabling_or_removing_waitlist() {
    let (database, mut state) = fixture().await;
    Arc::get_mut(&mut Arc::get_mut(&mut state).unwrap().secrets)
        .unwrap()
        .admin = Secret::new(b"synthetic-admin".to_vec());
    state
        .store
        .query(
            "UPDATE relay.users SET enabled=false,ingest_key_enc=$1 WHERE streamer_id=11",
            &[&vec![2_u8, 0]],
        )
        .await
        .unwrap();
    state
        .store
        .query("INSERT INTO relay.waitlist VALUES(11)", &[])
        .await
        .unwrap();
    let mut req = request("POST", "/v1/admin/users", "{\"streamer_id\":11}");
    req.headers_mut()
        .insert("X-Relay-Auth", "synthetic-admin".parse().unwrap());
    assert_eq!(
        router(state.clone()).oneshot(req).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        !state
            .store
            .query("SELECT enabled FROM relay.users WHERE streamer_id=11", &[])
            .await
            .unwrap()[0]
            .get::<_, bool>(0)
    );
    assert_eq!(
        state
            .store
            .query("SELECT count(*) FROM relay.waitlist", &[])
            .await
            .unwrap()[0]
            .get::<_, i64>(0),
        1
    );
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn me_never_reports_a_healthy_enabled_user_without_matching_credentials() {
    let (database, state) = fixture().await;
    let app = router(state.clone());
    let current = state
        .store
        .query(
            "SELECT ingest_key_enc,ingest_key_hash FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    let encrypted: Vec<u8> = current[0].get(0);
    let hash: String = current[0].get(1);
    let invalid_shape = state
        .secrets
        .encryption
        .seal(b"not-a-stream-key", "ingest_key:11")
        .unwrap();
    use sha2::Digest;
    for (encrypted, hash) in [
        (Some(encrypted.clone()), None),
        (None, Some(hash.clone())),
        (Some(vec![2_u8, 0]), Some(hash)),
        (Some(encrypted), Some("0".repeat(64))),
        (
            Some(invalid_shape),
            Some(hex::encode(sha2::Sha256::digest(b"not-a-stream-key"))),
        ),
    ] {
        state
            .store
            .query(
                "UPDATE relay.users SET ingest_key_enc=$1,ingest_key_hash=$2 WHERE streamer_id=11",
                &[&encrypted, &hash],
            )
            .await
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(request("GET", "/v1/me?streamer_id=11", ""))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz mit konkurrierenden Verbindungen."]
async fn admission_does_not_overwrite_a_key_rotated_after_validation() {
    let (database, mut state) = fixture().await;
    Arc::get_mut(&mut Arc::get_mut(&mut state).unwrap().secrets)
        .unwrap()
        .admin = Secret::new(b"synthetic-admin".to_vec());
    state
        .store
        .query("INSERT INTO relay.waitlist VALUES(11)", &[])
        .await
        .unwrap();
    let (mut locker, driver) = database.raw().await;
    let transaction = locker.transaction().await.unwrap();
    let rotated = b"rsr_11111111111111111111111111111111";
    let encrypted = state
        .secrets
        .encryption
        .seal(rotated, "ingest_key:11")
        .unwrap();
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(rotated));
    transaction
        .execute(
            "UPDATE relay.users SET ingest_key_hash=$1,ingest_key_enc=$2 WHERE streamer_id=11",
            &[&hash, &encrypted],
        )
        .await
        .unwrap();
    let mut req = request("POST", "/v1/admin/users", "{\"streamer_id\":11}");
    req.headers_mut()
        .insert("X-Relay-Auth", "synthetic-admin".parse().unwrap());
    let app = router(state.clone());
    let admission = tokio::spawn(async move { app.oneshot(req).await.unwrap() });
    tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            transaction.batch_execute("SELECT pg_stat_clear_snapshot()").await.unwrap();
            if transaction.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND pid<>pg_backend_pid())",&[]).await.unwrap().get::<_,bool>(0) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(admission.await.unwrap().status(), StatusCode::CONFLICT);
    let rows = state
        .store
        .query(
            "SELECT ingest_key_hash,ingest_key_enc FROM relay.users WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, String>(0), hash);
    assert_eq!(rows[0].get::<_, Vec<u8>>(1), encrypted);
    assert_eq!(
        state
            .store
            .query("SELECT count(*) FROM relay.waitlist", &[])
            .await
            .unwrap()[0]
            .get::<_, i64>(0),
        1
    );
    drop(locker);
    driver.await.unwrap();
    database.stop().await;
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL und tatsächliche 10-s-Laufzeitprüfung."]
async fn review_transient_database_error_recovers_active_chat_user() {
    use futures::future::BoxFuture;
    use uplink_chat::{BrokerError, DockIdentity, DockUser, Grant, Platform, PlatformBroker};
    struct Offline;
    impl DockIdentity for Offline {
        fn resolve(&self, _: [u8; 32]) -> BoxFuture<'_, Result<Option<DockUser>, BrokerError>> {
            Box::pin(async { Ok(None) })
        }
    }
    impl PlatformBroker for Offline {
        fn grant(&self, _: u64, _: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
            Box::pin(async { Err(BrokerError::Disconnected) })
        }
    }
    let (database, mut state) = fixture().await;
    let hub = uplink_chat::ChatHub::new(
        uplink_chat::ChatConfig::default(),
        Arc::new(Offline),
        Arc::new(Offline),
    )
    .unwrap();
    Arc::get_mut(&mut state).unwrap().chat = Some(hub.clone());
    let reservation = state.registry.reserve(11).unwrap();
    hub.set_active(11, true).unwrap();
    let generation = hub.identity_checks()[0].generation.clone();
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        Arc::new(Collector(Arc::new(std::sync::Mutex::new(Vec::new())))),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    bound.await.unwrap();
    state
        .store
        .query("ALTER TABLE relay.users RENAME TO users_fault", &[])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !hub
            .status_for(11)
            .unwrap()
            .iter()
            .all(|status| status.zustand == "access_unconfirmed")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.registry.active_count(), 1);
    state
        .store
        .query("ALTER TABLE relay.users_fault RENAME TO users", &[])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(12)).await;
    let recovered = hub
        .identity_checks()
        .iter()
        .any(|check| check.streamer_id == 11 && check.generation == generation && check.active)
        && hub
            .status_for(11)
            .unwrap()
            .iter()
            .any(|status| status.zustand != "access_unconfirmed");
    let still_active = state.registry.active_count();
    drop(reservation);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    database.stop().await;
    assert!(
        recovered,
        "The real runtime removed the active chat user after one SQL error and did not recover it; media reservations still active: {still_active}"
    );
}
