use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_postgres::{Row, types::ToSql};
use uplink_vod::{store::MIGRATION, youtube::YouTube, *};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};
struct Db(Arc<tokio::sync::Mutex<tokio_postgres::Client>>);
#[async_trait]
impl Database for Db {
    async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        self.0
            .lock()
            .await
            .query(sql, params)
            .await
            .map_err(|error| {
                if let Some(db) = error.as_db_error() {
                    eprintln!("Testdatenbank: {} {}", db.code().code(), db.message());
                }
                Error::Database
            })
    }
}
struct Fixture {
    child: tokio::process::Child,
    _dir: tempfile::TempDir,
    db: Arc<Db>,
    driver: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let init = tokio::process::Command::new("/usr/lib/postgresql/16/bin/initdb")
            .args(["--auth=trust", "--username=vod_test", "--no-locale", "-D"])
            .arg(dir.path().join("data"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status()
            .await
            .unwrap();
        assert!(init.success());
        let child = tokio::process::Command::new("/usr/lib/postgresql/16/bin/postgres")
            .arg("-D")
            .arg(dir.path().join("data"))
            .args(["-h", "", "-k"])
            .arg(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (client, driver) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut config = tokio_postgres::Config::new();
                config
                    .host_path(dir.path())
                    .user("vod_test")
                    .dbname("postgres");
                if let Ok(pair) = config.connect(tokio_postgres::NoTls).await {
                    break pair;
                }
                tokio::time::sleep(Duration::from_millis(30)).await
            }
        })
        .await
        .unwrap();
        let driver = tokio::spawn(async {
            let _ = driver.await;
        });
        client.batch_execute("CREATE SCHEMA relay; CREATE TABLE relay.users(streamer_id bigint PRIMARY KEY); CREATE TABLE relay.sessions(id bigint PRIMARY KEY,streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),ended_at timestamptz); INSERT INTO relay.users VALUES(7),(8); INSERT INTO relay.sessions(id,streamer_id) VALUES(1,7),(2,7),(3,8);").await.unwrap();
        client.batch_execute(MIGRATION).await.unwrap();
        Self {
            child,
            _dir: dir,
            db: Arc::new(Db(Arc::new(tokio::sync::Mutex::new(client)))),
            driver,
        }
    }
    async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.driver.abort();
    }
    async fn finish_session(&self, session: i64, streamer: u64, commit: bool) {
        let mut client = self.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        tx.execute(
            "UPDATE relay.sessions SET ended_at=now() WHERE id=$1",
            &[&session],
        )
        .await
        .unwrap();
        Store::enqueue_in_transaction(
            &tx,
            &LogicalSessionEnded {
                session_id: session,
                streamer_id: streamer,
                ended_at: Utc::now(),
                reason: EndReason::ExplicitStop,
            },
        )
        .await
        .unwrap();
        if commit {
            tx.commit().await.unwrap()
        } else {
            tx.rollback().await.unwrap()
        }
    }
    async fn due(&self) {
        self.db.query("UPDATE relay.vod_jobs SET next_attempt_at=now(),lease_until=CASE WHEN lease_owner IS NULL THEN NULL ELSE now()-interval '1 second' END",&[]).await.unwrap();
    }
}
fn settings() -> Settings {
    Settings {
        enabled: true,
        source: Source::InputRecording,
        youtube_channel_id: "UC-owner".into(),
        vod_audio_track: Some(1),
        privacy: Privacy::Private,
        publication_authorized: false,
        title: "Testaufnahme".into(),
        description: String::new(),
    }
}
#[tokio::test]
async fn abschluss_ist_atomar_idempotent_und_mandantengebunden() {
    let f = Fixture::new().await;
    let store = Store::new(f.db.clone());
    store.save_settings(7, &settings()).await.unwrap();
    f.finish_session(1, 7, false).await;
    assert!(store.statuses(7).await.unwrap().is_empty());
    f.finish_session(1, 7, true).await;
    f.finish_session(1, 7, true).await;
    assert_eq!(store.statuses(7).await.unwrap().len(), 1);
    f.finish_session(3, 7, true).await;
    assert_eq!(store.statuses(7).await.unwrap().len(), 1);
    f.stop().await;
}
struct Broker;
#[async_trait]
impl PlatformBroker for Broker {
    async fn grant(&self, _: u64, _: Platform, _: bool) -> Result<Grant> {
        Ok(Grant {
            access_token: Secret::new("test-grant".into()),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            platform_user_id: "UC-owner".into(),
            scopes: vec!["https://www.googleapis.com/auth/youtube.force-ssl".into()],
        })
    }
}
struct Protected;
impl Cipher for Protected {
    fn seal(&self, plain: &[u8], aad: &str) -> Result<Vec<u8>> {
        let mut bytes = aad.as_bytes().to_vec();
        bytes.push(0);
        bytes.extend(plain.iter().map(|b| b ^ 0x5a));
        Ok(bytes)
    }
    fn open(&self, cipher: &[u8], aad: &str) -> Result<Secret> {
        let prefix = [aad.as_bytes(), &[0]].concat();
        let body = cipher
            .strip_prefix(prefix.as_slice())
            .ok_or(Error::Invalid)?;
        Ok(Secret::new(
            String::from_utf8(body.iter().map(|b| b ^ 0x5a).collect())
                .map_err(|_| Error::Invalid)?,
        ))
    }
}
struct Ready {
    storage: Storage,
    object: String,
}
#[async_trait]
impl SourceProvider for Ready {
    async fn prepare(&self, _: SourceRequest) -> Result<PreparedSource> {
        Ok(PreparedSource {
            object_id: self.object.clone(),
            manifest: json!({"source":"isolierter_test"}),
            export: self.storage.digest(&self.object, "export.mp4").await?,
        })
    }
}
#[tokio::test]
async fn neustart_gleicht_angenommene_bytes_ab_und_loescht_erst_nach_processing() {
    let f = Fixture::new().await;
    let store = Store::new(f.db.clone());
    store.save_settings(7, &settings()).await.unwrap();
    f.finish_session(1, 7, true).await;
    let dir = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(dir.path().to_owned(), 8 * 1024 * 1024).unwrap();
    let object = storage.create_object().unwrap();
    let data = vec![42u8; 1024 * 1024 + 7];
    tokio::fs::write(storage.path(&object, "export.mp4").unwrap(), &data)
        .await
        .unwrap();
    storage.reserve(data.len() as u64).unwrap().commit();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/channels"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"UC-owner"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(200).insert_header(
            "Location",
            format!(
                "{}/upload/youtube/v3/videos?upload_id=private-session",
                server.uri()
            ),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let accepted = Arc::new(AtomicU64::new(0));
    let delivered = Arc::new(AtomicU64::new(0));
    let a = accepted.clone();
    let d = delivered.clone();
    let total = data.len() as u64;
    Mock::given(method("PUT"))
        .and(path("/upload/youtube/v3/videos"))
        .respond_with(move |r: &Request| {
            if r.body.is_empty() {
                let n = a.load(Ordering::SeqCst);
                if n == 0 {
                    return ResponseTemplate::new(308);
                }
                return ResponseTemplate::new(308)
                    .insert_header("Range", format!("bytes=0-{}", n - 1));
            }
            let count = d.fetch_add(r.body.len() as u64, Ordering::SeqCst) + r.body.len() as u64;
            a.fetch_add(r.body.len() as u64, Ordering::SeqCst);
            if count < total {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"id":"video-1"}))
            }
        })
        .mount(&server)
        .await;
    let checks = Arc::new(AtomicU64::new(0));
    let checks2 = checks.clone();
    Mock::given(method("GET")).and(path("/youtube/v3/videos")).respond_with(move|_:&Request|ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"video-1","snippet":{"channelId":"UC-owner"},"processingDetails":{"processingStatus":if checks2.fetch_add(1,Ordering::SeqCst)==0{"processing"}else{"succeeded"}}}]}))).mount(&server).await;
    let youtube = Arc::new(YouTube::for_test(Arc::new(Broker), &server.uri()).unwrap());
    let source = Arc::new(Ready {
        storage: storage.clone(),
        object: object.clone(),
    });
    let cipher = Arc::new(Protected);
    let worker = Worker::new(
        store.clone(),
        storage.clone(),
        source.clone(),
        youtube.clone(),
        cipher.clone(),
    );
    worker.run_one().await.unwrap();
    worker.run_one().await.unwrap();
    worker.run_one().await.unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 1024 * 1024);
    assert!(storage.path(&object, "export.mp4").unwrap().exists());
    let ciphertext: Vec<u8> =
        f.db.query("SELECT upload_session_enc FROM relay.vod_jobs", &[])
            .await
            .unwrap()[0]
            .get(0);
    assert!(!String::from_utf8_lossy(&ciphertext).contains("upload_id"));
    drop(worker);
    f.due().await;
    let restarted = Worker::new(store.clone(), storage.clone(), source, youtube, cipher);
    restarted.run_one().await.unwrap();
    assert_eq!(delivered.load(Ordering::SeqCst), total);
    restarted.run_one().await.unwrap();
    assert_eq!(store.statuses(7).await.unwrap()[0].state, "processing");
    assert!(storage.path(&object, "export.mp4").unwrap().exists());
    f.due().await;
    restarted.run_one().await.unwrap();
    assert_eq!(store.statuses(7).await.unwrap()[0].state, "ready");
    assert!(storage.path(&object, "export.mp4").unwrap().exists());
    f.finish_session(2, 7, true).await;
    f.db.query(
        "UPDATE relay.vod_jobs SET object_id=$1,state='blocked' WHERE session_id=2",
        &[&object],
    )
    .await
    .unwrap();
    assert!(
        !restarted.cleanup_one().await.unwrap(),
        "Ein anderer offener Auftrag benötigt die Quelle noch"
    );
    assert!(storage.path(&object, "export.mp4").unwrap().exists());
    f.db.query("UPDATE relay.vod_jobs SET state='ready',video_id='video-2',processing_succeeded=true,total_bytes=$1,confirmed_bytes=$1 WHERE session_id=2",&[&(total as i64)]).await.unwrap();
    assert!(restarted.cleanup_one().await.unwrap());
    assert!(!dir.path().join(&object).exists());
    server.verify().await;
    f.stop().await;
}

#[tokio::test]
async fn geaenderte_twitch_stream_id_macht_die_quellbindung_dauerhaft_mehrdeutig() {
    let fixture = Fixture::new().await;
    for stream in ["555", "556"] {
        let mut client = fixture.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: stream.into(),
                vod_audio_confirmed: true,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let rows = fixture
        .db
        .query(
            "SELECT conflicted FROM relay.vod_twitch_bindings WHERE session_id=1",
            &[],
        )
        .await
        .unwrap();
    assert!(
        rows[0].get::<_, bool>(0),
        "Ein einzelnes Teil-VOD darf nicht als ganzer logischer Stream gelten"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn verlorene_twitch_vod_audiofreigabe_bleibt_dauerhaft_gesperrt() {
    let fixture = Fixture::new().await;
    for confirmed in [true, false, true] {
        let mut client = fixture.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: confirmed,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let rows = fixture
        .db
        .query(
            "SELECT binding,conflicted FROM relay.vod_twitch_bindings WHERE session_id=1",
            &[],
        )
        .await
        .unwrap();
    let binding: TwitchBinding = serde_json::from_value(rows[0].get(0)).unwrap();
    assert!(!rows[0].get::<_, bool>(1));
    assert_eq!(binding.validate(), Err(Error::MissingVodAudio));
    fixture.stop().await;
}
