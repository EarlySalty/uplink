use crate::{
    flv::{FlvTag, HEADER},
    graph::Graph,
    *,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, VecDeque},
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
struct ProbeChild {
    child: Option<Child>,
    deadline: Duration,
}
impl ProbeChild {
    async fn terminate(&mut self) -> Result<()> {
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

impl MediaEngine {
    /// Begrenzt messen, wirklich benötigte Tracks prüfen, erst danach Ausgaben starten.
    pub async fn prepare_and_start(
        &self,
        spec: DesiredSessionSpec,
        mut input: mpsc::Receiver<MediaEvent>,
    ) -> Result<RunningMedia> {
        if spec.outputs.is_empty() || spec.outputs.len() > self.config.limits.max_outputs {
            return Err(MediaError::InvalidConfiguration);
        }
        let identity = spec.first.identity;
        let mut prefix = VecDeque::new();
        let mut next = Some(spec.first);
        let mut bytes = 0usize;
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
            if event.configuration_revision > 1
                || (event.configuration_revision == 0 && event.event_kind != EventKind::Metadata)
            {
                return Err(MediaError::UnsupportedProfile);
            }
            bytes = bytes
                .checked_add(event.wire_body().len() + 15)
                .ok_or(MediaError::ResourceLimit)?;
            if bytes > self.config.limits.queue_bytes
                || prefix.len() >= self.config.limits.queue_events
            {
                return Err(MediaError::Backpressure);
            }
            if event.event_kind == EventKind::SequenceHeader
                && tracks.insert(event.identity.track, event.codec).is_some()
            {
                return Err(MediaError::UnsupportedProfile);
            }
            if event.identity.track.kind == MediaKind::Video && event.event_kind == EventKind::Frame
            {
                let start = first_video_dts.get_or_insert(event.dts_ms);
                duration = event
                    .dts_ms
                    .checked_sub(*start)
                    .ok_or(MediaError::InvalidMedia)?;
            }
            prefix.push_back(event);
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
                tag = tag.with_audio_track(canonical as u8, self.config.limits.max_tag_bytes)?;
            }
            tag.write_to(&mut flv).await?;
            if flv.len() > self.config.limits.queue_bytes + HEADER.len() + prefix.len() * 5 {
                return Err(MediaError::Backpressure);
            }
        }
        let measured = probe(&self.config, flv).await?;
        let observation = observation(
            measured,
            &audio,
            video[0].wire_id,
            prefix.len(),
            bytes,
            duration,
        )?;
        let graph = Graph::observed(&observation, &spec.outputs)?;
        let routes = spec
            .outputs
            .into_iter()
            .map(|output| TargetRoute {
                output_id: output.target.id.clone(),
                target: output.target,
            })
            .collect();
        let identity = TrackIdentity {
            track: video[0],
            ..identity
        };
        self.start_graph(identity, routes, graph, input, prefix, Some(observation))
    }
}

async fn probe(config: &EngineConfig, bytes: Vec<u8>) -> Result<ProbeDocument> {
    let child=Command::new(&config.ffprobe)
        .args(["-v","quiet","-threads","1","-max_alloc","67108864","-max_pixels","9000000","-probesize"])
        .arg(config.limits.queue_bytes.to_string())
        .args(["-analyzeduration","1000000","-show_streams","-show_entries","stream=codec_type,codec_name,width,height,pix_fmt,r_frame_rate,sample_rate,channels,color_primaries,color_transfer,color_space,color_range","-of","json","-f","flv","pipe:0"])
        .env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true)
        .spawn().map_err(|_|MediaError::ProcessFailed)?;
    let mut guard = ProbeChild {
        child: Some(child),
        deadline: config.limits.shutdown_timeout,
    };
    let child = guard.child.as_mut().ok_or(MediaError::ProcessFailed)?;
    let mut stdin = child.stdin.take().ok_or(MediaError::ProcessFailed)?;
    let stdout = child.stdout.take().ok_or(MediaError::ProcessFailed)?;
    let operation = async {
        let write = async {
            stdin.write_all(&bytes).await.map_err(|_| MediaError::Io)?;
            drop(stdin);
            Ok::<_, MediaError>(())
        };
        let read = async {
            let mut output = Vec::new();
            stdout
                .take(65537)
                .read_to_end(&mut output)
                .await
                .map_err(|_| MediaError::Io)?;
            if output.len() > 65536 {
                return Err(MediaError::ResourceLimit);
            }
            Ok::<_, MediaError>(output)
        };
        let (_, output) = tokio::try_join!(write, read)?;
        let status = child
            .wait()
            .await
            .map_err(|_| MediaError::ProcessCleanupFailed)?;
        if !status.success() {
            return Err(MediaError::ProcessFailed);
        }
        serde_json::from_slice(&output).map_err(|_| MediaError::InvalidMedia)
    };
    let result = match timeout(config.limits.startup_timeout, operation).await {
        Ok(result) => result,
        Err(_) => Err(MediaError::StartTimeout),
    };
    match result {
        Ok(document) => {
            guard.child.take();
            Ok(document)
        }
        Err(reason) => {
            guard.terminate().await?;
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
