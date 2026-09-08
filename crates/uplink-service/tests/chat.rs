use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use uplink_chat::{BrokerError, Platform, PlatformBroker};
use uplink_service::{chat::BotBroker, crypto::Secret};

struct Reply {
    status: u16,
    body: String,
}
struct Mock {
    reply: Mutex<Reply>,
    calls: AtomicUsize,
}
async fn handler(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> impl IntoResponse {
    assert_eq!(
        headers.get("X-Internal-Token").unwrap(),
        "synthetic-broker-token"
    );
    mock.calls.fetch_add(1, Ordering::SeqCst);
    let reply = mock.reply.lock().unwrap();
    (
        StatusCode::from_u16(reply.status).unwrap(),
        reply.body.clone(),
    )
}
async fn broker() -> (BotBroker, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mock = Arc::new(Mock {
        reply: Mutex::new(Reply {
            status: 404,
            body: "{}".into(),
        }),
        calls: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/twitch/api/v2/internal/platform-token", get(handler))
        .with_state(mock.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        BotBroker::new(&base, Secret::new(b"synthetic-broker-token".to_vec())).unwrap(),
        mock,
        server,
    )
}
#[tokio::test]
async fn live_broker_errors_remain_function_specific_and_never_echo_private_body() {
    let (broker, mock, server) = broker().await;
    for (status, body, expected) in [
        (404, "{}", BrokerError::AccessUnconfirmed),
        (
            404,
            r#"{"error_code":"missing_scope","error":"synthetic-private-response"}"#,
            BrokerError::AccessUnconfirmed,
        ),
        (
            404,
            r#"{"error_code":"connection_missing"}"#,
            BrokerError::Disconnected,
        ),
        (409, "{}", BrokerError::NeedsReauth),
        (403, "{}", BrokerError::Unauthorized),
        (
            503,
            r#"{"error_code":"temporarily_unavailable"}"#,
            BrokerError::Unavailable,
        ),
        (
            200,
            "invalid synthetic-private-response",
            BrokerError::Unavailable,
        ),
    ] {
        *mock.reply.lock().unwrap() = Reply {
            status,
            body: body.into(),
        };
        let error = broker.grant(11, Platform::Twitch, false).await.unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("synthetic-private-response"));
    }
    let before = mock.calls.load(Ordering::SeqCst);
    assert_eq!(
        broker.grant(0, Platform::Twitch, false).await.unwrap_err(),
        BrokerError::Unauthorized
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), before);
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn broker_preserves_authoritative_identity_and_bounds_response() {
    let (broker, mock, server) = broker().await;
    *mock.reply.lock().unwrap()=Reply{status:200,body:r#"{"access_token":"synthetic-access","expires_at":"2099-01-01T00:00:00Z","platform_user_id":"11","platform_login":"unused-name","scopes":["user:read:chat"]}"#.into()};
    let grant = broker.grant(11, Platform::Twitch, true).await.unwrap();
    assert_eq!(grant.platform_user_id, "11");
    assert_eq!(
        broker.grant(12, Platform::Twitch, false).await.unwrap_err(),
        BrokerError::Unauthorized
    );
    assert!(!format!("{grant:?}").contains("synthetic-access"));
    *mock.reply.lock().unwrap() = Reply {
        status: 200,
        body: "x".repeat(256 * 1024 + 1),
    };
    assert_eq!(
        broker.grant(11, Platform::Twitch, false).await.unwrap_err(),
        BrokerError::Unavailable
    );
    server.abort();
    let _ = server.await;
}
#[test]
fn broker_refuses_remote_or_credential_bearing_configuration() {
    for url in [
        "https://example.org",
        "http://127.0.0.1/path",
        "http://synthetic@127.0.0.1",
        "http://127.0.0.1/?token=synthetic",
    ] {
        assert!(BotBroker::new(url, Secret::new(b"synthetic".to_vec())).is_err());
    }
}
