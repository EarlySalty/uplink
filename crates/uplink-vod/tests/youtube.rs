use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use uplink_vod::{youtube::*, *};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};
struct Broker;
#[async_trait]
impl PlatformBroker for Broker {
    async fn grant(&self, _: u64, _: Platform, _: bool) -> Result<Grant> {
        Ok(Grant {
            access_token: Secret::new("isolierter-testzugang".into()),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            platform_user_id: "UC-owner".into(),
            scopes: vec!["https://www.googleapis.com/auth/youtube.force-ssl".into()],
        })
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
async fn api(server: &MockServer) -> YouTube {
    Mock::given(method("GET"))
        .and(path("/youtube/v3/channels"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"UC-owner"}]})),
        )
        .mount(server)
        .await;
    YouTube::for_test(Arc::new(Broker), &server.uri()).unwrap()
}
#[tokio::test]
async fn force_ssl_startet_privaten_upload_und_validiert_kanal() {
    let s = MockServer::start().await;
    let yt = api(&s).await;
    Mock::given(method("POST")).and(path("/upload/youtube/v3/videos"))
        .and(wiremock::matchers::body_json(json!({"snippet":{"title":"Testaufnahme","description":""},"status":{"privacyStatus":"private"}})))
        .respond_with(ResponseTemplate::new(200).insert_header("Location",format!("{}/upload/youtube/v3/videos?upload_id=test",s.uri())))
        .expect(1).mount(&s).await;
    let session = yt.start(7, &settings(), 1024).await.unwrap();
    assert!(!format!("{session:?}").contains("upload_id"));
}

#[tokio::test]
async fn oeffentliche_regel_startet_zunaechst_immer_privaten_upload() {
    let server = MockServer::start().await;
    let yt = api(&server).await;
    let mut config = settings();
    config.privacy = Privacy::Public;
    config.publication_authorized = true;
    Mock::given(method("POST"))
        .and(wiremock::matchers::body_partial_json(
            json!({"status":{"privacyStatus":"private"}}),
        ))
        .respond_with(ResponseTemplate::new(200).insert_header(
            "Location",
            format!(
                "{}/upload/youtube/v3/videos?upload_id=protected",
                server.uri()
            ),
        ))
        .expect(1)
        .mount(&server)
        .await;
    yt.start(7, &config, 16).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn verlorener_abschlussbody_bleibt_mit_bekannter_session_wiederaufnehmbar() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = vec![];
        let mut byte = [0u8; 1];
        while !header.ends_with(b"\r\n\r\n") && header.len() < 8192 {
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{").await.unwrap();
    });
    let yt = YouTube::for_test(Arc::new(Broker), &endpoint).unwrap();
    let session = Secret::new(format!(
        "{endpoint}/upload/youtube/v3/videos?upload_id=protected"
    ));
    assert_eq!(
        yt.reconcile(7, "UC-owner", &session, 16).await,
        Err(Error::Network)
    );
    server.await.unwrap();
}
#[tokio::test]
async fn fortschritt_kommt_nur_aus_bestaetigtem_range() {
    let s = MockServer::start().await;
    let yt = api(&s).await;
    Mock::given(method("PUT"))
        .and(path("/upload/youtube/v3/videos"))
        .and(header("Content-Range", "bytes */1048576"))
        .respond_with(ResponseTemplate::new(308).insert_header("Range", "bytes=0-262143"))
        .mount(&s)
        .await;
    let session = Secret::new(format!(
        "{}/upload/youtube/v3/videos?upload_id=test",
        s.uri()
    ));
    assert_eq!(
        yt.reconcile(7, "UC-owner", &session, 1048576)
            .await
            .unwrap(),
        UploadReply::Offset(262144)
    );
}
#[tokio::test]
async fn unklarer_abschluss_und_processing_sind_keine_erfolge() {
    let s = MockServer::start().await;
    let yt = api(&s).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    let session = Secret::new(format!(
        "{}/upload/youtube/v3/videos?upload_id=test",
        s.uri()
    ));
    assert_eq!(
        yt.reconcile(7, "UC-owner", &session, 1024).await,
        Err(Error::Ambiguous)
    );
    Mock::given(method("GET")).and(path("/youtube/v3/videos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"v1","snippet":{"channelId":"UC-owner"},"processingDetails":{"processingStatus":"processing"}}]}))).mount(&s).await;
    assert_eq!(
        yt.processing(7, "UC-owner", "v1").await.unwrap(),
        Processing::Pending
    );
}
#[tokio::test]
async fn fremde_uploadadresse_bekommt_keinen_token() {
    let s = MockServer::start().await;
    let yt = api(&s).await;
    let session =
        Secret::new("http://169.254.169.254/upload/youtube/v3/videos?upload_id=secret".into());
    assert_eq!(
        yt.reconcile(7, "UC-owner", &session, 1).await,
        Err(Error::Protocol)
    );
    assert!(s.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn falscher_range_und_quota_werden_nicht_als_fortschritt_gespeichert() {
    for (status, range, expected) in [
        (308, Some("bytes=0-9999"), Error::Protocol),
        (308, Some("bytes=50-99"), Error::Protocol),
        (403, None, Error::Quota),
    ] {
        let server = MockServer::start().await;
        let yt = api(&server).await;
        let mut response = ResponseTemplate::new(status);
        if let Some(range) = range {
            response = response.insert_header("Range", range)
        } else {
            response =
                response.set_body_json(json!({"error":{"errors":[{"reason":"quotaExceeded"}]}}))
        }
        Mock::given(method("PUT"))
            .respond_with(response)
            .mount(&server)
            .await;
        let session = Secret::new(format!(
            "{}/upload/youtube/v3/videos?upload_id=protected",
            server.uri()
        ));
        assert_eq!(
            yt.reconcile(7, "UC-owner", &session, 100).await,
            Err(expected)
        );
    }
}
#[tokio::test]
async fn abgelehnte_verarbeitung_und_fremdes_video_bleiben_fehler() {
    for (status, channel, expected) in [
        ("failed", "UC-owner", Error::ProcessingFailed),
        ("terminated", "UC-owner", Error::ProcessingFailed),
        ("succeeded", "UC-other", Error::WrongChannel),
    ] {
        let server = MockServer::start().await;
        let yt = api(&server).await;
        Mock::given(method("GET")).and(path("/youtube/v3/videos")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"v1","snippet":{"channelId":channel},"processingDetails":{"processingStatus":status}}]}))).mount(&server).await;
        assert_eq!(yt.processing(7, "UC-owner", "v1").await, Err(expected));
    }
}
#[tokio::test]
async fn einmaliger_401_refresh_und_dauerhafter_widerruf() {
    let server = MockServer::start().await;
    let yt = api(&server).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(308))
        .expect(1)
        .mount(&server)
        .await;
    let session = Secret::new(format!(
        "{}/upload/youtube/v3/videos?upload_id=protected",
        server.uri()
    ));
    assert_eq!(
        yt.reconcile(7, "UC-owner", &session, 100).await.unwrap(),
        UploadReply::Offset(0)
    );
    server.verify().await;
    let server = MockServer::start().await;
    let yt = api(&server).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    let session = Secret::new(format!(
        "{}/upload/youtube/v3/videos?upload_id=protected",
        server.uri()
    ));
    assert_eq!(
        yt.reconcile(7, "UC-owner", &session, 100).await,
        Err(Error::NeedsReauth)
    );
    server.verify().await;
}

#[tokio::test]
async fn upload_ohne_fortschritt_gibt_den_worker_fuer_spaetere_wiederholung_frei() {
    let server = MockServer::start().await;
    let yt = api(&server).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(308))
        .expect(1)
        .mount(&server)
        .await;
    let session = Secret::new(format!(
        "{}/upload/youtube/v3/videos?upload_id=protected",
        server.uri()
    ));
    assert_eq!(
        yt.chunk(7, "UC-owner", &session, 16, 0, vec![7; 16]).await,
        Err(Error::Network)
    );
    server.verify().await;
}
