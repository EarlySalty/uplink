//! Kennzahlen fuer das Karussell im Chat-Dock.
//!
//! Zwei Quellen, ein Rahmen:
//!
//! - Was **in dieser Session** passiert, zaehlt das Relay selbst mit
//!   ([`SessionZaehler`]). Der Zaehler haengt am Weiterleiter des
//!   Chat-Supervisors, sieht also jede gelesene Zeile jeder Plattform und ist
//!   damit die einzige Stelle, an der ein plattformuebergreifender
//!   Top-Chatter ueberhaupt entstehen kann.
//! - Was **ueber alle Streams** gilt (Anwesenheit, Haeufigkeit, Nachrichten
//!   insgesamt, Zuschauerspitzen), weiss nur der Twitch-Bot. Das Relay holt
//!   es ueber `GET /twitch/api/v2/internal/stream-kennzahlen` mit demselben
//!   internen Zugang wie den Plattform-Token ([`KennzahlenQuelle`]).
//!
//! Jede Kennzahl kommt im Rahmen in zwei Sichten, `session` und `gesamt`
//! (GRILLME C4-A1). Die Namensfelder heissen ueberall `name`: das Dock soll
//! nicht wissen muessen, ob ein Name aus dem Chat oder aus der Auswertung
//! kommt. Wo das Relay zu einem Login den Anzeigenamen aus dem laufenden Chat
//! kennt, setzt es ihn ein.
//!
//! Der Bot antwortet mit Logins, nie mit IDs; hier kommt also auch keine an.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

use crate::Platform;

/// Pfad der internen Route beim Bot, relativ zur Basis-URL.
pub const PFAD: &str = "/twitch/api/v2/internal/stream-kennzahlen";
/// Header, in dem der interne Token steht. Derselbe wie beim Plattform-Token.
pub const HEADER: &str = "X-Internal-Token";
/// So lange gilt ein geholter Stand als frisch. Das Dock fragt alle 30
/// Sekunden; 25 Sekunden lassen jede Runde einen echten Abruf zu, ohne dass
/// ein zweites Dock desselben Streamers den Bot doppelt belastet.
pub const CACHE_FRIST: Duration = Duration::from_secs(25);
/// So lange wird nach einem erfolglosen Abruf gar nicht gefragt.
///
/// Der Abruf haengt im Dock-Socket, einmal beim Verbinden und dann in jedem
/// Ping-Takt. Ohne diese Pause steht bei haengendem Bot jeder offene Socket
/// alle dreissig Sekunden fuenf Sekunden still, und der Chat-Broadcast kann
/// in der Zeit ueberlaufen. Mit ihr kostet ein toter Bot die Frist einmal je
/// Pause. Dieselbe Klammer wie `hervorhebung::FEHLER_PAUSE`.
pub const FEHLER_PAUSE: Duration = Duration::from_secs(60);
/// So alt darf ein zurueckgehaltener Stand hoechstens werden.
///
/// Antwortet der Bot nicht, zeigt das Dock lieber den letzten bekannten Stand
/// als eine leere Karte. Irgendwann ist "37 gerade" aber keine Auskunft mehr,
/// sondern eine Behauptung. Danach fallen die Verlaufs-Karten weg und
/// `quellen` sagt wieder nur `relay`.
pub const STAND_HOECHSTALTER: Duration = Duration::from_secs(5 * 60);
/// Wie viele Namen je Liste. Gold, Silber, Bronze.
pub const TOP_N: usize = 3;

// ───────────────────────────────────────────────────────────────────────────
// Drahtformat des Rahmens
// ───────────────────────────────────────────────────────────────────────────

/// Ein Name mit seiner Zahl. `wert_name` entscheidet, wie die Zahl heisst.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Nachrichten {
    pub name: String,
    pub nachrichten: u32,
    /// Nur bei Werten aus dem laufenden Chat: von welcher Plattform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plattform: Option<Platform>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Minuten {
    pub name: String,
    pub minuten: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sessions {
    pub name: String,
    pub sessions: i64,
}

/// Eine Kennzahl in beiden Sichten.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sichten<T> {
    pub session: Vec<T>,
    pub gesamt: Vec<T>,
}

// Von Hand statt abgeleitet: `derive(Default)` verlangte `T: Default`, und
// leere Listen brauchen das nicht.
impl<T> Default for Sichten<T> {
    fn default() -> Self {
        Self {
            session: Vec::new(),
            gesamt: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NurGesamt<T> {
    pub gesamt: Vec<T>,
}

impl<T> Default for NurGesamt<T> {
    fn default() -> Self {
        Self { gesamt: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Zuschauer {
    pub jetzt: i64,
    pub spitze_session: i64,
    pub spitze_gesamt: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LurkerSession {
    pub anwesend: i64,
    pub still: i64,
    pub anteil: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LurkerGesamt {
    pub anteil_durchschnitt: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lurker {
    pub session: LurkerSession,
    pub gesamt: LurkerGesamt,
}

/// Der Rahmen, der ueber den Dock-Socket geht.
///
/// `quellen` sagt, woraus dieser Rahmen gebaut ist, nicht wie frisch er ist.
/// Steht dort `bot`, kommen die Verlaufs-Werte vom Bot; ob sie gerade eben
/// oder vor ein paar Minuten geholt wurden, steht in `bot_stand`. Fehlt der
/// Zugang zum Bot, oder ist der letzte Stand aelter als
/// [`STAND_HOECHSTALTER`], bleiben die Verlaufs-Felder leer und `quellen`
/// nennt nur `relay`. Das Dock zeigt dann die Karten, fuer die es Werte hat,
/// und keine Fehlermeldung.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Kennzahlen {
    pub typ: &'static str,
    /// Wann dieser Rahmen entstanden ist.
    pub stand: DateTime<Utc>,
    /// Seit wann das Relay fuer diese Session mitzaehlt.
    pub seit: DateTime<Utc>,
    /// Welche Quellen etwas beigetragen haben: `relay`, `bot`.
    pub quellen: Vec<&'static str>,
    /// Wann die Verlaufs-Werte beim Bot geholt wurden. Fehlt, wenn keine da
    /// sind. Liegt der Wert deutlich hinter `stand`, antwortet der Bot gerade
    /// nicht und die Verlaufs-Karten stehen still.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_stand: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zuschauer: Option<Zuschauer>,
    pub top_chatter: Sichten<Nachrichten>,
    pub laengster_zuschauer: Sichten<Minuten>,
    pub haeufigster_zuschauer: NurGesamt<Sessions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lurker: Option<Lurker>,
}

// ───────────────────────────────────────────────────────────────────────────
// Was der Bot liefert
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotKennzahlen {
    #[serde(default)]
    pub streamer_login: String,
    #[serde(default)]
    pub session_id: i64,
    /// Wann der Bot diese Zahlen gerechnet hat. Fehlt bei einer aelteren
    /// Fassung der Route; dann zaehlt der Zeitpunkt des Abrufs.
    #[serde(default)]
    pub stand: Option<DateTime<Utc>>,
    // Jedes Feld optional: faellt beim Bot eine Kennzahl weg, soll nicht die
    // ganze Antwort unlesbar werden und alle uebrigen Karten mitreissen.
    #[serde(default)]
    pub zuschauer: Option<BotZuschauer>,
    #[serde(default)]
    pub top_chatter: BotSichten<BotNachrichten>,
    #[serde(default)]
    pub laengster_zuschauer: BotSichten<BotMinuten>,
    #[serde(default)]
    pub haeufigster_zuschauer: BotNurGesamt<BotSessions>,
    #[serde(default)]
    pub lurker: Option<BotLurker>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotSichten<T> {
    #[serde(default = "Vec::new")]
    pub session: Vec<T>,
    #[serde(default = "Vec::new")]
    pub gesamt: Vec<T>,
}

impl<T> Default for BotSichten<T> {
    fn default() -> Self {
        Self {
            session: Vec::new(),
            gesamt: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotNurGesamt<T> {
    #[serde(default = "Vec::new")]
    pub gesamt: Vec<T>,
}

impl<T> Default for BotNurGesamt<T> {
    fn default() -> Self {
        Self { gesamt: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotNachrichten {
    pub login: String,
    pub nachrichten: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotMinuten {
    pub login: String,
    pub minuten: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotSessions {
    pub login: String,
    pub sessions: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotZuschauer {
    #[serde(default)]
    pub jetzt: i64,
    #[serde(default)]
    pub spitze_session: i64,
    #[serde(default)]
    pub spitze_gesamt: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BotLurker {
    pub session: BotLurkerSession,
    pub gesamt: BotLurkerGesamt,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct BotLurkerSession {
    #[serde(default)]
    pub anwesend: i64,
    #[serde(default)]
    pub still: i64,
    #[serde(default)]
    pub anteil: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct BotLurkerGesamt {
    #[serde(default)]
    pub anteil_durchschnitt: f64,
}

// ───────────────────────────────────────────────────────────────────────────
// Quelle: holt und cached
// ───────────────────────────────────────────────────────────────────────────

/// Was zu einem gemerkten Stand gehoert: wann er geholt wurde, auf welchen
/// Zeitpunkt er sich bezieht, und der Stand selbst (`None` heisst: der Bot hat
/// ausdruecklich gesagt, dass gerade kein Stream laeuft).
type GemerkterStand = (Instant, DateTime<Utc>, Option<BotKennzahlen>);

/// Holt die Verlaufs-Werte beim Bot und haelt sie kurz.
///
/// Fehlerverhalten, aus dem Blickwinkel des Docks gedacht:
/// - `404`: der Stream laeuft aus Sicht des Bots nicht. Das ist eine Aussage,
///   also wird der alte Stand verworfen und `None` gemerkt.
/// - alles andere (Netz, 5xx, kaputtes JSON): keine Aussage. Der letzte
///   bekannte Stand bleibt stehen, damit ein Aussetzer die Karten nicht leert.
pub struct KennzahlenQuelle {
    broker: std::sync::Arc<dyn crate::PlatformBroker>,
    cache: Mutex<HashMap<i64, GemerkterStand>>,
    frist: Duration,
    /// Bis dahin wird je Streamer nicht wieder angefragt. Gesetzt, wenn der
    /// Bot nicht geantwortet hat.
    pause: Mutex<HashMap<i64, Instant>>,
}

impl KennzahlenQuelle {
    pub fn from_broker(broker: std::sync::Arc<dyn crate::PlatformBroker>) -> Self {
        Self {
            broker,
            cache: Mutex::new(HashMap::new()),
            frist: CACHE_FRIST,
            pause: Mutex::new(HashMap::new()),
        }
    }
    #[cfg(test)]
    pub fn new(base_url: &str, token: &str) -> Self {
        Self::mit_frist(base_url, token, CACHE_FRIST)
    }

    #[cfg(test)]
    pub fn mit_frist(base_url: &str, token: &str, frist: Duration) -> Self {
        let mut s = Self::from_broker(std::sync::Arc::new(crate::token::TestBroker {
            base: base_url.into(),
            token: token.into(),
        }));
        s.frist = frist;
        s
    }

    /// Der Stand fuer einen Streamer, aus dem Cache oder frisch, dazu der
    /// Zeitpunkt, an dem er geholt wurde.
    pub async fn stand(&self, streamer_id: i64) -> (Option<BotKennzahlen>, Option<DateTime<Utc>>) {
        if let Some(treffer) = self.aus_cache(streamer_id) {
            return treffer;
        }
        // Der Bot war eben schon still. Bis die Pause um ist, gilt der letzte
        // bekannte Stand, statt dass jeder Dock-Socket die Frist abwartet.
        if self.in_pause(streamer_id) {
            return self.letzter_stand(streamer_id);
        }
        match self.holen(streamer_id).await {
            Ok(neu) => {
                let jetzt = Utc::now();
                self.pause
                    .lock()
                    .expect("Kennzahlen-Pause")
                    .remove(&streamer_id);
                self.cache
                    .lock()
                    .expect("Kennzahlen-Cache")
                    .insert(streamer_id, (Instant::now(), jetzt, neu.clone()));
                let geholt = neu.as_ref().map(|_| jetzt);
                (neu, geholt)
            }
            Err(grund) => {
                // Kein Urteil moeglich: den letzten bekannten Stand behalten,
                // solange er nicht zu alt ist. Eine leere Karte waere eine
                // Aussage, die wir gerade nicht treffen koennen; eine Stunde
                // alte Zuschauerzahl aber auch.
                tracing::debug!(streamer_id, %grund, "Kennzahlen: Bot ohne Antwort");
                self.pause
                    .lock()
                    .expect("Kennzahlen-Pause")
                    .insert(streamer_id, Instant::now() + FEHLER_PAUSE);
                self.letzter_stand(streamer_id)
            }
        }
    }

    /// Laeuft gerade eine Pause nach einem Fehlschlag?
    fn in_pause(&self, streamer_id: i64) -> bool {
        self.pause
            .lock()
            .expect("Kennzahlen-Pause")
            .get(&streamer_id)
            .is_some_and(|bis| Instant::now() < *bis)
    }

    /// Der letzte bekannte Stand, auch wenn die Cache-Frist abgelaufen ist,
    /// aber nur bis [`STAND_HOECHSTALTER`]. Danach gibt es nichts mehr, und
    /// das Dock zeigt wieder nur, was aus dem laufenden Chat kommt.
    fn letzter_stand(&self, streamer_id: i64) -> (Option<BotKennzahlen>, Option<DateTime<Utc>>) {
        let cache = self.cache.lock().expect("Kennzahlen-Cache");
        let Some((seit, geholt, stand)) = cache.get(&streamer_id) else {
            return (None, None);
        };
        if seit.elapsed() >= STAND_HOECHSTALTER {
            return (None, None);
        }
        match stand {
            Some(_) => (stand.clone(), Some(*geholt)),
            None => (None, None),
        }
    }

    /// Wirft den Stand weg, etwa am Session-Ende.
    pub fn vergessen(&self, streamer_id: i64) {
        self.cache
            .lock()
            .expect("Kennzahlen-Cache")
            .remove(&streamer_id);
        self.pause
            .lock()
            .expect("Kennzahlen-Pause")
            .remove(&streamer_id);
    }

    /// `Some(stand)` wenn frisch genug, sonst `None` (also: neu holen).
    #[allow(clippy::type_complexity)]
    fn aus_cache(
        &self,
        streamer_id: i64,
    ) -> Option<(Option<BotKennzahlen>, Option<DateTime<Utc>>)> {
        let cache = self.cache.lock().expect("Kennzahlen-Cache");
        let (seit, geholt, stand) = cache.get(&streamer_id)?;
        if seit.elapsed() >= self.frist {
            return None;
        }
        Some((stand.clone(), stand.as_ref().map(|_| *geholt)))
    }

    async fn holen(&self, id: i64) -> Result<Option<BotKennzahlen>, String> {
        self.broker
            .metrics(u64::try_from(id).map_err(|_| "Nutzeridentität ist ungültig")?)
            .await
            .map_err(|e| e.to_string())
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Zaehler der laufenden Session
// ───────────────────────────────────────────────────────────────────────────

/// Zaehlt Nachrichten je Autor und Plattform, solange eine Session laeuft.
///
/// Der Schluessel ist `(Plattform, Login)`: derselbe Name auf Twitch und Kick
/// sind zwei Menschen, bis das Gegenteil bewiesen ist. Der Anzeigename wird
/// mitgefuehrt, damit die Karte im Dock zeigt, was auch im Chat steht.
///
/// Eigene Nachrichten des Streamers zaehlen nicht mit: sonst gewinnt er seine
/// eigene Bestenliste.
#[derive(Default)]
pub struct SessionZaehler {
    inner: Mutex<HashMap<i64, Stand>>,
}

/// Eine Zeile der Bestenliste, waehrend sie sortiert wird.
struct Rang {
    platform: Platform,
    login: String,
    anzeige: String,
    anzahl: u32,
}

pub struct Stand {
    seit: DateTime<Utc>,
    /// `(Plattform, Login kleingeschrieben) -> (Anzeigename, Anzahl)`.
    zaehler: HashMap<(Platform, String), (String, u32)>,
}

impl SessionZaehler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Beginnt zu zaehlen. Ein zweiter Aufruf setzt zurueck; eine neue
    /// Session hat eine neue Bestenliste.
    pub fn starten(&self, streamer_id: i64) {
        self.starten_um(streamer_id, Utc::now());
    }

    pub fn starten_um(&self, streamer_id: i64, seit: DateTime<Utc>) {
        self.inner.lock().expect("Session-Zaehler").insert(
            streamer_id,
            Stand {
                seit,
                zaehler: HashMap::new(),
            },
        );
    }

    /// Nimmt den Stand heraus, ohne ihn wegzuwerfen.
    ///
    /// Fuer den einen Fall, in dem die Sitzung neu aufgebaut wird, der Stream
    /// aber derselbe bleibt: der Streamer bucht mitten im Stream eine
    /// Plattform dazu. Ohne diesen Weg spraengen die Zahlen "dieser Stream"
    /// im Karussell auf null zurueck, obwohl niemand neu angefangen hat.
    pub fn entnehmen(&self, streamer_id: i64) -> Option<Stand> {
        self.inner
            .lock()
            .expect("Session-Zaehler")
            .remove(&streamer_id)
    }

    /// Setzt einen zuvor entnommenen Stand zurueck.
    pub fn einsetzen(&self, streamer_id: i64, stand: Stand) {
        self.inner
            .lock()
            .expect("Session-Zaehler")
            .insert(streamer_id, stand);
    }

    /// Session zu Ende: der Zaehler geht weg, nicht auf null. Ohne Session
    /// gibt es keine Session-Werte.
    pub fn beenden(&self, streamer_id: i64) {
        self.inner
            .lock()
            .expect("Session-Zaehler")
            .remove(&streamer_id);
    }

    /// Eine gelesene Zeile. Alles ausser Chat wird ignoriert, eigene
    /// Nachrichten auch.
    pub fn zaehlen(&self, streamer_id: i64, ereignis: &crate::nachricht::Ereignis) {
        let crate::nachricht::Ereignis::Chat(n) = ereignis else {
            return;
        };
        if n.eigene {
            return;
        }
        let login = n.sender_login.trim().to_lowercase();
        if login.is_empty() {
            return;
        }
        let anzeige = if n.sender_display.trim().is_empty() {
            n.sender_login.clone()
        } else {
            n.sender_display.clone()
        };
        let mut inner = self.inner.lock().expect("Session-Zaehler");
        // Ohne laufende Session nichts zaehlen: sonst wuechse hier eine
        // Liste, die zu keinem Stream gehoert.
        let Some(stand) = inner.get_mut(&streamer_id) else {
            return;
        };
        let eintrag = stand
            .zaehler
            .entry((n.platform, login))
            .or_insert((anzeige.clone(), 0));
        eintrag.0 = anzeige;
        eintrag.1 += 1;
    }

    /// Seit wann gezaehlt wird, oder `None` ohne Session.
    pub fn seit(&self, streamer_id: i64) -> Option<DateTime<Utc>> {
        self.inner
            .lock()
            .expect("Session-Zaehler")
            .get(&streamer_id)
            .map(|s| s.seit)
    }

    /// Die Top-Namen dieser Session, meiste Nachrichten zuerst. Gleichstand
    /// nach Login sortiert, damit die Karte zwischen zwei Rahmen nicht
    /// springt.
    pub fn top(&self, streamer_id: i64, n: usize) -> Vec<Nachrichten> {
        let inner = self.inner.lock().expect("Session-Zaehler");
        let Some(stand) = inner.get(&streamer_id) else {
            return Vec::new();
        };
        let mut liste: Vec<Rang> = stand
            .zaehler
            .iter()
            .map(|((platform, login), (anzeige, anzahl))| Rang {
                platform: *platform,
                login: login.clone(),
                anzeige: anzeige.clone(),
                anzahl: *anzahl,
            })
            .collect();
        liste.sort_by(|a, b| {
            b.anzahl
                .cmp(&a.anzahl)
                .then_with(|| a.login.cmp(&b.login))
                .then_with(|| a.platform.rang().cmp(&b.platform.rang()))
        });
        liste
            .into_iter()
            .take(n)
            .map(|r| Nachrichten {
                name: r.anzeige,
                nachrichten: r.anzahl,
                plattform: Some(r.platform),
            })
            .collect()
    }

    /// Anzeigenamen der Twitch-Autoren dieser Session, nach Login.
    ///
    /// Damit ersetzt der Rahmenbau den Login aus der Bot-Auswertung durch den
    /// Namen, den der Streamer gerade im Chat sieht.
    ///
    /// Ausdruecklich nur Twitch: jeder Wert vom Bot ist ein Twitch-Login, und
    /// derselbe Name auf Twitch und Kick sind zwei Menschen. Ohne diese Sperre
    /// wuerde eine Kick-Zuschauerin `cara` ihren Anzeigenamen an die
    /// Twitch-Zahlen einer anderen `cara` heften, und bei zwei gleichen Logins
    /// entschiede die Reihenfolge der Karte, welcher Name gewinnt.
    pub fn anzeigenamen(&self, streamer_id: i64) -> HashMap<String, String> {
        let inner = self.inner.lock().expect("Session-Zaehler");
        let Some(stand) = inner.get(&streamer_id) else {
            return HashMap::new();
        };
        stand
            .zaehler
            .iter()
            .filter(|((platform, _), _)| *platform == Platform::Twitch)
            .map(|((_, login), (anzeige, _))| (login.clone(), anzeige.clone()))
            .collect()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Rahmenbau
// ───────────────────────────────────────────────────────────────────────────

/// Baut den Rahmen aus dem, was da ist. Ohne Bot-Werte bleiben die
/// Verlaufs-Listen leer, ohne Session-Zaehler die Session-Listen.
///
/// `session_laeuft` sagt, ob das Relay gerade eine Session mitzaehlt. Ist es
/// `false`, gehen die `session`-Werte des Bots nicht in den Rahmen, siehe
/// [`session_sichten_leeren`].
pub fn rahmen_bauen(
    seit: DateTime<Utc>,
    session_laeuft: bool,
    top_session: Vec<Nachrichten>,
    anzeigenamen: &HashMap<String, String>,
    bot: Option<&BotKennzahlen>,
    bot_geholt: Option<DateTime<Utc>>,
) -> Kennzahlen {
    let mut quellen = vec!["relay"];
    let name = |login: &str| -> String {
        anzeigenamen
            .get(&login.trim().to_lowercase())
            .cloned()
            .unwrap_or_else(|| login.to_string())
    };

    let Some(bot) = bot else {
        let mut rahmen = Kennzahlen {
            typ: "kennzahlen",
            stand: Utc::now(),
            seit,
            quellen,
            bot_stand: None,
            zuschauer: None,
            top_chatter: Sichten {
                session: top_session,
                gesamt: Vec::new(),
            },
            laengster_zuschauer: Sichten::default(),
            haeufigster_zuschauer: NurGesamt::default(),
            lurker: None,
        };
        if !session_laeuft {
            session_sichten_leeren(&mut rahmen);
        }
        return rahmen;
    };
    quellen.push("bot");

    let mut rahmen = Kennzahlen {
        typ: "kennzahlen",
        stand: Utc::now(),
        // Was der Bot selbst sagt, sonst der Zeitpunkt des Abrufs.
        bot_stand: bot.stand.or(bot_geholt),
        seit,
        quellen,
        zuschauer: bot.zuschauer.as_ref().map(|z| Zuschauer {
            jetzt: z.jetzt,
            spitze_session: z.spitze_session,
            spitze_gesamt: z.spitze_gesamt,
        }),
        top_chatter: Sichten {
            // Der Zaehler des Relays ist die plattformuebergreifende
            // Wahrheit und gewinnt. Hat er nichts (Dock frisch verbunden,
            // noch keine Zeile gelesen), tritt die Twitch-Zahl des Bots an
            // seine Stelle, statt eine leere Karte zu zeigen.
            session: if top_session.is_empty() {
                bot.top_chatter
                    .session
                    .iter()
                    .take(TOP_N)
                    .map(|e| Nachrichten {
                        name: name(&e.login),
                        nachrichten: e.nachrichten.max(0) as u32,
                        plattform: Some(Platform::Twitch),
                    })
                    .collect()
            } else {
                top_session
            },
            gesamt: bot
                .top_chatter
                .gesamt
                .iter()
                .take(TOP_N)
                .map(|e| Nachrichten {
                    name: name(&e.login),
                    nachrichten: e.nachrichten.max(0) as u32,
                    plattform: None,
                })
                .collect(),
        },
        laengster_zuschauer: Sichten {
            session: bot
                .laengster_zuschauer
                .session
                .iter()
                .take(TOP_N)
                .map(|e| Minuten {
                    name: name(&e.login),
                    minuten: e.minuten,
                })
                .collect(),
            gesamt: bot
                .laengster_zuschauer
                .gesamt
                .iter()
                .take(TOP_N)
                .map(|e| Minuten {
                    name: name(&e.login),
                    minuten: e.minuten,
                })
                .collect(),
        },
        haeufigster_zuschauer: NurGesamt {
            gesamt: bot
                .haeufigster_zuschauer
                .gesamt
                .iter()
                .take(TOP_N)
                .map(|e| Sessions {
                    name: name(&e.login),
                    sessions: e.sessions,
                })
                .collect(),
        },
        lurker: bot.lurker.as_ref().map(|l| Lurker {
            session: LurkerSession {
                anwesend: l.session.anwesend,
                still: l.session.still,
                anteil: l.session.anteil,
            },
            gesamt: LurkerGesamt {
                anteil_durchschnitt: l.gesamt.anteil_durchschnitt,
            },
        }),
    };
    if !session_laeuft {
        session_sichten_leeren(&mut rahmen);
    }
    rahmen
}

/// Raeumt alles aus dem Rahmen, was sich auf eine laufende Session bezieht.
///
/// Laeuft keine Session, sind die `session`-Zahlen des Bots die eines Streams,
/// der vorbei ist. Twitch meldet einen abgerissenen Kanal noch ein bis drei
/// Minuten als live, der Bot rechnet in diesem Fenster die alte Session weiter
/// und antwortet mit vollen Session-Listen. Das Dock beschriftet jede dieser
/// Sichten mit "dieser Stream" und behauptete damit "37 gerade" fuer einen
/// beendeten Stream. Der Verlauf (`gesamt`) bleibt, er gilt weiter.
///
/// Geleert statt weggelassen: das Dock zeigt eine Karte nur, wenn ihre Sicht
/// gefuellt ist, leere Liste und Null sind dort schon das Zeichen fuer "keine
/// Karte". Das Drahtformat bleibt damit unveraendert.
fn session_sichten_leeren(rahmen: &mut Kennzahlen) {
    rahmen.top_chatter.session.clear();
    rahmen.laengster_zuschauer.session.clear();
    if let Some(zuschauer) = rahmen.zuschauer.as_mut() {
        // `spitze_gesamt` gehoert zum Verlauf und bleibt stehen.
        zuschauer.jetzt = 0;
        zuschauer.spitze_session = 0;
    }
    if let Some(lurker) = rahmen.lurker.as_mut() {
        lurker.session = LurkerSession {
            anwesend: 0,
            still: 0,
            anteil: 0.0,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nachricht::{ChatNachricht, Ereignis, Fragment};
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn bot_antwort(top_gesamt: &str, nachrichten: i64) -> serde_json::Value {
        json!({
            "streamer_login": "earlysalty",
            "session_id": 91,
            "session_started_at": "2026-08-27T18:00:00Z",
            "stand": "2026-08-27T19:00:00Z",
            "zuschauer": { "jetzt": 37, "spitze_session": 44, "spitze_gesamt": 300 },
            "top_chatter": {
                "session": [{ "login": "anna", "nachrichten": 12 }],
                "gesamt": [{ "login": top_gesamt, "nachrichten": nachrichten }]
            },
            "laengster_zuschauer": {
                "session": [{ "login": "anna", "minuten": 10.0 }],
                "gesamt": [{ "login": "bert", "minuten": 52.0 }]
            },
            "haeufigster_zuschauer": { "gesamt": [{ "login": "bert", "sessions": 9 }] },
            "lurker": {
                "session": { "anwesend": 10, "still": 4, "anteil": 0.4 },
                "gesamt": { "anteil_durchschnitt": 0.55 }
            }
        })
    }

    fn zeile(platform: Platform, login: &str, display: &str, id: &str) -> Ereignis {
        Ereignis::Chat(ChatNachricht {
            platform,
            channel_id: "c1".into(),
            channel_login: "earlysalty".into(),
            message_id: id.into(),
            sender_id: "s1".into(),
            sender_login: login.into(),
            sender_display: display.into(),
            color: None,
            badges: Vec::new(),
            fragments: vec![Fragment::text("hallo")],
            sent_at: Utc::now(),
            is_action: false,
            reply_to: None,
            eigene: false,
            hervorhebung: None,
        })
    }

    // ── Quelle ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn holt_mit_header_und_cached() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("streamer", "7"))
            .and(header(HEADER, "geheim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bot_antwort("cara", 900)))
            .expect(1)
            .mount(&bot)
            .await;

        let quelle = KennzahlenQuelle::new(&bot.uri(), "geheim");
        let erst = quelle.stand(7).await.0.expect("Stand");
        assert_eq!(erst.zuschauer.as_ref().expect("Zuschauer").jetzt, 37);
        assert_eq!(erst.top_chatter.gesamt[0].login, "cara");
        // Zweiter Abruf kommt aus dem Cache; der Mock erwartet genau einen.
        assert_eq!(quelle.stand(7).await.0, Some(erst));
    }

    #[tokio::test]
    async fn ohne_internen_token_kein_stand() {
        // Der Bot antwortet auf einen Aufruf ohne passenden Header mit 401,
        // und 401 ist keine Aussage ueber den Stream: kein Stand, kein Cache.
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(401))
            .mount(&bot)
            .await;
        let quelle = KennzahlenQuelle::new(&bot.uri(), "falsch");
        assert_eq!(quelle.stand(7).await.0, None);
    }

    #[tokio::test]
    async fn vierhundertvier_verwirft_den_alten_stand() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("streamer", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bot_antwort("cara", 900)))
            .up_to_n_times(1)
            .mount(&bot)
            .await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(404))
            .mount(&bot)
            .await;

        // Ohne Cache-Frist geht jeder Abruf zum Bot.
        let quelle = KennzahlenQuelle::mit_frist(&bot.uri(), "geheim", Duration::ZERO);
        assert!(quelle.stand(7).await.0.is_some());
        // Der Stream ist zu Ende: das ist eine Aussage, also weg damit.
        assert_eq!(quelle.stand(7).await.0, None);
    }

    #[tokio::test]
    async fn fuenfhundert_behaelt_den_alten_stand() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("streamer", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bot_antwort("cara", 900)))
            .up_to_n_times(1)
            .mount(&bot)
            .await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(500))
            .mount(&bot)
            .await;

        let quelle = KennzahlenQuelle::mit_frist(&bot.uri(), "geheim", Duration::ZERO);
        let erst = quelle.stand(7).await.0.expect("Stand");
        // Aussetzer: keine Aussage, also bleibt der letzte Stand stehen.
        assert_eq!(quelle.stand(7).await.0, Some(erst));
    }

    #[tokio::test]
    async fn kaputtes_json_behaelt_den_alten_stand() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("streamer", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bot_antwort("cara", 900)))
            .up_to_n_times(1)
            .mount(&bot)
            .await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(200).set_body_string("kein json"))
            .mount(&bot)
            .await;

        let quelle = KennzahlenQuelle::mit_frist(&bot.uri(), "geheim", Duration::ZERO);
        let erst = quelle.stand(7).await.0.expect("Stand");
        assert_eq!(quelle.stand(7).await.0, Some(erst));
    }

    /// Der Abruf haengt im Dock-Socket: was er wartet, wartet das Dock. Ein
    /// stummer Bot darf deshalb nicht in jedem Ping-Takt neu befragt werden.
    #[tokio::test]
    async fn stummer_bot_wird_nicht_bei_jedem_takt_neu_gefragt() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(query_param("streamer", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bot_antwort("cara", 900)))
            .up_to_n_times(1)
            .mount(&bot)
            .await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(500))
            .mount(&bot)
            .await;

        let quelle = KennzahlenQuelle::mit_frist(&bot.uri(), "geheim", Duration::ZERO);
        let erst = quelle.stand(7).await.0.expect("Stand");
        for _ in 0..5 {
            assert_eq!(
                quelle.stand(7).await.0,
                Some(erst.clone()),
                "der letzte bekannte Stand bleibt stehen"
            );
        }

        let anfragen = bot.received_requests().await.unwrap_or_default().len();
        assert_eq!(
            anfragen, 2,
            "ein guter Abruf und ein Fehlschlag, danach Pause, waren aber {anfragen}"
        );
    }

    // ── Zaehler ────────────────────────────────────────────────────────────

    #[test]
    fn zaehlt_je_plattform_und_ignoriert_eigene() {
        let z = SessionZaehler::new();
        z.starten(7);
        z.zaehlen(7, &zeile(Platform::Twitch, "anna", "Anna", "m1"));
        z.zaehlen(7, &zeile(Platform::Twitch, "ANNA", "Anna", "m2"));
        z.zaehlen(7, &zeile(Platform::Kick, "anna", "AnnaAufKick", "m3"));

        // Eigene Zeilen des Streamers zaehlen nicht mit.
        let mut eigene = zeile(Platform::Twitch, "earlysalty", "EarlySalty", "m4");
        if let Ereignis::Chat(n) = &mut eigene {
            n.eigene = true;
        }
        z.zaehlen(7, &eigene);
        // Andere Ereignisarten auch nicht.
        z.zaehlen(
            7,
            &Ereignis::Info(crate::ereignis::StreamInfo {
                platform: Platform::Twitch,
                channel_id: "c1".into(),
                title: "Titel".into(),
                category_id: None,
                category_name: None,
                category_bild: None,
                tags: None,
                is_live: Some(true),
                started_at: None,
                viewers: None,
            }),
        );

        let top = z.top(7, TOP_N);
        assert_eq!(
            top,
            vec![
                Nachrichten {
                    name: "Anna".into(),
                    nachrichten: 2,
                    plattform: Some(Platform::Twitch),
                },
                Nachrichten {
                    name: "AnnaAufKick".into(),
                    nachrichten: 1,
                    plattform: Some(Platform::Kick),
                },
            ],
            "gleicher Login auf zwei Plattformen sind zwei Eintraege"
        );
    }

    #[test]
    fn gleichstand_bleibt_stabil() {
        let z = SessionZaehler::new();
        z.starten(7);
        for login in ["zoe", "mia", "bert", "anna"] {
            z.zaehlen(7, &zeile(Platform::Twitch, login, login, "m"));
        }
        let namen: Vec<String> = z.top(7, TOP_N).into_iter().map(|n| n.name).collect();
        assert_eq!(namen, vec!["anna", "bert", "mia"]);
        // Zweiter Aufruf liefert dieselbe Reihenfolge, nicht die der HashMap.
        let nochmal: Vec<String> = z.top(7, TOP_N).into_iter().map(|n| n.name).collect();
        assert_eq!(namen, nochmal);
    }

    #[test]
    fn session_ende_setzt_zurueck() {
        let z = SessionZaehler::new();
        z.starten(7);
        z.zaehlen(7, &zeile(Platform::Twitch, "anna", "Anna", "m1"));
        assert_eq!(z.top(7, TOP_N).len(), 1);

        z.beenden(7);
        assert!(z.top(7, TOP_N).is_empty());
        assert_eq!(z.seit(7), None);
        // Ohne laufende Session wird nichts mitgezaehlt.
        z.zaehlen(7, &zeile(Platform::Twitch, "anna", "Anna", "m2"));
        assert!(z.top(7, TOP_N).is_empty());

        // Neue Session, neue Bestenliste.
        z.starten(7);
        assert!(z.top(7, TOP_N).is_empty());
    }

    // ── Rahmen ─────────────────────────────────────────────────────────────

    #[test]
    fn ohne_bot_nur_session_werte() {
        let z = SessionZaehler::new();
        z.starten(7);
        z.zaehlen(7, &zeile(Platform::Twitch, "anna", "Anna", "m1"));
        let seit = z.seit(7).expect("Startzeit");

        let rahmen = rahmen_bauen(seit, true, z.top(7, TOP_N), &z.anzeigenamen(7), None, None);
        assert_eq!(rahmen.quellen, vec!["relay"]);
        assert_eq!(rahmen.top_chatter.session[0].name, "Anna");
        assert!(rahmen.top_chatter.gesamt.is_empty());
        assert!(rahmen.laengster_zuschauer.gesamt.is_empty());
        assert_eq!(rahmen.zuschauer, None);
        assert_eq!(rahmen.lurker, None);

        let json = serde_json::to_value(&rahmen).expect("serialisierbar");
        assert_eq!(json["typ"], "kennzahlen");
        assert!(json.get("zuschauer").is_none(), "{json}");
        assert!(json.get("lurker").is_none(), "{json}");
    }

    #[test]
    fn mit_bot_beide_sichten_und_anzeigenamen() {
        let z = SessionZaehler::new();
        z.starten(7);
        // anna schreibt gerade; ihr Anzeigename ersetzt den Login aus der
        // Bot-Auswertung.
        z.zaehlen(7, &zeile(Platform::Twitch, "anna", "Anna 🌸", "m1"));
        let seit = z.seit(7).expect("Startzeit");
        let bot: BotKennzahlen =
            serde_json::from_value(bot_antwort("anna", 900)).expect("Bot-Nutzlast lesbar");

        let rahmen = rahmen_bauen(
            seit,
            true,
            z.top(7, TOP_N),
            &z.anzeigenamen(7),
            Some(&bot),
            None,
        );
        assert_eq!(rahmen.quellen, vec!["relay", "bot"]);
        // Session gewinnt der Relay-Zaehler.
        assert_eq!(rahmen.top_chatter.session[0].name, "Anna 🌸");
        assert_eq!(rahmen.top_chatter.session[0].nachrichten, 1);
        // Gesamt kommt vom Bot, mit dem Namen aus dem laufenden Chat.
        assert_eq!(rahmen.top_chatter.gesamt[0].name, "Anna 🌸");
        assert_eq!(rahmen.top_chatter.gesamt[0].nachrichten, 900);
        // bert schreibt gerade nicht, also bleibt sein Login stehen.
        assert_eq!(rahmen.laengster_zuschauer.gesamt[0].name, "bert");
        assert_eq!(rahmen.haeufigster_zuschauer.gesamt[0].sessions, 9);
        assert_eq!(
            rahmen.zuschauer,
            Some(Zuschauer {
                jetzt: 37,
                spitze_session: 44,
                spitze_gesamt: 300,
            })
        );
        assert_eq!(rahmen.lurker.as_ref().map(|l| l.session.still), Some(4));
        assert_eq!(
            rahmen.lurker.as_ref().map(|l| l.gesamt.anteil_durchschnitt),
            Some(0.55)
        );

        // Kein Login-Feld, keine IDs im Drahtformat: ueberall heisst es `name`.
        let json = serde_json::to_string(&rahmen).expect("serialisierbar");
        assert!(!json.contains("\"login\""), "{json}");
        assert!(!json.contains("sender_id"), "{json}");
    }

    /// Der Bot rechnet Twitch. Wer denselben Login auf Kick benutzt, ist eine
    /// andere Person und darf seinen Anzeigenamen nicht an fremde Zahlen
    /// heften.
    #[test]
    fn ein_kick_name_landet_nicht_an_twitch_zahlen() {
        let z = SessionZaehler::new();
        z.starten(7);
        // Nur die Kick-cara schreibt. Die Twitch-cara, um die es beim Bot
        // geht, sagt in dieser Session kein Wort.
        z.zaehlen(7, &zeile(Platform::Kick, "cara", "CaraAufKick", "m1"));
        let seit = z.seit(7).expect("Startzeit");
        let bot: BotKennzahlen =
            serde_json::from_value(bot_antwort("cara", 900)).expect("Bot-Nutzlast lesbar");

        let rahmen = rahmen_bauen(
            seit,
            true,
            z.top(7, TOP_N),
            &z.anzeigenamen(7),
            Some(&bot),
            None,
        );
        assert_eq!(
            rahmen.top_chatter.gesamt[0].name, "cara",
            "der Twitch-Wert behaelt den Twitch-Login, nicht den Kick-Namen"
        );
        assert_eq!(rahmen.top_chatter.gesamt[0].nachrichten, 900);
        // Die Kick-cara steht weiter mit ihrem Namen in der Session-Karte.
        assert_eq!(rahmen.top_chatter.session[0].name, "CaraAufKick");
        assert_eq!(
            rahmen.top_chatter.session[0].plattform,
            Some(Platform::Kick)
        );
    }

    /// Laeuft keine Session, gehoeren die Session-Zahlen des Bots nicht in
    /// den Rahmen. Twitch meldet einen abgerissenen Stream noch ein bis drei
    /// Minuten als live; der Bot antwortet in dem Fenster mit den Zahlen von
    /// eben, und das Dock schriebe "dieser Stream" ueber einen beendeten.
    #[test]
    fn ohne_session_bleiben_die_session_sichten_leer() {
        let bot: BotKennzahlen =
            serde_json::from_value(bot_antwort("cara", 900)).expect("Bot-Nutzlast lesbar");
        let rahmen = rahmen_bauen(
            Utc::now(),
            false,
            Vec::new(),
            &HashMap::new(),
            Some(&bot),
            None,
        );
        assert!(rahmen.top_chatter.session.is_empty());
        assert!(rahmen.laengster_zuschauer.session.is_empty());
        assert_eq!(
            rahmen.zuschauer,
            Some(Zuschauer {
                jetzt: 0,
                spitze_session: 0,
                // Der Verlauf bleibt stehen.
                spitze_gesamt: 300,
            })
        );
        assert_eq!(
            rahmen.lurker.as_ref().map(|l| l.session.clone()),
            Some(LurkerSession {
                anwesend: 0,
                still: 0,
                anteil: 0.0,
            })
        );
        // Der Verlauf kommt weiter durch.
        assert_eq!(rahmen.top_chatter.gesamt[0].nachrichten, 900);
        assert_eq!(rahmen.laengster_zuschauer.gesamt[0].minuten, 52.0);
        assert_eq!(rahmen.haeufigster_zuschauer.gesamt[0].sessions, 9);
        assert_eq!(
            rahmen.lurker.as_ref().map(|l| l.gesamt.anteil_durchschnitt),
            Some(0.55)
        );
    }

    /// Frisch verbundenes Dock: das Relay hat noch keine Zeile gelesen, der
    /// Bot kennt die Session aber schon. Eine leere Karte waere hier falsch.
    #[test]
    fn ohne_eigene_zeilen_traegt_die_bot_session_die_karte() {
        let bot: BotKennzahlen =
            serde_json::from_value(bot_antwort("cara", 900)).expect("Bot-Nutzlast lesbar");
        let rahmen = rahmen_bauen(
            Utc::now(),
            true,
            Vec::new(),
            &HashMap::new(),
            Some(&bot),
            None,
        );
        assert_eq!(rahmen.top_chatter.session[0].name, "anna");
        assert_eq!(rahmen.top_chatter.session[0].nachrichten, 12);
        assert_eq!(
            rahmen.top_chatter.session[0].plattform,
            Some(Platform::Twitch)
        );
    }
}
