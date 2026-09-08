use crate::{Error, Result, Storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
};
use uplink_ingest::{EventKind, MediaKind, WireCodec, WireTrack};
use uplink_media::flv::FlvTag;

pub struct RecordingPacket {
    pub generation: u64,
    pub track: WireTrack,
    pub codec: WireCodec,
    pub event_kind: EventKind,
    pub configuration_revision: u64,
    pub pts_ms: i64,
    pub tag: Arc<FlvTag>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSpec {
    pub session_id: i64,
    pub streamer_id: u64,
    pub live_audio_track: u8,
    pub vod_audio_track: Option<u8>,
    pub metadata: serde_json::Value,
}
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    pub max_events: usize,
    pub max_queue_bytes: usize,
    pub max_recording_bytes: u64,
    pub segment_bytes: u64,
    pub max_segments: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub file: String,
    pub generation: u64,
    pub bytes: u64,
    pub sha256: String,
    pub first_dts_ms: u32,
    pub last_dts_ms: u32,
    pub first_pts_ms: i64,
    pub last_pts_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub kind: String,
    pub wire_id: u8,
    pub codec: String,
    pub configuration_revision: u64,
    pub headers: u64,
    pub frames: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingManifest {
    pub version: u32,
    pub object_id: String,
    pub spec: RecordingSpec,
    pub complete: bool,
    pub has_gaps: bool,
    pub error: Option<Error>,
    pub segments: Vec<Segment>,
    pub tracks: Vec<Track>,
}
impl RecordingManifest {
    pub async fn verify_segments(&self, storage: &Storage) -> Result<()> {
        if self.segments.is_empty() || self.segments.len() > 4096 {
            return Err(Error::Incomplete);
        }
        for segment in &self.segments {
            let actual = storage.digest(&self.object_id, &segment.file).await?;
            if actual.bytes != segment.bytes || actual.sha256 != segment.sha256 {
                return Err(Error::Incomplete);
            }
        }
        Ok(())
    }
    pub fn has_vod_audio(&self) -> bool {
        self.spec.vod_audio_track.is_some_and(|id| {
            self.tracks
                .iter()
                .any(|t| t.kind == "audio" && t.wire_id == id && t.headers > 0 && t.frames > 0)
        })
    }
}
struct Queued {
    packet: RecordingPacket,
    _bytes: OwnedSemaphorePermit,
    _event: OwnedSemaphorePermit,
}
struct State {
    pending_copies: std::sync::atomic::AtomicUsize,
    metadata: std::sync::Mutex<serde_json::Value>,
    failed: AtomicBool,
    closed: AtomicBool,
}
#[derive(Clone)]
pub struct RecordingSink {
    tx: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    events: Arc<Semaphore>,
    state: Arc<State>,
}
pub struct RecordingPermit {
    sink: RecordingSink,
    bytes: Option<OwnedSemaphorePermit>,
    event: Option<OwnedSemaphorePermit>,
    submitted: bool,
    length: usize,
}
impl RecordingSink {
    pub fn update_metadata(&self, value: serde_json::Value) -> Result<()> {
        if serde_json::to_vec(&value)
            .map_err(|_| Error::Invalid)?
            .len()
            > 65536
            || self.state.closed.load(Ordering::Acquire)
        {
            return Err(Error::Invalid);
        }
        *self.state.metadata.lock().map_err(|_| Error::Incomplete)? = value;
        Ok(())
    }
    pub fn mark_incomplete(&self) {
        self.state.failed.store(true, Ordering::Release);
    }
    /// Vor dem Kopieren eines Ingest-Pakets reservieren. Kein Ingest-Permit behalten.
    pub fn reserve(&self, length: usize) -> Result<RecordingPermit> {
        if self.state.closed.load(Ordering::Acquire) || self.state.failed.load(Ordering::Acquire) {
            return Err(Error::Incomplete);
        }
        let error = || {
            self.state.failed.store(true, Ordering::Release);
            Error::Incomplete
        };
        let count = u32::try_from(length)
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(error)?;
        let event = self
            .events
            .clone()
            .try_acquire_owned()
            .map_err(|_| error())?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(count)
            .map_err(|_| error())?;
        self.state.pending_copies.fetch_add(1, Ordering::AcqRel);
        Ok(RecordingPermit {
            sink: self.clone(),
            bytes: Some(bytes),
            event: Some(event),
            submitted: false,
            length,
        })
    }
}
impl RecordingPermit {
    pub fn submit(mut self, packet: RecordingPacket) -> Result<()> {
        if packet.tag.wire_len() != self.length
            || self.sink.state.closed.load(Ordering::Acquire)
            || self.sink.state.failed.load(Ordering::Acquire)
        {
            self.sink.state.failed.store(true, Ordering::Release);
            return Err(Error::Incomplete);
        }
        let queued = Queued {
            packet,
            _bytes: self.bytes.take().ok_or(Error::Incomplete)?,
            _event: self.event.take().ok_or(Error::Incomplete)?,
        };
        self.submitted = true;
        self.sink.tx.try_send(queued).map_err(|_| {
            self.sink.state.failed.store(true, Ordering::Release);
            Error::Incomplete
        })
    }
}
impl Drop for RecordingPermit {
    fn drop(&mut self) {
        if !self.submitted {
            self.sink.state.failed.store(true, Ordering::Release);
        }
        self.sink
            .state
            .pending_copies
            .fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct Recorder;
pub struct RunningRecording {
    object_id: String,
    sink: RecordingSink,
    finish: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<RecordingManifest>>>,
}
impl RunningRecording {
    pub fn object_id(&self) -> &str {
        &self.object_id
    }
    pub fn sink(&self) -> RecordingSink {
        self.sink.clone()
    }
    pub async fn finish(mut self) -> Result<RecordingManifest> {
        self.sink.state.closed.store(true, Ordering::Release);
        self.finish.send_replace(true);
        let mut task = self.task.take().ok_or(Error::Incomplete)?;
        match tokio::time::timeout(Duration::from_secs(30), &mut task).await {
            Ok(result) => result.map_err(|_| Error::Incomplete)?,
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(Error::Incomplete)
            }
        }
    }
}
impl Drop for RunningRecording {
    fn drop(&mut self) {
        self.sink.state.failed.store(true, Ordering::Release);
        self.sink.state.closed.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.abort()
        }
    }
}
impl Recorder {
    pub async fn start(
        storage: Storage,
        spec: RecordingSpec,
        cfg: RecordingConfig,
    ) -> Result<RunningRecording> {
        if spec.session_id <= 0
            || spec.streamer_id == 0
            || cfg.max_events == 0
            || cfg.max_events > 4096
            || cfg.max_queue_bytes == 0
            || cfg.max_queue_bytes > 64 * 1024 * 1024
            || cfg.max_recording_bytes == 0
            || cfg.segment_bytes == 0
            || cfg.max_segments == 0
            || cfg.max_segments > 4096
        {
            return Err(Error::Invalid);
        }
        if serde_json::to_vec(&spec.metadata)
            .map_err(|_| Error::Invalid)?
            .len()
            > 65536
        {
            return Err(Error::Invalid);
        }
        let object_id = storage.create_object()?;
        let manifest = RecordingManifest {
            version: 1,
            object_id: object_id.clone(),
            spec,
            complete: false,
            has_gaps: false,
            error: None,
            segments: vec![],
            tracks: vec![],
        };
        storage.write_manifest(&object_id, &manifest).await?;
        let state = Arc::new(State {
            pending_copies: std::sync::atomic::AtomicUsize::new(0),
            metadata: std::sync::Mutex::new(manifest.spec.metadata.clone()),
            failed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel(cfg.max_events);
        let (finish, watch) = watch::channel(false);
        let sink = RecordingSink {
            tx,
            bytes: Arc::new(Semaphore::new(cfg.max_queue_bytes)),
            events: Arc::new(Semaphore::new(cfg.max_events)),
            state: state.clone(),
        };
        let task = tokio::spawn(record(storage, manifest, cfg, rx, watch, state));
        Ok(RunningRecording {
            object_id,
            sink,
            finish,
            task: Some(task),
        })
    }
}
async fn record(
    storage: Storage,
    mut manifest: RecordingManifest,
    cfg: RecordingConfig,
    mut rx: mpsc::Receiver<Queued>,
    mut finish: watch::Receiver<bool>,
    state: Arc<State>,
) -> Result<RecordingManifest> {
    let mut file: Option<HashedFile> = None;
    let mut total = 0u64;
    let mut headers: BTreeMap<WireTrack, Arc<FlvTag>> = BTreeMap::new();
    loop {
        let queued = if *finish.borrow() {
            rx.close();
            rx.recv().await
        } else {
            tokio::select! {packet=rx.recv()=>packet,_=finish.changed()=>{rx.close();continue}}
        };
        let Some(queued) = queued else { break };
        let packet = &queued.packet;
        if state.failed.load(Ordering::Acquire) {
            manifest.error = Some(Error::Incomplete);
            break;
        }
        let generation_changed = manifest
            .segments
            .last()
            .is_some_and(|s| s.generation != packet.generation);
        if generation_changed {
            manifest.has_gaps = true;
            headers.clear()
        }
        let keyframe = packet.track.kind == MediaKind::Video
            && packet.event_kind == EventKind::Frame
            && packet.tag.body()[0] & 0x70 == 0x10;
        let rotate = file.is_none()
            || generation_changed
            || (keyframe
                && manifest
                    .segments
                    .last()
                    .is_some_and(|s| s.bytes >= cfg.segment_bytes));
        if rotate {
            if let Some(old) = file.take() {
                old.file.sync_all().await.map_err(|_| Error::Storage)?;
                if let Some(segment) = manifest.segments.last_mut() {
                    segment.sha256 = hex::encode(old.hash.finalize());
                }
            }
            if manifest.segments.len() >= cfg.max_segments {
                manifest.error = Some(Error::StorageFull);
                break;
            }
            let name = format!("input-{:04}.flv", manifest.segments.len());
            let raw = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(storage.path(&manifest.object_id, &name)?)
                .await
                .map_err(|_| Error::Storage)?;
            let mut new = HashedFile {
                file: raw,
                hash: Sha256::new(),
            };
            let size = 13 + headers.values().map(|h| h.wire_len() as u64).sum::<u64>();
            if total.saturating_add(size) > cfg.max_recording_bytes {
                manifest.error = Some(Error::StorageFull);
                break;
            }
            storage.reserve(size)?.commit();
            new.write_all(uplink_media::flv::HEADER)
                .await
                .map_err(|_| Error::Storage)?;
            for header in headers.values() {
                header
                    .write_to(&mut new)
                    .await
                    .map_err(|_| Error::Storage)?;
            }
            total += size;
            manifest.segments.push(Segment {
                file: name,
                generation: packet.generation,
                bytes: size,
                sha256: String::new(),
                first_dts_ms: packet.tag.timestamp_ms(),
                last_dts_ms: packet.tag.timestamp_ms(),
                first_pts_ms: packet.pts_ms,
                last_pts_ms: packet.pts_ms,
            });
            file = Some(new);
        }
        let bytes = packet.tag.wire_len() as u64;
        if total.saturating_add(bytes) > cfg.max_recording_bytes {
            manifest.error = Some(Error::StorageFull);
            break;
        }
        let reserve = match storage.reserve(bytes) {
            Ok(r) => r,
            Err(e) => {
                manifest.error = Some(e);
                break;
            }
        };
        let written = tokio::time::timeout(
            Duration::from_secs(10),
            packet.tag.write_to(file.as_mut().ok_or(Error::Storage)?),
        )
        .await;
        // Auch eine teilweise geschriebene Datei beansprucht Budget bis zur geprüften Bereinigung.
        reserve.commit();
        total += bytes;
        if !matches!(written, Ok(Ok(()))) {
            manifest.error = Some(Error::Storage);
            break;
        }
        if let Some(segment) = manifest.segments.last_mut() {
            segment.bytes += bytes;
            segment.last_dts_ms = packet.tag.timestamp_ms();
            segment.last_pts_ms = packet.pts_ms;
        }
        let kind = if packet.track.kind == MediaKind::Audio {
            "audio"
        } else {
            "video"
        };
        let index = if let Some(index) = manifest.tracks.iter().position(|t| {
            t.kind == kind
                && t.wire_id == packet.track.wire_id
                && t.configuration_revision == packet.configuration_revision
        }) {
            index
        } else {
            if manifest.tracks.len() >= 512 {
                manifest.error = Some(Error::Incomplete);
                break;
            }
            manifest.tracks.push(Track {
                kind: kind.into(),
                wire_id: packet.track.wire_id,
                codec: match packet.codec {
                    WireCodec::Aac => "aac",
                    WireCodec::Av1 => "av1",
                    WireCodec::H264 => "h264",
                }
                .into(),
                configuration_revision: packet.configuration_revision,
                headers: 0,
                frames: 0,
            });
            manifest.tracks.len() - 1
        };
        match packet.event_kind {
            EventKind::SequenceHeader => {
                manifest.tracks[index].headers += 1;
                headers.insert(packet.track, packet.tag.clone());
            }
            EventKind::Frame => manifest.tracks[index].frames += 1,
            _ => {}
        }
    }
    rx.close();
    drop(rx);
    if let Some(file) = file {
        if file.file.sync_all().await.is_err() {
            manifest.error = Some(Error::Storage);
        }
        if let Some(segment) = manifest.segments.last_mut() {
            segment.sha256 = hex::encode(file.hash.finalize());
        }
    }
    manifest.complete = state.pending_copies.load(Ordering::Acquire) == 0
        && !state.failed.load(Ordering::Acquire)
        && !manifest.has_gaps
        && manifest.error.is_none()
        && manifest.has_vod_audio()
        && manifest
            .tracks
            .iter()
            .any(|t| t.kind == "video" && t.frames > 0 && t.headers > 0);
    if !manifest.complete && manifest.error.is_none() {
        manifest.error = Some(if !manifest.has_vod_audio() {
            Error::MissingVodAudio
        } else {
            Error::Incomplete
        })
    }
    manifest.spec.metadata = state
        .metadata
        .lock()
        .map_err(|_| Error::Incomplete)?
        .clone();
    storage
        .write_manifest(&manifest.object_id, &manifest)
        .await?;
    Ok(manifest)
}

/// Prüfsumme umfasst genau die vom Dateideskriptor angenommenen Bytes.
struct HashedFile {
    file: tokio::fs::File,
    hash: Sha256,
}
impl tokio::io::AsyncWrite for HashedFile {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match std::pin::Pin::new(&mut self.file).poll_write(cx, buffer) {
            std::task::Poll::Ready(Ok(written)) => {
                self.hash.update(&buffer[..written]);
                std::task::Poll::Ready(Ok(written))
            }
            other => other,
        }
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.file).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.file).poll_shutdown(cx)
    }
}
