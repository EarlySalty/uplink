use std::{sync::Arc, time::Duration};
#[path = "support/database.rs"]
mod database;
#[path = "../../uplink-ingest/tests/support/tls.rs"]
#[allow(dead_code)]
mod tls;

struct NoMedia;
impl uplink_service::runtime::SessionProcessor for NoMedia {
    async fn process(
        &self,
        _: uplink_ingest::MediaEvent,
        mut events: tokio::sync::mpsc::Receiver<uplink_ingest::MediaEvent>,
    ) -> Result<(), &'static str> {
        while events.recv().await.is_some() {}
        Ok(())
    }
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16; prüft sechs Sekunden echte Requestdrainage."]
async fn accepted_database_request_finishes_during_graceful_shutdown() {
    let (database, mut state) = database::fixture().await;
    Arc::get_mut(&mut state)
        .unwrap()
        .config
        .request_timeout_seconds = 10;
    let (client, connection) = tokio_postgres::Config::new()
        .host(database.directory.to_str().unwrap())
        .user("uplink_test")
        .dbname("postgres")
        .connect(tokio_postgres::NoTls)
        .await
        .unwrap();
    let database_connection = tokio::spawn(connection);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let service = tokio::spawn(uplink_service::runtime::serve_with_ready(
        state.clone(),
        tls::test_tls().server,
        Arc::new(NoMedia),
        async {
            let _ = stopped.await;
        },
        Some(ready),
    ));
    let (api, _) = bound.await.unwrap();
    client
        .batch_execute("BEGIN; LOCK TABLE relay.users IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let request = tokio::spawn(async move {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(14))
            .build()
            .unwrap()
            .get(format!("http://{api}/v1/me?streamer_id=11"))
            .header("X-Relay-Auth", "synthetic-api")
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            client.batch_execute("SELECT pg_stat_clear_snapshot()").await.unwrap();
            let waiting:bool=client.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE '%relay.users%')",&[]).await.unwrap().get(0);
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    client.batch_execute("COMMIT").await.unwrap();
    let response = request.await.unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(10), service)
        .await
        .unwrap()
        .unwrap();
    drop(client);
    database_connection.abort();
    let _ = database_connection.await;
    drop(state);
    database.stop().await;
    assert!(
        response.is_ok_and(|response| response.status().is_success()),
        "Akzeptierte Anfrage muss innerhalb ihrer zugesagten Frist fertig antworten"
    );
    assert!(
        stopped.is_ok(),
        "Ein fristgerecht abgeschlossener Request darf keinen unsauberen Stopp erzeugen"
    );
}
