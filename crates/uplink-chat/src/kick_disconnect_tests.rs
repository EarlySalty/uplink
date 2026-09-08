use super::*;
use crate::{BrokerError, Grant, PlatformBroker};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct UplinkBroker {
    allowed: Arc<AtomicBool>,
}
impl PlatformBroker for UplinkBroker {
    fn grant(&self, _: u64, _: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
        Box::pin(async {
            if !self.allowed.load(Ordering::SeqCst) {
                return Err(BrokerError::Disconnected);
            }
            Ok(Grant {
                access_token: "synthetic-still-valid-platform-grant".into(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                platform_user_id: "123".into(),
                platform_login: "fixture".into(),
                scopes: vec!["events:subscribe".into(), "chat:write".into()],
            })
        })
    }
}

#[tokio::test]
async fn normal_uplink_disconnect_must_leave_a_way_to_cleanup_own_kick_subscription() {
    let server = MockServer::start().await;
    super::tests::initial_abgleich(&server).await;
    Mock::given(method("POST"))
        .and(path("/public/v1/events/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[
            {"name":"chat.message.sent","version":1,"subscription_id":"owned-chat"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/public/v1/events/subscriptions"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let allowed = Arc::new(AtomicBool::new(true));
    let source = Arc::new(TokenQuelle::from_broker(Arc::new(UplinkBroker {
        allowed: allowed.clone(),
    })));
    let factory = KickFabrik::mit_endpunkten(
        source.clone(),
        Arc::new(KickDrehkreuz::new("http://127.0.0.1:0")),
        KickEndpunkte { api: server.uri() },
    );
    let (input, _events) = mpsc::channel(8);
    let adapter = factory.bauen(7, Platform::Kick, input).await.unwrap();
    adapter.verbinden().await.unwrap();

    // The new Bot begin_disconnect disables its own Uplink broker access before
    // remote DELETE. No external revocation, expiry or platform error occurs.
    allowed.store(false, Ordering::SeqCst);
    // Exact normal ChatHub::reconcile ordering: discard cached grant, broker
    // reports Disconnected, then disconnect and discard the old adapter.
    source.recheck(7, Platform::Kick);
    assert!(source.zugang(7, Platform::Kick).await.is_err());
    assert!(adapter.senden("nicht mehr erlaubt").await.is_err());
    adapter.trennen().await;
    assert!(adapter.verbinden().await.is_err());
    drop(adapter);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let requests = server.received_requests().await.unwrap();
    let deleted = requests.iter().any(|r| {
        r.method == "DELETE"
            && r.url
                .query_pairs()
                .any(|(name, value)| name == "id" && value == "owned-chat")
    });
    assert!(
        deleted,
        "Our normal disconnect disabled the only credential path before cleanup: no DELETE for an existing owned subscription, although the platform credential remains valid"
    );
    assert!(!requests.iter().any(|r| r.url.path() == "/public/v1/chat"));
}

#[tokio::test]
async fn old_cleanup_finishes_before_a_new_connection_creates_its_subscription() {
    use std::sync::atomic::AtomicUsize;
    let server = MockServer::start().await;
    super::tests::initial_abgleich(&server).await;
    let created = Arc::new(AtomicUsize::new(0));
    let count = created.clone();
    Mock::given(method("POST"))
        .and(path("/public/v1/events/subscriptions"))
        .respond_with(move |_: &wiremock::Request| {
            let id = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                "old-owned"
            } else {
                "new-owned"
            };
            ResponseTemplate::new(200).set_body_json(json!({"data":[
                {"name":"chat.message.sent","version":1,"subscription_id":id}
            ]}))
        })
        .expect(2)
        .mount(&server)
        .await;
    let deletion_started = Arc::new(tokio::sync::Notify::new());
    let started = deletion_started.clone();
    let deleted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let ids = deleted.clone();
    Mock::given(method("DELETE"))
        .and(path("/public/v1/events/subscriptions"))
        .respond_with(move |request: &wiremock::Request| {
            let id = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "id")
                .unwrap()
                .1
                .into_owned();
            ids.lock().unwrap().push(id.clone());
            if id == "old-owned" {
                started.notify_one();
            }
            ResponseTemplate::new(204).set_delay(Duration::from_millis(150))
        })
        .mount(&server)
        .await;
    let allowed = Arc::new(AtomicBool::new(true));
    let source = Arc::new(TokenQuelle::from_broker(Arc::new(UplinkBroker {
        allowed: allowed.clone(),
    })));
    let factory = KickFabrik::mit_endpunkten(
        source.clone(),
        Arc::new(KickDrehkreuz::new("http://127.0.0.1:0")),
        KickEndpunkte { api: server.uri() },
    );
    let (input, _events) = mpsc::channel(8);
    let old = factory
        .bauen(7, Platform::Kick, input.clone())
        .await
        .unwrap();
    old.verbinden().await.unwrap();
    allowed.store(false, Ordering::SeqCst);
    source.recheck(7, Platform::Kick);
    drop(old);
    tokio::time::timeout(Duration::from_secs(2), deletion_started.notified())
        .await
        .unwrap();
    // The old owner's DELETE is in flight. A fresh permitted connection
    // may reserve the owner, but cannot create while cleanup holds its lock.
    allowed.store(true, Ordering::SeqCst);
    source.recheck(7, Platform::Kick);
    let current = factory.bauen(7, Platform::Kick, input).await.unwrap();
    current.verbinden().await.unwrap();
    assert_eq!(created.load(Ordering::SeqCst), 2);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(*deleted.lock().unwrap(), ["old-owned"]);
    assert!(current.verbunden());
    current.trennen().await;
    assert_eq!(*deleted.lock().unwrap(), ["old-owned", "new-owned"]);
    server.verify().await;
}

#[tokio::test]
async fn external_revocation_is_a_cleanup_error_and_never_reopens_normal_access() {
    let server = MockServer::start().await;
    super::tests::initial_abgleich(&server).await;
    Mock::given(method("POST"))
        .and(path("/public/v1/events/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[
            {"name":"chat.message.sent","version":1,"subscription_id":"owned-revoked"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let allowed = Arc::new(AtomicBool::new(true));
    let source = Arc::new(TokenQuelle::from_broker(Arc::new(UplinkBroker {
        allowed: allowed.clone(),
    })));
    let factory = KickFabrik::mit_endpunkten(
        source.clone(),
        Arc::new(KickDrehkreuz::new("http://127.0.0.1:0")),
        KickEndpunkte { api: server.uri() },
    );
    let (input, _events) = mpsc::channel(8);
    let adapter = factory.bauen(7, Platform::Kick, input).await.unwrap();
    adapter.verbinden().await.unwrap();
    Mock::given(method("POST"))
        .and(path("/oauth/token/introspect"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(&server)
        .await;
    allowed.store(false, Ordering::SeqCst);
    source.recheck(7, Platform::Kick);
    adapter.trennen().await;
    assert_eq!(
        adapter.ende_grund(),
        Some(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
    );
    assert!(!adapter.verbunden());
    assert!(adapter.senden("nicht erlaubt").await.is_err());
    assert!(adapter.verbinden().await.is_err());
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests
            .iter()
            .any(|r| r.method == "DELETE" || r.url.path() == "/public/v1/chat")
    );
    server.verify().await;
}

#[tokio::test]
async fn normal_broker_renewal_reaches_the_owner_before_disconnect() {
    broker_renewal_disconnect(false).await;
}

#[tokio::test]
async fn renewed_grant_of_another_app_cannot_take_over_old_cleanup() {
    broker_renewal_disconnect(true).await;
}

async fn broker_renewal_disconnect(changed_app: bool) {
    // Real TokenQuelle -> HTTP broker response -> existing owner, with no
    // test-only notification or extra cleanup refresh request.
    let server = MockServer::start().await;
    super::tests::initial_abgleich(&server).await;
    let broker_allowed = Arc::new(AtomicBool::new(true));
    let broker_renewed = Arc::new(AtomicBool::new(false));
    let allowed = broker_allowed.clone();
    let renewed = broker_renewed.clone();
    Mock::given(method("GET"))
        .and(path(crate::token::PFAD))
        .respond_with(move |_: &wiremock::Request| {
            if !allowed.load(Ordering::SeqCst) {
                return ResponseTemplate::new(404);
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "access_token": if renewed.load(Ordering::SeqCst) { "synthetic-new" } else { "synthetic-old" },
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                "platform_user_id":"123", "platform_login":"fixture",
                "scopes":["events:subscribe","chat:write"]
            }))
        })
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/v1/events/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[
            {"name":"chat.message.sent","version":1,"subscription_id":"owned-before-renewal"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/public/v1/events/subscriptions"))
        .and(header("authorization", "Bearer synthetic-new"))
        .respond_with(ResponseTemplate::new(204))
        .expect(if changed_app { 0 } else { 1 })
        .mount(&server)
        .await;
    let source = Arc::new(TokenQuelle::new(&server.uri(), "synthetic-internal"));
    let factory = KickFabrik::mit_endpunkten(
        source.clone(),
        Arc::new(KickDrehkreuz::new("http://127.0.0.1:0")),
        KickEndpunkte { api: server.uri() },
    );
    let (input, _events) = mpsc::channel(8);
    let adapter = factory.bauen(7, Platform::Kick, input).await.unwrap();
    adapter.verbinden().await.unwrap();
    broker_renewed.store(true, Ordering::SeqCst);
    source.recheck(7, Platform::Kick);
    source.zugang(7, Platform::Kick).await.unwrap();
    if changed_app {
        Mock::given(method("POST"))
            .and(path("/oauth/token/introspect"))
            .and(header("authorization", "Bearer synthetic-new"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":{
                "active":true, "client_id":"different-app", "token_type":"user"
            }})))
            .with_priority(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/oauth/token/introspect"))
        .and(header("authorization", "Bearer synthetic-old"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(&server)
        .await;
    broker_allowed.store(false, Ordering::SeqCst);
    source.recheck(7, Platform::Kick);
    assert!(source.zugang(7, Platform::Kick).await.is_err());
    adapter.trennen().await;
    if changed_app {
        assert!(matches!(adapter.ende_grund(), Some(ChatFehler::Netz(_))));
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.method == "DELETE")
        );
    } else {
        assert_eq!(adapter.ende_grund(), None);
    }
    server.verify().await;
}
