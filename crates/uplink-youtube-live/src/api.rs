use crate::fehler::ApiFehler;
use crate::model::Sichtbarkeit;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reqwest::header::HeaderValue;
use reqwest::{Method, Response};
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;
use zeroize::Zeroizing;

pub const STANDARD_BASIS: &str = "https://www.googleapis.com/youtube/v3";
const STANDARD_FRIST: Duration = Duration::from_secs(10);
const MAX_GET: usize = 1024 * 1024;
const MAX_POST: usize = 256 * 1024;
const MAX_SEITEN: usize = 5;

#[derive(Clone)]
pub struct StreamRessource {
    pub id: String,
    pub channel_id: Option<String>,
    pub titel: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub ingestion_type: Option<String>,
    pub stream_status: Option<String>,
    pub rtmps_url: Option<String>,
    pub stream_name: Zeroizing<String>,
}

impl fmt::Debug for StreamRessource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamRessource")
            .field("id", &self.id)
            .field("channel_id", &self.channel_id)
            .field("titel", &self.titel)
            .field("published_at", &self.published_at)
            .field("ingestion_type", &self.ingestion_type)
            .field("stream_status", &self.stream_status)
            .field("rtmps_url", &self.rtmps_url)
            .field("stream_name", &"[geschützt]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastRessource {
    pub id: String,
    pub titel: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub life_cycle_status: Option<String>,
    pub privacy_status: Option<String>,
    pub bound_stream_id: Option<String>,
    pub enable_auto_start: Option<bool>,
    pub enable_auto_stop: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct BroadcastWunsch {
    pub titel: String,
    pub scheduled_start: DateTime<Utc>,
    pub sichtbarkeit: Sichtbarkeit,
    pub auto_start: bool,
    pub auto_stop: bool,
}

pub trait LiveApi: Send + Sync {
    fn stream_anlegen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        titel: &str,
    ) -> BoxFuture<'a, Result<StreamRessource, ApiFehler>>;
    fn stream_lesen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<Option<StreamRessource>, ApiFehler>>;
    fn streams_eigene<'a>(
        &self,
        token: &'a Zeroizing<String>,
    ) -> BoxFuture<'a, Result<Vec<StreamRessource>, ApiFehler>>;
    fn broadcast_anlegen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        wunsch: &BroadcastWunsch,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>>;
    fn broadcast_lesen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        broadcast_id: &str,
    ) -> BoxFuture<'a, Result<Option<BroadcastRessource>, ApiFehler>>;
    fn broadcasts_eigene<'a>(
        &self,
        token: &'a Zeroizing<String>,
        status: &str,
    ) -> BoxFuture<'a, Result<Vec<BroadcastRessource>, ApiFehler>>;
    fn binden<'a>(
        &self,
        token: &'a Zeroizing<String>,
        broadcast_id: &str,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>>;
    fn transition<'a>(
        &self,
        token: &'a Zeroizing<String>,
        broadcast_id: &str,
        ziel: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>>;
}

#[derive(Clone)]
struct Klient {
    http: reqwest::Client,
    basis: String,
}

impl Klient {
    async fn anfrage(
        &self,
        method: Method,
        pfad: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        token: &Zeroizing<String>,
        ist_schreiben: bool,
    ) -> Result<Value, ApiFehler> {
        let mut bearer = Zeroizing::new(String::with_capacity(7 + token.len()));
        bearer.push_str("Bearer ");
        bearer.push_str(token);
        let mut kopf = HeaderValue::from_str(&bearer)
            .map_err(|_| ApiFehler::Transport("Tokenkopf ist ungültig".into()))?;
        kopf.set_sensitive(true);
        let mut bau = self
            .http
            .request(method, format!("{}{pfad}", self.basis))
            .header("Authorization", kopf)
            .query(query);
        if let Some(body) = &body {
            bau = bau.json(body);
        }
        let antwort = match bau.send().await {
            Ok(a) => a,
            Err(_) => {
                return Err(unklar_oder_transport(
                    ist_schreiben,
                    "Verbindung fehlgeschlagen",
                ));
            }
        };
        let status = antwort.status().as_u16();
        let deckel = if ist_schreiben { MAX_POST } else { MAX_GET };
        let rohdaten = match koerper_lesen(antwort, deckel).await {
            Ok(daten) => daten,
            Err(_) => {
                return Err(unklar_oder_transport(
                    ist_schreiben,
                    "Antwort ist nicht lesbar",
                ));
            }
        };
        if (200..300).contains(&status) {
            if rohdaten.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_slice(&rohdaten)
                .map_err(|_| unklar_oder_transport(ist_schreiben, "Antwort ist nicht lesbar"));
        }
        let grund = serde_json::from_slice::<Value>(&rohdaten)
            .ok()
            .as_ref()
            .and_then(|v| v.pointer("/error/errors/0/reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Err(fehler_aus_status(status, grund.as_deref(), ist_schreiben))
    }

    async fn seiten_sammeln(
        &self,
        pfad: &str,
        basis_query: &[(&str, &str)],
        token: &Zeroizing<String>,
    ) -> Result<Vec<Value>, ApiFehler> {
        let mut gesammelt = Vec::new();
        let mut seite: Option<String> = None;
        for _ in 0..MAX_SEITEN {
            let mut query = basis_query.to_vec();
            if let Some(page) = &seite {
                query.push(("pageToken", page));
            }
            let antwort = self
                .anfrage(Method::GET, pfad, &query, None, token, false)
                .await?;
            if let Some(items) = antwort.pointer("/items").and_then(Value::as_array) {
                gesammelt.extend(items.iter().cloned());
            }
            match antwort
                .pointer("/nextPageToken")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                Some(naechste) => seite = Some(naechste.to_owned()),
                None => return Ok(gesammelt),
            }
        }
        Err(ApiFehler::ZuVieleSeiten)
    }
}

pub struct GoogleLiveApi {
    klient: Klient,
}

fn basis_pruefen(basis: &str) -> Result<(), &'static str> {
    let url = reqwest::Url::parse(basis).map_err(|_| "Basis-URL ist ungültig.")?;
    if url.scheme() != "https" {
        return Err("Basis-URL muss https verwenden.");
    }
    match url.host_str() {
        Some(host) if host == "www.googleapis.com" || host.ends_with(".googleapis.com") => Ok(()),
        _ => Err("Basis-URL muss auf googleapis.com zeigen."),
    }
}

impl GoogleLiveApi {
    pub fn neu() -> Self {
        Self::mit_basis(STANDARD_BASIS).expect("Standardbasis ist gültig")
    }

    pub fn mit_basis(basis: impl Into<String>) -> Result<Self, &'static str> {
        Self::mit_basis_und_frist(basis, STANDARD_FRIST)
    }

    pub fn mit_basis_und_frist(
        basis: impl Into<String>,
        frist: Duration,
    ) -> Result<Self, &'static str> {
        let basis = basis.into();
        basis_pruefen(&basis)?;
        Ok(Self::bauen(basis, frist))
    }

    #[doc(hidden)]
    pub fn mit_basis_ungeprueft(basis: impl Into<String>, frist: Duration) -> Self {
        Self::bauen(basis.into(), frist)
    }

    fn bauen(basis: String, frist: Duration) -> Self {
        Self {
            klient: Klient {
                basis,
                http: reqwest::Client::builder()
                    .no_proxy()
                    .timeout(frist)
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("reqwest-Client ohne Sonderoptionen"),
            },
        }
    }
}

fn unklar_oder_transport(ist_schreiben: bool, meldung: &str) -> ApiFehler {
    if ist_schreiben {
        ApiFehler::Unklar
    } else {
        ApiFehler::Transport(format!("YouTube ist nicht erreichbar: {meldung}"))
    }
}

fn fehler_aus_status(status: u16, grund: Option<&str>, ist_schreiben: bool) -> ApiFehler {
    if status == 429 {
        return ApiFehler::Ratelimit;
    }
    if status == 401 {
        return ApiFehler::NeuAnmeldungNoetig;
    }
    if status == 404 {
        return ApiFehler::NichtGefunden;
    }
    if status == 403 {
        return match grund {
            Some("quotaExceeded") | Some("dailyLimitExceeded") => ApiFehler::Quota,
            Some("rateLimitExceeded") | Some("userRequestsExceedRateLimit") => ApiFehler::Ratelimit,
            Some("liveStreamingNotEnabled") => ApiFehler::LiveNichtFreigeschaltet,
            _ => ApiFehler::RechteFehlen,
        };
    }
    if status == 400 {
        return ApiFehler::Ungueltig(grund.unwrap_or("badRequest").to_owned());
    }
    unklar_oder_transport(ist_schreiben, &format!("HTTP {status}"))
}

async fn koerper_lesen(mut antwort: Response, deckel: usize) -> Result<Vec<u8>, ()> {
    if antwort.content_length().is_some_and(|n| n > deckel as u64) {
        return Err(());
    }
    let mut daten = Vec::new();
    while let Some(brocken) = antwort.chunk().await.map_err(|_| ())? {
        if daten.len().saturating_add(brocken.len()) > deckel {
            return Err(());
        }
        daten.extend_from_slice(&brocken);
    }
    Ok(daten)
}

fn zeit(v: Option<&str>) -> Option<DateTime<Utc>> {
    v.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn stream_aus(item: &Value) -> Option<StreamRessource> {
    let id = item.get("id").and_then(Value::as_str)?.to_owned();
    Some(StreamRessource {
        id,
        channel_id: item
            .pointer("/snippet/channelId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        titel: item
            .pointer("/snippet/title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        published_at: zeit(item.pointer("/snippet/publishedAt").and_then(Value::as_str)),
        ingestion_type: item
            .pointer("/cdn/ingestionType")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stream_status: item
            .pointer("/status/streamStatus")
            .and_then(Value::as_str)
            .map(str::to_owned),
        rtmps_url: item
            .pointer("/cdn/ingestionInfo/rtmpsIngestionAddress")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stream_name: Zeroizing::new(
            item.pointer("/cdn/ingestionInfo/streamName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
    })
}

fn broadcast_aus(item: &Value) -> Option<BroadcastRessource> {
    let id = item.get("id").and_then(Value::as_str)?.to_owned();
    Some(BroadcastRessource {
        id,
        titel: item
            .pointer("/snippet/title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        published_at: zeit(item.pointer("/snippet/publishedAt").and_then(Value::as_str)),
        life_cycle_status: item
            .pointer("/status/lifeCycleStatus")
            .and_then(Value::as_str)
            .map(str::to_owned),
        privacy_status: item
            .pointer("/status/privacyStatus")
            .and_then(Value::as_str)
            .map(str::to_owned),
        bound_stream_id: item
            .pointer("/contentDetails/boundStreamId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        enable_auto_start: item
            .pointer("/contentDetails/enableAutoStart")
            .and_then(Value::as_bool),
        enable_auto_stop: item
            .pointer("/contentDetails/enableAutoStop")
            .and_then(Value::as_bool),
    })
}

fn erstes_item<T>(v: &Value, wandeln: impl Fn(&Value) -> Option<T>) -> Option<T> {
    v.pointer("/items")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find_map(wandeln))
}

impl LiveApi for GoogleLiveApi {
    fn stream_anlegen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        titel: &str,
    ) -> BoxFuture<'a, Result<StreamRessource, ApiFehler>> {
        let klient = self.klient.clone();
        let titel = titel.to_owned();
        Box::pin(async move {
            let body = json!({
                "snippet": { "title": titel },
                "cdn": {
                    "frameRate": "variable",
                    "ingestionType": "rtmp",
                    "resolution": "variable"
                },
                "contentDetails": { "isReusable": true }
            });
            let v = klient
                .anfrage(
                    Method::POST,
                    "/liveStreams",
                    &[("part", "snippet,cdn,contentDetails,status")],
                    Some(body),
                    token,
                    true,
                )
                .await?;
            stream_aus(&v).ok_or(ApiFehler::Unklar)
        })
    }

    fn stream_lesen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<Option<StreamRessource>, ApiFehler>> {
        let klient = self.klient.clone();
        let stream_id = stream_id.to_owned();
        Box::pin(async move {
            let v = match klient
                .anfrage(
                    Method::GET,
                    "/liveStreams",
                    &[("part", "id,snippet,cdn,status"), ("id", &stream_id)],
                    None,
                    token,
                    false,
                )
                .await
            {
                Ok(v) => v,
                Err(ApiFehler::NichtGefunden) => return Ok(None),
                Err(e) => return Err(e),
            };
            Ok(erstes_item(&v, stream_aus))
        })
    }

    fn streams_eigene<'a>(
        &self,
        token: &'a Zeroizing<String>,
    ) -> BoxFuture<'a, Result<Vec<StreamRessource>, ApiFehler>> {
        let klient = self.klient.clone();
        Box::pin(async move {
            let items = klient
                .seiten_sammeln(
                    "/liveStreams",
                    &[
                        ("part", "id,snippet,cdn,status"),
                        ("mine", "true"),
                        ("maxResults", "50"),
                    ],
                    token,
                )
                .await?;
            Ok(items.iter().filter_map(stream_aus).collect())
        })
    }

    fn broadcast_anlegen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        wunsch: &BroadcastWunsch,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>> {
        let klient = self.klient.clone();
        let wunsch = wunsch.clone();
        Box::pin(async move {
            let body = json!({
                "snippet": {
                    "title": wunsch.titel,
                    "scheduledStartTime": wunsch.scheduled_start.to_rfc3339()
                },
                "status": { "privacyStatus": wunsch.sichtbarkeit.as_str() },
                "contentDetails": {
                    "enableAutoStart": wunsch.auto_start,
                    "enableAutoStop": wunsch.auto_stop,
                    "monitorStream": { "enableMonitorStream": false }
                }
            });
            let v = klient
                .anfrage(
                    Method::POST,
                    "/liveBroadcasts",
                    &[("part", "snippet,status,contentDetails")],
                    Some(body),
                    token,
                    true,
                )
                .await?;
            broadcast_aus(&v).ok_or(ApiFehler::Unklar)
        })
    }

    fn broadcast_lesen<'a>(
        &self,
        token: &'a Zeroizing<String>,
        broadcast_id: &str,
    ) -> BoxFuture<'a, Result<Option<BroadcastRessource>, ApiFehler>> {
        let klient = self.klient.clone();
        let broadcast_id = broadcast_id.to_owned();
        Box::pin(async move {
            let v = match klient
                .anfrage(
                    Method::GET,
                    "/liveBroadcasts",
                    &[
                        ("part", "id,snippet,status,contentDetails"),
                        ("id", &broadcast_id),
                    ],
                    None,
                    token,
                    false,
                )
                .await
            {
                Ok(v) => v,
                Err(ApiFehler::NichtGefunden) => return Ok(None),
                Err(e) => return Err(e),
            };
            Ok(erstes_item(&v, broadcast_aus))
        })
    }

    fn broadcasts_eigene<'a>(
        &self,
        token: &'a Zeroizing<String>,
        status: &str,
    ) -> BoxFuture<'a, Result<Vec<BroadcastRessource>, ApiFehler>> {
        let klient = self.klient.clone();
        let status = status.to_owned();
        Box::pin(async move {
            let items = klient
                .seiten_sammeln(
                    "/liveBroadcasts",
                    &[
                        ("part", "id,snippet,status,contentDetails"),
                        ("mine", "true"),
                        ("broadcastStatus", &status),
                        ("maxResults", "50"),
                    ],
                    token,
                )
                .await?;
            Ok(items.iter().filter_map(broadcast_aus).collect())
        })
    }

    fn binden<'a>(
        &self,
        token: &'a Zeroizing<String>,
        broadcast_id: &str,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>> {
        let klient = self.klient.clone();
        let broadcast_id = broadcast_id.to_owned();
        let stream_id = stream_id.to_owned();
        Box::pin(async move {
            let v = klient
                .anfrage(
                    Method::POST,
                    "/liveBroadcasts/bind",
                    &[
                        ("id", &broadcast_id),
                        ("streamId", &stream_id),
                        ("part", "id,contentDetails"),
                    ],
                    None,
                    token,
                    true,
                )
                .await?;
            broadcast_aus(&v).ok_or(ApiFehler::Unklar)
        })
    }

    fn transition<'a>(
        &self,
        token: &'a Zeroizing<String>,
        broadcast_id: &str,
        ziel: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>> {
        let klient = self.klient.clone();
        let broadcast_id = broadcast_id.to_owned();
        let ziel = ziel.to_owned();
        Box::pin(async move {
            let v = klient
                .anfrage(
                    Method::POST,
                    "/liveBroadcasts/transition",
                    &[
                        ("id", &broadcast_id),
                        ("broadcastStatus", &ziel),
                        ("part", "id,status"),
                    ],
                    None,
                    token,
                    true,
                )
                .await?;
            broadcast_aus(&v).ok_or(ApiFehler::Unklar)
        })
    }
}
