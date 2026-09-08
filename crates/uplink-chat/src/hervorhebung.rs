//! Erstchatter und Raider im Chat-Dock markieren (GRILLME C5, C5-A2).
//!
//! Zwei Dinge sollen im Dock auffallen:
//!
//! - **Erstchatter**: wer zum allerersten Mal in diesem Kanal schreibt, so wie
//!   Twitch es lila hervorhebt. Ausdruecklich NICHT "erste Nachricht in dieser
//!   Session" (C5-A2). Ob jemand neu ist, weiss nur der Bot; das Relay fragt
//!   ihn ueber `GET /twitch/api/v2/internal/chatter-verlauf`.
//! - **Raider**: wer nach einem `channel.raid` von X binnen zehn Minuten seine
//!   erste Nachricht schreibt, gilt als von X mitgebracht. Das ist eine
//!   Annaeherung, genau wie bei Twitch: welcher Zuschauer wirklich aus dem
//!   Raid kam, sagt uns niemand.
//!
//! Die Markierung haelt an (C5-A1): die ersten fuenf Nachrichten des Autors
//! oder zehn Minuten ab seiner ersten Nachricht, was zuerst erreicht ist.
//! Danach steht `art` auf `null`, `nachricht_nr` und `seit` bleiben, damit das
//! Dock den Uebergang selbst darstellen kann.
//!
//! Der Zaehler lebt nur, solange eine Session laeuft. Ohne Session wird nichts
//! markiert; ohne Zugang zum Bot bleibt nur der Raider-Weg, ein Aussetzer des
//! Bots nimmt also nie den ganzen Chat mit.
//!
//! Twitchs EventSub-Nutzlast fuer `channel.chat.message` traegt kein
//! first-msg-Kennzeichen (anders als die alten IRC-Tags). Der Verlauf des Bots
//! ist deshalb die einzige Quelle fuer "Erstchatter".

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

use crate::Platform;
use crate::ereignis::ActivityEvent;
use crate::nachricht::{ChatNachricht, Ereignis};

/// Pfad der internen Route beim Bot, relativ zur Basis-URL.
pub const PFAD: &str = "/twitch/api/v2/internal/chatter-verlauf";
/// Header, in dem der interne Token steht. Derselbe wie beim Plattform-Token.
pub const HEADER: &str = "X-Internal-Token";
/// So viele Logins nimmt der Bot je Anfrage. Mehr wird abgewiesen.
pub const LOGINS_MAX: usize = 50;
/// So lange nach einem Raid gilt eine erste Nachricht als "von dort".
pub const RAID_FENSTER_MIN: i64 = 10;
/// So lange bleibt ein Autor markiert, gerechnet ab seiner ersten Nachricht.
pub const MARKIER_FENSTER_MIN: i64 = 10;
/// So viele Nachrichten bleibt ein Autor markiert.
pub const MARKIER_NACHRICHTEN: u32 = 5;
/// So lange wird nach einem erfolglosen Abruf gar nicht gefragt.
///
/// Der Abruf haengt im Weiterleiter, also wartet der ganze Chat auf ihn. Ohne
/// diese Pause kostet ein toter Bot bei jeder Nachricht eines unbekannten
/// Autors die volle Frist, und der Chat steht. Mit ihr kostet er sie einmal
/// je Pause.
pub const FEHLER_PAUSE: Duration = Duration::from_secs(30);

fn raid_fenster() -> chrono::Duration {
    chrono::Duration::minutes(RAID_FENSTER_MIN)
}

fn markier_fenster() -> chrono::Duration {
    chrono::Duration::minutes(MARKIER_FENSTER_MIN)
}

// ───────────────────────────────────────────────────────────────────────────
// Drahtformat
// ───────────────────────────────────────────────────────────────────────────

/// Warum eine Nachricht auffaellt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Art {
    /// Erster Chat ueberhaupt in diesem Kanal.
    Erstchat,
    /// Erste Nachricht kurz nach einem Raid.
    Raider,
    /// Beides zugleich.
    RaiderErstchat,
}

/// Was das Dock ueber eine Nachricht wissen muss, um sie hervorzuheben.
///
/// `art = null` heisst: nichts hervorheben. `nachricht_nr` und `seit` stehen
/// trotzdem da, weil das Dock daran sieht, wie lange die Markierung noch
/// gilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hervorhebung {
    pub art: Option<Art>,
    /// Anzeigename des Raiders, wenn die Markierung an einem Raid haengt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid_von: Option<String>,
    /// Die wievielte Nachricht dieses Autors in dieser Session.
    pub nachricht_nr: u32,
    /// Wann dieser Autor in dieser Session zum ersten Mal geschrieben hat.
    pub seit: DateTime<Utc>,
}

// ───────────────────────────────────────────────────────────────────────────
// Verlauf beim Bot
// ───────────────────────────────────────────────────────────────────────────

/// Was der Bot je Login sagt.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct VerlaufEintrag {
    pub login: String,
    pub erster_chat_ueberhaupt: bool,
    /// Bei wie vielen Streams dieser Login schon dabei war. Anwesenheit, nicht
    /// Nachrichten. Das Dock zeigt die Zahl heute nicht; sie steht hier, weil
    /// der Bot sie ohne Zusatzkosten mitliefert.
    #[serde(default)]
    pub sessions: i64,
}

/// Fragt den Bot, wer neu im Kanal ist. Immer im Bund, nie einzeln.
pub struct ChatVerlaufQuelle {
    broker: std::sync::Arc<dyn crate::PlatformBroker>,
}

impl ChatVerlaufQuelle {
    pub fn from_broker(broker: std::sync::Arc<dyn crate::PlatformBroker>) -> Self {
        Self { broker }
    }
    #[cfg(test)]
    pub fn new(base_url: &str, token: &str) -> Self {
        Self::from_broker(std::sync::Arc::new(crate::token::TestBroker {
            base: base_url.into(),
            token: token.into(),
        }))
    }

    /// Der Verlauf zu bis zu [`LOGINS_MAX`] Logins.
    ///
    /// `Err` heisst: keine Aussage. Der Aufrufer markiert dann nichts als
    /// Erstchat, statt zu raten.
    pub async fn holen(&self, id: i64, logins: &[String]) -> Result<Vec<VerlaufEintrag>, String> {
        if logins.is_empty() {
            return Ok(Vec::new());
        }
        self.broker
            .chatter_history(
                u64::try_from(id).map_err(|_| "Nutzeridentität ist ungültig")?,
                &logins[..logins.len().min(LOGINS_MAX)],
            )
            .await
            .map_err(|e| e.to_string())
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Der Hervorheber
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Autor {
    erste: DateTime<Utc>,
    anzahl: u32,
    /// Anzeigename des Raiders, wenn die erste Nachricht ins Raid-Fenster fiel.
    raid_von: Option<String>,
}

#[derive(Debug, Clone)]
struct RaidFenster {
    von: String,
    zeit: DateTime<Utc>,
}

#[derive(Default)]
struct Sitzung {
    autoren: HashMap<(Platform, String), Autor>,
    raids: Vec<RaidFenster>,
    /// Login (klein) -> erster Chat ueberhaupt. Nur was der Bot bestaetigt hat.
    erstchat: HashMap<String, bool>,
    /// Logins, nach denen noch nicht gefragt wurde.
    offen: HashSet<String>,
    /// Bis dahin wird nicht wieder beim Bot angefragt. Gesetzt, wenn er
    /// nicht geantwortet hat.
    pause_bis: Option<Instant>,
}

/// Haelt je Streamer-Session den Stand und reichert jede Chatnachricht an.
pub struct Hervorheber {
    verlauf: Option<std::sync::Arc<ChatVerlaufQuelle>>,
    inner: Mutex<HashMap<i64, Sitzung>>,
    /// Ein Abruf zur Zeit **je Streamer**. Wer waehrenddessen fragt, wartet
    /// und findet seine Antwort danach im Bund des Ersten. Ohne diese Klammer
    /// wuerde ein Raid mit hundert neuen Namen hundert Anfragen ausloesen.
    ///
    /// Bewusst je Streamer und nicht einmal fuer alle: die Sperre wird ueber
    /// einen Netzaufruf gehalten, und der Weiterleiter einer Session ist der
    /// einzige, der ihre Zeilen verteilt. Eine gemeinsame Sperre wuerde einen
    /// langsamen Bot in eine Warteschlange ueber alle Streams verwandeln.
    abruf: Mutex<HashMap<i64, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

impl Hervorheber {
    pub fn new(verlauf: Option<std::sync::Arc<ChatVerlaufQuelle>>) -> Self {
        Self {
            verlauf,
            inner: Mutex::new(HashMap::new()),
            abruf: Mutex::new(HashMap::new()),
        }
    }

    /// Session beginnt: leerer Stand. Ein zweiter Aufruf setzt zurueck.
    pub fn starten(&self, streamer_id: i64) {
        self.inner
            .lock()
            .expect("Hervorheber")
            .insert(streamer_id, Sitzung::default());
    }

    /// Session endet: der Stand geht weg, nicht auf null.
    pub fn beenden(&self, streamer_id: i64) {
        self.inner.lock().expect("Hervorheber").remove(&streamer_id);
        self.abruf
            .lock()
            .expect("Hervorheber-Sperren")
            .remove(&streamer_id);
    }

    /// Nimmt ein Ereignis auf, bevor es in den Bus geht. Chatnachrichten
    /// bekommen ihre Hervorhebung, Raids oeffnen ein Fenster, alles andere
    /// laeuft unveraendert durch.
    pub async fn anreichern(&self, streamer_id: i64, ereignis: &mut Ereignis) {
        self.anreichern_um(streamer_id, ereignis, Utc::now()).await;
    }

    /// Wie [`Self::anreichern`], nur mit gesetzter Uhr. Die Fenster sind
    /// Minuten lang; ein Test, der sie in Echtzeit abwartet, waere kein Test.
    pub async fn anreichern_um(
        &self,
        streamer_id: i64,
        ereignis: &mut Ereignis,
        jetzt: DateTime<Utc>,
    ) {
        match ereignis {
            Ereignis::Activity(ActivityEvent::Raid { meta, from, .. }) => {
                let von = meta
                    .actor
                    .as_ref()
                    .map(|a| a.display.clone())
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| from.clone());
                let mut inner = self.inner.lock().expect("Hervorheber");
                if let Some(sitzung) = inner.get_mut(&streamer_id) {
                    sitzung.raids.push(RaidFenster {
                        von,
                        zeit: meta.occurred_at,
                    });
                    // Alte Fenster wegwerfen: sie koennen nichts mehr
                    // markieren und wuerden sonst bis zum Session-Ende
                    // mitwachsen.
                    sitzung.raids.retain(|r| jetzt - r.zeit < raid_fenster());
                }
            }
            Ereignis::Chat(nachricht) => {
                let hervorhebung = self.fuer_nachricht(streamer_id, nachricht, jetzt).await;
                nachricht.hervorhebung = hervorhebung;
            }
            _ => {}
        }
    }

    async fn fuer_nachricht(
        &self,
        streamer_id: i64,
        nachricht: &ChatNachricht,
        jetzt: DateTime<Utc>,
    ) -> Option<Hervorhebung> {
        if nachricht.eigene {
            return None;
        }
        let login = nachricht.sender_login.trim().to_lowercase();
        if login.is_empty() {
            return None;
        }

        let (nr, seit, raid_von, erstchat_bekannt) = {
            let mut inner = self.inner.lock().expect("Hervorheber");
            // Ohne laufende Session wird nicht markiert.
            let sitzung = inner.get_mut(&streamer_id)?;
            let schluessel = (nachricht.platform, login.clone());
            let neu = !sitzung.autoren.contains_key(&schluessel);
            let raid_von = if neu {
                // Der juengste Raid, dessen Fenster noch offen ist.
                sitzung
                    .raids
                    .iter()
                    .filter(|r| jetzt >= r.zeit && jetzt - r.zeit < raid_fenster())
                    .max_by_key(|r| r.zeit)
                    .map(|r| r.von.clone())
            } else {
                None
            };
            let autor = sitzung.autoren.entry(schluessel).or_insert(Autor {
                erste: jetzt,
                anzahl: 0,
                raid_von,
            });
            autor.anzahl = autor.anzahl.saturating_add(1);
            let erstchat_bekannt = sitzung.erstchat.get(&login).copied();
            // Nur Twitch-Logins vormerken. Der Bot fuehrt nur den
            // Twitch-Verlauf; ein Kick-Name im Bund verdraengt bei mehr als
            // LOGINS_MAX offenen Namen einen echten Twitch-Namen und
            // verzoegert dessen Markierung.
            if erstchat_bekannt.is_none() && nachricht.platform == Platform::Twitch {
                sitzung.offen.insert(login.clone());
            }
            (
                autor.anzahl,
                autor.erste,
                autor.raid_von.clone(),
                erstchat_bekannt,
            )
        };

        // Erstchat gibt es nur fuer Twitch: nur dort fuehrt der Bot einen
        // Verlauf. Andere Plattformen bleiben beim Raider-Weg.
        let erstchat = if nachricht.platform == Platform::Twitch {
            match erstchat_bekannt {
                Some(wert) => wert,
                None => self.erstchat_holen(streamer_id, &login).await,
            }
        } else {
            false
        };

        let markiert =
            nr <= MARKIER_NACHRICHTEN && jetzt >= seit && jetzt - seit < markier_fenster();
        let art = if !markiert {
            None
        } else {
            match (raid_von.is_some(), erstchat) {
                (true, true) => Some(Art::RaiderErstchat),
                (true, false) => Some(Art::Raider),
                (false, true) => Some(Art::Erstchat),
                (false, false) => None,
            }
        };
        Some(Hervorhebung {
            art,
            raid_von: if markiert { raid_von } else { None },
            nachricht_nr: nr,
            seit,
        })
    }

    /// Die Abruf-Sperre dieses Streamers, bei Bedarf neu angelegt.
    fn abruf_sperre(&self, streamer_id: i64) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.abruf
            .lock()
            .expect("Hervorheber-Sperren")
            .entry(streamer_id)
            .or_default()
            .clone()
    }

    /// Laeuft fuer diesen Streamer gerade eine Pause nach einem Fehlschlag?
    fn pausiert(&self, streamer_id: i64) -> bool {
        self.inner
            .lock()
            .expect("Hervorheber")
            .get(&streamer_id)
            .and_then(|s| s.pause_bis)
            .is_some_and(|bis| Instant::now() < bis)
    }

    /// Holt den Verlauf fuer alle offenen Logins der Session in einem Zug.
    /// Liefert die Antwort fuer `login`; ohne Zugang oder ohne Antwort
    /// `false`, also keine Markierung.
    async fn erstchat_holen(&self, streamer_id: i64, login: &str) -> bool {
        let Some(quelle) = self.verlauf.as_ref() else {
            return false;
        };
        // Erst pruefen, dann anstellen. Die Sperre gilt fuer alle Streamer;
        // wer in der Pause ist, darf sich gar nicht erst in die Reihe
        // stellen, sonst wartet bei stummem Bot jede Session hinter jeder
        // anderen.
        if self.pausiert(streamer_id) {
            return false;
        }
        let sperre = self.abruf_sperre(streamer_id);
        let _gehalten = sperre.lock().await;
        // Waehrend des Wartens kann ein anderer Abruf die Antwort schon
        // mitgebracht haben.
        let offen: Vec<String> = {
            let inner = self.inner.lock().expect("Hervorheber");
            let Some(sitzung) = inner.get(&streamer_id) else {
                return false;
            };
            if let Some(wert) = sitzung.erstchat.get(login) {
                return *wert;
            }
            // Der Bot war eben schon still. Bis die Pause um ist, laeuft der
            // Chat ohne Erstchatter-Markierung weiter, statt auf jede Frist
            // zu warten.
            if sitzung.pause_bis.is_some_and(|bis| Instant::now() < bis) {
                return false;
            }

            sitzung.offen.iter().take(LOGINS_MAX).cloned().collect()
        };
        if offen.is_empty() {
            return false;
        }
        match quelle.holen(streamer_id, &offen).await {
            Ok(eintraege) => {
                let mut inner = self.inner.lock().expect("Hervorheber");
                let Some(sitzung) = inner.get_mut(&streamer_id) else {
                    return false;
                };
                for eintrag in &eintraege {
                    let name = eintrag.login.trim().to_lowercase();
                    sitzung
                        .erstchat
                        .insert(name.clone(), eintrag.erster_chat_ueberhaupt);
                    sitzung.offen.remove(&name);
                }
                // Wonach gefragt wurde und was nicht zurueckkam, gilt als
                // beantwortet: sonst fragen wir bei jeder Nachricht neu.
                for name in &offen {
                    sitzung.erstchat.entry(name.clone()).or_insert(false);
                    sitzung.offen.remove(name);
                }
                sitzung.pause_bis = None;
                sitzung.erstchat.get(login).copied().unwrap_or(false)
            }
            Err(grund) => {
                // Keine Aussage: nichts markieren und nichts merken, damit
                // die Frage spaeter neu gestellt wird. Bis dahin aber Pause,
                // sonst wartet jede Nachricht eines unbekannten Autors die
                // volle Frist ab und der Chat kommt ins Stocken.
                if let Some(sitzung) = self
                    .inner
                    .lock()
                    .expect("Hervorheber")
                    .get_mut(&streamer_id)
                {
                    sitzung.pause_bis = Some(Instant::now() + FEHLER_PAUSE);
                }
                tracing::debug!(streamer_id, %grund, "Erstchat: Bot ohne Antwort");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ereignis::{ActivityMeta, Actor};
    use crate::nachricht::Fragment;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn zeit(minuten: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minuten)
    }

    fn chat(login: &str, id: &str) -> Ereignis {
        Ereignis::Chat(ChatNachricht {
            platform: Platform::Twitch,
            channel_id: "c1".into(),
            channel_login: "earlysalty".into(),
            message_id: id.into(),
            sender_id: "s1".into(),
            sender_login: login.into(),
            sender_display: login.into(),
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

    fn raid(von: &str, anzeige: &str, um: DateTime<Utc>) -> Ereignis {
        Ereignis::Activity(ActivityEvent::Raid {
            meta: ActivityMeta {
                platform: Platform::Twitch,
                channel_id: "c1".into(),
                occurred_at: um,
                dedupe_key: format!("twitch:c1:raid:{von}"),
                actor: Some(Actor::new("999", von, anzeige)),
            },
            from: von.into(),
            viewers: 42,
        })
    }

    fn hervorhebung(ereignis: &Ereignis) -> Option<&Hervorhebung> {
        let Ereignis::Chat(n) = ereignis else {
            return None;
        };
        n.hervorhebung.as_ref()
    }

    /// Bot-Attrappe: die genannten Logins sind neu, alle anderen nicht.
    async fn bot_mit(neue: &[&str]) -> MockServer {
        let bot = MockServer::start().await;
        let neue: Vec<String> = neue.iter().map(|s| s.to_string()).collect();
        Mock::given(method("GET"))
            .and(path(PFAD))
            .and(header(HEADER, "geheim"))
            .respond_with(move |anfrage: &wiremock::Request| {
                let logins = anfrage
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "logins")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_default();
                let eintraege: Vec<serde_json::Value> = logins
                    .split(',')
                    .filter(|l| !l.is_empty())
                    .map(|login| {
                        json!({
                            "login": login,
                            "erster_chat_ueberhaupt": neue.iter().any(|n| n == login),
                            "sessions": 0
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({ "eintraege": eintraege }))
            })
            .mount(&bot)
            .await;
        bot
    }

    fn hervorheber(bot: &MockServer) -> Hervorheber {
        Hervorheber::new(Some(Arc::new(ChatVerlaufQuelle::new(&bot.uri(), "geheim"))))
    }

    #[tokio::test]
    async fn erstchat_kommt_aus_dem_bot_verlauf() {
        let bot = bot_mit(&["neuling"]).await;
        let h = hervorheber(&bot);
        h.starten(7);

        let mut neu = chat("neuling", "m1");
        h.anreichern_um(7, &mut neu, zeit(0)).await;
        let markierung = hervorhebung(&neu).expect("Hervorhebung");
        assert_eq!(markierung.art, Some(Art::Erstchat));
        assert_eq!(markierung.nachricht_nr, 1);
        assert_eq!(markierung.raid_von, None);

        // Ein Stammgast bekommt nichts, obwohl es seine erste Nachricht in
        // dieser Session ist: "Erstchatter" heisst erster Chat ueberhaupt.
        let mut alt = chat("stammgast", "m2");
        h.anreichern_um(7, &mut alt, zeit(0)).await;
        assert_eq!(hervorhebung(&alt).expect("Hervorhebung").art, None);
        assert_eq!(hervorhebung(&alt).expect("Hervorhebung").nachricht_nr, 1);
    }

    #[tokio::test]
    async fn ohne_bot_antwort_bleibt_nur_der_raider_weg() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(500))
            .mount(&bot)
            .await;
        let h = hervorheber(&bot);
        h.starten(7);

        let mut neu = chat("neuling", "m1");
        h.anreichern_um(7, &mut neu, zeit(0)).await;
        assert_eq!(hervorhebung(&neu).expect("Hervorhebung").art, None);

        // Raid wirkt trotzdem.
        let mut r = raid("raiderin", "Raiderin", zeit(1));
        h.anreichern_um(7, &mut r, zeit(1)).await;
        let mut nach = chat("mitgebracht", "m2");
        h.anreichern_um(7, &mut nach, zeit(2)).await;
        let markierung = hervorhebung(&nach).expect("Hervorhebung");
        assert_eq!(markierung.art, Some(Art::Raider));
        assert_eq!(markierung.raid_von.as_deref(), Some("Raiderin"));
    }

    #[tokio::test]
    async fn raid_fenster_gilt_zehn_minuten() {
        let bot = bot_mit(&[]).await;
        let h = hervorheber(&bot);
        h.starten(7);

        let mut r = raid("raiderin", "Raiderin", zeit(0));
        h.anreichern_um(7, &mut r, zeit(0)).await;

        // Neun Minuten nach dem Raid: noch drin.
        let mut frueh = chat("frueh", "m1");
        h.anreichern_um(7, &mut frueh, zeit(9)).await;
        assert_eq!(
            hervorhebung(&frueh).expect("Hervorhebung").art,
            Some(Art::Raider)
        );

        // Elf Minuten danach: nicht mehr.
        let mut spaet = chat("spaet", "m2");
        h.anreichern_um(7, &mut spaet, zeit(11)).await;
        let markierung = hervorhebung(&spaet).expect("Hervorhebung");
        assert_eq!(markierung.art, None);
        assert_eq!(markierung.raid_von, None);
    }

    #[tokio::test]
    async fn raider_und_erstchat_zugleich() {
        let bot = bot_mit(&["neuling"]).await;
        let h = hervorheber(&bot);
        h.starten(7);
        let mut r = raid("raiderin", "Raiderin", zeit(0));
        h.anreichern_um(7, &mut r, zeit(0)).await;

        let mut neu = chat("neuling", "m1");
        h.anreichern_um(7, &mut neu, zeit(1)).await;
        let markierung = hervorhebung(&neu).expect("Hervorhebung");
        assert_eq!(markierung.art, Some(Art::RaiderErstchat));
        assert_eq!(markierung.raid_von.as_deref(), Some("Raiderin"));
    }

    #[tokio::test]
    async fn markierung_endet_nach_fuenf_nachrichten() {
        let bot = bot_mit(&["neuling"]).await;
        let h = hervorheber(&bot);
        h.starten(7);

        for nr in 1..=5 {
            let mut e = chat("neuling", &format!("m{nr}"));
            h.anreichern_um(7, &mut e, zeit(0)).await;
            let markierung = hervorhebung(&e).expect("Hervorhebung");
            assert_eq!(markierung.art, Some(Art::Erstchat), "Nachricht {nr}");
            assert_eq!(markierung.nachricht_nr, nr);
        }
        // Die sechste ist normal, traegt die Zahl aber weiter.
        let mut sechste = chat("neuling", "m6");
        h.anreichern_um(7, &mut sechste, zeit(0)).await;
        let markierung = hervorhebung(&sechste).expect("Hervorhebung");
        assert_eq!(markierung.art, None);
        assert_eq!(markierung.nachricht_nr, 6);
    }

    #[tokio::test]
    async fn markierung_endet_nach_zehn_minuten() {
        let bot = bot_mit(&["neuling"]).await;
        let h = hervorheber(&bot);
        h.starten(7);

        let mut erste = chat("neuling", "m1");
        h.anreichern_um(7, &mut erste, zeit(0)).await;
        assert_eq!(
            hervorhebung(&erste).expect("Hervorhebung").art,
            Some(Art::Erstchat)
        );

        // Neun Minuten spaeter, zweite Nachricht: noch markiert.
        let mut zweite = chat("neuling", "m2");
        h.anreichern_um(7, &mut zweite, zeit(9)).await;
        assert_eq!(
            hervorhebung(&zweite).expect("Hervorhebung").art,
            Some(Art::Erstchat)
        );

        // Elf Minuten spaeter: vorbei, obwohl erst die dritte Nachricht.
        let mut dritte = chat("neuling", "m3");
        h.anreichern_um(7, &mut dritte, zeit(11)).await;
        let markierung = hervorhebung(&dritte).expect("Hervorhebung");
        assert_eq!(markierung.art, None);
        assert_eq!(markierung.nachricht_nr, 3);
        assert_eq!(markierung.seit, zeit(0));
    }

    #[tokio::test]
    async fn ohne_session_wird_nichts_markiert() {
        let bot = bot_mit(&["neuling"]).await;
        let h = hervorheber(&bot);
        // Kein `starten`: der Stream laeuft nicht.
        let mut e = chat("neuling", "m1");
        h.anreichern_um(7, &mut e, zeit(0)).await;
        assert_eq!(hervorhebung(&e), None);

        h.starten(7);
        let mut zwei = chat("neuling", "m2");
        h.anreichern_um(7, &mut zwei, zeit(0)).await;
        assert!(hervorhebung(&zwei).is_some());

        // Session zu Ende: wieder nichts, und der Zaehler faengt bei der
        // naechsten Session von vorn an.
        h.beenden(7);
        let mut drei = chat("neuling", "m3");
        h.anreichern_um(7, &mut drei, zeit(0)).await;
        assert_eq!(hervorhebung(&drei), None);

        h.starten(7);
        let mut vier = chat("neuling", "m4");
        h.anreichern_um(7, &mut vier, zeit(0)).await;
        assert_eq!(hervorhebung(&vier).expect("Hervorhebung").nachricht_nr, 1);
    }

    /// Ein Raid bringt viele neue Namen auf einmal. Sie duerfen nicht je
    /// eine eigene Anfrage kosten: was einmal beantwortet ist, steht im
    /// Cache der Session.
    #[tokio::test]
    async fn zweiter_blick_auf_denselben_autor_fragt_nicht_nochmal() {
        let bot = bot_mit(&["neuling"]).await;
        let h = hervorheber(&bot);
        h.starten(7);

        for nr in 1..=4 {
            let mut e = chat("neuling", &format!("m{nr}"));
            h.anreichern_um(7, &mut e, zeit(0)).await;
            assert_eq!(
                hervorhebung(&e).expect("Hervorhebung").art,
                Some(Art::Erstchat)
            );
        }
        // Ein zweiter Autor war beim ersten Abruf schon offen und ist damit
        // mitbeantwortet worden.
        let mut anderer = chat("stammgast", "m9");
        h.anreichern_um(7, &mut anderer, zeit(0)).await;
        assert_eq!(hervorhebung(&anderer).expect("Hervorhebung").art, None);

        let anfragen = bot.received_requests().await.unwrap_or_default().len();
        assert!(
            anfragen <= 2,
            "fuenf Nachrichten von zwei Autoren, hoechstens zwei Anfragen, waren {anfragen}"
        );
    }

    /// Der Abruf haengt im Weiterleiter: was er wartet, wartet der ganze
    /// Chat. Ein stummer Bot darf deshalb nicht bei jeder Nachricht eines
    /// unbekannten Autors neu befragt werden.
    #[tokio::test]
    async fn stummer_bot_wird_nicht_bei_jeder_nachricht_neu_gefragt() {
        let bot = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(500))
            .mount(&bot)
            .await;
        let h = hervorheber(&bot);
        h.starten(7);

        for (nr, autor) in ["ein", "zwei", "drei", "vier", "fuenf"].iter().enumerate() {
            let mut e = chat(autor, &format!("m{nr}"));
            h.anreichern_um(7, &mut e, zeit(0)).await;
            assert_eq!(
                hervorhebung(&e).expect("Hervorhebung").art,
                None,
                "ohne Antwort wird nichts markiert"
            );
        }

        let anfragen = bot.received_requests().await.unwrap_or_default().len();
        assert_eq!(
            anfragen, 1,
            "nach einem Fehlschlag gilt eine Pause, es war aber {anfragen} mal"
        );
    }
}
