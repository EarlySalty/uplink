//! Uebersetzt EventSub-Nutzlasten neben dem Chat in [`Ereignis`].
//!
//! Muster aus `obs_dock.rs` im Bot: Kennzeichen fuer den Dedupe-Schluessel
//! ist die EventSub-Message-Id (Twitch behaelt sie bei einer
//! Wiederzustellung), beim Raid die Quell-Id, beim Community-Geschenk die
//! `community_gift_id`, damit Sammel- und Einzelmeldung zusammenfallen.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::Platform;
use crate::ereignis::{ActivityEvent, ActivityMeta, Actor, PunkteEreignis, StreamInfo};
use crate::nachricht::Ereignis;

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

fn u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        let roh = value.get(*key);
        let gelesen = roh.and_then(Value::as_u64).or_else(|| {
            roh.and_then(Value::as_str)
                .and_then(|t| t.trim().parse::<u64>().ok())
        });
        if gelesen.is_some() {
            return gelesen;
        }
    }
    None
}

fn u32_field(value: &Value, keys: &[&str]) -> Option<u32> {
    u64_field(value, keys).and_then(|z| u32::try_from(z).ok())
}

/// Kennzeichen: Message-Id, sonst der Zeitpunkt (entdoppelt dann nicht,
/// erzeugt aber auch keine falsche Gleichheit).
fn kennzeichen(message_id: Option<&str>, jetzt: DateTime<Utc>) -> String {
    message_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| jetzt.to_rfc3339())
}

/// Der Nutzer aus `user_id`, `user_login`, `user_name`. Anonyme Cheers und
/// Geschenke haben keinen.
fn user_actor(event: &Value) -> Option<Actor> {
    let id = str_field(event, &["user_id"])?;
    let login = str_field(event, &["user_login"]).unwrap_or_else(|| id.clone());
    let display = str_field(event, &["user_name"]).unwrap_or_else(|| login.clone());
    Some(Actor::new(id, login, display))
}

fn meta(
    event: &Value,
    art: &str,
    kennzeichen: &str,
    jetzt: DateTime<Utc>,
    actor: Option<Actor>,
) -> Option<ActivityMeta> {
    let channel_id = str_field(event, &["broadcaster_user_id"])?;
    let mut meta = ActivityMeta::derived(Platform::Twitch, channel_id, jetzt, art, kennzeichen);
    if let Some(actor) = actor {
        meta = meta.with_actor(actor);
    }
    Some(meta)
}

/// Stufe laut Twitch (`tier`), Vorgabe `1000`.
fn tier_of(event: &Value) -> String {
    str_field(event, &["tier", "sub_tier"]).unwrap_or_else(|| "1000".to_string())
}

fn nachricht_text(event: &Value) -> Option<String> {
    let text = match event.get("message") {
        Some(Value::String(text)) => Some(text.as_str()),
        Some(Value::Object(_)) => event.pointer("/message/text").and_then(Value::as_str),
        _ => None,
    };
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Uebersetzt eine Notification nach ihrem Subscription-Typ. `None` fuer
/// Typen, die das Dock nicht kennt, oder Nutzlasten ohne Kanal.
pub fn uebersetzen(
    typ: &str,
    event: &Value,
    message_id: Option<&str>,
    jetzt: DateTime<Utc>,
) -> Option<Ereignis> {
    match typ {
        "channel.follow" => follow(event, message_id, jetzt),
        "channel.subscribe" => subscribe(event, message_id, jetzt),
        "channel.subscription.message" => resub(event, message_id, jetzt),
        "channel.subscription.gift" => sub_gift(event, message_id, jetzt),
        "channel.cheer" => cheer(event, message_id, jetzt),
        "channel.raid" => raid(event, message_id, jetzt),
        "channel.update" => channel_update(event),
        "channel.channel_points_custom_reward_redemption.add"
        | "channel.channel_points_custom_reward_redemption.update" => redemption(event, jetzt),
        _ => None,
    }
}

fn follow(event: &Value, message_id: Option<&str>, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let zeit = str_field(event, &["followed_at"])
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .unwrap_or(jetzt);
    let meta = meta(
        event,
        "follow",
        &kennzeichen(message_id, jetzt),
        zeit,
        user_actor(event),
    )?;
    Some(Ereignis::Activity(ActivityEvent::Follow { meta }))
}

fn subscribe(event: &Value, message_id: Option<&str>, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let meta = meta(
        event,
        "subscribe",
        &kennzeichen(message_id, jetzt),
        jetzt,
        user_actor(event),
    )?;
    Some(Ereignis::Activity(ActivityEvent::Subscribe {
        meta,
        tier: tier_of(event),
        is_gift: event
            .get("is_gift")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }))
}

fn resub(event: &Value, message_id: Option<&str>, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let meta = meta(
        event,
        "resub",
        &kennzeichen(message_id, jetzt),
        jetzt,
        user_actor(event),
    )?;
    Some(Ereignis::Activity(ActivityEvent::Resub {
        meta,
        months: u32_field(event, &["cumulative_months", "duration_months"]).unwrap_or(1),
        streak: u32_field(event, &["streak_months"]),
        message: nachricht_text(event),
        // Ohne Angabe kein Rateschluss: der Satz im Dock laesst die Stufe
        // dann weg, statt eine falsche zu nennen.
        tier: str_field(event, &["tier", "sub_tier"]),
    }))
}

/// Kennzeichen eines Geschenks: `community_gift_id`, sonst `id`, sonst die
/// Message-Id. Sammel- und Einzelmeldung eines Community-Geschenks laufen so
/// im Bus zusammen, die erste gewinnt.
fn gift_kennzeichen(event: &Value, ruecktritt: &str) -> String {
    str_field(event, &["community_gift_id", "id"]).unwrap_or_else(|| ruecktritt.to_string())
}

fn sub_gift(event: &Value, message_id: Option<&str>, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let anonym = event
        .get("is_anonymous")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let actor = if anonym { None } else { user_actor(event) };
    let meta = meta(
        event,
        "sub_gift",
        &gift_kennzeichen(event, &kennzeichen(message_id, jetzt)),
        jetzt,
        actor,
    )?;
    Some(Ereignis::Activity(ActivityEvent::SubGift {
        meta,
        count: u32_field(event, &["total", "count"]).unwrap_or(1),
        tier: tier_of(event),
    }))
}

fn cheer(event: &Value, message_id: Option<&str>, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let anonym = event
        .get("is_anonymous")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let actor = if anonym { None } else { user_actor(event) };
    let meta = meta(
        event,
        "cheer",
        &kennzeichen(message_id, jetzt),
        jetzt,
        actor,
    )?;
    Some(Ereignis::Activity(ActivityEvent::Cheer {
        meta,
        bits: u64_field(event, &["bits"]).unwrap_or(0),
        message: nachricht_text(event),
    }))
}

/// Eingehender Raid: die Zeile gehoert dem Zielkanal.
///
/// Kennzeichen ist die `message_id` des Rahmens, wie bei Follow und Cheer.
/// Frueher stand hier die Quell-Id: derselbe Kanal, der in einem Stream
/// zweimal raidet, wurde damit beim zweiten Mal als Dublette verworfen und
/// tauchte im Aktivitaets-Dock nie auf.
fn raid(event: &Value, message_id: Option<&str>, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let channel_id = str_field(event, &["to_broadcaster_user_id"])?;
    let from_id = str_field(event, &["from_broadcaster_user_id"])?;
    let from_login =
        str_field(event, &["from_broadcaster_user_login"]).unwrap_or_else(|| from_id.clone());
    let from_display =
        str_field(event, &["from_broadcaster_user_name"]).unwrap_or_else(|| from_login.clone());
    let meta = ActivityMeta::derived(
        Platform::Twitch,
        channel_id,
        jetzt,
        "raid",
        &kennzeichen(message_id, jetzt),
    )
    .with_actor(Actor::new(from_id, from_login.clone(), from_display));
    Some(Ereignis::Activity(ActivityEvent::Raid {
        meta,
        from: from_login,
        viewers: u32_field(event, &["viewers", "viewer_count"]).unwrap_or(0),
    }))
}

/// `channel.update` wird zur Momentaufnahme fuer das Stream-Info-Dock.
/// Tags liefert das Ereignis nicht; sie bleiben leer und das Dock behaelt
/// seine.
fn channel_update(event: &Value) -> Option<Ereignis> {
    let channel_id = str_field(event, &["broadcaster_user_id"])?;
    Some(Ereignis::Info(StreamInfo {
        platform: Platform::Twitch,
        channel_id,
        title: str_field(event, &["title"]).unwrap_or_default(),
        category_id: str_field(event, &["category_id"]),
        category_name: str_field(event, &["category_name"]),
        // `channel.update` liefert kein Bild. Das Fenster behaelt seines,
        // solange die Kategorie dieselbe bleibt.
        category_bild: None,
        // Beides weiss `channel.update` nicht. `None` sagt genau das.
        tags: None,
        is_live: None,
        started_at: None,
        viewers: None,
    }))
}

fn redemption(event: &Value, jetzt: DateTime<Utc>) -> Option<Ereignis> {
    let redemption_id = str_field(event, &["id"])?;
    let reward = event.get("reward")?;
    let zeit = str_field(event, &["redeemed_at"])
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .unwrap_or(jetzt);
    let meta = meta(event, "redemption", &redemption_id, zeit, user_actor(event))?;
    Some(Ereignis::Punkte(PunkteEreignis::Redemption {
        meta,
        redemption_id,
        reward_id: str_field(reward, &["id"]).unwrap_or_default(),
        reward_title: str_field(reward, &["title"]).unwrap_or_default(),
        cost: u64_field(reward, &["cost"]).unwrap_or(0),
        user_input: str_field(event, &["user_input"]),
        status: str_field(event, &["status"]).unwrap_or_else(|| "unfulfilled".into()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn zeitpunkt() -> DateTime<Utc> {
        "2026-08-23T20:15:00Z".parse().unwrap()
    }

    fn basis() -> Value {
        json!({
            "broadcaster_user_id": "12345",
            "broadcaster_user_login": "earlysalty",
            "broadcaster_user_name": "EarlySalty",
            "user_id": "777",
            "user_login": "zuschauer",
            "user_name": "Zuschauer"
        })
    }

    fn mit(zusatz: Value) -> Value {
        let mut e = basis();
        for (k, v) in zusatz.as_object().unwrap() {
            e[k] = v.clone();
        }
        e
    }

    fn activity(ereignis: Ereignis) -> ActivityEvent {
        let Ereignis::Activity(a) = ereignis else {
            panic!("activity erwartet, kam {ereignis:?}")
        };
        a
    }

    #[test]
    fn follow_wird_uebersetzt() {
        let e = mit(json!({ "followed_at": "2026-08-23T20:14:00Z" }));
        let a = activity(uebersetzen("channel.follow", &e, Some("m-1"), zeitpunkt()).unwrap());
        assert_eq!(a.art(), "follow");
        assert_eq!(a.meta().dedupe_key, "twitch:12345:follow:m-1");
        assert_eq!(a.meta().actor.as_ref().unwrap().display, "Zuschauer");
        assert_eq!(
            a.meta().occurred_at,
            "2026-08-23T20:14:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        let json = serde_json::to_value(Ereignis::Activity(a)).unwrap();
        assert_eq!(json["typ"], "activity");
        assert_eq!(json["art"], "follow");
        assert_eq!(json["platform"], "twitch");
        assert!(uebersetzen("channel.unbekannt", &e, None, zeitpunkt()).is_none());
    }

    #[test]
    fn subscribe_und_resub_werden_uebersetzt() {
        let e = mit(json!({ "tier": "2000", "is_gift": true }));
        let a = activity(uebersetzen("channel.subscribe", &e, Some("m-2"), zeitpunkt()).unwrap());
        assert!(
            matches!(&a, ActivityEvent::Subscribe { tier, is_gift, .. } if tier == "2000" && *is_gift)
        );
        assert_eq!(a.meta().dedupe_key, "twitch:12345:subscribe:m-2");

        let e = mit(json!({
            "tier": "1000",
            "message": { "text": "sechs Monate, moin", "emotes": [] },
            "cumulative_months": 6,
            "streak_months": null,
            "duration_months": 1
        }));
        let a = activity(
            uebersetzen("channel.subscription.message", &e, Some("m-3"), zeitpunkt()).unwrap(),
        );
        let ActivityEvent::Resub {
            months,
            streak,
            message,
            tier,
            ..
        } = &a
        else {
            panic!("resub")
        };
        assert_eq!(*months, 6);
        assert_eq!(*streak, None);
        assert_eq!(message.as_deref(), Some("sechs Monate, moin"));
        // Ohne die Stufe kann das Fenster den Satz "auf Stufe 1 abonniert"
        // nicht schreiben.
        assert_eq!(tier.as_deref(), Some("1000"));
        assert_eq!(a.meta().dedupe_key, "twitch:12345:resub:m-3");

        // Nennt Twitch keine Stufe, wird auch keine erfunden, und die
        // Nutzlast bleibt feldgleich mit der des Bots.
        let ohne_stufe = mit(json!({ "cumulative_months": 3 }));
        let a = activity(
            uebersetzen(
                "channel.subscription.message",
                &ohne_stufe,
                Some("m-3b"),
                zeitpunkt(),
            )
            .unwrap(),
        );
        assert!(matches!(&a, ActivityEvent::Resub { tier: None, .. }));
        let json = serde_json::to_value(Ereignis::Activity(a)).unwrap();
        assert!(json.get("tier").is_none(), "kein leeres Feld auf dem Draht");
    }

    #[test]
    fn community_gift_bekommt_gift_kennzeichen() {
        let e = mit(json!({ "total": 5, "tier": "1000", "is_anonymous": false,
                            "community_gift_id": "cg-9", "cumulative_total": 20 }));
        let a = activity(
            uebersetzen("channel.subscription.gift", &e, Some("m-4"), zeitpunkt()).unwrap(),
        );
        assert!(matches!(&a, ActivityEvent::SubGift { count: 5, tier, .. } if tier == "1000"));
        assert_eq!(a.meta().dedupe_key, "twitch:12345:sub_gift:cg-9");
        assert_eq!(a.meta().actor.as_ref().unwrap().login, "zuschauer");

        // Anonym: kein Ausloeser, Kennzeichen faellt auf die Message-Id.
        let e = mit(json!({ "total": 1, "tier": "1000", "is_anonymous": true,
                            "user_id": null, "user_login": null, "user_name": null }));
        let a = activity(
            uebersetzen("channel.subscription.gift", &e, Some("m-5"), zeitpunkt()).unwrap(),
        );
        assert!(a.meta().actor.is_none());
        assert_eq!(a.meta().dedupe_key, "twitch:12345:sub_gift:m-5");
    }

    #[test]
    fn cheer_wird_uebersetzt() {
        let e = mit(json!({ "is_anonymous": false, "message": "Cheer100 stark", "bits": 100 }));
        let a = activity(uebersetzen("channel.cheer", &e, Some("m-6"), zeitpunkt()).unwrap());
        let ActivityEvent::Cheer { bits, message, .. } = &a else {
            panic!("cheer")
        };
        assert_eq!(*bits, 100);
        assert_eq!(message.as_deref(), Some("Cheer100 stark"));
        assert_eq!(a.meta().dedupe_key, "twitch:12345:cheer:m-6");
        let json = serde_json::to_value(Ereignis::Activity(a)).unwrap();
        assert_eq!(json["actor"]["display"], "Zuschauer");
    }

    #[test]
    fn channel_raid_wird_uebersetzt() {
        let e = json!({
            "from_broadcaster_user_id": "999",
            "from_broadcaster_user_login": "raider",
            "from_broadcaster_user_name": "Raider",
            "to_broadcaster_user_id": "12345",
            "to_broadcaster_user_login": "earlysalty",
            "viewers": 42
        });
        let a = activity(uebersetzen("channel.raid", &e, Some("m-9"), zeitpunkt()).unwrap());
        assert_eq!(a.meta().channel_id, "12345", "Zeile gehoert dem Zielkanal");
        assert_eq!(
            a.meta().dedupe_key,
            "twitch:12345:raid:m-9",
            "Message-Id, nicht Quell-Id"
        );
        // Derselbe Kanal raidet zweimal im selben Stream. Mit der Quell-Id im
        // Schluessel verwarf der Bus den zweiten Raid als Dublette, und er
        // tauchte im Aktivitaets-Dock nie auf.
        let zweiter = activity(uebersetzen("channel.raid", &e, Some("m-10"), zeitpunkt()).unwrap());
        assert_ne!(
            a.meta().dedupe_key,
            zweiter.meta().dedupe_key,
            "zwei Raids desselben Kanals sind zwei Ereignisse"
        );
        let json = serde_json::to_value(Ereignis::Activity(a)).unwrap();
        assert_eq!(json["typ"], "activity");
        assert_eq!(json["art"], "raid");
        assert_eq!(json["from"], "raider");
        assert_eq!(json["viewers"], 42);
        assert_eq!(json["actor"]["login"], "raider");
    }

    #[test]
    fn channel_update_wird_zur_info() {
        let e = json!({
            "broadcaster_user_id": "12345",
            "broadcaster_user_login": "earlysalty",
            "title": "Neuer Titel",
            "language": "de",
            "category_id": "1234",
            "category_name": "Deadlock",
            "content_classification_labels": []
        });
        let Some(Ereignis::Info(info)) =
            uebersetzen("channel.update", &e, Some("m-7"), zeitpunkt())
        else {
            panic!("info erwartet")
        };
        assert_eq!(info.platform, Platform::Twitch);
        assert_eq!(info.title, "Neuer Titel");
        assert_eq!(info.category_id.as_deref(), Some("1234"));
        assert_eq!(info.category_name.as_deref(), Some("Deadlock"));
        assert_eq!(
            info.tags, None,
            "`channel.update` liefert keine Tags; leer waere die falsche Aussage"
        );
        assert_eq!(
            info.is_live, None,
            "diese Quelle weiss nicht, ob der Kanal sendet"
        );
        // Und das Drahtformat sagt es auch nicht. Vorher stand dort fest
        // `is_live: false` und `tags: []`: ein zweiter Verbraucher desselben
        // Rahmens haette den Kanal mitten im Stream als offline gezeigt und
        // die Tags als geloescht gelesen.
        let drahtformat = serde_json::to_value(Ereignis::Info(info.clone())).expect("Drahtformat");
        assert!(
            drahtformat.get("is_live").is_none(),
            "das Drahtformat behauptet einen Sendezustand, den niemand geprueft hat: {drahtformat}"
        );
        assert!(
            drahtformat.get("tags").is_none(),
            "das Drahtformat behauptet leere Tags, die niemand gelesen hat: {drahtformat}"
        );
        // Zweimal derselbe Stand ist einer, das haelt weiterhin.
        assert_eq!(info.dedupe_key(), info.clone().dedupe_key());
        // Derselbe Stand zweimal: ein Schluessel, der Bus schluckt die Dublette.
        let Some(Ereignis::Info(nochmal)) =
            uebersetzen("channel.update", &e, Some("m-8"), zeitpunkt())
        else {
            panic!()
        };
        assert_eq!(info.dedupe_key(), nochmal.dedupe_key());
    }

    #[test]
    fn redemption_add_wird_uebersetzt() {
        let e = mit(json!({
            "id": "r-1",
            "user_input": "bitte Hydrate",
            "status": "unfulfilled",
            "reward": { "id": "rw-1", "title": "Hydrate", "cost": 250, "prompt": "trink was" },
            "redeemed_at": "2026-08-23T20:10:00Z"
        }));
        let Some(Ereignis::Punkte(PunkteEreignis::Redemption {
            meta,
            redemption_id,
            reward_id,
            reward_title,
            cost,
            user_input,
            status,
        })) = uebersetzen(
            "channel.channel_points_custom_reward_redemption.add",
            &e,
            Some("m-10"),
            zeitpunkt(),
        )
        else {
            panic!("points erwartet")
        };
        assert_eq!(redemption_id, "r-1");
        assert_eq!(reward_id, "rw-1");
        assert_eq!(reward_title, "Hydrate");
        assert_eq!(cost, 250);
        assert_eq!(user_input.as_deref(), Some("bitte Hydrate"));
        assert_eq!(status, "unfulfilled");
        assert_eq!(meta.dedupe_key, "twitch:12345:redemption:r-1");
        assert_eq!(meta.actor.unwrap().login, "zuschauer");
        assert_eq!(
            meta.occurred_at,
            "2026-08-23T20:10:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }
}
