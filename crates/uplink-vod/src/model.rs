use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Twitch,
    YouTube,
}

pub struct Secret(Zeroizing<String>);
impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([geschützt])")
    }
}

#[derive(Debug)]
pub struct Grant {
    pub access_token: Secret,
    pub expires_at: DateTime<Utc>,
    pub platform_user_id: String,
    pub scopes: Vec<String>,
}
#[async_trait]
pub trait PlatformBroker: Send + Sync {
    async fn grant(&self, streamer_id: u64, platform: Platform, refresh: bool) -> Result<Grant>;
}
/// Derselbe Dienstschlüssel, keine zweite Ablage. AAD bindet Nutzer und Auftrag.
pub trait Cipher: Send + Sync {
    fn seal(&self, plaintext: &[u8], aad: &str) -> Result<Vec<u8>>;
    fn open(&self, ciphertext: &[u8], aad: &str) -> Result<Secret>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    #[error("VOD-Konfiguration ist ungültig.")]
    Invalid,
    #[error("Die Kontoverbindung fehlt.")]
    Disconnected,
    #[error("Die Kontoverbindung muss im Dashboard erneut freigegeben werden.")]
    NeedsReauth,
    #[error("Die Kontoverwaltung ist gerade nicht erreichbar.")]
    BrokerUnavailable,
    #[error("Der autorisierte Kanal stimmt nicht mit dem Auftrag überein.")]
    WrongChannel,
    #[error("Die Plattform begrenzt die Übertragung. Der Auftrag wartet.")]
    Quota,
    #[error("Die Plattform ist vorübergehend nicht erreichbar.")]
    Network,
    #[error("Die Plattformantwort ist unvollständig oder ungültig.")]
    Protocol,
    #[error("Der Uploadabschluss ist unklar. Vor einem neuen Upload abgleichen.")]
    Ambiguous,
    #[error("YouTube konnte das Video nicht erfolgreich verarbeiten.")]
    ProcessingFailed,
    #[error("Die erforderliche VOD-Audiospur fehlt. Der Live-Mix wird nicht eingesetzt.")]
    MissingVodAudio,
    #[error(
        "Das Medienprofil hat sich in der Aufnahme geändert. Der Export benötigt eine gesonderte Verarbeitung."
    )]
    ChangedProfile,
    #[error("Die Aufnahme enthält Lücken oder ist unvollständig.")]
    Incomplete,
    #[error("Die Quelle ist noch nicht vollständig verfügbar.")]
    SourcePending,
    #[error("Das Twitch-VOD ist nicht eindeutig diesem Stream zugeordnet.")]
    UnboundTwitch,
    #[error("Das eigene Twitch-VOD ist über den freigegebenen Downloadweg nicht zugänglich.")]
    SourceUnavailable,
    #[error("Das VOD-Speicherbudget ist erschöpft.")]
    StorageFull,
    #[error("VOD-Speicher ist nicht verfügbar oder das Objekt ist beschädigt.")]
    Storage,
    #[error("Die dauerhafte VOD-Verwaltung ist nicht verfügbar.")]
    Database,
    #[error("Die Zuständigkeit des Workers ist abgelaufen oder nicht mehr bestätigt.")]
    LeaseLost,
    #[error("Die VOD-Verarbeitung wurde abgebrochen.")]
    Cancelled,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    InputRecording,
    TwitchVod,
}
impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InputRecording => "input_recording",
            Self::TwitchVod => "twitch_vod",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    ExplicitStop,
    ReconnectExpired,
    ControlledStop,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalSessionEnded {
    pub session_id: i64,
    pub streamer_id: u64,
    pub ended_at: DateTime<Utc>,
    pub reason: EndReason,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    #[default]
    Private,
    Unlisted,
    Public,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub source: Source,
    /// Vom Broker bestätigte Kanal-ID. Nie gegen einen beliebigen Requestwert ersetzen.
    pub youtube_channel_id: String,
    pub vod_audio_track: Option<u8>,
    pub privacy: Privacy,
    /// Nur ein bewusster Nutzerentscheid darf nicht-private Veröffentlichungen erlauben.
    pub publication_authorized: bool,
    pub title: String,
    pub description: String,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.youtube_channel_id.is_empty()
            || self.youtube_channel_id.len() > 128
            || self.title.trim().is_empty()
            || self.title.chars().count() > 100
            || self
                .title
                .chars()
                .any(|c| c.is_control() || c == '<' || c == '>')
            || self.description.len() > 5000
            || (self.privacy != Privacy::Private && !self.publication_authorized)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchBinding {
    pub broadcaster_id: String,
    pub stream_id: String,
    pub vod_audio_confirmed: bool,
}
impl TwitchBinding {
    pub fn validate(&self) -> Result<()> {
        self.validate_identity()?;
        if !self.vod_audio_confirmed {
            return Err(Error::MissingVodAudio);
        }
        Ok(())
    }
    pub(crate) fn validate_identity(&self) -> Result<()> {
        for value in [&self.broadcaster_id, &self.stream_id] {
            if value.is_empty() || value.len() > 32 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::UnboundTwitch);
            }
        }
        Ok(())
    }
}
