use crate::{
    Error, Platform, PlatformBroker, PreparedSource, Result, Source, SourceProvider, SourceRequest,
    Storage, Store, recording, storage::ExportFile,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uplink_media::flv::{FlvReader, HEADER};

#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub yt_dlp: PathBuf,
    pub twitch_client_id: String,
    pub max_source_bytes: u64,
    pub max_export_bytes: u64,
    pub download_bytes_per_second: u64,
    pub operation_timeout: Duration,
    pub twitch_settle_time: Duration,
}
pub struct NativeSources {
    store: Store,
    storage: Storage,
    broker: Arc<dyn PlatformBroker>,
    config: SourceConfig,
    http: reqwest::Client,
    twitch_api: reqwest::Url,
}
impl NativeSources {
    pub fn new(
        store: Store,
        storage: Storage,
        broker: Arc<dyn PlatformBroker>,
        config: SourceConfig,
    ) -> Result<Self> {
        if [&config.ffmpeg, &config.ffprobe, &config.yt_dlp]
            .iter()
            .any(|p| !p.is_absolute() || !p.is_file())
            || config.max_source_bytes == 0
            || config.max_export_bytes == 0
            || config.download_bytes_per_second == 0
            || config.operation_timeout.is_zero()
            || config.operation_timeout > Duration::from_secs(24 * 60 * 60)
        {
            return Err(Error::Invalid);
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| Error::Invalid)?;
        Ok(Self {
            store,
            storage,
            broker,
            config,
            http,
            twitch_api: reqwest::Url::parse("https://api.twitch.tv/helix/")
                .map_err(|_| Error::Invalid)?,
        })
    }
    /// Expliziter isolierter HTTP-Test, niemals eine frei eingegebene Plattformadresse.
    pub fn with_loopback_test_api(mut self, base: &str) -> Result<Self> {
        let url = reqwest::Url::parse(base).map_err(|_| Error::Invalid)?;
        if url.scheme() != "http"
            || !url
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        self.twitch_api = url;
        Ok(self)
    }
    async fn twitch_get(
        &self,
        streamer: u64,
        broadcaster: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value> {
        if self.config.twitch_client_id.is_empty() {
            return Err(Error::Invalid);
        }
        for attempt in 0..2 {
            let grant = tokio::time::timeout(
                Duration::from_secs(10),
                self.broker.grant(streamer, Platform::Twitch, attempt == 1),
            )
            .await
            .map_err(|_| Error::BrokerUnavailable)??;
            if grant.platform_user_id != broadcaster {
                return Err(Error::WrongChannel);
            }
            if grant.expires_at <= Utc::now() {
                return Err(Error::NeedsReauth);
            }
            let mut url = self.twitch_api.join(path).map_err(|_| Error::Invalid)?;
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
            let mut response = self
                .http
                .get(url)
                .bearer_auth(grant.access_token.expose())
                .header("Client-Id", &self.config.twitch_client_id)
                .send()
                .await
                .map_err(|_| Error::Network)?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                if attempt == 0 {
                    continue;
                } else {
                    return Err(Error::NeedsReauth);
                }
            }
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(Error::Quota);
            }
            if !response.status().is_success() {
                return Err(Error::SourceUnavailable);
            }
            let mut bytes = Vec::new();
            while let Some(part) = response.chunk().await.map_err(|_| Error::Network)? {
                if bytes.len() + part.len() > 1024 * 1024 {
                    return Err(Error::Protocol);
                }
                bytes.extend_from_slice(&part)
            }
            return serde_json::from_slice(&bytes).map_err(|_| Error::Protocol);
        }
        Err(Error::NeedsReauth)
    }
    async fn twitch_video(&self, request: &SourceRequest) -> Result<Value> {
        let binding = request
            .twitch_binding
            .as_ref()
            .ok_or(Error::UnboundTwitch)?;
        binding.validate()?;
        let live = self
            .twitch_get(
                request.streamer_id,
                &binding.broadcaster_id,
                "streams",
                &[("user_id", binding.broadcaster_id.clone())],
            )
            .await?;
        let live = live
            .get("data")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        if live
            .iter()
            .any(|s| s.get("id").and_then(Value::as_str) == Some(&binding.stream_id))
        {
            return Err(Error::SourcePending);
        }
        let mut cursor = None;
        let mut matched = None;
        for _ in 0..20 {
            let mut query = vec![
                ("user_id", binding.broadcaster_id.clone()),
                ("type", "archive".into()),
                ("first", "100".into()),
            ];
            if let Some(cursor) = cursor {
                query.push(("after", cursor))
            }
            let reply = self
                .twitch_get(
                    request.streamer_id,
                    &binding.broadcaster_id,
                    "videos",
                    &query,
                )
                .await?;
            for video in reply
                .get("data")
                .and_then(Value::as_array)
                .ok_or(Error::Protocol)?
            {
                if video.get("stream_id").and_then(Value::as_str) == Some(&binding.stream_id) {
                    if video.get("user_id").and_then(Value::as_str) != Some(&binding.broadcaster_id)
                        || video.get("type").and_then(Value::as_str) != Some("archive")
                    {
                        return Err(Error::WrongChannel);
                    }
                    if matched.is_some() {
                        return Err(Error::UnboundTwitch);
                    }
                    matched = Some(video.clone());
                }
            }
            cursor = reply
                .pointer("/pagination/cursor")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                return matched.ok_or(Error::SourcePending);
            }
        }
        Err(Error::SourcePending)
    }
    async fn download(&self, object: &str, vod_id: &str) -> Result<()> {
        if vod_id.is_empty() || vod_id.len() > 32 || !vod_id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::UnboundTwitch);
        }
        if self.storage.contains(object, "download.ts") {
            return Ok(());
        }
        let mut command = tokio::process::Command::new(&self.config.yt_dlp);
        command
            .args([
                "--ignore-config",
                "--no-cache-dir",
                "--no-playlist",
                "--no-progress",
                "--no-warnings",
                "--no-check-updates",
                "--hls-prefer-native",
                "--hls-use-mpegts",
                "--no-part",
                "--no-keep-fragments",
                "--concurrent-fragments",
                "1",
                "--retries",
                "2",
                "--fragment-retries",
                "2",
                "--limit-rate",
            ])
            .arg(self.config.download_bytes_per_second.to_string())
            .args(["-f", "best", "-o", "-"])
            .arg(format!("https://www.twitch.tv/videos/{vod_id}"));
        capture(
            &self.storage,
            object,
            "download.ts",
            command,
            None,
            self.config.max_source_bytes,
            self.config.operation_timeout,
        )
        .await
        .map_err(|e| {
            if e == Error::StorageFull {
                e
            } else {
                Error::SourceUnavailable
            }
        })?;
        Ok(())
    }
    async fn remux(&self, object: &str, input: Input) -> Result<ExportFile> {
        if !self.storage.contains(object, "export.mp4") {
            let mut command = tokio::process::Command::new(&self.config.ffmpeg);
            command.args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-max_alloc",
                "67108864",
                "-threads",
                "2",
                "-protocol_whitelist",
                "pipe",
                "-i",
                "pipe:0",
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-c",
                "copy",
                "-movflags",
                "+frag_keyframe+empty_moov+default_base_moof",
                "-f",
                "mp4",
                "pipe:1",
            ]);
            capture(
                &self.storage,
                object,
                "export.mp4",
                command,
                Some(input),
                self.config.max_export_bytes,
                self.config.operation_timeout,
            )
            .await?;
        }
        let info = self.probe(object, "export.mp4").await?;
        validate_single_program(&info)?;
        self.storage.digest(object, "export.mp4").await
    }
    async fn probe(&self, object: &str, name: &str) -> Result<Value> {
        let mut child = tokio::process::Command::new(&self.config.ffprobe)
            .env_clear()
            .args([
                "-v",
                "error",
                "-max_alloc",
                "67108864",
                "-show_entries",
                "format=duration:stream=codec_type,codec_name",
                "-of",
                "json",
                "-protocol_whitelist",
                "file",
            ])
            .arg(self.storage.path(object, name)?)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| Error::Storage)?;
        let mut stdout = child.stdout.take().ok_or(Error::Storage)?;
        let mut bytes = vec![];
        let operation = async {
            (&mut stdout)
                .take(65537)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| Error::Storage)?;
            drop(stdout);
            let status = child.wait().await.map_err(|_| Error::Storage)?;
            if !status.success() || bytes.len() > 65536 {
                return Err(Error::Incomplete);
            }
            serde_json::from_slice(&bytes).map_err(|_| Error::Incomplete)
        };
        match tokio::time::timeout(Duration::from_secs(30), operation).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(Error::Incomplete)
            }
        }
    }
}
#[async_trait]
impl SourceProvider for NativeSources {
    async fn prepare(&self, request: SourceRequest) -> Result<PreparedSource> {
        let _exclusive = request
            .object_id
            .as_deref()
            .map(|id| self.storage.lock_preparation(id))
            .transpose()?;
        match request.settings.source {
            Source::InputRecording => {
                let manifest: recording::RecordingManifest =
                    serde_json::from_value(request.manifest.ok_or(Error::SourcePending)?)
                        .map_err(|_| Error::Incomplete)?;
                if manifest.spec.session_id != request.session_id
                    || manifest.spec.streamer_id != request.streamer_id
                    || request.object_id.as_deref() != Some(&manifest.object_id)
                {
                    return Err(Error::WrongChannel);
                }
                if !manifest.complete {
                    return Err(manifest.error.unwrap_or(Error::Incomplete));
                }
                manifest.verify_segments(&self.storage).await?;
                let video_ids: std::collections::BTreeSet<_> = manifest
                    .tracks
                    .iter()
                    .filter(|track| track.kind == "video")
                    .map(|track| track.wire_id)
                    .collect();
                if video_ids.len() != 1 {
                    return Err(Error::ChangedProfile);
                }
                if request.settings.vod_audio_track != manifest.spec.vod_audio_track
                    || !manifest.has_vod_audio()
                {
                    return Err(Error::MissingVodAudio);
                }
                let track = request
                    .settings
                    .vod_audio_track
                    .ok_or(Error::MissingVodAudio)?;
                let export = self
                    .remux(
                        &manifest.object_id,
                        Input::Recording {
                            storage: self.storage.clone(),
                            object: manifest.object_id.clone(),
                            segments: manifest.segments.iter().map(|s| s.file.clone()).collect(),
                            audio: track,
                        },
                    )
                    .await?;
                Ok(PreparedSource {
                    object_id: manifest.object_id.clone(),
                    manifest: serde_json::to_value(manifest).map_err(|_| Error::Incomplete)?,
                    export,
                })
            }
            Source::TwitchVod => {
                let video = self.twitch_video(&request).await?;
                let vod_id = video
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(Error::UnboundTwitch)?;
                if video.get("viewable").and_then(Value::as_str) != Some("public") {
                    return Err(Error::SourceUnavailable);
                }
                let duration = parse_duration(
                    video
                        .get("duration")
                        .and_then(Value::as_str)
                        .ok_or(Error::Protocol)?,
                )?;
                let object = match request.object_id.clone() {
                    Some(id) => id,
                    None => self.storage.create_object()?,
                };
                let mut manifest = request
                    .manifest
                    .clone()
                    .unwrap_or_else(|| json!({"source":"twitch_vod"}));
                let previous_observation = manifest.get("observation").cloned();
                let stable = observe_vod(
                    &mut manifest,
                    vod_id,
                    duration,
                    Utc::now().timestamp(),
                    self.config.twitch_settle_time.as_secs(),
                );
                if !stable {
                    if previous_observation != manifest.get("observation").cloned() {
                        self.storage.retire_download(&object).await?;
                    }
                    manifest["binding"] = serde_json::to_value(&request.twitch_binding)
                        .map_err(|_| Error::Invalid)?;
                    self.store
                        .register_object(
                            request.session_id,
                            request.streamer_id,
                            &object,
                            &manifest,
                            false,
                        )
                        .await?;
                    return Err(Error::SourcePending);
                }
                self.download(&object, vod_id).await?;
                let info = self.probe(&object, "download.ts").await?;
                // Die Plattformquelle hat keine von uns bestätigte Mehrspurrolle.
                // Darum niemals aus mehreren Mischungen blind die erste wählen.
                validate_single_program(&info)?;
                let measured = info
                    .pointer("/format/duration")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|d| d.is_finite() && *d > 0.0)
                    .ok_or(Error::Incomplete)?;
                if (measured - duration as f64).abs() > 3.0 {
                    self.storage.retire_download(&object).await?;
                    return Err(Error::SourcePending);
                }
                // Nach dem Download nochmals binden und Dauer prüfen; ein noch wachsendes VOD bleibt offen.
                let current = self
                    .twitch_video(&SourceRequest {
                        object_id: Some(object.clone()),
                        manifest: None,
                        ..request.clone()
                    })
                    .await?;
                if current.get("id").and_then(Value::as_str) != Some(vod_id)
                    || parse_duration(
                        current
                            .get("duration")
                            .and_then(Value::as_str)
                            .ok_or(Error::Protocol)?,
                    )? != duration
                {
                    return Err(Error::SourcePending);
                }
                let export = self
                    .remux(
                        &object,
                        Input::File(self.storage.path(&object, "download.ts")?),
                    )
                    .await?;
                manifest["complete"] = json!(true);
                manifest["platform_audio"] = json!("confirmed_vod_mix");
                Ok(PreparedSource {
                    object_id: object,
                    manifest,
                    export,
                })
            }
        }
    }
}
fn validate_single_program(info: &Value) -> Result<()> {
    let streams = info
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(Error::Incomplete)?;
    if streams
        .iter()
        .filter(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"))
        .count()
        != 1
    {
        return Err(Error::MissingVodAudio);
    }
    if streams
        .iter()
        .filter(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
        .count()
        != 1
    {
        return Err(Error::ChangedProfile);
    }
    Ok(())
}
pub fn parse_duration(value: &str) -> Result<u64> {
    if value.is_empty() || value.len() > 32 {
        return Err(Error::Protocol);
    }
    let mut number = 0u64;
    let mut total = 0u64;
    let mut digits = false;
    for c in value.bytes() {
        if c.is_ascii_digit() {
            number = number
                .checked_mul(10)
                .and_then(|n| n.checked_add((c - b'0') as u64))
                .ok_or(Error::Protocol)?;
            digits = true
        } else {
            let factor = match c {
                b'h' => 3600,
                b'm' => 60,
                b's' => 1,
                _ => return Err(Error::Protocol),
            };
            if !digits {
                return Err(Error::Protocol);
            }
            total = total
                .checked_add(number.checked_mul(factor).ok_or(Error::Protocol)?)
                .ok_or(Error::Protocol)?;
            number = 0;
            digits = false
        }
    }
    if digits || total == 0 {
        return Err(Error::Protocol);
    }
    Ok(total)
}
enum Input {
    File(PathBuf),
    Recording {
        storage: Storage,
        object: String,
        segments: Vec<String>,
        audio: u8,
    },
}
async fn feed(input: Input, mut stdin: tokio::process::ChildStdin) -> Result<()> {
    match input {
        Input::File(path) => {
            let mut file = tokio::fs::File::open(path)
                .await
                .map_err(|_| Error::Storage)?;
            tokio::io::copy(&mut file, &mut stdin)
                .await
                .map_err(|_| Error::Storage)?;
        }
        Input::Recording {
            storage,
            object,
            segments,
            audio,
        } => {
            stdin.write_all(HEADER).await.map_err(|_| Error::Storage)?;
            let mut headers = std::collections::BTreeMap::new();
            for segment in segments {
                let mut reader =
                    FlvReader::new(storage.open(&object, &segment).await?, 16 * 1024 * 1024);
                while let Some(tag) = reader.next().await.map_err(|_| Error::Incomplete)? {
                    if tag.kind() == 8 {
                        if tag.audio_track().map_err(|_| Error::MissingVodAudio)? != Some(audio) {
                            continue;
                        }
                        ensure_header(&mut headers, &tag)?;
                        tag.with_audio_track(0, 16 * 1024 * 1024)
                            .map_err(|_| Error::MissingVodAudio)?
                            .write_to(&mut stdin)
                            .await
                            .map_err(|_| Error::Storage)?;
                    } else {
                        ensure_header(&mut headers, &tag)?;
                        tag.write_to(&mut stdin).await.map_err(|_| Error::Storage)?
                    }
                }
            }
        }
    }
    stdin.shutdown().await.map_err(|_| Error::Storage)?;
    Ok(())
}
async fn capture(
    storage: &Storage,
    object: &str,
    name: &str,
    mut command: tokio::process::Command,
    input: Option<Input>,
    max_bytes: u64,
    limit: Duration,
) -> Result<()> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
    let partial = format!("{}-{}.part", name, hex::encode(random));
    let path = storage.path(object, &partial)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await
        .map_err(|_| Error::Storage)?;
    command
        .env_clear()
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| Error::SourceUnavailable)?;
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().ok_or(Error::Storage)?;
    let write = async {
        let mut total = 0;
        let mut buffer = vec![0; 65536];
        loop {
            let len = stdout.read(&mut buffer).await.map_err(|_| Error::Storage)?;
            if len == 0 {
                break;
            }
            total += len as u64;
            if total > max_bytes {
                return Err(Error::StorageFull);
            }
            let reservation = storage.reserve(len as u64)?;
            reservation.commit();
            file.write_all(&buffer[..len])
                .await
                .map_err(|_| Error::Storage)?;
        }
        file.sync_all().await.map_err(|_| Error::Storage)?;
        if total == 0 {
            return Err(Error::Incomplete);
        }
        Ok(())
    };
    let send = async {
        if let Some(input) = input {
            feed(input, stdin.ok_or(Error::Storage)?).await
        } else {
            Ok(())
        }
    };
    let operation = async {
        let (sent, written) = tokio::try_join!(send, write)?;
        let status = child.wait().await.map_err(|_| Error::Storage)?;
        if !status.success() {
            return Err(Error::Incomplete);
        }
        let _ = (sent, written);
        Ok(())
    };
    let result = match tokio::time::timeout(limit, operation).await {
        Ok(result) => result,
        Err(_) => Err(Error::SourceUnavailable),
    };
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return result;
    }
    drop(file);
    tokio::fs::rename(path, storage.path(object, name)?)
        .await
        .map_err(|_| Error::Storage)?;
    Ok(())
}

fn observe_vod(manifest: &mut Value, vod_id: &str, duration: u64, now: i64, wait: u64) -> bool {
    let previous = manifest.get("observation");
    let unchanged = previous.is_some_and(|p| {
        p.get("vod_id").and_then(Value::as_str) == Some(vod_id)
            && p.get("duration_seconds").and_then(Value::as_u64) == Some(duration)
    });
    if unchanged {
        return previous
            .and_then(|p| p.get("observed_at"))
            .and_then(Value::as_i64)
            .is_some_and(|time| now >= time && now.saturating_sub(time) as u64 >= wait);
    }
    manifest["observation"] =
        json!({"vod_id":vod_id,"duration_seconds":duration,"observed_at":now});
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn abbruch_beendet_den_echten_werkzeugprozess_und_reapt_ihn() {
        let directory = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let storage = Storage::new(directory.path().to_owned(), 4096).unwrap();
        let object = storage.create_object().unwrap();
        let task_storage = storage.clone();
        let task_object = object.clone();
        let task = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", "printf '%s\\n' \"$$\"; exec /bin/sleep 30"]);
            capture(
                &task_storage,
                &task_object,
                "test.mp4",
                command,
                None,
                4096,
                Duration::from_secs(40),
            )
            .await
        });
        let pid = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let mut entries = tokio::fs::read_dir(storage.object_dir(&object).unwrap())
                    .await
                    .unwrap();
                while let Some(entry) = entries.next_entry().await.unwrap() {
                    if let Ok(bytes) = tokio::fs::read_to_string(entry.path()).await
                        && let Ok(pid) = bytes.trim().parse::<u32>()
                    {
                        return pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(3), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Abgebrochenes Werkzeug muss beendet und abgeholt sein");
        assert!(!storage.contains(&object, "test.mp4"));
    }
    #[test]
    fn wiederholte_pruefung_startet_die_stabilitaetsfrist_nicht_neu() {
        let mut manifest = json!({});
        assert!(!observe_vod(&mut manifest, "123", 500, 100, 60));
        assert!(!observe_vod(&mut manifest, "123", 500, 130, 60));
        assert_eq!(manifest["observation"]["observed_at"], 100);
        assert!(observe_vod(&mut manifest, "123", 500, 160, 60));
        assert!(!observe_vod(&mut manifest, "123", 501, 161, 60));
        assert_eq!(manifest["observation"]["observed_at"], 161);
    }
}

#[cfg(test)]
mod twitch_contract_tests {
    use super::*;
    use crate::{Grant, Privacy, Secret, TwitchBinding};
    #[test]
    fn unbekannte_mehrspurquelle_waehlt_keinen_mix_still_aus() {
        assert_eq!(
            validate_single_program(
                &json!({"streams":[{"codec_type":"video"},{"codec_type":"audio"},{"codec_type":"audio"}]})
            ),
            Err(Error::MissingVodAudio)
        );
        assert_eq!(
            validate_single_program(
                &json!({"streams":[{"codec_type":"video"},{"codec_type":"video"},{"codec_type":"audio"}]})
            ),
            Err(Error::ChangedProfile)
        );
        assert!(
            validate_single_program(
                &json!({"streams":[{"codec_type":"video"},{"codec_type":"audio"}]})
            )
            .is_ok()
        );
    }
    use tokio_postgres::{Row, types::ToSql};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };
    struct NoDb;
    #[async_trait]
    impl crate::Database for NoDb {
        async fn query(&self, _: &str, _: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
            Err(Error::Database)
        }
    }
    struct Broker;
    #[async_trait]
    impl PlatformBroker for Broker {
        async fn grant(&self, _: u64, _: Platform, _: bool) -> Result<Grant> {
            Ok(Grant {
                access_token: Secret::new("synthetischer-grant".into()),
                expires_at: Utc::now() + chrono::Duration::hours(1),
                platform_user_id: "123".into(),
                scopes: vec![],
            })
        }
    }
    fn request() -> SourceRequest {
        SourceRequest {
            session_id: 1,
            streamer_id: 7,
            ended_at: Utc::now(),
            settings: crate::Settings {
                enabled: true,
                source: Source::TwitchVod,
                youtube_channel_id: "UC-fixture".into(),
                vod_audio_track: Some(1),
                privacy: Privacy::Private,
                publication_authorized: false,
                title: "Probe".into(),
                description: String::new(),
            },
            object_id: None,
            manifest: None,
            twitch_binding: Some(TwitchBinding {
                broadcaster_id: "123".into(),
                stream_id: "555".into(),
                vod_audio_confirmed: true,
            }),
        }
    }
    async fn source(server: &MockServer) -> (tempfile::TempDir, NativeSources) {
        let dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let storage = Storage::new(dir.path().to_owned(), 1024 * 1024).unwrap();
        let source = NativeSources {
            store: Store::new(Arc::new(NoDb)),
            storage,
            broker: Arc::new(Broker),
            config: SourceConfig {
                ffmpeg: PathBuf::new(),
                ffprobe: PathBuf::new(),
                yt_dlp: PathBuf::new(),
                twitch_client_id: "public-test-client".into(),
                max_source_bytes: 1024,
                max_export_bytes: 1024,
                download_bytes_per_second: 1024,
                operation_timeout: Duration::from_secs(2),
                twitch_settle_time: Duration::from_secs(60),
            },
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            twitch_api: reqwest::Url::parse(&format!("{}/helix/", server.uri())).unwrap(),
        };
        (dir, source)
    }
    #[tokio::test]
    async fn twitch_nimmt_die_gebundene_stream_id_statt_des_neuesten_vods() {
        let server = MockServer::start().await;
        let (_dir, source) = source(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .and(query_param("user_id", "123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/helix/videos")).and(query_param("user_id","123")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"111","user_id":"123","stream_id":"999","type":"archive"},{"id":"222","user_id":"123","stream_id":"555","type":"archive"}],"pagination":{}}))).mount(&server).await;
        let selected = source.twitch_video(&request()).await.unwrap();
        assert_eq!(selected["id"], "222");
    }
    #[tokio::test]
    async fn laufender_gebundener_stream_ist_keine_fertige_quelle() {
        let server = MockServer::start().await;
        let (_dir, source) = source(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"555"}]})))
            .mount(&server)
            .await;
        assert_eq!(
            source.twitch_video(&request()).await,
            Err(Error::SourcePending)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn fremder_broadcaster_wird_trotz_passender_stream_id_abgewiesen() {
        let server = MockServer::start().await;
        let (_dir, source) = source(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/helix/videos")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"222","user_id":"999","stream_id":"555","type":"archive"}],"pagination":{}}))).mount(&server).await;
        assert_eq!(
            source.twitch_video(&request()).await,
            Err(Error::WrongChannel)
        );
    }
}

fn ensure_header(
    headers: &mut std::collections::BTreeMap<u8, Vec<u8>>,
    tag: &uplink_media::flv::FlvTag,
) -> Result<()> {
    if tag.is_sequence_header() {
        if let Some(previous) = headers.get(&tag.kind()) {
            if previous.as_slice() != tag.body() {
                return Err(Error::ChangedProfile);
            }
        } else {
            headers.insert(tag.kind(), tag.body().to_vec());
        }
    }
    Ok(())
}
#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn neue_codec_header_werden_nicht_unbemerkt_in_dieselbe_datei_kopiert() {
        let mut headers = std::collections::BTreeMap::new();
        let first =
            uplink_media::flv::FlvTag::new(8, 0, vec![0xaf, 0, 0x12, 0x10].into(), 64).unwrap();
        let changed =
            uplink_media::flv::FlvTag::new(8, 100, vec![0xaf, 0, 0x11, 0x90].into(), 64).unwrap();
        ensure_header(&mut headers, &first).unwrap();
        ensure_header(&mut headers, &first).unwrap();
        assert_eq!(
            ensure_header(&mut headers, &changed),
            Err(Error::ChangedProfile)
        );
    }
}
