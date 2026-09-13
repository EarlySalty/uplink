use chrono::Utc;
use futures::future::BoxFuture;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, NoTls, Row};
use uplink_youtube_live::model::{LiveEinstellungen, Schritt, Sichtbarkeit};
use uplink_youtube_live::store::{PostgresRunStore, RunNeu, RunStore, SqlZugang};

struct TestDb {
    child: tokio::process::Child,
    dir: PathBuf,
}

impl TestDb {
    async fn start() -> Self {
        let mut zufall = [0u8; 8];
        getrandom::fill(&mut zufall).unwrap();
        let dir = PathBuf::from("/tmp").join(format!("uplink-yt-db-{}", hex::encode(zufall)));
        std::fs::create_dir(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let initdb = Command::new("/usr/lib/postgresql/16/bin/initdb")
            .args([
                "--auth=trust",
                "--username=uplink_test",
                "--no-locale",
                "-D",
            ])
            .arg(dir.join("data"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status();
        assert!(
            tokio::time::timeout(Duration::from_secs(20), initdb)
                .await
                .unwrap()
                .unwrap()
                .success(),
            "PostgreSQL-16-Testwerkzeuge sind erforderlich"
        );
        let child = Command::new("/usr/lib/postgresql/16/bin/postgres")
            .arg("-D")
            .arg(dir.join("data"))
            .args(["-h", "", "-k"])
            .arg(&dir)
            .args(["-c", "fsync=off", "-c", "max_connections=10"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self { child, dir }
    }

    async fn verbinden(&self) -> Client {
        let conninfo = format!(
            "host={} user=uplink_test dbname=postgres",
            self.dir.display()
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match tokio_postgres::connect(&conninfo, NoTls).await {
                    Ok((client, verbindung)) => {
                        tokio::spawn(async move {
                            let _ = verbindung.await;
                        });
                        return client;
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(30)).await,
                }
            }
        })
        .await
        .unwrap()
    }

    async fn stop(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

struct PgZugang {
    client: Client,
}

impl SqlZugang for PgZugang {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> BoxFuture<'a, Result<Vec<Row>, &'static str>> {
        Box::pin(async move {
            self.client
                .query(sql, params)
                .await
                .map_err(|_| "Datenbankanfrage fehlgeschlagen.")
        })
    }
}

async fn schema_vorbereiten(client: &Client) {
    client.batch_execute("CREATE SCHEMA relay").await.unwrap();
    client
        .batch_execute("CREATE TABLE relay.users(streamer_id bigint PRIMARY KEY)")
        .await
        .unwrap();
    let migration = include_str!("../../../db/migrations/20260908_youtube_live.sql");
    let umhuellt = format!("DO $uplink_migration$ BEGIN\n{migration}\nEND $uplink_migration$");
    client.batch_execute(&umhuellt).await.unwrap();
    client
        .batch_execute("INSERT INTO relay.users(streamer_id) VALUES (77)")
        .await
        .unwrap();
}

fn einstellung(titel: &str, generation: i64) -> LiveEinstellungen {
    LiveEinstellungen {
        titel: titel.to_owned(),
        sichtbarkeit: Sichtbarkeit::Unlisted,
        auto_start: false,
        auto_stop: true,
        live_freigegeben_at: Some(Utc::now()),
        connection_generation: generation,
    }
}

fn run_neu(generation: i64) -> RunNeu<'static> {
    RunNeu {
        streamer_id: 77,
        channel_id: "UC-kanal",
        connection_generation: generation,
        uplink_session: "sitzung-a",
        titel: "Mein Stream",
        sichtbarkeit: Sichtbarkeit::Unlisted,
        auto_start: false,
        auto_stop: true,
        stream_id: None,
    }
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL-16-Testinstanz."]
async fn postgres_store_deckt_lebenszyklus_ab() {
    let db = TestDb::start().await;
    let client = db.verbinden().await;
    schema_vorbereiten(&client).await;
    let store = PostgresRunStore::neu(Arc::new(PgZugang { client }));

    store
        .einstellungen_speichern(77, "UC-kanal", &einstellung("Titel eins", 3))
        .await
        .unwrap();
    let geladen = store.einstellungen_laden(77).await.unwrap().unwrap();
    assert_eq!(geladen.einstellungen.titel, "Titel eins");
    assert_eq!(geladen.channel_id, "UC-kanal");
    assert_eq!(geladen.stream_id, None);

    store.stream_id_merken(77, 3, "S-alt").await.unwrap();
    assert!(
        store.stream_id_merken(77, 999, "S-x").await.is_err(),
        "falsche Generation trifft keine Einstellung"
    );
    store
        .einstellungen_speichern(77, "UC-kanal", &einstellung("Titel zwei", 4))
        .await
        .unwrap();
    let ueberschrieben = store.einstellungen_laden(77).await.unwrap().unwrap();
    assert_eq!(ueberschrieben.einstellungen.titel, "Titel zwei");
    assert_eq!(ueberschrieben.einstellungen.connection_generation, 4);
    assert_eq!(ueberschrieben.stream_id.as_deref(), Some("S-alt"));

    let run = store.run_anlegen(run_neu(4)).await.unwrap();
    assert_eq!(run.connection_generation, 4);

    let zweiter = store.run_anlegen(run_neu(4)).await;
    assert!(zweiter.is_err(), "Teilindex verhindert zweiten aktiven Run");

    let falsch = store
        .zustand_setzen(run.run_id, 4, &["sendet"], "live")
        .await;
    assert_eq!(falsch, Err("Run wurde zwischenzeitlich geändert."));
    let unveraendert = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(
        unveraendert.zustand,
        uplink_youtube_live::model::RunZustand::Vorbereitung
    );

    assert!(
        store
            .schritt_setzen(run.run_id, 999, Schritt::BroadcastInsert)
            .await
            .is_err(),
        "schritt_setzen lehnt falsche Generation ab"
    );
    store
        .schritt_setzen(run.run_id, 4, Schritt::BroadcastInsert)
        .await
        .unwrap();
    let mit_schritt = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(mit_schritt.schritt, Some(Schritt::BroadcastInsert));
    assert!(mit_schritt.schritt_seit.is_some());
    assert!(
        store.schritt_loeschen(run.run_id, 999).await.is_err(),
        "schritt_loeschen lehnt falsche Generation ab"
    );
    store.schritt_loeschen(run.run_id, 4).await.unwrap();
    let ohne_schritt = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(ohne_schritt.schritt, None);

    assert!(
        store
            .zustand_setzen(run.run_id, 999, &["vorbereitung"], "vorbereitet")
            .await
            .is_err(),
        "CAS lehnt falsche Generation ab"
    );
    store
        .zustand_setzen(run.run_id, 4, &["vorbereitung"], "vorbereitet")
        .await
        .unwrap();
    let vorbereitet = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(
        vorbereitet.zustand,
        uplink_youtube_live::model::RunZustand::Vorbereitet
    );

    assert!(
        store
            .schritt_abschliessen(run.run_id, 999, Some("S9"), None)
            .await
            .is_err(),
        "schritt_abschliessen lehnt falsche Generation ab"
    );
    store
        .schritt_abschliessen(run.run_id, 4, Some("S3"), Some("B1"))
        .await
        .unwrap();
    let nach_abschluss = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(nach_abschluss.schritt, None);
    assert_eq!(nach_abschluss.stream_id.as_deref(), Some("S3"));
    assert_eq!(nach_abschluss.broadcast_id.as_deref(), Some("B1"));

    store.unterbrochen_vermerken(run.run_id, 4).await.unwrap();
    assert!(store.unterbrochen_vermerken(run.run_id, 999).await.is_err());
    let nach_unterbrechung = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert!(nach_unterbrechung.unterbrochen_at.is_some());

    let jetzt = Utc::now();
    store.live_seit_setzen(run.run_id, 4, jetzt).await.unwrap();
    assert!(
        store
            .live_seit_setzen(run.run_id, 999, jetzt)
            .await
            .is_err()
    );
    let mit_live = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert!(mit_live.live_seit.is_some());

    store.start_angefordert_setzen(run.run_id, 4).await.unwrap();
    assert!(
        store
            .start_angefordert_setzen(run.run_id, 999)
            .await
            .is_err()
    );
    let mit_start = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert!(mit_start.start_angefordert_at.is_some());

    assert!(
        store
            .run_schliessen(run.run_id, 999, Some("nutzer_stop"), Some(true), None)
            .await
            .is_err(),
        "run_schliessen lehnt falsche Generation ab"
    );
    store
        .run_schliessen(run.run_id, 4, Some("nutzer_stop"), Some(true), None)
        .await
        .unwrap();
    assert!(store.aktiven_run_laden(77).await.unwrap().is_none());

    let neuer = store.run_anlegen(run_neu(4)).await.unwrap();
    assert_ne!(neuer.run_id, run.run_id);

    db.stop().await;
}
