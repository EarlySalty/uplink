//! Idee 1: Produktions-Coordinator, echte Codec-Fixtures und zwei lokale TLS-Peers.
//! Keine Plattformveröffentlichung und keine produktiven Zugangsdaten.
use futures::FutureExt;
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt;
use uplink_ingest::{AuthorizedSession, Authorizer, EndReason, EventKind, IngestServer, MediaKind};
use uplink_media::{
    MediaLimits, PublishSecret, PublishTarget,
    flv::{FlvReader, FlvTag, HEADER},
    pusher::RunningPusher,
};
#[path = "support/database.rs"]
mod database;
#[path = "../../uplink-ingest/tests/support/tls.rs"]
mod tls;

const FFMPEG: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg";
const FFPROBE: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffprobe";
const FRAMES: u64 = 360;

// Die Kindprüfung umfasst den Testprozess; Codec-Fälle teilen diesen Prozess.
static CODEC_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Benötigt PostgreSQL 16 und produktives FFmpeg 8; lokale Codec-Abnahme."]
async fn idee1_av1_1440p60_single_h264_1080p60_explicit_stop() {
    case("av1").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Benötigt PostgreSQL 16 und produktives FFmpeg 8; lokale Codec-Abnahme."]
async fn idee1_hevc_1440p60_single_h264_1080p60_explicit_stop() {
    case("hevc").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Benötigt PostgreSQL 16 und produktives FFmpeg 8; lokale Codec-Abnahme."]
async fn idee1_h264_1440p60_single_h264_1080p60_explicit_stop() {
    case("h264").await;
}

async fn fixture(codec: &str) -> Vec<u8> {
    let mut command = tokio::process::Command::new(FFMPEG);
    command.args([
        "-nostdin",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=2560x1440:rate=60",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=997:sample_rate=48000",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1499:sample_rate=48000",
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        "-map",
        "2:a:0",
        "-t",
        "6",
        "-pix_fmt",
        "yuv420p",
    ]);
    match codec {
        "av1" => {
            command.args([
                "-c:v",
                "libsvtav1",
                "-preset",
                "12",
                "-svtav1-params",
                "lp=4:rc=2:pred-struct=1",
            ]);
        }
        "hevc" => {
            command.args([
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-x265-params",
                "pools=4:frame-threads=1:strict-cbr=1:log-level=error",
                "-bf",
                "0",
            ]);
        }
        "h264" => {
            command.args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-threads",
                "4",
                "-x264-params",
                "nal-hrd=cbr:force-cfr=1",
                "-bf",
                "0",
            ]);
        }
        _ => panic!("Unbekannter Testcodec"),
    }
    let output = command
        .args([
            "-b:v",
            "5000k",
            "-minrate",
            "5000k",
            "-maxrate",
            if codec == "av1" { "0" } else { "5000k" },
            "-bufsize",
            "10000k",
            "-g",
            "120",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-f",
            "flv",
            "-flvflags",
            "no_duration_filesize",
            "pipe:1",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(120), output)
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "Fixture {codec}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

struct Destination;
impl Authorizer for Destination {
    async fn authorize(&self, app: &str, key: &str) -> Result<AuthorizedSession, ()> {
        if app == "live" && key == "idee1-synthetic-local" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

async fn case(codec: &str) {
    let _serial = CODEC_TEST_MUTEX.lock().await;
    let source_bytes = fixture(codec).await;
    let (database, mut state) = database::fixture().await;
    let certificates = tls::test_tls();
    let config = &mut Arc::get_mut(&mut state).unwrap().config;
    config.chat = None;
    config.media.worker_threads = 4;
    config.media.work_directory = database.directory.join("media");
    let ca_path = database.directory.join("public-test-ca.pem");
    std::fs::write(&ca_path, &certificates.certificate_pem).unwrap();
    config.loopback_test_ca = Some(ca_path);
    config
        .platforms
        .retain(|platform| platform.name == "twitch");
    config.platforms[0].allowed_hosts = vec!["localhost".into()];
    config.platforms[0].use_vod_audio = false;
    let output = IngestServer::bind_loopback(
        0,
        certificates.server.clone(),
        Arc::new(Destination),
        config.ingest_limits().unwrap(),
    )
    .await
    .unwrap();
    let endpoint = format!(
        "rtmps://localhost:{}/live",
        output.local_addr().unwrap().port()
    );
    let key = state
        .secrets
        .encryption
        .seal(b"idee1-synthetic-local", "destination:11:twitch")
        .unwrap();
    state.store.query("INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled,width,height,fps,bitrate_kbps,twitch_audio_mode,twitch_output_mode) VALUES(11,'twitch',$1,$2,true,1920,1080,60,6000,'live','single')", &[&endpoint, &key]).await.unwrap();
    let output_path = database.directory.join("received.flv");
    let capture_path = output_path.clone();
    let mut receiving = tokio::spawn(async move {
        let mut connection = output.accept().await.unwrap();
        let mut file = tokio::fs::File::create(capture_path).await.unwrap();
        file.write_all(HEADER).await.unwrap();
        let mut video = 0u64;
        let mut audio = std::collections::BTreeMap::<u8, Vec<Vec<u8>>>::new();
        while let Some(event) = connection.next().await {
            let kind = if event.identity.track.kind == MediaKind::Video {
                9
            } else {
                8
            };
            let tag = FlvTag::new(
                kind,
                event.dts_ms,
                Arc::from(event.wire_body()),
                2 * 1024 * 1024,
            )
            .unwrap();
            tag.write_to(&mut file).await.unwrap();
            if event.event_kind == EventKind::Frame {
                if kind == 9 {
                    assert_eq!(event.codec, uplink_ingest::WireCodec::H264);
                    video += 1;
                } else {
                    audio
                        .entry(event.identity.track.wire_id)
                        .or_default()
                        .push(event.payload().to_vec());
                }
            }
        }
        file.flush().await.unwrap();
        (video, audio, connection.finish().await.reason)
    });
    let coordinator = Arc::new(uplink_service::media::Coordinator::new(state.clone()).unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let runtime_state = state.clone();
    let mut runtime = tokio::spawn(async move {
        uplink_service::runtime::serve_with_ready(
            runtime_state,
            certificates.server,
            coordinator,
            async {
                let _ = stopped.await;
            },
            Some(ready),
        )
        .await
    });
    let (api, input) = bound.await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        let producer = RunningPusher::start(PublishTarget { id:"idee1-local-input".into(), endpoint:format!("rtmps://localhost:{}/live",input.port()), playpath:PublishSecret::new(b"rsr_00000000000000000000000000000000".to_vec()).unwrap(), tls:Some(certificates.client), allowed_hosts:vec!["localhost".into()], allow_loopback:true, allow_unencrypted:false }, MediaLimits::default()).await.unwrap();
        let mut source = FlvReader::new(source_bytes.as_slice(), 2 * 1024 * 1024);
        let start = tokio::time::Instant::now();
        let mut input_audio = std::collections::BTreeMap::<u8, Vec<Vec<u8>>>::new();
        while let Some(tag) = source.next().await.unwrap() {
            if !matches!(tag.kind(),8|9) { continue; }
            tokio::time::sleep_until(start + Duration::from_millis(u64::from(tag.timestamp_ms()))).await;
            if tag.kind()==8 {
                let body = tag.body();
                let is_frame = (body[0] >> 4 == 10 && body[1] == 1)
                    || (body[0] == 0x95 && matches!(body[1], 1 | 3));
                if is_frame {
                    let track = tag.audio_track().unwrap().expect("AAC-Paket braucht eine Spur");
                    let payload = if body[0] >> 4 == 10 { &body[2..] } else { &body[7..] };
                    input_audio.entry(track).or_default().push(payload.to_vec());
                }
            }
            producer.try_send(Arc::new(tag)).expect("Gepacete Quelle muss ohne Rückstau ankommen");
        }
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let status: serde_json::Value = http.get(format!("http://{api}/v1/me/destinations?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
                let destination = &status["destinations"][0];
                let sessions = state.registry.status(11);
                assert!(sessions[0].error.is_none(), "{codec}: {:?}", sessions[0].error);
                if destination["output_state"]=="sending" {
                    assert_eq!(destination["active_profile"]["width"],1920);
                    assert_eq!(destination["active_profile"]["height"],1080);
                    assert_eq!(destination["active_profile"]["fps"],60.0);
                    assert_eq!(destination["active_profile"]["bitrate_kbps"],6000);
                    assert_eq!(destination["input_codec"],codec);
                    assert!(destination["input_bitrate_kbps"].as_f64().is_some_and(|rate|rate>0.0));
                    println!("{codec}: active_profile={}",destination["active_profile"]);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }).await.expect("Einzelausgang muss senden");
        let me: serde_json::Value = http.get(format!("http://{api}/v1/me?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
        let observed=&me["session"]["source_observation"];
        assert_eq!(observed["codec"],codec);
        assert_eq!(observed["width"],2560);
        assert_eq!(observed["height"],1440);
        assert_eq!(observed["fps_numerator"],60);
        assert_eq!(observed["fps_denominator"],1);
        assert_eq!(observed["audio"][0]["sample_rate"],48000);
        producer.finish().await.unwrap();
        let (video,audio,end)=tokio::time::timeout(Duration::from_secs(15),&mut receiving).await.unwrap().unwrap();
        assert_eq!(video,FRAMES,"Alle Videoframes müssen ankommen");
        assert_eq!(audio.keys().collect::<Vec<_>>(), input_audio.keys().collect::<Vec<_>>(), "Live- und VOD-Audiospur müssen beide ankommen");
        for (track, expected) in &input_audio {
            let actual = &audio[track];
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_eq!(actual.len(), expected.len(), "AAC-Paketlänge Spur {track}, Paket {index}");
                assert!(actual == expected, "AAC-Payload weicht ab: Spur {track}, Paket {index}");
            }
            assert_eq!(actual.len(), expected.len(), "AAC-Paketanzahl für Spur {track}; gemeinsamer Präfix war bytegleich");
        }
        assert_eq!(end,EndReason::ExplicitStop);
        tokio::time::timeout(Duration::from_secs(5),async { while state.registry.active_count()!=0 {tokio::time::sleep(Duration::from_millis(20)).await;} }).await.unwrap();
        let sessions=state.registry.status(11);
        assert_eq!(sessions[0].ingest_end_reason.as_deref(),Some("ExplicitStop"));
        assert!(sessions[0].error.is_none());
        assert_no_encoder_children();
        let finished:serde_json::Value=http.get(format!("http://{api}/v1/me/destinations?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
        assert_eq!(finished["destinations"][0]["output_state"],"finished");
        assert!(finished["destinations"][0]["active_profile"].is_null());
        let probe=tokio::process::Command::new(FFPROBE).args(["-v","error","-count_frames","-show_streams","-of","json"]).arg(&output_path).env_clear().output().await.unwrap();
        assert!(probe.status.success());
        let probe:serde_json::Value=serde_json::from_slice(&probe.stdout).unwrap();
        let video=probe["streams"].as_array().unwrap().iter().find(|s|s["codec_type"]=="video").unwrap();
        assert_eq!(video["codec_name"],"h264");
        assert_eq!(video["width"],1920);
        assert_eq!(video["height"],1080);
        assert_eq!(video["r_frame_rate"],"60/1");
        assert_eq!(video["nb_read_frames"],FRAMES.to_string());
        println!("{codec}: Observation 2560x1440 60/1, H264 1920x1080 60/1, {FRAMES} Frames, {} AAC-Pakete, ExplicitStop beidseitig, Ziel finished",audio.len());
    }).catch_unwind();
    let result = tokio::time::timeout(Duration::from_secs(50), result).await;
    if !receiving.is_finished() {
        receiving.abort();
        let _ = receiving.await;
    }
    let _ = stop.send(());
    if tokio::time::timeout(Duration::from_secs(12), &mut runtime)
        .await
        .is_err()
    {
        runtime.abort();
        let _ = runtime.await;
    }
    drop(state);
    database.stop().await;
    assert!(
        matches!(result, Ok(Ok(()))),
        "Idee1-Coordinatornachweis {codec} fehlgeschlagen"
    );
}

fn assert_no_encoder_children() {
    for task in std::fs::read_dir("/proc/self/task").unwrap() {
        let children = std::fs::read_to_string(task.unwrap().path().join("children"));
        // Tokio kann einen Worker zwischen Verzeichnisauflistung und Lesen beenden.
        let children = match children {
            Ok(children) => children,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("Encoderkindprüfung fehlgeschlagen: {error}"),
        };
        for pid in children.split_whitespace() {
            if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
                assert_ne!(comm.trim(), "ffmpeg", "Encoderkind {pid} nach ExplicitStop");
                assert_ne!(comm.trim(), "ffprobe", "Probekind {pid} nach ExplicitStop");
            }
        }
    }
}
