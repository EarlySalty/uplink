use crate::fehler::ApiFehler;
use crate::model::Sichtbarkeit;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reqwest::{Method, Response};
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;
use zeroize::Zeroizing;

pub const STANDARD_BASIS: &str = "https://www.googleapis.com/youtube/v3";
const STANDARD_FRIST: Duration = Duration::from_secs(10);
const MAX_KOERPER: usize = 256 * 1024;

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
        &'a self,
        token: &str,
        titel: &str,
    ) -> BoxFuture<'a, Result<StreamRessource, ApiFehler>>;
    fn stream_lesen<'a>(
        &'a self,
        token: &str,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<Option<StreamRessource>, ApiFehler>>;
    fn streams_eigene<'a>(
        &'a self,
        token: &str,
    ) -> BoxFuture<'a, Result<Vec<StreamRessource>, ApiFehler>>;
    fn broadcast_anlegen<'a>(
        &'a self,
        token: &str,
        wunsch: &BroadcastWunsch,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>>;
    fn broadcast_lesen<'a>(
        &'a self,
        token: &str,
        broadcast_id: &str,
    ) -> BoxFuture<'a, Result<Option<BroadcastRessource>, ApiFehler>>;
    fn broadcasts_eigene<'a>(
        &'a self,
        token: &str,
        status: &str,
    ) -> BoxFuture<'a, Result<Vec<BroadcastRessource>, ApiFehler>>;
    fn binden<'a>(
        &'a self,
        token: &str,
        broadcast_id: &str,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>>;
    fn transition<'a>(
        &'a self,
        token: &str,
        broadcast_id: &str,
        ziel: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>>;
}

pub struct GoogleLiveApi {
    basis: String,
    http: reqwest::Client,
}

impl GoogleLiveApi {
    pub fn neu() -> Self {
        Self::mit_basis(STANDARD_BASIS)
    }

    pub fn mit_basis(basis: impl Into<String>) -> Self {
        Self::mit_basis_und_frist(basis, STANDARD_FRIST)
    }

    pub fn mit_basis_und_frist(basis: impl Into<String>, frist: Duration) -> Self {
        Self {
            basis: basis.into(),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(frist)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest-Client ohne Sonderoptionen"),
        }
    }

    async fn anfrage(
        &self,
        method: Method,
        pfad: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        token: &str,
        ist_schreiben: bool,
    ) -> Result<Value, ApiFehler> {
        let mut bau = self
            .http
            .request(method, format!("{}{pfad}", self.basis))
            .header("Authorization", format!("Bearer {token}"))
            .query(query);
        if let Some(body) = &body {
            bau = bau.json(body);
        }
        let antwort = match bau.send().await {
            Ok(a) => a,
            Err(_) => {
                return Err(if ist_schreiben {
                    ApiFehler::Unklar
                } else {
                    ApiFehler::Transport("Verbindung zu YouTube fehlgeschlagen".into())
                });
            }
        };
        let status = antwort.status().as_u16();
        let rohdaten = koerper_lesen(antwort)
            .await
            .map_err(|_| ApiFehler::Transport("Antwort von YouTube ist nicht lesbar".into()))?;
        if (200..300).contains(&status) {
            if rohdaten.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_slice(&rohdaten)
                .map_err(|_| ApiFehler::Transport("Antwort von YouTube ist nicht lesbar".into()));
        }
        let grund = serde_json::from_slice::<Value>(&rohdaten)
            .ok()
            .as_ref()
            .and_then(|v| v.pointer("/error/errors/0/reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Err(fehler_aus_status(status, grund.as_deref()))
    }
}

fn fehler_aus_status(status: u16, grund: Option<&str>) -> ApiFehler {
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
    if (500..600).contains(&status) {
        return ApiFehler::Transport(format!("YouTube antwortet mit HTTP {status}"));
    }
    ApiFehler::Transport(format!("YouTube antwortet unerwartet mit HTTP {status}"))
}

async fn koerper_lesen(mut antwort: Response) -> Result<Vec<u8>, ()> {
    if antwort
        .content_length()
        .is_some_and(|n| n > MAX_KOERPER as u64)
    {
        return Err(());
    }
    let mut daten = Vec::new();
    while let Some(brocken) = antwort.chunk().await.map_err(|_| ())? {
        if daten.len().saturating_add(brocken.len()) > MAX_KOERPER {
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
            .map(str::to_owned),
        enable_auto_start: item
            .pointer("/contentDetails/enableAutoStart")
            .and_then(Value::as_bool),
        enable_auto_stop: item
            .pointer("/contentDetails/enableAutoStop")
            .and_then(Value::as_bool),
    })
}

fn erste_liste<T>(v: &Value, wandeln: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    v.pointer("/items")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(wandeln).collect())
        .unwrap_or_default()
}

impl LiveApi for GoogleLiveApi {
    fn stream_anlegen<'a>(
        &'a self,
        token: &str,
        titel: &str,
    ) -> BoxFuture<'a, Result<StreamRessource, ApiFehler>> {
        let token = token.to_owned();
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
            let v = self
                .anfrage(
                    Method::POST,
                    "/liveStreams",
                    &[("part", "snippet,cdn,contentDetails,status")],
                    Some(body),
                    &token,
                    true,
                )
                .await?;
            stream_aus(&v)
                .ok_or_else(|| ApiFehler::Transport("YouTube-Antwort ohne Streamdaten".into()))
        })
    }

    fn stream_lesen<'a>(
        &'a self,
        token: &str,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<Option<StreamRessource>, ApiFehler>> {
        let token = token.to_owned();
        let stream_id = stream_id.to_owned();
        Box::pin(async move {
            let v = match self
                .anfrage(
                    Method::GET,
                    "/liveStreams",
                    &[("part", "id,snippet,cdn,status"), ("id", &stream_id)],
                    None,
                    &token,
                    false,
                )
                .await
            {
                Ok(v) => v,
                Err(ApiFehler::NichtGefunden) => return Ok(None),
                Err(e) => return Err(e),
            };
            Ok(erste_liste(&v, stream_aus).into_iter().next())
        })
    }

    fn streams_eigene<'a>(
        &'a self,
        token: &str,
    ) -> BoxFuture<'a, Result<Vec<StreamRessource>, ApiFehler>> {
        let token = token.to_owned();
        Box::pin(async move {
            let v = self
                .anfrage(
                    Method::GET,
                    "/liveStreams",
                    &[
                        ("part", "id,snippet,cdn,status"),
                        ("mine", "true"),
                        ("maxResults", "50"),
                    ],
                    None,
                    &token,
                    false,
                )
                .await?;
            Ok(erste_liste(&v, stream_aus))
        })
    }

    fn broadcast_anlegen<'a>(
        &'a self,
        token: &str,
        wunsch: &BroadcastWunsch,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>> {
        let token = token.to_owned();
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
            let v = self
                .anfrage(
                    Method::POST,
                    "/liveBroadcasts",
                    &[("part", "snippet,status,contentDetails")],
                    Some(body),
                    &token,
                    true,
                )
                .await?;
            broadcast_aus(&v)
                .ok_or_else(|| ApiFehler::Transport("YouTube-Antwort ohne Broadcastdaten".into()))
        })
    }

    fn broadcast_lesen<'a>(
        &'a self,
        token: &str,
        broadcast_id: &str,
    ) -> BoxFuture<'a, Result<Option<BroadcastRessource>, ApiFehler>> {
        let token = token.to_owned();
        let broadcast_id = broadcast_id.to_owned();
        Box::pin(async move {
            let v = match self
                .anfrage(
                    Method::GET,
                    "/liveBroadcasts",
                    &[
                        ("part", "id,snippet,status,contentDetails"),
                        ("id", &broadcast_id),
                    ],
                    None,
                    &token,
                    false,
                )
                .await
            {
                Ok(v) => v,
                Err(ApiFehler::NichtGefunden) => return Ok(None),
                Err(e) => return Err(e),
            };
            Ok(erste_liste(&v, broadcast_aus).into_iter().next())
        })
    }

    fn broadcasts_eigene<'a>(
        &'a self,
        token: &str,
        status: &str,
    ) -> BoxFuture<'a, Result<Vec<BroadcastRessource>, ApiFehler>> {
        let token = token.to_owned();
        let status = status.to_owned();
        Box::pin(async move {
            let v = self
                .anfrage(
                    Method::GET,
                    "/liveBroadcasts",
                    &[
                        ("part", "id,snippet,status,contentDetails"),
                        ("mine", "true"),
                        ("broadcastStatus", &status),
                        ("maxResults", "50"),
                    ],
                    None,
                    &token,
                    false,
                )
                .await?;
            Ok(erste_liste(&v, broadcast_aus))
        })
    }

    fn binden<'a>(
        &'a self,
        token: &str,
        broadcast_id: &str,
        stream_id: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>> {
        let token = token.to_owned();
        let broadcast_id = broadcast_id.to_owned();
        let stream_id = stream_id.to_owned();
        Box::pin(async move {
            let v = self
                .anfrage(
                    Method::POST,
                    "/liveBroadcasts/bind",
                    &[
                        ("id", &broadcast_id),
                        ("streamId", &stream_id),
                        ("part", "id,contentDetails"),
                    ],
                    None,
                    &token,
                    true,
                )
                .await?;
            broadcast_aus(&v)
                .ok_or_else(|| ApiFehler::Transport("YouTube-Antwort ohne Bindungsdaten".into()))
        })
    }

    fn transition<'a>(
        &'a self,
        token: &str,
        broadcast_id: &str,
        ziel: &str,
    ) -> BoxFuture<'a, Result<BroadcastRessource, ApiFehler>> {
        let token = token.to_owned();
        let broadcast_id = broadcast_id.to_owned();
        let ziel = ziel.to_owned();
        Box::pin(async move {
            let v = self
                .anfrage(
                    Method::POST,
                    "/liveBroadcasts/transition",
                    &[
                        ("id", &broadcast_id),
                        ("broadcastStatus", &ziel),
                        ("part", "id,status"),
                    ],
                    None,
                    &token,
                    true,
                )
                .await?;
            broadcast_aus(&v)
                .ok_or_else(|| ApiFehler::Transport("YouTube-Antwort ohne Übergangsdaten".into()))
        })
    }
}
