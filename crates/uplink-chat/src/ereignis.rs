//! Ereignisse neben dem Chat: Aktivitaeten, Kanalpunkte, Stream-Infos.
//!
//! Feldnamen und JSON-Form sind mit `tb-platform-core` aus dem Twitch-Bot
//! identisch (`activity.rs`, `stream_info.rs`): aeusserer Tag `typ` im
//! [`Ereignis`], innerer Tag `art` je Variante. Der Bot-Workspace bleibt
//! keine Abhaengigkeit; das Format ist kopiert und per Test festgenagelt.
//!
//! Neu gegenueber dem Bot ist nur [`PunkteEreignis`] (Tag `points`), das der
//! Bot nicht kennt.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Platform;
use crate::nachricht::dedupe_key;

/// Ausloeser eines Ereignisses, also der Follower, Abonnent oder Raider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub id: String,
    pub login: String,
    pub display: String,
}

impl Actor {
    pub fn new(
        id: impl Into<String>,
        login: impl Into<String>,
        display: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            login: login.into(),
            display: display.into(),
        }
    }
}

/// Grundangaben, die jede Ereignisvariante traegt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityMeta {
    pub platform: Platform,
    pub channel_id: String,
    pub occurred_at: DateTime<Utc>,
    /// Stabiler Schluessel gegen Dubletten im Nachlauf.
    pub dedupe_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
}

impl ActivityMeta {
    /// Baut die Grundangaben und leitet den Dedupe-Schluessel ab:
    /// `<plattform>:<kanal>:<art>:<kennzeichen>`, wie beim Bot.
    pub fn derived(
        platform: Platform,
        channel_id: impl Into<String>,
        occurred_at: DateTime<Utc>,
        art: &str,
        kennzeichen: &str,
    ) -> Self {
        let channel_id = channel_id.into();
        let key = dedupe_key(platform, &channel_id, art, kennzeichen);
        Self {
            platform,
            channel_id,
            occurred_at,
            dedupe_key: key,
            actor: None,
        }
    }

    #[must_use]
    pub fn with_actor(mut self, actor: Actor) -> Self {
        self.actor = Some(actor);
        self
    }
}

/// Ereignis neben dem Chat. Tag `art`, weil `typ` dem Rahmen gehoert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "art")]
pub enum ActivityEvent {
    #[serde(rename = "follow")]
    Follow {
        #[serde(flatten)]
        meta: ActivityMeta,
    },
    #[serde(rename = "subscribe")]
    Subscribe {
        #[serde(flatten)]
        meta: ActivityMeta,
        /// Stufe laut Plattform, bei Twitch `1000`, `2000`, `3000` oder `Prime`.
        tier: String,
        is_gift: bool,
    },
    #[serde(rename = "resub")]
    Resub {
        #[serde(flatten)]
        meta: ActivityMeta,
        months: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        streak: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Stufe laut Plattform, wie bei `Subscribe`. Der Bot fuehrt das Feld
        /// nicht, deshalb optional: eine Bot-Nutzlast bleibt feldgleich, und
        /// der Satz im Dock nennt die Stufe nur, wenn sie bekannt ist.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<String>,
    },
    #[serde(rename = "sub_gift")]
    SubGift {
        #[serde(flatten)]
        meta: ActivityMeta,
        count: u32,
        tier: String,
    },
    #[serde(rename = "cheer")]
    Cheer {
        #[serde(flatten)]
        meta: ActivityMeta,
        bits: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "raid")]
    Raid {
        #[serde(flatten)]
        meta: ActivityMeta,
        from: String,
        viewers: u32,
    },
    #[serde(rename = "stream_online")]
    StreamOnline {
        #[serde(flatten)]
        meta: ActivityMeta,
    },
    #[serde(rename = "stream_offline")]
    StreamOffline {
        #[serde(flatten)]
        meta: ActivityMeta,
    },
    #[serde(rename = "channel_update")]
    ChannelUpdate {
        #[serde(flatten)]
        meta: ActivityMeta,
        title: String,
        category: String,
    },
}

impl ActivityEvent {
    pub fn meta(&self) -> &ActivityMeta {
        match self {
            Self::Follow { meta }
            | Self::Subscribe { meta, .. }
            | Self::Resub { meta, .. }
            | Self::SubGift { meta, .. }
            | Self::Cheer { meta, .. }
            | Self::Raid { meta, .. }
            | Self::StreamOnline { meta }
            | Self::StreamOffline { meta }
            | Self::ChannelUpdate { meta, .. } => meta,
        }
    }

    /// Stabile Art-Kennung, identisch mit der serde-Darstellung.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn art(&self) -> &'static str {
        match self {
            Self::Follow { .. } => "follow",
            Self::Subscribe { .. } => "subscribe",
            Self::Resub { .. } => "resub",
            Self::SubGift { .. } => "sub_gift",
            Self::Cheer { .. } => "cheer",
            Self::Raid { .. } => "raid",
            Self::StreamOnline { .. } => "stream_online",
            Self::StreamOffline { .. } => "stream_offline",
            Self::ChannelUpdate { .. } => "channel_update",
        }
    }
}

/// Eine Kanalpunkte-Einloesung. Nicht im Bot-Format; eigener Tag `points`
/// im Rahmen, innen `art = redemption` wie bei den Aktivitaeten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "art")]
pub enum PunkteEreignis {
    #[serde(rename = "redemption")]
    Redemption {
        #[serde(flatten)]
        meta: ActivityMeta,
        redemption_id: String,
        reward_id: String,
        reward_title: String,
        cost: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_input: Option<String>,
        /// Stand laut Plattform: `unfulfilled`, `fulfilled`, `canceled`.
        status: String,
    },
}

impl PunkteEreignis {
    pub fn meta(&self) -> &ActivityMeta {
        match self {
            Self::Redemption { meta, .. } => meta,
        }
    }
}

/// Momentaufnahme der Kanalinformationen. Ein Zustand, kein Ereignis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub platform: Platform,
    pub channel_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_name: Option<String>,
    /// Bild der Kategorie mit den Platzhaltern `{width}` und `{height}`, wie
    /// die Plattform es liefert. Der Bot fuehrt das Feld nicht, deshalb
    /// optional; das Dock zeigt die gewaehlte Kategorie damit als Karte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_bild: Option<String>,
    /// `None` heisst "diese Quelle liefert keine Tags", nicht "keine Tags".
    /// `channel.update` ueber EventSub schickt sie nicht mit; ein leeres Feld
    /// haette das Dock als "der Streamer hat alle Tags geloescht" lesen
    /// koennen, und der Dedupe-Schluessel haette dieselbe Momentaufnahme aus
    /// zwei Quellen fuer zwei verschiedene gehalten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// `None` heisst "unbekannt". Keine der beiden Quellen weiss heute, ob
    /// der Kanal live ist; ein festes `false` behauptete einen Zustand, den
    /// niemand geprueft hat, und ein zweiter Verbraucher desselben Rahmens
    /// haette den Kanal mitten im Stream als offline gezeigt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewers: Option<u32>,
}

impl StreamInfo {
    /// Dedupe-Schluessel eines Zustands: Hash der Felder. Zwei gleiche
    /// Momentaufnahmen hintereinander sind eine.
    pub fn dedupe_key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.title.hash(&mut hasher);
        self.category_id.hash(&mut hasher);
        self.category_name.hash(&mut hasher);
        // Nur, was diese Quelle wirklich geliefert hat. Ein Feld, das sie
        // gar nicht kennt, darf den Schluessel nicht mitbestimmen; sonst
        // haenge der Schluessel an einem `false`, das niemand geprueft hat.
        //
        // Ueber Quellgrenzen hinweg entdoppelt das nicht, und das ist
        // Absicht: `channel.update` ohne Tags und der Kanal-Endpunkt mit
        // Tags beschreiben nicht dasselbe, sie wissen verschieden viel.
        //
        // Der Preis dafuer, ausdruecklich: derselbe Titel- und
        // Kategorie-Stand aus beiden Quellen ergibt zwei Rahmen statt einem.
        // Im Dock faellt das nicht auf, `standUebernehmen` ist idempotent
        // und ein Live-Rahmen ueberschreibt die Tags nicht. Wer einen
        // zweiten Verbraucher baut, rechnet damit.
        if let Some(tags) = &self.tags {
            tags.hash(&mut hasher);
        }
        if let Some(is_live) = self.is_live {
            is_live.hash(&mut hasher);
        }
        dedupe_key(
            self.platform,
            &self.channel_id,
            "info",
            &format!("{:016x}", hasher.finish()),
        )
    }
}

/// Aenderungswunsch an den Kanalinformationen. Jedes `None` heisst "nicht
/// anfassen"; der Adapter schickt nur die gesetzten Felder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfoPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl StreamInfoPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.category_id.is_none() && self.tags.is_none()
    }
}

/// Welche Arten ein Dock lesen will: `?arten=chat,activity,points,info`.
/// Ohne Angabe alles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtFilter {
    pub chat: bool,
    pub activity: bool,
    pub points: bool,
    pub info: bool,
}

impl ArtFilter {
    pub const ALLE: ArtFilter = ArtFilter {
        chat: true,
        activity: true,
        points: true,
        info: true,
    };

    /// Liest die Liste. Leer oder nur Unbekanntes heisst: alles.
    pub fn parse(roh: Option<&str>) -> ArtFilter {
        let mut filter = ArtFilter {
            chat: false,
            activity: false,
            points: false,
            info: false,
        };
        let mut eine = false;
        for teil in roh.unwrap_or("").split(',') {
            match teil.trim() {
                "chat" => filter.chat = true,
                "activity" => filter.activity = true,
                "points" => filter.points = true,
                "info" => filter.info = true,
                _ => continue,
            }
            eine = true;
        }
        if eine { filter } else { ArtFilter::ALLE }
    }

    pub fn passt(&self, ereignis: &crate::nachricht::Ereignis) -> bool {
        use crate::nachricht::Ereignis;
        match ereignis {
            Ereignis::Chat(_) => self.chat,
            Ereignis::Activity(_) => self.activity,
            Ereignis::Punkte(_) => self.points,
            Ereignis::Info(_) => self.info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nachricht::Ereignis;
    use serde_json::json;

    fn zeitpunkt() -> DateTime<Utc> {
        "2026-08-23T20:15:00Z".parse().unwrap()
    }

    /// Raid-Nutzlast, wie `channel_raid_zu_event` im Bot sie schreibt
    /// (obs_dock.rs, `eingehender_raid_gehoert_dem_zielkanal`).
    fn bot_raid() -> serde_json::Value {
        json!({
            "typ": "activity",
            "art": "raid",
            "platform": "twitch",
            "channel_id": "12345",
            "occurred_at": "2026-08-23T20:15:00Z",
            "dedupe_key": "twitch:12345:raid:999",
            "actor": { "id": "999", "login": "raider", "display": "Raider" },
            "from": "raider",
            "viewers": 42
        })
    }

    /// Go-Live ohne Actor, Schluessel aus der stream_id
    /// (obs_dock.rs, `go_live_reicht_die_stream_id_unveraendert_weiter`).
    fn bot_stream_online() -> serde_json::Value {
        json!({
            "typ": "activity",
            "art": "stream_online",
            "platform": "twitch",
            "channel_id": "12345",
            "occurred_at": "2026-08-23T20:15:00Z",
            "dedupe_key": "twitch:12345:stream_online:stream-7"
        })
    }

    fn bot_follow() -> serde_json::Value {
        json!({
            "typ": "activity",
            "art": "follow",
            "platform": "kick",
            "channel_id": "kanal-9",
            "occurred_at": "2026-08-23T20:15:00Z",
            "dedupe_key": "kick:kanal-9:follow:m-1",
            "actor": { "id": "5", "login": "neu", "display": "Neu" }
        })
    }

    #[test]
    fn activity_drahtformat_entspricht_tb_platform_core() {
        for (nutzlast, art) in [
            (bot_raid(), "raid"),
            (bot_stream_online(), "stream_online"),
            (bot_follow(), "follow"),
        ] {
            let ereignis: Ereignis =
                serde_json::from_value(nutzlast.clone()).expect("Bot-Nutzlast lesbar");
            let Ereignis::Activity(aktivitaet) = &ereignis else {
                panic!("activity erwartet");
            };
            assert_eq!(aktivitaet.art(), art);
            assert_eq!(aktivitaet.meta().occurred_at, zeitpunkt());
            assert_eq!(ereignis.dedupe_key(), nutzlast["dedupe_key"]);
            let zurueck = serde_json::to_value(&ereignis).expect("serialisierbar");
            assert_eq!(zurueck, nutzlast, "Rueckweg feldgleich");
        }
        let Ereignis::Activity(ActivityEvent::Raid {
            meta,
            from,
            viewers,
        }) = serde_json::from_value(bot_raid()).unwrap()
        else {
            panic!()
        };
        assert_eq!(from, "raider");
        assert_eq!(viewers, 42);
        assert_eq!(meta.actor.unwrap().login, "raider");
        assert_eq!(
            ActivityMeta::derived(Platform::Twitch, "12345", zeitpunkt(), "raid", "999").dedupe_key,
            "twitch:12345:raid:999"
        );
    }

    #[test]
    fn info_drahtformat_entspricht_tb_platform_core() {
        let nutzlast = json!({
            "typ": "info",
            "platform": "twitch",
            "channel_id": "12345",
            "title": "Deadlock Ranked",
            "category_id": "1234",
            "category_name": "Deadlock",
            "tags": ["deutsch", "ranked"],
            "is_live": true,
            "started_at": "2026-08-23T20:15:00Z",
            "viewers": 12
        });
        let ereignis: Ereignis = serde_json::from_value(nutzlast.clone()).unwrap();
        let Ereignis::Info(info) = &ereignis else {
            panic!("info erwartet")
        };
        assert_eq!(info.title, "Deadlock Ranked");
        assert_eq!(info.tags.as_deref().map(<[String]>::len), Some(2));
        assert_eq!(serde_json::to_value(&ereignis).unwrap(), nutzlast);
        // Zustand: gleiche Felder, gleicher Schluessel; anderer Titel, anderer.
        let mut anders = info.clone();
        anders.title = "Neu".into();
        assert_ne!(info.dedupe_key(), anders.dedupe_key());
        assert!(info.dedupe_key().starts_with("twitch:12345:info:"));
        // Patch schickt nur gesetzte Felder.
        let patch = StreamInfoPatch {
            title: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&patch).unwrap(),
            json!({ "title": "x" })
        );
        assert!(StreamInfoPatch::default().is_empty());
    }

    #[test]
    fn punkte_drahtformat_hat_tag_points_und_art_redemption() {
        let ereignis = Ereignis::Punkte(PunkteEreignis::Redemption {
            meta: ActivityMeta::derived(
                Platform::Twitch,
                "12345",
                zeitpunkt(),
                "redemption",
                "r-1",
            )
            .with_actor(Actor::new("7", "fan", "Fan")),
            redemption_id: "r-1".into(),
            reward_id: "rw-1".into(),
            reward_title: "Hydrate".into(),
            cost: 100,
            user_input: None,
            status: "unfulfilled".into(),
        });
        let json = serde_json::to_value(&ereignis).unwrap();
        assert_eq!(json["typ"], "points");
        assert_eq!(json["art"], "redemption");
        assert_eq!(json["dedupe_key"], "twitch:12345:redemption:r-1");
        assert_eq!(json["actor"]["login"], "fan");
        assert!(json.get("user_input").is_none());
        let zurueck: Ereignis = serde_json::from_value(json).unwrap();
        assert_eq!(zurueck, ereignis);
    }

    #[test]
    fn arten_filter_parst_liste_und_default_ist_alles() {
        assert_eq!(ArtFilter::parse(None), ArtFilter::ALLE);
        assert_eq!(ArtFilter::parse(Some("")), ArtFilter::ALLE);
        assert_eq!(ArtFilter::parse(Some("quatsch")), ArtFilter::ALLE);
        let nur_activity = ArtFilter::parse(Some("activity"));
        assert!(nur_activity.activity && !nur_activity.chat && !nur_activity.points);
        let zwei = ArtFilter::parse(Some(" points , info "));
        assert!(zwei.points && zwei.info && !zwei.chat && !zwei.activity);
        let ereignis: Ereignis = serde_json::from_value(bot_follow()).unwrap();
        assert!(nur_activity.passt(&ereignis));
        assert!(!zwei.passt(&ereignis));
        assert!(ArtFilter::ALLE.passt(&ereignis));
    }
}
