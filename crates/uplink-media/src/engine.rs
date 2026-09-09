use crate::{
    flv::{FlvReader, FlvTag, HEADER},
    graph::{Graph, Routing},
    pusher::RunningPusher,
    sockets::{SocketDirectory, accept_worker},
    *,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, ChildStdin, Command},
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout, timeout_at},
};
use uplink_ingest::{EventKind, MediaEvent, MediaKind};

pub struct MediaEngine {
    pub(crate) config: EngineConfig,
}
pub struct RunningMedia {
    status: watch::Receiver<MediaStatus>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<MediaReport>>,
    observation: Option<SourceObservation>,
}
#[derive(Clone)]
pub struct MediaObserver(watch::Receiver<MediaStatus>);
impl MediaObserver {
    pub fn snapshot(&self) -> MediaStatus {
        self.0.borrow().clone()
    }
}
impl RunningMedia {
    pub fn status(&self) -> MediaStatus {
        self.status.borrow().clone()
    }
    pub fn observer(&self) -> MediaObserver {
        MediaObserver(self.status.clone())
    }
    pub async fn finish(mut self) -> MediaReport {
        let result = self.task.as_mut().expect("owned media task").await;
        self.task.take();
        match result {
            Ok(report) => report,
            Err(_) => MediaReport {
                status: self.status(),
                error: Some(MediaError::ProcessFailed),
            },
        }
    }
    pub fn source_observation(&self) -> Option<&SourceObservation> {
        self.observation.as_ref()
    }
    pub async fn stop(self) -> MediaReport {
        let _ = self.stop.send(true);
        self.finish().await
    }
}
impl Drop for RunningMedia {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

struct Sink {
    pusher: Option<RunningPusher>,
    routing: Routing,
    failure: Option<MediaError>,
}
type Sinks = Arc<Mutex<Vec<Sink>>>;

impl MediaEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let limits = &config.limits;
        limits.validate_packet_limits()?;
        if !config.ffmpeg.is_absolute()
            || !config.ffmpeg.is_file()
            || !config.ffprobe.is_absolute()
            || !config.ffprobe.is_file()
            || limits.worker_threads == 0
            || limits.worker_threads > 64
            || limits.max_outputs == 0
            || limits.max_outputs > 32
            || limits.max_encode_groups > 16
            || limits.startup_timeout.is_zero()
            || limits.write_timeout.is_zero()
            || limits.shutdown_timeout.is_zero()
        {
            return Err(MediaError::InvalidConfiguration);
        }
        let _ = queue::bounded(limits)?;
        Ok(Self { config })
    }
    pub async fn start(
        &self,
        spec: SessionSpec,
        input: mpsc::Receiver<MediaEvent>,
    ) -> Result<RunningMedia> {
        let graph = Graph::build(&spec)?;
        self.start_graph(
            spec.identity,
            spec.routes,
            graph,
            input,
            VecDeque::new(),
            None,
        )
    }

    pub(crate) fn start_graph(
        &self,
        identity: TrackIdentity,
        routes: Vec<TargetRoute>,
        graph: Graph,
        input: mpsc::Receiver<MediaEvent>,
        prefix: VecDeque<MediaEvent>,
        observation: Option<SourceObservation>,
    ) -> Result<RunningMedia> {
        if graph.profiles.len() > self.config.limits.max_encode_groups
            || routes.len() > self.config.limits.max_outputs
        {
            return Err(MediaError::ResourceLimit);
        }
        let initial = MediaStatus {
            encode_groups: graph.profiles.len(),
            video_decoders: usize::from(!graph.profiles.is_empty()),
            graph: routes
                .iter()
                .zip(&graph.routes)
                .map(|(route, routing)| graph.describe_route(routing, &route.target.id))
                .collect(),
            outputs: routes
                .iter()
                .zip(&graph.routes)
                .map(|(route, routing)| OutputStatus {
                    id: route.target.id.clone(),
                    state: routing
                        .failure
                        .map_or(OutputState::Starting, OutputState::Failed),
                    received_bytes: 0,
                    received_events: 0,
                })
                .collect(),
        };
        let (status_tx, status) = watch::channel(initial);
        let (stop, stopped) = watch::channel(false);
        let config = self.config.clone();
        let task = tokio::spawn(async move {
            run(
                config, identity, routes, graph, input, prefix, status_tx, stopped,
            )
            .await
        });
        Ok(RunningMedia {
            status,
            stop,
            task: Some(task),
            observation,
        })
    }
}

fn update_status(sinks: &Sinks, status: &watch::Sender<MediaStatus>) {
    let sinks = sinks.lock().unwrap_or_else(|error| error.into_inner());
    status.send_modify(|current| {
        for (item, sink) in current.outputs.iter_mut().zip(sinks.iter()) {
            if let Some(pusher) = &sink.pusher {
                *item = pusher.status();
            }
            if let Some(error) = sink.failure {
                item.state = OutputState::Failed(error);
            }
        }
    });
}

fn distribute(
    tag: Arc<FlvTag>,
    group: Option<usize>,
    mux_offset_ms: u32,
    sinks: &Sinks,
    limits: &MediaLimits,
) {
    let routed = limits.routing_limits();
    let mut sinks = sinks.lock().unwrap_or_else(|error| error.into_inner());
    for sink in sinks.iter_mut().filter(|sink| sink.failure.is_none()) {
        let Some(pusher) = sink.pusher.as_ref() else {
            continue;
        };
        let result = if tag.kind() == 9 {
            let mut result = Ok(());
            for (_, destination) in sink
                .routing
                .video
                .iter()
                .filter(|(source, _)| *source == group)
            {
                result = tag
                    .with_video_track(*destination, routed.max_tag_bytes)
                    .and_then(|tag| {
                        let offset = mux_offset_ms
                            .checked_sub(sink.routing.timestamp_offset_ms)
                            .ok_or(MediaError::InvalidConfiguration)?;
                        tag.restore_mux_timestamp(offset)
                    })
                    .and_then(|tag| pusher.try_send(Arc::new(tag)));
                if result.is_err() {
                    break;
                }
            }
            result
        } else if sink.routing.group != group {
            continue;
        } else if tag.kind() == 8 {
            tag.audio_track().and_then(|track| {
                for (source, destination) in &sink.routing.audio {
                    if Some(*source) == track {
                        pusher.try_send(Arc::new(
                            tag.with_audio_track(*destination, routed.max_tag_bytes)?,
                        ))?;
                    }
                }
                Ok(())
            })
        } else {
            pusher.try_send(tag.clone())
        };
        if let Err(error) = result {
            pusher.cancel();
            sink.failure = Some(error);
        }
    }
}

async fn cleanup(child: &mut Child, deadline: std::time::Duration, kill: bool) -> Result<()> {
    if kill && !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.start_kill();
    }
    match timeout(deadline, child.wait()).await {
        Ok(Ok(status)) if kill || status.success() => Ok(()),
        Ok(Ok(_)) => Err(MediaError::ProcessFailed),
        Ok(Err(_)) => Err(MediaError::ProcessCleanupFailed),
        Err(_) => {
            let _ = child.start_kill();
            timeout(deadline, child.wait())
                .await
                .map_err(|_| MediaError::ProcessCleanupFailed)?
                .map_err(|_| MediaError::ProcessCleanupFailed)?;
            Err(MediaError::ProcessFailed)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    config: EngineConfig,
    identity: TrackIdentity,
    routes: Vec<TargetRoute>,
    graph: Graph,
    mut input: mpsc::Receiver<MediaEvent>,
    mut prefix: VecDeque<MediaEvent>,
    status: watch::Sender<MediaStatus>,
    mut stopped: watch::Receiver<bool>,
) -> MediaReport {
    let mut error = None;
    let origin = if graph.profiles.is_empty() {
        Some(0)
    } else {
        let measured = tokio::select! {
            result=video_origin(identity,&graph,&mut input,&mut prefix,&config.limits)=>result,
            _=stopped.changed()=>Err(MediaError::Cancelled),
        };
        match measured {
            Ok(origin) => Some(origin),
            Err(reason) => {
                error = Some(reason);
                None
            }
        }
    };
    let sinks: Sinks = Arc::new(Mutex::new(Vec::new()));
    for (route, routing) in routes.into_iter().zip(graph.routes.iter()) {
        if error.is_some() {
            break;
        }
        let started = match routing.failure {
            Some(reason) => Err(reason),
            None => RunningPusher::spawn(route.target, config.limits.routing_limits()),
        };
        match started {
            Ok(pusher) => sinks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(Sink {
                    pusher: Some(pusher),
                    routing: routing.clone(),
                    failure: None,
                }),
            Err(reason) => {
                sinks
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(Sink {
                        pusher: None,
                        routing: routing.clone(),
                        failure: Some(reason),
                    });
                if reason == MediaError::Cancelled {
                    error = Some(reason);
                    break;
                }
            }
        }
    }
    if sinks
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .all(|sink| sink.pusher.is_none())
    {
        error.get_or_insert(MediaError::PublishRejected);
    }
    let mut directory = None;
    let mut worker: Option<Child> = None;
    let mut worker_input: Option<ChildStdin> = None;
    let mut readers: Vec<JoinHandle<Result<()>>> = Vec::new();
    if error.is_none() && !graph.profiles.is_empty() {
        let setup = (|| {
            let mut private = SocketDirectory::create(&config.work_directory)?;
            let mut listeners = Vec::new();
            let mut paths = Vec::new();
            for index in 0..graph.profiles.len() {
                let (listener, path) = private.listener(index)?;
                listeners.push(listener);
                paths.push(path);
            }
            let arguments = graph.arguments(
                &paths,
                config.limits.worker_threads,
                origin.ok_or(MediaError::MissingTrack)?,
            )?;
            let mut child = Command::new(&config.ffmpeg)
                .args(arguments)
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| MediaError::ProcessFailed)?;
            let pid = child.id();
            worker_input = child.stdin.take();
            worker = Some(child);
            let pid = pid.ok_or(MediaError::ProcessFailed)?;
            directory = Some(private);
            for (index, listener) in listeners.into_iter().enumerate() {
                let mux_offset_ms = graph.profiles[index].mux_timestamp_offset_ms();
                let limits = config.limits.clone();
                let output_sinks = sinks.clone();
                let output_status = status.clone();
                readers.push(tokio::spawn(async move {
                    let stream = accept_worker(&listener, pid, limits.startup_timeout).await?;
                    let mut reader = FlvReader::worker_output(stream, &limits);
                    while let Some(tag) = reader.next().await? {
                        distribute(
                            Arc::new(tag),
                            Some(index),
                            mux_offset_ms,
                            &output_sinks,
                            &limits,
                        );
                        update_status(&output_sinks, &output_status);
                    }
                    Ok(())
                }));
            }
            Ok::<_, MediaError>(())
        })();
        if let Err(reason) = setup {
            error = Some(reason);
        }
    }
    update_status(&sinks, &status);
    if error.is_none() {
        let consumed = tokio::select! {
            result = consume(identity,&graph,&mut input,&mut prefix,&mut worker_input,&sinks,&status,&config.limits) => result,
            _ = stopped.changed() => Err(MediaError::Cancelled),
        };
        if let Err(reason) = consumed {
            error = Some(reason);
        }
    }
    drop(worker_input.take());
    if let Some(mut child) = worker
        && let Err(reason) =
            cleanup(&mut child, config.limits.shutdown_timeout, error.is_some()).await
    {
        error.get_or_insert(reason);
    }
    for mut reader in readers {
        match timeout(config.limits.shutdown_timeout, &mut reader).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(reason))) => {
                error.get_or_insert(reason);
            }
            Ok(Err(_)) => {
                error.get_or_insert(MediaError::ProcessFailed);
            }
            Err(_) => {
                reader.abort();
                let _ = reader.await;
                error.get_or_insert(MediaError::ProcessCleanupFailed);
            }
        }
    }
    if let Some(directory) = directory
        && let Err(reason) = directory.close()
    {
        error.get_or_insert(reason);
    }
    let owned_sinks = {
        let mut locked = sinks.lock().unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *locked)
    };
    let mut finishing = JoinSet::new();
    let mut cancellations = Vec::new();
    for (index, mut sink) in owned_sinks.into_iter().enumerate() {
        let Some(pusher) = sink.pusher.take() else {
            continue;
        };
        cancellations.push(pusher.cancellation());
        let updates = status.clone();
        let failure = sink.failure.or(error);
        finishing.spawn(async move {
            let final_state = if failure.is_some() {
                pusher.stop().await
            } else {
                pusher.finish().await
            };
            updates.send_modify(|current| {
                if let Some(item) = current.outputs.get_mut(index) {
                    match final_state {
                        Ok(ref final_state) => *item = final_state.clone(),
                        Err(reason) => item.state = OutputState::Failed(reason),
                    }
                    if let Some(reason) = failure {
                        item.state = OutputState::Failed(reason);
                    }
                }
            });
        });
    }
    let mut listen_for_stop = true;
    while !finishing.is_empty() {
        tokio::select! {
            biased;
            _=stopped.changed(), if listen_for_stop=>{
                listen_for_stop=false;
                error.get_or_insert(MediaError::Cancelled);
                for cancel in &cancellations {cancel.send_replace(true);}
            },
            result=finishing.join_next()=>{
                if matches!(result,Some(Err(_))) {
                    error.get_or_insert(MediaError::ProcessCleanupFailed);
                    status.send_modify(|current| {
                        for output in &mut current.outputs {
                            if matches!(output.state,OutputState::Starting|OutputState::Publishing) {output.state=OutputState::Failed(MediaError::ProcessCleanupFailed);}
                        }
                    });
                }
            },
        }
    }
    if let Some(reason) = error {
        status.send_modify(|current| {
            for output in &mut current.outputs {
                if matches!(
                    output.state,
                    OutputState::Starting | OutputState::Publishing
                ) {
                    output.state = OutputState::Failed(reason);
                }
            }
        });
    }
    let final_status = status.borrow().clone();
    MediaReport {
        status: final_status,
        error,
    }
}

/// Der erste decodierbare Keyframe bestimmt den Videoursprung auf derselben
/// Millisekundenachse wie AAC. Der FPS-Filter darf diese Achse nicht auf Null runden.
async fn video_origin(
    identity: TrackIdentity,
    graph: &Graph,
    input: &mut mpsc::Receiver<MediaEvent>,
    prefix: &mut VecDeque<MediaEvent>,
    limits: &MediaLimits,
) -> Result<i64> {
    let deadline = Instant::now() + limits.startup_timeout;
    let mut inspected = 0;
    let mut bytes = 0usize;
    loop {
        while let Some(event) = prefix.get(inspected) {
            inspected += 1;
            bytes = bytes
                .checked_add(event.wire_body().len() + 15)
                .ok_or(MediaError::ResourceLimit)?;
            if inspected > limits.queue_events || bytes > limits.queue_bytes {
                return Err(MediaError::Backpressure);
            }
            if event.identity.session != identity.session
                || event.identity.generation != identity.generation
            {
                return Err(MediaError::WrongSession);
            }
            if event.identity.track.kind == MediaKind::Audio
                && !graph.expected_tracks.contains(&event.identity.track)
            {
                continue;
            }
            if !graph.expected_tracks.contains(&event.identity.track) {
                return Err(MediaError::WrongSession);
            }
            if event.identity.track == graph.video_track && event.event_kind == EventKind::Frame {
                if event
                    .wire_body()
                    .first()
                    .is_none_or(|byte| byte & 0x70 != 0x10)
                    || !(0..=i64::from(u32::MAX)).contains(&event.pts_ms)
                {
                    return Err(MediaError::InvalidMedia);
                }
                return Ok(event.pts_ms);
            }
        }
        let event = timeout_at(deadline, input.recv())
            .await
            .map_err(|_| MediaError::StartTimeout)?
            .ok_or(MediaError::MissingTrack)?;
        prefix.push_back(event);
    }
}

#[allow(clippy::too_many_arguments)]
async fn consume(
    identity: uplink_ingest::TrackIdentity,
    graph: &Graph,
    input: &mut mpsc::Receiver<MediaEvent>,
    prefix: &mut VecDeque<MediaEvent>,
    worker: &mut Option<ChildStdin>,
    sinks: &Sinks,
    status: &watch::Sender<MediaStatus>,
    limits: &MediaLimits,
) -> Result<()> {
    let startup_deadline = Instant::now() + limits.startup_timeout;
    let mut headers = HashMap::new();
    let mut pending = Vec::new();
    let mut pending_bytes = 0;
    let mut keyframe = false;
    let mut started = false;
    let mut observed = HashSet::new();
    let maximum_timestamp = graph
        .profiles
        .iter()
        .filter_map(|profile| profile.maximum_input_timestamp_ms())
        .min();
    if let Some(worker) = worker {
        timeout(limits.write_timeout, worker.write_all(HEADER))
            .await
            .map_err(|_| MediaError::Backpressure)?
            .map_err(|_| MediaError::Io)?;
    }
    loop {
        let next = if let Some(event) = prefix.pop_front() {
            Some(event)
        } else if started {
            input.recv().await
        } else {
            timeout_at(startup_deadline, input.recv())
                .await
                .map_err(|_| MediaError::StartTimeout)?
        };
        let Some(event) = next else {
            break;
        };
        if event.identity.session != identity.session
            || event.identity.generation != identity.generation
        {
            return Err(MediaError::WrongSession);
        }
        if event.identity.track.kind == MediaKind::Audio
            && !graph.expected_tracks.contains(&event.identity.track)
        {
            continue;
        }
        if !graph.expected_tracks.contains(&event.identity.track) {
            return Err(MediaError::WrongSession);
        }
        if maximum_timestamp
            .is_some_and(|maximum| i64::from(event.dts_ms) > maximum || event.pts_ms > maximum)
        {
            return Err(MediaError::InvalidMedia);
        }
        if event.configuration_revision != 1
            && !(event.configuration_revision == 0 && event.event_kind == EventKind::Metadata)
        {
            return Err(MediaError::UnsupportedProfile);
        }
        if graph.codecs.get(&event.identity.track) != Some(&event.codec) {
            return Err(MediaError::UnsupportedProfile);
        }
        let mut tag = FlvTag::from_event(&event, limits.max_tag_bytes)?;
        if let Some(canonical) = graph.input_audio.get(&event.identity.track) {
            tag = tag.with_audio_track(*canonical, limits.routing_limits().max_tag_bytes)?;
        }
        let tag = Arc::new(tag);
        if !started {
            pending_bytes += tag.wire_len();
            if pending_bytes > limits.queue_bytes
                || pending.len() + headers.len() >= limits.queue_events
            {
                return Err(MediaError::Backpressure);
            }
            if event.event_kind == EventKind::SequenceHeader {
                observed.insert(event.identity.track);
                headers.insert(event.identity.track, tag);
            } else {
                if event.identity.track == graph.video_track
                    && event.event_kind == EventKind::Frame
                    && tag.body()[0] & 0x70 == 0x10
                {
                    keyframe = true;
                }
                pending.push(tag);
            }
            if observed != graph.expected_tracks || !keyframe {
                continue;
            }
            let mut ordered: Vec<_> = headers.drain().collect();
            ordered.sort_by_key(|(track, _)| {
                (
                    u8::from(track.kind == uplink_ingest::MediaKind::Audio),
                    graph.input_audio.get(track).copied().unwrap_or(0),
                )
            });
            for tag in ordered
                .into_iter()
                .map(|(_, tag)| tag)
                .chain(pending.drain(..))
            {
                deliver(tag, worker, sinks, status, limits).await?;
            }
            started = true;
        } else {
            deliver(tag, worker, sinks, status, limits).await?;
        }
    }
    if !started {
        return Err(MediaError::MissingTrack);
    }
    Ok(())
}
async fn deliver(
    tag: Arc<FlvTag>,
    worker: &mut Option<ChildStdin>,
    sinks: &Sinks,
    status: &watch::Sender<MediaStatus>,
    limits: &MediaLimits,
) -> Result<()> {
    distribute(tag.clone(), None, 0, sinks, limits);
    update_status(sinks, status);
    if let Some(worker) = worker {
        timeout(limits.write_timeout, tag.write_to(worker))
            .await
            .map_err(|_| MediaError::Backpressure)??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uplink_core::{Codec, FrameRate};
    use uplink_ingest::{
        AuthorizedSession, Authorizer, IngestLimits, IngestServer, MediaKind, WireTrack,
    };

    mod tls {
        include!("../../uplink-ingest/tests/support/tls.rs");
    }
    struct Auth;
    impl Authorizer for Auth {
        async fn authorize(
            &self,
            app: &str,
            stream: &str,
        ) -> std::result::Result<AuthorizedSession, ()> {
            if app == "live" && stream == "local-fixture" {
                AuthorizedSession::new(1, 1).map_err(|_| ())
            } else {
                Err(())
            }
        }
    }
    fn target(id: &str, port: u16) -> PublishTarget {
        PublishTarget {
            id: id.into(),
            endpoint: format!("rtmps://localhost:{port}/live"),
            playpath: PublishSecret::new(b"local-fixture".to_vec()).unwrap(),
            tls: None,
            allowed_hosts: vec!["localhost".into()],
            allow_loopback: true,
            allow_unencrypted: false,
        }
    }

    #[tokio::test]
    async fn maximum_sized_video_and_audio_allow_multitrack_wrappers() {
        for (kind, mut bytes) in [(9, vec![0x17, 1, 0, 0, 0]), (8, vec![0xaf, 1])] {
            let limits = MediaLimits {
                max_tag_bytes: 64,
                queue_bytes: 4096,
                ..MediaLimits::default()
            };
            bytes.resize(limits.max_tag_bytes, 0x12);
            let tag = Arc::new(FlvTag::new(kind, 0, bytes.into(), limits.max_tag_bytes).unwrap());
            let mut target = target("track", 9);
            target.endpoint = "rtmp://localhost:9/live".into();
            target.allow_unencrypted = true;
            let pusher = RunningPusher::spawn(target, limits.routing_limits()).unwrap();
            let sinks = Arc::new(Mutex::new(vec![Sink {
                pusher: Some(pusher),
                routing: crate::graph::Routing {
                    timestamp_offset_ms: 0,
                    group: None,
                    video: vec![(None, 5)],
                    audio: vec![(0, 3)],
                    failure: None,
                },
                failure: None,
            }]));
            distribute(tag, None, 0, &sinks, &limits);
            assert_eq!(
                sinks.lock().unwrap()[0].failure,
                None,
                "Spurumschrift muss Wrapperplatz haben"
            );
        }
    }

    #[tokio::test]
    async fn source_ending_before_first_video_marks_every_output_failed() {
        timeout(std::time::Duration::from_secs(3), async {
            let tls = tls::test_tls();
            drop(tls.certificate_pem);
            let server = IngestServer::bind_loopback(
                0,
                tls.server,
                Arc::new(Auth),
                IngestLimits::local_probe(),
            )
            .await
            .unwrap();
            let mut source_target = target("source", server.local_addr().unwrap().port());
            source_target.tls = Some(tls.client);
            let input = tokio::spawn(async move {
                let mut connection = server.accept().await.unwrap();
                let first = connection.next().await.unwrap();
                assert_eq!(first.identity.track.kind, MediaKind::Audio);
                while connection.next().await.is_some() {}
                let _ = connection.finish().await;
                first
            });
            let pusher = RunningPusher::start(source_target, MediaLimits::default())
                .await
                .unwrap();
            pusher
                .try_send(Arc::new(
                    FlvTag::new(8, 0, Arc::from(&b"\xaf\x00\x11\x90"[..]), 64).unwrap(),
                ))
                .unwrap();
            pusher.finish().await.unwrap();
            let first = input.await.unwrap();
            let identity = TrackIdentity {
                track: WireTrack {
                    kind: MediaKind::Video,
                    wire_id: 0,
                },
                ..first.identity
            };
            let output_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = output_listener.local_addr().unwrap().port();
            let outputs: Vec<_> = ["one", "two"]
                .into_iter()
                .map(|id| DesiredOutput {
                    target: target(id, port),
                    video: DesiredVideo {
                        width: 320,
                        height: 180,
                        fps: FrameRate::new(25, 1).unwrap(),
                        bitrate_kbps: 384,
                        codec: Codec::H264,
                    },
                    live_audio_track: 0,
                    vod_audio_track: None,
                    layout: None,
                })
                .collect();
            let observation = SourceObservation {
                video_wire_track: 0,
                codec: "h264".into(),
                width: 320,
                height: 180,
                fps_numerator: 25,
                fps_denominator: 1,
                pixel_format: "yuv420p".into(),
                color_primaries: None,
                color_transfer: None,
                color_matrix: None,
                color_range: None,
                rate_control: None,
                gop_frames: None,
                audio: vec![AudioObservation {
                    wire_track: 0,
                    codec: "aac".into(),
                    sample_rate: 48000,
                    channels: 2,
                }],
                sampled_events: 1,
                sampled_bytes: 4,
                sampled_duration_ms: 0,
            };
            let graph = Graph::observed(&observation, &outputs).unwrap();
            let routes = outputs
                .into_iter()
                .map(|output| TargetRoute {
                    output_id: output.target.id.clone(),
                    target: output.target,
                })
                .collect();
            let engine = MediaEngine::new(EngineConfig {
                ffmpeg: "/usr/bin/false".into(),
                ffprobe: "/usr/bin/false".into(),
                work_directory: "/not-created-by-this-test".into(),
                limits: MediaLimits::default(),
            })
            .unwrap();
            let (sender, receiver) = mpsc::channel(1);
            drop(sender);
            let running = engine
                .start_graph(
                    identity,
                    routes,
                    graph,
                    receiver,
                    VecDeque::from([first]),
                    None,
                )
                .unwrap();
            let observer = running.observer();
            let report = running.finish().await;
            assert_eq!(report.error, Some(MediaError::MissingTrack));
            assert_eq!(report.status.outputs.len(), 2);
            for status in [report.status, observer.snapshot()] {
                for output in status.outputs {
                    assert_eq!(output.state, OutputState::Failed(MediaError::MissingTrack));
                }
            }
            assert!(
                timeout(
                    std::time::Duration::from_millis(30),
                    output_listener.accept()
                )
                .await
                .is_err()
            );
        })
        .await
        .expect("früher Eingangsfehler muss ohne Worker-/Ausgangsstart abschließen");
    }

    async fn stopped_endpoint(name: &'static str) -> PublishTarget {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let mut target = target(name, port);
        target.endpoint = format!("rtmp://localhost:{port}/live");
        target.allow_unencrypted = true;
        target
    }

    #[tokio::test]
    async fn video_backpressure_isolates_only_the_stalled_sink() {
        let stalled_limits = MediaLimits {
            queue_events: 1,
            ..MediaLimits::default()
        };
        let stalled =
            RunningPusher::spawn(stopped_endpoint("stalled").await, stalled_limits).unwrap();
        let healthy =
            RunningPusher::spawn(stopped_endpoint("healthy").await, MediaLimits::default())
                .unwrap();
        let sinks = Arc::new(Mutex::new(vec![
            Sink {
                pusher: Some(stalled),
                routing: Routing {
                    timestamp_offset_ms: 0,
                    group: Some(0),
                    video: vec![(Some(0), 0)],
                    audio: Vec::new(),
                    failure: None,
                },
                failure: None,
            },
            Sink {
                pusher: Some(healthy),
                routing: Routing {
                    timestamp_offset_ms: 0,
                    group: Some(0),
                    video: vec![(Some(0), 3)],
                    audio: Vec::new(),
                    failure: None,
                },
                failure: None,
            },
        ]));
        let body: Arc<[u8]> = Arc::from(&b"\x17\x01\x00\x00\x00"[..]);
        let limits = MediaLimits::default();
        distribute(
            Arc::new(FlvTag::new(9, 0, body.clone(), 64).unwrap()),
            Some(0),
            0,
            &sinks,
            &limits,
        );
        assert!(sinks.lock().unwrap()[0].failure.is_none());
        distribute(
            Arc::new(FlvTag::new(9, 40, body, 64).unwrap()),
            Some(0),
            0,
            &sinks,
            &limits,
        );
        assert_eq!(
            sinks.lock().unwrap()[0].failure,
            Some(MediaError::Backpressure),
            "Ein volles Ziel meldet Rückdruck als eigenen Fehler"
        );
        distribute(
            Arc::new(FlvTag::new(9, 80, Arc::from(&b"\x17\x01\x00\x00\x00"[..]), 64).unwrap()),
            Some(0),
            0,
            &sinks,
            &limits,
        );
        assert_eq!(
            sinks.lock().unwrap()[1].failure,
            None,
            "Der Rückdruck eines Ziels darf das gesunde Ziel nicht treffen"
        );
    }
}
