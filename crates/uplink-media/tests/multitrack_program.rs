use std::{
    collections::BTreeMap, os::unix::fs::DirBuilderExt, process::Stdio, sync::Arc, time::Duration,
};
use tokio::{
    process::Command,
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, timeout},
};
use uplink_core::{
    Chroma, Codec, Color, ColorPrimaries, ColorRange, FrameRate, Gop, LayoutRevision, Matrix,
    RateControl, RateMode, Transfer, VideoProfile,
};
use uplink_ingest::{
    AuthorizedSession, Authorizer, EndReason, EventKind, IngestLimits, IngestServer, MediaEvent,
    MediaKind, SessionReport,
};
use uplink_media::{
    Composition, Crop, EngineConfig, LayoutSpec, MediaEngine, MediaError, MediaLimits, OutputState,
    ProgramAudio, ProgramOutput, ProgramSessionSpec, ProgramVideo, PublishSecret, PublishTarget,
    flv::FlvReader, pusher::RunningPusher,
};

const FFMPEG: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg";
const FFPROBE: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffprobe";
const FRIST: Duration = Duration::from_secs(45);

struct Auth;
impl Authorizer for Auth {
    async fn authorize(
        &self,
        app: &str,
        stream: &str,
    ) -> std::result::Result<AuthorizedSession, ()> {
        if app == "live" && stream == "mehrspur-synthetisch" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

#[path = "../../uplink-ingest/tests/support/tls.rs"]
mod tls;

async fn server_und_ziel(id: &'static str) -> (Arc<IngestServer<Auth>>, PublishTarget) {
    let zertifikate = tls::test_tls();
    drop(zertifikate.certificate_pem);
    let mut limits = IngestLimits::local_probe();
    limits.max_queued_events = 2048;
    limits.max_event_bytes = 2 * 1024 * 1024;
    limits.max_queued_bytes = 8 * 1024 * 1024;
    limits.rtmp.chunk.max_message_bytes = 2 * 1024 * 1024;
    limits.rtmp.chunk.max_partial_bytes = 8 * 1024 * 1024;
    limits.media_idle_timeout = FRIST;
    let server = IngestServer::bind_loopback(0, zertifikate.server, Arc::new(Auth), limits)
        .await
        .unwrap();
    let port = server.local_addr().unwrap().port();
    let ziel = PublishTarget {
        id: id.into(),
        endpoint: format!("rtmps://localhost:{port}/live"),
        playpath: PublishSecret::new(b"mehrspur-synthetisch".to_vec()).unwrap(),
        tls: Some(zertifikate.client),
        allowed_hosts: vec!["localhost".into()],
        allow_loopback: true,
        allow_unencrypted: false,
    };
    (Arc::new(server), ziel)
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Paket {
    dts: u32,
    pts: i64,
    körper: Vec<u8>,
}

#[derive(Default)]
struct SpurCapture {
    video: BTreeMap<u8, Vec<Paket>>,
    video_header: BTreeMap<u8, Vec<u8>>,
    video_header_zahl: BTreeMap<u8, usize>,
    audio: BTreeMap<u8, Vec<Paket>>,
    report: Option<SessionReport>,
}

async fn aufnehmen(server: Arc<IngestServer<Auth>>) -> SpurCapture {
    let mut verbindung = server.accept().await.unwrap();
    let mut capture = SpurCapture::default();
    while let Some(event) = verbindung.next().await {
        if event.event_kind == EventKind::SequenceHeader {
            if event.identity.track.kind == MediaKind::Video {
                let spur = event.identity.track.wire_id;
                *capture.video_header_zahl.entry(spur).or_default() += 1;
                capture
                    .video_header
                    .entry(spur)
                    .or_default()
                    .extend_from_slice(event.wire_body());
            }
            continue;
        }
        if event.event_kind != EventKind::Frame {
            continue;
        }
        let paket = Paket {
            dts: event.dts_ms,
            pts: event.pts_ms,
            körper: if event.identity.track.kind == MediaKind::Video {
                event.wire_body().to_vec()
            } else {
                event.payload().to_vec()
            },
        };
        match event.identity.track.kind {
            MediaKind::Video => capture
                .video
                .entry(event.identity.track.wire_id)
                .or_default()
                .push(paket),
            MediaKind::Audio => capture
                .audio
                .entry(event.identity.track.wire_id)
                .or_default()
                .push(paket),
        }
    }
    capture.report = Some(verbindung.finish().await);
    capture
}

fn erfasse(server: Arc<IngestServer<Auth>>) -> Task<SpurCapture> {
    Task::neu(tokio::spawn(aufnehmen(server)))
}

async fn quelle_generieren() -> Vec<u8> {
    let child = Command::new(FFMPEG)
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-x264-params",
            "colorprim=bt709:transfer=bt709:colormatrix=bt709",
            "-flags",
            "+global_header",
            "-threads",
            "2",
            "-b:v",
            "384k",
            "-g",
            "50",
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_range",
            "tv",
            "-c:a",
            "aac",
            "-b:a",
            "48k",
            "-f",
            "flv",
            "pipe:1",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "Synthetische Quelle fehlgeschlagen: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

async fn quelle_einspeisen(
    fixture: &[u8],
) -> (
    MediaEvent,
    mpsc::Sender<MediaEvent>,
    mpsc::Receiver<MediaEvent>,
    Vec<MediaEvent>,
    Vec<(u32, i64)>,
    BTreeMap<u8, Vec<(u32, i64)>>,
) {
    let (server, ziel) = server_und_ziel("quelle").await;
    let sammeln = tokio::spawn(async move {
        let mut verbindung = server.accept().await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = verbindung.next().await {
            events.push(event);
        }
        assert_eq!(verbindung.finish().await.reason, EndReason::ExplicitStop);
        events
    });
    let pusher = RunningPusher::start(ziel, MediaLimits::default())
        .await
        .unwrap();
    let mut leser = FlvReader::new(fixture, 65536);
    while let Some(tag) = leser.next().await.unwrap() {
        if matches!(tag.kind(), 8 | 9) {
            pusher.try_send(Arc::new(tag)).unwrap();
        }
    }
    pusher.finish().await.unwrap();
    let events = timeout(Duration::from_secs(5), sammeln)
        .await
        .unwrap()
        .unwrap();
    let mut video_zeit = Vec::new();
    let mut audio_zeit: BTreeMap<u8, Vec<(u32, i64)>> = BTreeMap::new();
    for event in &events {
        if event.event_kind != EventKind::Frame {
            continue;
        }
        match event.identity.track.kind {
            MediaKind::Video => video_zeit.push((event.dts_ms, event.pts_ms)),
            MediaKind::Audio => audio_zeit
                .entry(event.identity.track.wire_id)
                .or_default()
                .push((event.dts_ms, event.pts_ms)),
        }
    }
    let mut events = events.into_iter();
    let first = events.next().unwrap();
    let (send, receive) = mpsc::channel(512);
    (
        first,
        send,
        receive,
        events.collect(),
        video_zeit,
        audio_zeit,
    )
}

fn profil(codec: Codec, width: u32, height: u32) -> VideoProfile {
    VideoProfile {
        width,
        height,
        fps: FrameRate::new(25, 1).unwrap(),
        codec,
        codec_profile: if codec == Codec::H264 { "high" } else { "main" }.into(),
        level: if codec == Codec::H264 { "4.2" } else { "5.1" }.into(),
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

fn hochkant_layout() -> LayoutSpec {
    LayoutSpec {
        revision: LayoutRevision { id: 7, revision: 1 },
        composition: Composition::Crop(Crop {
            x: 100,
            y: 0,
            width: 100,
            height: 180,
        }),
    }
}

fn mehrspur_video() -> Vec<ProgramVideo> {
    vec![
        ProgramVideo {
            wire_track: 0,
            canvas_index: 0,
            profile: profil(Codec::H264, 256, 144),
            layout: None,
        },
        ProgramVideo {
            wire_track: 5,
            canvas_index: 1,
            profile: profil(Codec::Hevc, 144, 256),
            layout: Some(hochkant_layout()),
        },
    ]
}

fn mehrspur_audio() -> Vec<ProgramAudio> {
    vec![
        ProgramAudio {
            source_wire_track: 0,
            destination_wire_track: 2,
        },
        ProgramAudio {
            source_wire_track: 1,
            destination_wire_track: 9,
        },
    ]
}

struct EngineLauf {
    engine: MediaEngine,
    ordner: std::path::PathBuf,
}
impl EngineLauf {
    fn starten() -> Self {
        let mut suffix = [0_u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let ordner =
            std::path::PathBuf::from(format!("/tmp/mehrspur-{:016x}", u64::from_ne_bytes(suffix)));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&ordner)
            .unwrap();
        let engine = MediaEngine::new(EngineConfig {
            ffmpeg: FFMPEG.into(),
            ffprobe: FFPROBE.into(),
            work_directory: ordner.clone(),
            limits: MediaLimits::default(),
        })
        .unwrap();
        Self { engine, ordner }
    }
}
impl Drop for EngineLauf {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.ordner);
    }
}

struct Task<T>(Option<JoinHandle<T>>);
impl<T> Task<T> {
    fn neu(task: JoinHandle<T>) -> Self {
        Self(Some(task))
    }
    async fn fertig(&mut self) -> T {
        let task = self.0.as_mut().expect("Task fehlt bei der Auswertung");
        let ergebnis = timeout(FRIST, task)
            .await
            .expect("Task überschritt die Frist")
            .expect("Task wurde abgebrochen");
        let _ = self.0.take();
        ergebnis
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

fn versatz(eingang: &[(u32, i64)], ausgang: &[Paket]) -> i64 {
    assert_eq!(
        eingang.len(),
        ausgang.len(),
        "Zeitlinie hat {} Eingangs- und {} Ausgangspakete",
        eingang.len(),
        ausgang.len()
    );
    let versatz = i64::from(ausgang[0].dts) - i64::from(eingang[0].0);
    assert!(
        eingang
            .iter()
            .zip(ausgang)
            .all(
                |((dts, pts), paket)| i64::from(paket.dts) - i64::from(*dts) == versatz
                    && paket.pts - *pts == versatz
            ),
        "Zeitstempel folgen keinem gemeinsamen konstanten Versatz"
    );
    versatz
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokale TLS-Verbindungen."]
async fn mixed_codec_program_output_preserves_headers_tracks_and_timebase() {
    let fixture = quelle_generieren().await;
    let (first, send, receive, rest, eingang_video, eingang_audio) =
        quelle_einspeisen(&fixture).await;
    for event in rest {
        send.try_send(event).unwrap();
    }
    drop(send);
    let (multi_server, multi_ziel) = server_und_ziel("multi").await;
    let (shared_server, shared_ziel) = server_und_ziel("shared").await;
    let mut multi = erfasse(multi_server);
    let mut shared = erfasse(shared_server);
    let lauf = EngineLauf::starten();
    let running = lauf
        .engine
        .prepare_program_and_start(
            ProgramSessionSpec {
                first,
                outputs: vec![
                    ProgramOutput {
                        target: multi_ziel,
                        video: mehrspur_video(),
                        audio: mehrspur_audio(),
                    },
                    ProgramOutput {
                        target: shared_ziel,
                        video: vec![ProgramVideo {
                            wire_track: 0,
                            canvas_index: 0,
                            profile: profil(Codec::H264, 256, 144),
                            layout: None,
                        }],
                        audio: vec![ProgramAudio {
                            source_wire_track: 1,
                            destination_wire_track: 0,
                        }],
                    },
                ],
            },
            receive,
        )
        .await
        .expect("Mehrspurvortest muss mit synthetischer BT.709-Quelle gelingen");
    assert_eq!(running.status().encode_groups, 2);
    assert_eq!(running.status().video_decoders, 1);
    let report = timeout(FRIST, running.finish()).await.unwrap();
    assert_eq!(
        report.error, None,
        "Mehrspur-Abschluss: {:?}",
        report.status.outputs
    );
    for output in &report.status.outputs {
        assert_eq!(output.state, OutputState::LocalEndUnconfirmed);
    }
    let multi = multi.fertig().await;
    let shared = shared.fertig().await;
    assert_eq!(
        multi.video.keys().copied().collect::<Vec<_>>(),
        vec![0, 5],
        "Mehrspurziel trägt beide Videodrähte"
    );
    assert_eq!(
        multi.video[&0].len(),
        eingang_video.len(),
        "H.264-Spur trägt alle Eingangsbilder"
    );
    assert_eq!(
        multi.video[&5].len(),
        eingang_video.len(),
        "HEVC-Spur trägt alle Eingangsbilder"
    );
    assert_eq!(
        multi.audio.keys().copied().collect::<Vec<_>>(),
        vec![2, 9],
        "Mehrspurziel trägt beide Audio-Rollen"
    );
    assert_eq!(multi.audio[&2].len(), eingang_audio[&0].len());
    assert_eq!(multi.audio[&9].len(), eingang_audio[&1].len());
    assert_eq!(
        multi.video[&0], shared.video[&0],
        "Geteiltes Profil liefert bitidentischen H.264-Bitstream"
    );
    assert_eq!(
        multi.audio[&9], shared.audio[&0],
        "Geteilte Audiorolle liefert identische Pakete"
    );
    for (wire, zahl) in &multi.video_header_zahl {
        assert_eq!(*zahl, 1, "Videospur {wire} trägt genau einen Sequenzheader");
    }
    assert!(multi.video_header.contains_key(&0));
    assert!(multi.video_header.contains_key(&5));
    assert_eq!(shared.video_header.get(&0), multi.video_header.get(&0));
    let hevc_header = &multi.video_header[&5];
    assert_eq!(
        &hevc_header[2..6],
        b"hvc1",
        "HEVC-Spur muss als hvc1 markiert sein, Kopf {:?}",
        &hevc_header[..hevc_header.len().min(8)]
    );
    assert_ne!(
        hevc_header[0] & 0x80,
        0,
        "HEVC-Sequenzheader nutzt das erweiterte FLV-Format"
    );
    for frame in &multi.video[&5] {
        assert_ne!(
            frame.körper[0] & 0x80,
            0,
            "Jedes HEVC-Bild bleibt im erweiterten Format"
        );
        assert!(
            frame.körper.len() > 6 && &frame.körper[2..6] == b"hvc1",
            "HEVC-Bild behält seine hvc1-Kennung: {:?}",
            &frame.körper[..frame.körper.len().min(8)]
        );
    }
    let video_versatz = versatz(&eingang_video, &multi.video[&0]);
    assert_eq!(
        versatz(&eingang_video, &multi.video[&5]),
        video_versatz,
        "Beide Videoencodes folgen derselben Zeitlinie"
    );
    for (quelle, rolle) in [(0_u8, 2_u8), (1, 9)] {
        let eingang = &eingang_audio[&quelle];
        let ausgang = &multi.audio[&rolle];
        assert!(
            eingang
                .iter()
                .zip(ausgang)
                .all(|((dts, pts), paket)| *dts == paket.dts && *pts == paket.pts),
            "Audio-Rolle {rolle} behält exakte Zeitstempel"
        );
    }
    assert_eq!(
        video_versatz, 0,
        "Video und kopiertes Audio laufen auf derselben Zeitachse"
    );
    assert!(
        matches!(
            multi.report.unwrap().reason,
            EndReason::PeerClosed | EndReason::ExplicitStop
        ),
        "Mehrspurziel endet nicht sauber"
    );
    assert!(matches!(
        shared.report.unwrap().reason,
        EndReason::PeerClosed | EndReason::ExplicitStop
    ));
    assert!(
        std::fs::read_dir(&lauf.ordner).unwrap().next().is_none(),
        "Nach dem Abschluss bleiben keine Workerreste im Arbeitsordner"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokale TLS-Verbindungen."]
async fn dead_target_does_not_stop_healthy_multitrack_target() {
    let fixture = quelle_generieren().await;
    let (first, send, receive, rest, eingang_video, _eingang_audio) =
        quelle_einspeisen(&fixture).await;
    for event in rest {
        send.try_send(event).unwrap();
    }
    drop(send);
    let (tot_server, tot_ziel) = server_und_ziel("totes-ziel").await;
    let (gesund_server, gesund_ziel) = server_und_ziel("gesundes-ziel").await;
    let mut tot = Task::neu(tokio::spawn(async move {
        let mut verbindung = tot_server.accept().await.unwrap();
        let mut bilder = 0;
        while let Some(event) = verbindung.next().await {
            if event.event_kind == EventKind::Frame && event.identity.track.kind == MediaKind::Video
            {
                bilder += 1;
            }
            if bilder >= 8 {
                drop(verbindung);
                return bilder;
            }
        }
        bilder
    }));
    let mut gesund = erfasse(gesund_server);
    let lauf = EngineLauf::starten();
    let running = lauf
        .engine
        .prepare_program_and_start(
            ProgramSessionSpec {
                first,
                outputs: vec![
                    ProgramOutput {
                        target: tot_ziel,
                        video: mehrspur_video(),
                        audio: mehrspur_audio(),
                    },
                    ProgramOutput {
                        target: gesund_ziel,
                        video: mehrspur_video(),
                        audio: mehrspur_audio(),
                    },
                ],
            },
            receive,
        )
        .await
        .unwrap();
    let report = timeout(FRIST, running.finish()).await.unwrap();
    assert_eq!(
        report.error, None,
        "Ein totes Ziel darf die Session nicht mit einem Fehler beenden"
    );
    let tot_bilder = tot.fertig().await;
    assert!(
        tot_bilder < eingang_video.len(),
        "Das tote Ziel sollte mitten im Stream enden, bekam {tot_bilder} Bilder"
    );
    let gesund = gesund.fertig().await;
    assert!(!gesund.video.is_empty(), "Gesundes Ziel bleibt leer");
    assert_eq!(
        gesund.video[&0].len(),
        eingang_video.len(),
        "H.264-Spur des gesunden Ziels trägt alle Bilder"
    );
    assert_eq!(
        gesund.video[&5].len(),
        eingang_video.len(),
        "HEVC-Spur des gesunden Ziels trägt alle Bilder"
    );
    assert_eq!(
        gesund.audio[&2].len(),
        gesund.audio[&9].len(),
        "Beide Audio-Rollen des gesunden Ziels füllen sich gleich"
    );
    assert_eq!(report.status.outputs.len(), 2);
    assert!(
        matches!(report.status.outputs[0].state, OutputState::Failed(_)),
        "Das tote Ziel endet als Fehler: {:?}",
        report.status.outputs
    );
    assert_eq!(
        report.status.outputs[1].state,
        OutputState::LocalEndUnconfirmed,
        "Das gesunde Ziel läuft bis zum sauberen Ende durch"
    );
    assert!(
        std::fs::read_dir(&lauf.ordner).unwrap().next().is_none(),
        "Teilfehler hinterlässt keine Workerreste"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Benötigt den installierten geprüften FFmpeg-8-Build; ausschließlich lokale TLS-Verbindungen."]
async fn explicit_stop_releases_multitrack_resources() {
    let fixture = quelle_generieren().await;
    let (first, send, receive, mut rest, _eingang_video, _eingang_audio) =
        quelle_einspeisen(&fixture).await;
    let ungefüttert = rest.split_off(rest.len().saturating_sub(5));
    for event in rest {
        send.try_send(event).unwrap();
    }
    let (links_server, links_ziel) = server_und_ziel("links").await;
    let (rechts_server, rechts_ziel) = server_und_ziel("rechts").await;
    let mut links = erfasse(links_server);
    let mut rechts = erfasse(rechts_server);
    let lauf = EngineLauf::starten();
    let running = lauf
        .engine
        .prepare_program_and_start(
            ProgramSessionSpec {
                first,
                outputs: vec![
                    ProgramOutput {
                        target: links_ziel,
                        video: mehrspur_video(),
                        audio: mehrspur_audio(),
                    },
                    ProgramOutput {
                        target: rechts_ziel,
                        video: mehrspur_video(),
                        audio: mehrspur_audio(),
                    },
                ],
            },
            receive,
        )
        .await
        .unwrap();
    let mut geliefert = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        geliefert = running
            .status()
            .outputs
            .iter()
            .any(|output| output.received_events > 0);
        if geliefert {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        geliefert,
        "Session liefert vor dem Stopp keine Medien aus: {:?}",
        running.status()
    );
    let report = timeout(FRIST, running.stop()).await.unwrap();
    drop(send);
    drop(ungefüttert);
    assert_eq!(
        report.error,
        Some(MediaError::Cancelled),
        "Expliziter Stopp meldet sich als Abbruch: {:?}",
        report.status.outputs
    );
    for output in &report.status.outputs {
        assert_eq!(
            output.state,
            OutputState::Failed(MediaError::Cancelled),
            "Jedes Ziel endet beim Stopp als abgebrochen"
        );
    }
    for mut ziel in [links, rechts] {
        let capture = timeout(Duration::from_secs(5), ziel.fertig())
            .await
            .expect("Zielaufnahme endet nach dem Stopp nicht");
        assert!(
            matches!(
                capture.report.unwrap().reason,
                EndReason::PeerClosed | EndReason::ExplicitStop
            ),
            "Zielverbindung wird nach dem Stopp beendet"
        );
    }
    assert!(
        std::fs::read_dir(&lauf.ordner).unwrap().next().is_none(),
        "Nach dem Stopp bleiben Sockets oder Workerreste im Arbeitsordner"
    );
    drop(lauf);
}
