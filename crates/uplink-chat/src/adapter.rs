//! Schnittstelle eines Chat-Adapters je Plattform.
//!
//! Ein Adapter gehoert genau einem Streamer und genau einer Plattform. Er
//! liest den Chat der Plattform und gibt jede Zeile als [`ChatNachricht`]
//! in den `eingang` des Supervisors; er sendet Text im Namen des Streamers.
//! Verbindungsaufbau, Reconnect und Token-Erneuerung sind seine Sache, der
//! Supervisor sieht nur verbinden, trennen, senden.

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::mpsc;

use crate::Platform;
use crate::nachricht::Ereignis;

/// Was beim Verbinden oder Senden schiefgehen kann.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatFehler {
    /// Fuer diese Plattform gibt es noch keinen Adapter.
    #[error("{0} hat noch keinen Chat-Adapter")]
    NichtUnterstuetzt(Platform),
    /// Der Streamer hat die Plattform nicht mit uns verbunden (Bot: 404), oder
    /// das Relay laeuft ohne Zugang zum Bot.
    #[error("{0} ist nicht verbunden")]
    NichtVerbunden(Platform),
    /// Der Zugang ist abgelaufen oder widerrufen (Bot: 409, Plattform: 403,
    /// EventSub-Revocation). Der Streamer muss sich im Dashboard neu anmelden.
    #[error("{0}: Anmeldung muss erneuert werden")]
    NeuAnmeldungNoetig(Platform),
    /// Die Plattform hat die Nachricht angenommen, aber nicht zugestellt
    /// (Twitch `is_sent=false`). Kein Fehler des Relays.
    #[error("Nachricht verworfen: {0}")]
    Verworfen(String),
    /// Kein Adapter laeuft gerade, weil keine Session offen ist.
    #[error("kein Chat aktiv")]
    KeinAdapter,
    /// Die Plattform lehnt die Eingabe ab (400) oder die Aktion fuer dieses
    /// Objekt (403 bei fremden Rewards). Der Text ist fuer den Streamer.
    #[error("{0}")]
    Abgelehnt(String),
    /// Der Bot lehnt unseren internen Token ab (401 oder 403 auf der internen
    /// Route). Das ist keine Sache des Streamers, sondern der Konfiguration
    /// des Relays, und es aendert sich ohne Eingriff nicht: der Adapter gibt
    /// dann auf, statt bis zum Stream-Ende alle 30 s nachzufragen.
    #[error("Bot lehnt den internen Token ab")]
    InternerZugang,
    /// Netz oder Plattform antworten nicht wie erwartet.
    #[error("{0}")]
    Netz(String),
}

impl ChatFehler {
    /// Terminale Zugangsfehler erfordern eine Änderung in der Kontoverwaltung.
    pub fn vergeht_von_allein(&self) -> bool {
        !matches!(
            self,
            Self::NichtUnterstuetzt(_)
                | Self::NichtVerbunden(_)
                | Self::NeuAnmeldungNoetig(_)
                | Self::InternerZugang
        )
    }
}

/// Ein Chat-Adapter.
///
/// Die Methoden geben `BoxFuture` zurueck statt `async fn`, damit der Trait
/// als `dyn ChatAdapter` im Supervisor liegen kann.
pub trait ChatAdapter: Send + Sync {
    fn platform(&self) -> Platform;
    /// Autorisierte Plattformidentität, damit eine neue Verbindung alte Adapter ersetzt.
    fn account_id(&self) -> Option<&str> {
        None
    }
    /// Baut die Verbindung auf und haelt sie im Hintergrund. Kehrt zurueck,
    /// sobald die Leseschleife laeuft; Fehler danach behebt der Adapter selbst
    /// (Backoff), ausser der Zugang ist widerrufen.
    fn verbinden(&self) -> BoxFuture<'_, Result<(), ChatFehler>>;
    /// Beendet die Leseschleife. Mehrfach aufrufbar.
    fn trennen(&self) -> BoxFuture<'_, ()>;
    /// Sendet Text in den Kanal des Streamers.
    fn senden(&self, text: &str) -> BoxFuture<'_, Result<(), ChatFehler>>;
    /// Ob die Leseverbindung gerade steht.
    fn verbunden(&self) -> bool;
    /// Scopes, die der Zugang nicht hat und deren Funktion deshalb fehlt
    /// (Follows, Abos, Bits, Kanalpunkte). Chat laeuft trotzdem (INV-6).
    fn fehlende_scopes(&self) -> Vec<String> {
        Vec::new()
    }
    /// Warum der Adapter von selbst aufgehoert hat, falls er das tat.
    ///
    /// Der Supervisor merkt sich nur, was beim Verbinden schiefging. Gibt die
    /// Leseschleife spaeter auf (Zugang widerrufen, Streamer hat die
    /// Plattform im Dashboard getrennt), stuende im Dock sonst "getrennt"
    /// ohne Grund.
    fn ende_grund(&self) -> Option<ChatFehler> {
        None
    }
}

/// Baut Adapter. Im Dienst die Twitch-Fabrik, im Test ein Fake.
pub trait AdapterFabrik: Send + Sync {
    /// Ob der Streamer diese Plattform im Dashboard verbunden hat, unabhaengig
    /// davon, ob gerade ein Stream laeuft. Die Kopfzeile der Dock-Fenster
    /// zeigt nur verbundene Plattformen; ohne das stuenden dort dauerhaft
    /// vier graue Namen, von denen drei nie etwas liefern koennen.
    ///
    /// Vorgabe `false`: eine Fabrik ohne Zugang zur Plattform weiss es nicht
    /// und behauptet nichts.
    fn eingerichtet(&self, _streamer_id: i64, _platform: Platform) -> BoxFuture<'_, bool> {
        Box::pin(async move { false })
    }

    /// Ein Adapter fuer diesen Streamer und diese Plattform, mit dem Kanal, in
    /// den er gelesene Ereignisse legt (Chat, Aktivitaeten, Punkte, Infos). `Err` heisst: kein Adapter, und der
    /// Supervisor merkt sich den Grund fuer das Dock.
    fn bauen(
        &self,
        streamer_id: i64,
        platform: Platform,
        eingang: mpsc::Sender<Ereignis>,
    ) -> BoxFuture<'_, Result<Arc<dyn ChatAdapter>, ChatFehler>>;
}

/// Fabrik ohne Zugang zum Bot: das Relay laeuft ohne Chat.
pub struct OhneChat;

impl AdapterFabrik for OhneChat {
    fn bauen(
        &self,
        _streamer_id: i64,
        platform: Platform,
        _eingang: mpsc::Sender<Ereignis>,
    ) -> BoxFuture<'_, Result<Arc<dyn ChatAdapter>, ChatFehler>> {
        Box::pin(async move { Err(ChatFehler::NichtVerbunden(platform)) })
    }
}

/// Antwort der Fabrik fuer Plattformen ohne Adapter.
pub fn stub(platform: Platform) -> Result<Arc<dyn ChatAdapter>, ChatFehler> {
    Err(ChatFehler::NichtUnterstuetzt(platform))
}

pub struct PlattformFabrik {
    twitch: crate::twitch::TwitchFabrik,
    kick: crate::kick::KickFabrik,
    youtube: crate::youtube::YouTubeFabrik,
}

impl PlattformFabrik {
    pub fn neu(
        quelle: Arc<crate::token::TokenQuelle>,
        drehkreuz: Arc<crate::kick_webhook::KickDrehkreuz>,
    ) -> Self {
        Self {
            twitch: crate::twitch::TwitchFabrik::new(quelle.clone()),
            kick: crate::kick::KickFabrik::new(quelle.clone(), drehkreuz),
            youtube: crate::youtube::YouTubeFabrik::new(quelle),
        }
    }
}

impl AdapterFabrik for PlattformFabrik {
    fn eingerichtet(&self, streamer_id: i64, platform: Platform) -> BoxFuture<'_, bool> {
        match platform {
            Platform::Twitch => self.twitch.eingerichtet(streamer_id, platform),
            Platform::Kick => self.kick.eingerichtet(streamer_id, platform),
            Platform::YouTube => self.youtube.eingerichtet(streamer_id, platform),
            Platform::TikTok => Box::pin(async move { false }),
        }
    }

    fn bauen(
        &self,
        streamer_id: i64,
        platform: Platform,
        eingang: mpsc::Sender<Ereignis>,
    ) -> BoxFuture<'_, Result<Arc<dyn ChatAdapter>, ChatFehler>> {
        match platform {
            Platform::Twitch => self.twitch.bauen(streamer_id, platform, eingang),
            Platform::Kick => self.kick.bauen(streamer_id, platform, eingang),
            Platform::YouTube => self.youtube.bauen(streamer_id, platform, eingang),
            Platform::TikTok => Box::pin(async move { stub(platform) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ohne_chat_liefert_fuer_jede_plattform_nicht_verbunden() {
        let (tx, _rx) = mpsc::channel(1);
        for platform in Platform::ALL {
            let fehler = OhneChat.bauen(1, platform, tx.clone()).await.err();
            assert_eq!(fehler, Some(ChatFehler::NichtVerbunden(platform)));
        }
    }

    #[test]
    fn stub_meldet_nicht_unterstuetzt() {
        assert_eq!(
            stub(Platform::Kick).err(),
            Some(ChatFehler::NichtUnterstuetzt(Platform::Kick))
        );
    }

    #[tokio::test]
    async fn verteiler_laesst_tiktok_stub() {
        let quelle = Arc::new(crate::token::TokenQuelle::new("http://127.0.0.1:0", "x"));
        let drehkreuz = Arc::new(crate::kick_webhook::KickDrehkreuz::new(
            "http://127.0.0.1:0",
        ));
        let fabrik = PlattformFabrik::neu(quelle, drehkreuz);
        let (tx, _rx) = mpsc::channel(1);
        assert_eq!(
            fabrik.bauen(7, Platform::TikTok, tx).await.err(),
            Some(ChatFehler::NichtUnterstuetzt(Platform::TikTok))
        );
        assert!(!fabrik.eingerichtet(7, Platform::TikTok).await);
    }
}
