use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::process::Command;
use uplink_service::{
    api::ServiceState, config::Config, crypto::Secret, registry::Registry, secrets::ServiceSecrets,
    store::Store,
};
pub(crate) struct Database {
    child: tokio::process::Child,
    pub(crate) directory: PathBuf,
}
impl Database {
    pub(crate) async fn start() -> Self {
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
    pub(crate) async fn connect(&self) -> Store {
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
    pub(crate) async fn stop(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

pub(crate) async fn fixture() -> (Database, Arc<ServiceState>) {
    let database = Database::start().await;
    let store = Arc::new(database.connect().await);
    for sql in [
        "CREATE SCHEMA relay",
        "CREATE TABLE relay.users(streamer_id bigint PRIMARY KEY,enabled boolean NOT NULL,ingest_key_enc bytea,dock_token_enc bytea,ingest_key_hash text,reconnect_wait_s integer NOT NULL DEFAULT 0)",
        "CREATE TABLE relay.destinations(streamer_id bigint REFERENCES relay.users(streamer_id),platform text NOT NULL,rtmp_url text NOT NULL,stream_key_enc bytea NOT NULL,enabled boolean NOT NULL,width integer,height integer,fps integer,bitrate_kbps integer,UNIQUE(streamer_id,platform))",
        "CREATE TABLE relay.waitlist(streamer_id bigint PRIMARY KEY)",
        "CREATE TABLE relay.sessions(id bigserial PRIMARY KEY,streamer_id bigint NOT NULL,started_at timestamptz NOT NULL,ended_at timestamptz,ingest_protocol text NOT NULL,ingest_codec text,profile_json jsonb NOT NULL,end_reason text)",
        include_str!("../../../../db/migrations/20260908_destination_fences.sql"),
        include_str!("../../../../db/migrations/20260908_twitch_audio_mode.sql"),
    ] {
        store.query(sql, &[]).await.unwrap();
    }
    let encryption = Secret::new(vec![7; 32]);
    let key = encryption
        .seal(b"rsr_00000000000000000000000000000000", "ingest_key:11")
        .unwrap();
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(
        b"rsr_00000000000000000000000000000000",
    ));
    store.query("INSERT INTO relay.users(streamer_id,enabled,ingest_key_enc,ingest_key_hash) VALUES(11,true,$1,$2)", &[&key,&hash]).await.unwrap();
    let mut config =
        Config::parse(include_str!("../../../../config/uplink-beispiel.toml")).unwrap();
    config.api_bind = "127.0.0.1:0".parse().unwrap();
    config.ingest_bind = "127.0.0.1:0".parse().unwrap();
    config.request_timeout_seconds = 1;
    let state = Arc::new(ServiceState {
        chat: None,
        tls: None,
        config,
        store,
        secrets: Arc::new(ServiceSecrets {
            api: Secret::new(b"synthetic-api".to_vec()),
            admin: Secret::new(vec![]),
            database: Secret::new(vec![]),
            bot_internal: Secret::new(vec![]),
            encryption,
            tls_material: None,
        }),
        registry: Registry::new(2, 1).unwrap(),
    });
    (database, state)
}
