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
    async fn transaction(&self, task: &mut (dyn TransactionTask + Send)) -> Result<()> {
        let mut client = tokio::time::timeout(Duration::from_secs(5), self.0.lock())
            .await
            .map_err(|_| Error::Database)?;
        let tx = client.transaction().await.map_err(|_| Error::Database)?;
        tx.batch_execute("SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='10s'")
            .await
            .map_err(|_| Error::Database)?;
        match task.run(&tx).await {
            Ok(()) => tx.commit().await.map_err(|_| Error::Database),
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }
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
    async fn another_client(&self) -> (tokio_postgres::Client, tokio::task::JoinHandle<()>) {
        let mut config = tokio_postgres::Config::new();
        config
            .host_path(self._dir.path())
            .user("vod_test")
            .dbname("postgres");
        let (client, driver) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let driver = tokio::spawn(async move {
            let _ = driver.await;
        });
        (client, driver)
    }
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

fn private_storage() -> (tempfile::TempDir, Storage) {
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(directory.path().to_owned(), 65536).unwrap();
    (directory, storage)
}
async fn twitch_job(
    fixture: &Fixture,
    storage: &Storage,
    state: &str,
    uri: &str,
) -> (Store, String) {
    twitch_job_with_privacy(fixture, storage, state, uri, Privacy::Private).await
}
async fn twitch_job_with_privacy(
    fixture: &Fixture,
    storage: &Storage,
    state: &str,
    uri: &str,
    privacy: Privacy,
) -> (Store, String) {
    let store = Store::new(fixture.db.clone());
    let mut config = settings();
    config.source = Source::TwitchVod;
    config.privacy = privacy;
    config.publication_authorized = privacy != Privacy::Private;
    store.save_settings(7, &config).await.unwrap();
    {
        let mut client = fixture.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: true,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    fixture.finish_session(1, 7, true).await;
    let object = storage.create_object().unwrap();
    let data = b"synthetic export bytes";
    tokio::fs::write(storage.path(&object, "export.mp4").unwrap(), data)
        .await
        .unwrap();
    storage.reserve(data.len() as u64).unwrap().commit();
    let export = storage.digest(&object, "export.mp4").await.unwrap();
    store
        .register_object(1, 7, &object, &json!({"export":export}), true)
        .await
        .unwrap();
    fixture
        .db
        .query(
            "UPDATE relay.vod_objects SET source='twitch_vod' WHERE id=$1",
            &[&object],
        )
        .await
        .unwrap();
    let sealed = Protected
        .seal(
            uri.as_bytes(),
            "uplink-vod:streamer:7:job:1:upload-session:v1",
        )
        .unwrap();
    fixture.db.query("UPDATE relay.vod_jobs SET state=$1,object_id=$2,total_bytes=$3::bigint,confirmed_bytes=CASE WHEN $1='ready' THEN $3::bigint ELSE 0::bigint END,video_id=CASE WHEN $1='ready' THEN 'video-1' ELSE NULL END,processing_succeeded=($1='ready'),upload_session_enc=$4 WHERE session_id=1",&[&state,&object,&(data.len() as i64),&sealed]).await.unwrap();
    (store, object)
}
async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut header = vec![];
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") && header.len() < 8192 {
        socket.read_exact(&mut byte).await.unwrap();
        header.push(byte[0]);
    }
    let text = String::from_utf8(header).unwrap();
    let len = text
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(len <= 4096);
    let mut body = vec![0; len];
    socket.read_exact(&mut body).await.unwrap();
    body
}
#[tokio::test]
async fn bindungswiderruf_wartet_auf_uploadwirkung_und_sperrt_danach_den_job() {
    use tokio::io::AsyncWriteExt;
    let fixture = Fixture::new().await;
    let (_directory, storage) = private_storage();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (store, object) = twitch_job(
        &fixture,
        &storage,
        "uploading",
        &format!("{endpoint}/upload/youtube/v3/videos?upload_id=protected"),
    )
    .await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    let http_entered = entered.clone();
    let http_proceed = proceed.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(read_request(&mut socket).await.is_empty());
        socket
            .write_all(
                b"HTTP/1.1 308 Resume Incomplete\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        drop(socket);
        let (mut socket, _) = listener.accept().await.unwrap();
        assert_eq!(read_request(&mut socket).await, b"synthetic export bytes");
        http_entered.notify_one();
        http_proceed.notified().await;
        let body = r#"{"id":"video-1"}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let worker = Arc::new(Worker::new(
        store.clone(),
        storage.clone(),
        Arc::new(Ready {
            storage: storage.clone(),
            object: object.clone(),
        }),
        Arc::new(YouTube::for_test(Arc::new(Broker), &endpoint).unwrap()),
        Arc::new(Protected),
    ));
    let running = worker.clone();
    let task = tokio::spawn(async move { running.run_one().await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let (mut client, driver) = fixture.another_client().await;
    let writer_started = Arc::new(tokio::sync::Notify::new());
    let started = writer_started.clone();
    let mut writer = tokio::spawn(async move {
        let tx = client.transaction().await.unwrap();
        started.notify_one();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: false,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    writer_started.notified().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut writer)
            .await
            .is_err(),
        "Widerruf darf nicht mitten im freigegebenen HTTP-Schritt committen"
    );
    proceed.notify_one();
    writer.await.unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Ok(true) | Err(Error::LeaseLost)
    ));
    server.await.unwrap();
    let status = store.statuses(7).await.unwrap();
    assert_eq!(status[0].state, "blocked");
    assert_eq!(status[0].last_error.as_deref(), Some("missing_vod_audio"));
    assert!(store.retry(7, 1).await.is_err());
    assert!(!worker.cleanup_one().await.unwrap());
    assert!(storage.path(&object, "export.mp4").unwrap().exists());
    assert!(
        fixture
            .db
            .query(
                "UPDATE relay.vod_jobs SET proof_blocked=false,state='prepared' WHERE id=1",
                &[]
            )
            .await
            .is_err()
    );
    driver.abort();
    fixture.stop().await;
}
#[tokio::test]
async fn loeschung_wartet_auf_konkurrierenden_bindungsentscheid() {
    for commit_revocation in [true, false] {
        let fixture = Fixture::new().await;
        let (_directory, storage) = private_storage();
        let (store, object) =
            twitch_job(&fixture, &storage, "ready", "protected-test-session").await;
        let claimed = Arc::new(tokio::sync::Notify::new());
        let proceed = Arc::new(tokio::sync::Notify::new());
        let paused = Arc::new(CleanupPauseDb {
            db: fixture.db.clone(),
            claimed: claimed.clone(),
            proceed: proceed.clone(),
        });
        let worker = Arc::new(Worker::new(
            Store::new(paused),
            storage.clone(),
            Arc::new(Ready {
                storage: storage.clone(),
                object: object.clone(),
            }),
            Arc::new(YouTube::for_test(Arc::new(Broker), "http://127.0.0.1:9").unwrap()),
            Arc::new(Protected),
        ));
        let running = worker.clone();
        let mut cleanup = tokio::spawn(async move { running.cleanup_one().await });
        tokio::time::timeout(Duration::from_secs(5), claimed.notified())
            .await
            .unwrap();
        let (mut client, driver) = fixture.another_client().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: false,
            },
        )
        .await
        .unwrap();
        proceed.notify_one();
        let waiting = tokio::time::timeout(Duration::from_millis(50), &mut cleanup).await;
        assert!(
            waiting.is_err(),
            "Löschung muss den offenen Bindungsentscheid abwarten: {waiting:?}"
        );
        assert!(storage.path(&object, "export.mp4").unwrap().exists());
        if commit_revocation {
            tx.commit().await.unwrap();
            assert_eq!(cleanup.await.unwrap(), Err(Error::LeaseLost));
            assert!(storage.path(&object, "export.mp4").unwrap().exists());
            assert_eq!(store.statuses(7).await.unwrap()[0].state, "blocked");
        } else {
            tx.rollback().await.unwrap();
            assert_eq!(cleanup.await.unwrap(), Ok(true));
            assert!(!storage.object_dir(&object).is_ok());
            assert_eq!(store.statuses(7).await.unwrap()[0].state, "ready");
        }
        driver.abort();
        fixture.stop().await;
    }
}
struct CleanupPauseDb {
    db: Arc<Db>,
    claimed: Arc<tokio::sync::Notify>,
    proceed: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl Database for CleanupPauseDb {
    async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        let rows = self.db.query(sql, params).await?;
        if sql.contains("UPDATE relay.vod_objects o SET state='deleting'") && !rows.is_empty() {
            self.claimed.notify_one();
            self.proceed.notified().await;
        }
        Ok(rows)
    }
    async fn transaction(&self, task: &mut (dyn TransactionTask + Send)) -> Result<()> {
        self.db.transaction(task).await
    }
}

#[tokio::test]
async fn oeffentliche_freigabe_folgt_processing_und_bleibt_nach_widerruf_gesperrt() {
    for revoke in [false, true] {
        let fixture = Fixture::new().await;
        let (_directory, storage) = private_storage();
        let (store, object) = twitch_job_with_privacy(
            &fixture,
            &storage,
            "processing",
            "protected-test-session",
            Privacy::Public,
        )
        .await;
        fixture.db.query("UPDATE relay.vod_jobs SET confirmed_bytes=total_bytes,video_id='video-1' WHERE id=1",&[]).await.unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/youtube/v3/videos")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"video-1","snippet":{"channelId":"UC-owner"},"processingDetails":{"processingStatus":"succeeded"},"status":{"privacyStatus":"private","embeddable":false}}]}))).mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/youtube/v3/videos"))
            .and(wiremock::matchers::body_json(
                json!({"id":"video-1","status":{"privacyStatus":"public","embeddable":false}}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":"video-1","status":{"privacyStatus":"public"}})),
            )
            .expect(if revoke { 0 } else { 1 })
            .mount(&server)
            .await;
        let worker = Worker::new(
            store.clone(),
            storage.clone(),
            Arc::new(Ready {
                storage: storage.clone(),
                object: object.clone(),
            }),
            Arc::new(YouTube::for_test(Arc::new(Broker), &server.uri()).unwrap()),
            Arc::new(Protected),
        );
        worker.run_one().await.unwrap();
        assert_eq!(store.statuses(7).await.unwrap()[0].state, "publishing");
        if revoke {
            let mut client = fixture.db.0.lock().await;
            let tx = client.transaction().await.unwrap();
            Store::bind_twitch_stream(
                &tx,
                1,
                7,
                &TwitchBinding {
                    broadcaster_id: "123".into(),
                    stream_id: "555".into(),
                    vod_audio_confirmed: false,
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        worker.run_one().await.unwrap();
        assert_eq!(
            store.statuses(7).await.unwrap()[0].state,
            if revoke { "blocked" } else { "ready" }
        );
        let confirmed = fixture
            .db
            .query(
                "SELECT publication_confirmed FROM relay.vod_jobs WHERE id=1",
                &[],
            )
            .await
            .unwrap()[0]
            .get::<_, bool>(0);
        assert_eq!(confirmed, !revoke);
        assert!(storage.path(&object, "export.mp4").unwrap().exists());
        server.verify().await;
        fixture.stop().await;
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
struct DelayedTwitchSource {
    storage: Storage,
    object: String,
    entered: Arc<tokio::sync::Notify>,
    proceed: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl SourceProvider for DelayedTwitchSource {
    async fn prepare(&self, request: SourceRequest) -> Result<PreparedSource> {
        let captured_binding = request.twitch_binding.ok_or(Error::UnboundTwitch)?;
        captured_binding.validate()?;
        self.entered.notify_one();
        self.proceed.notified().await;
        Ok(PreparedSource {
            object_id: self.object.clone(),
            manifest: json!({"binding":captured_binding}),
            export: self.storage.digest(&self.object, "export.mp4").await?,
        })
    }
}
#[tokio::test]
async fn review_late_vod_audio_revocation_is_ignored_by_prepared_transition() {
    let fixture = Fixture::new().await;
    let store = Store::new(fixture.db.clone());
    let mut cfg = settings();
    cfg.source = Source::TwitchVod;
    store.save_settings(7, &cfg).await.unwrap();
    {
        let mut client = fixture.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: true,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    fixture.finish_session(1, 7, true).await;
    let dir = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(dir.path().to_owned(), 65536).unwrap();
    let object = storage.create_object().unwrap();
    tokio::fs::write(
        storage.path(&object, "export.mp4").unwrap(),
        b"synthetic export bytes",
    )
    .await
    .unwrap();
    storage.reserve(22).unwrap().commit();
    let entered = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    let source = Arc::new(DelayedTwitchSource {
        storage: storage.clone(),
        object,
        entered: entered.clone(),
        proceed: proceed.clone(),
    });
    let youtube = Arc::new(YouTube::for_test(Arc::new(Broker), "http://127.0.0.1:9").unwrap());
    let worker = Worker::new(store.clone(), storage, source, youtube, Arc::new(Protected));
    let task = tokio::spawn(async move { worker.run_one().await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    {
        let mut client = fixture.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: false,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    proceed.notify_one();
    assert!(matches!(
        task.await.unwrap(),
        Ok(true) | Err(Error::LeaseLost)
    ));
    let binding: serde_json::Value = fixture
        .db
        .query(
            "SELECT binding FROM relay.vod_twitch_bindings WHERE session_id=1",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    let status = store.statuses(7).await.unwrap();
    assert_eq!(binding["vod_audio_confirmed"], false);
    assert_eq!(
        status[0].state, "blocked",
        "Ein alter Audionachweis darf keine vorbereitete Ausgabe freigeben"
    );
    assert_eq!(status[0].last_error.as_deref(), Some("missing_vod_audio"));
    fixture.stop().await;
}

#[tokio::test]
async fn ready_braucht_eine_bestaetigte_gesamtgroesse_und_mandantenindizes() {
    let fixture = Fixture::new().await;
    let store = Store::new(fixture.db.clone());
    store.save_settings(7, &settings()).await.unwrap();
    fixture.finish_session(1, 7, true).await;
    assert!(fixture.db.query("UPDATE relay.vod_jobs SET state='ready',video_id='video',processing_succeeded=true,total_bytes=NULL WHERE session_id=1",&[]).await.is_err());
    let indexes=fixture.db.query("SELECT indexname FROM pg_indexes WHERE schemaname='relay' AND indexname IN ('vod_jobs_streamer_id','vod_objects_streamer_id','vod_twitch_bindings_streamer_id')",&[]).await.unwrap();
    assert_eq!(indexes.len(), 3);
    fixture
        .db
        .0
        .lock()
        .await
        .batch_execute(MIGRATION)
        .await
        .unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn unklarer_start_bleibt_gesperrt_bekannte_session_wird_nur_wiederaufgenommen() {
    let fixture = Fixture::new().await;
    let store = Store::new(fixture.db.clone());
    store.save_settings(7, &settings()).await.unwrap();
    fixture.finish_session(1, 7, true).await;
    fixture.finish_session(2, 7, true).await;
    fixture.db.query("UPDATE relay.vod_jobs SET state='blocked',last_error='ambiguous',resume_state=CASE WHEN session_id=1 THEN 'starting' ELSE 'uploading' END,upload_session_enc=CASE WHEN session_id=2 THEN decode('aa','hex') ELSE NULL END",&[]).await.unwrap();
    assert_eq!(store.retry(7, 1).await, Err(Error::Invalid));
    store.retry(7, 2).await.unwrap();
    let rows = fixture
        .db
        .query(
            "SELECT state,upload_session_enc FROM relay.vod_jobs WHERE session_id=2",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, String>(0), "uploading");
    assert_eq!(rows[0].get::<_, Vec<u8>>(1), vec![0xaa]);
    fixture.stop().await;
}

struct TwitchBroker;
#[async_trait]
impl PlatformBroker for TwitchBroker {
    async fn grant(&self, _: u64, _: Platform, _: bool) -> Result<Grant> {
        Ok(Grant {
            access_token: Secret::new("synthetischer-zugang".into()),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            platform_user_id: "123".into(),
            scopes: vec![],
        })
    }
}
#[tokio::test]
async fn twitch_quelle_ersetzt_keine_vollstaendige_eingangsaufnahme() {
    let fixture = Fixture::new().await;
    let store = Store::new(fixture.db.clone());
    let mut settings = settings();
    settings.source = Source::TwitchVod;
    store.save_settings(7, &settings).await.unwrap();
    {
        let mut client = fixture.db.0.lock().await;
        let tx = client.transaction().await.unwrap();
        Store::bind_twitch_stream(
            &tx,
            1,
            7,
            &TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: true,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    fixture.finish_session(1, 7, true).await;
    let directory = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(directory.path().to_owned(), 65536).unwrap();
    let input = storage.create_object().unwrap();
    store
        .register_object(
            1,
            7,
            &input,
            &json!({"complete":true,"input_marker":"must_stay"}),
            true,
        )
        .await
        .unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/helix/streams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/helix/videos")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"222","stream_id":"555","user_id":"123","type":"archive","viewable":"public","duration":"1h"}],"pagination":{}}))).mount(&server).await;
    let sources = NativeSources::new(
        store.clone(),
        storage.clone(),
        Arc::new(TwitchBroker),
        SourceConfig {
            ffmpeg: "/bin/true".into(),
            ffprobe: "/bin/true".into(),
            yt_dlp: "/bin/true".into(),
            twitch_client_id: "public-test-client".into(),
            max_source_bytes: 4096,
            max_export_bytes: 4096,
            download_bytes_per_second: 1024,
            operation_timeout: Duration::from_secs(10),
            twitch_settle_time: Duration::from_secs(60),
        },
    )
    .unwrap()
    .with_loopback_test_api(&format!("{}/helix/", server.uri()))
    .unwrap();
    let worker = Worker::new(
        store.clone(),
        storage,
        Arc::new(sources),
        Arc::new(YouTube::for_test(Arc::new(Broker), &server.uri()).unwrap()),
        Arc::new(Protected),
    );
    worker.run_one().await.unwrap();
    assert_eq!(
        store.statuses(7).await.unwrap()[0].last_error.as_deref(),
        Some("source_pending")
    );
    let objects = fixture
        .db
        .query(
            "SELECT manifest FROM relay.vod_objects WHERE session_id=1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(objects.len(), 2);
    assert!(
        objects
            .iter()
            .any(|row| row.get::<_, serde_json::Value>(0)["input_marker"] == "must_stay")
    );
    fixture.stop().await;
}
