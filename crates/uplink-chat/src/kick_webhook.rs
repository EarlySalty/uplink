use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::Platform;
use crate::ereignis::{ActivityEvent, ActivityMeta, Actor};
use crate::nachricht::{Badge, ChatNachricht, Ereignis, Fragment};

pub const KICK_PUBLIC_KEY_URL: &str = "https://api.kick.com/public/v1/public-key";
const FENSTER: Duration = Duration::from_secs(10 * 60);
const KEY_FRIST: Duration = Duration::from_secs(10);
const RELOAD_ENTPRELLUNG: Duration = Duration::from_secs(60);

pub struct KickZiel {
    pub eingang: mpsc::Sender<Ereignis>,
    pub channel_login: String,
    token: u64,
}

struct Gesehen {
    wann: Instant,
}

pub struct KickDrehkreuz {
    ziele: Mutex<HashMap<String, KickZiel>>,
    gesehen: Mutex<HashMap<String, Gesehen>>,
    schluessel: Mutex<Option<Vec<u8>>>,
    letzter_reload: Mutex<Option<Instant>>,
    naechster_token: AtomicU64,
    http: reqwest::Client,
    key_url: String,
}

pub enum WebhookAusgang {
    Angenommen,
    Verworfen,
    SignaturFalsch,
    Fehlerhaft,
    Ausgelastet,
}

impl WebhookAusgang {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Angenommen | Self::Verworfen => StatusCode::OK,
            Self::SignaturFalsch => StatusCode::UNAUTHORIZED,
            Self::Fehlerhaft => StatusCode::BAD_REQUEST,
            Self::Ausgelastet => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl KickDrehkreuz {
    pub fn new(key_url: &str) -> Self {
        Self {
            ziele: Mutex::new(HashMap::new()),
            gesehen: Mutex::new(HashMap::new()),
            schluessel: Mutex::new(None),
            letzter_reload: Mutex::new(None),
            naechster_token: AtomicU64::new(0),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(KEY_FRIST)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest-Client ohne Sonderoptionen"),
            key_url: key_url.trim_end_matches('/').to_string(),
        }
    }

    pub fn registrieren(
        &self,
        broadcaster_user_id: &str,
        eingang: mpsc::Sender<Ereignis>,
        channel_login: &str,
    ) -> u64 {
        let token = self.naechster_token.fetch_add(1, Ordering::Relaxed) + 1;
        self.ziele.lock().expect("Kick-Ziele").insert(
            broadcaster_user_id.to_string(),
            KickZiel {
                eingang,
                channel_login: channel_login.to_string(),
                token,
            },
        );
        token
    }

    pub fn abmelden(&self, broadcaster_user_id: &str, token: u64) {
        let mut ziele = self.ziele.lock().expect("Kick-Ziele");
        if ziele
            .get(broadcaster_user_id)
            .is_some_and(|ziel| ziel.token == token)
        {
            ziele.remove(broadcaster_user_id);
        }
    }

    async fn public_key(&self) -> Option<Vec<u8>> {
        if let Some(key) = self.schluessel.lock().expect("Kick-Key").clone() {
            return Some(key);
        }
        self.laden().await
    }

    async fn reload_key(&self) -> Option<Vec<u8>> {
        {
            let mut letzter = self.letzter_reload.lock().expect("Kick-Reload");
            if let Some(zeit) = *letzter
                && zeit.elapsed() < RELOAD_ENTPRELLUNG
            {
                return None;
            }
            *letzter = Some(Instant::now());
        }
        self.laden().await
    }

    async fn laden(&self) -> Option<Vec<u8>> {
        let antwort = self.http.get(self.key_url.as_str()).send().await.ok()?;
        if !antwort.status().is_success() {
            return None;
        }
        let json: Value = crate::http::json(antwort).await.ok()?;
        let pem = json
            .pointer("/data/public_key")
            .and_then(Value::as_str)?
            .trim();
        let pkcs1 = spki_pem_zu_pkcs1(pem)?;
        *self.schluessel.lock().expect("Kick-Key") = Some(pkcs1.clone());
        Some(pkcs1)
    }

    fn signatur_gueltig(
        key_pkcs1: &[u8],
        message_id: &str,
        timestamp: &str,
        body: &[u8],
        signatur_b64: &str,
    ) -> bool {
        let Ok(signatur) = base64::engine::general_purpose::STANDARD.decode(signatur_b64.trim())
        else {
            return false;
        };
        let mut nachricht = Vec::with_capacity(message_id.len() + timestamp.len() + body.len() + 2);
        nachricht.extend_from_slice(message_id.as_bytes());
        nachricht.push(b'.');
        nachricht.extend_from_slice(timestamp.as_bytes());
        nachricht.push(b'.');
        nachricht.extend_from_slice(body);
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key_pkcs1)
            .verify(&nachricht, &signatur)
            .is_ok()
    }

    pub async fn verarbeiten(
        &self,
        message_id: &str,
        timestamp: &str,
        event_typ: &str,
        signatur_b64: &str,
        body: &[u8],
    ) -> WebhookAusgang {
        if message_id.is_empty()
            || message_id.len() > 128
            || timestamp.is_empty()
            || timestamp.len() > 64
            || signatur_b64.is_empty()
            || signatur_b64.len() > 2048
            || body.len() > 1024 * 1024
            || event_typ.len() > 128
        {
            return WebhookAusgang::Fehlerhaft;
        }
        let mut gueltig = self.public_key().await.is_some_and(|key| {
            Self::signatur_gueltig(&key, message_id, timestamp, body, signatur_b64)
        });
        if !gueltig {
            gueltig = self.reload_key().await.is_some_and(|key| {
                Self::signatur_gueltig(&key, message_id, timestamp, body, signatur_b64)
            });
        }
        if !gueltig {
            return WebhookAusgang::SignaturFalsch;
        }
        let Ok(gesendet) = timestamp.parse::<DateTime<Utc>>() else {
            return WebhookAusgang::Fehlerhaft;
        };
        if (Utc::now() - gesendet).num_seconds().abs() > FENSTER.as_secs() as i64 {
            return WebhookAusgang::Verworfen;
        }
        let Ok(payload) = serde_json::from_slice::<Value>(body) else {
            return WebhookAusgang::Fehlerhaft;
        };
        let Some(broadcaster) = broadcaster_user_id(&payload) else {
            return WebhookAusgang::Verworfen;
        };
        let (eingang, channel_login) = {
            let ziele = self.ziele.lock().expect("Kick-Ziele");
            let Some(ziel) = ziele.get(&broadcaster) else {
                return WebhookAusgang::Verworfen;
            };
            (ziel.eingang.clone(), ziel.channel_login.clone())
        };
        let Some(ereignis) =
            kick_ereignis(event_typ, &payload, &broadcaster, &channel_login, gesendet)
        else {
            return WebhookAusgang::Verworfen;
        };
        // Atomar prüfen, lokal zustellen und erst dann bestätigen. Volle Queues
        // müssen beim Plattform-Retry erneut zustellbar bleiben.
        let mut seen = self.gesehen.lock().expect("Kick-Gesehen");
        seen.retain(|_, g| g.wann.elapsed() < FENSTER);
        if seen.contains_key(message_id) {
            return WebhookAusgang::Verworfen;
        }
        if seen.len() >= 10000 {
            return WebhookAusgang::Ausgelastet;
        }
        if eingang.try_send(ereignis).is_err() {
            return WebhookAusgang::Ausgelastet;
        }
        seen.insert(
            message_id.to_string(),
            Gesehen {
                wann: Instant::now(),
            },
        );
        WebhookAusgang::Angenommen
    }
}

#[cfg(test)]
impl KickDrehkreuz {
    fn reload_altern(&self) {
        *self.letzter_reload.lock().expect("Kick-Reload") = None;
    }
}

fn spki_pem_zu_pkcs1(pem: &str) -> Option<Vec<u8>> {
    let b64: String = pem
        .lines()
        .filter(|zeile| !zeile.starts_with("-----"))
        .flat_map(|zeile| zeile.trim().chars())
        .collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    spki_der_zu_pkcs1(&der)
}

fn spki_der_zu_pkcs1(spki: &[u8]) -> Option<Vec<u8>> {
    let mut top = DerLeser { rest: spki };
    let (tag, inner) = top.tlv()?;
    if tag != 0x30 {
        return None;
    }
    let mut koerper = DerLeser { rest: inner };
    let (alg_tag, _alg) = koerper.tlv()?;
    if alg_tag != 0x30 {
        return None;
    }
    let (bit_tag, bits) = koerper.tlv()?;
    if bit_tag != 0x03 {
        return None;
    }
    let (unbenutzt, schluessel) = bits.split_first()?;
    if *unbenutzt != 0 {
        return None;
    }
    Some(schluessel.to_vec())
}

struct DerLeser<'a> {
    rest: &'a [u8],
}

impl<'a> DerLeser<'a> {
    fn tlv(&mut self) -> Option<(u8, &'a [u8])> {
        let (&tag, nach_tag) = self.rest.split_first()?;
        let (&laenge0, nach_l0) = nach_tag.split_first()?;
        let (laenge, nach_laenge) = if laenge0 < 0x80 {
            (laenge0 as usize, nach_l0)
        } else {
            let anzahl = (laenge0 & 0x7f) as usize;
            if anzahl == 0 || anzahl > 4 || nach_l0.len() < anzahl {
                return None;
            }
            let mut laenge = 0usize;
            for &byte in &nach_l0[..anzahl] {
                laenge = (laenge << 8) | byte as usize;
            }
            (laenge, &nach_l0[anzahl..])
        };
        if nach_laenge.len() < laenge {
            return None;
        }
        let (wert, danach) = nach_laenge.split_at(laenge);
        self.rest = danach;
        Some((tag, wert))
    }
}

fn broadcaster_user_id(payload: &Value) -> Option<String> {
    payload
        .pointer("/broadcaster/user_id")
        .map(json_id)
        .filter(|s| !s.is_empty())
}

fn json_id(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn json_str(value: &Value, pfad: &str) -> Option<String> {
    value
        .pointer(pfad)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn nutzer_actor(value: &Value, wurzel: &str) -> Option<Actor> {
    let id = json_id(value.pointer(&format!("{wurzel}/user_id"))?);
    if id.is_empty() {
        return None;
    }
    let login = json_str(value, &format!("{wurzel}/channel_slug"))
        .or_else(|| json_str(value, &format!("{wurzel}/username")))
        .unwrap_or_else(|| id.clone());
    let display = json_str(value, &format!("{wurzel}/username")).unwrap_or_else(|| login.clone());
    Some(Actor::new(id, login, display))
}

pub fn kick_ereignis(
    event_typ: &str,
    payload: &Value,
    broadcaster_id: &str,
    channel_login: &str,
    gesendet: DateTime<Utc>,
) -> Option<Ereignis> {
    match event_typ {
        "chat.message.sent" => {
            chat_nachricht(payload, broadcaster_id, channel_login, gesendet).map(Ereignis::Chat)
        }
        "channel.followed" => {
            let actor = nutzer_actor(payload, "/follower")?;
            let meta = ActivityMeta::derived(
                Platform::Kick,
                broadcaster_id,
                gesendet,
                "follow",
                &actor.id,
            )
            .with_actor(actor);
            Some(Ereignis::Activity(ActivityEvent::Follow { meta }))
        }
        "channel.subscription.new" => {
            let actor = nutzer_actor(payload, "/subscriber")?;
            let meta = ActivityMeta::derived(
                Platform::Kick,
                broadcaster_id,
                gesendet,
                "subscribe",
                &actor.id,
            )
            .with_actor(actor);
            Some(Ereignis::Activity(ActivityEvent::Subscribe {
                meta,
                tier: String::new(),
                is_gift: false,
            }))
        }
        "channel.subscription.renewal" => {
            let actor = nutzer_actor(payload, "/subscriber")?;
            let months = payload
                .pointer("/duration")
                .and_then(Value::as_u64)
                .and_then(|m| u32::try_from(m).ok())
                .unwrap_or(1);
            let meta = ActivityMeta::derived(
                Platform::Kick,
                broadcaster_id,
                gesendet,
                "resub",
                &format!("{}:{months}", actor.id),
            )
            .with_actor(actor);
            Some(Ereignis::Activity(ActivityEvent::Resub {
                meta,
                months,
                streak: None,
                message: None,
                tier: None,
            }))
        }
        "channel.subscription.gifts" => {
            let count = payload
                .pointer("/giftees")
                .and_then(Value::as_array)
                .map(|g| u32::try_from(g.len()).unwrap_or(u32::MAX))
                .unwrap_or(0);
            let actor = nutzer_actor(payload, "/gifter");
            let kennzeichen = payload
                .pointer("/created_at")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| actor.as_ref().map(|a| a.id.clone()))
                .unwrap_or_else(|| gesendet.timestamp().to_string());
            let mut meta = ActivityMeta::derived(
                Platform::Kick,
                broadcaster_id,
                gesendet,
                "sub_gift",
                &kennzeichen,
            );
            if let Some(actor) = actor {
                meta = meta.with_actor(actor);
            }
            Some(Ereignis::Activity(ActivityEvent::SubGift {
                meta,
                count,
                tier: String::new(),
            }))
        }
        _ => None,
    }
}

fn chat_nachricht(
    payload: &Value,
    broadcaster_id: &str,
    channel_login: &str,
    gesendet: DateTime<Utc>,
) -> Option<ChatNachricht> {
    let message_id = json_str(payload, "/message_id")?;
    let sender_id = payload.pointer("/sender/user_id").map(json_id)?;
    if sender_id.is_empty() {
        return None;
    }
    let sender_login = json_str(payload, "/sender/channel_slug")
        .or_else(|| json_str(payload, "/sender/username"))
        .unwrap_or_else(|| sender_id.clone());
    let sender_display =
        json_str(payload, "/sender/username").unwrap_or_else(|| sender_login.clone());
    let channel_login =
        json_str(payload, "/broadcaster/channel_slug").unwrap_or_else(|| channel_login.to_string());
    let color = json_str(payload, "/sender/identity/username_color");
    let badges = payload
        .pointer("/sender/identity/badges")
        .and_then(Value::as_array)
        .map(|liste| liste.iter().filter_map(badge_lesen).collect::<Vec<_>>())
        .unwrap_or_default();
    let inhalt = json_str(payload, "/content").unwrap_or_default();
    let fragments = inhalt_zu_fragmenten(&inhalt);
    let sent_at = json_str(payload, "/created_at")
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .unwrap_or(gesendet);
    Some(ChatNachricht {
        platform: Platform::Kick,
        eigene: sender_id == broadcaster_id,
        hervorhebung: None,
        channel_id: broadcaster_id.to_string(),
        channel_login,
        message_id,
        sender_id,
        sender_login,
        sender_display,
        color,
        badges,
        fragments,
        sent_at,
        is_action: false,
        reply_to: None,
    })
}

fn badge_lesen(roh: &Value) -> Option<Badge> {
    let set_id = json_str(roh, "/type")?;
    let id = roh
        .pointer("/count")
        .and_then(Value::as_u64)
        .map(|c| c.to_string())
        .unwrap_or_default();
    Some(Badge {
        set_id,
        id,
        info: roh
            .pointer("/count")
            .and_then(Value::as_u64)
            .map(|c| c.to_string()),
        image_url: None,
    })
}

fn inhalt_zu_fragmenten(inhalt: &str) -> Vec<Fragment> {
    if inhalt.is_empty() {
        return Vec::new();
    }
    let mut sauber = String::with_capacity(inhalt.len());
    let mut rest = inhalt;
    while let Some(start) = rest.find("[emote:") {
        sauber.push_str(&rest[..start]);
        let nach_start = &rest[start..];
        let Some(ende) = nach_start.find(']') else {
            sauber.push_str(nach_start);
            rest = "";
            break;
        };
        let inner = &nach_start[7..ende];
        let name = inner.split(':').nth(1).unwrap_or("");
        sauber.push_str(name);
        rest = &nach_start[ende + 1..];
    }
    sauber.push_str(rest);
    vec![Fragment::text(sauber)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::RsaKeyPair;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn openssl(args: &[&str], input: &[u8]) -> Vec<u8> {
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};
        let mut child = Command::new("/usr/bin/openssl")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("OpenSSL ist für künstliche RSA-Testschlüssel erforderlich");
        child.stdin.take().unwrap().write_all(input).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("RSA-Testschlüssel hat die Frist überschritten");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            status.success(),
            "Künstlicher RSA-Testschlüssel konnte nicht erzeugt werden"
        );
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(16385)
            .read_to_end(&mut output)
            .unwrap();
        assert!(output.len() <= 16384);
        output
    }
    fn schluesselpaar() -> (Arc<RsaKeyPair>, String) {
        static PAAR: std::sync::OnceLock<(Arc<RsaKeyPair>, String)> = std::sync::OnceLock::new();
        PAAR.get_or_init(|| {
            let der = zeroize::Zeroizing::new(openssl(
                &[
                    "genpkey",
                    "-algorithm",
                    "RSA",
                    "-pkeyopt",
                    "rsa_keygen_bits:2048",
                    "-outform",
                    "DER",
                ],
                &[],
            ));
            let pem =
                String::from_utf8(openssl(&["pkey", "-inform", "DER", "-pubout"], &der)).unwrap();
            let privat = RsaKeyPair::from_der(&der).expect("Künstlicher RSA-Testschlüssel");
            (Arc::new(privat), pem)
        })
        .clone()
    }

    fn signieren(privat: &RsaKeyPair, message_id: &str, timestamp: &str, body: &[u8]) -> String {
        let mut nachricht = Vec::new();
        nachricht.extend_from_slice(message_id.as_bytes());
        nachricht.push(b'.');
        nachricht.extend_from_slice(timestamp.as_bytes());
        nachricht.push(b'.');
        nachricht.extend_from_slice(body);
        let mut signature = vec![0; privat.public().modulus_len()];
        privat
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                &nachricht,
                &mut signature,
            )
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(signature)
    }

    fn drehkreuz(server: &MockServer) -> KickDrehkreuz {
        KickDrehkreuz::new(&format!("{}/public/v1/public-key", server.uri()))
    }

    async fn key_abrufe(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .expect("Anfragen")
            .iter()
            .filter(|r| r.url.path() == "/public/v1/public-key")
            .count()
    }

    async fn key_server(pem: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/public/v1/public-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "data": { "public_key": pem } })),
            )
            .mount(&server)
            .await;
        server
    }

    fn chat_body(broadcaster: i64, sender: i64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "message_id": "msg-1",
            "broadcaster": { "user_id": broadcaster, "channel_slug": "earlysalty" },
            "sender": {
                "user_id": sender, "username": "Gast", "channel_slug": "gast",
                "identity": {
                    "username_color": "#D4AF37",
                    "badges": [ { "text": "Subscriber", "type": "subscriber", "count": 3 } ]
                }
            },
            "content": "moin [emote:123:KEKW] leute",
            "created_at": "2026-09-02T10:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn gueltige_nachricht_landet_beim_broadcaster() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx, mut rx) = mpsc::channel(4);
        kreuz.registrieren("123", tx, "earlysalty");
        let body = chat_body(123, 999);
        let ts = Utc::now().to_rfc3339();
        let sig = signieren(&privat, "m-1", &ts, &body);
        let ausgang = kreuz
            .verarbeiten("m-1", &ts, "chat.message.sent", &sig, &body)
            .await;
        assert_eq!(ausgang.status(), StatusCode::OK);
        let Ereignis::Chat(n) = rx.recv().await.expect("Nachricht") else {
            panic!("chat erwartet");
        };
        assert_eq!(n.platform, Platform::Kick);
        assert_eq!(n.channel_id, "123");
        assert_eq!(n.sender_display, "Gast");
        assert_eq!(n.color.as_deref(), Some("#D4AF37"));
        assert_eq!(n.plain_text(), "moin KEKW leute");
        assert!(!n.eigene);
    }

    #[tokio::test]
    async fn volle_queue_wird_nicht_bestaetigt_und_retry_bleibt_zustellbar() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx, mut rx) = mpsc::channel(1);
        kreuz.registrieren("123", tx, "example");
        let body = chat_body(123, 999);
        let ts = Utc::now().to_rfc3339();
        for (id, expected) in [
            ("first", StatusCode::OK),
            ("retry", StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let sig = signieren(&privat, id, &ts, &body);
            assert_eq!(
                kreuz
                    .verarbeiten(id, &ts, "chat.message.sent", &sig, &body)
                    .await
                    .status(),
                expected
            );
        }
        rx.recv().await.unwrap();
        let sig = signieren(&privat, "retry", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("retry", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::OK
        );
        rx.recv().await.unwrap();
        drop(rx);
        let sig = signieren(&privat, "closed", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("closed", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    #[tokio::test]
    async fn falsche_signatur_ist_401() {
        let (_privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx, mut rx) = mpsc::channel(4);
        kreuz.registrieren("123", tx, "earlysalty");
        let body = chat_body(123, 999);
        let ts = Utc::now().to_rfc3339();
        let ausgang = kreuz
            .verarbeiten("m-1", &ts, "chat.message.sent", "AAAA", &body)
            .await;
        assert_eq!(ausgang.status(), StatusCode::UNAUTHORIZED);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn reload_bei_fehlsignatur_wird_entprellt() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx, _rx) = mpsc::channel(4);
        kreuz.registrieren("123", tx, "earlysalty");
        let body = chat_body(123, 999);
        let ts = Utc::now().to_rfc3339();

        let sig = signieren(&privat, "m-1", &ts, &body);
        kreuz
            .verarbeiten("m-1", &ts, "chat.message.sent", &sig, &body)
            .await;
        assert_eq!(
            key_abrufe(&server).await,
            1,
            "erster Abruf laedt den Schluessel"
        );

        kreuz
            .verarbeiten("m-2", &ts, "chat.message.sent", "AAAA", &body)
            .await;
        assert_eq!(
            key_abrufe(&server).await,
            2,
            "erste Fehlsignatur laedt einmal neu"
        );

        kreuz
            .verarbeiten("m-3", &ts, "chat.message.sent", "AAAA", &body)
            .await;
        assert_eq!(
            key_abrufe(&server).await,
            2,
            "zweite Fehlsignatur innerhalb von 60 s laedt nicht erneut"
        );

        kreuz.reload_altern();
        kreuz
            .verarbeiten("m-4", &ts, "chat.message.sent", "AAAA", &body)
            .await;
        assert_eq!(
            key_abrufe(&server).await,
            3,
            "nach Ablauf wieder ein Reload"
        );
    }

    #[tokio::test]
    async fn stale_abmelden_loescht_neue_registrierung_nicht() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx_alt, mut rx_alt) = mpsc::channel(4);
        let alt_token = kreuz.registrieren("123", tx_alt, "earlysalty");
        let (tx_neu, mut rx_neu) = mpsc::channel(4);
        let neu_token = kreuz.registrieren("123", tx_neu, "earlysalty");

        kreuz.abmelden("123", alt_token);
        let body = chat_body(123, 999);
        let ts = Utc::now().to_rfc3339();
        let sig = signieren(&privat, "m-neu", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("m-neu", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::OK
        );
        assert!(
            rx_neu.recv().await.is_some(),
            "neue Registrierung ueberlebt"
        );
        assert!(rx_alt.try_recv().is_err());

        kreuz.abmelden("123", neu_token);
        let sig2 = signieren(&privat, "m-weg", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("m-weg", &ts, "chat.message.sent", &sig2, &body)
                .await
                .status(),
            StatusCode::OK
        );
        assert!(
            rx_neu.try_recv().is_err(),
            "gueltiges abmelden entfernt die Registrierung"
        );
    }

    #[tokio::test]
    async fn replay_wird_verworfen() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx, mut rx) = mpsc::channel(4);
        kreuz.registrieren("123", tx, "earlysalty");
        let body = chat_body(123, 999);
        let ts = Utc::now().to_rfc3339();
        let sig = signieren(&privat, "m-2", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("m-2", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            kreuz
                .verarbeiten("m-2", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
        assert!(rx.try_recv().is_err(), "zweite Zustellung bleibt aus");
    }

    #[tokio::test]
    async fn alter_zeitstempel_wird_verworfen() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let (tx, mut rx) = mpsc::channel(4);
        kreuz.registrieren("123", tx, "earlysalty");
        let body = chat_body(123, 999);
        let ts = (Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        let sig = signieren(&privat, "m-3", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("m-3", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::OK
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unbekannter_broadcaster_wird_still_verworfen() {
        let (privat, pem) = schluesselpaar();
        let server = key_server(&pem).await;
        let kreuz = drehkreuz(&server);
        let body = chat_body(555, 999);
        let ts = Utc::now().to_rfc3339();
        let sig = signieren(&privat, "m-4", &ts, &body);
        assert_eq!(
            kreuz
                .verarbeiten("m-4", &ts, "chat.message.sent", &sig, &body)
                .await
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn follow_wird_zu_aktivitaet() {
        let gesendet = Utc::now();
        let payload = json!({
            "broadcaster": { "user_id": 123, "channel_slug": "earlysalty" },
            "follower": { "user_id": 42, "username": "Neu", "channel_slug": "neu" }
        });
        let Ereignis::Activity(ActivityEvent::Follow { meta }) =
            kick_ereignis("channel.followed", &payload, "123", "earlysalty", gesendet).unwrap()
        else {
            panic!("follow erwartet");
        };
        assert_eq!(meta.platform, Platform::Kick);
        assert_eq!(meta.actor.unwrap().display, "Neu");
        assert_eq!(meta.dedupe_key, "kick:123:follow:42");
    }
}
