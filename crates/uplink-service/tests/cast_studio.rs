use std::{sync::Arc, time::Duration};

#[path = "support/database.rs"]
mod database;
#[path = "../../uplink-ingest/tests/support/tls.rs"]
mod tls;

use database::fixture;
use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use uplink_ingest::{
    AuthorizedSession, Authorizer, EventKind, IngestLimits, IngestServer, MediaKind,
};
use uplink_media::{
    MediaLimits, PublishSecret, PublishTarget, flv::FlvReader, pusher::RunningPusher,
};

const OUTPUT_KEY: &str = "synthetic-cast-output";
const SOURCE_A: &str = "cst_11111111111111111111111111111111";
const SOURCE_B: &str = "cst_22222222222222222222222222222222";

struct DestinationAuth;
impl Authorizer for DestinationAuth {
    async fn authorize(&self, app: &str, key: &str) -> Result<AuthorizedSession, ()> {
        if app == "live" && key == OUTPUT_KEY {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

async fn cast_fixture(ffmpeg: &std::path::Path, color: &str, tone: &str) -> Vec<u8> {
    use std::process::Stdio;
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(ffmpeg)
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=256x144:r=25:d=4"),
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency={tone}:sample_rate=48000:duration=4"),
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "10",
                "-keyint_min",
                "10",
                "-sc_threshold",
                "0",
                "-b:v",
                "800k",
                "-maxrate",
                "800k",
                "-bufsize",
                "800k",
                "-c:a",
                "aac",
                "-b:a",
                "96k",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-f",
                "flv",
                "-flvflags",
                "no_metadata+no_duration_filesize",
                "pipe:1",
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("FFmpeg-Zeitlimit")
    .expect("FFmpeg muss starten");
    assert!(
        output.status.success(),
        "Cast-Fixture konnte nicht erzeugt werden: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

async fn feed_source(
    address: std::net::SocketAddr,
    key: &'static str,
    tls: Arc<uplink_ingest::rustls::ClientConfig>,
    bytes: Vec<u8>,
    stop: tokio::sync::oneshot::Receiver<()>,
) {
    let producer = RunningPusher::start(
        PublishTarget {
            id: format!("source-{key}"),
            endpoint: format!("rtmps://localhost:{}/live", address.port()),
            playpath: PublishSecret::new(key.as_bytes().to_vec()).unwrap(),
            tls: Some(tls),
            allowed_hosts: vec!["localhost".into()],
            allow_loopback: true,
            allow_unencrypted: false,
        },
        MediaLimits::default(),
    )
    .await
    .unwrap();
    let mut source = FlvReader::new(bytes.as_slice(), 2 * 1024 * 1024);
    while let Some(tag) = source.next().await.unwrap() {
        producer.try_send(Arc::new(tag)).unwrap();
        tokio::time::sleep(Duration::from_millis(8)).await;
    }
    let _ = stop.await;
    drop(producer);
}

async fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(timeout, async {
        loop {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Bedingung wurde nicht rechtzeitig erfüllt");
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und den installierten geprüften FFmpeg-8-Build."]
async fn cast_switch_keeps_one_platform_connection_and_drops_standby_from_output_graph() {
    let (database, mut state) = fixture().await;
    state.registry.configure_cast_limit(2).unwrap();

    let certificates = tls::test_tls();
    let output = Arc::new(
        IngestServer::bind_loopback(
            0,
            certificates.server.clone(),
            Arc::new(DestinationAuth),
            IngestLimits::local_probe(),
        )
        .await
        .unwrap(),
    );
    let output_address = output.local_addr().unwrap();

    let config = &mut Arc::get_mut(&mut state).unwrap().config;
    config.chat = None;
    config.media.work_directory = database.directory.join("media");
    config.loopback_test_ca = Some(database.directory.join("cast-ca.pem"));
    std::fs::write(
        config.loopback_test_ca.as_ref().unwrap(),
        &certificates.certificate_pem,
    )
    .unwrap();
    config
        .platforms
        .retain(|platform| platform.name == "youtube");
    config.platforms[0].allowed_hosts = vec!["localhost".into()];
    config.platforms[0].use_vod_audio = false;

    let output_key = state
        .secrets
        .encryption
        .seal(OUTPUT_KEY.as_bytes(), "destination:11:youtube")
        .unwrap();
    let endpoint = format!("rtmps://localhost:{}/live", output_address.port());
    state
        .store
        .query(
            "INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled,width,height,fps,bitrate_kbps) VALUES(11,'youtube',$1,$2,true,256,144,25,500)",
            &[&endpoint, &output_key],
        )
        .await
        .unwrap();

    for (source_id, label, key) in [(101_i64, "Rot", SOURCE_A), (102_i64, "Blau", SOURCE_B)] {
        let hash = hex::encode(Sha256::digest(key.as_bytes()));
        let encrypted = state
            .secrets
            .encryption
            .seal(key.as_bytes(), &format!("cast_source:11:{hash}"))
            .unwrap();
        state
            .store
            .query(
                "INSERT INTO relay.cast_sources(streamer_id,source_id,label,kind,ingest_key_hash,ingest_key_enc) VALUES(11,$1,$2,'pov',$3,$4)",
                &[&source_id, &label, &hash, &encrypted],
            )
            .await
            .unwrap();
        state
            .store
            .query(
                "INSERT INTO relay.cast_scenes(streamer_id,scene_id,name,source_id,sort_order) VALUES(11,$1,$2,$3,$4)",
                &[&(source_id + 100), &label, &source_id, &(source_id as i32)],
            )
            .await
            .unwrap();
    }
    state
        .store
        .query(
            "INSERT INTO relay.cast_state(streamer_id,program_scene_id,preview_scene_id) VALUES(11,201,202)",
            &[],
        )
        .await
        .unwrap();

    let source_a = cast_fixture(&state.config.media.ffmpeg, "red", "440").await;
    let source_b = cast_fixture(&state.config.media.ffmpeg, "blue", "660").await;

    let coordinator = Arc::new(uplink_service::media::Coordinator::new(state.clone()).unwrap());
    let (stop_runtime, stopped_runtime) = tokio::sync::oneshot::channel();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let runtime_state = state.clone();
    let runtime_tls = certificates.server.clone();
    let runtime = tokio::spawn(async move {
        uplink_service::runtime::serve_with_ready(
            runtime_state,
            runtime_tls,
            coordinator,
            async {
                let _ = stopped_runtime.await;
            },
            Some(ready),
        )
        .await
    });
    let (api, ingest) = bound.await.unwrap();

    let output_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output_audio_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output_last_dts = Arc::new(std::sync::Mutex::new([None::<u32>; 2]));
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let output_reader_server = output.clone();
    let output_count_reader = output_count.clone();
    let output_audio_count_reader = output_audio_count.clone();
    let output_last_dts_reader = output_last_dts.clone();
    let output_reader = tokio::spawn(async move {
        let mut connection = output_reader_server.accept().await.unwrap();
        let _ = accepted_tx.send(());
        while let Some(event) = connection.next().await {
            if event.event_kind != EventKind::Frame {
                continue;
            }
            let track_index = match event.identity.track.kind {
                MediaKind::Video => {
                    output_count_reader.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    0
                }
                MediaKind::Audio => {
                    output_audio_count_reader.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    1
                }
            };
            let mut last = output_last_dts_reader.lock().unwrap();
            if let Some(previous) = last[track_index] {
                assert!(
                    event.dts_ms >= previous,
                    "Program-Zeitlinie darf je Track nicht zurückspringen"
                );
            }
            last[track_index] = Some(event.dts_ms);
        }
        connection.finish().await
    });

    let (stop_a_tx, stop_a_rx) = tokio::sync::oneshot::channel();
    let (stop_b_tx, stop_b_rx) = tokio::sync::oneshot::channel();
    let source_a_task = tokio::spawn(feed_source(
        ingest,
        SOURCE_A,
        certificates.client.clone(),
        source_a,
        stop_a_rx,
    ));
    let source_b_task = tokio::spawn(feed_source(
        ingest,
        SOURCE_B,
        certificates.client.clone(),
        source_b,
        stop_b_rx,
    ));

    tokio::time::timeout(Duration::from_secs(10), accepted_rx)
        .await
        .expect("Program-Ausgang wurde nicht geöffnet")
        .unwrap();
    wait_for(Duration::from_secs(5), || {
        let sessions = state.registry.status(11);
        sessions.iter().any(|session| {
            session.source_id == Some(102)
                && session.active
                && session
                    .source_observation
                    .as_ref()
                    .is_some_and(|value| value["codec"] == "h264")
        })
    })
    .await;

    use tokio_tungstenite::tungstenite::{
        Message as PreviewMessage, client::IntoClientRequest, http::HeaderValue,
    };
    let mut preview_request = format!("ws://{api}/v1/me/cast/sources/102/preview?streamer_id=11")
        .into_client_request()
        .unwrap();
    preview_request
        .headers_mut()
        .insert("x-relay-auth", HeaderValue::from_static("synthetic-api"));
    let (mut preview_socket, _) = tokio::time::timeout(
        Duration::from_secs(3),
        tokio_tungstenite::connect_async(preview_request),
    )
    .await
    .expect("Preview-WebSocket konnte nicht rechtzeitig verbinden")
    .expect("Preview-WebSocket wurde abgewiesen");
    let mut saw_header = false;
    let mut saw_frame = false;
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = preview_socket.next().await {
            let PreviewMessage::Binary(bytes) = message.unwrap() else {
                continue;
            };
            assert!(
                bytes.len() > 16,
                "Preview-Paket muss Header und FLV-Nutzdaten enthalten"
            );
            assert_eq!(bytes[0], 1, "Unbekannte Preview-Protokollversion");
            assert_eq!(
                bytes[2], 1,
                "H.264-Quelle muss als H.264 angekündigt werden"
            );
            match bytes[1] {
                0 => saw_header = true,
                1 => saw_frame = true,
                _ => {}
            }
            if saw_header && saw_frame {
                break;
            }
        }
    })
    .await
    .expect("Preview lieferte nicht rechtzeitig Header und Frame");
    assert!(saw_header && saw_frame);

    wait_for(Duration::from_secs(5), || {
        output_count.load(std::sync::atomic::Ordering::SeqCst) >= 6
    })
    .await;
    let before_switch = output_count.load(std::sync::atomic::Ordering::SeqCst);
    let audio_before_switch = output_audio_count.load(std::sync::atomic::Ordering::SeqCst);

    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let response = http
        .put(format!("http://{api}/v1/me/cast/program"))
        .header("X-Relay-Auth", "synthetic-api")
        .json(&serde_json::json!({
            "streamer_id": 11,
            "scene_id": 202,
            "expected_generation": 0,
            "swap_preview": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    wait_for(Duration::from_secs(5), || {
        state.registry.status(11).iter().any(|session| {
            session.source_id == Some(102)
                && session.cast_role == "program"
                && session
                    .source_observation
                    .as_ref()
                    .is_some_and(|value| value["cast_program_routed"] == true)
        })
    })
    .await;
    wait_for(Duration::from_secs(5), || {
        output_count.load(std::sync::atomic::Ordering::SeqCst) >= before_switch + 6
    })
    .await;
    wait_for(Duration::from_secs(5), || {
        output_audio_count.load(std::sync::atomic::Ordering::SeqCst) >= audio_before_switch + 6
    })
    .await;

    assert!(
        tokio::time::timeout(Duration::from_millis(600), output.accept())
            .await
            .is_err(),
        "Program-Schnitt darf keine zweite Plattformverbindung öffnen"
    );
    let sessions = state.registry.status(11);
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.source_id == Some(101))
            .unwrap()
            .cast_role,
        "preview"
    );
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.source_id == Some(102))
            .unwrap()
            .cast_role,
        "program"
    );

    let _ = preview_socket.send(PreviewMessage::Close(None)).await;
    let _ = stop_b_tx.send(());
    source_b_task.await.unwrap();
    let _ = stop_a_tx.send(());
    source_a_task.await.unwrap();
    let _ = stop_runtime.send(());
    runtime.await.unwrap().unwrap();
    let report = tokio::time::timeout(Duration::from_secs(5), output_reader)
        .await
        .expect("Program-Ausgang wurde nicht beendet")
        .unwrap();
    assert!(report.received_events > 0);
    assert_eq!(
        report.timestamp_clamps, 0,
        "Program-Ausgang durfte keine Zeitstempel klemmen"
    );
    assert!(
        report.timestamp_rejection.is_none(),
        "Program-Ausgang durfte keinen Zeitstempelfehler verbergen"
    );
    database.stop().await;
}
