use chrono::Utc;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use uplink_chat::Platform;
use uplink_chat::token::{BrokerError, Grant, PlatformBroker, TokenQuelle};
use uplink_youtube_live::model::{
    Blockgrund, Endegrund, Identitaet, IngestZugang, LiveEinstellungen, RunAnforderung, RunZustand,
    Schritt, Sichtbarkeit, Vorbereitung, Zustand,
};
use uplink_youtube_live::store::{RunNeu, RunStore, SpeicherRunStore};
use uplink_youtube_live::{ApiFehler, GoogleLiveApi, YouTubeLive};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroize::Zeroizing;

const SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";

struct TestBroker {
    channel: String,
    scopes: Vec<String>,
}

impl PlatformBroker for TestBroker {
    fn grant(
        &self,
        _id: u64,
        _p: Platform,
        _refresh: bool,
    ) -> BoxFuture<'_, Result<Grant, BrokerError>> {
        let grant = Grant {
            access_token: "acc-token".into(),
            expires_at: Utc::now() + chrono::Duration::hours(3),
            platform_user_id: self.channel.clone(),
            platform_login: "earlysalty".into(),
            scopes: self.scopes.clone(),
        };
        Box::pin(async move { Ok(grant) })
    }
}

fn ident(generation: i64) -> Identitaet {
    Identitaet {
        streamer_id: 77,
        channel_id: "UC-kanal".into(),
        connection_generation: generation,
    }
}

fn anforderung() -> RunAnforderung {
    RunAnforderung {
        uplink_session: "sess-1".into(),
    }
}

async fn harness_mit(
    channel: &str,
    scopes: Vec<String>,
    frist: Duration,
) -> (MockServer, YouTubeLive, Arc<SpeicherRunStore>) {
    let server = MockServer::start().await;
    let api = Arc::new(GoogleLiveApi::mit_basis_ungeprueft(server.uri(), frist));
    let broker = Arc::new(TestBroker {
        channel: channel.into(),
        scopes,
    });
    let tokens = Arc::new(TokenQuelle::from_broker(broker));
    let store = Arc::new(SpeicherRunStore::neu());
    let adapter = YouTubeLive::new(api, store.clone(), tokens);
    (server, adapter, store)
}

async fn harness() -> (MockServer, YouTubeLive, Arc<SpeicherRunStore>) {
    harness_mit("UC-kanal", vec![SCOPE.into()], Duration::from_secs(10)).await
}

async fn freigabe_voll(store: &SpeicherRunStore, generation: i64, auto_start: bool, live: bool) {
    let e = LiveEinstellungen {
        titel: "Mein Stream".into(),
        sichtbarkeit: Sichtbarkeit::Private,
        auto_start,
        auto_stop: false,
        live_freigegeben_at: live.then(Utc::now),
        connection_generation: generation,
    };
    store
        .einstellungen_speichern(77, "UC-kanal", &e)
        .await
        .unwrap();
}

async fn freigabe(store: &SpeicherRunStore, generation: i64) {
    freigabe_voll(store, generation, false, true).await;
}

fn run_neu(
    channel: &'static str,
    generation: i64,
    auto_start: bool,
    stream_id: Option<&'static str>,
) -> RunNeu<'static> {
    RunNeu {
        streamer_id: 77,
        channel_id: channel,
        connection_generation: generation,
        uplink_session: "sess-1",
        titel: "Mein Stream",
        sichtbarkeit: Sichtbarkeit::Private,
        auto_start,
        auto_stop: false,
        stream_id,
    }
}

async fn fabriziere(
    store: &SpeicherRunStore,
    generation: i64,
    auto_start: bool,
    zustand: &str,
    schritt: Option<Schritt>,
    stream_id: Option<&'static str>,
    broadcast_id: Option<&'static str>,
) -> i64 {
    let run = store
        .run_anlegen(run_neu("UC-kanal", generation, auto_start, stream_id))
        .await
        .unwrap();
    store
        .referenzen_setzen(run.run_id, generation, stream_id, broadcast_id)
        .await
        .unwrap();
    if zustand != "vorbereitung" {
        store
            .zustand_setzen(run.run_id, generation, &["vorbereitung"], zustand)
            .await
            .unwrap();
    }
    if let Some(s) = schritt {
        store
            .schritt_setzen(run.run_id, generation, s)
            .await
            .unwrap();
    }
    run.run_id
}

fn stream_item(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "snippet": { "channelId": "UC-kanal", "title": "Uplink", "publishedAt": Utc::now().to_rfc3339() },
        "cdn": {
            "ingestionType": "rtmp",
            "ingestionInfo": { "streamName": "geheim-name", "rtmpsIngestionAddress": "rtmps://ingest/app" }
        },
        "status": { "streamStatus": status }
    })
}

fn stream_ohne_ingest(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "snippet": { "channelId": "UC-kanal", "title": "Uplink", "publishedAt": Utc::now().to_rfc3339() },
        "cdn": { "ingestionType": "rtmp" },
        "status": { "streamStatus": status }
    })
}

fn broadcast_item(id: &str, leben: &str, bound: Option<&str>) -> Value {
    let mut cd = json!({ "enableAutoStart": false, "enableAutoStop": false });
    if let Some(b) = bound {
        cd["boundStreamId"] = json!(b);
    }
    json!({
        "id": id,
        "snippet": { "title": "Mein Stream", "publishedAt": Utc::now().to_rfc3339() },
        "status": { "lifeCycleStatus": leben, "privacyStatus": "private" },
        "contentDetails": cd
    })
}

fn slist(item: Value) -> Value {
    json!({ "items": [item] })
}

fn fehler_body(reason: &str) -> Value {
    json!({ "error": { "errors": [ { "reason": reason } ] } })
}

async fn get(server: &MockServer, pfad: &str, query: &[(&str, &str)], body: Value) {
    let mut m = Mock::given(method("GET")).and(path(pfad));
    for (k, v) in query {
        m = m.and(query_param(*k, *v));
    }
    m.respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn get_status(server: &MockServer, pfad: &str, status: u16, body: Value) {
    Mock::given(method("GET"))
        .and(path(pfad))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn get_seq(
    server: &MockServer,
    pfad: &str,
    query: &[(&str, &str)],
    erst: Value,
    dann: Value,
) {
    let mut m1 = Mock::given(method("GET")).and(path(pfad));
    for (k, v) in query {
        m1 = m1.and(query_param(*k, *v));
    }
    m1.respond_with(ResponseTemplate::new(200).set_body_json(erst))
        .up_to_n_times(1)
        .mount(server)
        .await;
    let mut m2 = Mock::given(method("GET")).and(path(pfad));
    for (k, v) in query {
        m2 = m2.and(query_param(*k, *v));
    }
    m2.respond_with(ResponseTemplate::new(200).set_body_json(dann))
        .mount(server)
        .await;
}

async fn get_seq_status(
    server: &MockServer,
    pfad: &str,
    query: &[(&str, &str)],
    status: u16,
    erst: Value,
    dann: Value,
) {
    let mut m1 = Mock::given(method("GET")).and(path(pfad));
    for (k, v) in query {
        m1 = m1.and(query_param(*k, *v));
    }
    m1.respond_with(ResponseTemplate::new(status).set_body_json(erst))
        .up_to_n_times(1)
        .mount(server)
        .await;
    let mut m2 = Mock::given(method("GET")).and(path(pfad));
    for (k, v) in query {
        m2 = m2.and(query_param(*k, *v));
    }
    m2.respond_with(ResponseTemplate::new(200).set_body_json(dann))
        .mount(server)
        .await;
}

async fn post(server: &MockServer, pfad: &str, body: Value) {
    Mock::given(method("POST"))
        .and(path(pfad))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn post_status(server: &MockServer, pfad: &str, status: u16, body: Value) {
    Mock::given(method("POST"))
        .and(path(pfad))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn anzahl(server: &MockServer, m: &str, pfad: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == m && r.url.path() == pfad)
        .count()
}

async fn keine_youtube_requests(server: &MockServer) -> bool {
    server.received_requests().await.unwrap().is_empty()
}

#[tokio::test]
async fn ohne_freigabe_blockiert_ohne_requests() {
    let (server, adapter, _store) = harness().await;
    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    assert!(matches!(
        v.zustand,
        Zustand::Blockiert {
            grund: Blockgrund::KeineFreigabe,
            ..
        }
    ));
    assert!(keine_youtube_requests(&server).await);
}

#[tokio::test]
async fn generationskonflikt_blockiert_ohne_requests() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    let neuer = adapter.prepare(&ident(2), anforderung()).await.unwrap();
    assert!(matches!(
        neuer.zustand,
        Zustand::Blockiert {
            grund: Blockgrund::KeineFreigabe,
            ..
        }
    ));
    let alter = adapter.prepare(&ident(0), anforderung()).await.unwrap();
    assert!(matches!(
        alter.zustand,
        Zustand::Blockiert {
            grund: Blockgrund::VeralteteGeneration,
            ..
        }
    ));
    assert!(keine_youtube_requests(&server).await);
}

#[tokio::test]
async fn fremder_kanal_blockiert_ohne_requests() {
    let (server, adapter, store) =
        harness_mit("UC-fremd", vec![SCOPE.into()], Duration::from_secs(10)).await;
    freigabe(&store, 1).await;
    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    assert!(matches!(
        v.zustand,
        Zustand::Blockiert {
            grund: Blockgrund::IdentitaetAbweichung,
            ..
        }
    ));
    assert!(keine_youtube_requests(&server).await);
}

#[tokio::test]
async fn fehlender_scope_blockiert() {
    let (server, adapter, store) = harness_mit("UC-kanal", vec![], Duration::from_secs(10)).await;
    freigabe(&store, 1).await;
    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    assert!(matches!(
        v.zustand,
        Zustand::Blockiert {
            grund: Blockgrund::RechteFehlen,
            ..
        }
    ));
    assert!(keine_youtube_requests(&server).await);
}

#[tokio::test]
async fn ungueltige_kanal_id_wird_abgewiesen() {
    let (_server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    let id = Identitaet {
        streamer_id: 77,
        channel_id: "UC kanal mit leer!".into(),
        connection_generation: 1,
    };
    assert!(matches!(
        adapter.prepare(&id, anforderung()).await,
        Err(uplink_youtube_live::LiveFehler::Ungueltig(_))
    ));
}

#[tokio::test]
async fn voller_erstlauf_und_idempotent() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    store.stream_id_merken(77, 1, "S-alt").await.unwrap();
    get(
        &server,
        "/liveStreams",
        &[("id", "S-alt")],
        json!({ "items": [] }),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S-neu")],
        slist(stream_item("S-neu", "ready")),
    )
    .await;
    post(&server, "/liveStreams", stream_item("S-neu", "ready")).await;
    Mock::given(method("POST"))
        .and(path("/liveBroadcasts"))
        .and(body_partial_json(
            json!({ "snippet": { "title": "Mein Stream" } }),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(broadcast_item("B-neu", "created", None)),
        )
        .expect(1)
        .mount(&server)
        .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B-neu", "created", Some("S-neu")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B-neu")],
        slist(broadcast_item("B-neu", "created", Some("S-neu"))),
    )
    .await;

    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    match &v.zustand {
        Zustand::Vorbereitet { refs } => {
            assert_eq!(refs.stream_id, "S-neu");
            assert_eq!(refs.broadcast_id, "B-neu");
        }
        anderes => panic!("erwartet Vorbereitet, war {anderes:?}"),
    }
    let ingest = v.ingest.expect("Ingest");
    assert_eq!(ingest.stream_name.as_str(), "geheim-name");
    let bodies: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    assert!(
        bodies.iter().all(|b| !b.contains("sess-1")),
        "kein Session im Titel"
    );
    assert_eq!(
        store
            .einstellungen_laden(77)
            .await
            .unwrap()
            .unwrap()
            .stream_id
            .as_deref(),
        Some("S-neu")
    );

    let zweiter = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    assert!(matches!(zweiter.zustand, Zustand::Vorbereitet { .. }));
    assert!(zweiter.ingest.is_some());
    assert_eq!(anzahl(&server, "POST", "/liveStreams").await, 1);
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts").await, 1);
}

#[tokio::test]
async fn start_auto_start_false_geht_live_bei_active() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "active")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "ready", Some("S1"))),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/transition",
        broadcast_item("B1", "live", Some("S1")),
    )
    .await;

    let z = adapter.start(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Live { .. }), "war {z:?}");
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        1
    );
}

#[tokio::test]
async fn start_ready_bleibt_sendet_ohne_transition() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "ready", Some("S1"))),
    )
    .await;

    let z = adapter.start(&ident(1)).await.unwrap();
    match z {
        Zustand::Sendet { stream_status, .. } => assert_eq!(stream_status, "ready"),
        anderes => panic!("erwartet Sendet, war {anderes:?}"),
    }
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        0
    );
}

#[tokio::test]
async fn start_fremde_bindung_ist_fehler_nicht_wiederaufnehmbar() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "active")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "live", Some("S-fremd"))),
    )
    .await;

    let z = adapter.start(&ident(1)).await.unwrap();
    assert!(
        matches!(
            z,
            Zustand::Fehler {
                wiederaufnehmbar: false,
                ..
            }
        ),
        "war {z:?}"
    );
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        0
    );
}

#[tokio::test]
async fn start_auto_start_true_ruft_keine_transition() {
    let (server, adapter, store) = harness().await;
    freigabe_voll(&store, 1, true, true).await;
    fabriziere(&store, 1, true, "vorbereitet", None, Some("S1"), Some("B1")).await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "active")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "live", Some("S1"))),
    )
    .await;

    let z = adapter.start(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Live { .. }), "war {z:?}");
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        0
    );
}

#[tokio::test]
async fn start_ready_dann_status_active_schaltet_live() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get_seq(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
        slist(stream_item("S1", "active")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "ready", Some("S1"))),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/transition",
        broadcast_item("B1", "live", Some("S1")),
    )
    .await;

    let erst = adapter.start(&ident(1)).await.unwrap();
    assert!(matches!(erst, Zustand::Sendet { .. }), "war {erst:?}");
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        0
    );
    let dann = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(dann, Zustand::Live { .. }), "war {dann:?}");
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        1
    );
}

#[tokio::test]
async fn finish_live_beendet_mit_bestaetigung() {
    let (server, adapter, store) = harness().await;
    fabriziere(&store, 1, false, "live", None, Some("S1"), Some("B1")).await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "live", Some("S1"))),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/transition",
        broadcast_item("B1", "complete", Some("S1")),
    )
    .await;

    let z = adapter
        .finish(&ident(1), Endegrund::NutzerStop)
        .await
        .unwrap();
    match z {
        Zustand::Beendet {
            grund,
            youtube_bestaetigt,
            ..
        } => {
            assert_eq!(grund, Endegrund::NutzerStop);
            assert!(youtube_bestaetigt);
        }
        anderes => panic!("erwartet Beendet, war {anderes:?}"),
    }
    assert!(store.aktiven_run_laden(77).await.unwrap().is_none());
}

#[tokio::test]
async fn finish_ready_beendet_ohne_transition() {
    let (server, adapter, store) = harness().await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "ready", Some("S1"))),
    )
    .await;

    let z = adapter
        .finish(&ident(1), Endegrund::NutzerStop)
        .await
        .unwrap();
    assert!(matches!(
        z,
        Zustand::Beendet {
            youtube_bestaetigt: false,
            ..
        }
    ));
    assert_eq!(
        anzahl(&server, "POST", "/liveBroadcasts/transition").await,
        0
    );
}

#[tokio::test]
async fn quota_bei_finish_ist_wiederaufnehmbar() {
    let (server, adapter, store) = harness().await;
    fabriziere(&store, 1, false, "live", None, Some("S1"), Some("B1")).await;
    get_status(
        &server,
        "/liveBroadcasts",
        403,
        fehler_body("quotaExceeded"),
    )
    .await;

    let z = adapter
        .finish(&ident(1), Endegrund::NutzerStop)
        .await
        .unwrap();
    assert!(matches!(
        z,
        Zustand::Fehler {
            fehler: ApiFehler::Quota,
            wiederaufnehmbar: true,
            ..
        }
    ));
    assert!(
        store.aktiven_run_laden(77).await.unwrap().is_some(),
        "Run bleibt offen"
    );
}

#[tokio::test]
async fn finish_offener_broadcast_insert_gleicht_ab_ohne_anlegen() {
    let (server, adapter, store) = harness().await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitung",
        Some(Schritt::BroadcastInsert),
        Some("S1"),
        None,
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "upcoming")],
        slist(broadcast_item("B1", "ready", Some("S1"))),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "active")],
        json!({ "items": [] }),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "ready", Some("S1"))),
    )
    .await;

    let z = adapter
        .finish(&ident(1), Endegrund::AdminStop)
        .await
        .unwrap();
    assert!(
        matches!(
            z,
            Zustand::Beendet {
                youtube_bestaetigt: false,
                ..
            }
        ),
        "war {z:?}"
    );
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts").await, 0);
    assert!(store.aktiven_run_laden(77).await.unwrap().is_none());
}

#[tokio::test]
async fn medien_unterbrochen_ohne_request_haelt_zustand() {
    let (server, adapter, store) = harness().await;
    fabriziere(&store, 1, false, "live", None, Some("S1"), Some("B1")).await;
    let z = adapter.medien_unterbrochen(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Live { .. }));
    assert!(keine_youtube_requests(&server).await);
    let run = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert!(run.unterbrochen_at.is_some());
    assert_eq!(run.zustand, RunZustand::Live);
}

#[tokio::test]
async fn erneuter_stream_nach_beendet_verwendet_stream_wieder() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    store.stream_id_merken(77, 1, "S1").await.unwrap();
    let alt = fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B-alt"),
    )
    .await;
    store
        .run_schliessen(alt, 1, Some("nutzer_stop"), Some(true), None)
        .await
        .unwrap();
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts",
        broadcast_item("B-neu", "created", None),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B-neu", "created", Some("S1")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B-neu")],
        slist(broadcast_item("B-neu", "created", Some("S1"))),
    )
    .await;

    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    match &v.zustand {
        Zustand::Vorbereitet { refs } => {
            assert_eq!(refs.stream_id, "S1");
            assert_eq!(refs.broadcast_id, "B-neu");
        }
        anderes => panic!("erwartet Vorbereitet, war {anderes:?}"),
    }
    assert_eq!(anzahl(&server, "POST", "/liveStreams").await, 0);
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts").await, 1);
}

#[tokio::test]
async fn quota_bei_status_ist_wiederaufnehmbar_und_haelt_zustand() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get_status(&server, "/liveStreams", 403, fehler_body("quotaExceeded")).await;
    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(
        z,
        Zustand::Fehler {
            fehler: ApiFehler::Quota,
            wiederaufnehmbar: true,
            ..
        }
    ));
    assert_eq!(
        store.aktiven_run_laden(77).await.unwrap().unwrap().zustand,
        RunZustand::Vorbereitet
    );
}

#[tokio::test]
async fn transport_bei_status_ist_wiederaufnehmbar() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(
        z,
        Zustand::Fehler {
            wiederaufnehmbar: true,
            ..
        }
    ));
}

#[tokio::test]
async fn live_nicht_freigeschaltet_bei_insert_blockiert() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    post_status(
        &server,
        "/liveStreams",
        403,
        fehler_body("liveStreamingNotEnabled"),
    )
    .await;
    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    assert!(matches!(
        v.zustand,
        Zustand::Blockiert {
            grund: Blockgrund::LiveNichtFreigeschaltet,
            ..
        }
    ));
}

#[tokio::test]
async fn fehlender_ingest_zugang_ist_fehler() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    store.stream_id_merken(77, 1, "S1").await.unwrap();
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_ohne_ingest("S1", "ready")),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts",
        broadcast_item("B1", "created", None),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B1", "created", Some("S1")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;

    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    match v.zustand {
        Zustand::Fehler {
            fehler: ApiFehler::Ungueltig(m),
            wiederaufnehmbar: false,
            ..
        } => {
            assert_eq!(m, "YouTube liefert keinen Ingest-Zugang.");
        }
        anderes => panic!("erwartet Ingest-Fehler, war {anderes:?}"),
    }
}

#[tokio::test]
async fn post_503_bei_broadcast_insert_ist_unklar_dann_abgleich() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    store.stream_id_merken(77, 1, "S1").await.unwrap();
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    post_status(&server, "/liveBroadcasts", 503, json!({})).await;

    let v = adapter.prepare(&ident(1), anforderung()).await.unwrap();
    match v.zustand {
        Zustand::Unklar { schritt, .. } => assert_eq!(schritt, Schritt::BroadcastInsert),
        anderes => panic!("erwartet Unklar, war {anderes:?}"),
    }
    let run = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(run.schritt, Some(Schritt::BroadcastInsert));

    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "upcoming")],
        slist(broadcast_item("B1", "created", None)),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "active")],
        json!({ "items": [] }),
    )
    .await;
    get_seq(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", None)),
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B1", "created", Some("S1")),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts").await, 1);
}

#[tokio::test]
async fn freigabe_entzogen_status_blockiert_ohne_requests() {
    let (server, adapter, store) = harness().await;
    freigabe_voll(&store, 1, false, false).await;
    fabriziere(&store, 1, false, "vorbereitung", None, None, None).await;
    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(
        z,
        Zustand::Blockiert {
            grund: Blockgrund::KeineFreigabe,
            ..
        }
    ));
    assert!(keine_youtube_requests(&server).await);
}

#[tokio::test]
async fn fremder_run_kanal_blockiert_ohne_requests() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    let run = store
        .run_anlegen(run_neu("UC-fremd", 1, false, Some("S1")))
        .await
        .unwrap();
    store
        .referenzen_setzen(run.run_id, 1, Some("S1"), Some("B1"))
        .await
        .unwrap();
    store
        .zustand_setzen(run.run_id, 1, &["vorbereitung"], "vorbereitet")
        .await
        .unwrap();
    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(
        z,
        Zustand::Blockiert {
            grund: Blockgrund::IdentitaetAbweichung,
            ..
        }
    ));
    assert!(keine_youtube_requests(&server).await);
}

#[tokio::test]
async fn einstellungswechsel_stoert_laufende_vorbereitung_nicht() {
    let (server, adapter, store) = harness().await;
    freigabe_voll(&store, 1, true, true).await;
    store.stream_id_merken(77, 1, "S1").await.unwrap();
    fabriziere(&store, 1, false, "vorbereitung", None, Some("S1"), None).await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts",
        broadcast_item("B1", "created", None),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B1", "created", Some("S1")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
}

#[tokio::test]
async fn abgleich_broadcast_insert_ein_kandidat_geht_weiter_zu_bind() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitung",
        Some(Schritt::BroadcastInsert),
        Some("S1"),
        None,
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "upcoming")],
        slist(broadcast_item("B1", "created", None)),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "active")],
        json!({ "items": [] }),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    get_seq(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", None)),
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B1", "created", Some("S1")),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts/bind").await, 1);
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts").await, 0);
}

#[tokio::test]
async fn abgleich_broadcast_insert_zwei_kandidaten_blockiert() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitung",
        Some(Schritt::BroadcastInsert),
        Some("S1"),
        None,
    )
    .await;
    get(&server, "/liveBroadcasts", &[("mine", "true"), ("broadcastStatus", "upcoming")], json!({ "items": [broadcast_item("B1", "created", None), broadcast_item("B2", "created", None)] })).await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "active")],
        json!({ "items": [] }),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(
        z,
        Zustand::Blockiert {
            grund: Blockgrund::UnklareZuordnung,
            ..
        }
    ));
}

#[tokio::test]
async fn abgleich_broadcast_insert_kein_kandidat_fuegt_erneut_ein() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitung",
        Some(Schritt::BroadcastInsert),
        Some("S1"),
        None,
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "upcoming")],
        json!({ "items": [] }),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("mine", "true"), ("broadcastStatus", "active")],
        json!({ "items": [] }),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts",
        broadcast_item("B9", "created", None),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B9", "created", Some("S1")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B9")],
        slist(broadcast_item("B9", "created", Some("S1"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts").await, 1);
}

#[tokio::test]
async fn abgleich_stream_insert_ein_kandidat_mine() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitung",
        Some(Schritt::StreamInsert),
        None,
        None,
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("mine", "true")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts",
        broadcast_item("B1", "created", None),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B1", "created", Some("S1")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
    assert_eq!(anzahl(&server, "POST", "/liveStreams").await, 0);
    assert_eq!(
        store
            .einstellungen_laden(77)
            .await
            .unwrap()
            .unwrap()
            .stream_id
            .as_deref(),
        Some("S1")
    );
}

#[tokio::test]
async fn abgleich_bind_bereits_gebunden_ohne_zweiten_bind() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitung",
        Some(Schritt::Bind),
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
    assert_eq!(anzahl(&server, "POST", "/liveBroadcasts/bind").await, 0);
}

#[tokio::test]
async fn abgleich_transition_live_uebernimmt_live() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "sendet",
        Some(Schritt::TransitionLive),
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "live", Some("S1"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Live { .. }), "war {z:?}");
    let run = store.aktiven_run_laden(77).await.unwrap().unwrap();
    assert_eq!(run.schritt, None);
    assert_eq!(run.zustand, RunZustand::Live);
}

#[tokio::test]
async fn abgleich_transition_live_fremde_bindung_ist_fehler() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "sendet",
        Some(Schritt::TransitionLive),
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "live", Some("S-fremd"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(
        matches!(
            z,
            Zustand::Fehler {
                wiederaufnehmbar: false,
                ..
            }
        ),
        "war {z:?}"
    );
}

#[tokio::test]
async fn retry_nach_401_gelingt() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get_seq_status(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        401,
        fehler_body("authError"),
        slist(stream_item("S1", "ready")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "created", Some("S1"))),
    )
    .await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(z, Zustand::Vorbereitet { .. }), "war {z:?}");
    assert_eq!(anzahl(&server, "GET", "/liveStreams").await, 2);
}

#[tokio::test]
async fn zweites_401_blockiert() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get_status(&server, "/liveStreams", 401, fehler_body("authError")).await;

    let z = adapter.status(&ident(1)).await.unwrap();
    assert!(matches!(
        z,
        Zustand::Blockiert {
            grund: Blockgrund::NeuAnmeldungNoetig,
            ..
        }
    ));
    assert_eq!(anzahl(&server, "GET", "/liveStreams").await, 2);
}

#[tokio::test]
async fn fremde_generation_nach_neustart_schliesst_ohne_requests() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 1).await;
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    let z = adapter.status(&ident(2)).await.unwrap();
    assert!(matches!(z, Zustand::Inaktiv));
    assert!(keine_youtube_requests(&server).await);
    assert!(store.aktiven_run_laden(77).await.unwrap().is_none());
}

#[tokio::test]
async fn fremde_generation_neuer_lauf_nur_mit_freigabe() {
    let (server, adapter, store) = harness().await;
    freigabe(&store, 2).await;
    store.stream_id_merken(77, 2, "S1").await.unwrap();
    fabriziere(
        &store,
        1,
        false,
        "vorbereitet",
        None,
        Some("S1"),
        Some("B1"),
    )
    .await;
    get(
        &server,
        "/liveStreams",
        &[("id", "S1")],
        slist(stream_item("S1", "ready")),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts",
        broadcast_item("B-neu", "created", None),
    )
    .await;
    post(
        &server,
        "/liveBroadcasts/bind",
        broadcast_item("B-neu", "created", Some("S1")),
    )
    .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B-neu")],
        slist(broadcast_item("B-neu", "created", Some("S1"))),
    )
    .await;

    let v = adapter.prepare(&ident(2), anforderung()).await.unwrap();
    match &v.zustand {
        Zustand::Vorbereitet { refs } => assert_eq!(refs.broadcast_id, "B-neu"),
        anderes => panic!("erwartet Vorbereitet, war {anderes:?}"),
    }
    assert_eq!(
        store
            .aktiven_run_laden(77)
            .await
            .unwrap()
            .unwrap()
            .connection_generation,
        2
    );
}

#[tokio::test]
async fn paralleler_aufruf_ist_belegt() {
    let (server, adapter, store) = harness().await;
    let adapter = Arc::new(adapter);
    freigabe(&store, 1).await;
    fabriziere(&store, 1, false, "live", None, Some("S1"), Some("B1")).await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_json(slist(stream_item("S1", "active"))),
        )
        .mount(&server)
        .await;
    get(
        &server,
        "/liveBroadcasts",
        &[("id", "B1")],
        slist(broadcast_item("B1", "live", Some("S1"))),
    )
    .await;

    let zweit = adapter.clone();
    let erste = tokio::spawn(async move { zweit.status(&ident(1)).await });
    tokio::time::sleep(Duration::from_millis(60)).await;
    let ergebnis = adapter.status(&ident(1)).await;
    assert!(matches!(
        ergebnis,
        Err(uplink_youtube_live::LiveFehler::Belegt)
    ));
    let _ = erste.await.unwrap();
}

#[test]
fn debug_maskiert_ingest_und_vorbereitung() {
    let ingest = IngestZugang {
        rtmps_url: "rtmps://ingest/app".into(),
        stream_name: Zeroizing::new("streng-geheim".into()),
    };
    let text = format!("{ingest:?}");
    assert!(!text.contains("streng-geheim"));
    assert!(text.contains("[geschützt]"));
    let vorbereitung = Vorbereitung {
        zustand: Zustand::Inaktiv,
        ingest: Some(ingest),
    };
    assert!(!format!("{vorbereitung:?}").contains("streng-geheim"));
}
