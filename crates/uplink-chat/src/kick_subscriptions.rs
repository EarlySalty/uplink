//! Subscription ownership survives adapter disposal. Remote uncertainty is reconciled before
//! another create; the factory owns one bounded cleanup task, not an asynchronous Drop task.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

use super::{ABOS, KickEndpunkte};
use crate::Platform;
use crate::adapter::ChatFehler;
use crate::helix::fehler_aus_token;
use crate::token::{Grant, GrantAbo, TokenQuelle};
use zeroize::Zeroizing;

const PFAD: &str = "/public/v1/events/subscriptions";
const MAX_BESITZER: usize = 512;
const MAX_ABOS: usize = 256;
const MAX_ID: usize = 256;
const CLEANUP_FRIST: Duration = Duration::from_secs(45);
const WIEDERHOLUNG: Duration = Duration::from_secs(30);

fn unklar() -> ChatFehler {
    ChatFehler::Netz(
        "Kick-Abos konnten nicht sicher abgeglichen werden. Der Dienst versucht es erneut.".into(),
    )
}

#[derive(Default)]
struct Zustand {
    ids: Vec<String>,
    unklar: bool,
    app_id: Option<String>,
    cleanup: Option<CleanupZugang>,
    cleanup_reported: bool,
}

/// Nur RAM und nur für die bereits verantworteten Abos. Kein Refresh-Token,
/// keine Rückgabe an den Broker und kein Zugriff aus Chat-/Anlagepfaden.
struct CleanupZugang {
    token: Zeroizing<String>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

enum CleanupAnfrage<'a> {
    Introspect,
    Bestand,
    Loeschen(&'a [String]),
}

pub(super) struct Besitzer {
    streamer_id: i64,
    konto: String,
    quelle: Arc<TokenQuelle>,
    endpunkte: KickEndpunkte,
    http: reqwest::Client,
    zustand: Mutex<Zustand>,
    grant_wechsel: std::sync::Mutex<Option<Arc<GrantAbo>>>,
    belegt: AtomicBool,
    abbauen: AtomicBool,
    wecken: Arc<Notify>,
}

pub(super) struct Verwaltung {
    besitzer: Mutex<HashMap<i64, Arc<Besitzer>>>,
    wecken: Arc<Notify>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Verwaltung {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            besitzer: Mutex::new(HashMap::new()),
            wecken: Arc::new(Notify::new()),
            task: std::sync::Mutex::new(None),
        })
    }

    fn starten(self: &Arc<Self>) {
        let mut task = self.task.lock().expect("Kick-Cleanup-Task");
        if task.is_some() {
            return;
        }
        let schwach = Arc::downgrade(self);
        let wecken = self.wecken.clone();
        *task = Some(tokio::spawn(async move {
            let mut intervall = tokio::time::interval(WIEDERHOLUNG);
            intervall.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = intervall.tick() => {},
                    _ = wecken.notified() => {},
                }
                let Some(verwaltung) = schwach.upgrade() else {
                    break;
                };
                verwaltung.aufraeumen().await;
            }
        }));
    }

    pub fn stoppen(&self) {
        if let Some(task) = self.task.lock().expect("Kick-Cleanup-Task").take() {
            task.abort();
        }
    }

    pub async fn belegen(
        self: &Arc<Self>,
        streamer_id: i64,
        konto: String,
        quelle: Arc<TokenQuelle>,
        endpunkte: KickEndpunkte,
        http: reqwest::Client,
    ) -> Result<Arc<Besitzer>, ChatFehler> {
        self.starten();
        let mut besitzer = self.besitzer.lock().await;
        if let Some(alt) = besitzer.get(&streamer_id) {
            // A changed account cannot inherit or discard an unresolved previous owner's debt.
            if alt.konto != konto
                || alt
                    .belegt
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return Err(unklar());
            }
            return Ok(alt.clone());
        }
        if besitzer.len() >= MAX_BESITZER {
            return Err(unklar());
        }
        let neu = Arc::new(Besitzer {
            streamer_id,
            konto,
            quelle,
            endpunkte,
            http,
            zustand: Mutex::new(Zustand {
                unklar: true,
                ..Zustand::default()
            }),
            grant_wechsel: std::sync::Mutex::new(None),
            belegt: AtomicBool::new(true),
            abbauen: AtomicBool::new(false),
            wecken: self.wecken.clone(),
        });
        besitzer.insert(streamer_id, neu.clone());
        Ok(neu)
    }

    async fn aufraeumen(&self) {
        let besitzer: Vec<_> = self.besitzer.lock().await.values().cloned().collect();
        futures::stream::iter(besitzer)
            .map(|owner| async move {
                if owner.abbauen.load(Ordering::Acquire)
                    && matches!(
                        tokio::time::timeout(CLEANUP_FRIST, owner.aufraeumen()).await,
                        Ok(Ok(()))
                    )
                {
                    let mut alle = self.besitzer.lock().await;
                    if !owner.belegt.load(Ordering::Acquire)
                        && alle
                            .get(&owner.streamer_id)
                            .is_some_and(|current| Arc::ptr_eq(current, &owner))
                    {
                        alle.remove(&owner.streamer_id);
                    }
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
    }
}

impl Besitzer {
    pub fn freigeben(&self) {
        self.abbauen.store(true, Ordering::Release);
        self.grant_wechsel_stoppen();
        self.belegt.store(false, Ordering::Release);
        self.wecken.notify_one();
    }

    fn grant_wechsel_stoppen(&self) {
        if let Some(abo) = self
            .grant_wechsel
            .lock()
            .expect("Kick-Grant-Wechsel")
            .as_ref()
        {
            abo.stoppen();
        }
    }

    pub async fn anfrage(
        &self,
        method: Method,
        pfad: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), ChatFehler> {
        // Der normale Schreibweg besitzt keinen Zugriff auf CleanupZugang.
        if method != Method::POST || pfad != "/public/v1/chat" {
            return Err(unklar());
        }
        for versuch in 0..2 {
            let zugang = self
                .quelle
                .zugang(self.streamer_id, Platform::Kick)
                .await
                .map_err(fehler_aus_token)?;
            if zugang.platform_user_id != self.konto {
                return Err(ChatFehler::Abgelehnt(
                    "Kick-Kontozuordnung hat sich geändert. Verbindung im Dashboard prüfen.".into(),
                ));
            }
            let scope = if pfad == "/public/v1/chat" {
                "chat:write"
            } else {
                "events:subscribe"
            };
            if !zugang.scopes.iter().any(|s| s == scope) {
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
            }
            let mut request = self
                .http
                .request(method.clone(), format!("{}{}", self.endpunkte.api, pfad))
                .bearer_auth(&zugang.access_token)
                .query(query);
            if let Some(body) = body {
                request = request.json(body);
            }
            let antwort = request.send().await.map_err(|_| unklar())?;
            let status = antwort.status();
            if status == StatusCode::UNAUTHORIZED {
                if versuch == 0 {
                    self.quelle.invalidieren(self.streamer_id, Platform::Kick);
                    continue;
                }
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
            }
            let bytes = crate::http::bytes(antwort).await?;
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).map_err(|_| unklar())?
            };
            return Ok((status, value));
        }
        Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
    }

    async fn live_zugang(&self) -> Result<Grant, ChatFehler> {
        let grant = self
            .quelle
            .zugang(self.streamer_id, Platform::Kick)
            .await
            .map_err(fehler_aus_token)?;
        if grant.platform_user_id != self.konto
            || !grant.scopes.iter().any(|scope| scope == "events:subscribe")
        {
            return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
        }
        Ok(grant)
    }

    async fn http_anfrage(
        &self,
        token: &str,
        method: Method,
        pfad: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), ChatFehler> {
        let mut request = self
            .http
            .request(method, format!("{}{}", self.endpunkte.api, pfad))
            .bearer_auth(token)
            .query(query);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| unklar())?;
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
        }
        let bytes = crate::http::bytes(response).await?;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| unklar())?
        };
        Ok((status, value))
    }

    fn app_bestaetigen(
        state: &mut Zustand,
        status: StatusCode,
        antwort: &Value,
    ) -> Result<(), ChatFehler> {
        let app = antwort
            .pointer("/data/client_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= MAX_ID)
            .ok_or_else(unklar)?;
        if !status.is_success()
            || antwort.pointer("/data/active").and_then(Value::as_bool) != Some(true)
            || antwort.pointer("/data/token_type").and_then(Value::as_str) != Some("user")
            || state
                .app_id
                .as_ref()
                .is_some_and(|previous| previous != app)
        {
            return Err(unklar());
        }
        state.app_id = Some(app.into());
        Ok(())
    }

    async fn cleanup_autorisieren(
        &self,
        state: &mut Zustand,
        grant: &Grant,
    ) -> Result<(), ChatFehler> {
        let (status, value) = self
            .http_anfrage(
                &grant.access_token,
                Method::POST,
                "/oauth/token/introspect",
                &[],
                None,
            )
            .await?;
        Self::app_bestaetigen(state, status, &value)?;
        state.cleanup = Some(CleanupZugang {
            token: Zeroizing::new(grant.access_token.clone()),
            expires_at: grant.expires_at,
        });
        Ok(())
    }

    async fn cleanup_anfrage(
        &self,
        state: &Zustand,
        operation: CleanupAnfrage<'_>,
    ) -> Result<(StatusCode, Value), ChatFehler> {
        let access = state
            .cleanup
            .as_ref()
            .filter(|access| access.expires_at > chrono::Utc::now())
            .ok_or(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))?;
        let (method, path, query) = match operation {
            CleanupAnfrage::Introspect => (Method::POST, "/oauth/token/introspect", vec![]),
            CleanupAnfrage::Bestand => (
                Method::GET,
                PFAD,
                vec![("broadcaster_user_id", self.konto.clone())],
            ),
            CleanupAnfrage::Loeschen(ids) => {
                if ids.is_empty() || ids.len() > 16 || ids.iter().any(|id| !state.ids.contains(id))
                {
                    return Err(unklar());
                }
                (
                    Method::DELETE,
                    PFAD,
                    ids.iter().map(|id| ("id", id.clone())).collect(),
                )
            }
        };
        self.http_anfrage(&access.token, method, path, &query, None)
            .await
    }

    async fn abgleichen(&self, state: &mut Zustand) -> Result<(), ChatFehler> {
        let (status, antwort) = self.cleanup_anfrage(state, CleanupAnfrage::Bestand).await?;
        if !status.is_success() {
            return Err(unklar());
        }
        let data = antwort
            .get("data")
            .and_then(Value::as_array)
            .filter(|a| a.len() <= MAX_ABOS)
            .ok_or_else(unklar)?;
        let mut ids = Vec::new();
        for row in data {
            let app = row
                .get("app_id")
                .and_then(Value::as_str)
                .ok_or_else(unklar)?;
            let konto = row
                .get("broadcaster_user_id")
                .and_then(Value::as_i64)
                .ok_or_else(unklar)?;
            let event = row
                .get("event")
                .and_then(Value::as_str)
                .ok_or_else(unklar)?;
            let version = row
                .get("version")
                .and_then(Value::as_u64)
                .ok_or_else(unklar)?;
            let method = row
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(unklar)?;
            if Some(app) != state.app_id.as_deref()
                || konto.to_string() != self.konto
                || method != "webhook"
                || !ABOS
                    .iter()
                    .any(|(name, v)| *name == event && u64::from(*v) == version)
            {
                continue;
            }
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= MAX_ID)
                .ok_or_else(unklar)?;
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_owned());
            }
        }
        state.ids = ids;
        state.unklar = false;
        Ok(())
    }

    async fn loeschen(&self, state: &mut Zustand) -> Result<(), ChatFehler> {
        while !state.ids.is_empty() {
            // Bound URL size as well as the retained list; already confirmed batches can advance.
            let ids: Vec<_> = state.ids.iter().take(16).cloned().collect();
            state.unklar = true;
            let (status, _) = self
                .cleanup_anfrage(state, CleanupAnfrage::Loeschen(&ids))
                .await?;
            if !status.is_success() {
                return Err(unklar());
            }
            state.ids.drain(..ids.len());
            state.unklar = false;
        }
        Ok(())
    }

    pub async fn verbinden(&self) -> Result<(), ChatFehler> {
        let mut state = self.zustand.lock().await;
        self.abbauen.store(true, Ordering::Release);
        self.grant_wechsel_stoppen();
        let grant = self.live_zugang().await?;
        self.cleanup_autorisieren(&mut state, &grant).await?;
        if state.unklar {
            self.abgleichen(&mut state).await?;
        }
        self.loeschen(&mut state).await?;
        let body = json!({"method":"webhook", "broadcaster_user_id":self.konto.parse::<i64>().map_err(|_| unklar())?,
            "events":ABOS.iter().map(|(name, version)| json!({"name":name,"version":version})).collect::<Vec<_>>()});
        // Neue Anlage benötigt weiterhin den regulär erlaubten Brokergrant.
        // Cleanup-Zugang allein kann diesen Pfad niemals öffnen.
        let current = self.live_zugang().await?;
        if current.access_token != grant.access_token {
            self.cleanup_autorisieren(&mut state, &current).await?;
        }
        // Jeder neue Besitzerdurchlauf erhält einen eigenen Empfänger. Alte
        // Empfänger sind vor Abbau/Neuanlage gestoppt und werden ersetzt.
        *self.grant_wechsel.lock().expect("Kick-Grant-Wechsel") = Some(
            self.quelle
                .grant_wechsel(
                    self.streamer_id,
                    Platform::Kick,
                    self.konto.clone(),
                    "events:subscribe",
                )
                .map_err(fehler_aus_token)?,
        );
        state.unklar = true;
        let (status, antwort) = self
            .http_anfrage(&current.access_token, Method::POST, PFAD, &[], Some(&body))
            .await?;
        if !status.is_success() {
            return Err(unklar());
        }
        let data = antwort
            .get("data")
            .and_then(Value::as_array)
            .filter(|a| a.len() <= ABOS.len())
            .ok_or_else(unklar)?;
        let mut chat = false;
        for row in data {
            let name = row.get("name").and_then(Value::as_str).ok_or_else(unklar)?;
            if !ABOS.iter().any(|(expected, _)| *expected == name) {
                return Err(unklar());
            }
            if let Some(id) = row.get("subscription_id").and_then(Value::as_str) {
                if id.is_empty() || id.len() > MAX_ID {
                    return Err(unklar());
                }
                if !state.ids.iter().any(|known| known == id) {
                    state.ids.push(id.into());
                }
                chat |= name == "chat.message.sent";
            } else if row
                .get("error")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                // Neither success nor an explicit failure: a remote allocation may exist.
                return Err(unklar());
            }
        }
        // Preserve successful activity IDs even when the necessary chat subscription failed.
        state.unklar = false;
        if !chat {
            return Err(ChatFehler::Abgelehnt("Kick hat den Chat-Zugang nicht eingerichtet. Bitte die Verbindung im Dashboard prüfen.".into()));
        }
        self.abbauen.store(false, Ordering::Release);
        Ok(())
    }

    pub async fn trennen(&self) -> Result<(), ChatFehler> {
        self.abbauen.store(true, Ordering::Release);
        self.grant_wechsel_stoppen();
        let result = tokio::time::timeout(CLEANUP_FRIST, self.aufraeumen()).await
            .unwrap_or_else(|_| Err(ChatFehler::Netz("Kick-Bereinigung hat ihre Frist überschritten. Die offenen Abos bleiben zur Wiederholung vorgemerkt.".into())));
        self.wecken.notify_one();
        result
    }

    async fn aufraeumen(&self) -> Result<(), ChatFehler> {
        let mut state = self.zustand.lock().await;
        let result = self.cleanup_locked(&mut state).await;
        if let Err(error) = &result {
            if !state.cleanup_reported {
                tracing::warn!(streamer_id=self.streamer_id, %error, "Kick-Abos konnten nicht bereinigt werden; offene Zuständigkeit bleibt erhalten");
                state.cleanup_reported = true;
            }
        } else {
            state.cleanup_reported = false;
        }
        result
    }

    async fn cleanup_locked(&self, state: &mut Zustand) -> Result<(), ChatFehler> {
        if !self.abbauen.load(Ordering::Acquire) {
            return Ok(());
        }
        if state.ids.is_empty() && !state.unklar {
            state.cleanup = None;
            self.grant_wechsel
                .lock()
                .expect("Kick-Grant-Wechsel")
                .take();
            return Ok(());
        }
        let normal_erneuert = self
            .grant_wechsel
            .lock()
            .expect("Kick-Grant-Wechsel")
            .as_ref()
            .and_then(|abo| abo.letzter());
        if let Some(grant) = normal_erneuert.filter(|grant| grant.expires_at > chrono::Utc::now()) {
            // Der Empfänger prüft Konto und Scope, dieser vorhandene Owner
            // bestätigt zusätzlich seine unveränderte App vor der Übernahme.
            self.cleanup_autorisieren(state, &grant).await?;
        }
        if state
            .cleanup
            .as_ref()
            .is_some_and(|access| access.expires_at <= chrono::Utc::now())
        {
            state.cleanup = None;
            self.grant_wechsel
                .lock()
                .expect("Kick-Grant-Wechsel")
                .take();
            return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick));
        }
        let (status, value) = self
            .cleanup_anfrage(state, CleanupAnfrage::Introspect)
            .await?;
        Self::app_bestaetigen(state, status, &value)?;
        if state.unklar {
            self.abgleichen(state).await?;
        }
        self.loeschen(state).await?;
        state.cleanup = None;
        self.grant_wechsel
            .lock()
            .expect("Kick-Grant-Wechsel")
            .take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abgelaufener_cleanup_zugang_wird_verworfen_aber_die_schuld_bleibt() {
        let verwaltung = Verwaltung::new();
        let owner = verwaltung
            .belegen(
                7,
                "123".into(),
                Arc::new(TokenQuelle::new("http://127.0.0.1:1", "synthetic")),
                KickEndpunkte::default(),
                reqwest::Client::new(),
            )
            .await
            .unwrap();
        {
            let mut state = owner.zustand.lock().await;
            state.ids = vec!["owned-expired".into()];
            state.unklar = false;
            state.cleanup = Some(CleanupZugang {
                token: Zeroizing::new("synthetic-expired".into()),
                expires_at: chrono::Utc::now() - chrono::Duration::seconds(1),
            });
        }
        owner.freigeben();
        assert_eq!(
            owner.aufraeumen().await,
            Err(ChatFehler::NeuAnmeldungNoetig(Platform::Kick))
        );
        let state = owner.zustand.lock().await;
        assert!(state.cleanup.is_none());
        assert_eq!(state.ids, ["owned-expired"]);
        drop(state);
        verwaltung.stoppen();
    }

    #[tokio::test]
    async fn besitzerlimit_verhindert_unbegrenzte_cleanup_schulden() {
        let verwaltung = Verwaltung::new();
        let quelle = Arc::new(TokenQuelle::new("http://127.0.0.1:1", "synthetic"));
        let http = reqwest::Client::new();
        for id in 1..=MAX_BESITZER {
            verwaltung
                .belegen(
                    id as i64,
                    "123".into(),
                    quelle.clone(),
                    KickEndpunkte::default(),
                    http.clone(),
                )
                .await
                .unwrap();
        }
        assert!(
            verwaltung
                .belegen(
                    1,
                    "123".into(),
                    quelle.clone(),
                    KickEndpunkte::default(),
                    http.clone()
                )
                .await
                .is_err()
        );
        assert!(
            verwaltung
                .belegen(
                    MAX_BESITZER as i64 + 1,
                    "123".into(),
                    quelle,
                    KickEndpunkte::default(),
                    http
                )
                .await
                .is_err()
        );
        assert_eq!(verwaltung.besitzer.lock().await.len(), MAX_BESITZER);
        verwaltung.stoppen();
    }
}
