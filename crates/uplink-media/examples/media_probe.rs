//! Gekoppelte lokale Medienprobe; kein OBS-, Plattform- oder Kapazitätsnachweis.
//! cargo run -p uplink-media --example media_probe -- --ffmpeg /pfad/ffmpeg --ffprobe /pfad/ffprobe
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{ExitCode, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};
use uplink_core::{Codec, FrameRate};
use uplink_ingest::{
    AuthorizedSession, Authorizer, EndReason, EventKind, IngestLimits, IngestServer, MediaKind,
    SessionReport,
    rustls::{
        self,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use uplink_media::{
    DesiredOutput, DesiredSessionSpec, DesiredVideo, EngineConfig, MediaEngine, MediaLimits,
    OutputState, PublishSecret, PublishTarget, flv::FlvReader, pusher::RunningPusher,
};

type ProbeResult<T> = Result<T, &'static str>;
const DEADLINE: Duration = Duration::from_secs(30);
const H264: &[u8] = include_bytes!("../../../experiments/scuffle-probe/fixtures/h264.flv");

struct ProbeAuth;
impl Authorizer for ProbeAuth {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        if app == "live" && stream == "local-fixture" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}
struct Task<T>(Option<JoinHandle<T>>);
impl<T> Task<T> {
    fn new(task: JoinHandle<T>) -> Self {
        Self(Some(task))
    }
    async fn finish(mut self) -> ProbeResult<T> {
        let result = timeout(DEADLINE, self.0.as_mut().ok_or("Probe-Task fehlt")?)
            .await
            .map_err(|_| "Probe-Task überschritt die Frist")?
            .map_err(|_| "Probe-Task wurde abgebrochen")?;
        self.0.take();
        Ok(result)
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}
struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn create() -> ProbeResult<Self> {
        for _ in 0..16 {
            let mut suffix = [0; 8];
            getrandom::fill(&mut suffix).map_err(|_| "Zufall nicht verfügbar")?;
            let path = PathBuf::from(format!(
                "/tmp/ump-{}",
                suffix
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("Privater Probeordner konnte nicht angelegt werden"),
            }
        }
        Err("Kein freier Probeordner")
    }
    fn remove(self) -> ProbeResult<()> {
        fs::remove_dir(&self.0)
            .map_err(|_| "Medienworker hat den Probeordner nicht vollständig freigegeben")
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn tls() -> ProbeResult<(Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>)> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .map_err(|_| "Testzertifikat konnte nicht erzeugt werden")?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(cert.der().clone())
        .map_err(|_| "Test-CA ungültig")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS-Versionen ungültig")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    // Privater Schlüssel ausschließlich im RAM; kein PEM und keine Datei.
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS-Versionen ungültig")?
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .map_err(|_| "Testzertifikat ungültig")?;
    Ok((Arc::new(server), Arc::new(client)))
}
async fn endpoint(
    id: &str,
    queued_events: usize,
) -> ProbeResult<(Arc<IngestServer<ProbeAuth>>, PublishTarget)> {
    let (server_tls, client_tls) = tls()?;
    let mut limits = IngestLimits::local_probe();
    limits.max_queued_events = queued_events;
    limits.media_idle_timeout = DEADLINE;
    let server = Arc::new(
        IngestServer::bind_loopback(0, server_tls, Arc::new(ProbeAuth), limits)
            .await
            .map_err(|_| "Lokaler Probe-Eingang nicht verfügbar")?,
    );
    let port = server
        .local_addr()
        .map_err(|_| "Lokale Adresse fehlt")?
        .port();
    let target = PublishTarget {
        id: id.into(),
        endpoint: format!("rtmps://localhost:{port}/live"),
        playpath: PublishSecret::new(b"local-fixture".to_vec())
            .map_err(|_| "Testautorisierung ungültig")?,
        tls: Some(client_tls),
        allowed_hosts: vec!["localhost".into()],
        allow_loopback: true,
        allow_unencrypted: false,
    };
    Ok((server, target))
}
#[derive(PartialEq, Eq)]
struct Packet {
    timestamp: u32,
    pts: i64,
    hash: [u8; 32],
}
struct Capture {
    video: Vec<Packet>,
    video_headers: Vec<Packet>,
    audio: BTreeMap<u8, Vec<Packet>>,
    report: SessionReport,
}
async fn capture(server: Arc<IngestServer<ProbeAuth>>) -> ProbeResult<Capture> {
    capture_notifying(server, None).await
}
async fn capture_notifying(
    server: Arc<IngestServer<ProbeAuth>>,
    mut first_frame: Option<oneshot::Sender<Instant>>,
) -> ProbeResult<Capture> {
    let mut connection = server
        .accept()
        .await
        .map_err(|_| "Ausgangsverbindung fehlt")?;
    let mut video = Vec::new();
    let mut video_headers = Vec::new();
    let mut audio: BTreeMap<u8, Vec<Packet>> = BTreeMap::new();
    while let Some(event) = connection.next().await {
        if event.identity.track.kind == MediaKind::Video
            && event.event_kind == EventKind::Frame
            && let Some(notice) = first_frame.take()
        {
            let _ = notice.send(Instant::now());
        }
        if event.event_kind != EventKind::Frame
            && !(event.identity.track.kind == MediaKind::Video
                && event.event_kind == EventKind::SequenceHeader)
        {
            continue;
        }
        let packet = Packet {
            timestamp: event.dts_ms,
            pts: event.pts_ms,
            hash: Sha256::digest(if event.identity.track.kind == MediaKind::Video {
                event.wire_body()
            } else {
                event.payload()
            })
            .into(),
        };
        if event.event_kind == EventKind::SequenceHeader {
            video_headers.push(packet);
            continue;
        }
        match event.identity.track.kind {
            MediaKind::Video => video.push(packet),
            MediaKind::Audio => audio
                .entry(event.identity.track.wire_id)
                .or_default()
                .push(packet),
        }
    }
    Ok(Capture {
        video,
        video_headers,
        audio,
        report: connection.finish().await,
    })
}
fn desired(
    target: PublishTarget,
    live_audio_track: u8,
    vod_audio_track: Option<u8>,
) -> ProbeResult<DesiredOutput> {
    Ok(DesiredOutput {
        target,
        video: DesiredVideo {
            width: 256,
            height: 144,
            fps: FrameRate::new(25, 1).map_err(|_| "Testbildrate ungültig")?,
            bitrate_kbps: 384,
            codec: Codec::H264,
        },
        live_audio_track,
        vod_audio_track,
        layout: None,
    })
}
#[derive(Serialize)]
struct ProbeReport {
    source: &'static str,
    output: &'static str,
    encode_groups: usize,
    video_decoders: usize,
    identical_video_packets: usize,
    live_audio_packets: usize,
    vod_audio_packets: usize,
    different_audio_signals: bool,
    slow_receiver_isolated: bool,
    common_timestamp_offset_ms: i64,
    source_audio_ids: [u8; 2],
    stalled_handshake_isolated: bool,
    healthy_first_frame_ms: Option<u128>,
    healthy_ended_ms: Option<u128>,
}
#[derive(Default)]
struct InputTimes {
    video: Vec<(u32, i64)>,
    audio: BTreeMap<u8, Vec<(u32, i64)>>,
}
impl InputTimes {
    fn add(&mut self, event: &uplink_ingest::MediaEvent) {
        if event.event_kind != EventKind::Frame {
            return;
        }
        let time = (event.dts_ms, event.pts_ms);
        match event.identity.track.kind {
            MediaKind::Video => self.video.push(time),
            MediaKind::Audio => self
                .audio
                .entry(event.identity.track.wire_id)
                .or_default()
                .push(time),
        }
    }
}
fn offset(input: &[(u32, i64)], output: &[Packet]) -> ProbeResult<i64> {
    if input.len() != output.len() || input.is_empty() {
        return Err("Zeitlinien haben unterschiedliche Paketanzahlen");
    }
    let delta = i64::from(output[0].timestamp) - i64::from(input[0].0);
    if input.iter().zip(output).any(|((dts, pts), packet)| {
        i64::from(packet.timestamp) - i64::from(*dts) != delta || packet.pts - *pts != delta
    }) {
        return Err("Medienzeitstempel weisen keinen gemeinsamen konstanten Versatz auf");
    }
    Ok(delta)
}
async fn run(
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    fixture: Arc<[u8]>,
    source_name: &'static str,
    with_slow: bool,
    audio_ids: [u8; 2],
    with_stalled: bool,
) -> ProbeResult<ProbeReport> {
    let directory = PrivateDirectory::create()?;
    let (source_server, source_target) = endpoint("source", 512).await?;
    let (left_server, left_target) = endpoint("left", 512).await?;
    let (right_server, right_target) = endpoint("right", 512).await?;
    let (first_frame, first_frame_rx) = oneshot::channel();
    let left = Task::new(tokio::spawn(capture_notifying(
        left_server,
        Some(first_frame),
    )));
    let right = Task::new(tokio::spawn(capture(right_server)));
    let mut outputs = vec![
        desired(left_target, audio_ids[0], Some(audio_ids[1]))?,
        desired(right_target, audio_ids[1], None)?,
    ];
    let mut slow = None;
    let mut stalled = None;
    if with_stalled {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "Handshake-Probeport fehlt")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Handshake-Probeadresse fehlt")?
            .port();
        outputs.insert(
            0,
            desired(
                PublishTarget {
                    id: "stalled-handshake".into(),
                    endpoint: format!("rtmp://localhost:{port}/live"),
                    playpath: PublishSecret::new(b"local-fixture".to_vec())
                        .map_err(|_| "Probezugang ungültig")?,
                    tls: None,
                    allowed_hosts: vec!["localhost".into()],
                    allow_loopback: true,
                    allow_unencrypted: true,
                },
                audio_ids[0],
                None,
            )?,
        );
        stalled = Some(Task::new(tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .map_err(|_| "Hängender Handshake wurde nicht verbunden")?;
            let mut request = [0; 1537];
            socket
                .read_exact(&mut request)
                .await
                .map_err(|_| "RTMP-Handshakestart fehlt")?;
            let mut byte = [0];
            let count = socket
                .read(&mut byte)
                .await
                .map_err(|_| "Handshake-Abschluss fehlgeschlagen")?;
            if count != 0 {
                return Err("Unerwartete Daten ohne bestätigten Handshake");
            }
            Ok::<_, &'static str>(())
        })));
    }
    let mut release_slow = None;
    if with_slow {
        let (server, target) = endpoint("slow", 1).await?;
        outputs.push(desired(target, audio_ids[0], None)?);
        let (release, wait) = oneshot::channel();
        release_slow = Some(release);
        slow = Some(Task::new(tokio::spawn(async move {
            let mut connection = server
                .accept()
                .await
                .map_err(|_| "Langsames Testziel fehlt")?;
            // Der erste Event bleibt gehalten; weitere Events füllen genau eine
            // Queueposition. Der echte Eingang muss danach Backpressure melden.
            let held = connection.next().await;
            let _ = wait.await;
            while connection.next().await.is_some() {}
            drop(held);
            Ok::<_, &'static str>(connection.finish().await)
        })));
    }
    let source = Task::new(tokio::spawn(async move {
        let pusher = RunningPusher::start(source_target, MediaLimits::default())
            .await
            .map_err(|_| "Quell-RTMPS konnte nicht starten")?;
        let mut input = FlvReader::new(fixture.as_ref(), 65536);
        let mut tags = Vec::new();
        while let Some(mut tag) = input.next().await.map_err(|_| "Quellfixture ungültig")? {
            if audio_ids != [0, 1] {
                // Zweite Zeitachse: verhindert einen zufällig auf 0/21 ms
                // passenden Fix. CTS bleibt unverändert im komprimierten Tag.
                tag = uplink_media::flv::FlvTag::new(
                    tag.kind(),
                    tag.timestamp_ms()
                        .checked_add(137)
                        .ok_or("Probezeitstempel übergelaufen")?,
                    Arc::from(tag.body()),
                    65536,
                )
                .map_err(|_| "Verschobener Probe-Tag ungültig")?;
            }
            if tag.kind() == 8 {
                let id = tag
                    .audio_track()
                    .map_err(|_| "Quelltonspur ungültig")?
                    .ok_or("Quelltonspur fehlt")?;
                let destination = *audio_ids
                    .get(id as usize)
                    .ok_or("Zusätzliche Quelltonspur")?;
                tag = tag
                    .with_audio_track(destination, 65536)
                    .map_err(|_| "Quelltonspur konnte nicht zugeordnet werden")?;
            }
            tags.push(tag);
            if tags.len() > 512 {
                return Err("Zu viele Probe-Tags");
            }
        }
        if audio_ids != [0, 1] {
            let indices: Vec<_> = tags
                .iter()
                .enumerate()
                .filter(|(_, tag)| tag.kind() == 8 && tag.is_sequence_header())
                .map(|(index, _)| index)
                .collect();
            if indices.len() != 2 {
                return Err("Probe benötigt genau zwei Audioheader");
            }
            tags.swap(indices[0], indices[1]);
        }
        for tag in tags {
            pusher
                .try_send(Arc::new(tag))
                .map_err(|_| "Quell-Queue überlastet")?;
        }
        pusher
            .finish()
            .await
            .map_err(|_| "Quell-RTMPS nicht sauber beendet")?;
        Ok::<_, &'static str>(())
    }));
    let mut connection = source_server
        .accept()
        .await
        .map_err(|_| "Quellverbindung fehlt")?;
    let first = match connection.next().await {
        Some(first) => first,
        None => {
            let reason = connection.finish().await.reason;
            eprintln!("Quellabschluss ohne Medien: {reason:?}");
            source.finish().await??;
            return Err("Keine echte Quellmedien empfangen");
        }
    };
    let (send, receive) = mpsc::channel(512);
    let mut input_times = InputTimes::default();
    input_times.add(&first);
    let feed = Task::new(tokio::spawn(async move {
        while let Some(event) = connection.next().await {
            input_times.add(&event);
            if send.send(event).await.is_err() {
                break;
            }
        }
        drop(send);
        (connection.finish().await, input_times)
    }));
    let engine = MediaEngine::new(EngineConfig {
        ffmpeg,
        ffprobe,
        work_directory: directory.0.clone(),
        limits: MediaLimits::default(),
    })
    .map_err(|_| "Medienkonfiguration nicht akzeptiert")?;
    let started_at = Instant::now();
    let running = engine
        .prepare_and_start(DesiredSessionSpec { first, outputs }, receive)
        .await
        .map_err(|error| {
            eprintln!("Vorprüfung: {error}");
            "Quellprüfung oder Medienstart fehlgeschlagen"
        })?;
    eprintln!("Gemessene Quelle: {:?}", running.source_observation());
    let healthy_first_frame_ms =
        if with_stalled {
            let received = timeout(Duration::from_secs(2), first_frame_rx)
                .await
                .map_err(|_| "Hängender Handshake blockiert den gesunden Medienfluss")?
                .map_err(|_| "Gesunder Empfänger meldete kein Videobild")?;
            if !running.status().outputs.iter().any(|output| {
                output.id == "stalled-handshake" && output.state == OutputState::Starting
            }) {
                return Err(
                    "Gesunder Medienfluss wurde nicht während des fremden Handshakes nachgewiesen",
                );
            }
            Some(received.duration_since(started_at).as_millis())
        } else {
            None
        };
    let healthy_ended_ms = if with_stalled {
        Some(
            timeout(Duration::from_secs(2), async {
                loop {
                    let state = running.status();
                    if ["left", "right"].iter().all(|id| {
                        state
                            .outputs
                            .iter()
                            .any(|output| output.id == *id && output.state == OutputState::Ended)
                    }) {
                        if !state.outputs.iter().any(|output| {
                            output.id == "stalled-handshake"
                                && output.state == OutputState::Starting
                        }) {
                            return Err(
                                "Gesunde Ziele wurden erst nach dem fremden Handshake beendet",
                            );
                        }
                        return Ok(Instant::now().duration_since(started_at).as_millis());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| "Fremder Handshake blockiert den Abschluss gesunder Ziele")??,
        )
    } else {
        None
    };
    let report = running.finish().await;
    if let Some(stalled) = stalled {
        stalled.finish().await??;
    }
    source.finish().await??;
    let (input_report, input_times) = feed.finish().await?;
    if input_report.reason != EndReason::ExplicitStop {
        return Err("Eingang wurde nicht explizit abgeschlossen");
    }
    if let Some(release) = release_slow {
        let _ = release.send(());
    }
    let slow_report = if let Some(slow) = slow {
        Some(slow.finish().await??)
    } else {
        None
    };
    let left = left.finish().await??;
    let right = right.finish().await??;
    if left.report.reason != EndReason::ExplicitStop
        || right.report.reason != EndReason::ExplicitStop
    {
        return Err("Ein reguläres Ziel wurde nicht sauber abgeschlossen");
    }
    if report.error.is_some() {
        eprintln!("Medienbericht: {report:?}");
        return Err("Medienverarbeitung meldete einen Fehler");
    }
    if report.status.encode_groups != 1 || report.status.video_decoders != 1 {
        return Err("Geteiltes Encoding wurde nicht bestätigt");
    }
    if left.video.is_empty()
        || left.video != right.video
        || left.video_headers.is_empty()
        || left.video_headers != right.video_headers
    {
        return Err("Videopakete der beiden Ausgänge unterscheiden sich");
    }
    let live = left.audio.get(&0).ok_or("Live-Audio fehlt")?;
    let vod = left.audio.get(&1).ok_or("VOD-Audio fehlt")?;
    let second = right
        .audio
        .get(&0)
        .ok_or("Gewähltes Audio des zweiten Ziels fehlt")?;
    if live.is_empty() || vod.is_empty() || live == vod || vod != second || right.audio.len() != 1 {
        return Err("Tatsächliche Audioauswahl wurde nicht bestätigt");
    }
    let timestamp_offset = offset(&input_times.video, &left.video)?;
    for (track, packets) in [(audio_ids[0], live), (audio_ids[1], vod)] {
        let audio_offset = offset(
            input_times
                .audio
                .get(&track)
                .ok_or("Eingangszeitlinie der Tonspur fehlt")?,
            packets,
        )?;
        if audio_offset != timestamp_offset {
            eprintln!(
                "Gemessener Versatz: Video {timestamp_offset} ms, Audio {track}: {audio_offset} ms"
            );
            return Err("Audio- und Videozeitstempel haben unterschiedlichen Versatz");
        }
    }
    let isolated = if let Some(slow) = slow_report {
        slow.reason == EndReason::Backpressure
            && report
                .status
                .outputs
                .iter()
                .find(|o| o.id == "slow")
                .is_some_and(|o| matches!(o.state, OutputState::Failed(_)))
    } else {
        false
    };
    if with_slow && !isolated {
        return Err("Isolation des langsamen Empfängers wurde nicht bestätigt");
    }
    let stalled_isolated = with_stalled
        && report.status.outputs.iter().any(|output| {
            output.id == "stalled-handshake" && matches!(output.state, OutputState::Failed(_))
        });
    if with_stalled && !stalled_isolated {
        return Err("Hängender Handshake endete ohne sichtbaren Fehler");
    }
    let result = ProbeReport {
        source: source_name,
        output: "H.264 256×144/25, 384 kbit/s, lokale TLS-Ziele",
        encode_groups: report.status.encode_groups,
        video_decoders: report.status.video_decoders,
        identical_video_packets: left.video.len(),
        live_audio_packets: live.len(),
        vod_audio_packets: vod.len(),
        different_audio_signals: true,
        slow_receiver_isolated: isolated,
        common_timestamp_offset_ms: timestamp_offset,
        source_audio_ids: audio_ids,
        stalled_handshake_isolated: stalled_isolated,
        healthy_first_frame_ms,
        healthy_ended_ms,
    };
    directory.remove()?;
    Ok(result)
}
async fn ffmpeg_listener(ffmpeg: &std::path::Path) -> ProbeResult<()> {
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "FFmpeg-Probeport fehlt")?;
    let port = reserved
        .local_addr()
        .map_err(|_| "FFmpeg-Probeadresse fehlt")?
        .port();
    drop(reserved);
    // Die Listenadresse enthält ausschließlich die Anwendung, keinen Publish-Key.
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-listen", "1", "-i"])
        .arg(format!("rtmp://127.0.0.1:{port}/live"))
        .args(["-map", "0", "-c", "copy", "-f", "flv", "pipe:1"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Unabhängiger FFmpeg-Empfänger konnte nicht starten")?;
    let stdout = child.stdout.take().ok_or("FFmpeg-Empfängerausgabe fehlt")?;
    let capture = Task::new(tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "FFmpeg-Empfängerausgabe unterbrochen")?;
        if bytes.len() > 1_048_576 {
            return Err("FFmpeg-Empfänger überschreitet sein RAM-Budget");
        }
        Ok::<_, &'static str>(bytes)
    }));
    let operation = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let pusher = loop {
            let target = PublishTarget {
                id: "ffmpeg-independent".into(),
                endpoint: format!("rtmp://localhost:{port}/live"),
                playpath: PublishSecret::new(b"local-fixture".to_vec())
                    .map_err(|_| "Testautorisierung ungültig")?,
                tls: None,
                allowed_hosts: vec!["localhost".into()],
                allow_loopback: true,
                allow_unencrypted: true,
            };
            match RunningPusher::start(target, MediaLimits::default()).await {
                Ok(pusher) => break pusher,
                Err(uplink_media::MediaError::Io)
                    if tokio::time::Instant::now() < deadline
                        && matches!(child.try_wait(), Ok(None)) =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                Err(error) => {
                    eprintln!("Unabhängiger RTMP-Empfänger: {error}");
                    return Err("Nativer Publisher wurde von FFmpeg nicht bestätigt");
                }
            }
        };
        let mut source = FlvReader::new(H264, 65536);
        while let Some(tag) = source
            .next()
            .await
            .map_err(|_| "H264-Probequelle ungültig")?
        {
            pusher
                .try_send(Arc::new(tag))
                .map_err(|_| "Unabhängige RTMP-Probe überlastet")?;
        }
        pusher.finish().await.map_err(|error| {
            eprintln!("Unabhängiger RTMP-Abschluss: {error}");
            "Unabhängige RTMP-Verbindung nicht sauber abgeschlossen"
        })?;
        let bytes = capture.finish().await??;
        if !child
            .wait()
            .await
            .map_err(|_| "FFmpeg-Empfänger konnte nicht abgeholt werden")?
            .success()
        {
            return Err("FFmpeg-Empfänger meldete einen Fehler");
        }
        let source = copy_packets(H264).await?;
        let output = copy_packets(&bytes).await?;
        if source.len() != 3 || output.len() != 3 {
            return Err("FFmpeg hat nicht alle drei Medienspuren empfangen");
        }
        let mut common = None;
        let mut packets = 0;
        for (track, input) in source {
            let output = output.get(&track).ok_or("FFmpeg-Empfängerspur fehlt")?;
            if input.len() != output.len()
                || input.iter().zip(output).any(|(a, b)| a.hash != b.hash)
            {
                return Err("FFmpeg-Empfänger hat komprimierte Medien verändert");
            }
            let times: Vec<_> = input.iter().map(|p| (p.timestamp, p.pts)).collect();
            let shift = offset(&times, output)?;
            if common.is_some_and(|v| v != shift) {
                return Err("FFmpeg-Empfänger verschiebt Audio und Video unterschiedlich");
            }
            common = Some(shift);
            packets += output.len();
        }
        println!(
            "{}",
            serde_json::json!({"independent_receiver":"FFmpeg RTMP listener","streams":3,"identical_compressed_packets":packets,"common_timestamp_offset_ms":common})
        );
        Ok(())
    };
    let result = timeout(DEADLINE, operation)
        .await
        .map_err(|_| "Unabhängige RTMP-Probe überschritt die Frist")
        .and_then(|r| r);
    if result.is_err() {
        let _ = child.start_kill();
        timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| "FFmpeg-Empfänger konnte nicht beendet werden")?
            .map_err(|_| "FFmpeg-Empfänger konnte nicht abgeholt werden")?;
    }
    result
}
async fn copy_packets(bytes: &[u8]) -> ProbeResult<BTreeMap<(u8, u8), Vec<Packet>>> {
    let mut reader = FlvReader::new(bytes, 65536);
    let mut tracks: BTreeMap<(u8, u8), Vec<Packet>> = BTreeMap::new();
    while let Some(tag) = reader
        .next()
        .await
        .map_err(|_| "FFmpeg-Empfänger lieferte ungültiges FLV")?
    {
        if tag.kind() == 18 || tag.body().get(1) != Some(&1) {
            continue;
        }
        let (track, prefix, pts) = if tag.kind() == 9 {
            if tag.body().len() < 5 || tag.body()[0] & 15 != 7 {
                return Err("FFmpeg-Empfänger lieferte unerwartetes Video");
            }
            (
                0,
                5,
                i64::from(tag.timestamp_ms())
                    + i64::from(
                        i32::from_be_bytes([tag.body()[2], tag.body()[3], tag.body()[4], 0]) >> 8,
                    ),
            )
        } else {
            (
                tag.audio_track()
                    .map_err(|_| "FFmpeg-Audiospur ungültig")?
                    .ok_or("FFmpeg-Audiospur fehlt")?,
                if tag.body()[0] >> 4 == 10 { 2 } else { 7 },
                i64::from(tag.timestamp_ms()),
            )
        };
        tracks.entry((tag.kind(), track)).or_default().push(Packet {
            timestamp: tag.timestamp_ms(),
            pts,
            hash: Sha256::digest(
                tag.body()
                    .get(prefix..)
                    .ok_or("FFmpeg-Paket abgeschnitten")?,
            )
            .into(),
        });
    }
    Ok(tracks)
}
async fn generate_av1(ffmpeg: &std::path::Path) -> ProbeResult<Arc<[u8]>> {
    let mut child = Command::new(ffmpeg)
        .args([
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
            "libsvtav1",
            "-flags",
            "+global_header",
            "-preset",
            "12",
            "-svtav1-params",
            "lp=2",
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
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Synthetischer AV1-Generator konnte nicht starten")?;
    let stdout = child.stdout.take().ok_or("AV1-Generatorausgabe fehlt")?;
    let operation = async {
        let mut bytes = Vec::new();
        stdout
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "AV1-Generatorausgabe unterbrochen")?;
        if bytes.len() > 1_048_576 {
            return Err("AV1-Probequelle überschreitet ihr RAM-Budget");
        }
        if !child
            .wait()
            .await
            .map_err(|_| "AV1-Generator konnte nicht abgeschlossen werden")?
            .success()
        {
            return Err("AV1-Generator meldete einen Fehler");
        }
        Ok(Arc::from(bytes))
    };
    match timeout(DEADLINE, operation).await {
        Ok(Ok(bytes)) => Ok(bytes),
        result => {
            let _ = child.start_kill();
            timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "AV1-Generator konnte nicht beendet werden")?
                .map_err(|_| "AV1-Generator konnte nicht abgeholt werden")?;
            match result {
                Ok(Err(error)) => Err(error),
                _ => Err("AV1-Generator überschritt die Frist"),
            }
        }
    }
}
fn arguments() -> ProbeResult<(PathBuf, PathBuf)> {
    let mut args = std::env::args_os().skip(1);
    let mut ffmpeg = None;
    let mut ffprobe = None;
    while let Some(flag) = args.next() {
        let destination = if flag == "--ffmpeg" {
            &mut ffmpeg
        } else if flag == "--ffprobe" {
            &mut ffprobe
        } else {
            return Err("Aufruf: media_probe --ffmpeg /absolut/ffmpeg --ffprobe /absolut/ffprobe");
        };
        if destination.is_some() {
            return Err("Werkzeugpfad wurde doppelt angegeben");
        }
        let path = PathBuf::from(args.next().ok_or("Werkzeugpfad fehlt")?);
        if !path.is_absolute() || !path.is_file() {
            return Err("Werkzeugpfad muss auf eine vorhandene absolute Datei zeigen");
        }
        *destination = Some(path);
    }
    Ok((
        ffmpeg.ok_or("--ffmpeg fehlt")?,
        ffprobe.ok_or("--ffprobe fehlt")?,
    ))
}
#[tokio::main]
async fn main() -> ExitCode {
    let result = async {
        let (ffmpeg, ffprobe) = arguments()?;
        ffmpeg_listener(&ffmpeg).await?;
        let av1 = generate_av1(&ffmpeg).await?;
        for (fixture, name, slow, audio_ids, stalled) in [
            (
                Arc::from(H264),
                "H.264 320×180/25 + AAC 440/880 Hz",
                false,
                [0, 1],
                true,
            ),
            (
                av1.clone(),
                "AV1 BT.709 320×180/25 + AAC 440/880 Hz",
                false,
                [0, 1],
                false,
            ),
            (
                av1.clone(),
                "AV1 BT.709 320×180/25 + AAC 440/880 Hz",
                true,
                [0, 1],
                false,
            ),
            (
                av1,
                "AV1 BT.709 320×180/25 + AAC 440/880 Hz, vertauschte Header",
                false,
                [7, 12],
                false,
            ),
        ] {
            let report = timeout(
                DEADLINE,
                run(
                    ffmpeg.clone(),
                    ffprobe.clone(),
                    fixture,
                    name,
                    slow,
                    audio_ids,
                    stalled,
                ),
            )
            .await
            .map_err(|_| "Gekoppelte Medienprobe überschritt die Frist")??;
            println!(
                "{}",
                serde_json::to_string(&report)
                    .map_err(|_| "Probebericht konnte nicht erzeugt werden")?
            );
        }
        Ok::<_, &'static str>(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
