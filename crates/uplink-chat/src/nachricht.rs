//! Das Drahtformat einer Chatnachricht.
//!
//! Feldnamen und JSON-Form sind mit `tb-platform-core::ChatMessage` aus dem
//! Twitch-Bot identisch (Leitentscheidung 1 des Plans): ein Dock-Bundle, das
//! den Bot-Bus liest, liest auch diesen. Der Bot-Workspace ist bewusst keine
//! Abhaengigkeit; das Format ist kopiert und per Test festgenagelt.
//!
//! Neu gegenueber dem Bot ist nur `eigene`: das Dock markiert damit, was der
//! Streamer selbst aus dem Dock geschickt hat.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Platform;

/// Art-Kennung der Chatnachricht im Dedupe-Schluessel.
pub const CHAT_DEDUPE_ART: &str = "chat";

/// Trennzeichen im Dedupe-Schluessel, wie in tb-platform-core `dedupe.rs`.
const TRENNER: char = ':';

// `Platform` gehoert dem Video-Pfad (transcode/profile.rs) und bleibt dort
// unangetastet. Die serde-Darstellung als Kleinbuchstaben lebt deshalb hier.
impl Serialize for Platform {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let roh = String::deserialize(deserializer)?;
        roh.parse().map_err(serde::de::Error::custom)
    }
}

/// Baut einen deterministischen Dedupe-Schluessel.
///
/// Aufbau `<plattform>:<kanal>:<art>:<kennzeichen>`, Trenner und Backslash in
/// den Teilen maskiert, damit `a:b`+`c` nicht dasselbe ergibt wie `a`+`b:c`.
/// Wortgleich mit tb-platform-core `dedupe_key`.
pub fn dedupe_key(platform: Platform, channel_id: &str, art: &str, kennzeichen: &str) -> String {
    let mut key = String::with_capacity(
        platform.as_str().len() + channel_id.len() + art.len() + kennzeichen.len() + 3,
    );
    key.push_str(platform.as_str());
    key.push(TRENNER);
    maskiert_anhaengen(&mut key, channel_id);
    key.push(TRENNER);
    maskiert_anhaengen(&mut key, art);
    key.push(TRENNER);
    maskiert_anhaengen(&mut key, kennzeichen);
    key
}

fn maskiert_anhaengen(ziel: &mut String, teil: &str) {
    for zeichen in teil.chars() {
        match zeichen {
            '\\' => ziel.push_str("\\\\"),
            TRENNER => ziel.push_str("\\:"),
            sonst => ziel.push(sonst),
        }
    }
}

/// Ein Abzeichen des Absenders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Badge {
    pub set_id: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// Ein Baustein der Nachricht. Tag `art`, weil `typ` dem Rahmen gehoert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "art")]
pub enum Fragment {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "emote")]
    Emote {
        text: String,
        emote_id: String,
        url_template: String,
    },
    #[serde(rename = "mention")]
    Mention {
        text: String,
        user_id: String,
        user_login: String,
    },
    #[serde(rename = "cheermote")]
    Cheermote {
        text: String,
        prefix: String,
        bits: u64,
        tier: u32,
        url_template: String,
    },
}

impl Fragment {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    // Teil des kopierten Drahtformats; das Dock setzt den Text im Browser
    // zusammen, im Dienst braucht es die Funktion bisher nur der Test.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text { text }
            | Self::Emote { text, .. }
            | Self::Mention { text, .. }
            | Self::Cheermote { text, .. } => text,
        }
    }
}

/// Verweis auf die beantwortete Nachricht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRef {
    pub message_id: String,
    pub sender_id: String,
    pub sender_login: String,
    pub sender_display: String,
    pub text: String,
}

/// Eine Chatnachricht, plattformneutral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatNachricht {
    pub platform: Platform,
    pub channel_id: String,
    pub channel_login: String,
    pub message_id: String,
    pub sender_id: String,
    pub sender_login: String,
    pub sender_display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub badges: Vec<Badge>,
    pub fragments: Vec<Fragment>,
    pub sent_at: DateTime<Utc>,
    #[serde(default)]
    pub is_action: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyRef>,
    /// Vom Streamer selbst aus dem Dock geschickt. Nicht im Bot-Format,
    /// deshalb mit Vorgabe, damit Bot-Nutzlasten weiter lesbar bleiben.
    #[serde(default)]
    pub eigene: bool,
    /// Warum diese Nachricht im Dock auffallen soll (Erstchatter, Raider).
    /// Setzt der [`crate::hervorhebung::Hervorheber`] beim Weiterleiten;
    /// aeltere Docks lesen das Feld nicht und zeigen die Zeile normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hervorhebung: Option<crate::hervorhebung::Hervorhebung>,
}

impl ChatNachricht {
    /// Stabiler Dedupe-Schluessel: `<plattform>:<kanal>:chat:<message_id>`.
    pub fn dedupe_key(&self) -> String {
        dedupe_key(
            self.platform,
            &self.channel_id,
            CHAT_DEDUPE_ART,
            &self.message_id,
        )
    }

    /// Nachrichtentext ohne Auszeichnung.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn plain_text(&self) -> String {
        self.fragments.iter().map(Fragment::as_text).collect()
    }
}

/// Steuerereignisse, die bereits angezeigte Chatzeilen veraendern. Sie sind
/// bewusst eigene Drahtobjekte statt erfundene Chatnachrichten: ein Delete
/// hat keinen Nachrichtentext, ein Clear keine einzelne Nachrichten-ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "art")]
pub enum ChatSteuerung {
    #[serde(rename = "message_delete")]
    NachrichtLoeschen {
        platform: Platform,
        channel_id: String,
        message_id: String,
        occurred_at: DateTime<Utc>,
        dedupe_key: String,
    },
    #[serde(rename = "clear_user")]
    NutzerLeeren {
        platform: Platform,
        channel_id: String,
        target_user_id: String,
        occurred_at: DateTime<Utc>,
        dedupe_key: String,
    },
    #[serde(rename = "clear")]
    ChatLeeren {
        platform: Platform,
        channel_id: String,
        occurred_at: DateTime<Utc>,
        dedupe_key: String,
    },
}

impl ChatSteuerung {
    pub fn dedupe_key(&self) -> &str {
        match self {
            Self::NachrichtLoeschen { dedupe_key, .. }
            | Self::NutzerLeeren { dedupe_key, .. }
            | Self::ChatLeeren { dedupe_key, .. } => dedupe_key,
        }
    }
}

/// Was ueber den Bus und den WebSocket geht. Tag `typ`, wie beim Bot
/// (`chat`, `activity`, `info`); `points` und `chat_control` sind Uplink-
/// Erweiterungen. `chat_control` gehoert fachlich zum Chatfilter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "typ")]
pub enum Ereignis {
    #[serde(rename = "chat")]
    Chat(ChatNachricht),
    #[serde(rename = "chat_control")]
    ChatSteuerung(ChatSteuerung),
    #[serde(rename = "activity")]
    Activity(crate::ereignis::ActivityEvent),
    #[serde(rename = "points")]
    Punkte(crate::ereignis::PunkteEreignis),
    #[serde(rename = "info")]
    Info(crate::ereignis::StreamInfo),
}

impl Ereignis {
    pub fn dedupe_key(&self) -> String {
        match self {
            Self::Chat(n) => n.dedupe_key(),
            Self::ChatSteuerung(s) => s.dedupe_key().to_owned(),
            Self::Activity(a) => a.meta().dedupe_key.clone(),
            Self::Punkte(p) => p.meta().dedupe_key.clone(),
            Self::Info(i) => i.dedupe_key(),
        }
    }
}

/// Ein Rahmen auf dem Draht: laufende Nummer je Streamer plus Ereignis.
///
/// Die Nummer ist der Anker fuer `?seit=`: ein Dock, das neu verbindet, nennt
/// die letzte Nummer, die es gesehen hat, und bekommt nur, was danach kam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rahmen {
    pub id: u64,
    pub ereignis: Ereignis,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn zeitpunkt() -> DateTime<Utc> {
        "2026-08-23T20:15:00Z".parse().unwrap()
    }

    /// Die volle Nachricht aus tb-platform-core/tests/drahtformat.rs, so wie
    /// der Bot sie serialisiert. Dieselbe Nutzlast muss hier unveraendert
    /// lesbar sein und identisch wieder herauskommen.
    fn bot_nutzlast() -> serde_json::Value {
        json!({
            "typ": "chat",
            "platform": "kick",
            "channel_id": "kanal-9",
            "channel_login": "earlysalty",
            "message_id": "msg-2",
            "sender_id": "778",
            "sender_login": "stammgast",
            "sender_display": "Stammgast",
            "color": "#D4AF37",
            "badges": [
                { "set_id": "moderator", "id": "1" },
                { "set_id": "subscriber", "id": "12", "info": "14",
                  "image_url": "https://example.invalid/badge.png" }
            ],
            "fragments": [
                { "art": "text", "text": "moin " },
                { "art": "emote", "text": "Kappa", "emote_id": "25",
                  "url_template": "https://example.invalid/emote/{{format}}" },
                { "art": "mention", "text": "@earlysalty", "user_id": "12345",
                  "user_login": "earlysalty" },
                { "art": "cheermote", "text": "Cheer100", "prefix": "Cheer",
                  "bits": 100, "tier": 1,
                  "url_template": "https://example.invalid/cheer/{{scale}}" }
            ],
            "sent_at": "2026-08-23T20:15:00Z",
            "is_action": true,
            "reply_to": {
                "message_id": "msg-0", "sender_id": "12345",
                "sender_login": "earlysalty", "sender_display": "EarlySalty",
                "text": "und?"
            }
        })
    }

    #[test]
    fn drahtformat_entspricht_tb_platform_core() {
        let ereignis: Ereignis =
            serde_json::from_value(bot_nutzlast()).expect("Bot-Nutzlast lesbar");
        let Ereignis::Chat(nachricht) = &ereignis else {
            panic!("chat erwartet")
        };
        assert_eq!(nachricht.platform, Platform::Kick);
        assert_eq!(nachricht.sent_at, zeitpunkt());
        assert_eq!(nachricht.fragments.len(), 4);
        assert!(nachricht.is_action);
        assert!(
            !nachricht.eigene,
            "Bot-Nutzlast kennt das Feld nicht: Vorgabe false"
        );
        assert_eq!(nachricht.plain_text(), "moin Kappa@earlysaltyCheer100");

        // Rueckweg: bis auf das neue Feld `eigene` dieselbe Nutzlast.
        let mut zurueck = serde_json::to_value(&ereignis).expect("serialisierbar");
        assert_eq!(zurueck["eigene"], json!(false));
        zurueck.as_object_mut().unwrap().remove("eigene");
        assert_eq!(zurueck, bot_nutzlast());
    }

    #[test]
    fn dedupe_key_bildung() {
        let Ereignis::Chat(mut nachricht) = serde_json::from_value(bot_nutzlast()).unwrap() else {
            panic!("chat erwartet")
        };
        nachricht.platform = Platform::Twitch;
        nachricht.channel_id = "12345".into();
        nachricht.message_id = "msg-1".into();
        assert_eq!(nachricht.dedupe_key(), "twitch:12345:chat:msg-1");
        // Trenner im Kennzeichen wird maskiert, wie beim Bot.
        assert_eq!(
            dedupe_key(Platform::Twitch, "a:b", "chat", "c"),
            "twitch:a\\:b:chat:c"
        );
        assert_ne!(
            dedupe_key(Platform::Twitch, "a:b", "chat", "c"),
            dedupe_key(Platform::Twitch, "a", "b:chat", "c")
        );
    }

    #[test]
    fn rahmen_traegt_id_und_typ() {
        let Ereignis::Chat(nachricht) = serde_json::from_value(bot_nutzlast()).unwrap() else {
            panic!("chat erwartet")
        };
        let rahmen = Rahmen {
            id: 7,
            ereignis: Ereignis::Chat(nachricht),
        };
        let json = serde_json::to_value(&rahmen).unwrap();
        assert_eq!(json["id"], json!(7));
        assert_eq!(json["ereignis"]["typ"], json!("chat"));
        assert_eq!(json["ereignis"]["platform"], json!("kick"));
    }
}
