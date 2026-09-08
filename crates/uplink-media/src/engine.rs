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
use uplink_ingest::{EventKind, MediaEvent};

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
        if !config.ffmpeg.is_absolute()
            || !config.ffmpeg.is_file()
            || !config.ffprobe.is_absolute()
            || !config.ffprobe.is_file()
            || limits.max_tag_bytes == 0
            || limits.max_tag_bytes > 0xff_ffff
            || limits.queue_bytes < limits.max_tag_bytes + 15
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
            outputs: routes
                .iter()
                .map(|route| OutputStatus {
                    id: route.target.id.clone(),
                    state: OutputState::Starting,
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

fn distribute(tag: Arc<FlvTag>, group: Option<usize>, sinks: &Sinks, limits: &MediaLimits) {
    let mut sinks = sinks.lock().unwrap_or_else(|error| error.into_inner());
    for sink in sinks
        .iter_mut()
        .filter(|sink| sink.routing.group == group && sink.failure.is_none())
    {
        let Some(pusher) = sink.pusher.as_ref() else {
            continue;
        };
        let result = if tag.kind() == 8 {
            tag.audio_track().and_then(|track| {
                for (source, destination) in &sink.routing.audio {
                    if Some(*source) == track {
                        pusher.try_send(Arc::new(
                            tag.with_audio_track(*destination, limits.max_tag_bytes)?,
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
        let started = RunningPusher::spawn(route.target, config.limits.clone());
        match started {
            Ok(pusher) => sinks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(Sink {
                    pusher: Some(pusher),
                    routing: Routing {
                        group: routing.group,
                        audio: routing.audio.clone(),
                    },
                    failure: None,
                }),
            Err(reason) => {
                sinks
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(Sink {
                        pusher: None,
                        routing: Routing {
                            group: routing.group,
                            audio: routing.audio.clone(),
                        },
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
                let limits = config.limits.clone();
                let output_sinks = sinks.clone();
                let output_status = status.clone();
                readers.push(tokio::spawn(async move {
                    let stream = accept_worker(&listener, pid, limits.startup_timeout).await?;
                    let mut reader = FlvReader::new(stream, limits.max_tag_bytes);
                    while let Some(tag) = reader.next().await? {
                        distribute(Arc::new(tag), Some(index), &output_sinks, &limits);
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
                || !graph.expected_tracks.contains(&event.identity.track)
            {
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
            || !graph.expected_tracks.contains(&event.identity.track)
        {
            return Err(MediaError::WrongSession);
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
            tag = tag.with_audio_track(*canonical, limits.max_tag_bytes)?;
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
    distribute(tag.clone(), None, sinks, limits);
    update_status(sinks, status);
    if let Some(worker) = worker {
        timeout(limits.write_timeout, tag.write_to(worker))
            .await
            .map_err(|_| MediaError::Backpressure)??;
    }
    Ok(())
}
