use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::Platform;
use crate::adapter::{AdapterFabrik, ChatAdapter, ChatFehler, stub};
use crate::helix::fehler_aus_token;
use crate::kick_webhook::KickDrehkreuz;
use crate::nachricht::Ereignis;
use crate::token::TokenQuelle;

#[path = "kick_subscriptions.rs"]
mod subscriptions;

const KICK_API: &str = "https://api.kick.com";
const FRIST: Duration = Duration::from_secs(10);
const SENDE_ABSTAND: Duration = Duration::from_millis(300);
const EINGERICHTET_FRIST: Duration = Duration::from_secs(20);

const ABOS: &[(&str, u32)] = &[
    ("chat.message.sent", 1),
    ("channel.followed", 1),
    ("channel.subscription.new", 1),
    ("channel.subscription.gifts", 1),
    ("channel.subscription.renewal", 1),
];

#[derive(Debug, Clone)]
pub struct KickEndpunkte {
    pub api: String,
}

impl Default for KickEndpunkte {
    fn default() -> Self {
        Self {
            api: KICK_API.into(),
        }
    }
}

pub struct KickFabrik {
    abos: Arc<subscriptions::Verwaltung>,
    quelle: Arc<TokenQuelle>,
    endpunkte: KickEndpunkte,
    drehkreuz: Arc<KickDrehkreuz>,
    http: reqwest::Client,
    eingerichtet_cache:
        std::sync::Mutex<std::collections::HashMap<i64, (bool, std::time::Instant)>>,
}

impl KickFabrik {
    pub fn new(quelle: Arc<TokenQuelle>, drehkreuz: Arc<KickDrehkreuz>) -> Self {
        Self::mit_endpunkten(quelle, drehkreuz, KickEndpunkte::default())
    }

    pub fn mit_endpunkten(
        quelle: Arc<TokenQuelle>,
        drehkreuz: Arc<KickDrehkreuz>,
        endpunkte: KickEndpunkte,
    ) -> Self {
        Self {
            abos: subscriptions::Verwaltung::new(),
            quelle,
            endpunkte,
            drehkreuz,
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(FRIST)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest-Client ohne Sonderoptionen"),
            eingerichtet_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn eingerichtet_gemerkt(&self, streamer_id: i64) -> Option<bool> {
        let cache = self.eingerichtet_cache.lock().expect("eingerichtet-Cache");
        let (wert, seit) = cache.get(&streamer_id)?;
        if seit.elapsed() > EINGERICHTET_FRIST {
            return None;
        }
        Some(*wert)
    }

    fn eingerichtet_merken(&self, streamer_id: i64, wert: bool) {
        self.eingerichtet_cache
            .lock()
            .expect("eingerichtet-Cache")
            .insert(streamer_id, (wert, std::time::Instant::now()));
    }
}

impl AdapterFabrik for KickFabrik {
    fn eingerichtet(&self, streamer_id: i64, platform: Platform) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            if platform != Platform::Kick {
                return false;
            }
            if let Some(gemerkt) = self.eingerichtet_gemerkt(streamer_id) {
                return gemerkt;
            }
            let wert = matches!(
                self.quelle.zugang(streamer_id, Platform::Kick).await,
                Ok(_) | Err(crate::token::TokenFehler::NeuAnmeldungNoetig(_))
            );
            self.eingerichtet_merken(streamer_id, wert);
            wert
        })
    }

    fn bauen(
        &self,
        streamer_id: i64,
        platform: Platform,
        eingang: mpsc::Sender<Ereignis>,
    ) -> BoxFuture<'_, Result<Arc<dyn ChatAdapter>, ChatFehler>> {
        Box::pin(async move {
            if platform != Platform::Kick {
                return stub(platform);
            }
            let zugang = self
                .quelle
                .zugang(streamer_id, Platform::Kick)
                .await
                .map_err(fehler_aus_token)?;
            if !zugang.scopes.iter().any(|s| s == "events:subscribe") {
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
            }
            if zugang
                .platform_user_id
                .parse::<i64>()
                .ok()
                .is_none_or(|id| id <= 0)
            {
                return Err(ChatFehler::Abgelehnt(
                    "Kick-Konto ohne gültige Nutzer-ID. Chat ist nicht möglich.".into(),
                ));
            }
            let abos = self
                .abos
                .belegen(
                    streamer_id,
                    zugang.platform_user_id.clone(),
                    self.quelle.clone(),
                    self.endpunkte.clone(),
                    self.http.clone(),
                )
                .await?;
            let adapter: Arc<dyn ChatAdapter> = Arc::new(KickAdapter {
                abos,
                streamer_id,
                broadcaster_user_id: zugang.platform_user_id.clone(),
                channel_login: zugang.platform_login.clone(),
                quelle: self.quelle.clone(),
                drehkreuz: self.drehkreuz.clone(),
                eingang,
                verbunden: AtomicBool::new(false),
                zuletzt_gesendet: Mutex::new(None),
                ende_grund: std::sync::Mutex::new(None),
                registrierungs_token: AtomicU64::new(0),
            });
            Ok(adapter)
        })
    }
}

pub struct KickAdapter {
    abos: Arc<subscriptions::Besitzer>,
    streamer_id: i64,
    broadcaster_user_id: String,
    channel_login: String,
    quelle: Arc<TokenQuelle>,
    drehkreuz: Arc<KickDrehkreuz>,
    eingang: mpsc::Sender<Ereignis>,
    verbunden: AtomicBool,
    zuletzt_gesendet: Mutex<Option<tokio::time::Instant>>,
    ende_grund: std::sync::Mutex<Option<ChatFehler>>,
    registrierungs_token: AtomicU64,
}

impl KickAdapter {
    async fn drosseln(&self) {
        let mut zuletzt = self.zuletzt_gesendet.lock().await;
        if let Some(vorher) = *zuletzt {
            let seit = vorher.elapsed();
            if seit < SENDE_ABSTAND {
                tokio::time::sleep(SENDE_ABSTAND - seit).await;
            }
        }
        *zuletzt = Some(tokio::time::Instant::now());
    }

    fn auth_ende_merken(&self, fehler: &ChatFehler) {
        if matches!(fehler, ChatFehler::NeuAnmeldungNoetig(_)) {
            *self.ende_grund.lock().expect("ende_grund") = Some(fehler.clone());
            self.verbunden.store(false, Ordering::SeqCst);
        }
    }

    async fn senden_inner(&self, text: &str) -> Result<(), ChatFehler> {
        if !self
            .quelle
            .zugang(self.streamer_id, Platform::Kick)
            .await
            .map_err(fehler_aus_token)?
            .scopes
            .iter()
            .any(|s| s == "chat:write")
        {
            return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
        }
        self.drosseln().await;
        let mut koerper = json!({ "content": text, "type": "user" });
        if let Ok(id) = self.broadcaster_user_id.parse::<i64>() {
            koerper["broadcaster_user_id"] = json!(id);
        }
        let (status, antwort) = self
            .abos
            .anfrage(Method::POST, "/public/v1/chat", &[], Some(&koerper))
            .await?;
        match status {
            s if s.is_success() => match antwort.pointer("/data/is_sent").and_then(Value::as_bool) {
                Some(true) => Ok(()),
                Some(false) => Err(ChatFehler::Verworfen("von Kick nicht zugestellt".into())),
                None => Err(ChatFehler::Netz(
                    "Kick hat die Zustellung nicht bestätigt. Vor erneutem Senden den Chat prüfen."
                        .into(),
                )),
            },
            StatusCode::FORBIDDEN => Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick)),
            sonst => Err(ChatFehler::Netz(format!("chat: HTTP {sonst}"))),
        }
    }
}

impl ChatAdapter for KickAdapter {
    fn account_id(&self) -> Option<&str> {
        Some(&self.broadcaster_user_id)
    }
    fn platform(&self) -> Platform {
        Platform::Kick
    }

    fn verbinden(&self) -> BoxFuture<'_, Result<(), ChatFehler>> {
        Box::pin(async move {
            if self.verbunden.load(Ordering::SeqCst) {
                return Ok(());
            }
            *self.ende_grund.lock().expect("ende_grund") = None;
            let token = self.drehkreuz.registrieren(
                &self.broadcaster_user_id,
                self.eingang.clone(),
                &self.channel_login,
            );
            self.registrierungs_token.store(token, Ordering::SeqCst);
            match self.abos.verbinden().await {
                Ok(()) => {
                    self.verbunden.store(true, Ordering::SeqCst);
                    tracing::info!(
                        streamer_id = self.streamer_id,
                        kanal = %self.channel_login,
                        "Kick-Chat: verbunden"
                    );
                    Ok(())
                }
                Err(fehler) => {
                    self.drehkreuz.abmelden(&self.broadcaster_user_id, token);
                    self.auth_ende_merken(&fehler);
                    Err(fehler)
                }
            }
        })
    }

    fn trennen(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let token = self.registrierungs_token.load(Ordering::SeqCst);
            self.drehkreuz.abmelden(&self.broadcaster_user_id, token);
            self.verbunden.store(false, Ordering::SeqCst);
            if let Err(error) = self.abos.trennen().await {
                *self.ende_grund.lock().expect("ende_grund") = Some(error);
            }
        })
    }

    fn senden(&self, text: &str) -> BoxFuture<'_, Result<(), ChatFehler>> {
        let text = text.trim().to_string();
        Box::pin(async move {
            if text.is_empty() {
                return Err(ChatFehler::Verworfen("leere Nachricht".into()));
            }
            let ergebnis = self.senden_inner(&text).await;
            if let Err(fehler) = &ergebnis {
                self.auth_ende_merken(fehler);
            }
            ergebnis
        })
    }

    fn verbunden(&self) -> bool {
        self.verbunden.load(Ordering::SeqCst)
    }

    fn ende_grund(&self) -> Option<ChatFehler> {
        self.ende_grund.lock().expect("ende_grund").clone()
    }
}

impl Drop for KickFabrik {
    fn drop(&mut self) {
        self.abos.stoppen();
    }
}

impl Drop for KickAdapter {
    fn drop(&mut self) {
        self.abos.freigeben();
        self.drehkreuz.abmelden(
            &self.broadcaster_user_id,
            self.registrierungs_token.load(Ordering::Acquire),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::PFAD;
    use chrono::Utc;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn bot_mit_kick_token(server: &MockServer, erwartet: u64) {
        initial_abgleich(server).await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("platform", "kick"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "kick-acc",
                "expires_at": (Utc::now() + chrono::Duration::hours(3)).to_rfc3339(),
                "platform_user_id": "123",
                "platform_login": "earlysalty",
                "scopes": ["chat:write", "events:subscribe"]
            })))
            .expect(erwartet)
            .mount(server)
            .await;
    }

    pub(super) async fn initial_abgleich(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/oauth/token/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data":{"active":true,"client_id":"our-app","token_type":"user"}}),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
            .with_priority(10)
            .mount(server)
            .await;
    }

    async fn bestand_nach_erster_liste(server: &MockServer, id: &str) {
        let id = id.to_owned();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        Mock::given(method("GET")).and(path("/public/v1/events/subscriptions"))
            .respond_with(move |_: &wiremock::Request| {
                let rows = if calls.fetch_add(1, Ordering::SeqCst) == 0 { json!([]) } else {
                    json!([{"id":id,"app_id":"our-app","broadcaster_user_id":123,"event":"chat.message.sent","method":"webhook","version":1}])
                };
                ResponseTemplate::new(200).set_body_json(json!({"data":rows}))
            }).mount(server).await;
    }

    fn fabrik(server: &MockServer) -> KickFabrik {
        KickFabrik::mit_endpunkten(
            Arc::new(TokenQuelle::new(&server.uri(), "intern")),
            Arc::new(KickDrehkreuz::new(&server.uri())),
            KickEndpunkte { api: server.uri() },
        )
    }

    #[tokio::test]
    async fn verbinden_legt_abos_an_und_registriert() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .and(header("Authorization", "Bearer kick-acc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "name": "chat.message.sent", "version": 1, "subscription_id": "s-1" },
                    { "name": "channel.followed", "version": 1, "subscription_id": "s-2" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        adapter.verbinden().await.expect("verbunden");
        assert!(adapter.verbunden());
        server.verify().await;
    }

    #[tokio::test]
    async fn senden_ruft_chat_und_wertet_is_sent() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/chat"))
            .and(header("Authorization", "Bearer kick-acc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "is_sent": true, "message_id": "m-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        adapter.senden("moin").await.expect("gesendet");
    }

    #[tokio::test]
    async fn erste_401_holt_token_neu_und_wiederholt() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 2).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/chat"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/public/v1/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "is_sent": true, "message_id": "m-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        adapter.senden("moin").await.expect("nach Wiederholung ok");
        server.verify().await;
    }

    #[tokio::test]
    async fn zweite_401_heisst_neu_anmelden() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 2).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/chat"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        assert_eq!(
            adapter.senden("moin").await.err(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
        );
    }

    #[tokio::test]
    async fn abos_bleiben_bei_fehlgeschlagenem_delete_erhalten() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        bestand_nach_erster_liste(&server, "s-1").await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [ { "name": "chat.message.sent", "version": 1, "subscription_id": "s-1" } ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        adapter.verbinden().await.expect("verbunden");
        adapter.trennen().await;
        adapter.trennen().await;
        server.verify().await;
    }

    #[tokio::test]
    async fn verworfener_adapter_behaelt_cleanup_besitzer() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        bestand_nach_erster_liste(&server, "retained").await;
        Mock::given(method("POST")).and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"name":"chat.message.sent","version":1,"subscription_id":"retained"}]})))
            .mount(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        adapter.verbinden().await.unwrap();
        adapter.trennen().await;
        drop(adapter);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let deletes = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method == "DELETE")
            .count();
        assert!(deletes > 1, "Cleanup must outlive the discarded adapter");
    }

    fn remote_abo(id: &str, app: &str, konto: i64, event: &str) -> Value {
        json!({"id":id,"app_id":app,"broadcaster_user_id":konto,"event":event,"method":"webhook","version":1})
    }

    #[tokio::test]
    async fn unklare_post_antwort_wird_vor_neuem_post_abgeglichen() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        let remote = Arc::new(AtomicBool::new(false));
        let r = remote.clone();
        Mock::given(method("GET"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(move |_: &wiremock::Request| {
                let mut rows = vec![
                    remote_abo("foreign-app", "other-app", 123, "chat.message.sent"),
                    remote_abo("foreign-user", "our-app", 456, "chat.message.sent"),
                    remote_abo("foreign-event", "our-app", 123, "livestream.status.updated"),
                ];
                if r.load(Ordering::SeqCst) {
                    rows.push(remote_abo("uncertain", "our-app", 123, "chat.message.sent"));
                }
                ResponseTemplate::new(200).set_body_json(json!({"data":rows}))
            })
            .mount(&server)
            .await;
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = posts.clone();
        let r = remote.clone();
        Mock::given(method("POST")).and(path("/public/v1/events/subscriptions"))
            .respond_with(move |_: &wiremock::Request| {
                assert!(!r.swap(true, Ordering::SeqCst), "No create before confirmed cleanup");
                if p.fetch_add(1, Ordering::SeqCst) == 0 { ResponseTemplate::new(500) } else {
                    ResponseTemplate::new(200).set_body_json(json!({"data":[{"name":"chat.message.sent","subscription_id":"uncertain"}]}))
                }
            }).mount(&server).await;
        let r = remote.clone();
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(move |request: &wiremock::Request| {
                let ids: Vec<_> = request
                    .url
                    .query_pairs()
                    .filter(|(k, _)| k == "id")
                    .map(|(_, v)| v.into_owned())
                    .collect();
                assert_eq!(ids, ["uncertain"]);
                r.store(false, Ordering::SeqCst);
                ResponseTemplate::new(204)
            })
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        assert!(adapter.verbinden().await.is_err());
        adapter.verbinden().await.unwrap();
        assert_eq!(posts.load(Ordering::SeqCst), 2);
        assert!(adapter.verbunden());
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn verlorene_delete_antwort_wird_durch_remote_leerstand_geklaert() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data":[{"name":"chat.message.sent","subscription_id":"gone"}]}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        adapter.verbinden().await.unwrap();
        adapter.trennen().await;
        drop(adapter);
        // Initial GET and reconciliation return an authoritative empty list.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .filter(|r| r.method == "GET" && r.url.path() == "/public/v1/events/subscriptions")
                .count()
                >= 2
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn abgebrochener_post_behaelt_remote_cleanup_besitzer() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        let accepted = Arc::new(AtomicBool::new(false));
        let r = accepted.clone();
        Mock::given(method("POST")).and(path("/public/v1/events/subscriptions"))
            .respond_with(move |_: &wiremock::Request| {
                r.store(true,Ordering::SeqCst);
                ResponseTemplate::new(200).set_delay(Duration::from_secs(3)).set_body_json(json!({"data":[{"name":"chat.message.sent","subscription_id":"cancelled"}]}))
            }).expect(1).mount(&server).await;
        let r = accepted.clone();
        Mock::given(method("GET")).and(path("/public/v1/events/subscriptions"))
            .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({"data":if r.load(Ordering::SeqCst) {vec![remote_abo("cancelled","our-app",123,"chat.message.sent")]} else {vec![]}})))
            .mount(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .and(query_param("id", "cancelled"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        let task = tokio::spawn(async move { adapter.verbinden().await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !accepted.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|r| r.method == "DELETE")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn unbekannter_remote_bestand_sperrt_erneute_anlage() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("GET"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        assert!(adapter.verbinden().await.is_err());
        assert!(adapter.verbinden().await.is_err());
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.method == "POST" && r.url.path() == "/public/v1/events/subscriptions")
        );
    }

    #[tokio::test]
    async fn geaenderte_app_uebernimmt_keine_alten_abos() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        let changed = Arc::new(AtomicBool::new(false));
        let c = changed.clone();
        Mock::given(method("POST")).and(path("/oauth/token/introspect"))
            .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({"data":{"active":true,"client_id":if c.load(Ordering::SeqCst){"new-app"}else{"our-app"},"token_type":"user"}})))
            .with_priority(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data":[{"name":"chat.message.sent","subscription_id":"old-app-id"}]}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        adapter.verbinden().await.unwrap();
        changed.store(true, Ordering::SeqCst);
        adapter.trennen().await;
        assert!(adapter.verbinden().await.is_err());
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.method == "DELETE")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn zu_grosse_abonnementliste_loest_keine_mutation_aus() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        let rows: Vec<_> = (0..257)
            .map(|i| remote_abo(&format!("id-{i}"), "our-app", 123, "chat.message.sent"))
            .collect();
        Mock::given(method("GET"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":rows})))
            .mount(&server)
            .await;
        let factory = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = factory.bauen(7, Platform::Kick, tx).await.unwrap();
        assert!(adapter.verbinden().await.is_err());
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path() == "/public/v1/events/subscriptions" && r.method != "GET")
        );
    }

    #[tokio::test]
    async fn dauerhafte_401_beim_senden_beendet_adapter() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 2).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [ { "name": "chat.message.sent", "version": 1, "subscription_id": "s-1" } ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/public/v1/chat"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        adapter.verbinden().await.expect("verbunden");
        assert!(adapter.verbunden());
        assert_eq!(
            adapter.senden("moin").await.err(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
        );
        assert_eq!(
            adapter.ende_grund(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
        );
        assert!(!adapter.verbunden());
    }

    #[tokio::test]
    async fn fehlender_chat_abo_ist_verbindungsfehler() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "name": "chat.message.sent", "version": 1, "error": "insufficient scope" },
                    { "name": "channel.followed", "version": 1, "subscription_id": "s-2" }
                ]
            })))
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        let fehler = adapter.verbinden().await.expect_err("chat-Abo fehlt");
        assert!(matches!(fehler, ChatFehler::Abgelehnt(_)));
        assert!(!adapter.verbunden());
    }

    #[tokio::test]
    async fn fehlende_activity_abos_lassen_chat_laufen() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "name": "chat.message.sent", "version": 1, "subscription_id": "s-1" },
                    { "name": "channel.followed", "version": 1, "error": "not available" }
                ]
            })))
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik.bauen(7, Platform::Kick, tx).await.expect("Adapter");
        adapter.verbinden().await.expect("chat laeuft trotzdem");
        assert!(adapter.verbunden());
    }

    #[tokio::test]
    async fn nicht_numerische_id_wird_abgewiesen() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("platform", "kick"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "kick-acc",
                "expires_at": (Utc::now() + chrono::Duration::hours(3)).to_rfc3339(),
                "platform_user_id": "nicht-numerisch",
                "platform_login": "earlysalty",
                "scopes": ["chat:write", "events:subscribe"]
            })))
            .mount(&server)
            .await;
        let fabrik = fabrik(&server);
        let (tx, _rx) = mpsc::channel(4);
        let ergebnis = fabrik.bauen(7, Platform::Kick, tx).await;
        assert!(matches!(ergebnis, Err(ChatFehler::Abgelehnt(_))));
    }
    #[tokio::test]
    async fn senden_ohne_is_sent_ist_keine_bestaetigung() {
        let server = MockServer::start().await;
        bot_mit_kick_token(&server, 1).await;
        Mock::given(method("POST"))
            .and(path("/public/v1/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "message_id": "m-1" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik(&server)
            .bauen(7, Platform::Kick, tx)
            .await
            .expect("Adapter");
        assert!(adapter.senden("moin").await.is_err());
    }
}

#[cfg(test)]
mod independent_review_probes {
    use super::*;
    use crate::{BrokerError, Grant, PlatformBroker};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    struct Broker;
    impl PlatformBroker for Broker {
        fn grant(&self, _: u64, _: Platform, _: bool) -> BoxFuture<'_, Result<Grant, BrokerError>> {
            Box::pin(async {
                Ok(Grant {
                    access_token: "synthetic-kick-token".into(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                    platform_user_id: "123".into(),
                    platform_login: "synthetic".into(),
                    scopes: vec!["events:subscribe".into()],
                })
            })
        }
    }
    #[tokio::test]
    async fn review_failed_chat_subscription_must_cleanup_successful_activity_subscription() {
        let server = MockServer::start().await;
        super::tests::initial_abgleich(&server).await;
        Mock::given(method("POST")).and(path("/public/v1/events/subscriptions")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"name":"channel.followed","subscription_id":"allocated-follow"},{"name":"chat.message.sent","error":"denied"}]}))).mount(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/public/v1/events/subscriptions"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let factory = KickFabrik::mit_endpunkten(
            Arc::new(TokenQuelle::from_broker(Arc::new(Broker))),
            Arc::new(KickDrehkreuz::new("http://127.0.0.1:0")),
            KickEndpunkte { api: server.uri() },
        );
        let (sender, _receiver) = mpsc::channel(10);
        let adapter = factory.bauen(7, Platform::Kick, sender).await.unwrap();
        assert!(adapter.verbinden().await.is_err());
        adapter.trennen().await;
        drop(adapter);
        let requests = server.received_requests().await.unwrap();
        let deleted = requests.iter().any(|r| {
            r.method == "DELETE"
                && r.url
                    .query_pairs()
                    .any(|(key, value)| key == "id" && value == "allocated-follow")
        });
        assert!(
            deleted,
            "Successful partial remote subscription was forgotten even after explicit disconnect and adapter drop"
        );
    }
}

#[cfg(test)]
#[path = "kick_disconnect_tests.rs"]
mod coupled_disconnect_review;
