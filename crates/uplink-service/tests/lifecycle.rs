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
        _: uplink_ingest::MediaEvent,
        _: tokio::sync::mpsc::Receiver<uplink_ingest::MediaEvent>,
    ) -> Result<(), &'static str> {
        Err("Kein Ausgang konnte gestartet werden.")
    }
}

async fn completed_connections(
    write_failure: bool,
) -> (Vec<serde_json::Value>, Result<(), &'static str>) {
    let (database, state) = database::fixture().await;
    state.store.query("CREATE TABLE IF NOT EXISTS relay.sessions(id bigserial PRIMARY KEY,streamer_id bigint NOT NULL,started_at timestamptz NOT NULL,ended_at timestamptz,ingest_protocol text NOT NULL,ingest_codec text,profile_json jsonb NOT NULL,end_reason text)", &[]).await.unwrap();
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
        assert_eq!(row["end_reason"], reason);
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
