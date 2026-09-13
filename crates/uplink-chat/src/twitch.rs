//! Twitch-Adapter: EventSub per WebSocket lesen, Helix zum Senden.
//!
//! Lesen: `wss://eventsub.wss.twitch.tv/ws`. Nach `session_welcome` binnen
//! 10 s die Subscriptions aus [`SUBSCRIPTIONS`] per
//! `POST helix/eventsub/subscriptions` (Transport websocket, Streamer-Token),
//! alle parallel auf dieselbe Session (INV-5). `channel.chat.message` ist
//! Pflicht; alles andere kommt nur, wenn der Scope im Zugang steht, sonst
//! landet der Scope in `fehlende_scopes` und der Chat laeuft trotzdem (INV-6). `session_keepalive` wird ignoriert,
//! `session_reconnect` wechselt auf die neue URL und schliesst die alte erst
//! nach dem neuen Welcome, `revocation` beendet den Adapter: der Streamer muss
//! sich neu anmelden. Abriss: Backoff 1, 2, 4, 8, hoechstens 30 s.
//!
//! Senden: `POST helix/chat/messages` mit `user:write:chat`. `is_sent=false`
//! ist kein Fehler des Relays, sondern eine Ablehnung der Plattform
//! (`Verworfen`). 401: Token einmal neu holen und wiederholen. 403: Scope
//! fehlt, neu anmelden. Drossel: eine Nachricht je 300 ms je Streamer.
//!
//! Uebersetzung der Nutzlast nach dem Muster von `obs_dock.rs` im Bot.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::Platform;
use crate::adapter::{AdapterFabrik, ChatAdapter, ChatFehler, stub};
use crate::helix::{HINWEIS_BREMSE, HelixClient, HelixEndpunkte, HelixFabrik, fehler_aus_token};
use crate::nachricht::{Badge, ChatNachricht, Ereignis, Fragment, ReplyRef};
use crate::token::{TokenQuelle, Zugang};

/// Frist von Welcome bis zur Subscription laut Twitch. Danach schliesst
/// Twitch die Verbindung.
const WELCOME_FRIST: Duration = Duration::from_secs(10);
/// Rueckzug nach einem Abriss: erster Abstand und Obergrenze.
///
/// Gedeckelt, damit ein Twitch, das laenger ausfaellt, nicht zu einer
/// Verbindung fuehrt, die erst in Stunden wiederkommt, und verdoppelnd, damit
/// eine Stoerung von einer Sekunde nicht als Sturm auf Twitch endet.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Der naechste Abstand nach einem Abriss.
///
/// Als eigene Funktion, damit die Zusage (waechst, ist gedeckelt) ohne echte
/// Wartezeit pruefbar ist. Vorher lag sie als Ausdruck in der Leseschleife
/// und war nur ueber einen Reconnect-Sturm mit echter Uhr erreichbar.
fn naechster_rueckzug(bisher: Duration, deckel: Duration) -> Duration {
    (bisher * 2).min(deckel)
}
/// Mindestabstand zwischen zwei gesendeten Nachrichten je Streamer.
const SENDE_ABSTAND: Duration = Duration::from_millis(300);
/// Keepalive-Abstand, den die EventSub-URL bei Twitch bestellt.
const KEEPALIVE_SEKUNDEN: u64 = 30;
/// Luft auf den Keepalive-Abstand, bevor die Verbindung als tot gilt.
const KEEPALIVE_LUFT: Duration = Duration::from_secs(10);
/// Bleibt so lange jeder Rahmen aus (auch Keepalives), ist die Verbindung
/// halb offen: Twitch schickt sonst spaetestens alle `KEEPALIVE_SEKUNDEN`.
const KEEPALIVE_FRIST: Duration =
    Duration::from_secs(KEEPALIVE_SEKUNDEN).saturating_add(KEEPALIVE_LUFT);
/// Bild-URL-Muster fuer Twitch-Emotes, wie im Bot.
const EMOTE_URL_TEMPLATE: &str =
    "https://static-cdn.jtvnw.net/emoticons/v2/{id}/{{format}}/{{theme_mode}}/{{scale}}";

/// Wohin der Adapter spricht. Im Dienst Twitch, im Test wiremock und ein
/// lokaler WebSocket.
#[derive(Debug, Clone)]
pub struct TwitchEndpunkte {
    /// Helix-Basis ohne Schraegstrich am Ende.
    pub helix: String,
    /// id.twitch.tv-Basis, fuer `/oauth2/validate` (liefert die Client-Id).
    pub id: String,
    /// EventSub-WebSocket-URL.
    pub eventsub_ws: String,
    /// Ohne Rahmen in dieser Zeit gilt die Verbindung als abgerissen.
    /// Im Dienst `KEEPALIVE_FRIST`, im Test kuerzer.
    pub keepalive_frist: Duration,
    /// Frist vom Welcome bis zur ersten Subscription. Im Dienst
    /// `WELCOME_FRIST` (10 s, so lange laesst Twitch die Session offen),
    /// im Test kuerzer, damit die Frist ohne zehn Sekunden Wartezeit
    /// pruefbar bleibt.
    pub subscription_frist: Duration,
    /// Erster Abstand nach einem Abriss. Im Dienst `BACKOFF_START`, im Test
    /// kurz, damit ein Reconnect-Sturm ohne echte Wartezeit pruefbar bleibt.
    pub rueckzug_start: Duration,
    /// Obergrenze des Abstands. Im Dienst `BACKOFF_MAX`.
    pub rueckzug_max: Duration,
}

impl Default for TwitchEndpunkte {
    fn default() -> Self {
        Self {
            helix: "https://api.twitch.tv/helix".into(),
            id: "https://id.twitch.tv".into(),
            eventsub_ws: format!(
                "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds={KEEPALIVE_SEKUNDEN}"
            ),
            keepalive_frist: KEEPALIVE_FRIST,
            subscription_frist: WELCOME_FRIST,
            rueckzug_start: BACKOFF_START,
            rueckzug_max: BACKOFF_MAX,
        }
    }
}

/// So lange gilt eine Antwort auf die Frage "hat der Streamer Twitch
/// verbunden". Die Frage stellt jedes offene Dock-Fenster alle 30 Sekunden;
/// ohne Gedaechtnis liefe fuer jeden Streamer ohne Verbindung eine eigene
/// HTTP-Anfrage an den Bot, weil die [`TokenQuelle`] nur Erfolge behaelt.
/// Kurz genug, dass eine frische Verbindung binnen einer halben Minute in der
/// Kopfzeile steht.
const EINGERICHTET_FRIST: Duration = Duration::from_secs(20);

/// Baut Twitch-Adapter; alles andere bleibt Stub.
pub struct TwitchFabrik {
    quelle: Arc<TokenQuelle>,
    endpunkte: TwitchEndpunkte,
    helix: HelixFabrik,
    /// Gemerkte Antworten fuer `eingerichtet`, je Streamer mit Zeitstempel.
    /// Nur fuer die Anzeige; `bauen` fragt immer frisch.
    eingerichtet_cache: std::sync::Mutex<std::collections::HashMap<i64, (bool, Instant)>>,
}

impl TwitchFabrik {
    pub fn new(quelle: Arc<TokenQuelle>) -> Self {
        Self::mit_endpunkten(quelle, TwitchEndpunkte::default())
    }

    pub fn mit_endpunkten(quelle: Arc<TokenQuelle>, endpunkte: TwitchEndpunkte) -> Self {
        let helix = HelixFabrik::mit_endpunkten(
            quelle.clone(),
            HelixEndpunkte {
                helix: endpunkte.helix.clone(),
                id: endpunkte.id.clone(),
            },
        );
        Self {
            quelle,
            endpunkte,
            helix,
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
            .insert(streamer_id, (wert, Instant::now()));
    }
}

impl AdapterFabrik for TwitchFabrik {
    /// Verbunden heisst: der Bot hat einen Zugang zu diesem Kanal. Ein
    /// abgelaufener Grant zaehlt mit, denn das Konto ist verbunden und
    /// braucht nur eine neue Anmeldung; die Plattform bleibt deshalb in der
    /// Kopfzeile stehen, statt kommentarlos zu verschwinden. Netzfehler
    /// zaehlen nicht, sonst behauptete eine kurze Stoerung beim Bot, der
    /// Streamer haette nie etwas verbunden.
    fn eingerichtet(&self, streamer_id: i64, platform: Platform) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            if platform != Platform::Twitch {
                return false;
            }
            if let Some(gemerkt) = self.eingerichtet_gemerkt(streamer_id) {
                return gemerkt;
            }
            let wert = matches!(
                self.quelle.zugang(streamer_id, Platform::Twitch).await,
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
            if platform != Platform::Twitch {
                return stub(platform);
            }
            // Ohne Zugang kein Adapter: 404 vom Bot heisst "nicht verbunden",
            // und das soll der Supervisor wissen, bevor irgendetwas laeuft.
            let zugang = self
                .quelle
                .zugang(streamer_id, Platform::Twitch)
                .await
                .map_err(fehler_aus_token)?;
            let adapter: Arc<dyn ChatAdapter> = Arc::new(TwitchAdapter::new(
                streamer_id,
                zugang,
                self.helix.client(streamer_id),
                self.endpunkte.clone(),
                eingang,
            ));
            Ok(adapter)
        })
    }
}

/// Wie die `condition` einer Subscription gebaut wird.
#[derive(Clone, Copy)]
enum Bedingung {
    /// `broadcaster_user_id` und `user_id` (Chat).
    BroadcasterUndUser,
    /// `broadcaster_user_id` und `moderator_user_id` (Follow v2).
    BroadcasterUndModerator,
    /// Nur `broadcaster_user_id`.
    Broadcaster,
    /// `to_broadcaster_user_id` (eingehender Raid).
    ZielBroadcaster,
}

struct Subscription {
    typ: &'static str,
    version: &'static str,
    /// Noetiger Scope; `None` heisst: geht mit jedem Streamer-Token.
    scope: Option<&'static str>,
    bedingung: Bedingung,
}

/// Ob der Zugang den noetigen Scope hat. Twitch laesst fuer einen Lesescope
/// auch den passenden `manage`-Scope gelten: wer verwalten darf, darf lesen.
pub(crate) fn scope_vorhanden(scopes: &[String], noetig: &str) -> bool {
    if scopes.iter().any(|s| s == noetig) {
        return true;
    }
    match noetig.strip_prefix("channel:read:") {
        Some(rest) => {
            let manage = format!("channel:manage:{rest}");
            scopes.contains(&manage)
        }
        None => false,
    }
}

/// Alles, was eine Session abonniert. Der erste Eintrag ist Pflicht.
const SUBSCRIPTIONS: &[Subscription] = &[
    Subscription {
        typ: "channel.chat.message",
        version: "1",
        scope: Some("user:read:chat"),
        bedingung: Bedingung::BroadcasterUndUser,
    },
    Subscription {
        typ: "channel.chat.message_delete",
        version: "1",
        scope: Some("user:read:chat"),
        bedingung: Bedingung::BroadcasterUndUser,
    },
    Subscription {
        typ: "channel.chat.clear_user_messages",
        version: "1",
        scope: Some("user:read:chat"),
        bedingung: Bedingung::BroadcasterUndUser,
    },
    Subscription {
        typ: "channel.chat.clear",
        version: "1",
        scope: Some("user:read:chat"),
        bedingung: Bedingung::BroadcasterUndUser,
    },
    Subscription {
        typ: "channel.follow",
        version: "2",
        scope: Some("moderator:read:followers"),
        bedingung: Bedingung::BroadcasterUndModerator,
    },
    Subscription {
        typ: "channel.subscribe",
        version: "1",
        scope: Some("channel:read:subscriptions"),
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "channel.subscription.message",
        version: "1",
        scope: Some("channel:read:subscriptions"),
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "channel.subscription.gift",
        version: "1",
        scope: Some("channel:read:subscriptions"),
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "channel.cheer",
        version: "1",
        scope: Some("bits:read"),
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "channel.raid",
        version: "1",
        scope: None,
        bedingung: Bedingung::ZielBroadcaster,
    },
    Subscription {
        typ: "channel.update",
        version: "2",
        scope: None,
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "stream.online",
        version: "1",
        scope: None,
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "stream.offline",
        version: "1",
        scope: None,
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "channel.channel_points_custom_reward_redemption.add",
        version: "1",
        scope: Some("channel:read:redemptions"),
        bedingung: Bedingung::Broadcaster,
    },
    Subscription {
        typ: "channel.channel_points_custom_reward_redemption.update",
        version: "1",
        scope: Some("channel:read:redemptions"),
        bedingung: Bedingung::Broadcaster,
    },
];

struct Inner {
    streamer_id: i64,
    broadcaster_id: String,
    broadcaster_login: String,
    helix: HelixClient,
    endpunkte: TwitchEndpunkte,
    eingang: mpsc::Sender<Ereignis>,
    /// Scopes, die der Zugang beim letzten Abonnieren nicht hatte.
    fehlende_scopes: std::sync::Mutex<Vec<String>>,
    verbunden: AtomicBool,
    /// Warum die Leseschleife aufgegeben hat. `None`, solange sie laeuft
    /// oder nur ein Abriss dazwischen war.
    ende_grund: std::sync::Mutex<Option<ChatFehler>>,
    stop: watch::Sender<bool>,
    zuletzt_gesendet: Mutex<Option<tokio::time::Instant>>,
}

/// Ein Twitch-Adapter fuer einen Streamer.
pub struct TwitchAdapter {
    inner: Arc<Inner>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TwitchAdapter {
    pub fn new(
        streamer_id: i64,
        zugang: Zugang,
        helix: HelixClient,
        endpunkte: TwitchEndpunkte,
        eingang: mpsc::Sender<Ereignis>,
    ) -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                streamer_id,
                broadcaster_id: zugang.platform_user_id.clone(),
                broadcaster_login: zugang.platform_login.clone(),
                helix,
                endpunkte,
                eingang,
                fehlende_scopes: std::sync::Mutex::new(Vec::new()),
                verbunden: AtomicBool::new(false),
                ende_grund: std::sync::Mutex::new(None),
                stop,
                zuletzt_gesendet: Mutex::new(None),
            }),
            task: Mutex::new(None),
        }
    }
}

impl ChatAdapter for TwitchAdapter {
    fn account_id(&self) -> Option<&str> {
        Some(&self.inner.broadcaster_id)
    }
    fn platform(&self) -> Platform {
        Platform::Twitch
    }

    fn verbinden(&self) -> BoxFuture<'_, Result<(), ChatFehler>> {
        Box::pin(async move {
            let mut task = self.task.lock().await;
            if task.as_ref().is_some_and(|t| !t.is_finished()) {
                return Ok(());
            }
            // Client-Id vorab, aber nur als Tuersteher fuer das, was sich
            // ohne Eingriff nicht aendert (nicht verbunden, widerrufen,
            // interner Token abgelehnt). Ein einzelner Netzfehler auf
            // `/oauth2/validate` darf den Adapter nicht fuer den ganzen
            // Stream wegwerfen: den holt die Leseschleife mit Backoff nach.
            if let Err(fehler) = self.inner.helix.client_id().await {
                if !fehler.vergeht_von_allein() {
                    return Err(fehler);
                }
                tracing::info!(
                    streamer_id = self.inner.streamer_id,
                    %fehler,
                    "Twitch-Chat: Client-Id noch nicht da, die Leseschleife versucht es weiter"
                );
            }
            *self.inner.ende_grund.lock().expect("ende_grund") = None;
            // `send_replace`, nicht `send`: ohne Empfaenger (alte Schleife
            // ist zu Ende) wuerde `send` den Wert nicht setzen und die neue
            // Schleife saehe noch das `true` vom Trennen.
            self.inner.stop.send_replace(false);
            let inner = self.inner.clone();
            *task = Some(tokio::spawn(async move { leseschleife(inner).await }));
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
                    "Twitch-Chat: Leseschleife haengt beim Trennen"
                );
            }
            self.inner.verbunden.store(false, Ordering::SeqCst);
        })
    }

    fn senden(&self, text: &str) -> BoxFuture<'_, Result<(), ChatFehler>> {
        let text = text.to_string();
        Box::pin(async move { self.inner.senden(&text).await })
    }

    fn verbunden(&self) -> bool {
        self.inner.verbunden.load(Ordering::SeqCst)
    }

    fn fehlende_scopes(&self) -> Vec<String> {
        self.inner
            .fehlende_scopes
            .lock()
            .expect("fehlende_scopes")
            .clone()
    }

    fn ende_grund(&self) -> Option<ChatFehler> {
        self.inner.ende_grund.lock().expect("ende_grund").clone()
    }
}

impl Inner {
    fn subscription_koerper(&self, sub: &Subscription, session_id: &str) -> Value {
        let id = &self.broadcaster_id;
        let condition = match sub.bedingung {
            Bedingung::BroadcasterUndUser => {
                json!({ "broadcaster_user_id": id, "user_id": id })
            }
            Bedingung::BroadcasterUndModerator => {
                json!({ "broadcaster_user_id": id, "moderator_user_id": id })
            }
            Bedingung::Broadcaster => json!({ "broadcaster_user_id": id }),
            Bedingung::ZielBroadcaster => json!({ "to_broadcaster_user_id": id }),
        };
        json!({
            "type": sub.typ,
            "version": sub.version,
            "condition": condition,
            "transport": { "method": "websocket", "session_id": session_id },
        })
    }

    /// Eine Subscription anlegen. 409 heisst: die Session traegt sie schon.
    async fn eine_abonnieren(
        &self,
        sub: &Subscription,
        session_id: &str,
    ) -> Result<(), ChatFehler> {
        let koerper = self.subscription_koerper(sub, session_id);
        let (status, _) = self
            .helix
            .anfrage(
                reqwest::Method::POST,
                "/eventsub/subscriptions",
                &[],
                Some(&koerper),
            )
            .await?;
        match status {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::CONFLICT => Ok(()),
            reqwest::StatusCode::FORBIDDEN => Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch)),
            sonst => Err(ChatFehler::Netz(format!(
                "subscriptions {}: HTTP {sonst}",
                sub.typ
            ))),
        }
    }

    /// Client-Id und Zugang vorab holen, bevor die WebSocket-Verbindung
    /// steht.
    ///
    /// Nach dem Welcome bleiben laut Twitch 10 s bis zur ersten Subscription.
    /// Beides haengt nicht an der `session_id`, gehoert also davor: sonst
    /// koennen ein abgelaufener Token-Cache und ein langsamer Bot die Frist
    /// allein aufbrauchen, und die Verbindung ist zu, bevor der erste POST
    /// rausgeht. Rueckgabe sind die Scopes des Zugangs.
    async fn vorladen(&self) -> Result<Vec<String>, ChatFehler> {
        self.helix.client_id().await?;
        let scopes = self.helix.zugang().await?.scopes.clone();
        if !scope_vorhanden(&scopes, "user:read:chat") {
            return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
        }
        Ok(scopes)
    }

    /// Traegt einen Scope in `fehlende_scopes` nach, wenn er noch nicht dort
    /// steht.
    fn scope_nachtragen(&self, scope: &str) {
        let mut fehlend = self.fehlende_scopes.lock().expect("fehlende_scopes");
        if !fehlend.iter().any(|f| f == scope) {
            fehlend.push(scope.to_string());
        }
    }

    /// Legt alle Subscriptions fuer diese WebSocket-Session an, parallel,
    /// damit die 10-s-Frist nach dem Welcome auch mit allen Aufrufen haelt.
    /// Nur der Chat ist Pflicht; ein fehlender Scope ueberspringt den Typ
    /// und merkt sich den Scope, ein anderer Fehler wird nur geloggt.
    ///
    /// `scopes` kommt aus [`Self::vorladen`], nicht aus einem eigenen Abruf:
    /// in dieser Funktion laeuft die Frist schon.
    async fn abonnieren(&self, session_id: &str, scopes: &[String]) -> Result<(), ChatFehler> {
        // Die zentrale Kontoverwaltung liefert die tatsächlich gewährten Scopes.
        let scopes_bekannt = true;
        let mut fehlend: Vec<String> = Vec::new();
        let merken = |scope: &str, fehlend: &mut Vec<String>| {
            if !fehlend.iter().any(|f| f == scope) {
                fehlend.push(scope.to_string());
            }
        };
        let mut aufrufe = Vec::new();
        for (i, sub) in SUBSCRIPTIONS.iter().enumerate() {
            if let Some(scope) = sub.scope {
                // Der erste Eintrag ist der Chat: den versuchen wir immer,
                // ein alter Grant ohne Scope-Liste soll nicht stumm bleiben.
                if scopes_bekannt && i > 0 && !scope_vorhanden(scopes, scope) {
                    merken(scope, &mut fehlend);
                    continue;
                }
            }
            aufrufe.push(async move { (sub, self.eine_abonnieren(sub, session_id).await) });
        }
        let mut chat_ergebnis = Err(ChatFehler::Netz("Chat-Subscription nicht versucht".into()));
        for (sub, ergebnis) in futures::future::join_all(aufrufe).await {
            if sub.typ == SUBSCRIPTIONS[0].typ {
                chat_ergebnis = ergebnis;
                continue;
            }
            let Err(fehler) = ergebnis else { continue };
            // Ein 403 auf eine Nebensubscription kann einen fehlenden
            // optionalen Scope beweisen. Teilt sie dagegen den Pflicht-Scope
            // des erfolgreichen Chat-Abos, waere "Scope fehlt" widerspruechlich:
            // dann bleibt nur der konkrete Nebentyp nicht verfuegbar.
            if matches!(fehler, ChatFehler::NeuAnmeldungNoetig(_))
                && let Some(scope) = sub.scope
                && Some(scope) != SUBSCRIPTIONS[0].scope
            {
                merken(scope, &mut fehlend);
            }
            tracing::warn!(
                streamer_id = self.streamer_id,
                typ = sub.typ,
                %fehler,
                "Twitch: Subscription nicht angelegt, Chat laeuft weiter"
            );
        }
        // Erst jetzt schreiben: vorher stuenden die Ergebnisse der POSTs noch
        // nicht fest.
        *self.fehlende_scopes.lock().expect("fehlende_scopes") = fehlend;
        chat_ergebnis
    }

    async fn senden(&self, text: &str) -> Result<(), ChatFehler> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ChatFehler::Verworfen("leere Nachricht".into()));
        }
        if !scope_vorhanden(&self.helix.zugang().await?.scopes, "user:write:chat") {
            return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
        }
        self.drosseln().await;
        let koerper = json!({
            "broadcaster_id": self.broadcaster_id,
            "sender_id": self.broadcaster_id,
            "message": text,
        });
        let (status, antwort) = self
            .helix
            .anfrage(reqwest::Method::POST, "/chat/messages", &[], Some(&koerper))
            .await?;
        match status {
            s if s.is_success() => {
                let eintrag = antwort
                    .get("data")
                    .and_then(Value::as_array)
                    .and_then(|d| d.first());
                let is_sent = eintrag
                    .and_then(|e| e.get("is_sent"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_sent {
                    return Ok(());
                }
                let grund = "von Twitch nicht zugestellt".into();
                Err(ChatFehler::Verworfen(grund))
            }
            reqwest::StatusCode::FORBIDDEN => Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch)),
            // Der Helix-Client hat schon einmal abgewartet und wiederholt.
            // Bremst Twitch danach immer noch, ist das kein Netzfehler,
            // sondern ein Hinweis: gleich noch einmal tippen reicht.
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                Err(ChatFehler::Abgelehnt(HINWEIS_BREMSE.into()))
            }
            sonst => Err(ChatFehler::Netz(format!("chat/messages: HTTP {sonst}"))),
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
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum Ende {
    Stop,
    Widerrufen,
    /// Der Zustand aendert sich ohne Eingriff nicht (Streamer hat die
    /// Plattform im Dashboard getrennt, interner Token abgelehnt). Weiter zu
    /// versuchen kostet nur Aufrufe; der Grund gehoert ins Dock.
    Beendet(ChatFehler),
    Abgerissen(String),
}

async fn leseschleife(inner: Arc<Inner>) {
    let mut stop = inner.stop.subscribe();
    let mut backoff = inner.endpunkte.rueckzug_start;
    loop {
        if *stop.borrow() {
            return;
        }
        let ende = verbindung(&inner, &mut stop, &mut backoff).await;
        inner.verbunden.store(false, Ordering::SeqCst);
        match ende {
            Ende::Stop => return,
            Ende::Widerrufen => {
                tracing::warn!(
                    streamer_id = inner.streamer_id,
                    "Twitch-Chat: Zugang widerrufen, Adapter aus"
                );
                *inner.ende_grund.lock().expect("ende_grund") =
                    Some(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
                return;
            }
            Ende::Beendet(fehler) => {
                tracing::warn!(
                    streamer_id = inner.streamer_id,
                    %fehler,
                    "Twitch-Chat: Adapter aus, das aendert sich nicht von allein"
                );
                *inner.ende_grund.lock().expect("ende_grund") = Some(fehler);
                return;
            }
            Ende::Abgerissen(grund) => {
                tracing::info!(streamer_id = inner.streamer_id, %grund, wartezeit_s = backoff.as_secs(), "Twitch-Chat: Verbindung weg, neuer Versuch");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stop.changed() => return,
                }
                backoff = naechster_rueckzug(backoff, inner.endpunkte.rueckzug_max);
            }
        }
    }
}

/// Verbindet, wartet auf Welcome, meldet die Subscription an. Kehrt mit
/// `Ok` zurueck, wenn die Verbindung steht und der Leser laeuft.
async fn verbinden_mit_welcome(url: &str) -> Result<(Ws, String), String> {
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(256 * 1024))
        .max_frame_size(Some(256 * 1024));
    let (ws, _) = tokio::time::timeout(
        WELCOME_FRIST,
        tokio_tungstenite::connect_async_with_config(url, Some(config), false),
    )
    .await
    .map_err(|_| "Twitch-Verbindung hat die Frist überschritten".to_string())?
    .map_err(|_| "Twitch-Verbindung fehlgeschlagen".to_string())?;
    let mut ws = ws;
    let welcome = tokio::time::timeout(WELCOME_FRIST, async {
        while let Some(frame) = ws.next().await {
            let frame = frame.map_err(|_| "Twitch-Antwort ist ungültig".to_string())?;
            if let Message::Text(text) = frame {
                let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                if nachrichten_typ(&json) == Some("session_welcome") {
                    let session_id = json
                        .pointer("/payload/session/id")
                        .and_then(Value::as_str)
                        .ok_or("welcome ohne session id")?
                        .to_string();
                    return Ok(session_id);
                }
            }
        }
        Err("geschlossen vor dem Welcome".to_string())
    })
    .await
    .map_err(|_| "kein Welcome binnen 10 s".to_string())??;
    Ok((ws, welcome))
}

async fn verbindung(
    inner: &Arc<Inner>,
    stop: &mut watch::Receiver<bool>,
    backoff: &mut Duration,
) -> Ende {
    // Vor dem Connect, nicht danach: nach dem Welcome laeuft die 10-s-Frist.
    let scopes = match inner.vorladen().await {
        Ok(scopes) => scopes,
        Err(ChatFehler::NeuAnmeldungNoetig(_)) => return Ende::Widerrufen,
        Err(fehler) if !fehler.vergeht_von_allein() => return Ende::Beendet(fehler),
        Err(fehler) => return Ende::Abgerissen(fehler.to_string()),
    };
    let (mut ws, session_id) = match verbinden_mit_welcome(&inner.endpunkte.eventsub_ws).await {
        Ok(v) => v,
        Err(grund) => return Ende::Abgerissen(grund),
    };
    // Ab hier laeuft die Frist. Wird sie gerissen, schliesst Twitch mit 4003
    // und im Log stuende sonst nur "Verbindung weg", ohne den Grund.
    match tokio::time::timeout(
        inner.endpunkte.subscription_frist,
        inner.abonnieren(&session_id, &scopes),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(ChatFehler::NeuAnmeldungNoetig(_))) => return Ende::Widerrufen,
        Ok(Err(fehler)) if !fehler.vergeht_von_allein() => return Ende::Beendet(fehler),
        Ok(Err(fehler)) => return Ende::Abgerissen(fehler.to_string()),
        Err(_) => {
            return Ende::Abgerissen(format!(
                "keine Subscription binnen {} ms nach dem Welcome",
                inner.endpunkte.subscription_frist.as_millis()
            ));
        }
    }
    inner.verbunden.store(true, Ordering::SeqCst);
    *backoff = inner.endpunkte.rueckzug_start;
    tracing::info!(
        streamer_id = inner.streamer_id,
        kanal = %inner.broadcaster_login,
        "Twitch-Chat: verbunden"
    );

    let mut drains = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = drains.join_next(), if !drains.is_empty() => {},
            _ = stop.changed() => {
                let _ = tokio::time::timeout(Duration::from_secs(1),ws.close(None)).await;
                return Ende::Stop;
            }
            // Twitch schickt spaetestens alle `KEEPALIVE_SEKUNDEN` einen Rahmen.
            // Bleibt laenger alles aus, ist das TCP halb offen: `next` liefert
            // dann weder `None` noch `Err`, nur die Frist bricht ab.
            frame = tokio::time::timeout(inner.endpunkte.keepalive_frist, ws.next()) => {
                let frame = match frame {
                    Err(_) => return Ende::Abgerissen(format!(
                        "kein Keepalive binnen {} s",
                        inner.endpunkte.keepalive_frist.as_secs()
                    )),
                    Ok(None) => return Ende::Abgerissen("Verbindung geschlossen".into()),
                    Ok(Some(Err(_))) => return Ende::Abgerissen("Twitch-Verbindung ist unterbrochen".into()),
                    Ok(Some(Ok(frame))) => frame,
                };
                let Message::Text(text) = frame else {
                    if matches!(frame, Message::Close(_)) {
                        return Ende::Abgerissen("Close von Twitch".into());
                    }
                    continue;
                };
                let json: Value = match serde_json::from_str(&text) {
                    Ok(json) => json,
                    Err(_) => continue,
                };
                match nachrichten_typ(&json) {
                    Some("session_keepalive") | Some("session_welcome") => {}
                    Some("notification") => {
                        if notification_verarbeiten(inner, &json).await.is_err() {
                            return Ende::Stop;
                        }
                    }
                    Some("session_reconnect") => {
                        let Some(neu) = json.pointer("/payload/session/reconnect_url").and_then(Value::as_str) else {
                            return Ende::Abgerissen("reconnect ohne URL".into());
                        };
                        if !reconnect_allowed(neu,&inner.endpunkte.eventsub_ws) || drains.len()>=2 {
                            return Ende::Abgerissen("Twitch-Reconnect-Adresse ist ungültig".into());
                        }
                        // Erst die neue Verbindung mit Welcome, dann die alte
                        // zu Ende lesen: Twitch schickt auf der alten noch,
                        // was in der Handshake-Luecke ankommt, und schliesst sie
                        // selbst. Die Subscriptions ziehen mit um.
                        match verbinden_mit_welcome(neu).await {
                            Ok((neue_ws, _)) => {
                                let alte = std::mem::replace(&mut ws, neue_ws);
                                // Nebenlaeufig, nicht hier: die Nachlese darf
                                // weder die neue Verbindung noch das Stop-Signal
                                // bis zu 5 s aufhalten.
                                drains.spawn(alte_verbindung_nachlesen(inner.clone(), alte));
                            }
                            Err(grund) => return Ende::Abgerissen(format!("reconnect: {grund}")),
                        }
                    }
                    Some("revocation") => {
                        if let Widerruf::Alles = revocation_bewerten(inner, &json) {
                            return Ende::Widerrufen;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Was eine `revocation` fuer den Adapter bedeutet.
enum Widerruf {
    /// Der ganze Zugang ist weg, der Adapter hoert auf.
    Alles,
    /// Nur eine Nebensubscription ist weg. Der Chat laeuft weiter (INV-6),
    /// die Funktion dahinter fehlt im Dock.
    NurEine,
}

/// Bewertet eine `revocation`.
///
/// Twitch schickt sie je Subscription, mit `payload.subscription.type` und
/// `status` (`authorization_revoked`, `user_removed`, `version_removed`). Nur
/// `authorization_revoked` betrifft den ganzen Zugang; zieht Twitch dagegen
/// eine Version von `channel.follow` zurueck, ist das kein Grund, dem
/// Streamer den Chat abzuschalten.
fn revocation_bewerten(inner: &Inner, json: &Value) -> Widerruf {
    let status = json
        .pointer("/payload/subscription/status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let typ = json
        .pointer("/payload/subscription/type")
        .and_then(Value::as_str)
        .or_else(|| {
            json.pointer("/metadata/subscription_type")
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    if status == "authorization_revoked" {
        return Widerruf::Alles;
    }
    // Ohne Typ laesst sich nichts eingrenzen: dann gilt die alte, sichere
    // Lesart, sonst liefe der Adapter halb tot weiter.
    let Some(sub) = SUBSCRIPTIONS.iter().find(|s| s.typ == typ) else {
        tracing::warn!(
            streamer_id = inner.streamer_id,
            typ,
            status,
            "Twitch-Chat: Widerruf ohne bekannten Typ, Adapter aus"
        );
        return Widerruf::Alles;
    };
    // Der Chat ist Pflicht. Faellt er weg, hat der Adapter nichts mehr zu tun.
    if sub.typ == SUBSCRIPTIONS[0].typ {
        return Widerruf::Alles;
    }
    if let Some(scope) = sub.scope
        && Some(scope) != SUBSCRIPTIONS[0].scope
    {
        inner.scope_nachtragen(scope);
    }
    tracing::warn!(
        streamer_id = inner.streamer_id,
        typ = sub.typ,
        status,
        "Twitch-Chat: eine Subscription widerrufen, Chat laeuft weiter"
    );
    Widerruf::NurEine
}

/// Wie lange die alte Verbindung nach einem Reconnect noch gelesen wird,
/// falls Twitch sie nicht selbst schliesst.
const NACHLESE_FRIST: Duration = Duration::from_secs(5);

/// Reicht eine Notification als Ereignis in den Eingang. `Err` heisst: der
/// Eingang ist zu, der Supervisor hat die Session beendet.
async fn notification_verarbeiten(inner: &Inner, json: &Value) -> Result<(), ()> {
    let Some(typ) = json
        .pointer("/payload/subscription/type")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let zeitpunkt = json
        .pointer("/metadata/message_timestamp")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now);
    let message_id = json.pointer("/metadata/message_id").and_then(Value::as_str);
    let Some(event) = json.pointer("/payload/event") else {
        return Ok(());
    };
    let ereignis = if typ == "channel.chat.message" {
        chat_message_zu_nachricht(event, zeitpunkt).map(Ereignis::Chat)
    } else {
        crate::twitch_activity::uebersetzen(typ, event, message_id, zeitpunkt)
    };
    let Some(ereignis) = ereignis else {
        return Ok(());
    };
    inner.eingang.send(ereignis).await.map_err(|_| ())
}

/// Liest die alte Verbindung nach einem Reconnect leer, bis Twitch sie
/// schliesst, die Frist ablaeuft oder der Adapter getrennt wird. Laeuft als
/// eigene Task neben der neuen Verbindung. Dubletten faengt der Bus.
async fn alte_verbindung_nachlesen(inner: Arc<Inner>, mut alte: Ws) {
    let mut stop = inner.stop.subscribe();
    let lesen = async {
        while let Some(Ok(frame)) = alte.next().await {
            match frame {
                Message::Close(_) => break,
                Message::Text(text) => {
                    if let Ok(json) = serde_json::from_str::<Value>(&text)
                        && nachrichten_typ(&json) == Some("notification")
                        && notification_verarbeiten(&inner, &json).await.is_err()
                    {
                        tracing::debug!(
                            streamer_id = inner.streamer_id,
                            "Twitch-Chat: Eingang zu waehrend der Nachlese"
                        );
                        break;
                    }
                }
                _ => {}
            }
        }
    };
    tokio::select! {
        _ = tokio::time::timeout(NACHLESE_FRIST, lesen) => {}
        _ = stop.changed() => {}
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), alte.close(None)).await;
}

fn nachrichten_typ(json: &Value) -> Option<&str> {
    json.pointer("/metadata/message_type")
        .and_then(Value::as_str)
}

fn str_field(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            let getrimmt = text.trim();
            if !getrimmt.is_empty() {
                return Some(getrimmt.to_string());
            }
        }
    }
    None
}

fn u32_field(value: &Value, keys: &[&str]) -> Option<u32> {
    for key in keys {
        let roh = value.get(*key);
        let gelesen = roh.and_then(Value::as_u64).or_else(|| {
            roh.and_then(Value::as_str)
                .and_then(|t| t.trim().parse::<u64>().ok())
        });
        if let Some(zahl) = gelesen {
            return u32::try_from(zahl).ok();
        }
    }
    None
}

/// Uebersetzt ein `channel.chat.message`-Event. Muster aus `obs_dock.rs`.
///
/// `eigene` ist gesetzt, wenn der Absender der Kanalinhaber ist: das ist die
/// Zeile, die der Streamer selbst aus dem Dock geschickt hat (oder aus einem
/// anderen Client, das ist fuer das Dock dasselbe).
pub fn chat_message_zu_nachricht(event: &Value, sent_at: DateTime<Utc>) -> Option<ChatNachricht> {
    let channel_id = str_field(event, &["broadcaster_user_id"])?;
    let message_id = str_field(event, &["message_id"])?;
    let sender_id = str_field(event, &["chatter_user_id"])?;
    let channel_login = str_field(event, &["broadcaster_user_login"])
        .or_else(|| str_field(event, &["broadcaster_user_name"]))
        .unwrap_or_else(|| channel_id.clone());
    let sender_login =
        str_field(event, &["chatter_user_login"]).unwrap_or_else(|| sender_id.clone());
    let sender_display =
        str_field(event, &["chatter_user_name"]).unwrap_or_else(|| sender_login.clone());

    let badges = event
        .get("badges")
        .and_then(Value::as_array)
        .map(|liste| liste.iter().filter_map(badge_lesen).collect::<Vec<_>>())
        .unwrap_or_default();

    let nachricht = event.get("message");
    let fragments = nachricht
        .and_then(|body| body.get("fragments"))
        .and_then(Value::as_array)
        .map(|liste| liste.iter().filter_map(fragment_lesen).collect::<Vec<_>>())
        .unwrap_or_default();
    let fragments = if fragments.is_empty() {
        match nachricht
            .and_then(|body| body.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            Some(text) => vec![Fragment::text(text)],
            None => Vec::new(),
        }
    } else {
        fragments
    };

    Some(ChatNachricht {
        platform: Platform::Twitch,
        eigene: sender_id == channel_id,
        hervorhebung: None,
        channel_id,
        channel_login,
        message_id,
        sender_id,
        sender_login,
        sender_display,
        color: str_field(event, &["color"]),
        badges,
        fragments,
        sent_at,
        is_action: false,
        reply_to: reply_lesen(event),
    })
}

fn badge_lesen(roh: &Value) -> Option<Badge> {
    let set_id = str_field(roh, &["set_id"])?;
    Some(Badge {
        set_id,
        id: str_field(roh, &["id"]).unwrap_or_default(),
        info: str_field(roh, &["info"]),
        image_url: None,
    })
}

fn fragment_lesen(roh: &Value) -> Option<Fragment> {
    let text = roh
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let art = roh
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("text")
        .trim();
    match art {
        "emote" => {
            let Some(emote_id) = roh.get("emote").and_then(|e| str_field(e, &["id"])) else {
                return Some(Fragment::text(text));
            };
            Some(Fragment::Emote {
                url_template: EMOTE_URL_TEMPLATE.replace("{id}", &emote_id),
                text,
                emote_id,
            })
        }
        "mention" => {
            let Some(user_id) = roh.get("mention").and_then(|m| str_field(m, &["user_id"])) else {
                return Some(Fragment::text(text));
            };
            let user_login = roh
                .get("mention")
                .and_then(|m| str_field(m, &["user_login"]))
                .unwrap_or_else(|| user_id.clone());
            Some(Fragment::Mention {
                text,
                user_id,
                user_login,
            })
        }
        "cheermote" => {
            let Some(cheermote) = roh.get("cheermote") else {
                return Some(Fragment::text(text));
            };
            let Some(prefix) = str_field(cheermote, &["prefix"]) else {
                return Some(Fragment::text(text));
            };
            Some(Fragment::Cheermote {
                url_template: String::new(),
                text,
                prefix,
                bits: u64::from(u32_field(cheermote, &["bits"]).unwrap_or(0)),
                tier: u32_field(cheermote, &["tier"]).unwrap_or(1),
            })
        }
        _ => Some(Fragment::text(text)),
    }
}

fn reply_lesen(event: &Value) -> Option<ReplyRef> {
    let reply = event.get("reply")?;
    let message_id = str_field(reply, &["parent_message_id"])?;
    let sender_id = str_field(reply, &["parent_user_id"]).unwrap_or_default();
    let sender_login = str_field(reply, &["parent_user_login"]).unwrap_or_default();
    let sender_display =
        str_field(reply, &["parent_user_name"]).unwrap_or_else(|| sender_login.clone());
    Some(ReplyRef {
        message_id,
        sender_id,
        sender_login,
        sender_display,
        text: str_field(reply, &["parent_message_body"]).unwrap_or_default(),
    })
}

impl Drop for TwitchAdapter {
    fn drop(&mut self) {
        self.inner.stop.send_replace(true);
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

fn reconnect_allowed(next: &str, initial: &str) -> bool {
    let (Ok(next), Ok(_initial)) = (reqwest::Url::parse(next), reqwest::Url::parse(initial)) else {
        return false;
    };
    if next.username() != "" || next.password().is_some() || next.fragment().is_some() {
        return false;
    }
    #[cfg(test)]
    if next.scheme() == "ws"
        && _initial.scheme() == "ws"
        && next.host_str() == Some("127.0.0.1")
        && _initial.host_str() == Some("127.0.0.1")
    {
        return true;
    }
    next.scheme() == "wss"
        && next.host_str() == Some("eventsub.wss.twitch.tv")
        && next.port_or_known_default() == Some(443)
        && next.path() == "/ws"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix::testhilfe::bot_und_helix_mit_scopes;
    use crate::token::PFAD;
    use futures::SinkExt;
    use futures::future::BoxFuture;
    use tokio::net::TcpListener;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Eine vollstaendige `channel.chat.message`-Nutzlast (Fixture aus
    /// obs_dock.rs im Bot).
    fn chat_nutzlast() -> Value {
        json!({
            "broadcaster_user_id": "12345",
            "broadcaster_user_login": "earlysalty",
            "broadcaster_user_name": "EarlySalty",
            "chatter_user_id": "777",
            "chatter_user_login": "zuschauer",
            "chatter_user_name": "Zuschauer",
            "message_id": "abc-1",
            "message_type": "text",
            "color": "#1E90FF",
            "badges": [
                { "set_id": "subscriber", "id": "12", "info": "14" },
                { "set_id": "moderator", "id": "1", "info": "" }
            ],
            "message": {
                "text": "moin @earlysalty Kappa",
                "fragments": [
                    { "type": "text", "text": "moin " },
                    { "type": "mention", "text": "@earlysalty",
                      "mention": { "user_id": "12345", "user_login": "earlysalty", "user_name": "EarlySalty" } },
                    { "type": "text", "text": " " },
                    { "type": "emote", "text": "Kappa",
                      "emote": { "id": "25", "emote_set_id": "0", "format": ["static"] } }
                ]
            }
        })
    }

    fn zeitpunkt() -> DateTime<Utc> {
        "2026-08-23T20:15:00Z".parse().unwrap()
    }

    #[test]
    fn chat_message_wird_uebersetzt() {
        let n = chat_message_zu_nachricht(&chat_nutzlast(), zeitpunkt()).expect("uebersetzt");
        assert_eq!(n.platform, Platform::Twitch);
        assert_eq!(n.channel_id, "12345");
        assert_eq!(n.channel_login, "earlysalty");
        assert_eq!(n.message_id, "abc-1");
        assert_eq!(n.sender_display, "Zuschauer");
        assert_eq!(n.color.as_deref(), Some("#1E90FF"));
        assert_eq!(n.badges.len(), 2);
        assert_eq!(n.badges[1].info, None, "leeres info wird None");
        assert_eq!(n.fragments.len(), 4);
        assert!(matches!(&n.fragments[1], Fragment::Mention { user_id, .. } if user_id == "12345"));
        assert!(
            matches!(&n.fragments[3], Fragment::Emote { emote_id, url_template, .. }
            if emote_id == "25" && url_template.contains("/v2/25/"))
        );
        assert_eq!(n.plain_text(), "moin @earlysalty Kappa");
        assert_eq!(n.dedupe_key(), "twitch:12345:chat:abc-1");
        assert!(!n.eigene);

        let mut eigene = chat_nutzlast();
        eigene["chatter_user_id"] = json!("12345");
        assert!(
            chat_message_zu_nachricht(&eigene, zeitpunkt())
                .unwrap()
                .eigene
        );

        let mut ohne_fragmente = chat_nutzlast();
        ohne_fragmente["message"] = json!({ "text": "nur text" });
        let n = chat_message_zu_nachricht(&ohne_fragmente, zeitpunkt()).unwrap();
        assert_eq!(n.fragments, vec![Fragment::text("nur text")]);
    }

    // ----- Testgeruest: Fake-Bot, Fake-Helix, lokaler EventSub-WebSocket -----

    type WsServer = tokio_tungstenite::WebSocketStream<TcpStream>;

    /// Lokaler WebSocket-Server fuer genau eine Verbindung. `skript` spielt
    /// Twitch: schickt Rahmen, liest, was der Adapter tut.
    async fn ws_server<F>(skript: F) -> String
    where
        F: FnOnce(WsServer) -> BoxFuture<'static, ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            skript(ws).await;
        });
        format!("ws://127.0.0.1:{port}/ws")
    }

    fn welcome(session_id: &str) -> Message {
        Message::Text(
            json!({
                "metadata": { "message_id": "w1", "message_type": "session_welcome" },
                "payload": { "session": { "id": session_id, "status": "connected" } }
            })
            .to_string()
            .into(),
        )
    }

    fn notification(message_id: &str) -> Message {
        let mut event = chat_nutzlast();
        event["message_id"] = json!(message_id);
        Message::Text(
            json!({
                "metadata": {
                    "message_id": format!("n-{message_id}"),
                    "message_type": "notification",
                    "message_timestamp": "2026-08-23T20:15:00Z"
                },
                "payload": {
                    "subscription": { "type": "channel.chat.message" },
                    "event": event
                }
            })
            .to_string()
            .into(),
        )
    }

    fn reconnect(url: &str) -> Message {
        Message::Text(
            json!({
                "metadata": { "message_id": "r1", "message_type": "session_reconnect" },
                "payload": { "session": { "id": "s1", "status": "reconnecting", "reconnect_url": url } }
            })
            .to_string()
            .into(),
        )
    }

    /// Bot mit Token (nur Chat-Scopes) plus Helix mit validate, jeweils auf
    /// demselben wiremock.
    async fn bot_und_helix(bot_erwartet: u64) -> MockServer {
        bot_und_helix_mit_scopes(bot_erwartet, &["user:read:chat", "user:write:chat"]).await
    }

    /// Alle Scopes, die "Verbinden" im Bot holt (REQ-12).
    const DOCK_SCOPES: &[&str] = &[
        "user:read:chat",
        "user:write:chat",
        "moderator:read:followers",
        "channel:read:subscriptions",
        "bits:read",
        "channel:manage:broadcast",
        "channel:read:redemptions",
        "channel:manage:redemptions",
    ];

    /// Notification eines beliebigen Subscription-Typs.
    fn notification_typ(typ: &str, message_id: &str, event: Value) -> Message {
        Message::Text(
            json!({
                "metadata": {
                    "message_id": message_id,
                    "message_type": "notification",
                    "message_timestamp": "2026-08-23T20:15:00Z"
                },
                "payload": {
                    "subscription": { "type": typ },
                    "event": event
                }
            })
            .to_string()
            .into(),
        )
    }

    /// Zaehlt die Subscription-Typen, die auf dem Mock ankamen.
    async fn abonnierte_typen(server: &MockServer) -> Vec<String> {
        let mut typen: Vec<String> = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method == "POST" && r.url.path() == "/eventsub/subscriptions")
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .filter_map(|b| b.get("type").and_then(Value::as_str).map(str::to_string))
            .collect();
        typen.sort();
        typen
    }

    #[tokio::test]
    async fn welcome_legt_alle_subscriptions_an_deren_scope_da_ist() {
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification_typ(
                    "channel.follow",
                    "n-f1",
                    json!({ "broadcaster_user_id": "12345", "user_id": "777",
                            "user_login": "neu", "user_name": "Neu",
                            "followed_at": "2026-08-23T20:15:00Z" }),
                ))
                .await
                .unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        let ereignis = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Follow binnen 5 s")
            .expect("Kanal offen");
        let Ereignis::Activity(a) = ereignis else {
            panic!("activity erwartet, kam {ereignis:?}")
        };
        assert_eq!(a.art(), "follow");
        assert_eq!(a.meta().dedupe_key, "twitch:12345:follow:n-f1");
        assert!(adapter.verbunden());
        assert!(adapter.fehlende_scopes().is_empty(), "alle Scopes da");
        assert_eq!(
            abonnierte_typen(&server).await,
            vec![
                "channel.channel_points_custom_reward_redemption.add",
                "channel.channel_points_custom_reward_redemption.update",
                "channel.chat.clear",
                "channel.chat.clear_user_messages",
                "channel.chat.message",
                "channel.chat.message_delete",
                "channel.cheer",
                "channel.follow",
                "channel.raid",
                "channel.subscribe",
                "channel.subscription.gift",
                "channel.subscription.message",
                "channel.update",
                "stream.offline",
                "stream.online",
            ]
        );
        // Condition je Typ: follow braucht moderator_user_id, raid to_broadcaster.
        let anfragen = server.received_requests().await.unwrap();
        let koerper = |typ: &str| -> Value {
            anfragen
                .iter()
                .filter(|r| r.url.path() == "/eventsub/subscriptions")
                .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
                .find(|b| b["type"] == typ)
                .unwrap()
        };
        assert_eq!(koerper("channel.follow")["version"], "2");
        assert_eq!(
            koerper("channel.follow")["condition"]["moderator_user_id"],
            "12345"
        );
        assert_eq!(
            koerper("channel.raid")["condition"]["to_broadcaster_user_id"],
            "12345"
        );
        assert_eq!(koerper("channel.update")["version"], "2");
        assert_eq!(
            koerper("channel.chat.message")["condition"]["user_id"],
            "12345"
        );
        assert_eq!(
            koerper("channel.chat.message_delete")["condition"]["user_id"],
            "12345"
        );
        assert_eq!(
            koerper("stream.online")["condition"]["broadcaster_user_id"],
            "12345"
        );
        assert_eq!(
            koerper("channel.chat.message")["transport"]["session_id"],
            "sess-1"
        );
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn fehlender_scope_ueberspringt_und_meldet_hinweis() {
        // Alter Chat-Grant: nur die zwei Chat-Scopes.
        let server = bot_und_helix(1).await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        warte_bis(&adapter, true, "Chat verbunden trotz fehlender Scopes").await;
        assert_eq!(
            abonnierte_typen(&server).await,
            vec![
                "channel.chat.clear",
                "channel.chat.clear_user_messages",
                "channel.chat.message",
                "channel.chat.message_delete",
                "channel.raid",
                "channel.update",
                "stream.offline",
                "stream.online",
            ],
            "Chatsteuerung und scope-freie Typen bleiben ohne optionale Aktivitaetsscopes aktiv"
        );
        let mut fehlend = adapter.fehlende_scopes();
        fehlend.sort();
        assert_eq!(
            fehlend,
            vec![
                "bits:read",
                "channel:read:redemptions",
                "channel:read:subscriptions",
                "moderator:read:followers",
            ]
        );
        adapter.trennen().await;
    }

    #[test]
    fn manage_scope_deckt_den_lesescope_mit_ab() {
        let nur_manage = vec!["channel:manage:redemptions".to_string()];
        assert!(
            scope_vorhanden(&nur_manage, "channel:read:redemptions"),
            "wer Einloesungen verwalten darf, darf sie auch lesen"
        );
        assert!(!scope_vorhanden(&nur_manage, "bits:read"));
        assert!(scope_vorhanden(&["bits:read".to_string()], "bits:read"));
        assert!(!scope_vorhanden(&Vec::new(), "moderator:read:followers"));
    }

    /// Ohne Scope-Liste vom Bot wird alles versucht, statt alles als
    /// fehlend zu melden: sonst fordert das Dock grundlos zum Neuverbinden.
    #[tokio::test]
    async fn ohne_scope_liste_wird_keine_berechtigung_behauptet() {
        let server = bot_und_helix_mit_scopes(1, &[]).await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        tokio::time::timeout(Duration::from_secs(2), async {
            while adapter.ende_grund().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!adapter.verbunden());
        assert_eq!(
            adapter.ende_grund(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
        );
        assert!(abonnierte_typen(&server).await.is_empty());
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn chat_message_fehlschlag_bleibt_fatal_andere_nicht() {
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        // Alles ausser Chat scheitert mit 500: der Adapter bleibt verbunden.
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(body_partial_json(json!({ "type": "channel.chat.message" })))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "data": [] })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let ws_url = ws_server_mehrfach(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        warte_bis(&adapter, true, "verbunden, obwohl Follow und Co. scheitern").await;
        adapter.trennen().await;

        // Umgekehrt: Chat scheitert, Rest geht. Der Adapter verbindet nicht.
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(body_partial_json(json!({ "type": "channel.chat.message" })))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        subscription_mock(&server).await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter
            .verbinden()
            .await
            .expect("verbinden startet die Schleife");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !adapter.verbunden(),
            "ohne Chat-Subscription nicht verbunden"
        );
        adapter.trennen().await;
    }

    /// Eine `revocation`, wie Twitch sie schickt: je Subscription, mit Typ
    /// und Grund.
    fn revocation(typ: &str, status: &str) -> Message {
        Message::Text(
            json!({
                "metadata": {
                    "message_id": format!("rev-{typ}"),
                    "message_type": "revocation",
                    "message_timestamp": "2026-08-23T20:15:00Z",
                    "subscription_type": typ,
                    "subscription_version": "1"
                },
                "payload": {
                    "subscription": {
                        "id": "sub-1",
                        "type": typ,
                        "version": "1",
                        "status": status,
                        "condition": { "broadcaster_user_id": "12345" }
                    }
                }
            })
            .to_string()
            .into(),
        )
    }

    /// INV-6: ein Widerruf einer Nebensubscription darf den Chat nicht
    /// mitreissen. Twitch zieht zum Beispiel eine Version von
    /// `channel.follow` zurueck; danach fehlen Follows, sonst nichts.
    #[tokio::test]
    async fn widerruf_einer_nebensubscription_laesst_den_chat_laufen() {
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(revocation("channel.follow", "version_removed"))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                ws.send(notification("nach-widerruf")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        let ereignis = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Chatzeile nach dem Widerruf binnen 5 s")
            .expect("Kanal offen");
        let Ereignis::Chat(n) = ereignis else {
            panic!("Chatzeile erwartet, kam {ereignis:?}")
        };
        assert_eq!(n.message_id, "nach-widerruf");
        assert!(adapter.verbunden(), "der Chat laeuft weiter");
        assert!(
            adapter
                .fehlende_scopes()
                .contains(&"moderator:read:followers".to_string()),
            "der Scope der widerrufenen Subscription gehoert ins Dock, es kamen {:?}",
            adapter.fehlende_scopes()
        );
        assert_eq!(adapter.ende_grund(), None);
        adapter.trennen().await;
    }

    /// Der Streamer zieht den Zugang zurueck: dann ist alles weg, auch der
    /// Chat, und das Dock soll den Grund lesen koennen.
    #[tokio::test]
    async fn widerruf_des_ganzen_zugangs_beendet_den_adapter() {
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(revocation("channel.follow", "authorization_revoked"))
                    .await
                    .unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        warte_bis(&adapter, true, "Adapter verbindet sich zuerst").await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while adapter.ende_grund().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Adapter gibt nach dem Widerruf auf");
        assert!(!adapter.verbunden());
        assert_eq!(
            adapter.ende_grund(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
        );
        adapter.trennen().await;
    }

    /// REQ-12: ein 403 von Twitch auf eine Nebensubscription gehoert in
    /// `fehlende_scopes`, sonst faellt die Funktion still aus und das Dock
    /// meldet null fehlende Scopes.
    #[tokio::test]
    async fn abgelehnte_nebensubscription_landet_in_fehlenden_scopes() {
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(body_partial_json(json!({ "type": "channel.follow" })))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": "Forbidden", "status": 403, "message": "missing scope"
            })))
            .mount(&server)
            .await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification("trotzdem-chat")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Chatzeile binnen 5 s")
            .expect("Kanal offen");
        assert!(adapter.verbunden(), "der Chat laeuft trotz der 403");
        assert_eq!(
            adapter.fehlende_scopes(),
            vec!["moderator:read:followers".to_string()],
            "die 403 gehoert ins Dock"
        );
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn abgelehntes_chat_delete_erfindet_keinen_fehlenden_pflicht_scope() {
        let server = bot_und_helix_mit_scopes(1, DOCK_SCOPES).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(body_partial_json(
                json!({ "type": "channel.chat.message_delete" }),
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": "Forbidden", "status": 403, "message": "subscription rejected"
            })))
            .mount(&server)
            .await;
        subscription_mock(&server).await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification("chat-laeuft")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Chatzeile binnen 5 s")
            .expect("Kanal offen");
        assert!(adapter.verbunden(), "das Pflicht-Chatabo laeuft weiter");
        assert!(
            !adapter
                .fehlende_scopes()
                .iter()
                .any(|scope| scope == "user:read:chat"),
            "ein erfolgreich genutzter Pflicht-Scope darf nicht als fehlend erscheinen"
        );
        adapter.trennen().await;
    }

    /// Die Frist vom Welcome bis zur ersten Subscription wird gemessen.
    /// Ohne sie liefe der Adapter in eine Schleife, die nie zu einer
    /// Subscription kommt, und im Log stuende nur "Verbindung weg".
    #[tokio::test]
    async fn subscription_nach_der_frist_gilt_als_abriss() {
        let server = bot_und_helix_mit_scopes(1, &["user:read:chat"]).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .respond_with(
                ResponseTemplate::new(202)
                    .set_body_json(json!({ "data": [] }))
                    .set_delay(Duration::from_secs(2)),
            )
            .mount(&server)
            .await;
        let verbindungen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let zaehler = verbindungen.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let zaehler = zaehler.clone();
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    zaehler.fetch_add(1, Ordering::SeqCst);
                    ws.send(welcome("sess-1")).await.unwrap();
                    while let Some(Ok(_)) = ws.next().await {}
                });
            }
        });
        let quelle = Arc::new(TokenQuelle::new(&server.uri(), "intern"));
        let fabrik = TwitchFabrik::mit_endpunkten(
            quelle,
            TwitchEndpunkte {
                helix: server.uri(),
                id: server.uri(),
                eventsub_ws: format!("ws://127.0.0.1:{port}/ws"),
                keepalive_frist: KEEPALIVE_FRIST,
                subscription_frist: Duration::from_millis(200),
                rueckzug_start: Duration::from_millis(20),
                rueckzug_max: Duration::from_millis(200),
            },
        );
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        tokio::time::timeout(Duration::from_secs(5), async {
            while verbindungen.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("die gerissene Frist fuehrt zu einem neuen Versuch");
        assert!(!adapter.verbunden(), "ohne Subscription nicht verbunden");
        adapter.trennen().await;
    }

    /// Ein einzelner Netzfehler auf `/oauth2/validate` in der Sekunde des
    /// Session-Starts darf nicht den ganzen Stream kosten: der Vorabaufruf
    /// gab frueher `Err` zurueck und der Supervisor warf den Adapter weg.
    #[tokio::test]
    async fn netzfehler_bei_der_client_id_wirft_den_adapter_nicht_weg() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "acc-1",
                "expires_at": (Utc::now() + chrono::Duration::hours(3)).to_rfc3339(),
                "platform_user_id": "12345",
                "platform_login": "earlysalty",
                "scopes": ["user:read:chat"]
            })))
            .mount(&server)
            .await;
        // Einmal 500, danach die echte Antwort.
        Mock::given(method("GET"))
            .and(path("/oauth2/validate"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oauth2/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "client_id": "client-abc", "login": "earlysalty", "user_id": "12345"
            })))
            .mount(&server)
            .await;
        subscription_mock(&server).await;
        let ws_url = ws_server_mehrfach(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
                ws.send(notification("spaeter-doch")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter
            .verbinden()
            .await
            .expect("ein 500 auf validate ist kein Grund, den Adapter wegzuwerfen");
        let ereignis = tokio::time::timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("Chatzeile nach dem zweiten Anlauf binnen 8 s")
            .expect("Kanal offen");
        assert!(matches!(ereignis, Ereignis::Chat(_)));
        assert!(adapter.verbunden());
        adapter.trennen().await;
    }

    /// Der Streamer trennt Twitch mitten im Stream im Dashboard: der Bot
    /// antwortet ab jetzt mit 404. Frueher fragte das Relay bis zum
    /// Stream-Ende alle 30 s nach, ohne dass sich etwas aendern konnte.
    #[tokio::test]
    async fn getrennte_plattform_beendet_den_adapter_statt_endlos_zu_fragen() {
        let server = MockServer::start().await;
        // Kurze Restlaufzeit: der Cache haelt 60 s Vorlauf, dieser Zugang
        // gilt damit bei jedem Zugriff als abgelaufen und der naechste Anlauf
        // fragt den Bot wirklich noch einmal. Drei Antworten reichen fuer
        // Adapter bauen, Client-Id und den ersten Verbindungsaufbau.
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "acc-1",
                "expires_at": (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
                "platform_user_id": "12345",
                "platform_login": "earlysalty",
                "scopes": ["user:read:chat"]
            })))
            .up_to_n_times(3)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": "keine_verbindung"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oauth2/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "client_id": "client-abc", "login": "earlysalty", "user_id": "12345"
            })))
            .mount(&server)
            .await;
        subscription_mock(&server).await;
        // Jede Verbindung wird sofort wieder geschlossen: der Adapter geht in
        // den Backoff und holt beim naechsten Anlauf einen neuen Zugang.
        let ws_url = ws_server_mehrfach(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = ws.close(None).await;
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        tokio::time::timeout(Duration::from_secs(10), async {
            while adapter.ende_grund().is_none() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("der Adapter gibt auf, statt weiter zu fragen");
        assert_eq!(
            adapter.ende_grund(),
            Some(ChatFehler::NichtVerbunden(Platform::Twitch))
        );
        adapter.trennen().await;
    }

    async fn fabrik(server: &MockServer, ws_url: &str) -> TwitchFabrik {
        let quelle = Arc::new(TokenQuelle::new(&server.uri(), "intern"));
        TwitchFabrik::mit_endpunkten(
            quelle,
            TwitchEndpunkte {
                helix: server.uri(),
                id: server.uri(),
                eventsub_ws: ws_url.to_string(),
                keepalive_frist: KEEPALIVE_FRIST,
                subscription_frist: WELCOME_FRIST,
                // Im Test kurz: ein Reconnect-Sturm soll Sekunden dauern,
                // nicht Minuten.
                rueckzug_start: Duration::from_millis(20),
                rueckzug_max: Duration::from_millis(200),
            },
        )
    }

    /// Wie `fabrik`, mit eigener Keepalive-Frist.
    async fn fabrik_mit_frist(server: &MockServer, ws_url: &str, frist: Duration) -> TwitchFabrik {
        let quelle = Arc::new(TokenQuelle::new(&server.uri(), "intern"));
        TwitchFabrik::mit_endpunkten(
            quelle,
            TwitchEndpunkte {
                helix: server.uri(),
                id: server.uri(),
                eventsub_ws: ws_url.to_string(),
                keepalive_frist: frist,
                subscription_frist: WELCOME_FRIST,
                // Im Test kurz: ein Reconnect-Sturm soll Sekunden dauern,
                // nicht Minuten.
                rueckzug_start: Duration::from_millis(20),
                rueckzug_max: Duration::from_millis(200),
            },
        )
    }

    /// Lokaler WebSocket-Server, der beliebig viele Verbindungen nacheinander
    /// annimmt und jede mit demselben Skript bedient.
    /// L3 aus 05-tests-und-docks, erste Haelfte: der Rueckzug hatte keinen
    /// Test. Dass er waechst und gedeckelt ist, war unbelegt, und ueber einen
    /// echten Reconnect-Sturm waere es nur mit halben Minuten Wartezeit
    /// pruefbar gewesen.
    #[test]
    fn der_rueckzug_waechst_und_ist_gedeckelt() {
        let deckel = BACKOFF_MAX;
        let mut jetzt = BACKOFF_START;
        let mut vorher = Vec::new();
        for _ in 0..12 {
            vorher.push(jetzt);
            let naechster = naechster_rueckzug(jetzt, deckel);
            assert!(
                naechster >= jetzt,
                "der Rueckzug wird kuerzer statt laenger: {jetzt:?} -> {naechster:?}"
            );
            assert!(
                naechster <= deckel,
                "der Rueckzug laeuft ueber den Deckel: {naechster:?} > {deckel:?}"
            );
            jetzt = naechster;
        }
        assert_eq!(
            jetzt, deckel,
            "nach zwoelf Abrissen steht der Rueckzug nicht auf dem Deckel"
        );
        assert_eq!(
            vorher[..5],
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
            ],
            "der Rueckzug verdoppelt nicht"
        );
    }

    /// L3, zweite Haelfte: fuenf Abrisse hintereinander duerfen keine zweite
    /// gleichzeitige EventSub-Verbindung hinterlassen (INV-5). Geprueft wird
    /// die Zahl der gleichzeitig offenen Verbindungen, nicht die Zahl der
    /// Versuche: Versuche soll es viele geben, offen sein darf immer nur eine.
    #[tokio::test]
    async fn ein_reconnect_sturm_laesst_genau_eine_verbindung_zurueck() {
        let server = bot_und_helix_mit_scopes(1, &["user:read:chat"]).await;
        subscription_mock(&server).await;

        let versuche = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let offen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hoechststand = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (v, o, h) = (versuche.clone(), offen.clone(), hoechststand.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (v, o, h) = (v.clone(), o.clone(), h.clone());
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    let nummer = v.fetch_add(1, Ordering::SeqCst);
                    let jetzt = o.fetch_add(1, Ordering::SeqCst) + 1;
                    h.fetch_max(jetzt, Ordering::SeqCst);
                    if nummer < 5 {
                        // Die ersten fuenf gehen sofort wieder zu, ohne
                        // Welcome: genau der Sturm, um den es geht.
                        let _ = ws.close(None).await;
                        o.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }
                    let _ = ws.send(welcome("sess-sturm")).await;
                    while let Some(Ok(_)) = ws.next().await {}
                    o.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        let fabrik = fabrik(&server, &format!("ws://127.0.0.1:{port}/ws")).await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        warte_bis(&adapter, true, "nach dem Sturm kommt der Adapter zurueck").await;

        assert!(
            versuche.load(Ordering::SeqCst) >= 6,
            "der Adapter hat nach den Abrissen nicht weiter versucht"
        );
        assert_eq!(
            hoechststand.load(Ordering::SeqCst),
            1,
            "es waren zwei EventSub-Verbindungen gleichzeitig offen (INV-5)"
        );
        adapter.trennen().await;
    }

    async fn ws_server_mehrfach<F>(skript: F) -> String
    where
        F: Fn(WsServer) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                tokio::spawn(skript(ws));
            }
        });
        format!("ws://127.0.0.1:{port}/ws")
    }

    async fn subscription_mock(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "data": [] })))
            .mount(server)
            .await;
    }

    async fn warte_bis(adapter: &Arc<dyn ChatAdapter>, verbunden: bool, was: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while adapter.verbunden() != verbunden {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(was);
    }

    #[tokio::test]
    async fn ausbleibende_keepalives_reissen_die_verbindung_ab() {
        let server = bot_und_helix(1).await;
        subscription_mock(&server).await;
        // Halb offen: Welcome, danach kommt nie wieder ein Rahmen, die
        // Verbindung wird aber auch nicht geschlossen.
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                std::future::pending::<()>().await;
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik_mit_frist(&server, &ws_url, Duration::from_millis(300))
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        warte_bis(&adapter, true, "verbunden nach Welcome").await;
        warte_bis(
            &adapter,
            false,
            "ohne Keepalive binnen Frist als abgerissen erkannt",
        )
        .await;
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn verbinden_nach_trennen_startet_wieder() {
        let server = bot_und_helix(1).await;
        subscription_mock(&server).await;
        let ws_url = ws_server_mehrfach(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        warte_bis(&adapter, true, "erste Verbindung").await;
        adapter.trennen().await;
        assert!(!adapter.verbunden());
        // Ohne Empfaenger merkt sich `watch::Sender::send` den Wert nicht:
        // das Stop-Flag bleibt sonst auf true und die neue Leseschleife
        // endet sofort.
        adapter.verbinden().await.expect("zweites verbinden");
        warte_bis(&adapter, true, "zweite Verbindung nach trennen").await;
        adapter.trennen().await;
    }

    async fn empfangen(rx: &mut mpsc::Receiver<Ereignis>) -> ChatNachricht {
        let ereignis = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Nachricht binnen 5 s")
            .expect("Kanal offen");
        let Ereignis::Chat(n) = ereignis else {
            panic!("chat erwartet, kam {ereignis:?}")
        };
        n
    }

    #[tokio::test]
    async fn welcome_fuehrt_zu_subscription_binnen_10s() {
        let server = bot_und_helix(1).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(header("Authorization", "Bearer acc-1"))
            .and(header("Client-Id", "client-abc"))
            .and(body_partial_json(json!({
                "type": "channel.chat.message",
                "version": "1",
                "condition": { "broadcaster_user_id": "12345", "user_id": "12345" },
                "transport": { "method": "websocket", "session_id": "sess-1" }
            })))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "data": [] })))
            .expect(1)
            .mount(&server)
            .await;
        let ws_url = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                // Twitch schickt nach der Subscription eine Nachricht.
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification("m1")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_url)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        let n = empfangen(&mut rx).await;
        assert_eq!(n.message_id, "m1");
        assert!(adapter.verbunden());
        adapter.trennen().await;
        assert!(!adapter.verbunden());
        server.verify().await;
    }

    #[tokio::test]
    async fn reconnect_nachricht_wechselt_url_ohne_verlust() {
        let server = bot_und_helix(1).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(body_partial_json(json!({ "type": "channel.chat.message" })))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "data": [] })))
            .expect(1)
            .mount(&server)
            .await;
        let (alt_zu_tx, alt_zu_rx) = tokio::sync::oneshot::channel::<()>();
        let ws_b = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-2")).await.unwrap();
                // Erst wenn die alte Verbindung zu ist, kommt die zweite Zeile:
                // so ist bewiesen, dass der Wechsel vor dem Schliessen fertig war.
                let _ = alt_zu_rx.await;
                ws.send(notification("m2")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let ws_a = ws_server(move |mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification("m1")).await.unwrap();
                ws.send(reconnect(&ws_b)).await.unwrap();
                // Twitch schickt auf der alten Verbindung noch, was in der
                // Handshake-Luecke ankommt, und schliesst dann selbst.
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification("m-luecke")).await.unwrap();
                ws.send(Message::Close(None)).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
                let _ = alt_zu_tx.send(());
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_a)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        assert_eq!(empfangen(&mut rx).await.message_id, "m1");
        assert_eq!(
            empfangen(&mut rx).await.message_id,
            "m-luecke",
            "Nachricht aus der Handshake-Luecke auf der alten Verbindung"
        );
        assert_eq!(empfangen(&mut rx).await.message_id, "m2");
        assert!(adapter.verbunden());
        adapter.trennen().await;
        // Genau eine Subscription: nach dem Reconnect wird nicht neu abonniert.
        server.verify().await;
    }

    #[tokio::test]
    async fn neue_verbindung_liest_waehrend_die_alte_noch_offen_ist() {
        let server = bot_und_helix(1).await;
        Mock::given(method("POST"))
            .and(path("/eventsub/subscriptions"))
            .and(body_partial_json(json!({ "type": "channel.chat.message" })))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "data": [] })))
            .expect(1)
            .mount(&server)
            .await;
        // Der Test gibt der alten Verbindung erst frei, wenn die neue
        // geliefert hat: bliebe die neue bis zum Close der alten stehen,
        // liefe das hier in den Timeout.
        let (neu_da_tx, neu_da_rx) = tokio::sync::oneshot::channel::<()>();
        let ws_b = ws_server(|mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-2")).await.unwrap();
                ws.send(notification("m-neu")).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let ws_a = ws_server(move |mut ws| {
            Box::pin(async move {
                ws.send(welcome("sess-1")).await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                ws.send(notification("m1")).await.unwrap();
                ws.send(reconnect(&ws_b)).await.unwrap();
                let _ = neu_da_rx.await;
                ws.send(notification("m-alt-spaet")).await.unwrap();
                ws.send(Message::Close(None)).await.unwrap();
                while let Some(Ok(_)) = ws.next().await {}
            })
        })
        .await;
        let (tx, mut rx) = mpsc::channel(8);
        let adapter = fabrik(&server, &ws_a)
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .expect("Adapter");
        adapter.verbinden().await.expect("verbinden");
        assert_eq!(empfangen(&mut rx).await.message_id, "m1");
        assert_eq!(
            empfangen(&mut rx).await.message_id,
            "m-neu",
            "die neue Verbindung liefert, waehrend die alte noch offen ist"
        );
        let _ = neu_da_tx.send(());
        assert_eq!(empfangen(&mut rx).await.message_id, "m-alt-spaet");
        adapter.trennen().await;
    }

    #[tokio::test]
    async fn senden_ist_sent_false_ist_kein_fehler() {
        let server = bot_und_helix(1).await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .and(body_partial_json(json!({ "broadcaster_id": "12345", "sender_id": "12345", "message": "hallo" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "message_id": "", "is_sent": false,
                           "drop_reason": { "code": "msg_rejected", "message": "Nachricht abgelehnt" } }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, "ws://127.0.0.1:1/unbenutzt")
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .unwrap();
        let ergebnis = adapter.senden("hallo").await;
        assert_eq!(
            ergebnis,
            Err(ChatFehler::Verworfen("von Twitch nicht zugestellt".into()))
        );
        assert_eq!(
            adapter.senden("  ").await,
            Err(ChatFehler::Verworfen("leere Nachricht".into()))
        );
    }

    #[tokio::test]
    async fn senden_401_holt_token_neu_und_wiederholt_einmal() {
        // Zwei Token-Abrufe: einer vorab, einer nach der 401.
        let server = bot_und_helix(2).await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "message_id": "x", "is_sent": true }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, "ws://127.0.0.1:1/unbenutzt")
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .unwrap();
        assert_eq!(adapter.senden("hallo").await, Ok(()));
        server.verify().await;
    }

    #[tokio::test]
    async fn senden_403_heisst_neu_anmelden_und_zwei_nachrichten_halten_abstand() {
        let server = bot_und_helix(1).await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(8);
        let adapter = fabrik(&server, "ws://127.0.0.1:1/unbenutzt")
            .await
            .bauen(7, Platform::Twitch, tx)
            .await
            .unwrap();
        let start = tokio::time::Instant::now();
        assert_eq!(
            adapter.senden("a").await,
            Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
        );
        assert_eq!(
            adapter.senden("b").await,
            Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
        );
        assert!(
            start.elapsed() >= SENDE_ABSTAND,
            "Drossel 300 ms zwischen zwei Nachrichten"
        );
    }

    #[tokio::test]
    async fn fabrik_ohne_verbindung_liefert_keinen_adapter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let (tx, _rx) = mpsc::channel(8);
        let fabrik = fabrik(&server, "ws://127.0.0.1:1/unbenutzt").await;
        assert_eq!(
            fabrik.bauen(7, Platform::Twitch, tx.clone()).await.err(),
            Some(ChatFehler::NichtVerbunden(Platform::Twitch))
        );
        assert_eq!(
            fabrik.bauen(7, Platform::Kick, tx).await.err(),
            Some(ChatFehler::NichtUnterstuetzt(Platform::Kick))
        );
    }
}
