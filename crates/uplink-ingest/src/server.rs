use crate::{EventKind, MediaError, MediaKind, WireCodec, WireTrack, media::inspect};
use bytes::Bytes;
pub use scuffle_rtmp::session::server::ServerSessionLimits;
use scuffle_rtmp::session::server::{
    ServerSession, ServerSessionError, SessionData, SessionHandler,
};
use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroU64,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::JoinHandle,
    time::{Instant, sleep_until, timeout_at},
};
use tokio_rustls::{TlsAcceptor, rustls::ServerConfig};
mod admission;
use admission::SessionSlot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthorizedSession {
    tenant: NonZeroU64,
    session: NonZeroU64,
}
impl AuthorizedSession {
    pub fn tenant_id(self) -> u64 {
        self.tenant.get()
    }
    pub fn session_id(self) -> u64 {
        self.session.get()
    }
    pub fn new(tenant: u64, session: u64) -> Result<Self, IngestError> {
        Ok(Self {
            tenant: NonZeroU64::new(tenant).ok_or(IngestError::InvalidIdentity)?,
            session: NonZeroU64::new(session).ok_or(IngestError::InvalidIdentity)?,
        })
    }
}

/// Der vorhandene Broker kann hier angebunden werden. Kein Token wird gehalten,
/// keine eigene Identität oder Berechtigung aus einem Namen abgeleitet.
pub trait Authorizer: Send + Sync + 'static {
    fn authorize(
        &self,
        app: &str,
        stream: &str,
    ) -> impl Future<Output = Result<AuthorizedSession, ()>> + Send;
    /// Synchroner, kurzer Abschluss jeder erfolgreichen Autorisierung, auch bei
    /// Taskabbruch. Verbraucher halten benötigte eigene Reservationsanteile.
    fn release(&self, _session: AuthorizedSession) {}
    /// Genau ein kurzer, synchroner Abschluss vor release, auch ohne Medien.
    /// Nur erfolgreich autorisierte Sessions werden zugeordnet. Bei Taskabbruch
    /// bleibt ein bereits belegter Fehler erhalten, sonst lautet er TaskFailed.
    fn completed(&self, _session: AuthorizedSession, _report: &SessionReport) {}
    /// Optionaler, nicht serialisierbarer Reservationsanteil. Handler und alle
    /// weitergereichten Medien halten ihn bis zum letzten Verbraucher.
    fn retention(&self, _session: AuthorizedSession) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// Frische Serverinstanz (128 Bit Zufall) plus nicht wiederverwendbarer Zähler.
/// Die Autorisierung liefert diese Generation ausdrücklich nicht selbst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionGeneration {
    instance: [u8; 16],
    counter: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackIdentity {
    pub session: AuthorizedSession,
    pub generation: ConnectionGeneration,
    pub track: WireTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestError {
    InvalidLimits,
    InvalidIdentity,
    RandomUnavailable,
    BindFailed,
    AcceptFailed,
    Capacity,
    GenerationExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    PeerClosed,
    ExplicitStop,
    TlsRejected,
    StartTimeout,
    MediaTimeout,
    ProtocolTimeout,
    AuthorizationRejected,
    SessionCapacity,
    ProtocolRejected,
    MediaRejected(MediaError),
    Backpressure,
    ConsumerClosed,
    TaskFailed,
}

#[derive(Debug, Clone)]
pub struct SessionReport {
    pub reason: EndReason,
    pub duration: Duration,
    pub generation: ConnectionGeneration,
    pub received_events: u64,
    pub received_bytes: u64,
    /// Größter vor Versand beobachteter Eventbudgetwert. Nebenläufige Drops
    /// können frühere Spitzen verkürzen; kein exakter historischer Peak/RAM-Wert.
    pub max_queued_bytes: usize,
    pub track_count: usize,
    pub ignored_amf_messages: u64,
}

/// Ausschließlich empfangene Wire-Metadaten, kein vollständiges Planner-Profil.
/// Die Rohbytes enthalten keine geprüfte Auflösung/FPS oder Live-/VOD-Rolle.
pub struct MediaEvent {
    pub identity: TrackIdentity,
    pub event_kind: EventKind,
    pub codec: WireCodec,
    pub dts_ms: u32,
    pub pts_ms: i64,
    pub configuration_revision: u64,
    body: Box<[u8]>,
    payload: Range<usize>,
    _budget: OwnedSemaphorePermit,
    _event_budget: OwnedSemaphorePermit,
    _slot: Arc<SessionSlot>,
    retention: Option<Arc<dyn Any + Send + Sync>>,
}
impl MediaEvent {
    pub fn authorization_retention(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.retention.clone()
    }
    pub fn payload(&self) -> &[u8] {
        &self.body[self.payload.clone()]
    }
    pub fn wire_body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Debug, Clone)]
pub struct IngestLimits {
    /// Gleichzeitige autorisierte Sessions, einschließlich gehaltener Events.
    pub max_connections: usize,
    /// Eigener Pool für TCP/TLS/RTMP vor erfolgreicher Autorisierung.
    pub max_pending_connections: usize,
    pub max_event_bytes: usize,
    pub max_header_bytes: usize,
    pub max_tracks: usize,
    pub max_queued_events: usize,
    pub max_queued_bytes: usize,
    /// Absolute gemeinsame Frist ab TCP-Annahme bis TLS + RTMP + Autorisierung.
    pub start_timeout: Duration,
    /// Ausbleibende Medien, auch wenn Kontrollnachrichten weiter eintreffen.
    pub media_idle_timeout: Duration,
    pub rtmp: ServerSessionLimits,
}
impl IngestLimits {
    /// Technische Grenzen der lokalen Teststrecke. Kein Produktpreset.
    pub fn local_probe() -> Self {
        let mut rtmp = ServerSessionLimits::default();
        rtmp.chunk.max_message_bytes = 65536;
        rtmp.chunk.max_command_bytes = 16384;
        rtmp.chunk.max_chunk_streams = 16;
        rtmp.chunk.max_partial_messages = 4;
        rtmp.chunk.max_partial_bytes = 4 * 65536;
        rtmp.amf.max_input_bytes = 16384;
        Self {
            max_connections: 4,
            max_pending_connections: 4,
            max_event_bytes: 65536,
            max_header_bytes: 4096,
            max_tracks: 8,
            max_queued_events: 512,
            max_queued_bytes: 1024 * 1024,
            start_timeout: Duration::from_secs(10),
            media_idle_timeout: Duration::from_secs(5),
            rtmp,
        }
    }
    fn validate(&self) -> Result<(), IngestError> {
        self.rtmp
            .validate()
            .map_err(|_| IngestError::InvalidLimits)?;
        if self.max_connections == 0
            || self.max_connections > Semaphore::MAX_PERMITS
            || self.max_pending_connections == 0
            || self.max_pending_connections > Semaphore::MAX_PERMITS
            || self.max_event_bytes == 0
            || self.max_event_bytes > self.max_queued_bytes
            || self.max_queued_bytes > u32::MAX as usize
            || self.max_queued_bytes > Semaphore::MAX_PERMITS
            || self.max_header_bytes == 0
            || self.max_header_bytes > self.max_event_bytes
            || self.max_tracks == 0
            || self.max_queued_events == 0
            || self.max_queued_events > Semaphore::MAX_PERMITS
            || self.start_timeout.is_zero()
            || self.media_idle_timeout.is_zero()
            || self.start_timeout > Duration::from_secs(86_400)
            || self.media_idle_timeout > Duration::from_secs(86_400)
            || self.rtmp.max_publishing_streams != 1
        {
            return Err(IngestError::InvalidLimits);
        }
        Ok(())
    }
}

pub struct IngestServer<A: Authorizer> {
    listener: TcpListener,
    tls: Arc<ServerConfig>,
    authorizer: Arc<A>,
    limits: IngestLimits,
    slots: Arc<Semaphore>,
    pending_slots: Arc<Semaphore>,
    instance: [u8; 16],
    next: AtomicU64,
    loopback_only: bool,
}
impl<A: Authorizer> IngestServer<A> {
    /// Eine öffentliche Adresse kann mit dieser API absichtlich nicht gewählt werden.
    pub async fn bind_loopback(
        port: u16,
        tls: Arc<ServerConfig>,
        authorizer: Arc<A>,
        limits: IngestLimits,
    ) -> Result<Self, IngestError> {
        Self::bind(
            (Ipv4Addr::LOCALHOST, port).into(),
            tls,
            authorizer,
            limits,
            true,
        )
        .await
    }
    /// Explizite TLS-Bindung für den autorisierten Dienst. TLS und Authorizer
    /// müssen vor dem Öffnen bereitstehen; alle Admission-Grenzen bleiben aktiv.
    pub async fn bind_tls(
        address: SocketAddr,
        tls: Arc<ServerConfig>,
        authorizer: Arc<A>,
        limits: IngestLimits,
    ) -> Result<Self, IngestError> {
        Self::bind(address, tls, authorizer, limits, false).await
    }
    async fn bind(
        address: SocketAddr,
        tls: Arc<ServerConfig>,
        authorizer: Arc<A>,
        limits: IngestLimits,
        loopback_only: bool,
    ) -> Result<Self, IngestError> {
        limits.validate()?;
        let mut instance = [0; 16];
        getrandom::fill(&mut instance).map_err(|_| IngestError::RandomUnavailable)?;
        let listener = TcpListener::bind(address)
            .await
            .map_err(|_| IngestError::BindFailed)?;
        Ok(Self {
            listener,
            tls,
            authorizer,
            slots: Arc::new(Semaphore::new(limits.max_connections)),
            pending_slots: Arc::new(Semaphore::new(limits.max_pending_connections)),
            limits,
            instance,
            next: AtomicU64::new(1),
            loopback_only,
        })
    }
    pub fn local_addr(&self) -> Result<SocketAddr, IngestError> {
        self.listener
            .local_addr()
            .map_err(|_| IngestError::BindFailed)
    }
    pub async fn accept(&self) -> Result<RunningConnection, IngestError> {
        let (socket, peer) = self
            .listener
            .accept()
            .await
            .map_err(|_| IngestError::AcceptFailed)?;
        let start_deadline = Instant::now() + self.limits.start_timeout;
        if self.loopback_only && !peer.ip().is_loopback() {
            return Err(IngestError::AcceptFailed);
        }
        let pending = self
            .pending_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| IngestError::Capacity)?;
        let slot = Arc::new(SessionSlot::empty());
        let counter = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| IngestError::GenerationExhausted)?;
        let generation = ConnectionGeneration {
            instance: self.instance,
            counter,
        };
        let (sender, receiver) = mpsc::channel(self.limits.max_queued_events);
        let (stop_consumer, consumer_stopped) = watch::channel(false);
        let report = Arc::new(Mutex::new(SessionReport {
            reason: EndReason::ProtocolRejected,
            duration: Duration::ZERO,
            generation,
            received_events: 0,
            received_bytes: 0,
            max_queued_bytes: 0,
            track_count: 0,
            ignored_amf_messages: 0,
        }));
        let task = tokio::spawn(run_connection(
            socket,
            self.tls.clone(),
            self.authorizer.clone(),
            self.limits.clone(),
            sender,
            report.clone(),
            Admission {
                slot: slot.clone(),
                pending,
                sessions: self.slots.clone(),
                start_deadline,
                consumer_stopped,
            },
        ));
        Ok(RunningConnection {
            receiver,
            task: Some(task),
            report,
            stop_consumer,
            _slot: slot,
        })
    }
}

pub struct RunningConnection {
    receiver: mpsc::Receiver<MediaEvent>,
    task: Option<JoinHandle<SessionReport>>,
    report: Arc<Mutex<SessionReport>>,
    stop_consumer: watch::Sender<bool>,
    _slot: Arc<SessionSlot>,
}
impl RunningConnection {
    pub async fn finish_consumer(self) -> SessionReport {
        self.stop_consumer.send_replace(true);
        self.finish().await
    }
    pub async fn next(&mut self) -> Option<MediaEvent> {
        self.receiver.recv().await
    }
    pub async fn finish(mut self) -> SessionReport {
        // Verbraucher darf finish auch ohne vollständiges Leeren aufrufen.
        // Das Ende seiner Abnahme beendet dann den Producer sichtbar.
        self.receiver.close();
        // Keep ownership through the await so cancelling this future still
        // lets Drop abort the producer and release its connection slot.
        let result = self.task.as_mut().expect("owned task").await;
        self.task.take();
        match result {
            Ok(report) => report,
            Err(_) => {
                let mut report = self
                    .report
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                report.reason = EndReason::TaskFailed;
                report
            }
        }
    }
}
impl Drop for RunningConnection {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Clone, Copy)]
struct TrackState {
    codec: WireCodec,
    revision: u64,
    last_dts: Option<u32>,
    ended: bool,
}
struct Handler<A: Authorizer> {
    authorizer: Arc<A>,
    retention: Option<Arc<dyn Any + Send + Sync>>,
    session: Option<(u32, AuthorizedSession)>,
    completion_session: Arc<Mutex<Option<AuthorizedSession>>>,
    generation: ConnectionGeneration,
    tracks: HashMap<WireTrack, TrackState>,
    limits: IngestLimits,
    sender: mpsc::Sender<MediaEvent>,
    budget: Arc<Semaphore>,
    status: watch::Sender<Activity>,
    report: Arc<Mutex<SessionReport>>,
    event_budget: Arc<Semaphore>,
    slot: Arc<SessionSlot>,
    pending: Option<OwnedSemaphorePermit>,
    session_slots: Arc<Semaphore>,
}
// Lebt außerhalb des RTMP-Handlers, damit der endgültige Transport-Endgrund
// bereits feststeht, wenn die autorisierte Reservierung freigegeben wird.
struct AuthorizationCompletion<A: Authorizer> {
    authorizer: Arc<A>,
    session: Arc<Mutex<Option<AuthorizedSession>>>,
    report: Arc<Mutex<SessionReport>>,
    finished: bool,
    started: Instant,
}
impl<A: Authorizer> Drop for AuthorizationCompletion<A> {
    fn drop(&mut self) {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(session) = session {
            let mut report = self
                .report
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if !self.finished && report.reason == EndReason::ProtocolRejected {
                report.reason = EndReason::TaskFailed;
            }
            report.duration = self.started.elapsed();
            self.authorizer.completed(session, &report);
            self.authorizer.release(session);
        }
    }
}
#[derive(Clone, Copy)]
struct Activity {
    published: bool,
    last_media: Instant,
}
impl<A: Authorizer> Handler<A> {
    fn reject(&mut self, reason: EndReason) -> ServerSessionError {
        self.report.lock().unwrap_or_else(|e| e.into_inner()).reason = reason;
        ServerSessionError::HandlerRejected
    }
    fn media(
        &mut self,
        stream_id: u32,
        kind: MediaKind,
        timestamp: u32,
        data: Bytes,
    ) -> Result<(), ServerSessionError> {
        let Some((authorized_stream, session)) = self.session else {
            return Err(self.reject(EndReason::AuthorizationRejected));
        };
        if authorized_stream != stream_id {
            return Err(self.reject(EndReason::AuthorizationRejected));
        }
        if data.len() > self.limits.max_event_bytes {
            return Err(self.reject(EndReason::Backpressure));
        }
        let parsed = inspect(kind, &data, timestamp, self.limits.max_header_bytes)
            .map_err(|error| self.reject(EndReason::MediaRejected(error)))?;
        let mut track = self.tracks.get(&parsed.track).copied();
        if track.is_none()
            && matches!(
                parsed.event_kind,
                EventKind::SequenceHeader | EventKind::Metadata
            )
            && self.tracks.len() >= self.limits.max_tracks
        {
            return Err(self.reject(EndReason::MediaRejected(MediaError::TrackLimit)));
        }
        if parsed.event_kind == EventKind::SequenceHeader {
            let revision = track
                .map_or(0, |track| track.revision)
                .checked_add(1)
                .ok_or_else(|| {
                    self.reject(EndReason::MediaRejected(MediaError::RevisionExhausted))
                })?;
            track = Some(TrackState {
                codec: parsed.codec,
                revision,
                last_dts: None,
                ended: false,
            });
        } else {
            if parsed.event_kind == EventKind::Metadata && track.is_none() {
                // Metadata already addresses a track and consumes its budget.
                // Revision zero explicitly means no SequenceHeader received.
                track = Some(TrackState {
                    codec: parsed.codec,
                    revision: 0,
                    last_dts: None,
                    ended: false,
                });
            }
            let Some(state) = track.as_mut() else {
                return Err(self.reject(EndReason::MediaRejected(MediaError::FrameBeforeHeader)));
            };
            if state.ended {
                return Err(self.reject(EndReason::MediaRejected(MediaError::FrameBeforeHeader)));
            }
            if state.codec != parsed.codec {
                return Err(self.reject(EndReason::MediaRejected(
                    MediaError::CodecChangedWithoutHeader,
                )));
            }
            if parsed.event_kind != EventKind::Metadata {
                if state.revision == 0 {
                    return Err(
                        self.reject(EndReason::MediaRejected(MediaError::FrameBeforeHeader))
                    );
                }
                if state.last_dts.is_some_and(|last| timestamp < last) {
                    return Err(
                        self.reject(EndReason::MediaRejected(MediaError::TimestampRegression))
                    );
                }
                state.last_dts = Some(timestamp);
                state.ended = parsed.event_kind == EventKind::SequenceEnd;
            }
        }
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(data.len() as u32)
            .map_err(|_| self.reject(EndReason::Backpressure))?;
        let event_permit = self
            .event_budget
            .clone()
            .try_acquire_owned()
            .map_err(|_| self.reject(EndReason::Backpressure))?;
        // Observe while this event's permit is still owned here. A consumer
        // may drop the published event before we acquire the report lock.
        let observed_budget = self.limits.max_queued_bytes - self.budget.available_permits();
        let size = data.len();
        let event = MediaEvent {
            identity: TrackIdentity {
                session,
                generation: self.generation,
                track: parsed.track,
            },
            event_kind: parsed.event_kind,
            codec: parsed.codec,
            dts_ms: timestamp,
            pts_ms: parsed.pts_ms,
            configuration_revision: track.map_or(0, |track| track.revision),
            body: data.as_ref().into(),
            payload: parsed.payload,
            _budget: permit,
            _event_budget: event_permit,
            _slot: self.slot.clone(),
            retention: self.retention.clone(),
        };
        self.sender.try_send(event).map_err(|error| {
            self.reject(match error {
                mpsc::error::TrySendError::Closed(_) => EndReason::ConsumerClosed,
                mpsc::error::TrySendError::Full(_) => EndReason::Backpressure,
            })
        })?;
        if let Some(track) = track {
            self.tracks.insert(parsed.track, track);
        }
        let mut report = self.report.lock().unwrap_or_else(|e| e.into_inner());
        report.received_events += 1;
        report.received_bytes += size as u64;
        report.track_count = self.tracks.len();
        report.max_queued_bytes = report.max_queued_bytes.max(observed_budget);
        // Metadaten adressieren eine Spur, belegen aber keinen abspielbaren
        // Medienfluss. Sie und ein Sequenzende verlängern die Frist nicht.
        if matches!(
            parsed.event_kind,
            EventKind::SequenceHeader | EventKind::Frame
        ) {
            self.status.send_replace(Activity {
                published: true,
                last_media: Instant::now(),
            });
        }
        Ok(())
    }
}
impl<A: Authorizer> SessionHandler for Handler<A> {
    async fn on_publish(
        &mut self,
        stream_id: u32,
        app: &str,
        stream: &str,
    ) -> Result<(), ServerSessionError> {
        if self.session.is_some() {
            return Err(self.reject(EndReason::AuthorizationRejected));
        }
        let session = self
            .authorizer
            .authorize(app, stream)
            .await
            .map_err(|()| self.reject(EndReason::AuthorizationRejected))?;
        self.session = Some((stream_id, session));
        *self
            .completion_session
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(session);
        self.slot
            .authorize(&self.session_slots)
            .map_err(|()| self.reject(EndReason::SessionCapacity))?;
        // Pending-Kapazität endet nach Auth, nicht erst beim Streamende.
        self.pending.take();
        self.retention = self.authorizer.retention(session);
        self.status.send_replace(Activity {
            published: true,
            last_media: Instant::now(),
        });
        Ok(())
    }
    async fn on_unpublish(&mut self, stream_id: u32) -> Result<(), ServerSessionError> {
        if self.session.is_none_or(|(id, _)| id != stream_id) {
            return Err(self.reject(EndReason::AuthorizationRejected));
        }
        // Explizites Ende ist keine Freigabe für einen zweiten Publish derselben Verbindung.
        Err(self.reject(EndReason::ExplicitStop))
    }
    async fn on_data(
        &mut self,
        stream_id: u32,
        data: SessionData,
    ) -> Result<(), ServerSessionError> {
        match data {
            SessionData::Audio { timestamp, data } => {
                self.media(stream_id, MediaKind::Audio, timestamp, data)
            }
            SessionData::Video { timestamp, data } => {
                self.media(stream_id, MediaKind::Video, timestamp, data)
            }
            SessionData::Amf0 { .. } => {
                self.report
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .ignored_amf_messages += 1;
                Ok(())
            }
        }
    }
    async fn on_unknown_message(
        &mut self,
        _: u32,
        _: scuffle_rtmp::messages::UnknownMessage,
    ) -> Result<(), ServerSessionError> {
        Ok(())
    }
    async fn on_unknown_command(
        &mut self,
        _: u32,
        _: scuffle_rtmp::command_messages::UnknownCommand<'_>,
    ) -> Result<(), ServerSessionError> {
        Ok(())
    }
}

struct Admission {
    slot: Arc<SessionSlot>,
    pending: OwnedSemaphorePermit,
    sessions: Arc<Semaphore>,
    start_deadline: Instant,
    consumer_stopped: watch::Receiver<bool>,
}

async fn run_connection<A: Authorizer>(
    socket: TcpStream,
    tls: Arc<ServerConfig>,
    authorizer: Arc<A>,
    limits: IngestLimits,
    sender: mpsc::Sender<MediaEvent>,
    report: Arc<Mutex<SessionReport>>,
    admission: Admission,
) -> SessionReport {
    let mut completion = AuthorizationCompletion {
        authorizer: authorizer.clone(),
        session: Arc::new(Mutex::new(None)),
        report: report.clone(),
        finished: false,
        started: admission.start_deadline - limits.start_timeout,
    };
    let mut consumer_stopped = admission.consumer_stopped;
    let deadline = admission.start_deadline;
    let reason = match timeout_at(deadline, TlsAcceptor::from(tls).accept(socket)).await {
        Err(_) => EndReason::StartTimeout,
        Ok(Err(_)) => EndReason::TlsRejected,
        Ok(Ok(socket)) => {
            let (status, mut activity) = watch::channel(Activity {
                published: false,
                last_media: Instant::now(),
            });
            let generation = report.lock().unwrap_or_else(|e| e.into_inner()).generation;
            let handler = Handler {
                authorizer,
                retention: None,
                session: None,
                completion_session: completion.session.clone(),
                generation,
                tracks: HashMap::new(),
                budget: Arc::new(Semaphore::new(limits.max_queued_bytes)),
                sender,
                status,
                event_budget: Arc::new(Semaphore::new(limits.max_queued_events)),
                slot: admission.slot,
                pending: Some(admission.pending),
                session_slots: admission.sessions,
                report: report.clone(),
                limits: limits.clone(),
            };
            match ServerSession::new(socket, handler).with_limits(limits.rtmp) {
                Err(_) => EndReason::ProtocolRejected,
                Ok(session) => {
                    let future = session.run();
                    tokio::pin!(future);
                    loop {
                        let current = *activity.borrow_and_update();
                        let expiry = if current.published {
                            current.last_media + limits.media_idle_timeout
                        } else {
                            deadline
                        };
                        tokio::select! {
                            biased;
                            result = &mut future => {
                                let saved = report.lock().unwrap_or_else(|e| e.into_inner()).reason;
                                break match result {
                                    Ok(_) => EndReason::PeerClosed,
                                    Err(_) if saved != EndReason::ProtocolRejected => saved,
                                    Err(scuffle_rtmp::error::RtmpError::Session(ServerSessionError::Timeout(_))) => EndReason::ProtocolTimeout,
                                    Err(error) if error.is_client_closed() => EndReason::PeerClosed,
                                    Err(_) => EndReason::ProtocolRejected,
                                };
                            }
                            _ = consumer_stopped.changed() => break EndReason::ConsumerClosed,
                            _ = sleep_until(expiry) => { break if current.published { EndReason::MediaTimeout } else { EndReason::StartTimeout }; }
                            changed = activity.changed() => { if changed.is_err() { continue; } }
                        }
                    }
                }
            }
        }
    };
    let mut final_report = report.lock().unwrap_or_else(|e| e.into_inner()).clone();
    final_report.reason = reason;
    final_report.duration = completion.started.elapsed();
    *report.lock().unwrap_or_else(|error| error.into_inner()) = final_report.clone();
    completion.finished = true;
    final_report
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnusedAuthorizer;
    impl Authorizer for UnusedAuthorizer {
        async fn authorize(&self, _: &str, _: &str) -> Result<AuthorizedSession, ()> {
            Err(())
        }
    }

    #[test]
    fn immediate_consumer_drop_cannot_erase_observed_event_budget() {
        let limits = IngestLimits::local_probe();
        let generation = ConnectionGeneration {
            instance: [0; 16],
            counter: 1,
        };
        let report = Arc::new(Mutex::new(SessionReport {
            reason: EndReason::ProtocolRejected,
            duration: Duration::ZERO,
            generation,
            received_events: 0,
            received_bytes: 0,
            max_queued_bytes: 0,
            track_count: 0,
            ignored_amf_messages: 0,
        }));
        let budget = Arc::new(Semaphore::new(limits.max_queued_bytes));
        let (sender, mut receiver) = mpsc::channel(1);
        let (status, _) = watch::channel(Activity {
            published: true,
            last_media: Instant::now(),
        });
        let mut handler = Handler {
            authorizer: Arc::new(UnusedAuthorizer),
            retention: None,
            session: Some((1, AuthorizedSession::new(1, 1).unwrap())),
            completion_session: Arc::new(Mutex::new(None)),
            generation,
            tracks: HashMap::new(),
            limits: limits.clone(),
            sender,
            budget: budget.clone(),
            status,
            report: report.clone(),
            event_budget: Arc::new(Semaphore::new(limits.max_queued_events)),
            slot: Arc::new(SessionSlot::empty()),
            pending: None,
            session_slots: Arc::new(Semaphore::new(1)),
        };
        // Deterministic interleaving: the consumer frees the event while the
        // producer is waiting to update the report. No timing/race lottery.
        let guard = report.lock().unwrap();
        let producer = std::thread::spawn(move || {
            handler.media(
                1,
                MediaKind::Audio,
                0,
                Bytes::from_static(&[0xaf, 0, 0x11, 0x90]),
            )
        });
        drop(receiver.blocking_recv().unwrap());
        assert_eq!(budget.available_permits(), limits.max_queued_bytes);
        drop(guard);
        producer.join().unwrap().unwrap();
        assert_eq!(report.lock().unwrap().max_queued_bytes, 4);
    }
}
