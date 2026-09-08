//! Helix-Aufrufe im Namen eines Streamers, mit Token vom Bot.
//!
//! Ein Client je Streamer: holt den Zugang bei der [`TokenQuelle`], fragt
//! die Client-Id einmal ueber `/oauth2/validate` (Helix verlangt sie als
//! Header, der Bot reicht sie nicht mit) und wiederholt jeden Aufruf nach
//! einer 401 genau einmal mit frischem Token. Alles andere (403, 400, 5xx)
//! gibt er mit Status und Koerper zurueck, denn was das heisst, weiss nur
//! der Aufrufer: beim Senden ist 403 "neu anmelden", bei Kanalpunkten
//! "Reward gehoert einer anderen App".
//!
//! Ein Refresh-Token kommt hier nie an (INV-7).

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::Platform;
use crate::adapter::ChatFehler;
use crate::token::{TokenFehler, TokenQuelle, Zugang};

/// Frist je Helix-Aufruf.
pub const HELIX_FRIST: Duration = Duration::from_secs(10);

/// Wohin der Client spricht. Im Dienst Twitch, im Test wiremock.
#[derive(Debug, Clone)]
pub struct HelixEndpunkte {
    /// Helix-Basis ohne Schraegstrich am Ende.
    pub helix: String,
    /// id.twitch.tv-Basis, fuer `/oauth2/validate`.
    pub id: String,
}

impl Default for HelixEndpunkte {
    fn default() -> Self {
        Self {
            helix: "https://api.twitch.tv/helix".into(),
            id: "https://id.twitch.tv".into(),
        }
    }
}

/// Baut Helix-Clients je Streamer. Token und Client-Id kommen erst beim
/// ersten Aufruf; ein Streamer ohne Verbindung faellt dort als
/// `NichtVerbunden` auf, nicht beim Bauen.
pub struct HelixFabrik {
    quelle: Arc<TokenQuelle>,
    endpunkte: HelixEndpunkte,
    http: reqwest::Client,
}

impl HelixFabrik {
    pub fn new(quelle: Arc<TokenQuelle>) -> Self {
        Self::mit_endpunkten(quelle, HelixEndpunkte::default())
    }

    pub fn mit_endpunkten(quelle: Arc<TokenQuelle>, endpunkte: HelixEndpunkte) -> Self {
        Self {
            quelle,
            endpunkte,
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(HELIX_FRIST)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest-Client ohne Sonderoptionen"),
        }
    }

    pub fn client(&self, streamer_id: i64) -> HelixClient {
        HelixClient {
            streamer_id,
            quelle: self.quelle.clone(),
            endpunkte: self.endpunkte.clone(),
            http: self.http.clone(),
            client_id: std::sync::Mutex::new(None),
        }
    }
}

pub struct HelixClient {
    streamer_id: i64,
    quelle: Arc<TokenQuelle>,
    endpunkte: HelixEndpunkte,
    http: reqwest::Client,
    client_id: std::sync::Mutex<Option<String>>,
}

pub fn fehler_aus_token(fehler: TokenFehler) -> ChatFehler {
    match fehler {
        TokenFehler::NichtVerbunden(p) => ChatFehler::NichtVerbunden(p),
        TokenFehler::NeuAnmeldungNoetig(p) => ChatFehler::NeuAnmeldungNoetig(p),
        // Eigene Variante statt eines Netzfehlers mit Text: ein abgelehnter
        // interner Token vergeht nicht von allein, und die Leseschleife soll
        // das erkennen koennen, ohne eine Meldung zu vergleichen.
        TokenFehler::Abgelehnt => ChatFehler::InternerZugang,
        TokenFehler::Netz(text) => ChatFehler::Netz(text),
    }
}

/// Was das Dock liest, wenn Twitch gerade drosselt.
pub const HINWEIS_BREMSE: &str = "Twitch bremst gerade, bitte gleich noch einmal";

/// Laengste Wartezeit, die ein 429 auslösen darf.
///
/// Twitch nennt in `Ratelimit-Reset` den Zeitpunkt, ab dem wieder Punkte da
/// sind. Der liegt normal wenige Sekunden voraus; laenger warten hiesse, den
/// Aufrufer (und mit ihm den Streamer im Dock) haengen zu lassen.
const BREMSE_MAX: Duration = Duration::from_secs(5);
/// Wartezeit ohne verwertbaren `Ratelimit-Reset`.
const BREMSE_VORGABE: Duration = Duration::from_secs(1);

/// Wie lange ein 429 abgewartet wird. `Ratelimit-Reset` ist ein Unix-
/// Zeitstempel in Sekunden.
fn bremse_wartezeit(headers: &reqwest::header::HeaderMap, jetzt: i64) -> Duration {
    let reset = headers
        .get("Ratelimit-Reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok());
    match reset {
        Some(reset) if reset > jetzt => Duration::from_secs((reset - jetzt) as u64).min(BREMSE_MAX),
        _ => BREMSE_VORGABE,
    }
}

impl HelixClient {
    /// Der Zugang des Streamers: Kanal-Id, Login, Scopes. Aus dem Cache der
    /// Quelle oder frisch vom Bot.
    pub async fn zugang(&self) -> Result<Zugang, ChatFehler> {
        self.quelle
            .zugang(self.streamer_id, Platform::Twitch)
            .await
            .map_err(fehler_aus_token)
    }

    async fn token(&self) -> Result<String, ChatFehler> {
        self.zugang().await.map(|z| z.access_token.clone())
    }

    /// Client-Id zum Token, ueber `/oauth2/validate`; einmal je Client gefragt.
    ///
    /// Derselbe Zweiversuchsrahmen wie in [`Self::anfrage`]: ein einzelner 401
    /// auf einen gerade abgelaufenen Cache-Eintrag darf dem Streamer nicht
    /// "neu verbinden" anzeigen, solange der Bot einen frischen Token liefern
    /// kann.
    pub async fn client_id(&self) -> Result<String, ChatFehler> {
        if let Some(id) = self.client_id.lock().expect("client_id").clone() {
            return Ok(id);
        }
        for versuch in 0..2 {
            let token = self.token().await?;
            let antwort = self
                .http
                .get(format!("{}/oauth2/validate", self.endpunkte.id))
                .header("Authorization", format!("OAuth {token}"))
                .send()
                .await
                .map_err(|_| ChatFehler::Netz("Plattformverbindung fehlgeschlagen".into()))?;
            if antwort.status() == StatusCode::UNAUTHORIZED {
                self.quelle.invalidieren(self.streamer_id, Platform::Twitch);
                if versuch == 0 {
                    continue;
                }
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
            }
            if !antwort.status().is_success() {
                return Err(ChatFehler::Netz(format!(
                    "validate: HTTP {}",
                    antwort.status()
                )));
            }
            let koerper: Value = crate::http::json(antwort).await?;
            let id = koerper
                .get("client_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ChatFehler::Netz("validate ohne client_id".into()))?;
            *self.client_id.lock().expect("client_id") = Some(id.clone());
            return Ok(id);
        }
        Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
    }

    /// Ein Helix-Aufruf: `pfad` relativ zur Helix-Basis (`/channels`),
    /// `query` als Paare, `body` als JSON. 401: Token einmal neu holen und
    /// wiederholen, danach `NeuAnmeldungNoetig`. Alle anderen Antworten
    /// kommen als Status plus Koerper zurueck (leerer Koerper wird `null`).
    pub async fn anfrage(
        &self,
        method: Method,
        pfad: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), ChatFehler> {
        let client_id = self.client_id().await?;
        for versuch in 0..2 {
            let token = self.token().await?;
            let mut bau = self
                .http
                .request(method.clone(), format!("{}{}", self.endpunkte.helix, pfad))
                .header("Authorization", format!("Bearer {token}"))
                .header("Client-Id", &client_id)
                .query(query);
            if let Some(body) = body {
                bau = bau.json(body);
            }
            let antwort = bau
                .send()
                .await
                .map_err(|_| ChatFehler::Netz("Plattformverbindung fehlgeschlagen".into()))?;
            let status = antwort.status();
            if status == StatusCode::UNAUTHORIZED {
                if versuch == 0 {
                    self.quelle.invalidieren(self.streamer_id, Platform::Twitch);
                    continue;
                }
                return Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch));
            }
            // Twitchs Kontingent haengt an der Client-Id, also an der ganzen
            // App. Einmal abwarten und wiederholen ist billiger als die
            // Nachricht wegzuwerfen; was danach noch bremst, geht als 429 an
            // den Aufrufer, der daraus einen deutschen Hinweis macht.
            if status == StatusCode::TOO_MANY_REQUESTS && versuch == 0 {
                let warten = bremse_wartezeit(antwort.headers(), chrono::Utc::now().timestamp());
                tracing::info!(
                    streamer_id = self.streamer_id,
                    pfad,
                    wartezeit_s = warten.as_secs(),
                    "Twitch drosselt, ein Versuch nach der Wartezeit"
                );
                tokio::time::sleep(warten).await;
                continue;
            }
            let text = String::from_utf8(crate::http::bytes(antwort).await?)
                .map_err(|_| ChatFehler::Netz("Plattformantwort ist nicht lesbar".into()))?;
            let koerper = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            };
            return Ok((status, koerper));
        }
        Err(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
    }
}

#[cfg(test)]
pub(crate) mod testhilfe {
    //! Fake-Bot und Fake-Helix auf einem wiremock, fuer alle Helix-Tests.

    use super::*;
    use crate::token::PFAD;
    use chrono::Utc;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Bot mit Token (Scopes nach Wahl) plus validate, `bot_erwartet` Abrufe.
    pub async fn bot_und_helix_mit_scopes(bot_erwartet: u64, scopes: &[&str]) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(PFAD))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "acc-1",
                "expires_at": (Utc::now() + chrono::Duration::hours(3)).to_rfc3339(),
                "platform_user_id": "12345",
                "platform_login": "earlysalty",
                "scopes": scopes
            })))
            .expect(bot_erwartet)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oauth2/validate"))
            .and(header("Authorization", "OAuth acc-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "client_id": "client-abc", "login": "earlysalty", "user_id": "12345", "expires_in": 9999
            })))
            .mount(&server)
            .await;
        server
    }

    pub fn fabrik(server: &MockServer) -> HelixFabrik {
        HelixFabrik::mit_endpunkten(
            Arc::new(TokenQuelle::new(&server.uri(), "intern")),
            HelixEndpunkte {
                helix: server.uri(),
                id: server.uri(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testhilfe::{bot_und_helix_mit_scopes, fabrik};
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, ResponseTemplate};

    #[tokio::test]
    async fn helix_401_holt_token_neu_und_wiederholt_einmal() {
        // Zwei Token-Abrufe: einer vorab, einer nach der 401.
        let server = bot_und_helix_mit_scopes(2, &["user:read:chat"]).await;
        Mock::given(method("GET"))
            .and(path("/channels"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/channels"))
            .and(query_param("broadcaster_id", "12345"))
            .and(header("Client-Id", "client-abc"))
            .and(header("Authorization", "Bearer acc-1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "data": [{ "title": "x" }] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = fabrik(&server).client(7);
        let (status, koerper) = client
            .anfrage(
                Method::GET,
                "/channels",
                &[("broadcaster_id", "12345")],
                None,
            )
            .await
            .expect("nach Wiederholung ok");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(koerper["data"][0]["title"], "x");
        server.verify().await;
    }

    #[tokio::test]
    async fn zweite_401_heisst_neu_anmelden_und_andere_status_kommen_durch() {
        // Token vorab, einmal neu nach der ersten 401; die zweite 401 holt
        // nicht mehr nach, die Suche danach laeuft mit dem Cache.
        let server = bot_und_helix_mit_scopes(2, &[]).await;
        Mock::given(method("PATCH"))
            .and(path("/channels"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/categories"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                json!({ "error": "Bad Request", "status": 400, "message": "query fehlt" }),
            ))
            .mount(&server)
            .await;
        let client = fabrik(&server).client(7);
        assert_eq!(
            client
                .anfrage(Method::PATCH, "/channels", &[], Some(&json!({})))
                .await
                .err(),
            Some(ChatFehler::NeuAnmeldungNoetig(Platform::Twitch))
        );
        let (status, koerper) = client
            .anfrage(Method::GET, "/search/categories", &[], None)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(koerper["message"], "query fehlt");
    }

    /// Twitchs Kontingent haengt an der Client-Id, also an der ganzen App.
    /// Ein 429 darf die Nachricht nicht wegwerfen: einmal abwarten und
    /// wiederholen kostet Sekunden, der Verlust kostet den Text des
    /// Streamers.
    #[tokio::test]
    async fn ratelimit_wird_einmal_abgewartet_und_wiederholt() {
        let server = bot_und_helix_mit_scopes(1, &["user:write:chat"]).await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .respond_with(ResponseTemplate::new(429).insert_header(
                "Ratelimit-Reset",
                chrono::Utc::now().timestamp().to_string(),
            ))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "data": [{ "is_sent": true }] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = fabrik(&server).client(7);
        let start = std::time::Instant::now();
        let (status, koerper) = client
            .anfrage(Method::POST, "/chat/messages", &[], Some(&json!({})))
            .await
            .expect("nach der Wartezeit ok");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(koerper["data"][0]["is_sent"], true);
        assert!(
            start.elapsed() >= BREMSE_VORGABE,
            "es wurde nicht gewartet, nur wiederholt"
        );
        server.verify().await;
    }

    #[test]
    fn wartezeit_kommt_aus_ratelimit_reset_und_ist_gedeckelt() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(bremse_wartezeit(&headers, 1000), BREMSE_VORGABE);
        headers.insert("Ratelimit-Reset", "1003".parse().unwrap());
        assert_eq!(bremse_wartezeit(&headers, 1000), Duration::from_secs(3));
        headers.insert("Ratelimit-Reset", "9999".parse().unwrap());
        assert_eq!(bremse_wartezeit(&headers, 1000), BREMSE_MAX);
        headers.insert("Ratelimit-Reset", "500".parse().unwrap());
        assert_eq!(bremse_wartezeit(&headers, 1000), BREMSE_VORGABE);
        headers.insert("Ratelimit-Reset", "quatsch".parse().unwrap());
        assert_eq!(bremse_wartezeit(&headers, 1000), BREMSE_VORGABE);
    }

    /// Ein einzelner 401 auf `/oauth2/validate` mit einem gerade abgelaufenen
    /// Cache-Eintrag darf dem Streamer nicht "neu verbinden" anzeigen,
    /// solange der Bot einen frischen Token liefern kann.
    #[tokio::test]
    async fn client_id_holt_nach_einer_401_den_token_neu_und_wiederholt() {
        // Eigener Server statt der Testhilfe: die 401 muss vor der guten
        // Antwort haengen, sonst greift sie nie.
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(crate::token::PFAD))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "acc-1",
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339(),
                "platform_user_id": "12345",
                "platform_login": "earlysalty",
                "scopes": ["user:read:chat"]
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oauth2/validate"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oauth2/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "client_id": "client-abc", "login": "earlysalty", "user_id": "12345"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = fabrik(&server).client(7);
        assert_eq!(
            client.client_id().await.expect("zweiter Versuch"),
            "client-abc"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn ohne_verbindung_faellt_der_erste_aufruf_als_nicht_verbunden() {
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(crate::token::PFAD))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = fabrik(&server).client(7);
        assert_eq!(
            client
                .anfrage(Method::GET, "/channels", &[], None)
                .await
                .err(),
            Some(ChatFehler::NichtVerbunden(Platform::Twitch))
        );
    }
}
