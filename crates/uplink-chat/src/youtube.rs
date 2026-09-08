use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, watch};

use crate::Platform;
use crate::adapter::{AdapterFabrik, ChatAdapter, ChatFehler, stub};
use crate::helix::fehler_aus_token;
use crate::nachricht::{Badge, ChatNachricht, Ereignis, Fragment};
use crate::token::TokenQuelle;

const YT_API: &str = "https://www.googleapis.com/youtube/v3";
const FRIST: Duration = Duration::from_secs(10);
const BROADCAST_TAKT: Duration = Duration::from_secs(30);
const POLL_MIN: Duration = Duration::from_secs(3);
const SENDE_ABSTAND: Duration = Duration::from_millis(300);
const EINGERICHTET_FRIST: Duration = Duration::from_secs(20);
const KONTINGENT_PAUSE: Duration = Duration::from_secs(5 * 60);
const KONTINGENT_POLL: Duration = Duration::from_secs(60);

fn poll_wartezeit(polling_ms: Option<u64>, min: Duration) -> Duration {
    match polling_ms {
        Some(ms) => Duration::from_millis(ms).max(min),
        None => min,
    }
}

#[derive(Debug, Clone)]
pub struct YouTubeEndpunkte {
    pub api: String,
    pub broadcast_takt: Duration,
    pub poll_min: Duration,
    pub kontingent_pause: Duration,
    pub kontingent_poll: Duration,
}

impl Default for YouTubeEndpunkte {
    fn default() -> Self {
        Self {
            api: YT_API.into(),
            broadcast_takt: BROADCAST_TAKT,
            poll_min: POLL_MIN,
            kontingent_pause: KONTINGENT_PAUSE,
            kontingent_poll: KONTINGENT_POLL,
        }
    }
}

pub struct YouTubeFabrik {
    quelle: Arc<TokenQuelle>,
    endpunkte: YouTubeEndpunkte,
    http: reqwest::Client,
    eingerichtet_cache:
        std::sync::Mutex<std::collections::HashMap<i64, (bool, std::time::Instant)>>,
}

impl YouTubeFabrik {
    pub fn new(quelle: Arc<TokenQuelle>) -> Self {
        Self::mit_endpunkten(quelle, YouTubeEndpunkte::default())
    }

    pub fn mit_endpunkten(quelle: Arc<TokenQuelle>, endpunkte: YouTubeEndpunkte) -> Self {
        Self {
            quelle,
            endpunkte,
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

impl AdapterFabrik for YouTubeFabrik {
    fn eingerichtet(&self, streamer_id: i64, platform: Platform) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            if platform != Platform::YouTube {
                return false;
            }
            if let Some(gemerkt) = self.eingerichtet_gemerkt(streamer_id) {
                return gemerkt;
            }
            let wert = matches!(
                self.quelle.zugang(streamer_id, Platform::YouTube).await,
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
            if platform != Platform::YouTube {
                return stub(platform);
            }
            let zugang = self
                .quelle
                .zugang(streamer_id, Platform::YouTube)
                .await
                .map_err(fehler_aus_token)?;
            if !zugang.scopes.iter().any(|s| {
                matches!(
                    s.as_str(),
                    "https://www.googleapis.com/auth/youtube.force-ssl"
                        | "https://www.googleapis.com/auth/youtube"
                )
            }) {
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube));
            }
            let (stop, _) = watch::channel(false);
            let inner = Arc::new(Inner {
                streamer_id,
                channel_id: zugang.platform_user_id.clone(),
                channel_login: zugang.platform_login.clone(),
                quelle: self.quelle.clone(),
                endpunkte: self.endpunkte.clone(),
                http: self.http.clone(),
                eingang,
                live_chat_id: Mutex::new(None),
                verbunden: AtomicBool::new(false),
                ende_grund: std::sync::Mutex::new(None),
                stop,
                zuletzt_gesendet: Mutex::new(None),
                kontingent_gemeldet: AtomicBool::new(false),
            });
            let adapter: Arc<dyn ChatAdapter> = Arc::new(YouTubeAdapter {
                inner,
                task: Mutex::new(None),
            });
            Ok(adapter)
        })
    }
}

struct Inner {
    streamer_id: i64,
    channel_id: String,
    channel_login: String,
    quelle: Arc<TokenQuelle>,
    endpunkte: YouTubeEndpunkte,
    http: reqwest::Client,
    eingang: mpsc::Sender<Ereignis>,
    live_chat_id: Mutex<Option<String>>,
    verbunden: AtomicBool,
    ende_grund: std::sync::Mutex<Option<ChatFehler>>,
    stop: watch::Sender<bool>,
    zuletzt_gesendet: Mutex<Option<tokio::time::Instant>>,
    kontingent_gemeldet: AtomicBool,
}

enum ChatSuche {
    Gefunden(String),
    Keiner,
    Kontingent,
}

enum ListenFehler {
    BroadcastVorbei,
    NeuAnmeldung,
    Kontingent,
    Netz(String),
}

enum Verbot {
    Kontingent,
    Auth,
    Sonst,
}

fn verbot_einordnen(koerper: &Value) -> Verbot {
    let mut kontingent = false;
    let mut auth = false;
    if let Some(fehler) = koerper.pointer("/error/errors").and_then(Value::as_array) {
        for eintrag in fehler {
            match eintrag.get("reason").and_then(Value::as_str) {
                Some("quotaExceeded" | "rateLimitExceeded" | "userRateLimitExceeded") => {
                    kontingent = true
                }
                Some("authError" | "forbidden") => auth = true,
                _ => {}
            }
        }
    }
    if kontingent {
        Verbot::Kontingent
    } else if auth {
        Verbot::Auth
    } else {
        Verbot::Sonst
    }
}

impl Inner {
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
                .zugang(self.streamer_id, Platform::YouTube)
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
                    self.quelle
                        .invalidieren(self.streamer_id, Platform::YouTube);
                    continue;
                }
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube));
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
        Err(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube))
    }

    async fn aktiven_chat_finden(&self) -> Result<ChatSuche, ChatFehler> {
        let (status, koerper) = self
            .anfrage(
                Method::GET,
                "/liveBroadcasts",
                &[
                    ("part", "snippet".into()),
                    ("broadcastStatus", "active".into()),
                    ("maxResults", "1".into()),
                ],
                None,
            )
            .await?;
        if status == StatusCode::FORBIDDEN {
            return match verbot_einordnen(&koerper) {
                Verbot::Kontingent => Ok(ChatSuche::Kontingent),
                Verbot::Sonst => Ok(ChatSuche::Keiner),
                Verbot::Auth => Err(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube)),
            };
        }
        if !status.is_success() {
            return Err(ChatFehler::Netz(format!("liveBroadcasts: HTTP {status}")));
        }
        Ok(koerper
            .pointer("/items/0/snippet/liveChatId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|id| ChatSuche::Gefunden(id.to_string()))
            .unwrap_or(ChatSuche::Keiner))
    }

    async fn nachrichten_holen(
        &self,
        live_chat_id: &str,
        page_token: Option<&str>,
    ) -> Result<(Vec<ChatNachricht>, Option<String>, Duration), ListenFehler> {
        let mut query = vec![
            ("liveChatId", live_chat_id.to_string()),
            ("part", "snippet,authorDetails".into()),
            ("maxResults", "200".into()),
        ];
        if let Some(token) = page_token {
            query.push(("pageToken", token.to_string()));
        }
        let (status, koerper) = self
            .anfrage(Method::GET, "/liveChat/messages", &query, None)
            .await
            .map_err(|f| match f {
                ChatFehler::NeuAnmeldungNoetig(_) => ListenFehler::NeuAnmeldung,
                sonst => ListenFehler::Netz(sonst.to_string()),
            })?;
        match status {
            s if s.is_success() => {}
            StatusCode::FORBIDDEN => {
                return Err(match verbot_einordnen(&koerper) {
                    Verbot::Kontingent => ListenFehler::Kontingent,
                    Verbot::Auth => ListenFehler::NeuAnmeldung,
                    Verbot::Sonst => ListenFehler::BroadcastVorbei,
                });
            }
            StatusCode::NOT_FOUND => return Err(ListenFehler::BroadcastVorbei),
            sonst => {
                return Err(ListenFehler::Netz(format!(
                    "liveChatMessages: HTTP {sonst}"
                )));
            }
        }
        let items = koerper
            .pointer("/items")
            .and_then(Value::as_array)
            .map(|liste| {
                liste
                    .iter()
                    .filter_map(|item| self.nachricht_lesen(item))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let next = koerper
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        let wait = poll_wartezeit(
            koerper.get("pollingIntervalMillis").and_then(Value::as_u64),
            self.endpunkte.poll_min,
        );
        Ok((items, next, wait))
    }

    fn nachricht_lesen(&self, item: &Value) -> Option<ChatNachricht> {
        let typ = item.pointer("/snippet/type").and_then(Value::as_str)?;
        if typ != "textMessageEvent" {
            return None;
        }
        let message_id = item.get("id").and_then(Value::as_str)?.to_string();
        let text = item
            .pointer("/snippet/displayMessage")
            .and_then(Value::as_str)
            .or_else(|| {
                item.pointer("/snippet/textMessageDetails/messageText")
                    .and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_string();
        let sender_id = item
            .pointer("/authorDetails/channelId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let display = item
            .pointer("/authorDetails/displayName")
            .and_then(Value::as_str)
            .unwrap_or(&sender_id)
            .to_string();
        let sent_at = item
            .pointer("/snippet/publishedAt")
            .and_then(Value::as_str)
            .and_then(|t| t.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);
        let ist_besitzer = item
            .pointer("/authorDetails/isChatOwner")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut badges = Vec::new();
        if ist_besitzer {
            badges.push(badge("broadcaster"));
        }
        if item
            .pointer("/authorDetails/isChatModerator")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            badges.push(badge("moderator"));
        }
        if item
            .pointer("/authorDetails/isChatSponsor")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            badges.push(badge("subscriber"));
        }
        Some(ChatNachricht {
            platform: Platform::YouTube,
            eigene: ist_besitzer,
            hervorhebung: None,
            channel_id: self.channel_id.clone(),
            channel_login: self.channel_login.clone(),
            message_id,
            sender_id: sender_id.clone(),
            sender_login: display.clone(),
            sender_display: display,
            color: None,
            badges,
            fragments: vec![Fragment::text(text)],
            sent_at,
            is_action: false,
            reply_to: None,
        })
    }

    async fn senden(&self, text: &str) -> Result<(), ChatFehler> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ChatFehler::Verworfen("leere Nachricht".into()));
        }
        let live_chat_id = self.live_chat_id.lock().await.clone();
        let Some(live_chat_id) = live_chat_id else {
            return Err(ChatFehler::Verworfen("kein laufender YouTube-Chat".into()));
        };
        self.drosseln().await;
        let koerper = json!({
            "snippet": {
                "liveChatId": live_chat_id,
                "type": "textMessageEvent",
                "textMessageDetails": { "messageText": text }
            }
        });
        let (status, antwort) = self
            .anfrage(
                Method::POST,
                "/liveChat/messages",
                &[("part", "snippet".into())],
                Some(&koerper),
            )
            .await?;
        match status {
            s if s.is_success() => {
                if antwort
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
                {
                    Ok(())
                } else {
                    Err(ChatFehler::Netz(
                        "YouTube hat die Zustellung nicht bestätigt. Vor erneutem Senden den Chat prüfen.".into(),
                    ))
                }
            }
            StatusCode::FORBIDDEN => Err(match verbot_einordnen(&antwort) {
                Verbot::Auth => ChatFehler::NeuAnmeldungNoetig(Platform::YouTube),
                Verbot::Kontingent => ChatFehler::Abgelehnt(
                    "YouTube begrenzt gerade den Versand. Bitte später erneut versuchen.".into(),
                ),
                Verbot::Sonst => ChatFehler::Abgelehnt(
                    "YouTube nimmt in diesem Chat gerade keine Nachrichten an.".into(),
                ),
            }),
            sonst => Err(ChatFehler::Netz(format!("liveChatMessages: HTTP {sonst}"))),
        }
    }

    async fn kontingent_melden(&self) {
        if self.kontingent_gemeldet.swap(true, Ordering::SeqCst) {
            return;
        }
        let info = crate::ereignis::StreamInfo {
            platform: Platform::YouTube,
            channel_id: self.channel_id.clone(),
            title: "YouTube-Kontingent erschöpft, Chat pausiert".into(),
            category_id: None,
            category_name: None,
            category_bild: None,
            tags: None,
            is_live: None,
            started_at: None,
            viewers: None,
        };
        let _ = self.eingang.send(Ereignis::Info(info)).await;
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
}

async fn leseschleife(inner: Arc<Inner>) {
    let mut stop = inner.stop.subscribe();
    loop {
        if *stop.borrow() {
            return;
        }
        let live_chat_id = match inner.aktiven_chat_finden().await {
            Ok(ChatSuche::Gefunden(id)) => {
                inner.kontingent_gemeldet.store(false, Ordering::SeqCst);
                id
            }
            Ok(ChatSuche::Keiner) => {
                tokio::select! {
                    _ = tokio::time::sleep(inner.endpunkte.broadcast_takt) => continue,
                    _ = stop.changed() => return,
                }
            }
            Ok(ChatSuche::Kontingent) => {
                inner.kontingent_melden().await;
                tracing::info!(
                    streamer_id = inner.streamer_id,
                    "YouTube-Chat: Kontingent erschoepft, Broadcast-Suche pausiert"
                );
                tokio::select! {
                    _ = tokio::time::sleep(inner.endpunkte.kontingent_pause) => continue,
                    _ = stop.changed() => return,
                }
            }
            Err(ChatFehler::NeuAnmeldungNoetig(_)) => {
                *inner.ende_grund.lock().expect("ende_grund") =
                    Some(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube));
                return;
            }
            Err(fehler) => {
                tracing::info!(streamer_id = inner.streamer_id, %fehler, "YouTube-Chat: Broadcast-Suche fehlgeschlagen, neuer Versuch");
                tokio::select! {
                    _ = tokio::time::sleep(inner.endpunkte.broadcast_takt) => continue,
                    _ = stop.changed() => return,
                }
            }
        };
        *inner.live_chat_id.lock().await = Some(live_chat_id.clone());
        tracing::info!(
            streamer_id = inner.streamer_id,
            kanal = %inner.channel_login,
            "YouTube-Chat: aktiver Chat gefunden"
        );
        let mut page_token: Option<String> = None;
        loop {
            if *stop.borrow() {
                return;
            }
            let result = inner
                .nachrichten_holen(&live_chat_id, page_token.as_deref())
                .await;
            inner.verbunden.store(result.is_ok(), Ordering::SeqCst);
            match result {
                Ok((items, next, wait)) => {
                    inner.kontingent_gemeldet.store(false, Ordering::SeqCst);
                    for nachricht in items {
                        if inner.eingang.send(Ereignis::Chat(nachricht)).await.is_err() {
                            return;
                        }
                    }
                    page_token = next;
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = stop.changed() => return,
                    }
                }
                Err(ListenFehler::BroadcastVorbei) => {
                    tracing::info!(
                        streamer_id = inner.streamer_id,
                        "YouTube-Chat: Broadcast vorbei, warte auf den naechsten"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(inner.endpunkte.broadcast_takt) => {}
                        _ = stop.changed() => return,
                    }
                    break;
                }
                Err(ListenFehler::NeuAnmeldung) => {
                    *inner.ende_grund.lock().expect("ende_grund") =
                        Some(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube));
                    return;
                }
                Err(ListenFehler::Kontingent) => {
                    inner.kontingent_melden().await;
                    tracing::info!(
                        streamer_id = inner.streamer_id,
                        "YouTube-Chat: Kontingent erschoepft, Poll pausiert"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(inner.endpunkte.kontingent_poll) => {}
                        _ = stop.changed() => return,
                    }
                }
                Err(ListenFehler::Netz(grund)) => {
                    tracing::info!(streamer_id = inner.streamer_id, %grund, "YouTube-Chat: Poll fehlgeschlagen, neuer Versuch");
                    tokio::select! {
                        _ = tokio::time::sleep(inner.endpunkte.poll_min) => {}
                        _ = stop.changed() => return,
                    }
                }
            }
        }
        inner.verbunden.store(false, Ordering::SeqCst);
        *inner.live_chat_id.lock().await = None;
    }
}

pub struct YouTubeAdapter {
    inner: Arc<Inner>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ChatAdapter for YouTubeAdapter {
    fn account_id(&self) -> Option<&str> {
        Some(&self.inner.channel_id)
    }
    fn platform(&self) -> Platform {
        Platform::YouTube
    }

    fn verbinden(&self) -> BoxFuture<'_, Result<(), ChatFehler>> {
        Box::pin(async move {
            let mut task = self.task.lock().await;
            if task.as_ref().is_some_and(|t| !t.is_finished()) {
                return Ok(());
            }
            *self.inner.ende_grund.lock().expect("ende_grund") = None;
            self.inner.stop.send_replace(false);
            let inner = self.inner.clone();
            *task = Some(tokio::spawn(async move {
                leseschleife(inner.clone()).await;
                inner.verbunden.store(false, Ordering::SeqCst);
                *inner.live_chat_id.lock().await = None;
            }));
            Ok(())
        })
    }

    fn trennen(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.inner.stop.send_replace(true);
            if let Some(mut task) = self.task.lock().await.take()
                && tokio::time::timeout(Duration::from_secs(5), &mut task)
                    .await
                    .is_err()
            {
                task.abort();
                let _ = task.await;
                tracing::warn!(
                    streamer_id = self.inner.streamer_id,
                    "YouTube-Chat: Leseschleife haengt beim Trennen"
                );
            }
            self.inner.verbunden.store(false, Ordering::SeqCst);
            *self.inner.live_chat_id.lock().await = None;
        })
    }

    fn senden(&self, text: &str) -> BoxFuture<'_, Result<(), ChatFehler>> {
        let text = text.to_string();
        Box::pin(async move { self.inner.senden(&text).await })
    }

    fn verbunden(&self) -> bool {
        self.inner.verbunden.load(Ordering::SeqCst)
    }

    fn ende_grund(&self) -> Option<ChatFehler> {
        self.inner.ende_grund.lock().expect("ende_grund").clone()
    }
}

fn badge(set_id: &str) -> Badge {
    Badge {
        set_id: set_id.to_string(),
        id: String::new(),
        info: None,
        image_url: None,
    }
}

impl Drop for YouTubeAdapter {
    fn drop(&mut self) {
        self.inner.stop.send_replace(true);
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::PFAD;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn poll_wartezeit_nie_unter_dem_mindestabstand() {
        let min = Duration::from_secs(3);
        assert_eq!(poll_wartezeit(Some(500), min), min);
        assert_eq!(poll_wartezeit(None, min), min);
        assert_eq!(poll_wartezeit(Some(9000), min), Duration::from_millis(9000));
    }

    async fn bot_mit_youtube_token(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("platform", "youtube"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "yt-acc",
                "expires_at": (Utc::now() + chrono::Duration::hours(3)).to_rfc3339(),
                "platform_user_id": "UC-kanal",
                "platform_login": "EarlySalty",
                "scopes": ["https://www.googleapis.com/auth/youtube.force-ssl"]
            })))
            .mount(server)
            .await;
    }

    fn fabrik(server: &MockServer) -> YouTubeFabrik {
        YouTubeFabrik::mit_endpunkten(
            Arc::new(TokenQuelle::new(&server.uri(), "intern")),
            YouTubeEndpunkte {
                api: server.uri(),
                broadcast_takt: Duration::from_millis(50),
                poll_min: Duration::from_millis(50),
                kontingent_pause: Duration::from_millis(50),
                kontingent_poll: Duration::from_millis(50),
            },
        )
    }

    #[tokio::test]
    async fn ohne_broadcast_bleibt_getrennt_und_wartet() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::channel(4);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!adapter.verbunden());
        assert!(rx.try_recv().is_err());
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn nachrichten_landen_im_bus() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [ { "snippet": { "liveChatId": "chat-1" } } ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/liveChat/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "pollingIntervalMillis": 10,
                "nextPageToken": "p-2",
                "items": [ {
                    "id": "yt-msg-1",
                    "snippet": {
                        "type": "textMessageEvent",
                        "displayMessage": "moin aus YouTube",
                        "publishedAt": "2026-09-02T10:00:00Z"
                    },
                    "authorDetails": {
                        "channelId": "UC-gast",
                        "displayName": "YT Gast",
                        "isChatModerator": true
                    }
                } ]
            })))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        let ereignis = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("rechtzeitig")
            .expect("Nachricht");
        let Ereignis::Chat(n) = ereignis else {
            panic!("chat erwartet");
        };
        assert_eq!(n.platform, Platform::YouTube);
        assert_eq!(n.sender_display, "YT Gast");
        assert_eq!(n.message_id, "yt-msg-1");
        assert_eq!(n.plain_text(), "moin aus YouTube");
        assert!(n.badges.iter().any(|b| b.set_id == "moderator"));
        adapter.trennen().await;
    }

    #[test]
    fn verbot_wird_nach_grund_eingeordnet() {
        let quota = json!({ "error": { "errors": [ { "reason": "quotaExceeded" } ] } });
        assert!(matches!(verbot_einordnen(&quota), Verbot::Kontingent));
        let rate = json!({ "error": { "errors": [ { "reason": "userRateLimitExceeded" } ] } });
        assert!(matches!(verbot_einordnen(&rate), Verbot::Kontingent));
        let auth = json!({ "error": { "errors": [ { "reason": "authError" } ] } });
        assert!(matches!(verbot_einordnen(&auth), Verbot::Auth));
        let ende = json!({ "error": { "errors": [ { "reason": "liveChatEnded" } ] } });
        assert!(matches!(verbot_einordnen(&ende), Verbot::Sonst));
        assert!(matches!(verbot_einordnen(&json!({})), Verbot::Sonst));
    }

    #[tokio::test]
    async fn kontingent_haelt_adapter_und_meldet_einmal() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": { "errors": [ { "reason": "quotaExceeded" } ] }
            })))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        let ereignis = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("rechtzeitig")
            .expect("Info");
        let Ereignis::Info(info) = ereignis else {
            panic!("info erwartet");
        };
        assert_eq!(info.platform, Platform::YouTube);
        assert_eq!(info.title, "YouTube-Kontingent erschöpft, Chat pausiert");
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(rx.try_recv().is_err(), "Info kommt nur einmal");
        assert!(
            adapter.ende_grund().is_none(),
            "Kontingent beendet den Adapter nicht"
        );
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn auth_verbot_beendet_adapter() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": { "errors": [ { "reason": "authError" } ] }
            })))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(matches!(
            adapter.ende_grund(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::YouTube))
        ));
        assert!(rx.try_recv().is_err(), "kein Info-Ereignis bei Auth-Verbot");
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn poll_kontingent_haelt_live_chat_id() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [ { "snippet": { "liveChatId": "chat-1" } } ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/liveChat/messages"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": { "errors": [ { "reason": "rateLimitExceeded" } ] }
            })))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        let ereignis = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("rechtzeitig")
            .expect("Info");
        assert!(matches!(ereignis, Ereignis::Info(_)));
        assert!(
            adapter.ende_grund().is_none(),
            "Kontingent im Poll beendet den Adapter nicht"
        );
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn deaktivierter_chat_dreht_nicht_heiss() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [ { "snippet": { "liveChatId": "chat-1" } } ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/liveChat/messages"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": { "errors": [ { "reason": "liveChatDisabled" } ] }
            })))
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        tokio::time::sleep(Duration::from_millis(250)).await;
        let polls = server
            .received_requests()
            .await
            .expect("Anfragen")
            .iter()
            .filter(|r| r.url.path() == "/liveChat/messages")
            .count();
        adapter.trennen().await;
        assert!(polls < 15, "kein Backoff: {polls} Poll-Anfragen in 250 ms");
        assert!(adapter.ende_grund().is_none());
    }

    #[tokio::test]
    async fn broadcast_403_sonst_haelt_adapter() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": { "errors": [ { "reason": "liveStreamingNotEnabled" } ] }
            })))
            .mount(&server)
            .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("gestartet");
        tokio::time::sleep(Duration::from_millis(250)).await;
        let suchen = server
            .received_requests()
            .await
            .expect("Anfragen")
            .iter()
            .filter(|r| r.url.path() == "/liveBroadcasts")
            .count();
        adapter.trennen().await;
        assert!(
            adapter.ende_grund().is_none(),
            "Sonst-403 beendet den Adapter nicht"
        );
        assert!(!adapter.verbunden());
        assert!(rx.try_recv().is_err());
        assert!(suchen < 15, "kein Backoff: {suchen} Suchen in 250 ms");
    }

    #[tokio::test]
    async fn senden_ohne_chat_wird_verworfen() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        assert!(matches!(
            adapter.senden("hallo").await,
            Err(ChatFehler::Verworfen(_))
        ));
    }
    async fn pruefe_versandantwort(status: u16, antwort: Value) -> Result<(), ChatFehler> {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/youtube/v3/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"snippet": {"liveChatId": "chat-1"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/youtube/v3/liveChat/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [], "pollingIntervalMillis": 5000, "nextPageToken": "next"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/youtube/v3/liveChat/messages"))
            .and(query_param("part", "snippet"))
            .and(wiremock::matchers::body_json(json!({"snippet": {
                "liveChatId": "chat-1", "type": "textMessageEvent",
                "textMessageDetails": {"messageText": "moin"}
            }})))
            .respond_with(ResponseTemplate::new(status).set_body_json(antwort))
            .expect(1)
            .mount(&server)
            .await;
        let mut fabrik = fabrik(&server);
        fabrik.endpunkte.api = format!("{}/youtube/v3", server.uri());
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("Start");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !adapter.verbunden() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Chat verbunden");
        let result = adapter.senden("moin").await;
        adapter.trennen().await;
        server.verify().await;
        result
    }

    #[tokio::test]
    async fn senden_verwendet_offiziellen_api_pfad_und_nachrichten_id() {
        pruefe_versandantwort(
            200,
            json!({"kind":"youtube#liveChatMessage","id":"yt-sent-1"}),
        )
        .await
        .expect("bestätigter Versand");
    }

    #[tokio::test]
    async fn senden_ohne_nachrichten_id_ist_keine_bestaetigung() {
        assert!(
            pruefe_versandantwort(200, json!({"kind":"youtube#liveChatMessage"}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn sendefehler_unterscheiden_quota_chatende_und_auth() {
        for (reason, reauth) in [
            ("quotaExceeded", false),
            ("liveChatEnded", false),
            ("authError", true),
        ] {
            let result =
                pruefe_versandantwort(403, json!({"error":{"errors":[{"reason":reason}]}})).await;
            assert_eq!(
                matches!(result, Err(ChatFehler::NeuAnmeldungNoetig(_))),
                reauth,
                "{reason}"
            );
            assert!(result.is_err());
        }
    }
    #[tokio::test]
    async fn widerrufener_lesezugang_meldet_keine_verbindung() {
        let server = MockServer::start().await;
        bot_mit_youtube_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/liveBroadcasts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"snippet": {"liveChatId": "chat-1"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/liveChat/messages"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": {"errors": [{"reason": "authError"}]}
            })))
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(4);
        let adapter = fabrik(&server)
            .bauen(7, Platform::YouTube, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("Start");
        tokio::time::timeout(Duration::from_secs(2), async {
            while adapter.ende_grund().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Widerruf erkannt");
        assert!(
            !adapter.verbunden(),
            "Keine erfolgreiche Leseverbindung behaupten"
        );
        adapter.trennen().await;
    }
}
