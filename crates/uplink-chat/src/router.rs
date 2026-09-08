use crate::{
    ChatHub, Platform,
    ereignis::{ArtFilter, StreamInfoPatch},
    hub::User,
    punkte::EinloesungStatus,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
type Error = (StatusCode, Json<serde_json::Value>);
fn error(code: StatusCode, text: &'static str) -> Error {
    (code, Json(json!({"error":text,"hinweis":text})))
}
pub fn router(hub: Arc<ChatHub>) -> Router {
    Router::new()
        .route("/dock/{page}", get(page))
        .route("/v1/chat/ws", get(ws))
        .route("/v1/chat/send", post(send))
        .route("/v1/stream-info", get(info_get).put(info_put))
        .route("/v1/stream-info/kategorien", get(categories))
        .route("/v1/points/einloesungen/{id}", post(points))
        .route("/v1/webhooks/kick", post(webhook))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let mut response = next.run(req).await;
                response
                    .headers_mut()
                    .insert("Cache-Control", "no-store".parse().expect("Header"));
                response
                    .headers_mut()
                    .insert("Referrer-Policy", "no-referrer".parse().expect("Header"));
                response
                    .headers_mut()
                    .insert("X-Content-Type-Options", "nosniff".parse().expect("Header"));
                response
            },
        ))
        .with_state(hub)
}
#[derive(Deserialize)]
struct DockQuery {
    t: Option<String>,
    seit: Option<u64>,
    r#gen: Option<String>,
    arten: Option<String>,
}
fn origin(hub: &ChatHub, headers: &HeaderMap, required: bool) -> Result<(), Error> {
    match headers.get("Origin").and_then(|v| v.to_str().ok()) {
        Some(o)
            if hub
                .config
                .allowed_origins
                .iter()
                .any(|allowed| allowed == o) =>
        {
            Ok(())
        }
        None if !required => Ok(()),
        _ => Err(error(
            StatusCode::FORBIDDEN,
            "Dieses Fenster hat keine Zugriffsberechtigung.",
        )),
    }
}
async fn authenticate(hub: &ChatHub, token: Option<&str>) -> Result<(u64, [u8; 32]), Error> {
    let token = token
        .filter(|t| {
            t.len() == 37
                && t.starts_with("dock_")
                && t[5..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "OBS-Adresse ist ungültig. Bitte im Dashboard neu erzeugen.",
            )
        })?;
    let _slot = hub.requests.try_acquire().map_err(|_| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "Zu viele Anfragen. Bitte kurz warten.",
        )
    })?;
    let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let user = tokio::time::timeout(Duration::from_secs(5), hub.identity.resolve(hash))
        .await
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Zugang konnte gerade nicht geprüft werden.",
            )
        })?
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Zugang konnte gerade nicht geprüft werden.",
            )
        })?;
    let id = user
        .filter(|u| u.enabled && u.streamer_id > 0 && u.streamer_id <= i64::MAX as u64)
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "OBS-Adresse ist nicht mehr gültig.",
            )
        })?
        .streamer_id;
    Ok((id, hash))
}
async fn header_user(hub: &Arc<ChatHub>, h: &HeaderMap, write: bool) -> Result<Arc<User>, Error> {
    origin(hub, h, write)?;
    let (id, _) = authenticate(hub, h.get("X-Dock-Token").and_then(|v| v.to_str().ok())).await?;
    let user = hub
        .ensure(id)
        .map_err(|e| error(StatusCode::SERVICE_UNAVAILABLE, e))?;
    if !user.access_confirmed() {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Chatzugang wird erneut geprüft.",
        ));
    }
    Ok(user)
}
async fn page(
    State(h): State<Arc<ChatHub>>,
    Path(page): Path<String>,
    Query(q): Query<DockQuery>,
) -> Result<Response, Error> {
    authenticate(&h, q.t.as_deref()).await?;
    let text = match page.as_str() {
        "chat" => include_str!("../../../web/docks/chat.html"),
        "activity" => include_str!("../../../web/docks/activity.html"),
        "points" => include_str!("../../../web/docks/points.html"),
        "stream-info" => include_str!("../../../web/docks/stream-info.html"),
        _ => {
            return Err(error(
                StatusCode::NOT_FOUND,
                "Fenster wurde nicht gefunden.",
            ));
        }
    };
    let mut response = Html(text).into_response();
    response.headers_mut().insert("Content-Security-Policy","default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src https: data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'".parse().expect("CSP"));
    Ok(response)
}
async fn ws(
    State(h): State<Arc<ChatHub>>,
    headers: HeaderMap,
    Query(q): Query<DockQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, Error> {
    origin(&h, &headers, true)?;
    let (id, hash) = authenticate(&h, q.t.as_deref()).await?;
    let user = h
        .ensure(id)
        .map_err(|e| error(StatusCode::SERVICE_UNAVAILABLE, e))?;
    if !user.access_confirmed() {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Chatzugang wird erneut geprüft.",
        ));
    }
    let permit = SocketPermit::acquire(user.clone(), h.config.max_sockets_per_user)?;
    let cursor = (q.r#gen, q.seit);
    let filter = ArtFilter::parse(q.arten.as_deref());
    Ok(upgrade
        .max_frame_size(8192)
        .max_message_size(8192)
        .on_upgrade(move |socket| socket_loop(h, user, hash, cursor, filter, socket, permit))
        .into_response())
}
struct SocketPermit(Arc<User>, tokio_util::sync::CancellationToken);
impl SocketPermit {
    fn acquire(u: Arc<User>, max: usize) -> Result<Self, Error> {
        u.sockets
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .map_err(|_| {
                error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Zu viele OBS-Fenster geöffnet.",
                )
            })?;
        let cancel = u.socket_cancel.lock().expect("Dockverbindungen").clone();
        Ok(Self(u, cancel))
    }
}
impl Drop for SocketPermit {
    fn drop(&mut self) {
        self.0.sockets.fetch_sub(1, Ordering::AcqRel);
        *self.0.last_seen.lock().expect("Aktivität") = Instant::now();
    }
}
async fn ws_send(socket: &mut WebSocket, value: &impl serde::Serialize) -> Result<(), ()> {
    let msg = serde_json::to_string(value).map_err(|_| ())?;
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(msg.into())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}
async fn socket_loop(
    h: Arc<ChatHub>,
    u: Arc<User>,
    hash: [u8; 32],
    cursor: (Option<String>, Option<u64>),
    filter: ArtFilter,
    mut socket: WebSocket,
    permit: SocketPermit,
) {
    let cancel = permit.1.clone();
    let session = async {
        let (mut rx, replay, gap) = u.resume(cursor.0.as_deref(), cursor.1);
        if ws_send(&mut socket,&json!({"typ":"status","generation":u.generation,"plattformen":h.status(&u),"nachlauf_unvollstaendig":gap})).await.is_err(){return}
        for frame in replay {
            if filter.passt(&frame.ereignis) && ws_send(&mut socket, &frame).await.is_err() {
                return;
            }
        }
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {biased;_=tick.tick()=>{
             let valid=tokio::time::timeout(Duration::from_secs(5),h.identity.resolve(hash)).await;
             if !matches!(valid,Ok(Ok(Some(identity)))if identity.enabled&&identity.streamer_id==u.id){
               let (code,reason)=if matches!(valid,Ok(Ok(_))){(1008,"OBS-Adresse ist nicht mehr gültig")}else{(1013,"Zugang konnte gerade nicht geprüft werden")};
               let _=tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Close(Some(axum::extract::ws::CloseFrame{code,reason:reason.into()})))).await;break
             }
             if ws_send(&mut socket,&json!({"typ":"status","generation":u.generation,"plattformen":h.status(&u)})).await.is_err(){break}
             let metrics=u.metrics.lock().expect("Kennzahlen").clone();if let Some(metrics)=metrics&& ws_send(&mut socket,&metrics).await.is_err(){break}
            },frame=rx.recv()=>{match frame{Ok(frame)=>if filter.passt(&frame.ereignis)&&ws_send(&mut socket,&frame).await.is_err(){break},Err(_)=>break}},incoming=socket.next()=>{match incoming{Some(Ok(Message::Close(_)))|Some(Err(_))|None=>break,Some(Ok(Message::Ping(v)))=>{if tokio::time::timeout(Duration::from_secs(5),socket.send(Message::Pong(v))).await.is_err(){break}},_=>{}}}}
        }
    };
    // Rotation und Sperre unterbrechen auch ausstehende Sends, Identitäts-
    // abfragen und Replay. Das Socketpermit bleibt bis dahin im Besitzer.
    tokio::select! { biased; _ = cancel.cancelled() => {}, _ = session => {} }
    drop(permit);
}
#[derive(Deserialize)]
struct SendBody {
    text: String,
}
async fn send(
    State(h): State<Arc<ChatHub>>,
    headers: HeaderMap,
    Json(body): Json<SendBody>,
) -> Result<Json<serde_json::Value>, Error> {
    let u = header_user(&h, &headers, true).await?;
    let _permit = u.actions.try_acquire().map_err(|_| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "Eine Aktion läuft bereits. Bitte kurz warten.",
        )
    })?;
    let text = body.text.trim();
    if text.is_empty() || text.chars().count() > 500 || text.chars().any(|c| c.is_control()) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Nachricht muss 1 bis 500 Zeichen ohne Steuerzeichen enthalten.",
        ));
    }
    let results = send_all(&u, text).await;
    if results.is_empty() {
        return Err(error(
            StatusCode::CONFLICT,
            "Keine verbundene Plattform kann die Nachricht gerade annehmen.",
        ));
    }
    Ok(Json(json!({"ergebnisse":results})))
}
async fn send_all(u: &User, text: &str) -> Vec<serde_json::Value> {
    let adapters = u.adapters.lock().expect("Adapter").clone();
    let errors = u.errors.lock().expect("Fehler").clone();
    futures::future::join_all(Platform::ALL.into_iter().map(|p| {
        let adapter = adapters.get(&p).cloned();
        let error = errors.get(&p).cloned();
        async move {
            if let Some(adapter) = adapter {
                let result = tokio::time::timeout(Duration::from_secs(15), adapter.senden(text)).await;
                let hint = match result {
                    Ok(Ok(())) => return Some(json!({"platform": p, "ok": true})),
                    Err(_) | Ok(Err(crate::adapter::ChatFehler::Netz(_))) =>
                        "Zustellung wurde nicht bestätigt. Bitte vor erneutem Senden im Chat prüfen.".into(),
                    Ok(Err(error)) => crate::supervisor::hinweis_fuer(&error),
                };
                Some(json!({"platform": p, "ok": false, "hinweis": hint}))
            } else {
                error.filter(|e| !matches!(e,
                    crate::adapter::ChatFehler::NichtVerbunden(_) |
                    crate::adapter::ChatFehler::NichtUnterstuetzt(_)))
                    .map(|e| json!({"platform": p, "ok": false,
                        "hinweis": crate::supervisor::hinweis_fuer(&e)}))
            }
        }
    })).await.into_iter().flatten().collect()
}

async fn info_get(
    State(h): State<Arc<ChatHub>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, Error> {
    let u = header_user(&h, &headers, false).await?;
    let _p = u
        .actions
        .try_acquire()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "Eine Aktion läuft bereits."))?;
    Ok(Json(
        json!({"ergebnisse":h.info.lesen_alle(u.id as i64).await}),
    ))
}
async fn info_put(
    State(h): State<Arc<ChatHub>>,
    headers: HeaderMap,
    Json(mut patch): Json<StreamInfoPatch>,
) -> Result<Json<serde_json::Value>, Error> {
    let u = header_user(&h, &headers, true).await?;
    let _p = u
        .actions
        .try_acquire()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "Eine Aktion läuft bereits."))?;
    if let Some(t) = &mut patch.title {
        *t = t.trim().to_string();
        if t.is_empty() || t.chars().count() > 140 || t.chars().any(char::is_control) {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "Titel muss 1 bis 140 Zeichen enthalten.",
            ));
        }
    }
    if patch.category_id.as_ref().is_some_and(|id| !identifier(id))
        || patch.tags.as_ref().is_some_and(|t| {
            t.len() > 10
                || t.iter().any(|s| {
                    s.is_empty()
                        || s.chars().count() > 25
                        || s.chars().any(|c| !c.is_alphanumeric())
                })
        })
        || patch.is_empty()
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Stream-Infos sind ungültig.",
        ));
    }
    Ok(Json(
        json!({"ergebnisse":h.info.setzen_alle(u.id as i64,&patch).await}),
    ))
}
#[derive(Deserialize)]
struct Search {
    q: String,
}
async fn categories(
    State(h): State<Arc<ChatHub>>,
    headers: HeaderMap,
    Query(q): Query<Search>,
) -> Result<Json<serde_json::Value>, Error> {
    let u = header_user(&h, &headers, false).await?;
    let _p = u
        .actions
        .try_acquire()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "Eine Aktion läuft bereits."))?;
    let q = q.q.trim();
    if q.chars().count() > 100 {
        return Err(error(StatusCode::BAD_REQUEST, "Suchtext ist zu lang."));
    }
    if q.chars().count() < 2 {
        return Ok(Json(json!({"ergebnisse":[]})));
    }
    Ok(Json(
        json!({"ergebnisse":h.info.kategorien(u.id as i64,q).await}),
    ))
}
#[derive(Deserialize)]
struct Redemption {
    platform: Option<Platform>,
    reward_id: String,
    status: EinloesungStatus,
}
fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
async fn points(
    State(h): State<Arc<ChatHub>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Redemption>,
) -> Result<Json<serde_json::Value>, Error> {
    let u = header_user(&h, &headers, true).await?;
    let _p = u
        .actions
        .try_acquire()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "Eine Aktion läuft bereits."))?;
    if !identifier(&id) || !identifier(&body.reward_id) {
        return Err(error(StatusCode::BAD_REQUEST, "Einlösung ist ungültig."));
    }
    let p = body.platform.unwrap_or(Platform::Twitch);
    let result = h
        .points
        .einloesung_setzen(u.id as i64, p, &id, &body.reward_id, body.status)
        .await;
    Ok(Json(match result {
        Ok(()) => json!({"platform":p,"ok":true}),
        Err(e) => json!({"platform":p,"ok":false,"hinweis":crate::supervisor::hinweis_fuer(&e)}),
    }))
}
async fn webhook(State(h): State<Arc<ChatHub>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let get = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let _p = match h.requests.try_acquire() {
        Ok(p) => p,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    };
    use crate::kick_webhook::WebhookAusgang::*;
    match h
        .kick
        .verarbeiten(
            get("Kick-Event-Message-Id"),
            get("Kick-Event-Message-Timestamp"),
            get("Kick-Event-Type"),
            get("Kick-Event-Signature"),
            &body,
        )
        .await
    {
        Angenommen | Verworfen => StatusCode::OK,
        SignaturFalsch => StatusCode::UNAUTHORIZED,
        Fehlerhaft => StatusCode::BAD_REQUEST,
        Ausgelastet => StatusCode::SERVICE_UNAVAILABLE,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrokerError, DockIdentity, DockUser, Grant, PlatformBroker};
    use futures::future::BoxFuture;
    use std::sync::atomic::AtomicBool;
    use tower::ServiceExt;
    pub(super) const TOKEN: &str = "dock_0123456789abcdef0123456789abcdef";
    pub(super) struct Identity(pub(super) AtomicBool);
    impl DockIdentity for Identity {
        fn resolve(&self, hash: [u8; 32]) -> BoxFuture<'_, Result<Option<DockUser>, BrokerError>> {
            Box::pin(async move {
                Ok((self.0.load(Ordering::Acquire)
                    && hash == <[u8; 32]>::from(Sha256::digest(TOKEN.as_bytes())))
                .then_some(DockUser {
                    streamer_id: 7,
                    enabled: true,
                }))
            })
        }
    }
    struct Broker;
    impl PlatformBroker for Broker {
        fn grant(&self, _: u64, _: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
            Box::pin(async { Err(BrokerError::Disconnected) })
        }
    }
    pub(super) fn testhub() -> (Arc<ChatHub>, Arc<Identity>) {
        let i = Arc::new(Identity(AtomicBool::new(true)));
        (
            ChatHub::new(crate::ChatConfig::default(), i.clone(), Arc::new(Broker)).unwrap(),
            i,
        )
    }
    #[tokio::test]
    async fn pages_require_persistent_identity_and_never_cache() {
        let (h, i) = testhub();
        let app = router(h.clone());
        for path in ["chat", "activity", "points", "stream-info"] {
            let res = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(format!("/dock/{path}?t={TOKEN}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(res.headers()["Cache-Control"], "no-store");
        }
        i.0.store(false, Ordering::Release);
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/dock/chat?t={TOKEN}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        h.shutdown().await;
    }
    #[tokio::test]
    async fn foreign_origin_cannot_write_and_empty_send_is_not_success() {
        let (h, _) = testhub();
        let app = router(h.clone());
        for (origin, expected) in [
            ("https://evil.example", StatusCode::FORBIDDEN),
            (
                "https://deutsche-deadlock-community.de",
                StatusCode::CONFLICT,
            ),
        ] {
            let res = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/v1/chat/send")
                        .header("Origin", origin)
                        .header("X-Dock-Token", TOKEN)
                        .header("Content-Type", "application/json")
                        .body(axum::body::Body::from(
                            r#"{"text":"Isolierte Testnachricht"}"#,
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), expected);
        }
        h.shutdown().await;
    }
    #[tokio::test]
    async fn token_never_accepted_as_streamer_id() {
        let (h, _) = testhub();
        assert!(authenticate(&h, Some("7")).await.is_err());
        assert!(
            authenticate(&h, Some("dock_ffffffffffffffffffffffffffffffff"))
                .await
                .is_err()
        );
        h.shutdown().await;
    }
}
#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::adapter::{ChatAdapter, ChatFehler};
    use futures::future::BoxFuture;
    use std::sync::atomic::AtomicUsize;
    struct A {
        p: Platform,
        calls: Arc<AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
        error: Option<ChatFehler>,
    }
    impl ChatAdapter for A {
        fn platform(&self) -> Platform {
            self.p
        }
        fn verbinden(&self) -> BoxFuture<'_, Result<(), ChatFehler>> {
            Box::pin(async { Ok(()) })
        }
        fn trennen(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
        fn verbunden(&self) -> bool {
            true
        }
        fn senden(&self, _: &str) -> BoxFuture<'_, Result<(), ChatFehler>> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.barrier.wait().await;
                if let Some(ref error) = self.error {
                    Err(error.clone())
                } else {
                    Ok(())
                }
            })
        }
    }
    #[tokio::test]
    async fn all_connected_platforms_send_concurrently_and_report_individual_failure() {
        let (h, _) = super::tests::testhub();
        let u = h.ensure(7).unwrap();
        h.shutdown().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        *u.adapters.lock().unwrap() = Platform::ALL[..3]
            .iter()
            .map(|&p| {
                (
                    p,
                    Arc::new(A {
                        p,
                        calls: calls.clone(),
                        barrier: barrier.clone(),
                        error: (p == Platform::Kick).then_some(ChatFehler::NeuAnmeldungNoetig(p)),
                    }) as Arc<dyn ChatAdapter>,
                )
            })
            .collect();
        let results =
            tokio::time::timeout(Duration::from_secs(1), send_all(&u, "Nur isolierte Mocks"))
                .await
                .expect("Alle Ziele müssen unabhängig gestartet werden");
        assert_eq!(calls.load(Ordering::Acquire), 3);
        assert_eq!(results.len(), 3);
        assert_eq!(results.iter().filter(|r| r["ok"] == true).count(), 2);
        assert_eq!(
            results.iter().find(|r| r["platform"] == "kick").unwrap()["ok"],
            false
        );
    }
    #[tokio::test]
    async fn unklarer_versand_warnt_vor_blinder_wiederholung() {
        let (h, _) = super::tests::testhub();
        let u = h.ensure(7).unwrap();
        h.shutdown().await;
        u.adapters.lock().unwrap().insert(
            Platform::YouTube,
            Arc::new(A {
                p: Platform::YouTube,
                calls: Arc::new(AtomicUsize::new(0)),
                barrier: Arc::new(tokio::sync::Barrier::new(1)),
                error: Some(ChatFehler::Netz(
                    "private Antwort darf nicht ins Dock".into(),
                )),
            }),
        );
        let results = send_all(&u, "Nur isolierte Mocks").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["ok"], false);
        let hint = results[0]["hinweis"].as_str().unwrap();
        assert!(hint.contains("vor erneutem Senden im Chat prüfen"));
        assert!(!hint.contains("private Antwort"));
    }
    #[test]
    fn socket_limit_is_atomic_and_released_on_drop() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (h, _) = super::tests::testhub();
            let u = h.ensure(7).unwrap();
            let first = SocketPermit::acquire(u.clone(), 1).unwrap();
            assert!(SocketPermit::acquire(u.clone(), 1).is_err());
            drop(first);
            assert!(SocketPermit::acquire(u, 1).is_ok());
            h.shutdown().await;
        });
    }
    #[tokio::test]
    async fn actual_websocket_replays_updates_and_rotation_closes_it() {
        use tokio_tungstenite::tungstenite::{Message as TMessage, client::IntoClientRequest};
        let (h, identity) = super::tests::testhub();
        let user = h.ensure(7).unwrap();
        assert_eq!(user.generation.len(), 32);
        let fixture = |title: &str| {
            crate::nachricht::Ereignis::Chat(serde_json::from_value(json!({"platform":"youtube","channel_id":"7","channel_login":"example","message_id":"same","sender_id":"9","sender_login":"viewer","sender_display":"Viewer","badges":[],"fragments":[{"art":"text","text":title}],"sent_at":"2026-09-08T10:00:00Z","is_action":false,"eigene":false})).unwrap())
        };
        user.bus.publish(fixture("first")).unwrap();
        user.bus.publish(fixture("update")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(h.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut request = format!(
            "ws://{addr}/v1/chat/ws?t={}&gen={}&seit=1",
            super::tests::TOKEN,
            user.generation
        )
        .into_client_request()
        .unwrap();
        request.headers_mut().insert(
            "Origin",
            "https://deutsche-deadlock-community.de".parse().unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let status = socket.next().await.unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_str(status.to_text().unwrap()).unwrap();
        assert_eq!(json["typ"], "status");
        let update = socket.next().await.unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_str(update.to_text().unwrap()).unwrap();
        assert_eq!(json["id"], 2);
        assert_eq!(json["ereignis"]["fragments"][0]["text"], "update");
        identity.0.store(false, Ordering::Release);
        let code = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if let Some(Ok(TMessage::Close(Some(frame)))) = socket.next().await {
                    break frame.code;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(u16::from(code), 1008);
        h.shutdown().await;
        server.abort();
        let _ = server.await;
    }
}

#[cfg(test)]
mod recreated_cursor_tests {
    use super::*;
    #[tokio::test]
    async fn websocket_new_bus_replays_all_events_despite_overlapping_old_cursor() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let (hub, _) = super::tests::testhub();
        let previous = hub.ensure(7).unwrap();
        let old_generation = previous.generation.clone();
        let event = |title| {
            crate::nachricht::Ereignis::Info(
                serde_json::from_value(json!({"platform":"twitch","channel_id":"7","title":title}))
                    .unwrap(),
            )
        };
        for title in ["A", "B", "C"] {
            previous.bus.publish(event(title)).unwrap();
        }
        hub.invalidate(7);
        let current = hub.ensure(7).unwrap();
        for title in ["D", "E", "F", "G"] {
            current.bus.publish(event(title)).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(hub.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut request = format!(
            "ws://{address}/v1/chat/ws?t={}&gen={old_generation}&seit=3",
            super::tests::TOKEN
        )
        .into_client_request()
        .unwrap();
        request.headers_mut().insert(
            "Origin",
            "https://deutsche-deadlock-community.de".parse().unwrap(),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let frames = tokio::time::timeout(Duration::from_secs(3), async {
            let mut frames = Vec::new();
            for _ in 0..5 {
                let message = socket.next().await.unwrap().unwrap();
                frames.push(
                    serde_json::from_str::<serde_json::Value>(message.to_text().unwrap()).unwrap(),
                );
            }
            frames
        })
        .await
        .unwrap();
        assert_eq!(frames[0]["generation"], current.generation);
        assert_eq!(frames[0]["nachlauf_unvollstaendig"], true);
        for (index, title) in ["D", "E", "F", "G"].iter().enumerate() {
            assert_eq!(frames[index + 1]["id"], index + 1);
            assert_eq!(frames[index + 1]["ereignis"]["title"], *title);
        }
        hub.shutdown().await;
        server.abort();
        let _ = server.await;
    }
}
