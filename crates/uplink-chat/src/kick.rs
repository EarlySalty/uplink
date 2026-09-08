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
            let adapter: Arc<dyn ChatAdapter> = Arc::new(KickAdapter {
                streamer_id,
                broadcaster_user_id: zugang.platform_user_id.clone(),
                channel_login: zugang.platform_login.clone(),
                quelle: self.quelle.clone(),
                endpunkte: self.endpunkte.clone(),
                http: self.http.clone(),
                drehkreuz: self.drehkreuz.clone(),
                eingang,
                abo_ids: Mutex::new(Vec::new()),
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
    streamer_id: i64,
    broadcaster_user_id: String,
    channel_login: String,
    quelle: Arc<TokenQuelle>,
    endpunkte: KickEndpunkte,
    http: reqwest::Client,
    drehkreuz: Arc<KickDrehkreuz>,
    eingang: mpsc::Sender<Ereignis>,
    abo_ids: Mutex<Vec<String>>,
    verbunden: AtomicBool,
    zuletzt_gesendet: Mutex<Option<tokio::time::Instant>>,
    ende_grund: std::sync::Mutex<Option<ChatFehler>>,
    registrierungs_token: AtomicU64,
}

impl KickAdapter {
    async fn anfrage(
        &self,
        method: Method,
        pfad: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), ChatFehler> {
        for versuch in 0..2 {
            let token = self
                .quelle
                .zugang(self.streamer_id, Platform::Kick)
                .await
                .map_err(fehler_aus_token)?
                .access_token
                .clone();
            let mut bau = self
                .http
                .request(method.clone(), format!("{}{}", self.endpunkte.api, pfad))
                .header("Authorization", format!("Bearer {token}"))
                .query(query);
            if let Some(body) = body {
                bau = bau.json(body);
            }
            let antwort = bau
                .send()
                .await
                .map_err(|_| ChatFehler::Netz("Plattformverbindung fehlgeschlagen".into()))?;
            let status = antwort.status();
            if status == StatusCode::UNAUTHORIZED {
                if versuch == 0 {
                    self.quelle.invalidieren(self.streamer_id, Platform::Kick);
                    continue;
                }
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
            }
            let text = String::from_utf8(crate::http::bytes(antwort).await?)
                .map_err(|_| ChatFehler::Netz("Plattformantwort ist nicht lesbar".into()))?;
            let koerper = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            };
            return Ok((status, koerper));
        }
        Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
    }

    async fn abonnieren(&self) -> Result<Vec<String>, ChatFehler> {
        let events: Vec<Value> = ABOS
            .iter()
            .map(|(name, version)| json!({ "name": name, "version": version }))
            .collect();
        let mut koerper = json!({ "method": "webhook", "events": events });
        if let Ok(id) = self.broadcaster_user_id.parse::<i64>() {
            koerper["broadcaster_user_id"] = json!(id);
        }
        let (status, antwort) = self
            .anfrage(
                Method::POST,
                "/public/v1/events/subscriptions",
                &[],
                Some(&koerper),
            )
            .await?;
        match status {
            s if s.is_success() => {}
            StatusCode::FORBIDDEN => return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick)),
            sonst => {
                return Err(ChatFehler::Netz(format!(
                    "events/subscriptions: HTTP {sonst}"
                )));
            }
        }
        let leer = Vec::new();
        let data = antwort
            .pointer("/data")
            .and_then(Value::as_array)
            .unwrap_or(&leer);
        let mut ids = Vec::new();
        let mut chat_ok = false;
        let mut chat_grund: Option<String> = None;
        let mut fehlende_activity: Vec<String> = Vec::new();
        for eintrag in data {
            let name = eintrag
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let sub_id = eintrag
                .get("subscription_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let grund = eintrag
                .get("error")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            match sub_id {
                Some(id) => {
                    ids.push(id.to_string());
                    if name == "chat.message.sent" {
                        chat_ok = true;
                    }
                }
                None if name == "chat.message.sent" => {
                    chat_grund = Some(grund.unwrap_or("kein subscription_id").to_string());
                }
                None => fehlende_activity.push(name.to_string()),
            }
        }
        if !chat_ok {
            return Err(match chat_grund {
                Some(_) => ChatFehler::Abgelehnt("Kick hat den Chat-Zugang nicht eingerichtet. Bitte die Verbindung im Dashboard prüfen.".into()),
                None => ChatFehler::Netz(
                    "events/subscriptions: chat.message.sent nicht abonniert".into(),
                ),
            });
        }
        if !fehlende_activity.is_empty() {
            tracing::warn!(
                streamer_id = self.streamer_id,
                fehlend = %fehlende_activity.join(","),
                "Kick-Chat: Aktivitaets-Abos fehlen, Chat laeuft trotzdem"
            );
        }
        Ok(ids)
    }

    async fn abos_loeschen(&self) {
        let ids: Vec<String> = self.abo_ids.lock().await.clone();
        if ids.is_empty() {
            return;
        }
        let query: Vec<(&str, String)> = ids.iter().map(|id| ("id", id.clone())).collect();
        match self
            .anfrage(
                Method::DELETE,
                "/public/v1/events/subscriptions",
                &query,
                None,
            )
            .await
        {
            Ok((status, _)) if status.is_success() => {
                self.abo_ids.lock().await.retain(|id| !ids.contains(id));
            }
            Ok((status, _)) => {
                tracing::warn!(
                    streamer_id = self.streamer_id,
                    %status,
                    "Kick-Chat: Abos nicht abbestellt, bleiben fuer den naechsten Versuch"
                );
            }
            Err(fehler) => {
                tracing::warn!(
                    streamer_id = self.streamer_id,
                    %fehler,
                    "Kick-Chat: Abos nicht abbestellt, bleiben fuer den naechsten Versuch"
                );
            }
        }
    }

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
            match self.abonnieren().await {
                Ok(ids) => {
                    *self.abo_ids.lock().await = ids;
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
            self.abos_loeschen().await;
            self.verbunden.store(false, Ordering::SeqCst);
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

impl Drop for KickAdapter {
    fn drop(&mut self) {
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
