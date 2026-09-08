//! Isolierte echte FFmpeg-Probe: zwei AAC-Mixe, sparse Wire-ID, Aufnahme → MP4.
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_postgres::{Row, types::ToSql};
use uplink_ingest::{EventKind, MediaKind, WireCodec, WireTrack};
use uplink_media::flv::FlvReader;
use uplink_vod::{recording::*, *};
struct NoDatabase;
#[async_trait]
impl Database for NoDatabase {
    async fn query(&self, _: &str, _: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        Err(Error::Database)
    }
}
struct FixtureBroker;
#[async_trait]
impl PlatformBroker for FixtureBroker {
    async fn grant(&self, _: u64, _: Platform, _: bool) -> Result<Grant> {
        Ok(Grant {
            access_token: Secret::new("synthetischer-privater-zugang".into()),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            platform_user_id: "123".into(),
            scopes: vec![],
        })
    }
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 6 || args[0] != "--ffmpeg" || args[2] != "--ffprobe" || args[4] != "--yt-dlp" {
        return Err("Aufruf: --ffmpeg /pfad --ffprobe /pfad --yt-dlp /pfad".into());
    }
    let root = tempfile::tempdir()?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let storage = Storage::new(root.path().to_owned(), 64 * 1024 * 1024)?;
    for (codec, fixture) in [
        (
            "h264",
            include_bytes!("../../../experiments/scuffle-probe/fixtures/h264.flv").as_slice(),
        ),
        (
            "av1",
            include_bytes!("../../../experiments/scuffle-probe/fixtures/av1.flv").as_slice(),
        ),
    ] {
        let mut reader = FlvReader::new(std::io::Cursor::new(fixture), 1024 * 1024);
        let running = Recorder::start(
            storage.clone(),
            RecordingSpec {
                session_id: 1,
                streamer_id: 7,
                live_audio_track: 0,
                vod_audio_track: Some(12),
                metadata: serde_json::json!({"source":codec,"layout_revision":1}),
            },
            RecordingConfig {
                max_events: 1024,
                max_queue_bytes: 2 * 1024 * 1024,
                max_recording_bytes: 8 * 1024 * 1024,
                segment_bytes: 1024 * 1024,
                max_segments: 16,
            },
        )
        .await?;
        let sink = running.sink();
        let mut expected = vec![];
        use sha2::{Digest, Sha256};
        while let Some(mut tag) = reader.next().await? {
            if tag.kind() == 18 {
                continue;
            }
            let audio = tag.audio_track()?;
            let kind = if audio.is_some() {
                MediaKind::Audio
            } else {
                MediaKind::Video
            };
            let track = if audio == Some(1) { 12 } else { 0 };
            if audio == Some(1) {
                if tag.body().get(1) == Some(&1) {
                    let prefix = if tag.body()[0] == 0x95 { 7 } else { 2 };
                    expected.push(format!(
                        "SHA256:{}",
                        hex::encode(Sha256::digest(&tag.body()[prefix..]))
                    ));
                }
                tag = tag.with_audio_track(12, 1024 * 1024)?;
            }
            let event_kind = if tag.is_sequence_header() {
                EventKind::SequenceHeader
            } else if audio.is_some() && tag.body().get(1) == Some(&4) {
                EventKind::Metadata
            } else {
                EventKind::Frame
            };
            let pts_ms = tag.timestamp_ms() as i64;
            let packet = RecordingPacket {
                generation: 1,
                track: WireTrack {
                    kind,
                    wire_id: track,
                },
                codec: if audio.is_some() {
                    WireCodec::Aac
                } else if codec == "av1" {
                    WireCodec::Av1
                } else {
                    WireCodec::H264
                },
                event_kind,
                configuration_revision: 1,
                pts_ms,
                tag: Arc::new(tag),
            };
            sink.reserve(packet.tag.wire_len())?.submit(packet)?;
        }
        let manifest = running.finish().await?;
        assert!(manifest.complete);
        let native = NativeSources::new(
            Store::new(Arc::new(NoDatabase)),
            storage.clone(),
            Arc::new(FixtureBroker),
            SourceConfig {
                ffmpeg: PathBuf::from(&args[1]),
                ffprobe: PathBuf::from(&args[3]),
                yt_dlp: PathBuf::from(&args[5]),
                twitch_client_id: String::new(),
                max_source_bytes: 16 * 1024 * 1024,
                max_export_bytes: 16 * 1024 * 1024,
                download_bytes_per_second: 1024 * 1024,
                operation_timeout: Duration::from_secs(60),
                twitch_settle_time: Duration::from_secs(60),
            },
        )?;
        let prepared = native
            .prepare(SourceRequest {
                session_id: 1,
                streamer_id: 7,
                ended_at: chrono::Utc::now(),
                settings: Settings {
                    enabled: true,
                    source: Source::InputRecording,
                    youtube_channel_id: "UC-fixture".into(),
                    vod_audio_track: Some(12),
                    privacy: Privacy::Private,
                    publication_authorized: false,
                    title: "Isolierte Probe".into(),
                    description: String::new(),
                },
                object_id: Some(manifest.object_id.clone()),
                manifest: Some(serde_json::to_value(&manifest)?),
                twitch_binding: None,
            })
            .await?;
        let output = tokio::process::Command::new(&args[3])
            .env_clear()
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_packets",
                "-show_data_hash",
                "sha256",
                "-show_entries",
                "packet=data_hash",
                "-of",
                "json",
            ])
            .arg(storage.path(&prepared.object_id, "export.mp4")?)
            .kill_on_drop(true)
            .output()
            .await?;
        assert!(output.status.success());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let actual = json["packets"]
            .as_array()
            .ok_or("Paketliste fehlt")?
            .iter()
            .map(|v| v["data_hash"].as_str().unwrap_or("").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            actual.len(),
            expected.len(),
            "Anzahl der tatsächlichen AAC-Pakete"
        );
        assert!(
            actual == expected,
            "Export muss den tatsächlich ausgewählten VOD-Mix bytegenau behalten"
        );
        if codec == "h264" {
            use serde_json::json;
            use wiremock::{
                Mock, MockServer, ResponseTemplate,
                matchers::{method, path},
            };
            let api = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/helix/streams"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
                .mount(&api)
                .await;
            Mock::given(method("GET")).and(path("/helix/videos")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"222","stream_id":"555","user_id":"123","type":"archive","viewable":"public","duration":"2s"}],"pagination":{}}))).mount(&api).await;
            let object = storage.create_object()?;
            let tool = storage.path(&object, "fixture-downloader.sh")?;
            let source_file = storage.path(&prepared.object_id, "export.mp4")?;
            tokio::fs::write(&tool,format!("#!/bin/sh\nfor value do last=\"$value\"; done\n[ \"$last\" = 'https://www.twitch.tv/videos/222' ] || exit 9\nexec /usr/bin/cat '{}'\n",source_file.display())).await?;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700))?;
            let native_twitch = NativeSources::new(
                Store::new(Arc::new(NoDatabase)),
                storage.clone(),
                Arc::new(FixtureBroker),
                SourceConfig {
                    ffmpeg: PathBuf::from(&args[1]),
                    ffprobe: PathBuf::from(&args[3]),
                    yt_dlp: tool,
                    twitch_client_id: "public-fixture-client".into(),
                    max_source_bytes: 16 * 1024 * 1024,
                    max_export_bytes: 16 * 1024 * 1024,
                    download_bytes_per_second: 1024 * 1024,
                    operation_timeout: Duration::from_secs(60),
                    twitch_settle_time: Duration::from_secs(60),
                },
            )?
            .with_loopback_test_api(&format!("{}/helix/", api.uri()))?;
            let twitch=native_twitch.prepare(SourceRequest{session_id:2,streamer_id:7,ended_at:chrono::Utc::now(),settings:Settings{enabled:true,source:Source::TwitchVod,youtube_channel_id:"UC-fixture".into(),vod_audio_track:Some(12),privacy:Privacy::Private,publication_authorized:false,title:"Isolierter Twitch-Quellweg".into(),description:String::new()},object_id:Some(object.clone()),manifest:Some(json!({"source":"twitch_vod","observation":{"vod_id":"222","duration_seconds":2,"observed_at":chrono::Utc::now().timestamp()-120}})),twitch_binding:Some(TwitchBinding{broadcaster_id:"123".into(),stream_id:"555".into(),vod_audio_confirmed:true})}).await?;
            assert!(twitch.export.bytes > 0);
            println!(
                "Twitch-Quellweg bestanden: eigener gebundener Stream, lokale API und Downloadfixture, echte Rust-Pipe/FFprobe/FFmpeg-Kette; kein Twitch-Netzzugriff."
            );
        }
        println!(
            "Aufnahme → MP4 bestanden: {codec}, {} AAC-Pakete ausschließlich Wire-ID 12, {} Byte Export; keine Plattformverbindung.",
            actual.len(),
            prepared.export.bytes
        );
    }
    Ok(())
}
