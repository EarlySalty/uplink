use std::{sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
#[path = "support/database.rs"]
mod database;
#[path = "support/rtmp.rs"]
mod rtmp;
#[path = "../../uplink-ingest/tests/support/tls.rs"]
#[allow(dead_code)]
mod tls;

struct RejectOutput;
impl uplink_service::runtime::SessionProcessor for RejectOutput {
    async fn process(
        &self,
        first: uplink_ingest::MediaEvent,
        _: tokio::sync::mpsc::Receiver<uplink_ingest::MediaEvent>,
    ) -> Result<(), &'static str> {
        first.authorization_retention().unwrap().downcast::<uplink_service::registry::Reservation>().unwrap()
            .media_diagnostic(serde_json::json!({"phase":"prepare","error":"UnsupportedProfile","probe":{"pix_fmt":"nv12"}}));
        Err("Kein Ausgang konnte gestartet werden.")
    }
}

async fn completed_connections(
    write_failure: bool,
) -> (Vec<serde_json::Value>, Result<(), &'static str>) {
    completed_connections_with_delay(write_failure, false).await
}

async fn completed_connections_with_delay(
    write_failure: bool,
    delayed_inserts: bool,
) -> (Vec<serde_json::Value>, Result<(), &'static str>) {
    let (database, state) = database::fixture().await;
    if delayed_inserts {
        state.store.query("CREATE FUNCTION relay.delayed_session() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN PERFORM pg_sleep(9); RETURN NEW; END'", &[]).await.unwrap();
        state.store.query("CREATE TRIGGER delayed_session BEFORE INSERT ON relay.sessions FOR EACH ROW EXECUTE FUNCTION relay.delayed_session()", &[]).await.unwrap();
    }
    if write_failure {
        state
            .store
            .query(
                "ALTER TABLE relay.sessions ADD CONSTRAINT reject_insert CHECK(false)",
                &[],
            )
            .await
            .unwrap();
    }
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let service = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        Arc::new(RejectOutput),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    let (_, address) = bound.await.unwrap();
    for (index, mode) in ["truncated", "consumer", "stop"].iter().enumerate() {
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut peer = tokio_rustls::TlsConnector::from(certificates.client.clone())
            .connect("localhost".try_into().unwrap(), socket)
            .await
            .unwrap();
        rtmp::publish(&mut peer, "rsr_00000000000000000000000000000000").await;
        match *mode {
            "truncated" => rtmp::message(&mut peer, 8, 1, &[0xaf]).await,
            "consumer" => {
                rtmp::message(&mut peer, 8, 1, &[0xaf, 0, 0x11, 0x90]).await;
            }
            _ => rtmp::stop(&mut peer).await,
        }
        let mut reply = Vec::new();
        let _ = tokio::time::timeout(
            Duration::from_secs(4),
            (&mut peer).take(16 * 1024).read_to_end(&mut reply),
        )
        .await
        .unwrap();
        drop(peer);
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if state.registry.active_count() == 0
                    && state
                        .registry
                        .status(11)
                        .first()
                        .is_some_and(|status| status.id == (index + 1) as u64)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let status = &state.registry.status(11)[0];
        assert!(
            !status
                .ingest_end_reason
                .as_deref()
                .unwrap()
                .contains("Diagnose")
        );
        assert!(
            !status
                .error
                .unwrap_or_default()
                .contains("UnsupportedProfile")
        );
    }
    stop.send(()).unwrap();
    let stopped = service.await.unwrap();
    let rows = state.store.query("SELECT jsonb_build_object('streamer_id',streamer_id,'end_reason',end_reason,'profile',profile_json,'duration',extract(epoch from ended_at-started_at),'protocol',ingest_protocol) FROM relay.sessions ORDER BY id", &[]).await.unwrap()
        .iter().map(|row| row.get(0)).collect();
    drop(state);
    database.stop().await;
    (rows, stopped)
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
async fn session_end_persists_once_even_without_started_output() {
    let (rows, stopped) = completed_connections(false).await;
    stopped.unwrap();
    assert_eq!(
        rows.len(),
        3,
        "Jede autorisierte Verbindung braucht genau eine relay.sessions-Zeile, auch ohne gestarteten Ausgang"
    );
    for (row, reason) in rows.iter().zip([
        "MediaRejected(Truncated)",
        "ConsumerClosed: Kein Ausgang konnte gestartet werden.",
        "ExplicitStop",
    ]) {
        assert_eq!(row["streamer_id"], 11);
        if reason.starts_with("ConsumerClosed") {
            assert!(row["end_reason"].as_str().unwrap().starts_with(reason));
            assert!(
                row["end_reason"]
                    .as_str()
                    .unwrap()
                    .contains("UnsupportedProfile")
            );
            assert_eq!(
                row["profile"]["media_diagnostic"]["probe"]["pix_fmt"],
                "nv12"
            );
        } else {
            assert_eq!(row["end_reason"], reason);
        }
        assert_eq!(row["protocol"], "rtmps");
        assert!(
            row["duration"]
                .as_f64()
                .is_some_and(|seconds| seconds >= 0.0)
        );
    }
}

#[tokio::test]
#[ignore = "Interner Prozess für die Journalerfassung mit isoliertem PostgreSQL 16."]
async fn journal_probe() {
    completed_connections(false).await.1.unwrap();
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
async fn failed_session_insert_is_reported_by_service_shutdown() {
    let (rows, stopped) = completed_connections(true).await;
    assert!(rows.is_empty());
    assert_eq!(
        stopped,
        Err("Mindestens ein Streamabschluss konnte nicht gespeichert werden.")
    );
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
async fn shutdown_persists_an_authorized_connection_without_media() {
    let (database, state) = database::fixture().await;
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let service = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        Arc::new(RejectOutput),
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
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.registry.active_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let rows = state
        .store
        .query(
            "SELECT end_reason,profile_json FROM relay.sessions WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get::<_, String>(0).starts_with("ConsumerClosed:"));
    assert_eq!(rows[0].get::<_, serde_json::Value>(1)["source_tracks"], 0);
    assert_eq!(state.registry.active_count(), 0);
    drop(peer);
    drop(state);
    database.stop().await;
}

#[test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
fn session_end_writes_one_safe_journal_line_with_coordinator_reason() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "journal_probe", "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Journalprobe fehlgeschlagen: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("Uplink-Eingang beendet:"))
        .collect();
    assert_eq!(
        lines.len(),
        3,
        "Pro autorisierter Verbindung fehlt genau eine Journal-Abschlusszeile: {stderr}"
    );
    for (line, reason) in lines.iter().zip([
        "MediaRejected(Truncated)",
        "ConsumerClosed: Kein Ausgang konnte gestartet werden.",
        "ExplicitStop",
    ]) {
        assert!(line.contains("streamer_id=11"), "{line}");
        assert!(line.contains("duration_ms="), "{line}");
        assert!(line.contains("source_tracks="), "{line}");
        assert!(line.contains(reason), "{line}");
    }
    assert!(!stderr.contains("rsr_"));
    assert!(!stderr.contains("playpath"));
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
async fn occupied_database_capacity_delays_completion_without_losing_it() {
    let (database, state) = database::fixture().await;
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut service = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        Arc::new(RejectOutput),
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
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.registry.active_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (observer, driver) = tokio_postgres::Config::new()
        .host(database.directory.to_str().unwrap())
        .user("uplink_test")
        .dbname("postgres")
        .connect(tokio_postgres::NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(driver);
    observer
        .query("SELECT pg_advisory_lock(819246)", &[])
        .await
        .unwrap();
    let mut busy = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let store = state.store.clone();
        busy.spawn(async move {
            store
                .query("SELECT pg_advisory_xact_lock(819246)", &[])
                .await
        });
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = observer.query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'", &[]).await.unwrap().get(0);
            if count == 4 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    rtmp::stop(&mut peer).await;
    let mut reply = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        (&mut peer).take(16 * 1024).read_to_end(&mut reply),
    )
    .await
    .unwrap();
    stop.send(()).unwrap();
    let early = tokio::time::timeout(Duration::from_millis(150), &mut service).await;
    observer
        .query("SELECT pg_advisory_unlock(819246)", &[])
        .await
        .unwrap();
    while let Some(result) = busy.join_next().await {
        result.unwrap().unwrap();
    }
    let (waited, result) = match early {
        Ok(result) => (false, result.unwrap()),
        Err(_) => (
            true,
            tokio::time::timeout(Duration::from_secs(3), service)
                .await
                .unwrap()
                .unwrap(),
        ),
    };
    let rows = state
        .store
        .query(
            "SELECT end_reason FROM relay.sessions WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    drop(observer);
    driver.await.unwrap().unwrap();
    drop(peer);
    drop(state);
    database.stop().await;
    assert_eq!(
        rows.len(),
        1,
        "Belegte Store-Kapazität darf den einzigen Sessionabschluss nicht verwerfen"
    );
    assert_eq!(rows[0].get::<_, String>(0), "ExplicitStop");
    assert!(
        waited,
        "Dienststopp muss auf den noch ausstehenden Abschluss warten"
    );
    result.unwrap();
}

struct DelayedCleanup {
    ready: tokio::sync::Notify,
    complete: tokio::sync::Notify,
    keep_receiver: bool,
    delay: Duration,
}
impl uplink_service::runtime::SessionProcessor for DelayedCleanup {
    async fn process(
        &self,
        first: uplink_ingest::MediaEvent,
        events: tokio::sync::mpsc::Receiver<uplink_ingest::MediaEvent>,
    ) -> Result<(), &'static str> {
        let receiver = if self.keep_receiver {
            Some(events)
        } else {
            drop(events);
            None
        };
        let reservation = first
            .authorization_retention()
            .unwrap()
            .downcast::<uplink_service::registry::Reservation>()
            .unwrap();
        self.ready.notify_one();
        self.complete.notified().await;
        tokio::time::sleep(self.delay).await;
        reservation
            .media_diagnostic(serde_json::json!({"phase":"prepare_cleanup","error":"ProbeFailed"}));
        drop(receiver);
        Err("Die Medienprüfung ist nach dem Aufräumen fehlgeschlagen.")
    }
}

async fn delayed_coordinator_case(keep_receiver: bool) {
    let (database, state) = database::fixture().await;
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let processor = Arc::new(DelayedCleanup {
        ready: tokio::sync::Notify::new(),
        complete: tokio::sync::Notify::new(),
        keep_receiver,
        delay: Duration::ZERO,
    });
    let service = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        processor.clone(),
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
    tokio::time::timeout(Duration::from_secs(3), processor.ready.notified())
        .await
        .unwrap();
    let extra = if keep_receiver { 257 } else { 1 };
    for _ in 0..extra {
        rtmp::message(&mut peer, 8, 1, &[0xaf, 0, 0x11, 0x90]).await;
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.registry.status(11)[0].received_events < extra + 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut reply = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        (&mut peer).take(16 * 1024).read_to_end(&mut reply),
    )
    .await
    .unwrap();
    processor.complete.notify_one();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let rows = state
        .store
        .query(
            "SELECT end_reason,profile_json FROM relay.sessions WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    drop(peer);
    drop(state);
    database.stop().await;
    assert_eq!(rows.len(), 1);
    let end_reason: String = rows[0].get(0);
    assert!(
        end_reason.starts_with(
            "ConsumerClosed: Die Medienprüfung ist nach dem Aufräumen fehlgeschlagen."
        ),
        "Finaler Coordinatorfehler wurde ersetzt: {end_reason}"
    );
    assert_eq!(
        rows[0].get::<_, serde_json::Value>(1)["media_diagnostic"]["error"],
        "ProbeFailed"
    );
    assert_eq!(
        rows[0].get::<_, serde_json::Value>(1)["input_backpressure"],
        keep_receiver
    );
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
async fn closed_receiver_preserves_delayed_coordinator_error_and_diagnostic() {
    delayed_coordinator_case(false).await;
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16 und TLS."]
async fn full_receiver_preserves_delayed_coordinator_error_and_diagnostic() {
    delayed_coordinator_case(true).await;
}

async fn long_cleanup_case(shutdown: bool) {
    let (database, state) = database::fixture().await;
    let certificates = tls::test_tls();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let processor = Arc::new(DelayedCleanup {
        ready: tokio::sync::Notify::new(),
        complete: tokio::sync::Notify::new(),
        keep_receiver: false,
        delay: Duration::from_secs(11),
    });
    let service = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        certificates.server,
        processor.clone(),
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
    tokio::time::timeout(Duration::from_secs(3), processor.ready.notified())
        .await
        .unwrap();
    if !shutdown {
        rtmp::stop(&mut peer).await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.registry.status(11)[0].ingest_end_reason.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    processor.complete.notify_one();
    stop.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), service)
        .await
        .unwrap()
        .unwrap();
    let status = state.registry.status(11)[0].clone();
    let rows = state
        .store
        .query(
            "SELECT profile_json FROM relay.sessions WHERE streamer_id=11",
            &[],
        )
        .await
        .unwrap();
    drop(peer);
    drop(state);
    database.stop().await;
    assert_eq!(
        status.error,
        Some("Die Medienprüfung ist nach dem Aufräumen fehlgeschlagen."),
        "Zulässiger Medienabschluss wurde vor seiner endgültigen Diagnose gekappt"
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, serde_json::Value>(0)["media_diagnostic"]["error"],
        "ProbeFailed"
    );
    result.unwrap();
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16, TLS und elf Sekunden echtes Mediencleanup."]
async fn eof_preserves_coordinator_cleanup_beyond_eight_seconds() {
    long_cleanup_case(false).await;
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16, TLS und elf Sekunden echtes Mediencleanup."]
async fn shutdown_preserves_coordinator_cleanup_beyond_eight_seconds() {
    long_cleanup_case(true).await;
}

#[tokio::test]
#[ignore = "Benötigt isoliertes PostgreSQL 16, TLS und drei verzögerte Abschlussinserts."]
async fn shutdown_drains_all_three_slow_session_inserts() {
    let (rows, stopped) = completed_connections_with_delay(false, true).await;
    assert_eq!(
        rows.len(),
        3,
        "Der Dienststopp muss alle drei wartenden Abschlussinserts einsammeln"
    );
    stopped.unwrap();
}
