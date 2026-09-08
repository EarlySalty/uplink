use crate::{Error, Grant, Platform, PlatformBroker, Privacy, Result, Secret, Settings};
use chrono::Utc;
use reqwest::{Client, Method, StatusCode, Url};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const API: &str = "https://www.googleapis.com";
const MAX_REPLY: usize = 1024 * 1024;
pub const CHUNK_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadReply {
    Offset(u64),
    Complete(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Processing {
    Pending,
    Succeeded,
}

pub struct YouTube {
    broker: Arc<dyn PlatformBroker>,
    http: Client,
    base: Url,
    test_loopback: bool,
}
impl YouTube {
    pub fn new(broker: Arc<dyn PlatformBroker>) -> Result<Self> {
        Self::build(broker, API, false)
    }
    /// Ausschließlich isolierte Loopback-HTTP-Tests, nie aus einer Nutzeradresse ableiten.
    pub fn for_test(broker: Arc<dyn PlatformBroker>, base: &str) -> Result<Self> {
        Self::build(broker, base, true)
    }
    fn build(broker: Arc<dyn PlatformBroker>, base: &str, test_loopback: bool) -> Result<Self> {
        let base = Url::parse(base).map_err(|_| Error::Invalid)?;
        if test_loopback
            && !(base.scheme() == "http"
                && base
                    .host_str()
                    .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback()))
        {
            return Err(Error::Invalid);
        }
        let http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|_| Error::Invalid)?;
        Ok(Self {
            broker,
            http,
            base,
            test_loopback,
        })
    }
    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base.join(path).map_err(|_| Error::Invalid)
    }
    fn session_url(&self, session: &Secret) -> Result<Url> {
        if session.expose().len() > 8192 {
            return Err(Error::Protocol);
        }
        let url = Url::parse(session.expose()).map_err(|_| Error::Protocol)?;
        if url.origin() != self.base.origin()
            || url.path() != "/upload/youtube/v3/videos"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || (!self.test_loopback && url.scheme() != "https")
            || !url
                .query_pairs()
                .any(|(k, v)| k == "upload_id" && !v.is_empty())
        {
            return Err(Error::Protocol);
        }
        Ok(url)
    }
    async fn grant(&self, streamer: u64, expected: &str, refresh: bool) -> Result<Grant> {
        let grant = tokio::time::timeout(
            Duration::from_secs(10),
            self.broker.grant(streamer, Platform::YouTube, refresh),
        )
        .await
        .map_err(|_| Error::BrokerUnavailable)??;
        if grant.platform_user_id != expected {
            return Err(Error::WrongChannel);
        }
        if grant.expires_at <= Utc::now()
            || grant.access_token.expose().is_empty()
            || !grant.scopes.iter().any(|s| {
                matches!(
                    s.as_str(),
                    "https://www.googleapis.com/auth/youtube"
                        | "https://www.googleapis.com/auth/youtube.upload"
                        | "https://www.googleapis.com/auth/youtube.force-ssl"
                )
            })
        {
            return Err(Error::NeedsReauth);
        }
        Ok(grant)
    }
    async fn request(
        &self,
        streamer: u64,
        expected: &str,
        method: Method,
        url: Url,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let grant = self.grant(streamer, expected, attempt == 1).await?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(grant.access_token.expose());
            for (key, value) in headers {
                request = request.header(*key, value)
            }
            if let Some(ref body) = body {
                request = request.body(body.clone())
            }
            let response = request.send().await.map_err(|_| Error::Network)?;
            if response.status() == StatusCode::UNAUTHORIZED {
                if attempt == 0 {
                    continue;
                } else {
                    return Err(Error::NeedsReauth);
                }
            }
            return Ok(response);
        }
        Err(Error::NeedsReauth)
    }
    async fn json(mut response: reqwest::Response) -> Result<Value> {
        if response
            .content_length()
            .is_some_and(|n| n > MAX_REPLY as u64)
        {
            return Err(Error::Protocol);
        }
        let mut data = Vec::new();
        while let Some(bytes) = response.chunk().await.map_err(|_| Error::Network)? {
            if data.len().saturating_add(bytes.len()) > MAX_REPLY {
                return Err(Error::Protocol);
            }
            data.extend_from_slice(&bytes);
        }
        serde_json::from_slice(&data).map_err(|_| Error::Protocol)
    }
    async fn check(response: reqwest::Response) -> Result<reqwest::Response> {
        if response.status().is_success() || response.status() == StatusCode::PERMANENT_REDIRECT {
            return Ok(response);
        }
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::Quota);
        }
        if status.is_server_error() {
            return Err(Error::Network);
        }
        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
            return Err(Error::Ambiguous);
        }
        if status == StatusCode::FORBIDDEN {
            let body = Self::json(response).await?;
            let quota = body
                .pointer("/error/errors")
                .and_then(Value::as_array)
                .is_some_and(|errors| {
                    errors.iter().any(|e| {
                        matches!(
                            e.get("reason").and_then(Value::as_str),
                            Some(
                                "quotaExceeded"
                                    | "dailyLimitExceeded"
                                    | "rateLimitExceeded"
                                    | "uploadLimitExceeded"
                            )
                        )
                    })
                });
            return Err(if quota {
                Error::Quota
            } else {
                Error::NeedsReauth
            });
        }
        Err(Error::Protocol)
    }
    pub async fn verify_channel(&self, streamer: u64, expected: &str) -> Result<()> {
        let mut url = self.endpoint("/youtube/v3/channels")?;
        url.query_pairs_mut()
            .append_pair("part", "id")
            .append_pair("mine", "true")
            .append_pair("maxResults", "50");
        let response = self
            .request(streamer, expected, Method::GET, url, &[], None)
            .await?;
        let body = Self::json(Self::check(response).await?).await?;
        let ids = body
            .get("items")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        if ids.len() != 1 || ids[0].get("id").and_then(Value::as_str) != Some(expected) {
            return Err(Error::WrongChannel);
        }
        Ok(())
    }
    pub async fn start(&self, streamer: u64, settings: &Settings, total: u64) -> Result<Secret> {
        settings.validate()?;
        if total == 0 {
            return Err(Error::Incomplete);
        }
        self.verify_channel(streamer, &settings.youtube_channel_id)
            .await?;
        let privacy = match settings.privacy {
            Privacy::Private => "private",
            Privacy::Unlisted => "unlisted",
            Privacy::Public => "public",
        };
        let body=serde_json::to_vec(&json!({"snippet":{"title":settings.title,"description":settings.description},"status":{"privacyStatus":privacy}})).map_err(|_|Error::Invalid)?;
        let mut url = self.endpoint("/upload/youtube/v3/videos")?;
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("part", "snippet,status");
        let response = self
            .request(
                streamer,
                &settings.youtube_channel_id,
                Method::POST,
                url,
                &[
                    ("Content-Type", "application/json; charset=UTF-8".into()),
                    ("X-Upload-Content-Type", "video/mp4".into()),
                    ("X-Upload-Content-Length", total.to_string()),
                ],
                Some(body),
            )
            .await?;
        let response = Self::check(response).await?;
        let uri = response
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .ok_or(Error::Ambiguous)?;
        let session = Secret::new(uri.to_owned());
        self.session_url(&session)?;
        Ok(session)
    }
    async fn reply(response: reqwest::Response, total: u64) -> Result<UploadReply> {
        let response = Self::check(response).await?;
        if response.status() == StatusCode::PERMANENT_REDIRECT {
            let Some(header) = response.headers().get("Range") else {
                return Ok(UploadReply::Offset(0));
            };
            let range = header.to_str().map_err(|_| Error::Protocol)?;
            let end = range.strip_prefix("bytes=0-").ok_or(Error::Protocol)?;
            if end.is_empty() || !end.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Protocol);
            }
            let offset = end
                .parse::<u64>()
                .ok()
                .and_then(|n| n.checked_add(1))
                .filter(|n| *n <= total)
                .ok_or(Error::Protocol)?;
            return Ok(UploadReply::Offset(offset));
        }
        let body = Self::json(response).await.map_err(|_| Error::Ambiguous)?;
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_video_id(id))
            .ok_or(Error::Ambiguous)?;
        Ok(UploadReply::Complete(id.into()))
    }
    pub async fn reconcile(
        &self,
        streamer: u64,
        channel: &str,
        session: &Secret,
        total: u64,
    ) -> Result<UploadReply> {
        let url = self.session_url(session)?;
        let response = self
            .request(
                streamer,
                channel,
                Method::PUT,
                url,
                &[
                    ("Content-Length", "0".into()),
                    ("Content-Range", format!("bytes */{total}")),
                ],
                Some(Vec::new()),
            )
            .await?;
        Self::reply(response, total).await
    }
    pub async fn chunk(
        &self,
        streamer: u64,
        channel: &str,
        session: &Secret,
        total: u64,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<UploadReply> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|end| *end <= total)
            .ok_or(Error::Invalid)?;
        if bytes.is_empty()
            || bytes.len() > CHUNK_BYTES
            || (end < total && !bytes.len().is_multiple_of(256 * 1024))
        {
            return Err(Error::Invalid);
        }
        let url = self.session_url(session)?;
        let response = self
            .request(
                streamer,
                channel,
                Method::PUT,
                url,
                &[
                    ("Content-Type", "video/mp4".into()),
                    ("Content-Length", bytes.len().to_string()),
                    (
                        "Content-Range",
                        format!("bytes {offset}-{}/{total}", end - 1),
                    ),
                ],
                Some(bytes),
            )
            .await?;
        let reply = Self::reply(response, total).await?;
        if let UploadReply::Offset(next) = reply
            && (next < offset || next > end)
        {
            return Err(Error::Protocol);
        }
        if reply == UploadReply::Offset(offset) {
            // Kein endloser Chunk-Loop mit erneuerter Lease, wenn nichts angenommen
            // wurde. Der nächste Versuch gleicht die Session erneut ab.
            return Err(Error::Network);
        }
        Ok(reply)
    }
    pub async fn processing(
        &self,
        streamer: u64,
        channel: &str,
        video_id: &str,
    ) -> Result<Processing> {
        if !valid_video_id(video_id) {
            return Err(Error::Invalid);
        }
        let mut url = self.endpoint("/youtube/v3/videos")?;
        url.query_pairs_mut()
            .append_pair("part", "snippet,processingDetails,status")
            .append_pair("id", video_id);
        let response = self
            .request(streamer, channel, Method::GET, url, &[], None)
            .await?;
        let body = Self::json(Self::check(response).await?).await?;
        let items = body
            .get("items")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        if items.len() != 1 || items[0].get("id").and_then(Value::as_str) != Some(video_id) {
            return Err(Error::Ambiguous);
        }
        let item = &items[0];
        if item.pointer("/snippet/channelId").and_then(Value::as_str) != Some(channel) {
            return Err(Error::WrongChannel);
        }
        if matches!(
            item.pointer("/status/uploadStatus").and_then(Value::as_str),
            Some("failed" | "rejected" | "deleted")
        ) {
            return Err(Error::ProcessingFailed);
        }
        match item
            .pointer("/processingDetails/processingStatus")
            .and_then(Value::as_str)
        {
            Some("succeeded") => Ok(Processing::Succeeded),
            Some("processing") => Ok(Processing::Pending),
            Some("failed" | "terminated") => Err(Error::ProcessingFailed),
            _ => Err(Error::Protocol),
        }
    }
}
fn valid_video_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
