//! Stream-Infos bei Twitch: Helix `GET /channels`, `PATCH /channels`
//! (Scope `channel:manage:broadcast`), `GET /search/categories`.

use std::sync::Arc;

use futures::future::BoxFuture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::{Kategorie, StreamInfoAdapter, StreamInfoFabrik};
use crate::Platform;
use crate::adapter::ChatFehler;
use crate::ereignis::{StreamInfo, StreamInfoPatch};
use crate::helix::{HINWEIS_BREMSE, HelixClient, HelixFabrik};

/// Wie viele Kategorien die Suche liefert.
const KATEGORIEN_MAX: &str = "10";
/// Name des Aufrufs in `fehler`. Steht als Konstante da, damit Aufruf und
/// Fallunterscheidung nicht auseinanderlaufen koennen.
const SUCHE: &str = "search/categories";

pub struct TwitchStreamInfoFabrik {
    helix: Arc<HelixFabrik>,
}

impl TwitchStreamInfoFabrik {
    pub fn new(helix: Arc<HelixFabrik>) -> Self {
        Self { helix }
    }
}

impl StreamInfoFabrik for TwitchStreamInfoFabrik {
    fn bauen(
        &self,
        streamer_id: i64,
        platform: Platform,
    ) -> BoxFuture<'_, Result<Arc<dyn StreamInfoAdapter>, ChatFehler>> {
        Box::pin(async move {
            if platform != Platform::Twitch {
                return Err(ChatFehler::NichtUnterstuetzt(platform));
            }
            let client = self.helix.client(streamer_id);
            // Ohne Verbindung kein Adapter; 404 vom Bot faellt hier auf.
            let zugang = client.zugang().await?;
            let adapter: Arc<dyn StreamInfoAdapter> = Arc::new(TwitchStreamInfo {
                client,
                broadcaster_id: zugang.platform_user_id.clone(),
            });
            Ok(adapter)
        })
    }
}

pub struct TwitchStreamInfo {
    client: HelixClient,
    broadcaster_id: String,
}

impl TwitchStreamInfo {
    fn fehler(status: StatusCode, _koerper: &Value, was: &str) -> ChatFehler {
        match status {
            StatusCode::FORBIDDEN => ChatFehler::NeuAnmeldungNoetig(Platform::Twitch),
            StatusCode::TOO_MANY_REQUESTS => ChatFehler::Abgelehnt(HINWEIS_BREMSE.into()),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                // Die Meldung von Twitch ist englisch und technisch. Sie
                // gehoert ins Log, nicht ins Dock (INV-3).
                tracing::info!(was, "Twitch nimmt die Stream-Infos nicht an");
                // Die Kategoriesuche laeuft durch dieselbe Funktion. Wer nur
                // getippt hat, soll nicht lesen, er solle seine Tags pruefen.
                if was == SUCHE {
                    ChatFehler::Abgelehnt("Kategoriesuche gerade nicht möglich".into())
                } else {
                    ChatFehler::Abgelehnt(
                        "Twitch nimmt diese Angaben nicht an. Titel, Kategorie und Tags prüfen."
                            .into(),
                    )
                }
            }
            sonst => ChatFehler::Netz(format!("{was}: HTTP {sonst}")),
        }
    }

    /// Bild einer Kategorie ueber `GET /games?id=`. Ein Fehlschlag ist
    /// `None`, damit das Lesen der Stream-Infos daran nicht scheitert.
    async fn box_art(&self, category_id: &str) -> Option<String> {
        let (status, koerper) = self
            .client
            .anfrage(Method::GET, "/games", &[("id", category_id)], None)
            .await
            .ok()?;
        if !status.is_success() {
            return None;
        }
        text(koerper.pointer("/data/0")?, "box_art_url")
    }
}

impl StreamInfoAdapter for TwitchStreamInfo {
    fn platform(&self) -> Platform {
        Platform::Twitch
    }

    fn lesen(&self) -> BoxFuture<'_, Result<StreamInfo, ChatFehler>> {
        Box::pin(async move {
            let (status, koerper) = self
                .client
                .anfrage(
                    Method::GET,
                    "/channels",
                    &[("broadcaster_id", &self.broadcaster_id)],
                    None,
                )
                .await?;
            if !status.is_success() {
                return Err(Self::fehler(status, &koerper, "channels"));
            }
            let eintrag = koerper
                .pointer("/data/0")
                .ok_or_else(|| ChatFehler::Netz("channels ohne Eintrag".into()))?;
            let mut info = channel_zu_info(eintrag, &self.broadcaster_id);
            // `GET /channels` nennt die Kategorie nur mit Id und Name. Das
            // Bild steht bei `GET /games`; ohne Antwort bleibt die Karte im
            // Fenster beim Namen, das ist kein Fehler des Lesens. Ohne
            // Kategorie faellt der zweite Aufruf ganz weg.
            if let Some(id) = info.category_id.clone() {
                info.category_bild = self.box_art(&id).await;
            }
            Ok(info)
        })
    }

    fn setzen<'a>(&'a self, patch: &'a StreamInfoPatch) -> BoxFuture<'a, Result<(), ChatFehler>> {
        Box::pin(async move {
            let mut body = serde_json::Map::new();
            if let Some(title) = &patch.title {
                body.insert("title".into(), json!(title));
            }
            if let Some(game_id) = &patch.category_id {
                body.insert("game_id".into(), json!(game_id));
            }
            if let Some(tags) = &patch.tags {
                body.insert("tags".into(), json!(tags));
            }
            if body.is_empty() {
                return Ok(());
            }
            if !crate::twitch::scope_vorhanden(
                &self.client.zugang().await?.scopes,
                "channel:manage:broadcast",
            ) {
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
            }
            let (status, koerper) = self
                .client
                .anfrage(
                    Method::PATCH,
                    "/channels",
                    &[("broadcaster_id", &self.broadcaster_id)],
                    Some(&Value::Object(body)),
                )
                .await?;
            if status.is_success() {
                Ok(())
            } else {
                Err(Self::fehler(status, &koerper, "channels"))
            }
        })
    }

    fn kategorien_suchen<'a>(
        &'a self,
        suche: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Kategorie>, ChatFehler>> {
        Box::pin(async move {
            let (status, koerper) = self
                .client
                .anfrage(
                    Method::GET,
                    "/search/categories",
                    &[("query", suche), ("first", KATEGORIEN_MAX)],
                    None,
                )
                .await?;
            if !status.is_success() {
                return Err(Self::fehler(status, &koerper, SUCHE));
            }
            Ok(koerper
                .get("data")
                .and_then(Value::as_array)
                .map(|liste| {
                    liste
                        .iter()
                        .filter_map(|k| {
                            Some(Kategorie {
                                id: text(k, "id")?,
                                name: text(k, "name")?,
                                bild: text(k, "box_art_url"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default())
        })
    }
}

fn text(wert: &Value, feld: &str) -> Option<String> {
    wert.get(feld)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Bildet einen Eintrag aus `GET /channels` ab. Livestatus liefert der
/// Endpunkt nicht; er bleibt `false` und das Dock zeigt ihn nicht.
pub fn channel_zu_info(eintrag: &Value, broadcaster_id: &str) -> StreamInfo {
    StreamInfo {
        platform: Platform::Twitch,
        channel_id: text(eintrag, "broadcaster_id").unwrap_or_else(|| broadcaster_id.to_string()),
        title: text(eintrag, "title").unwrap_or_default(),
        category_id: text(eintrag, "game_id"),
        category_name: text(eintrag, "game_name"),
        // `GET /channels` fuehrt kein Bild. Es kommt aus `GET /games`,
        // deshalb steht hier nichts zu holen.
        category_bild: None,
        // Diese Quelle fuehrt das Feld. Eine leere Liste heisst hier wirklich
        // "keine gesetzt", nicht "weiss ich nicht": anders als bei
        // `channel.update`, das die Tags gar nicht mitschickt.
        tags: Some(
            eintrag
                .get("tags")
                .and_then(Value::as_array)
                .map(|liste| {
                    liste
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        // Der Kanal-Endpunkt sagt nichts ueber den Sendebetrieb.
        is_live: None,
        started_at: None,
        viewers: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helix::testhilfe::{bot_und_helix_mit_scopes, fabrik};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};

    async fn adapter(server: &wiremock::MockServer) -> Arc<dyn StreamInfoAdapter> {
        TwitchStreamInfoFabrik::new(Arc::new(fabrik(server)))
            .bauen(7, Platform::Twitch)
            .await
            .expect("Adapter")
    }

    #[tokio::test]
    async fn stream_info_lesen_bildet_helix_channels_ab() {
        let server = bot_und_helix_mit_scopes(1, &["channel:manage:broadcast"]).await;
        Mock::given(method("GET"))
            .and(path("/channels"))
            .and(query_param("broadcaster_id", "12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [{
                "broadcaster_id": "12345", "broadcaster_login": "earlysalty",
                "game_id": "1234", "game_name": "Deadlock",
                "title": "Ranked mit Chat", "tags": ["Deutsch", "Ranked"],
                "delay": 0, "content_classification_labels": [], "is_branded_content": false
            }] })))
            .mount(&server)
            .await;
        // `GET /channels` kennt kein Bild; das holt der Adapter bei `GET /games`,
        // damit das Fenster die Kategorie als Karte zeigen kann.
        Mock::given(method("GET"))
            .and(path("/games"))
            .and(query_param("id", "1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [{
                "id": "1234", "name": "Deadlock",
                "box_art_url": "https://static-cdn.jtvnw.net/ttv-boxart/1234-{width}x{height}.jpg"
            }] })))
            .expect(1)
            .mount(&server)
            .await;
        let info = adapter(&server).await.lesen().await.expect("gelesen");
        assert_eq!(info.platform, Platform::Twitch);
        assert_eq!(info.channel_id, "12345");
        assert_eq!(info.title, "Ranked mit Chat");
        assert_eq!(info.category_id.as_deref(), Some("1234"));
        assert_eq!(info.category_name.as_deref(), Some("Deadlock"));
        assert_eq!(
            info.category_bild.as_deref(),
            Some("https://static-cdn.jtvnw.net/ttv-boxart/1234-{width}x{height}.jpg")
        );
        assert_eq!(
            info.tags.as_deref(),
            Some(&["Deutsch".to_string(), "Ranked".to_string()][..])
        );
        let a = TwitchStreamInfoFabrik::new(Arc::new(fabrik(&server)))
            .bauen(7, Platform::Kick)
            .await
            .err();
        assert_eq!(a, Some(ChatFehler::NichtUnterstuetzt(Platform::Kick)));
    }

    /// Antwortet `GET /games` nicht, bleibt die Karte beim Namen. Das Lesen
    /// der Stream-Infos darf daran nicht scheitern.
    #[tokio::test]
    async fn ohne_bild_bleibt_das_lesen_erfolgreich() {
        let server = bot_und_helix_mit_scopes(1, &["channel:manage:broadcast"]).await;
        Mock::given(method("GET"))
            .and(path("/channels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [{
                "broadcaster_id": "12345", "game_id": "1234", "game_name": "Deadlock",
                "title": "Ranked mit Chat", "tags": []
            }] })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/games"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let info = adapter(&server).await.lesen().await.expect("gelesen");
        assert_eq!(info.category_name.as_deref(), Some("Deadlock"));
        assert_eq!(info.category_bild, None);
    }

    #[tokio::test]
    async fn stream_info_setzen_schickt_nur_gesetzte_felder() {
        let server = bot_und_helix_mit_scopes(1, &["channel:manage:broadcast"]).await;
        Mock::given(method("PATCH"))
            .and(path("/channels"))
            .and(query_param("broadcaster_id", "12345"))
            .and(body_json(json!({ "title": "Neu", "tags": ["a", "b"] })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let patch = StreamInfoPatch {
            title: Some("Neu".into()),
            category_id: None,
            tags: Some(vec!["a".into(), "b".into()]),
        };
        adapter(&server)
            .await
            .setzen(&patch)
            .await
            .expect("gesetzt");
        server.verify().await;
    }

    #[tokio::test]
    async fn titel_ohne_verwaltungsrecht_sendet_keinen_patch() {
        let server = bot_und_helix_mit_scopes(1, &["user:read:chat"]).await;
        Mock::given(method("PATCH"))
            .and(path("/channels"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let patch = StreamInfoPatch {
            title: Some("Neuer Titel".into()),
            ..Default::default()
        };
        assert_eq!(
            adapter(&server).await.setzen(&patch).await,
            Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
        );
    }

    #[tokio::test]
    async fn stream_info_setzen_400_ist_lesbarer_fehler() {
        let server = bot_und_helix_mit_scopes(1, &["channel:manage:broadcast"]).await;
        Mock::given(method("PATCH"))
            .and(path("/channels"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "Bad Request", "status": 400,
                "message": "The tags field may contain no more than 10 tags."
            })))
            .mount(&server)
            .await;
        let patch = StreamInfoPatch {
            tags: Some(vec!["x".into()]),
            ..Default::default()
        };
        let fehler = adapter(&server).await.setzen(&patch).await.err().unwrap();
        // Die englische Helix-Meldung darf nicht durchschlagen (INV-3).
        let text = crate::supervisor::hinweis_fuer(&fehler);
        assert_eq!(
            fehler,
            ChatFehler::Abgelehnt(
                "Twitch nimmt diese Angaben nicht an. Titel, Kategorie und Tags prüfen.".into()
            )
        );
        assert_eq!(
            text,
            "Twitch nimmt diese Angaben nicht an. Titel, Kategorie und Tags prüfen."
        );
        assert!(
            !text.contains("tags field"),
            "kein Englisch im Dock: {text}"
        );
    }

    #[tokio::test]
    async fn kategorien_suche_liefert_id_und_name() {
        let server = bot_und_helix_mit_scopes(1, &[]).await;
        Mock::given(method("GET"))
            .and(path("/search/categories"))
            .and(query_param("query", "dead"))
            .and(query_param("first", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [
                { "id": "1234", "name": "Deadlock", "box_art_url": "https://example.invalid/d.jpg" },
                { "id": "5678", "name": "Dead by Daylight" }
            ] })))
            .mount(&server)
            .await;
        let liste = adapter(&server)
            .await
            .kategorien_suchen("dead")
            .await
            .expect("gesucht");
        assert_eq!(liste.len(), 2);
        assert_eq!(liste[0].id, "1234");
        assert_eq!(liste[0].name, "Deadlock");
        assert_eq!(
            liste[0].bild.as_deref(),
            Some("https://example.invalid/d.jpg")
        );
        assert_eq!(liste[1].bild, None);
    }

    /// Die Kategoriesuche laeuft durch dieselbe Fehlerfunktion wie das
    /// Speichern. Wer nur getippt hat, soll nicht lesen, er solle Titel und
    /// Tags pruefen.
    #[tokio::test]
    async fn kategoriesuche_400_hat_einen_eigenen_text() {
        let server = bot_und_helix_mit_scopes(1, &["channel:manage:broadcast"]).await;
        Mock::given(method("GET"))
            .and(path("/search/categories"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "Bad Request", "status": 400, "message": "invalid query"
            })))
            .mount(&server)
            .await;
        let adapter = adapter(&server).await;
        let fehler = adapter
            .kategorien_suchen("dead")
            .await
            .expect_err("400 ist ein Fehler");
        assert_eq!(
            fehler,
            ChatFehler::Abgelehnt("Kategoriesuche gerade nicht möglich".into())
        );
    }
}
