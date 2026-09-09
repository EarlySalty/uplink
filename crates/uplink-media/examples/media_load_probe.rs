use serde::Serialize;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::{ExitCode, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::JoinSet,
    time::{Instant, timeout},
};
use uplink_ingest::{EndReason, EventKind, MediaEvent, MediaKind};
use uplink_media::{
    EngineConfig, MediaEngine, MediaError, MediaLimits, OutputState, ProgramAudio, ProgramOutput,
    ProgramSessionSpec,
    flv::{FlvTag, HEADER},
    pusher::RunningPusher,
};

#[path = "media_load_probe/config.rs"]
mod config;
#[path = "media_load_probe/local.rs"]
mod local;
#[path = "media_load_probe/metrics.rs"]
mod metrics;
#[path = "media_load_probe/source.rs"]
mod source;

use config::Config;
use metrics::{Counters, Processes, Sample, SessionSnapshot, distribution};
use source::{MAX_TAG, Source};
type ProbeResult<T> = Result<T, &'static str>;
type Shared = Arc<Mutex<SessionSnapshot>>;

fn open_regular(path: impl AsRef<std::path::Path>) -> ProbeResult<tokio::fs::File> {
    const NONBLOCK_NOFOLLOW: i32 = 0x800 | 0x20000;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK_NOFOLLOW)
        .open(path)
        .map_err(|_| "Lokale Messdatei ist nicht regulär lesbar")?;
    if !file
        .metadata()
        .map_err(|_| "Lokale Dateimetadaten fehlen")?
        .is_file()
    {
        return Err("Lokale Messdatei muss eine reguläre Datei sein");
    }
    Ok(tokio::fs::File::from_std(file))
}

fn update(shared: &Shared, change: impl FnOnce(&mut SessionSnapshot)) -> ProbeResult<()> {
    change(
        &mut *shared
            .lock()
            .map_err(|_| "Lastmetriken wurden unterbrochen")?,
    );
    Ok(())
}

fn count(counter: &mut Counters, event: &MediaEvent) {
    counter.bytes += event.wire_body().len() as u64;
    if event.event_kind == EventKind::Frame {
        match event.identity.track.kind {
            MediaKind::Video => {
                counter.video_frames += 1;
                counter.last_video_dts_ms = event.dts_ms;
            }
            MediaKind::Audio => counter.audio_frames += 1,
        }
    }
}

fn lag(snapshot: &mut SessionSnapshot, expected_tracks: usize) {
    snapshot.missing_video_tracks =
        expected_tracks.saturating_sub(snapshot.output_track_dts_ms.len());
    let progress = if snapshot.missing_video_tracks == 0 {
        snapshot
            .output_track_dts_ms
            .values()
            .copied()
            .min()
            .unwrap_or(0)
    } else {
        0
    };
    snapshot.timestamp_lag_ms = snapshot.input.last_video_dts_ms.saturating_sub(progress);
}

fn forwarding_capacity(config: &Config) -> usize {
    config.queue_events.min(config.queue_bytes / MAX_TAG)
}

fn limits(config: &Config) -> MediaLimits {
    MediaLimits {
        queue_events: config.queue_events,
        queue_bytes: config.queue_bytes,
        worker_threads: config.worker_threads,
        ..MediaLimits::default()
    }
}

async fn feed(
    config: Arc<Config>,
    source: Arc<Source>,
    target: uplink_media::PublishTarget,
) -> ProbeResult<()> {
    let pusher = RunningPusher::start(target, limits(&config))
        .await
        .map_err(|_| "Synthetischer Quell-Publisher konnte nicht starten")?;
    let start = Instant::now();
    let duration = config.media_seconds * 1000;
    for cycle in 0..duration.div_ceil(config.repeat_ms) {
        for original in &source.tags {
            let Some(tag) = source::repeated(original, cycle, config.repeat_ms)? else {
                continue;
            };
            if tag.timestamp_ms() >= duration {
                continue;
            }
            if config.paced {
                tokio::time::sleep_until(
                    start + Duration::from_millis(u64::from(tag.timestamp_ms())),
                )
                .await;
            }
            let tag = Arc::new(tag);
            loop {
                match pusher.try_send(tag.clone()) {
                    Ok(()) => break,
                    Err(MediaError::Backpressure) => {
                        tokio::time::sleep(Duration::from_millis(1)).await
                    }
                    Err(_) => {
                        return Err("Synthetischer Quell-Publisher wurde abgewiesen oder getrennt");
                    }
                }
            }
        }
    }
    pusher
        .finish()
        .await
        .map_err(|_| "Synthetischer Quell-Publisher endete nicht sauber")?;
    Ok(())
}

async fn capture(
    config: Arc<Config>,
    server: Arc<uplink_ingest::IngestServer<local::LocalAuth>>,
    shared: Shared,
    sample_path: PathBuf,
) -> ProbeResult<()> {
    let mut connection = server
        .accept()
        .await
        .map_err(|_| "Lokaler Ausgabeempfänger fehlt")?;
    let mut file = if config.sample_seconds > 0 {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&sample_path)
            .await
            .map_err(|_| "Synthetische Ausgabeprobe konnte nicht angelegt werden")?;
        file.write_all(HEADER)
            .await
            .map_err(|_| "Synthetische Ausgabeprobe konnte nicht geschrieben werden")?;
        Some(file)
    } else {
        None
    };
    let mut sample_bytes = 0;
    let mut last = std::collections::BTreeMap::new();
    while let Some(event) = connection.next().await {
        let kind = if event.identity.track.kind == MediaKind::Video {
            9
        } else {
            8
        };
        if event.event_kind == EventKind::Frame
            && last
                .insert((kind, event.identity.track.wire_id), event.dts_ms)
                .is_some_and(|dts| dts >= event.dts_ms)
        {
            return Err("Ausgabezeitstempel steigen nicht streng je Spur");
        }
        update(&shared, |value| {
            count(&mut value.output, &event);
            if event.event_kind == EventKind::Frame && event.identity.track.kind == MediaKind::Video
            {
                value
                    .output_track_dts_ms
                    .insert(event.identity.track.wire_id, event.dts_ms);
            }
            lag(value, config.profiles.len());
        })?;
        if let Some(file) = &mut file
            && event.dts_ms < config.sample_seconds * 1_000
        {
            let tag = FlvTag::from_event(&event, MAX_TAG)
                .map_err(|_| "Ausgabeprobe enthält einen ungültigen Medientag")?;
            sample_bytes += tag.wire_len();
            if sample_bytes > 32 * 1024 * 1024 {
                return Err("Ausgabeprobe überschreitet 32 MiB");
            }
            tag.write_to(file)
                .await
                .map_err(|_| "Synthetische Ausgabeprobe konnte nicht geschrieben werden")?;
        }
        if config.receiver_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(config.receiver_delay_ms)).await;
        }
    }
    if let Some(mut file) = file {
        file.flush()
            .await
            .map_err(|_| "Ausgabeprobe konnte nicht abgeschlossen werden")?;
    }
    let reason = connection.finish().await.reason;
    let observed_tracks: std::collections::BTreeSet<_> = last.keys().copied().collect();
    let expected_tracks: std::collections::BTreeSet<_> = config
        .profiles
        .iter()
        .map(|p| (9, p.wire_track))
        .chain(std::iter::once((8, 0)))
        .collect();
    if observed_tracks != expected_tracks {
        return Err("Ausgabeprobe enthält fehlende oder zusätzliche Drahtspuren");
    }
    println!(
        "{}",
        serde_json::json!({"event":"output_end", "reason": format!("{reason:?}")})
    );
    if reason != EndReason::ExplicitStop {
        return Err("Lokaler Ausgabeempfänger endete nicht mit ExplicitStop");
    }
    Ok(())
}

async fn session(
    config: Arc<Config>,
    source: Arc<Source>,
    shared: Shared,
    index: usize,
) -> ProbeResult<()> {
    let work = config.output_directory.join(format!("worker-{index}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&work)
        .map_err(|_| "Privater Workerordner konnte nicht angelegt werden")?;
    let sample_path = config.output_directory.join(format!("sample-{index}.flv"));
    let (source_server, source_target) = local::endpoint(&config, "synthetic-source").await?;
    let (output_server, output_target) = local::endpoint(&config, "synthetic-output").await?;
    let mut tasks = JoinSet::new();
    tasks.spawn(feed(config.clone(), source, source_target));
    tasks.spawn(capture(
        config.clone(),
        output_server,
        shared.clone(),
        sample_path,
    ));
    let mut connection = source_server
        .accept()
        .await
        .map_err(|_| "Synthetischer Quell-Eingang fehlt")?;
    let first = connection
        .next()
        .await
        .ok_or("Synthetische Quelle lieferte keinen Header")?;
    update(&shared, |value| count(&mut value.input, &first))?;
    let capacity = forwarding_capacity(&config);
    let (send, receive) = mpsc::channel(capacity);
    let forward_state = shared.clone();
    let expected_tracks = config.profiles.len();
    tasks.spawn(async move {
        while let Some(event) = connection.next().await {
            update(&forward_state, |value| {
                count(&mut value.input, &event);
                value.input_queue_events = send.max_capacity() - send.capacity();
                value.input_queue_capacity = send.max_capacity();
                lag(value, expected_tracks);
            })?;
            send.send(event)
                .await
                .map_err(|_| "Medienworker schloss die Eingangsqueue")?;
        }
        drop(send);
        let reason = connection.finish().await.reason;
        println!(
            "{}",
            serde_json::json!({"event":"input_end", "reason": format!("{reason:?}")})
        );
        if reason != EndReason::ExplicitStop {
            return Err("Synthetischer Eingang endete nicht mit ExplicitStop");
        }
        Ok(())
    });
    let engine = MediaEngine::new(EngineConfig {
        ffmpeg: config.ffmpeg.clone(),
        ffprobe: config.ffprobe.clone(),
        work_directory: work.clone(),
        limits: limits(&config),
    })
    .map_err(|_| "Last-Medienengine konnte nicht angelegt werden")?;
    let video = config
        .profiles
        .iter()
        .map(|profile| profile.video(&config))
        .collect::<ProbeResult<_>>()?;
    let running = engine
        .prepare_program_and_start(
            ProgramSessionSpec {
                first,
                outputs: vec![ProgramOutput {
                    target: output_target,
                    video,
                    audio: vec![ProgramAudio {
                        encoding: config
                            .audio_encoding
                            .map(|audio| uplink_media::AudioEncoding {
                                channels: audio.channels,
                                bitrate_kbps: audio.bitrate_kbps,
                            }),
                        source_wire_track: 0,
                        destination_wire_track: 0,
                    }],
                }],
            },
            receive,
        )
        .await
        .map_err(|_| "Integrierter Programmpfad konnte die Quelle oder Profile nicht starten")?;
    let observation = running
        .source_observation()
        .ok_or("Gemessene Quellbeobachtung fehlt")?;
    if observation.codec != "av1"
        || observation.width != config.source_width
        || observation.height != config.source_height
        || u64::from(observation.fps_numerator)
            != u64::from(config.source_fps) * u64::from(observation.fps_denominator)
        || observation.audio.len() != 1
        || observation.audio[0].codec != "aac"
    {
        return Err("Gemessene AV1/AAC-Quelle entspricht nicht der Konfiguration");
    }
    println!(
        "{}",
        serde_json::json!({"event":"session_start", "session":index, "source":observation, "graph":running.status().graph})
    );
    let observer = running.observer();
    let finish = running.finish();
    tokio::pin!(finish);
    let report = loop {
        let status = observer.snapshot();
        update(&shared, |value| {
            value.started = true;
            value.encode_groups = status.encode_groups;
            value.video_decoders = status.video_decoders;
            value.publishing_outputs = status
                .outputs
                .iter()
                .filter(|o| o.state == OutputState::Publishing)
                .count();
            value.failed_outputs = status
                .outputs
                .iter()
                .filter(|o| matches!(o.state, OutputState::Failed(_)))
                .count();
        })?;
        tokio::select! {
            report = &mut finish => break report,
            result = tasks.join_next(), if !tasks.is_empty() => {
                result.ok_or("Lasttask fehlt")?.map_err(|_| "Lasttask wurde unterbrochen")??;
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    };
    println!(
        "{}",
        serde_json::json!({"event":"session_end", "session":index, "report":report})
    );
    if report.error.is_some()
        || report
            .status
            .outputs
            .iter()
            .any(|o| o.state != OutputState::LocalEndUnconfirmed)
    {
        return Err("Integrierter Programmpfad endete mit einem Medienfehler");
    }
    while let Some(result) = tasks.join_next().await {
        result.map_err(|_| "Lasttask wurde unterbrochen")??;
    }
    update(&shared, |value| {
        value.completed = true;
        value.publishing_outputs = 0;
        value.input_queue_events = 0;
    })?;
    fs::remove_dir(work).map_err(|_| "Medienworker hinterließ Arbeitsreste")?;
    Ok(())
}

async fn verify_sample(config: &Config, index: usize) -> ProbeResult<()> {
    let path = config.output_directory.join(format!("sample-{index}.flv"));
    let mut child = tokio::process::Command::new(&config.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=index,codec_name,codec_type,width,height,r_frame_rate",
            "-of",
            "json",
        ])
        .arg(&path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Ausgabeprüfung konnte nicht starten")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Ausgabeprüfung lieferte keine Metadaten")?;
    let mut data = Vec::new();
    stdout
        .take(65_537)
        .read_to_end(&mut data)
        .await
        .map_err(|_| "Ausgabemetadaten sind nicht lesbar")?;
    if data.len() > 65_536
        || !child
            .wait()
            .await
            .map_err(|_| "Ausgabeprüfung wurde unterbrochen")?
            .success()
    {
        return Err("Ausgabeprüfung meldete ungültige Medien");
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&data).map_err(|_| "Ausgabemetadaten sind ungültig")?;
    let streams = metadata
        .get("streams")
        .and_then(|v| v.as_array())
        .ok_or("Ausgabespuren fehlen")?;
    let mut video: Vec<_> = streams
        .iter()
        .filter(|v| v["codec_type"] == "video")
        .collect();
    if video.len() != config.profiles.len()
        || streams.iter().filter(|v| v["codec_name"] == "aac").count() != 1
    {
        return Err("Ausgabeprobe hat unerwartete Spuranzahl");
    }
    for profile in &config.profiles {
        let matching = video.iter().position(|stream| {
            stream["codec_name"] == "h264"
                && stream["width"] == profile.width
                && stream["height"] == profile.height
                && stream["r_frame_rate"] == format!("{}/1", profile.fps)
        });
        if let Some(index) = matching {
            video.remove(index);
        } else {
            return Err("Ausgabeprobe hat unerwartete Codec-, Flächen- oder Bildratenzuordnung");
        }
    }
    let status = tokio::process::Command::new(&config.ffmpeg)
        .args([
            "-nostdin",
            "-hide_banner",
            "-v",
            "error",
            "-xerror",
            "-threads",
            "2",
            "-i",
        ])
        .arg(path)
        .args(["-map", "0", "-f", "null", "-"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|_| "Vollständige Dekodierprüfung konnte nicht starten")?;
    if !status.success() {
        return Err("Vollständige Dekodierprüfung der Ausgabeprobe scheiterte");
    }
    println!(
        "{}",
        serde_json::json!({"event":"sample_verified", "session":index, "streams":streams, "fully_decoded":true})
    );
    Ok(())
}

#[derive(Serialize)]
struct Summary {
    event: &'static str,
    synthetic_only: bool,
    capacity_proven: bool,
    elapsed_seconds: f64,
    source_file_bytes: usize,
    source_video_frames: u64,
    source_audio_frames: u64,
    audio_encoding: Option<config::Audio>,
    own_cpu_percent: Option<metrics::Distribution>,
    own_rss_kib: Option<metrics::Distribution>,
    timestamp_lag_ms: Option<metrics::Distribution>,
    input_queue_events: Option<metrics::Distribution>,
    input_bytes_per_second: Option<metrics::Distribution>,
    output_bytes_per_second: Option<metrics::Distribution>,
    output_video_frames_per_second: Option<metrics::Distribution>,
    host_busy_percent: Option<metrics::Distribution>,
    final_sessions: Vec<SessionSnapshot>,
    gaps: &'static [&'static str],
}

async fn run(config: Arc<Config>) -> ProbeResult<()> {
    let source = Arc::new(Source::read(&config).await?);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&config.output_directory)
        .map_err(|_| "Ausgabeordner muss neu und privat sein")?;
    let mut processes = Processes::new().await?;
    processes.sample(1.0)?;
    let mut host = metrics::Host::default();
    host.sample()?;
    let start = Instant::now();
    let mut previous = start;
    let mut sessions = JoinSet::new();
    let mut state = Vec::new();
    for index in 0..config.sessions {
        let shared = Arc::new(Mutex::new(SessionSnapshot::default()));
        state.push(shared.clone());
        sessions.spawn(session(config.clone(), source.clone(), shared, index));
    }
    let mut samples = Vec::new();
    let mut previous_totals = (0_u64, 0_u64, 0_u64);
    let mut next_sample = start + Duration::from_secs(1);
    while !sessions.is_empty() {
        tokio::select! {
            result = sessions.join_next() => {
                result.ok_or("Sessiontask fehlt")?.map_err(|_| "Sessiontask wurde unterbrochen")??;
            }
            _ = tokio::time::sleep_until(next_sample) => {
                let now = Instant::now();
                let (own_pids, own_cpu_percent, own_rss_kib) = processes.sample(now.duration_since(previous).as_secs_f64())?;
                let sessions = state.iter().map(|value| value.lock().map(|v| v.clone()).map_err(|_| "Lastmetrik wurde unterbrochen")).collect::<ProbeResult<Vec<_>>>()?;
                let totals = sessions.iter().fold((0, 0, 0), |acc, value| (acc.0 + value.input.bytes, acc.1 + value.output.bytes, acc.2 + value.output.video_frames));
                let seconds = now.duration_since(previous).as_secs_f64();
                let host_sample = host.sample()?.ok_or("Host-CPU-Vergleichsmessung fehlt")?;
                let must_stop = host.must_stop(host_sample.busy_percent);
                let sample = Sample { elapsed_ms: start.elapsed().as_millis() as u64, measurement_phase: start.elapsed().as_secs() >= u64::from(config.warmup_seconds), own_pids, own_cpu_percent, own_rss_kib,
                    input_bytes_per_second: (totals.0 - previous_totals.0) as f64 / seconds,
                    output_bytes_per_second: (totals.1 - previous_totals.1) as f64 / seconds,
                    output_video_frames_per_second: (totals.2 - previous_totals.2) as f64 / seconds, sessions, host: host_sample };
                previous_totals = totals;
                println!("{}", serde_json::to_string(&sample).map_err(|_| "Lastmetrik kann nicht serialisiert werden")?);
                samples.push(sample);
                if must_stop { return Err("Host-CPU liegt drei Messsekunden über 85 Prozent; nur eigener Lastlauf wurde abgebrochen"); }
                if samples.len() > 1_920 { return Err("Metriksammlung überschreitet ihre feste Grenze"); }
                previous = now;
                next_sample = now + Duration::from_secs(1);
            }
        }
    }
    let elapsed_seconds = start.elapsed().as_secs_f64();
    if config.sample_seconds > 0 {
        for index in 0..config.sessions {
            verify_sample(&config, index).await?;
        }
    }
    let measured = || samples.iter().filter(|sample| sample.measurement_phase);
    let summary = Summary {
        event: "summary",
        synthetic_only: true,
        capacity_proven: false,
        elapsed_seconds,
        source_file_bytes: source.bytes,
        source_video_frames: source.video_frames,
        source_audio_frames: source.audio_frames,
        audio_encoding: config.audio_encoding,
        own_cpu_percent: distribution(measured().map(|s| s.own_cpu_percent)),
        own_rss_kib: distribution(measured().map(|s| s.own_rss_kib as f64)),
        timestamp_lag_ms: distribution(
            measured().flat_map(|s| s.sessions.iter().map(|v| f64::from(v.timestamp_lag_ms))),
        ),
        input_queue_events: distribution(
            measured().flat_map(|s| s.sessions.iter().map(|v| v.input_queue_events as f64)),
        ),
        input_bytes_per_second: distribution(measured().map(|s| s.input_bytes_per_second)),
        output_bytes_per_second: distribution(measured().map(|s| s.output_bytes_per_second)),
        output_video_frames_per_second: distribution(
            measured().map(|s| s.output_video_frames_per_second),
        ),
        host_busy_percent: distribution(measured().map(|s| s.host.busy_percent)),
        final_sessions: state
            .iter()
            .map(|value| {
                value
                    .lock()
                    .map(|v| v.clone())
                    .map_err(|_| "Finale Lastmetrik wurde unterbrochen")
            })
            .collect::<ProbeResult<_>>()?,
        gaps: &[
            "Keine fünfminütige Host-Baseline oder cgroup-Quotamessung",
            "Keine kontogenau angebotenen Profile; ausschließlich synthetische Wünsche",
            "CPU kurzlebiger Prozesse zwischen Messpunkten fehlt; RSS nicht zwischen Prozessen dedupliziert",
            "Interne Worker-/Pusher-Queuebytes und verworfene Frames sind nicht öffentlich messbar",
            "Rückstand ist die größte beobachtete Video-DTS-Differenz; keine Ende-zu-Ende-Latenz",
            "AAC-Bytes werden gezählt; Bitidentität wird durch diesen Lasttreiber nicht geprüft",
        ],
    };
    println!(
        "{}",
        serde_json::to_string(&summary)
            .map_err(|_| "Lastbericht kann nicht serialisiert werden")?
    );
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> ExitCode {
    let result = async {
        let mut args = std::env::args_os().skip(1);
        let path = args
            .next()
            .ok_or("Aufruf benötigt genau eine TOML-Konfigurationsdatei")?;
        if args.next().is_some() {
            return Err("Aufruf benötigt genau eine TOML-Konfigurationsdatei");
        }
        let file = open_regular(path)?;
        let mut data = String::new();
        file.take(65_537)
            .read_to_string(&mut data)
            .await
            .map_err(|_| "Lastkonfiguration ist nicht lesbar")?;
        if data.len() > 65_536 {
            return Err("Lastkonfiguration überschreitet 64 KiB");
        }
        let config = Arc::new(Config::parse(&data)?);
        timeout(
            Duration::from_secs(u64::from(config.hard_timeout_seconds)),
            run(config),
        )
        .await
        .map_err(|_| "Harte Laufzeitgrenze erreicht; eigene Lasttasks wurden abgebrochen")?
    }
    .await;
    let cleanup = timeout(Duration::from_secs(10), async {
        loop {
            if metrics::descendants()?.len() == 1 {
                return Ok::<_, &'static str>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if !matches!(cleanup, Ok(Ok(()))) {
        eprintln!("Eigene Kindprozesse sind nach zehn Sekunden noch nicht vollständig beendet");
        return ExitCode::FAILURE;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarding_queue_respects_both_byte_and_event_budget() {
        let mut config = config::tests::valid();
        assert_eq!(forwarding_capacity(&config), 4);
        config.queue_events = 2;
        assert_eq!(forwarding_capacity(&config), 2);
    }

    #[test]
    fn missing_or_slow_video_track_cannot_be_hidden_by_fast_track() {
        let mut value = SessionSnapshot::default();
        value.input.last_video_dts_ms = 1000;
        value.output_track_dts_ms.insert(0, 1000);
        lag(&mut value, 2);
        assert_eq!(
            (value.missing_video_tracks, value.timestamp_lag_ms),
            (1, 1000)
        );
        value.output_track_dts_ms.insert(1, 200);
        lag(&mut value, 2);
        assert_eq!(
            (value.missing_video_tracks, value.timestamp_lag_ms),
            (0, 800)
        );
    }

    #[tokio::test]
    async fn regular_file_gate_rejects_fifo_symlink_and_directory_without_waiting() {
        let mut suffix = [0_u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "load-file-test-{:016x}",
            u64::from_ne_bytes(suffix)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let regular = directory.join("regular");
        fs::write(&regular, b"synthetic").unwrap();
        let link = directory.join("link");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        let fifo = directory.join("fifo");
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .env_clear()
                .status()
                .unwrap()
                .success()
        );
        assert!(open_regular(&regular).is_ok());
        assert!(open_regular(&link).is_err());
        assert!(open_regular(&fifo).is_err());
        assert!(open_regular(&directory).is_err());
        for path in [regular, link, fifo] {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }
}
