use crate::{
    flv::{FlvTag, HEADER},
    graph::Graph,
    *,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    process::Stdio,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::mpsc,
    time::{Instant, timeout, timeout_at},
};
use uplink_ingest::{EventKind, MediaKind};

/// Auch beim Abbruch des aufrufenden Futures behält eine Aufräumtask den Kindprozess.
pub(crate) struct ProbeChild {
    pub(crate) child: Option<Child>,
    pub(crate) deadline: Duration,
}
impl ProbeChild {
    pub(crate) async fn terminate(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.start_kill();
            }
            timeout(self.deadline, child.wait())
                .await
                .map_err(|_| MediaError::ProcessCleanupFailed)?
                .map_err(|_| MediaError::ProcessCleanupFailed)?;
        }
        self.child.take();
        Ok(())
    }
}
impl Drop for ProbeChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let deadline = self.deadline;
            let _ = child.start_kill();
            tokio::spawn(async move {
                // Das Ergebnis bleibt bei abgebrochenem Aufrufer nicht mehr zustellbar.
                // Trotzdem wird der tote Prozess abgeholt; kein wartender Fremdprozess.
                if timeout(deadline, child.wait()).await.is_err() {
                    let _ = child.start_kill();
                    if timeout(deadline, child.wait()).await.is_err() {
                        eprintln!("Medienprobe konnte nicht rechtzeitig beendet werden.");
                    }
                }
            });
        }
    }
}

#[derive(Deserialize)]
struct ProbeDocument {
    streams: Vec<ProbeStream>,
}
#[derive(Deserialize)]
struct ProbeStream {
    codec_type: String,
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    r_frame_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u8>,
    color_primaries: Option<String>,
    color_transfer: Option<String>,
    color_space: Option<String>,
    color_range: Option<String>,
}

pub const MAX_PROBE_DUMP_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default, serde::Serialize)]
pub struct PreparationDiagnostic {
    probe_result_available: bool,
    probe_dump_status: Option<&'static str>,
    probe_dump_bytes: Option<usize>,
    #[serde(skip)]
    probe_dump_requested: bool,
    #[serde(skip)]
    probe_dump: Option<Vec<u8>>,
    phase: &'static str,
    probe_streams: usize,
    probe: Vec<ProbeDiagnostic>,
    requested: Vec<RequestedDiagnostic>,
    ffprobe_exit_code: Option<i32>,
    io_error_kind: Option<&'static str>,
    ffprobe_stderr_first_line: Option<&'static str>,
}

#[derive(serde::Serialize)]
struct RequestedDiagnostic {
    platform: &'static str,
    width: u32,
    height: u32,
    fps_numerator: u32,
    fps_denominator: u32,
    bitrate_kbps: u32,
    codec: &'static str,
    live_audio_track: u8,
    vod_audio_track: Option<u8>,
}

#[derive(serde::Serialize)]
struct ProbeDiagnostic {
    kind: Option<&'static str>,
    codec: Option<&'static str>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<(u32, u32)>,
    pixel_format: Option<&'static str>,
    color_primaries: Option<&'static str>,
    color_transfer: Option<&'static str>,
    color_matrix: Option<&'static str>,
    color_range: Option<&'static str>,
    sample_rate: Option<u32>,
    channels: Option<u8>,
}

fn diagnostic_label(value: Option<&str>, allowed: &[&'static str]) -> Option<&'static str> {
    value.map(|value| {
        allowed
            .iter()
            .copied()
            .find(|candidate| *candidate == value)
            .unwrap_or("other")
    })
}

impl PreparationDiagnostic {
    pub fn request_probe_dump(&mut self) {
        self.probe_dump_requested = true;
    }
    pub fn take_probe_dump(&mut self) -> Option<Vec<u8>> {
        self.probe_dump.take()
    }
    pub fn probe_dump_finished(&mut self, status: &'static str) {
        self.probe_dump_status = Some(status);
    }
    fn retain_probe_dump(&mut self, bytes: Vec<u8>) {
        if !self.probe_dump_requested || self.probe_streams != 0 {
            return;
        }
        self.probe_dump_bytes = Some(bytes.len());
        if bytes.len() > MAX_PROBE_DUMP_BYTES {
            self.probe_dump_status = Some("too_large");
        } else {
            self.probe_dump_status = Some("pending_write");
            self.probe_dump = Some(bytes);
        }
    }
    fn probe(&mut self, document: &ProbeDocument) {
        self.probe_result_available = true;
        self.probe_streams = document.streams.len();
        self.probe = document
            .streams
            .iter()
            .take(32)
            .map(|stream| ProbeDiagnostic {
                kind: diagnostic_label(Some(&stream.codec_type), &["video", "audio"]),
                codec: diagnostic_label(
                    Some(&stream.codec_name),
                    &["av1", "h264", "hevc", "aac", "opus", "mp3"],
                ),
                width: stream.width,
                height: stream.height,
                fps: stream
                    .r_frame_rate
                    .as_deref()
                    .and_then(|value| value.split_once('/'))
                    .and_then(|(n, d)| Some((n.parse().ok()?, d.parse().ok()?))),
                pixel_format: diagnostic_label(
                    stream.pix_fmt.as_deref(),
                    &[
                        "yuv420p",
                        "yuv420p10le",
                        "yuv422p",
                        "yuv422p10le",
                        "yuv444p",
                        "yuv444p10le",
                        "nv12",
                        "p010le",
                        "gbrp",
                        "gbrp10le",
                    ],
                ),
                color_primaries: diagnostic_label(
                    stream.color_primaries.as_deref(),
                    &[
                        "unknown",
                        "unspecified",
                        "bt709",
                        "bt2020",
                        "smpte170m",
                        "bt470bg",
                        "smpte432",
                    ],
                ),
                color_transfer: diagnostic_label(
                    stream.color_transfer.as_deref(),
                    &[
                        "unknown",
                        "unspecified",
                        "bt709",
                        "smpte170m",
                        "smpte2084",
                        "arib-std-b67",
                        "iec61966-2-1",
                        "linear",
                        "bt2020-10",
                        "bt2020-12",
                    ],
                ),
                color_matrix: diagnostic_label(
                    stream.color_space.as_deref(),
                    &[
                        "unknown",
                        "unspecified",
                        "bt709",
                        "smpte170m",
                        "gbr",
                        "bt2020nc",
                        "bt2020c",
                        "bt470bg",
                    ],
                ),
                color_range: diagnostic_label(
                    stream.color_range.as_deref(),
                    &["unknown", "unspecified", "tv", "pc"],
                ),
                sample_rate: stream
                    .sample_rate
                    .as_deref()
                    .and_then(|value| value.parse().ok()),
                channels: stream.channels,
            })
            .collect();
    }
}

impl MediaEngine {
    /// Begrenzt messen, wirklich benötigte Tracks prüfen, erst danach Ausgaben starten.
    pub async fn prepare_and_start(
        &self,
        spec: DesiredSessionSpec,
        input: mpsc::Receiver<MediaEvent>,
    ) -> Result<RunningMedia> {
        self.prepare_and_start_diagnosed(spec, input, &mut PreparationDiagnostic::default())
            .await
    }

    pub async fn prepare_and_start_diagnosed(
        &self,
        spec: DesiredSessionSpec,
        mut input: mpsc::Receiver<MediaEvent>,
        diagnostic: &mut PreparationDiagnostic,
    ) -> Result<RunningMedia> {
        let probe_dump_requested = diagnostic.probe_dump_requested;
        *diagnostic = PreparationDiagnostic {
            probe_dump_requested,
            ..PreparationDiagnostic::default()
        };
        diagnostic.phase = "configuration";
        diagnostic.requested = spec
            .outputs
            .iter()
            .take(32)
            .map(|output| RequestedDiagnostic {
                platform: diagnostic_label(
                    Some(&output.target.id),
                    &["twitch", "youtube", "kick", "tiktok"],
                )
                .unwrap_or("other"),
                width: output.video.width,
                height: output.video.height,
                fps_numerator: output.video.fps.numerator(),
                fps_denominator: output.video.fps.denominator(),
                bitrate_kbps: output.video.bitrate_kbps,
                codec: match output.video.codec {
                    uplink_core::Codec::H264 => "h264",
                    uplink_core::Codec::Av1 => "av1",
                    uplink_core::Codec::Hevc => "hevc",
                },
                live_audio_track: output.live_audio_track,
                vod_audio_track: output.vod_audio_track,
            })
            .collect();
        if spec.outputs.is_empty() || spec.outputs.len() > self.config.limits.max_outputs {
            return Err(MediaError::InvalidConfiguration);
        }
        let audio = spec
            .outputs
            .iter()
            .flat_map(|output| {
                std::iter::once(output.live_audio_track).chain(output.vod_audio_track)
            })
            .collect();
        let prepared = self
            .prepare_source_diagnosed(spec.first, &mut input, &audio, diagnostic)
            .await?;
        diagnostic.phase = "graph";
        let graph = Graph::observed(&prepared.observation, &spec.outputs)?;
        let routes = spec
            .outputs
            .into_iter()
            .map(|output| TargetRoute {
                output_id: output.target.id.clone(),
                target: output.target,
            })
            .collect();
        diagnostic.phase = "start_graph";
        self.start_graph(
            prepared.identity,
            routes,
            graph,
            input,
            prepared.prefix,
            Some(prepared.observation),
        )
    }

    pub async fn prepare_program_and_start(
        &self,
        spec: ProgramSessionSpec,
        mut input: mpsc::Receiver<MediaEvent>,
    ) -> Result<RunningMedia> {
        if spec.outputs.is_empty() || spec.outputs.len() > self.config.limits.max_outputs {
            return Err(MediaError::InvalidConfiguration);
        }
        let audio = spec
            .outputs
            .iter()
            .flat_map(|output| output.audio.iter().map(|audio| audio.source_wire_track))
            .collect();
        let prepared = self.prepare_source(spec.first, &mut input, &audio).await?;
        let graph = Graph::program(&prepared.observation, &spec.outputs)?;
        let routes = spec
            .outputs
            .into_iter()
            .map(|output| TargetRoute {
                output_id: output.target.id.clone(),
                target: output.target,
            })
            .collect();
        self.start_graph(
            prepared.identity,
            routes,
            graph,
            input,
            prepared.prefix,
            Some(prepared.observation),
        )
    }

    async fn prepare_source(
        &self,
        first: MediaEvent,
        input: &mut mpsc::Receiver<MediaEvent>,
        selected_audio: &HashSet<u8>,
    ) -> Result<PreparedSource> {
        self.prepare_source_diagnosed(
            first,
            input,
            selected_audio,
            &mut PreparationDiagnostic::default(),
        )
        .await
    }

    async fn prepare_source_diagnosed(
        &self,
        first: MediaEvent,
        input: &mut mpsc::Receiver<MediaEvent>,
        selected_audio: &HashSet<u8>,
        diagnostic: &mut PreparationDiagnostic,
    ) -> Result<PreparedSource> {
        diagnostic.phase = "prepare_source";
        let identity = first.identity;
        let mut prefix = VecDeque::new();
        let mut next = Some(first);
        let mut bytes = 0usize;
        let mut events = 0usize;
        let mut first_video_dts = None;
        let mut duration = 0u32;
        let deadline = Instant::now() + self.config.limits.startup_timeout;
        let mut tracks = BTreeMap::new();
        loop {
            let Some(event) = next.take() else {
                break;
            };
            if event.identity.session != identity.session
                || event.identity.generation != identity.generation
            {
                return Err(MediaError::WrongSession);
            }
            if event.wire_body().len() > self.config.limits.max_tag_bytes {
                return Err(MediaError::InvalidMedia);
            }
            bytes = bytes
                .checked_add(event.wire_body().len() + 15)
                .ok_or(MediaError::ResourceLimit)?;
            if bytes > self.config.limits.queue_bytes || events >= self.config.limits.queue_events {
                return Err(MediaError::Backpressure);
            }
            events += 1;
            // Auch verworfene Zusatzspuren verbrauchen das Vorlaufbudget und
            // gehören zur selben Session. Erst danach gilt die Audioauswahl.
            if event.identity.track.kind != MediaKind::Audio
                || selected_audio.contains(&event.identity.track.wire_id)
            {
                if event.configuration_revision > 1
                    || (event.configuration_revision == 0
                        && event.event_kind != EventKind::Metadata)
                {
                    return Err(MediaError::UnsupportedProfile);
                }
                if event.event_kind == EventKind::SequenceHeader
                    && tracks.insert(event.identity.track, event.codec).is_some()
                {
                    return Err(MediaError::UnsupportedProfile);
                }
                if event.identity.track.kind == MediaKind::Video
                    && event.event_kind == EventKind::Frame
                {
                    let start = first_video_dts.get_or_insert(event.dts_ms);
                    duration = event
                        .dts_ms
                        .checked_sub(*start)
                        .ok_or(MediaError::InvalidMedia)?;
                }
                prefix.push_back(event);
            }
            if duration >= 1000 {
                break;
            }
            next = timeout_at(deadline, input.recv())
                .await
                .map_err(|_| MediaError::StartTimeout)?;
        }
        if first_video_dts.is_none() {
            return Err(MediaError::MissingTrack);
        }
        let video: Vec<_> = tracks
            .keys()
            .filter(|track| track.kind == MediaKind::Video)
            .copied()
            .collect();
        if video.len() != 1 {
            return Err(MediaError::UnsupportedProfile);
        }
        let audio: Vec<_> = tracks
            .keys()
            .filter(|track| track.kind == MediaKind::Audio)
            .copied()
            .collect();
        if audio.is_empty() || audio.len() > 16 {
            return Err(MediaError::MissingTrack);
        }
        let mut flv = HEADER.to_vec();
        let mut ordered_headers = Vec::new();
        for track in std::iter::once(&video[0]).chain(audio.iter()) {
            let event = prefix
                .iter()
                .find(|event| {
                    event.identity.track == *track && event.event_kind == EventKind::SequenceHeader
                })
                .ok_or(MediaError::MissingTrack)?;
            ordered_headers.push(event);
        }
        for event in ordered_headers.into_iter().chain(
            prefix
                .iter()
                .filter(|event| event.event_kind != EventKind::SequenceHeader),
        ) {
            let mut tag = FlvTag::from_event(event, self.config.limits.max_tag_bytes)?;
            if event.identity.track.kind == MediaKind::Audio {
                let canonical = audio
                    .iter()
                    .position(|track| *track == event.identity.track)
                    .ok_or(MediaError::MissingTrack)?;
                tag = tag.with_audio_track(
                    canonical as u8,
                    self.config.limits.routing_limits().max_tag_bytes,
                )?;
            }
            tag.write_to(&mut flv).await?;
            if flv.len() > self.config.limits.queue_bytes + HEADER.len() + prefix.len() * 5 {
                return Err(MediaError::Backpressure);
            }
        }
        diagnostic.phase = "probe";
        let measured = probe(&self.config, &flv, diagnostic).await;
        if let Ok(document) = &measured {
            diagnostic.probe(document);
        }
        diagnostic.retain_probe_dump(flv);
        let measured = measured?;
        diagnostic.phase = "observation";
        let observation = observation(measured, &audio, video[0].wire_id, events, bytes, duration)?;
        let identity = TrackIdentity {
            track: video[0],
            ..identity
        };
        Ok(PreparedSource {
            identity,
            prefix,
            observation,
        })
    }
}
struct PreparedSource {
    identity: TrackIdentity,
    prefix: VecDeque<MediaEvent>,
    observation: SourceObservation,
}

#[cfg(test)]
#[path = "prepare/review_tests.rs"]
mod review_tests;

fn sanitized_probe_line(bytes: &[u8]) -> Option<&'static str> {
    let line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .filter(|line| !line.is_empty())?;
    let line = String::from_utf8_lossy(line);
    Some(
        [
            "Invalid data found when processing input",
            "End of file",
            "Could not find codec parameters",
            "Could not find codec parameters for stream",
            "Invalid NAL unit size",
            "Error splitting the input into NAL units",
            "Missing Sequence Header",
            "No sequence header available",
            "Invalid frame dimensions",
            "Packet mismatch",
            "Unknown video codec",
            "Unsupported video codec",
            "Failed to open codec",
            "Cannot allocate memory",
            "Broken pipe",
        ]
        .into_iter()
        .find(|signature| line.contains(signature))
        .unwrap_or("[redigiert]"),
    )
}

fn io_error_label(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::BrokenPipe => "broken_pipe",
        std::io::ErrorKind::UnexpectedEof => "unexpected_eof",
        std::io::ErrorKind::WriteZero => "write_zero",
        std::io::ErrorKind::ConnectionReset => "connection_reset",
        std::io::ErrorKind::Interrupted => "interrupted",
        std::io::ErrorKind::TimedOut => "timed_out",
        _ => "other",
    }
}

async fn drain_probe_stderr(
    stderr: &mut tokio::process::ChildStderr,
    first: &mut Vec<u8>,
) -> Result<()> {
    let mut buffer = [0u8; 1024];
    loop {
        let count = stderr.read(&mut buffer).await.map_err(|_| MediaError::Io)?;
        if count == 0 {
            return Ok(());
        }
        if !first.contains(&b'\n') && first.len() < 1024 {
            let remaining = 1024 - first.len();
            first.extend_from_slice(&buffer[..count.min(remaining)]);
        }
    }
}

async fn probe(
    config: &EngineConfig,
    bytes: &[u8],
    diagnostic: &mut PreparationDiagnostic,
) -> Result<ProbeDocument> {
    let child=Command::new(&config.ffprobe)
        .args(["-v","error","-threads","1","-max_alloc","67108864","-max_pixels","9000000","-probesize"])
        .arg(config.limits.queue_bytes.to_string())
        .args(["-analyzeduration","1000000","-show_streams","-show_entries","stream=codec_type,codec_name,width,height,pix_fmt,r_frame_rate,sample_rate,channels,color_primaries,color_transfer,color_space,color_range","-of","json","-f","flv","pipe:0"])
        .env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true)
        .spawn().map_err(|_|MediaError::ProcessFailed)?;
    let mut guard = ProbeChild {
        child: Some(child),
        deadline: config.limits.shutdown_timeout,
    };
    let child = guard.child.as_mut().ok_or(MediaError::ProcessFailed)?;
    let mut stdin = child.stdin.take().ok_or(MediaError::ProcessFailed)?;
    let stdout = child.stdout.take().ok_or(MediaError::ProcessFailed)?;
    let mut stderr = child.stderr.take().ok_or(MediaError::ProcessFailed)?;
    let mut stderr_first = Vec::new();
    let mut exit_code = None;
    let mut write_error_kind = None;
    let mut read_error_kind = None;
    let operation = async {
        let write = async {
            stdin.write_all(bytes).await.map_err(|error| {
                write_error_kind = Some(io_error_label(error.kind()));
                MediaError::Io
            })?;
            drop(stdin);
            Ok::<_, MediaError>(())
        };
        let read = async {
            let mut output = Vec::new();
            stdout
                .take(65537)
                .read_to_end(&mut output)
                .await
                .map_err(|error| {
                    read_error_kind = Some(io_error_label(error.kind()));
                    MediaError::Io
                })?;
            if output.len() > 65536 {
                return Err(MediaError::ResourceLimit);
            }
            Ok::<_, MediaError>(output)
        };
        let (_, output, _) = tokio::try_join!(
            write,
            read,
            drain_probe_stderr(&mut stderr, &mut stderr_first)
        )?;
        let status = child
            .wait()
            .await
            .map_err(|_| MediaError::ProcessCleanupFailed)?;
        exit_code = status.code();
        if !status.success() {
            return Err(MediaError::ProcessFailed);
        }
        serde_json::from_slice(&output).map_err(|_| MediaError::InvalidMedia)
    };
    let result = match timeout(config.limits.startup_timeout, operation).await {
        Ok(result) => result,
        Err(_) => Err(MediaError::StartTimeout),
    };
    diagnostic.io_error_kind = write_error_kind.or(read_error_kind);
    diagnostic.ffprobe_exit_code = exit_code.or_else(|| {
        guard
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .and_then(|status| status.code())
    });
    diagnostic.ffprobe_stderr_first_line = sanitized_probe_line(&stderr_first);
    match result {
        Ok(document) => {
            guard.child.take();
            Ok(document)
        }
        Err(reason) => {
            if diagnostic.ffprobe_exit_code.is_none()
                && let Some(child) = guard.child.as_mut()
                && let Ok(Ok(status)) = timeout(Duration::from_millis(100), child.wait()).await
            {
                diagnostic.ffprobe_exit_code = status.code();
            }
            guard.terminate().await?;
            let _ = timeout(
                config.limits.shutdown_timeout,
                drain_probe_stderr(&mut stderr, &mut stderr_first),
            )
            .await;
            diagnostic.ffprobe_stderr_first_line = sanitized_probe_line(&stderr_first);
            Err(reason)
        }
    }
}

fn observation(
    document: ProbeDocument,
    audio: &[WireTrack],
    video: u8,
    events: usize,
    bytes: usize,
    duration: u32,
) -> Result<SourceObservation> {
    let mut videos = document.streams.iter().filter(|s| s.codec_type == "video");
    let source = videos.next().ok_or(MediaError::MissingTrack)?;
    if videos.next().is_some() || document.streams.len() != audio.len() + 1 {
        return Err(MediaError::InvalidMedia);
    }
    let fps = source
        .r_frame_rate
        .as_deref()
        .and_then(|v| v.split_once('/'))
        .ok_or(MediaError::InvalidMedia)?;
    let fps = FrameRate::new(
        fps.0.parse().map_err(|_| MediaError::InvalidMedia)?,
        fps.1.parse().map_err(|_| MediaError::InvalidMedia)?,
    )
    .map_err(|_| MediaError::InvalidMedia)?;
    let width = source.width.ok_or(MediaError::InvalidMedia)?;
    let height = source.height.ok_or(MediaError::InvalidMedia)?;
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || u64::from(width) * u64::from(height) > 9_000_000
        || fps > FrameRate::new(120, 1).map_err(|_| MediaError::InvalidConfiguration)?
    {
        return Err(MediaError::ResourceLimit);
    }
    let observed_audio = document
        .streams
        .iter()
        .filter(|s| s.codec_type == "audio")
        .zip(audio)
        .map(|(stream, track)| {
            Ok(AudioObservation {
                wire_track: track.wire_id,
                codec: stream.codec_name.clone(),
                sample_rate: stream
                    .sample_rate
                    .as_deref()
                    .ok_or(MediaError::InvalidMedia)?
                    .parse()
                    .map_err(|_| MediaError::InvalidMedia)?,
                channels: stream.channels.ok_or(MediaError::InvalidMedia)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if observed_audio.len() != audio.len() {
        return Err(MediaError::MissingTrack);
    }
    let known = |value: &Option<String>| {
        value
            .clone()
            .filter(|v| v != "unknown" && v != "unspecified")
    };
    Ok(SourceObservation {
        video_wire_track: video,
        codec: source.codec_name.clone(),
        width,
        height,
        fps_numerator: fps.numerator(),
        fps_denominator: fps.denominator(),
        pixel_format: source.pix_fmt.clone().ok_or(MediaError::InvalidMedia)?,
        color_primaries: known(&source.color_primaries),
        color_transfer: known(&source.color_transfer),
        color_matrix: known(&source.color_space),
        color_range: known(&source.color_range),
        rate_control: None,
        gop_frames: None,
        audio: observed_audio,
        sampled_events: events,
        sampled_bytes: bytes,
        sampled_duration_ms: duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> ProbeDocument {
        // Öffentliche synthetische H.264-Referenz: FFprobe 8, 320x180/25, zwei AAC.
        serde_json::from_str(r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":320,"height":180,"pix_fmt":"yuv420p","r_frame_rate":"25/1"},{"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":1},{"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":1}]}"#).unwrap()
    }
    fn audio() -> [WireTrack; 2] {
        [
            WireTrack {
                kind: MediaKind::Audio,
                wire_id: 7,
            },
            WireTrack {
                kind: MediaKind::Audio,
                wire_id: 12,
            },
        ]
    }
    #[test]
    fn probe_dump_trigger_distinguishes_missing_result_empty_result_and_streams() {
        for (requested, available, streams, expected) in [
            (false, false, 0, false),
            (true, false, 0, true),
            (true, true, 0, true),
            (true, true, 1, false),
        ] {
            let mut diagnostic = PreparationDiagnostic::default();
            if requested {
                diagnostic.request_probe_dump();
            }
            if available {
                diagnostic.probe(&ProbeDocument {
                    streams: if streams == 0 {
                        vec![]
                    } else {
                        document().streams
                    },
                });
            }
            let bytes = b"private-media-payload".to_vec();
            diagnostic.retain_probe_dump(bytes.clone());
            let value = serde_json::to_value(&diagnostic).unwrap();
            assert_eq!(value["probe_result_available"], available);
            assert!(!value.to_string().contains("private-media-payload"));
            assert!(value.get("probe_dump").is_none());
            assert!(value.get("probe_dump_requested").is_none());
            assert_eq!(diagnostic.take_probe_dump(), expected.then_some(bytes));
        }
    }

    #[test]
    fn probe_dump_buffer_never_truncates_or_copies_the_owned_capture() {
        let mut diagnostic = PreparationDiagnostic::default();
        diagnostic.request_probe_dump();
        diagnostic.retain_probe_dump(vec![0; MAX_PROBE_DUMP_BYTES + 1]);
        assert!(diagnostic.take_probe_dump().is_none());
        assert_eq!(diagnostic.probe_dump_status, Some("too_large"));
        let bytes = vec![9; MAX_PROBE_DUMP_BYTES];
        let address = bytes.as_ptr();
        diagnostic.retain_probe_dump(bytes);
        let captured = diagnostic.take_probe_dump().unwrap();
        assert_eq!(captured.as_ptr(), address);
        assert_eq!(captured.len(), MAX_PROBE_DUMP_BYTES);
    }

    #[test]
    fn diagnostic_probe_fields_are_bounded_and_never_copy_untrusted_text() {
        let mut document = document();
        document.streams[0].color_space = Some("gbr".into());
        document.streams[0].pix_fmt =
            Some("rtmps://secret.example/live/private-key\nforged".into());
        document.streams[0].r_frame_rate = Some("0/0".into());
        let mut diagnostic = PreparationDiagnostic::default();
        diagnostic.probe(&document);
        let value = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(value["probe"][0]["pixel_format"], "other");
        assert_eq!(value["probe"][0]["color_matrix"], "gbr");
        assert_eq!(value["probe"][0]["fps"], serde_json::json!([0, 0]));
        assert!(!value.to_string().contains("private-key"));
        assert!(observation(document, &audio(), 0, 1, 1, 1000).is_err());
        assert_eq!(
            sanitized_probe_line(
                b"[flv @ 0xdeadbeef] Invalid data found when processing input: private-key\nforged"
            ),
            Some("Invalid data found when processing input")
        );
        assert_eq!(
            sanitized_probe_line(b"rtmps://secret.example/live/private-key"),
            Some("[redigiert]")
        );
        assert_eq!(sanitized_probe_line(b""), None);
    }

    #[test]
    fn observation_keeps_unknowns_and_explicit_audio_wire_ids() {
        let value = observation(document(), &audio(), 0, 120, 15000, 1000).unwrap();
        assert_eq!(
            (
                value.width,
                value.height,
                value.fps_numerator,
                value.fps_denominator
            ),
            (320, 180, 25, 1)
        );
        assert!(value.rate_control.is_none());
        assert!(value.gop_frames.is_none());
        assert!(value.color_matrix.is_none());
        assert_eq!(
            value.audio.iter().map(|v| v.wire_track).collect::<Vec<_>>(),
            vec![7, 12]
        );
    }
    #[test]
    fn invalid_or_missing_stream_descriptions_never_become_success() {
        let mut missing = document();
        missing.streams.pop();
        assert!(observation(missing, &audio(), 0, 120, 15000, 1000).is_err());
        let mut huge = document();
        huge.streams[0].width = Some(u32::MAX);
        assert!(matches!(
            observation(huge, &audio(), 0, 120, 15000, 1000),
            Err(MediaError::ResourceLimit)
        ));
        let mut fps = document();
        fps.streams[0].r_frame_rate = Some("0/0".into());
        assert!(observation(fps, &audio(), 0, 120, 15000, 1000).is_err());
    }
    #[tokio::test]
    async fn closed_probe_stdin_preserves_natural_exit_status_and_safe_stderr() {
        use std::os::unix::fs::PermissionsExt;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let directory = std::path::PathBuf::from(format!(
            "/tmp/uplink-probe-exit-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("probe");
        std::fs::write(&executable, b"#!/bin/sh\nexec 0<&-\nprintf 'Invalid data found when processing input: synthetic-private-key\\n' >&2\n/usr/bin/sleep 0.03\nexit 42\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = EngineConfig {
            ffmpeg: "/usr/bin/true".into(),
            ffprobe: executable,
            work_directory: directory.clone(),
            limits: MediaLimits::default(),
        };
        let mut diagnostic = PreparationDiagnostic::default();
        let result = probe(&config, &vec![0; 4 * 1024 * 1024], &mut diagnostic).await;
        std::fs::remove_dir_all(directory).unwrap();
        assert!(matches!(result, Err(MediaError::Io)));
        assert_eq!(
            diagnostic.ffprobe_exit_code,
            Some(42),
            "Ein bereits abbrechender Probeprozess muss seinen natürlichen Exitstatus behalten"
        );
        assert_eq!(
            diagnostic.ffprobe_stderr_first_line,
            Some("Invalid data found when processing input")
        );
        assert_eq!(
            serde_json::to_value(diagnostic).unwrap()["io_error_kind"],
            "broken_pipe"
        );
    }

    #[tokio::test]
    async fn cancelled_probe_owner_kills_and_reaps_its_child() {
        let child = Command::new("/usr/bin/sleep")
            .arg("20")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let guard = ProbeChild {
            child: Some(child),
            deadline: Duration::from_secs(1),
        };
        drop(guard);
        timeout(Duration::from_secs(2), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("abgebrochener Probeprozess muss beendet und abgeholt sein");
    }
}
