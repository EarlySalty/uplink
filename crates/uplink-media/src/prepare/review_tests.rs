//! Lokale Repros des unabhängigen Reviews; keine Plattformzugänge.
use super::*;
use crate::{flv::FlvReader, pusher::RunningPusher};
use std::os::unix::fs::DirBuilderExt;
use std::sync::Arc;
use uplink_core::{
    Chroma, Color, ColorPrimaries, ColorRange, Gop, Matrix, RateControl, RateMode, Transfer,
};
use uplink_ingest::{AuthorizedSession, Authorizer, EndReason, IngestLimits, IngestServer};

#[path = "../../../uplink-ingest/tests/support/tls.rs"]
mod tls;

const FFMPEG: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg";
const FFPROBE: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffprobe";
const FIXTURE: &[u8] = include_bytes!("../../../../experiments/scuffle-probe/fixtures/h264.flv");

struct Auth;
impl Authorizer for Auth {
    async fn authorize(
        &self,
        app: &str,
        stream: &str,
    ) -> std::result::Result<AuthorizedSession, ()> {
        if app == "live" && stream == "review-synthetic" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

async fn endpoint() -> (IngestServer<Auth>, PublishTarget) {
    let tls = tls::test_tls();
    drop(tls.certificate_pem);
    let server =
        IngestServer::bind_loopback(0, tls.server, Arc::new(Auth), IngestLimits::local_probe())
            .await
            .unwrap();
    let target = PublishTarget {
        id: "review-local".into(),
        endpoint: format!(
            "rtmps://localhost:{}/live",
            server.local_addr().unwrap().port()
        ),
        playpath: PublishSecret::new(b"review-synthetic".to_vec()).unwrap(),
        tls: Some(tls.client),
        allowed_hosts: vec!["localhost".into()],
        allow_loopback: true,
        allow_unencrypted: false,
    };
    (server, target)
}

async fn capture(server: IngestServer<Auth>) -> Vec<MediaEvent> {
    let mut connection = server.accept().await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = connection.next().await {
        events.push(event);
    }
    let report = connection.finish().await;
    assert_eq!(
        report.reason,
        EndReason::ExplicitStop,
        "{:?}",
        report.timestamp_rejection
    );
    events
}

async fn source_events(duplicate_aux: bool) -> Vec<MediaEvent> {
    source_events_from(FIXTURE, duplicate_aux).await
}

async fn source_events_from(fixture: &[u8], duplicate_aux: bool) -> Vec<MediaEvent> {
    let (server, target) = endpoint().await;
    let capture = tokio::spawn(capture(server));
    let pusher = RunningPusher::start(target, MediaLimits::default())
        .await
        .unwrap();
    let aux = FlvTag::new(8, 0, Arc::from(&b"\xaf\x00\x12\x10"[..]), 65536)
        .unwrap()
        .with_audio_track(7, 65536)
        .unwrap();
    pusher.try_send(Arc::new(aux.clone())).unwrap();
    if duplicate_aux {
        pusher.try_send(Arc::new(aux)).unwrap();
    }
    let mut reader = FlvReader::new(fixture, 65536);
    while let Some(tag) = reader.next().await.unwrap() {
        if matches!(tag.kind(), 8 | 9) {
            pusher.try_send(Arc::new(tag)).unwrap();
        }
    }
    pusher.finish().await.unwrap();
    timeout(Duration::from_secs(5), capture)
        .await
        .unwrap()
        .unwrap()
}

fn input(events: Vec<MediaEvent>) -> (MediaEvent, mpsc::Receiver<MediaEvent>) {
    let mut events = events.into_iter();
    let first = events.next().unwrap();
    let (send, receive) = mpsc::channel(512);
    for event in events {
        send.try_send(event).unwrap();
    }
    (first, receive)
}

struct EngineFixture {
    engine: MediaEngine,
    directory: std::path::PathBuf,
}
impl std::ops::Deref for EngineFixture {
    type Target = MediaEngine;
    fn deref(&self) -> &MediaEngine {
        &self.engine
    }
}
impl Drop for EngineFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.directory);
    }
}
fn engine() -> EngineFixture {
    let mut suffix = [0_u8; 8];
    getrandom::fill(&mut suffix).unwrap();
    let directory = std::path::PathBuf::from(format!(
        "/tmp/um-review-{:016x}",
        u64::from_ne_bytes(suffix)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let engine = MediaEngine::new(EngineConfig {
        ffmpeg: FFMPEG.into(),
        ffprobe: FFPROBE.into(),
        work_directory: directory.clone(),
        limits: MediaLimits::default(),
    })
    .unwrap();
    EngineFixture { engine, directory }
}

fn profile() -> VideoProfile {
    VideoProfile {
        width: 256,
        height: 144,
        fps: FrameRate::new(25, 1).unwrap(),
        codec: Codec::H264,
        codec_profile: "high".into(),
        level: "4.2".into(),
        bit_depth: 8,
        chroma: Chroma::Yuv420,
        color: Color {
            primaries: ColorPrimaries::Bt709,
            transfer: Transfer::Bt709,
            matrix: Matrix::Bt709,
            range: ColorRange::Limited,
        },
        rate: RateControl {
            mode: RateMode::Cbr,
            target_kbps: 384,
            max_kbps: 384,
            buffer_kbits: 768,
        },
        gop: Gop {
            keyframe_interval_frames: 50,
            closed: true,
        },
    }
}

async fn start_public(
    engine: &MediaEngine,
    events: Vec<MediaEvent>,
    target: PublishTarget,
    program: bool,
    track: u8,
) -> Result<RunningMedia> {
    let (first, receive) = input(events);
    if program {
        engine
            .prepare_program_and_start(
                ProgramSessionSpec {
                    first,
                    outputs: vec![ProgramOutput {
                        target,
                        video: vec![ProgramVideo {
                            wire_track: 0,
                            canvas_index: 0,
                            profile: profile(),
                            bframes: 0,
                            layout: None,
                        }],
                        audio: vec![ProgramAudio {
                            encoding: None,
                            source_wire_track: track,
                            destination_wire_track: 0,
                        }],
                    }],
                },
                receive,
            )
            .await
    } else {
        engine
            .prepare_and_start(
                DesiredSessionSpec {
                    first,
                    outputs: vec![DesiredOutput {
                        target,
                        video: DesiredVideo {
                            width: 256,
                            height: 144,
                            fps: FrameRate::new(25, 1).unwrap(),
                            bitrate_kbps: 384,
                            codec: Codec::H264,
                        },
                        live_audio_track: track,
                        vod_audio_track: None,
                        layout: None,
                    }],
                },
                receive,
            )
            .await
    }
}

#[tokio::test]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokales TLS."]
async fn auxiliary_second_header_preserves_normal_and_program_outputs() {
    for program in [false, true] {
        // Program verlangt ein vollständiges Farbprofil. Die öffentliche
        // Fixture trägt unbekannte Farben; FFmpeg schreibt hier echte VUI-Tags.
        let fixture = if program {
            bt709_fixture().await
        } else {
            FIXTURE.to_vec()
        };
        let events = source_events_from(&fixture, true).await;
        let expected_audio: Vec<_> = events
            .iter()
            .filter(|event| {
                event.identity.track.kind == MediaKind::Audio
                    && event.identity.track.wire_id == 0
                    && event.event_kind == EventKind::Frame
            })
            .map(|event| (event.dts_ms, event.payload().to_vec()))
            .collect();
        let (server, target) = endpoint().await;
        let capture = tokio::spawn(capture(server));
        let engine = engine();
        let result = start_public(&engine, events, target, program, 0).await;
        if result.is_err() {
            capture.abort();
        }
        let running =
            result.expect("Ungewählter Aux-Header darf den öffentlichen Vorlauf nicht abbrechen");
        let observed = running.source_observation().unwrap();
        assert_eq!(
            observed
                .audio
                .iter()
                .map(|audio| audio.wire_track)
                .collect::<Vec<_>>(),
            vec![0]
        );
        let report = timeout(Duration::from_secs(15), running.finish())
            .await
            .unwrap();
        assert_eq!(
            report.error, None,
            "Program={program}, Ausgänge={:?}",
            report.status.outputs
        );
        assert_eq!(
            report.status.outputs[0].state,
            OutputState::LocalEndUnconfirmed
        );
        let received = timeout(Duration::from_secs(5), capture)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            received
                .iter()
                .filter(|event| event.identity.track.kind == MediaKind::Video
                    && event.event_kind == EventKind::Frame)
                .count(),
            50
        );
        let audio: Vec<_> = received
            .iter()
            .filter(|event| {
                event.identity.track.kind == MediaKind::Audio
                    && event.event_kind == EventKind::Frame
            })
            .map(|event| {
                assert_eq!(event.identity.track.wire_id, 0);
                (event.dts_ms, event.payload().to_vec())
            })
            .collect();
        assert_eq!(audio, expected_audio);
    }
}

async fn bt709_fixture() -> Vec<u8> {
    let mut child = Command::new(FFMPEG)
        .args(["-nostdin", "-v", "error", "-f", "flv", "-i", "pipe:0", "-map", "0", "-c", "copy", "-bsf:v", "h264_metadata=colour_primaries=1:transfer_characteristics=1:matrix_coefficients=1:video_full_range_flag=0", "-f", "flv", "-flvflags", "no_metadata+no_duration_filesize", "pipe:1"])
        .env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let write = tokio::spawn(async move {
        stdin.write_all(FIXTURE).await.unwrap();
    });
    let output = timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    write.await.unwrap();
    assert!(output.status.success());
    output.stdout
}

#[tokio::test]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokales TLS."]
async fn mixed_program_encodes_audio_while_ordinary_preserves_aac_with_one_decoder() {
    for offset in [0, 123] {
        mixed_program_case(offset).await;
    }
}

async fn mixed_program_case(offset: u32) {
    let original = bt709_fixture().await;
    let mut fixture = HEADER.to_vec();
    let mut reader = FlvReader::new(&original[..], 65536);
    while let Some(tag) = reader.next().await.unwrap() {
        FlvTag::new(
            tag.kind(),
            tag.timestamp_ms() + offset,
            Arc::from(tag.body()),
            65536,
        )
        .unwrap()
        .write_to(&mut fixture)
        .await
        .unwrap();
    }
    let events = source_events_from(&fixture, false).await;
    let expected_video: Vec<_> = events
        .iter()
        .filter(|event| {
            event.identity.track.kind == MediaKind::Video && event.event_kind == EventKind::Frame
        })
        .map(|event| event.dts_ms)
        .collect();
    let expected_audio: Vec<_> = events
        .iter()
        .filter(|event| {
            event.identity.track.kind == MediaKind::Audio
                && event.identity.track.wire_id == 1
                && event.event_kind == EventKind::Frame
        })
        .map(|event| (event.dts_ms, event.payload().to_vec()))
        .collect();
    let (ordinary_server, mut ordinary_target) = endpoint().await;
    ordinary_target.id = "youtube".into();
    let (program_server, mut program_target) = endpoint().await;
    program_target.id = "twitch".into();
    let ordinary_capture = tokio::spawn(capture(ordinary_server));
    let program_capture = tokio::spawn(capture(program_server));
    let (first, mut receive) = input(events);
    let engine = engine();
    let mut diagnostic = PreparationDiagnostic::default();
    let prepared = engine
        .prepare_source_diagnosed(first, &mut receive, &HashSet::from([0, 1]), &mut diagnostic)
        .await
        .unwrap();
    let mut lower = profile();
    lower.width = 128;
    lower.height = 72;
    let running = engine
        .start_prepared_mixed(
            prepared,
            vec![DesiredOutput {
                target: ordinary_target,
                video: DesiredVideo {
                    width: 256,
                    height: 144,
                    fps: FrameRate::new(25, 1).unwrap(),
                    bitrate_kbps: 384,
                    codec: Codec::H264,
                },
                live_audio_track: 1,
                vod_audio_track: None,
                layout: None,
            }],
            vec![ProgramOutput {
                target: program_target,
                video: [profile(), lower]
                    .into_iter()
                    .enumerate()
                    .map(|(index, profile)| ProgramVideo {
                        wire_track: index as u8,
                        canvas_index: 0,
                        profile,
                        bframes: 0,
                        layout: None,
                    })
                    .collect(),
                audio: [0, 1]
                    .into_iter()
                    .map(|track| crate::ProgramAudio {
                        source_wire_track: track,
                        destination_wire_track: track,
                        encoding: Some(crate::AudioEncoding {
                            channels: 2,
                            bitrate_kbps: 160,
                        }),
                    })
                    .collect(),
            }],
            receive,
            &mut diagnostic,
        )
        .unwrap();
    let report = timeout(Duration::from_secs(15), running.finish())
        .await
        .unwrap();
    assert_eq!(report.error, None);
    assert_eq!(report.status.video_decoders, 1);
    assert_eq!(report.status.encode_groups, 2);
    assert_eq!(report.status.graph[0].timestamp_offset_ms, 0);
    assert_eq!(report.status.graph[1].timestamp_offset_ms, 22);
    assert!(
        report
            .status
            .outputs
            .iter()
            .all(|output| output.state == OutputState::LocalEndUnconfirmed)
    );
    let ordinary = timeout(Duration::from_secs(5), ordinary_capture)
        .await
        .unwrap()
        .unwrap();
    let program = timeout(Duration::from_secs(5), program_capture)
        .await
        .unwrap()
        .unwrap();
    let audio: Vec<_> = ordinary
        .iter()
        .filter(|event| {
            event.identity.track.kind == MediaKind::Audio && event.event_kind == EventKind::Frame
        })
        .map(|event| {
            assert_eq!(event.identity.track.wire_id, 0);
            (event.dts_ms, event.payload().to_vec())
        })
        .collect();
    assert_eq!(audio, expected_audio);
    let ordinary_video: Vec<_> = ordinary
        .iter()
        .filter(|event| {
            event.identity.track.kind == MediaKind::Video && event.event_kind == EventKind::Frame
        })
        .map(|event| event.dts_ms)
        .collect();
    assert_eq!(ordinary_video, expected_video);
    assert_eq!(
        ordinary
            .iter()
            .filter(|event| event.identity.track.kind == MediaKind::Video
                && event.event_kind == EventKind::Frame)
            .count(),
        50
    );
    for track in [0, 1] {
        let actual_video: Vec<_> = program
            .iter()
            .filter(|event| {
                event.identity.track.kind == MediaKind::Video
                    && event.identity.track.wire_id == track
                    && event.event_kind == EventKind::Frame
            })
            .map(|event| event.dts_ms)
            .collect();
        assert_eq!(
            actual_video,
            expected_video
                .iter()
                .map(|timestamp| timestamp + 22)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            program
                .iter()
                .filter(|event| event.identity.track.kind == MediaKind::Video
                    && event.identity.track.wire_id == track
                    && event.event_kind == EventKind::Frame)
                .count(),
            50
        );
        assert!(
            program
                .iter()
                .any(|event| event.identity.track.kind == MediaKind::Audio
                    && event.identity.track.wire_id == track
                    && event.event_kind == EventKind::Frame)
        );
    }
    let encoded: Vec<_> = program
        .iter()
        .filter(|event| {
            event.identity.track.kind == MediaKind::Audio
                && event.identity.track.wire_id == 1
                && event.event_kind == EventKind::Frame
        })
        .map(|event| (event.dts_ms, event.payload().to_vec()))
        .collect();
    assert_ne!(encoded, expected_audio);
    assert_eq!(encoded[0].0, offset + 1);
    assert!(
        encoded
            .windows(2)
            .all(|packets| packets[0].0 < packets[1].0)
    );
    let mut received_flv = HEADER.to_vec();
    for event in program.iter().filter(|event| {
        event.identity.track.kind == MediaKind::Audio && event.identity.track.wire_id == 1
    }) {
        FlvTag::from_event(event, 65536)
            .unwrap()
            .with_audio_track(0, 65536)
            .unwrap()
            .write_to(&mut received_flv)
            .await
            .unwrap();
    }
    let reference = ffmpeg_bytes(
        fixture.clone(),
        &[
            "-copyts",
            "-f",
            "flv",
            "-i",
            "pipe:0",
            "-map",
            "0:a:1",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-b:a",
            "160k",
            "-output_ts_offset",
            "0.022",
            "-avoid_negative_ts",
            "disabled",
            "-f",
            "flv",
            "pipe:1",
        ],
    )
    .await;
    let decode = [
        "-f", "flv", "-i", "pipe:0", "-map", "0:a:0", "-ac", "2", "-f", "f32le", "pipe:1",
    ];
    let received_pcm = ffmpeg_bytes(received_flv, &decode).await;
    let reference_pcm = ffmpeg_bytes(reference, &decode).await;
    assert_eq!(
        received_pcm, reference_pcm,
        "Auch der vollständige AAC-Encoder-Vorlauf bleibt erhalten"
    );
    let source_pcm = ffmpeg_bytes(
        fixture,
        &[
            "-f", "flv", "-i", "pipe:0", "-map", "0:a:1", "-ac", "2", "-f", "f32le", "pipe:1",
        ],
    )
    .await;
    assert_eq!(received_pcm.len(), source_pcm.len() + 1024 * 2 * 4);
}

async fn ffmpeg_bytes(bytes: Vec<u8>, arguments: &[&str]) -> Vec<u8> {
    let mut child = Command::new(FFMPEG)
        .args(["-nostdin", "-v", "error"])
        .args(arguments)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = tokio::spawn(async move {
        stdin.write_all(&bytes).await.unwrap();
    });
    let output = timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    writer.await.unwrap();
    assert!(output.status.success());
    output.stdout
}

#[tokio::test]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokales TLS."]
async fn selected_auxiliary_revision_still_rejects_both_public_preludes() {
    for program in [false, true] {
        let (server, target) = endpoint().await;
        assert!(matches!(
            start_public(&engine(), source_events(true).await, target, program, 7).await,
            Err(MediaError::UnsupportedProfile)
        ));
        assert!(
            timeout(Duration::from_millis(30), server.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokales TLS."]
async fn independent_auxiliary_second_header_must_not_reject_prelude() {
    for duplicate in [false, true] {
        let (first, mut receive) = input(source_events(duplicate).await);
        let prepared = engine()
            .prepare_source(first, &mut receive, &HashSet::from([0, 1]))
            .await
            .expect("Ungewählter Aux-Header darf den gemeinsamen Vorlauf nicht abbrechen");
        assert!(!prepared.prefix.is_empty());
        assert!(
            prepared
                .prefix
                .iter()
                .all(|event| event.identity.track.kind != MediaKind::Audio
                    || event.identity.track.wire_id != 7)
        );
    }
}

#[tokio::test]
async fn ignored_audio_still_obeys_identity_and_prelude_limits() {
    for case in 0..5 {
        let mut events = source_events(true).await;
        let mut limits = MediaLimits::default();
        let expected = match case {
            0 => {
                events[1].identity.generation = source_events(false).await[0].identity.generation;
                MediaError::WrongSession
            }
            1 => {
                events[1].identity.session = AuthorizedSession::new(2, 1).unwrap();
                MediaError::WrongSession
            }
            2 => {
                limits.queue_events = 1;
                MediaError::Backpressure
            }
            3 => {
                events.retain(|event| event.identity.track.wire_id == 7);
                limits.max_tag_bytes = events[0].wire_body().len();
                limits.queue_bytes = limits.max_tag_bytes + 15;
                MediaError::Backpressure
            }
            _ => {
                limits.max_tag_bytes = events[0].wire_body().len() - 1;
                MediaError::InvalidMedia
            }
        };
        // Die Fehler müssen vor FFprobe auftreten; kein externer Medienbuild nötig.
        let engine = MediaEngine::new(EngineConfig {
            ffmpeg: "/usr/bin/false".into(),
            ffprobe: "/usr/bin/false".into(),
            work_directory: "/tmp".into(),
            limits,
        })
        .unwrap();
        let (first, mut receive) = input(events);
        let result = engine
            .prepare_source(first, &mut receive, &HashSet::from([0]))
            .await;
        assert_eq!(result.err(), Some(expected), "Zusatzspur-Grenzfall {case}");
    }
}

#[tokio::test]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; kein Netzwerk."]
async fn independent_wrapped_aac_must_survive_worker_reader_limit() {
    let mut source = FlvReader::new(FIXTURE, 65536);
    let mut audio = Vec::new();
    while let Some(tag) = source.next().await.unwrap() {
        if tag.audio_track().unwrap() == Some(0) {
            audio.push(tag);
        }
    }
    let max = audio.iter().map(|tag| tag.body().len()).max().unwrap();
    let mut bytes = HEADER.to_vec();
    let header = audio.iter().find(|tag| tag.is_sequence_header()).unwrap();
    header.write_to(&mut bytes).await.unwrap();
    header
        .with_audio_track(1, max + 5)
        .unwrap()
        .write_to(&mut bytes)
        .await
        .unwrap();
    for frame in audio.iter().filter(|tag| !tag.is_sequence_header()) {
        frame.write_to(&mut bytes).await.unwrap();
        frame
            .with_audio_track(1, max + 5)
            .unwrap()
            .write_to(&mut bytes)
            .await
            .unwrap();
    }
    let mut child = Command::new(FFMPEG)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "flv",
            "-i",
            "pipe:0",
            "-map",
            "0:a",
            "-c:a",
            "copy",
            "-f",
            "flv",
            "-flvflags",
            "no_metadata+no_duration_filesize",
            "pipe:1",
        ])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let write = tokio::spawn(async move {
        stdin.write_all(&bytes).await.unwrap();
    });
    let output = timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    write.await.unwrap();
    assert!(output.status.success());
    let mut reader = FlvReader::new(&output.stdout[..], max + 5);
    let mut largest = 0;
    while let Some(tag) = reader.next().await.unwrap() {
        largest = largest.max(tag.body().len());
    }
    assert_eq!(largest, max + 5);
    eprintln!("AAC-Eingangskörper {max}, kopierter Mehrspur-AAC-Körper {largest}");
    let mut worker = FlvReader::worker_output(
        &output.stdout[..],
        &MediaLimits {
            max_tag_bytes: max,
            ..MediaLimits::default()
        },
    );
    while worker
        .next()
        .await
        .expect("Erlaubtes AAC muss den echten Worker-Reader passieren")
        .is_some()
    {}
}

#[tokio::test]
#[ignore = "Benötigt geprüften FFmpeg 8 und ausschließlich lokale TLS-Verbindungen."]
async fn obs_shaped_header_order_metadata_large_keyframe_and_60fps_keep_probe_diagnosis() {
    let mut reader = FlvReader::new(FIXTURE, 2 * 1024 * 1024);
    let mut tags = Vec::new();
    while let Some(tag) = reader.next().await.unwrap() {
        tags.push(tag);
    }
    let audio_header = tags
        .iter()
        .find(|tag| {
            tag.kind() == 8 && tag.is_sequence_header() && tag.audio_track().unwrap() == Some(0)
        })
        .unwrap();
    let video_header = tags
        .iter()
        .find(|tag| tag.kind() == 9 && tag.is_sequence_header())
        .unwrap();
    let video_frame = tags
        .iter()
        .find(|tag| tag.kind() == 9 && !tag.is_sequence_header())
        .unwrap();
    let audio_frame = tags
        .iter()
        .find(|tag| {
            tag.kind() == 8 && !tag.is_sequence_header() && tag.audio_track().unwrap() == Some(0)
        })
        .unwrap();
    let certificates = tls::test_tls();
    let mut limits = IngestLimits::local_probe();
    limits.max_event_bytes = 2 * 1024 * 1024;
    limits.max_queued_bytes = 8 * 1024 * 1024;
    limits.rtmp.chunk.max_message_bytes = 2 * 1024 * 1024;
    limits.rtmp.chunk.max_partial_bytes = 8 * 1024 * 1024;
    let server = IngestServer::bind_loopback(0, certificates.server, Arc::new(Auth), limits)
        .await
        .unwrap();
    let target = PublishTarget {
        id: "obs-shaped-local".into(),
        endpoint: format!(
            "rtmps://localhost:{}/live",
            server.local_addr().unwrap().port()
        ),
        playpath: PublishSecret::new(b"review-synthetic".to_vec()).unwrap(),
        tls: Some(certificates.client),
        allowed_hosts: vec!["localhost".into()],
        allow_loopback: true,
        allow_unencrypted: false,
    };
    let captured = tokio::spawn(capture(server));
    let producer = RunningPusher::start(target, MediaLimits::default())
        .await
        .unwrap();
    producer.try_send(Arc::new(audio_header.clone())).unwrap();
    producer.try_send(Arc::new(video_header.clone())).unwrap();
    producer
        .try_send(Arc::new(
            FlvTag::new(
                9,
                0,
                Arc::from(&b"\x94avc1\x03\x00\x00\x09"[..]),
                2 * 1024 * 1024,
            )
            .unwrap(),
        ))
        .unwrap();
    let mut large = video_frame.body().to_vec();
    let filler_size = 600 * 1024u32;
    large.extend_from_slice(&filler_size.to_be_bytes());
    large.push(12);
    large.extend(std::iter::repeat_n(0xff, filler_size as usize - 2));
    large.push(0x80);
    producer
        .try_send(Arc::new(
            FlvTag::new(9, 0, Arc::from(large), 2 * 1024 * 1024).unwrap(),
        ))
        .unwrap();
    for frame in 1..=60u32 {
        let timestamp = (frame * 1000 + 30) / 60;
        producer
            .try_send(Arc::new(
                FlvTag::new(9, timestamp, Arc::from(video_frame.body()), 2 * 1024 * 1024).unwrap(),
            ))
            .unwrap();
        producer
            .try_send(Arc::new(
                FlvTag::new(8, timestamp, Arc::from(audio_frame.body()), 2 * 1024 * 1024).unwrap(),
            ))
            .unwrap();
    }
    producer.finish().await.unwrap();
    let events = captured.await.unwrap();
    assert_eq!(events[0].identity.track.kind, MediaKind::Audio);
    assert_eq!(events[1].identity.track.kind, MediaKind::Video);
    assert_eq!(events[2].event_kind, EventKind::Metadata);
    assert!(events[3].wire_body().len() > 500 * 1024);
    let (first, mut receiver) = input(events);
    let mut diagnostic = PreparationDiagnostic::default();
    diagnostic.request_probe_dump();
    let result = engine()
        .prepare_source_diagnosed(first, &mut receiver, &HashSet::from([0]), &mut diagnostic)
        .await;
    let source = result.expect("Gültiger OBS-Vorlauf muss trotz BrokenPipe vorbereitet werden");
    assert_eq!(
        (source.observation.width, source.observation.height),
        (320, 180)
    );
    assert_eq!(source.observation.audio.len(), 1);
    assert_eq!(source.observation.audio[0].codec, "aac");
    assert_eq!(source.observation.audio[0].sample_rate, 48000);
    assert_eq!(diagnostic.phase, "observation");
    assert_eq!(diagnostic.ffprobe_exit_code, Some(0));
    assert!(diagnostic.ffprobe_stderr_first_line.is_some());
    assert_eq!(diagnostic.io_error_kind, Some("broken_pipe"));
    assert!(diagnostic.probe_result_available);
    assert!(diagnostic.take_probe_dump().is_none());
    let diagnostic = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(diagnostic["probe_streams"], 2);
    assert_eq!(diagnostic["probe"][0]["kind"], "video");
    assert_eq!(diagnostic["probe"][0]["codec"], "h264");
    assert_eq!(diagnostic["probe"][0]["width"], 320);
    assert_eq!(diagnostic["probe"][0]["height"], 180);
    assert_eq!(diagnostic["probe"][1]["kind"], "audio");
    assert_eq!(diagnostic["probe"][1]["codec"], "aac");
    assert_eq!(diagnostic["probe"][1]["sample_rate"], 48000);
}
