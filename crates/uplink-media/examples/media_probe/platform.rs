use super::*;
use uplink_core::{
    Chroma, Color, ColorPrimaries, ColorRange, Gop, LayoutRevision, Matrix, RateControl, RateMode,
    Transfer, VideoProfile,
};
use uplink_media::{
    Composition, Crop, LayoutSpec, ProgramAudio, ProgramOutput, ProgramSessionSpec, ProgramVideo,
};

struct MultiCapture {
    video: BTreeMap<u8, Vec<Packet>>,
    audio: BTreeMap<u8, Vec<Packet>>,
    headers: BTreeMap<(u8, u8), usize>,
    report: SessionReport,
}
async fn receive(server: Arc<IngestServer<ProbeAuth>>) -> ProbeResult<MultiCapture> {
    let mut connection = server
        .accept()
        .await
        .map_err(|_| "Mehrvideo-Ausgang fehlt")?;
    let mut video: BTreeMap<u8, Vec<Packet>> = BTreeMap::new();
    let mut audio: BTreeMap<u8, Vec<Packet>> = BTreeMap::new();
    let mut headers = BTreeMap::new();
    while let Some(event) = connection.next().await {
        let kind = if event.identity.track.kind == MediaKind::Video {
            9
        } else {
            8
        };
        let id = event.identity.track.wire_id;
        if event.event_kind == EventKind::SequenceHeader {
            *headers.entry((kind, id)).or_default() += 1;
        }
        if event.event_kind != EventKind::Frame {
            continue;
        }
        let packet = Packet {
            timestamp: event.dts_ms,
            pts: event.pts_ms,
            hash: Sha256::digest(event.payload()).into(),
        };
        if kind == 9 {
            video.entry(id).or_default().push(packet);
        } else {
            audio.entry(id).or_default().push(packet);
        }
    }
    Ok(MultiCapture {
        video,
        audio,
        headers,
        report: connection.finish().await,
    })
}
fn profile(width: u32, height: u32) -> ProbeResult<VideoProfile> {
    Ok(VideoProfile {
        width,
        height,
        fps: FrameRate::new(25, 1).map_err(|_| "Probe-FPS ungültig")?,
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
    })
}

pub(super) async fn run(ffmpeg: &std::path::Path, ffprobe: &std::path::Path) -> ProbeResult<()> {
    let fixture = generate_source(ffmpeg, Codec::H264).await?;
    let directory = PrivateDirectory::create()?;
    let (source_server, source_target) = endpoint("source", 512).await?;
    let (multi_server, multi_target) = endpoint("multi", 512).await?;
    let (shared_server, shared_target) = endpoint("shared", 512).await?;
    let (missing_server, missing_target) = endpoint("missing-vod", 512).await?;
    let multi = Task::new(tokio::spawn(receive(multi_server)));
    let shared = Task::new(tokio::spawn(receive(shared_server)));
    let producer = Task::new(tokio::spawn(async move {
        let pusher = RunningPusher::start(source_target, MediaLimits::default())
            .await
            .map_err(|_| "Mehrvideoquelle konnte nicht starten")?;
        let mut reader = FlvReader::new(fixture.as_ref(), 65536);
        while let Some(tag) = reader
            .next()
            .await
            .map_err(|_| "Mehrvideoquelle ungültig")?
        {
            if tag.kind() == 18 {
                continue;
            }
            pusher
                .try_send(Arc::new(tag))
                .map_err(|_| "Mehrvideoquelle überlastet")?;
        }
        pusher
            .finish()
            .await
            .map_err(|_| "Mehrvideoquelle wurde nicht abgeschlossen")?;
        Ok::<_, &'static str>(())
    }));
    let mut input = source_server
        .accept()
        .await
        .map_err(|_| "Mehrvideoeingang fehlt")?;
    let first = input
        .next()
        .await
        .ok_or("Mehrvideoeingang hat keine Medien")?;
    let (send, receive) = mpsc::channel(512);
    let feed = Task::new(tokio::spawn(async move {
        while let Some(event) = input.next().await {
            if send.send(event).await.is_err() {
                break;
            }
        }
        drop(send);
        input.finish().await
    }));
    let horizontal = profile(256, 144)?;
    let vertical = profile(144, 256)?;
    let outputs = vec![
        ProgramOutput {
            target: multi_target,
            video: vec![
                ProgramVideo {
                    wire_track: 0,
                    canvas_index: 0,
                    profile: horizontal.clone(),
                    layout: None,
                },
                ProgramVideo {
                    wire_track: 5,
                    canvas_index: 1,
                    profile: vertical,
                    layout: Some(LayoutSpec {
                        revision: LayoutRevision { id: 1, revision: 1 },
                        composition: Composition::Crop(Crop {
                            x: 100,
                            y: 0,
                            width: 100,
                            height: 180,
                        }),
                    }),
                },
            ],
            audio: vec![
                ProgramAudio {
                    source_wire_track: 0,
                    destination_wire_track: 2,
                },
                ProgramAudio {
                    source_wire_track: 1,
                    destination_wire_track: 9,
                },
            ],
        },
        ProgramOutput {
            target: shared_target,
            video: vec![ProgramVideo {
                wire_track: 0,
                canvas_index: 0,
                profile: horizontal.clone(),
                layout: None,
            }],
            audio: vec![ProgramAudio {
                source_wire_track: 1,
                destination_wire_track: 0,
            }],
        },
        ProgramOutput {
            target: missing_target,
            video: vec![ProgramVideo {
                wire_track: 0,
                canvas_index: 0,
                profile: horizontal,
                layout: None,
            }],
            audio: vec![ProgramAudio {
                source_wire_track: 12,
                destination_wire_track: 0,
            }],
        },
    ];
    let engine = MediaEngine::new(EngineConfig {
        ffmpeg: ffmpeg.into(),
        ffprobe: ffprobe.into(),
        work_directory: directory.0.clone(),
        limits: MediaLimits::default(),
    })
    .map_err(|_| "Mehrvideo-Enginekonfiguration ungültig")?;
    let running = engine
        .prepare_program_and_start(ProgramSessionSpec { first, outputs }, receive)
        .await
        .map_err(|_| "Mehrvideo-Vorprüfung fehlgeschlagen")?;
    eprintln!("Mehrvideo-Quellmessung: {:?}", running.source_observation());
    let report = timeout(DEADLINE, running.finish())
        .await
        .map_err(|_| "Mehrvideo-Engine überschritt Frist")?;
    if report.error.is_some() {
        eprintln!("Mehrvideo-Abschluss: {report:?}");
        return Err("Mehrvideo-Engine meldete einen Fehler");
    }
    producer.finish().await??;
    let source_report = feed.finish().await?;
    let left = multi.finish().await??;
    let right = shared.finish().await??;
    if report.error.is_some()
        || report.status.encode_groups != 2
        || report.status.video_decoders != 1
        || report.status.outputs[0].state != OutputState::LocalEndUnconfirmed
        || report.status.outputs[1].state != OutputState::LocalEndUnconfirmed
        || report.status.outputs[2].state
            != OutputState::Failed(uplink_media::MediaError::MissingTrack)
    {
        eprintln!("Mehrvideo-Abschluss: {:?}", report);
        return Err("Mehrvideo-Status, Encoderwiederverwendung oder Zielisolation fehlerhaft");
    }
    if timeout(Duration::from_millis(50), missing_server.accept())
        .await
        .is_ok()
    {
        return Err("Fehlender VOD-Mix öffnete trotzdem ein Ziel");
    }
    if left.video.keys().copied().collect::<Vec<_>>() != vec![0, 5]
        || right.video.len() != 1
        || left.video[&0] != right.video[&0]
        || left.video[&0].len() != 50
        || left.video[&5].len() != 50
        || left.audio.keys().copied().collect::<Vec<_>>() != vec![2, 9]
        || right.audio.len() != 1
        || left.audio[&9] != right.audio[&0]
        || left.audio[&2].len() != 95
        || left.audio[&9].len() != 95
        || left.audio[&2]
            .iter()
            .zip(&left.audio[&9])
            .all(|(a, b)| a.hash == b.hash)
        || left.headers.values().any(|count| *count != 1)
        || left.headers.len() != 4
    {
        return Err("Mehrvideo-Pakete, Header oder Audio-Rollen stimmen nicht");
    }
    if [
        source_report.reason,
        left.report.reason,
        right.report.reason,
    ]
    .into_iter()
    .any(|r| !matches!(r, EndReason::PeerClosed | EndReason::ExplicitStop))
    {
        return Err("Mehrvideo-RTMPS wurde nicht vollständig beendet");
    }
    println!(
        "{}",
        serde_json::json!({"local_multivideo":true,"video_wire_ids":[0,5],"audio_wire_ids":[2,9],"video_frames_per_track":50,"audio_frames_per_track":95,"encodes":2,"decoders":1,"shared_horizontal_bitstream":true,"missing_vod_target_isolated":true,"twitch_live_proven":false})
    );
    directory.remove()?;
    Ok(())
}
