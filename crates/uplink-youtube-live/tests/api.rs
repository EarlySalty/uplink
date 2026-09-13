use serde_json::json;
use std::time::Duration;
use uplink_youtube_live::api::{BroadcastWunsch, GoogleLiveApi, LiveApi};
use uplink_youtube_live::fehler::ApiFehler;
use uplink_youtube_live::model::Sichtbarkeit;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroize::Zeroizing;

fn tok() -> Zeroizing<String> {
    Zeroizing::new("acc-token".into())
}

fn stream_json(id: &str, status: &str) -> serde_json::Value {
    json!({
        "id": id,
        "snippet": { "channelId": "UC-kanal", "title": "Uplink", "publishedAt": "2026-09-08T10:00:00Z" },
        "cdn": {
            "ingestionType": "rtmp",
            "ingestionInfo": { "streamName": "geheim-name", "rtmpsIngestionAddress": "rtmps://ingest/app" }
        },
        "status": { "streamStatus": status }
    })
}

fn broadcast_json(id: &str, leben: &str) -> serde_json::Value {
    json!({
        "id": id,
        "snippet": { "title": "Mein Stream", "publishedAt": "2026-09-08T10:00:00Z" },
        "status": { "lifeCycleStatus": leben, "privacyStatus": "private" },
        "contentDetails": { "boundStreamId": "S1", "enableAutoStart": false, "enableAutoStop": false }
    })
}

async fn api(server: &MockServer) -> GoogleLiveApi {
    GoogleLiveApi::mit_basis_ungeprueft(server.uri(), Duration::from_secs(10))
}

#[tokio::test]
async fn stream_anlegen_sendet_pflichtfelder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/liveStreams"))
        .and(query_param("part", "snippet,cdn,contentDetails,status"))
        .and(body_partial_json(json!({
            "snippet": { "title": "Uplink" },
            "cdn": { "frameRate": "variable", "ingestionType": "rtmp", "resolution": "variable" },
            "contentDetails": { "isReusable": true }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(stream_json("S1", "ready")))
        .expect(1)
        .mount(&server)
        .await;
    let stream = api(&server)
        .await
        .stream_anlegen(&tok(), "Uplink")
        .await
        .expect("Stream");
    assert_eq!(stream.id, "S1");
    assert_eq!(stream.ingestion_type.as_deref(), Some("rtmp"));
    assert_eq!(stream.rtmps_url.as_deref(), Some("rtmps://ingest/app"));
    assert_eq!(stream.stream_name.as_str(), "geheim-name");
    assert!(!format!("{stream:?}").contains("geheim-name"));
}

#[tokio::test]
async fn stream_lesen_liefert_treffer_und_leer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .and(query_param("id", "S1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [stream_json("S1", "ready")] })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .and(query_param("id", "S9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .mount(&server)
        .await;
    let api = api(&server).await;
    assert!(api.stream_lesen(&tok(), "S1").await.unwrap().is_some());
    assert!(api.stream_lesen(&tok(), "S9").await.unwrap().is_none());
}

#[tokio::test]
async fn stream_lesen_404_ist_leer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "error": { "errors": [] } })),
        )
        .mount(&server)
        .await;
    assert!(
        api(&server)
            .await
            .stream_lesen(&tok(), "S1")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn streams_eigene_liefert_liste() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .and(query_param("mine", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [stream_json("S1", "ready"), stream_json("S2", "active")]
        })))
        .mount(&server)
        .await;
    let liste = api(&server).await.streams_eigene(&tok()).await.unwrap();
    assert_eq!(liste.len(), 2);
}

#[tokio::test]
async fn broadcast_anlegen_sendet_felder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/liveBroadcasts"))
        .and(query_param("part", "snippet,status,contentDetails"))
        .and(body_partial_json(json!({
            "snippet": { "title": "Mein Stream" },
            "status": { "privacyStatus": "unlisted" },
            "contentDetails": {
                "enableAutoStart": true,
                "enableAutoStop": false,
                "monitorStream": { "enableMonitorStream": false }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(broadcast_json("B1", "created")))
        .expect(1)
        .mount(&server)
        .await;
    let wunsch = BroadcastWunsch {
        titel: "Mein Stream".into(),
        scheduled_start: chrono::Utc::now(),
        sichtbarkeit: Sichtbarkeit::Unlisted,
        auto_start: true,
        auto_stop: false,
    };
    let b = api(&server)
        .await
        .broadcast_anlegen(&tok(), &wunsch)
        .await
        .unwrap();
    assert_eq!(b.id, "B1");
    assert_eq!(b.life_cycle_status.as_deref(), Some("created"));
}

#[tokio::test]
async fn binden_traegt_query_und_liefert_bindung() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/liveBroadcasts/bind"))
        .and(query_param("id", "B1"))
        .and(query_param("streamId", "S1"))
        .and(query_param("part", "id,contentDetails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(broadcast_json("B1", "ready")))
        .expect(1)
        .mount(&server)
        .await;
    let b = api(&server).await.binden(&tok(), "B1", "S1").await.unwrap();
    assert_eq!(b.bound_stream_id.as_deref(), Some("S1"));
}

#[tokio::test]
async fn transition_traegt_zielstatus() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/liveBroadcasts/transition"))
        .and(query_param("id", "B1"))
        .and(query_param("broadcastStatus", "live"))
        .and(query_param("part", "id,status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(broadcast_json("B1", "live")))
        .expect(1)
        .mount(&server)
        .await;
    let b = api(&server)
        .await
        .transition(&tok(), "B1", "live")
        .await
        .unwrap();
    assert_eq!(b.life_cycle_status.as_deref(), Some("live"));
}

async fn fehler_bei_get(server: &MockServer, status: u16, reason: &str) -> ApiFehler {
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({
            "error": { "errors": [ { "reason": reason } ] }
        })))
        .mount(server)
        .await;
    api(server).await.streams_eigene(&tok()).await.unwrap_err()
}

#[tokio::test]
async fn fehlerabbildung_getrennt() {
    for (status, reason, erwartet) in [
        (401u16, "authError", ApiFehler::NeuAnmeldungNoetig),
        (403, "insufficientPermissions", ApiFehler::RechteFehlen),
        (403, "forbidden", ApiFehler::RechteFehlen),
        (403, "quotaExceeded", ApiFehler::Quota),
        (403, "dailyLimitExceeded", ApiFehler::Quota),
        (
            403,
            "liveStreamingNotEnabled",
            ApiFehler::LiveNichtFreigeschaltet,
        ),
        (403, "rateLimitExceeded", ApiFehler::Ratelimit),
        (429, "userRateLimitExceeded", ApiFehler::Ratelimit),
        (400, "badRequest", ApiFehler::Ungueltig("badRequest".into())),
    ] {
        let server = MockServer::start().await;
        assert_eq!(fehler_bei_get(&server, status, reason).await, erwartet);
    }
}

#[tokio::test]
async fn get_500_ist_transport_post_404_ist_nicht_gefunden() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/liveBroadcasts/transition"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "error": { "errors": [] } })),
        )
        .mount(&server)
        .await;
    let api = api(&server).await;
    assert!(matches!(
        api.streams_eigene(&tok()).await.unwrap_err(),
        ApiFehler::Transport(_)
    ));
    assert_eq!(
        api.transition(&tok(), "B1", "live").await.unwrap_err(),
        ApiFehler::NichtGefunden
    );
}

#[tokio::test]
async fn timeout_ist_transport_bei_get_und_unklar_bei_post() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(400)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/liveStreams"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(400)))
        .mount(&server)
        .await;
    let api = GoogleLiveApi::mit_basis_ungeprueft(server.uri(), Duration::from_millis(80));
    assert!(matches!(
        api.streams_eigene(&tok()).await.unwrap_err(),
        ApiFehler::Transport(_)
    ));
    assert_eq!(
        api.stream_anlegen(&tok(), "Uplink").await.unwrap_err(),
        ApiFehler::Unklar
    );
}

#[tokio::test]
async fn zu_viele_seiten_werden_gemeldet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .and(query_param("mine", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [stream_json("S1", "ready")],
            "nextPageToken": "weiter"
        })))
        .mount(&server)
        .await;
    assert_eq!(
        api(&server).await.streams_eigene(&tok()).await.unwrap_err(),
        ApiFehler::ZuVieleSeiten
    );
}

#[tokio::test]
async fn paginierung_sammelt_mehrere_seiten() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .and(query_param("pageToken", "P2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [stream_json("S2", "ready")]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/liveStreams"))
        .and(query_param("mine", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [stream_json("S1", "ready")],
            "nextPageToken": "P2"
        })))
        .mount(&server)
        .await;
    let liste = api(&server).await.streams_eigene(&tok()).await.unwrap();
    assert_eq!(liste.len(), 2);
}

#[test]
fn basis_pruefung_lehnt_fremde_hosts_ab() {
    assert!(GoogleLiveApi::mit_basis("http://www.googleapis.com/youtube/v3").is_err());
    assert!(GoogleLiveApi::mit_basis("https://boese.example.com/youtube/v3").is_err());
    assert!(GoogleLiveApi::mit_basis("https://www.googleapis.com/youtube/v3").is_ok());
    assert!(GoogleLiveApi::mit_basis("https://youtube.googleapis.com/v3").is_ok());
}
