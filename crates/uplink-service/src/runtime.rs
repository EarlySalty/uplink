use crate::{
    api::{ServiceState, router},
    registry::{Reservation, SessionCompletion},
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinSet};
use uplink_ingest::{AuthorizedSession, Authorizer, IngestServer, MediaEvent};

pub struct ServiceAuthorizer {
    state: Arc<ServiceState>,
    reservations: Mutex<HashMap<AuthorizedSession, Arc<Reservation>>>,
    recorder: SessionRecorder,
}
struct SessionRecorder {
    sender: mpsc::Sender<(SessionCompletion, tokio::sync::OwnedSemaphorePermit)>,
    slots: Arc<tokio::sync::Semaphore>,
    capacity: u32,
    failed: Arc<AtomicBool>,
}
impl SessionRecorder {
    fn new(state: &ServiceState) -> Self {
        let (sender, mut receiver) = mpsc::channel::<(
            SessionCompletion,
            tokio::sync::OwnedSemaphorePermit,
        )>(state.config.max_sessions);
        let store = state.store.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        tokio::spawn(async move {
            while let Some((record, _permit)) = receiver.recv().await {
                let streamer_id = i64::try_from(record.streamer_id);
                let started_at = record.ended_at.checked_sub(record.duration);
                let result = match (streamer_id, started_at) {
                    (Ok(streamer_id), Some(started_at)) => store.query_completion(
                        "INSERT INTO relay.sessions(streamer_id,started_at,ended_at,ingest_protocol,profile_json,end_reason) VALUES($1,$2,$3,'rtmps',$4,$5)",
                        &[&streamer_id, &started_at, &record.ended_at, &record.profile, &record.end_reason],
                    ).await.map(|_| ()),
                    _ => Err("Zeit oder Streamer-ID des Abschlusses ist ungültig."),
                };
                if let Err(error) = result {
                    worker_failed.store(true, Ordering::Release);
                    eprintln!(
                        "Uplink-Abschluss konnte nicht gespeichert werden: streamer_id={} error={error}",
                        record.streamer_id
                    );
                }
            }
        });
        Self {
            sender,
            slots: Arc::new(tokio::sync::Semaphore::new(state.config.max_sessions)),
            capacity: state.config.max_sessions as u32,
            failed,
        }
    }
    async fn drained(&self) -> Result<(), &'static str> {
        let _idle = tokio::time::timeout(
            crate::store::CLEANUP_GRACE * 2 + std::time::Duration::from_secs(20),
            self.slots.clone().acquire_many_owned(self.capacity),
        )
        .await
        .map_err(|_| "Streamabschlüsse konnten nicht rechtzeitig gespeichert werden.")?
        .map_err(|_| "Streamabschluss ist nicht verfügbar.")?;
        if self.failed.load(Ordering::Acquire) {
            Err("Mindestens ein Streamabschluss konnte nicht gespeichert werden.")
        } else {
            Ok(())
        }
    }
}
impl ServiceAuthorizer {
    pub fn new(state: Arc<ServiceState>) -> Self {
        let recorder = SessionRecorder::new(&state);
        Self {
            state,
            reservations: Mutex::new(HashMap::new()),
            recorder,
        }
    }
    pub fn reservation(&self, session: AuthorizedSession) -> Option<Arc<Reservation>> {
        self.reservations.lock().ok()?.get(&session).cloned()
    }
}
impl Authorizer for ServiceAuthorizer {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        if app != "live" {
            return Err(());
        }
        let permit = self
            .recorder
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ())?;
        let tenant = self
            .state
            .store
            .authenticate_ingest(stream)
            .await
            .map_err(|_| ())?;
        if !self.state.config.permits_tenant(tenant) {
            return Err(());
        }
        let mut reservation = self.state.registry.reserve(tenant).map_err(|_| ())?;
        let sender = self.recorder.sender.clone();
        let failed = self.recorder.failed.clone();
        reservation.on_completion(move |record| {
            eprintln!("Uplink-Eingang beendet: streamer_id={} duration_ms={} source_tracks={} EndReason={}",
                record.streamer_id, record.duration.as_millis(), record.source_tracks, record.end_reason);
            if let Err(error) = sender.try_send((record, permit)) {
                failed.store(true, Ordering::Release);
                eprintln!("Uplink-Abschluss konnte nicht gespeichert werden: streamer_id={} error=Speicherung ist nicht verfügbar.", error.into_inner().0.streamer_id);
            }
        });
        let reservation = Arc::new(reservation);
        let session = AuthorizedSession::new(tenant, reservation.id()).map_err(|_| ())?;
        self.reservations
            .lock()
            .map_err(|_| ())?
            .insert(session, reservation);
        if let Some(hub) = &self.state.chat {
            let _ = hub.set_active(tenant, true);
        }
        Ok(session)
    }
    fn release(&self, session: AuthorizedSession) {
        if let Ok(mut active) = self.reservations.lock() {
            active.remove(&session);
        }
    }
    fn completed(&self, session: AuthorizedSession, report: &uplink_ingest::SessionReport) {
        if let Some(reservation) = self.reservation(session) {
            reservation.ingest_report(report);
        }
        if let Some(hub) = &self.state.chat {
            let _ = hub.set_active(session.tenant_id(), false);
        }
    }
    fn retention(
        &self,
        session: AuthorizedSession,
    ) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.reservation(session)
            .map(|value| value as Arc<dyn std::any::Any + Send + Sync>)
    }
}

/// Der Medienkoordinator übernimmt ausschließlich Events derselben autorisierten
/// Verbindung. Sein Receiver trägt weiterhin die begrenzten Ingest-Permits.
pub trait SessionProcessor: Send + Sync + 'static {
    fn process(
        &self,
        first: MediaEvent,
        events: mpsc::Receiver<MediaEvent>,
    ) -> impl Future<Output = Result<(), &'static str>> + Send;
}

pub async fn serve<P: SessionProcessor>(
    state: Arc<ServiceState>,
    tls: Arc<uplink_ingest::rustls::ServerConfig>,
    processor: Arc<P>,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), &'static str> {
    serve_with_ready(state, tls, processor, shutdown, None).await
}
pub async fn serve_with_ready<P: SessionProcessor>(
    state: Arc<ServiceState>,
    tls: Arc<uplink_ingest::rustls::ServerConfig>,
    processor: Arc<P>,
    shutdown: impl Future<Output = ()> + Send,
    ready: Option<tokio::sync::oneshot::Sender<(std::net::SocketAddr, std::net::SocketAddr)>>,
) -> Result<(), &'static str> {
    let authorizer = Arc::new(ServiceAuthorizer::new(state.clone()));
    let http_shutdown_grace = std::time::Duration::from_secs(state.config.request_timeout_seconds)
        + crate::store::CLEANUP_GRACE
        + std::time::Duration::from_secs(1);
    let limits = state.config.ingest_limits()?;
    let ingest = IngestServer::bind_tls(state.config.ingest_bind, tls, authorizer.clone(), limits)
        .await
        .map_err(|_| "RTMPS-Eingang konnte nicht starten.")?;
    let listener = TcpListener::bind(state.config.api_bind)
        .await
        .map_err(|_| "Steuerung konnte nicht starten.")?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    if let Some(ready) = ready {
        let _ = ready.send((
            listener
                .local_addr()
                .map_err(|_| "API-Adresse ist nicht verfügbar.")?,
            ingest
                .local_addr()
                .map_err(|_| "Eingangsadresse ist nicht verfügbar.")?,
        ));
    }
    let chat = state.chat.clone();
    let chat_store = state.store.clone();
    let app = router(state);
    let mut http = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let mut tasks = JoinSet::new();
    // Auch geöffnete Dashboards ohne Socket dürfen Chat nach einer Sperre
    // nicht unbegrenzt weiterbetreiben. Unbekannte DBergebnisse pausieren den
    // Zugang, behalten aber die zur laufenden Session gehörige Zuordnung.
    let chat_check = async {
        let Some(hub) = &chat else {
            std::future::pending::<()>().await;
            return;
        };
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            tick.tick().await;
            let checks = hub.identity_checks();
            if checks.is_empty() {
                continue;
            }
            let ids: Vec<i64> = checks
                .iter()
                .filter_map(|check| i64::try_from(check.streamer_id).ok())
                .collect();
            let allowed=chat_store.query("SELECT streamer_id FROM relay.users WHERE streamer_id=ANY($1) AND enabled=true", &[&ids]).await;
            let allowed = allowed.ok().and_then(|rows| {
                rows.iter()
                    .map(|row| row.try_get::<_, i64>(0))
                    .collect::<Result<std::collections::HashSet<_>, _>>()
                    .ok()
            });
            for check in checks {
                hub.identity_checked(
                    &check,
                    allowed
                        .as_ref()
                        .map(|ids| ids.contains(&(check.streamer_id as i64))),
                );
            }
        }
    };
    tokio::pin!(chat_check);
    let (stop_media, media_stopped) = tokio::sync::watch::channel(false);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = &mut chat_check => break,
            connection = ingest.accept() => {
                let Ok(mut connection) = connection else { continue; };
                let processor = processor.clone();
                let mut media_stopped=media_stopped.clone();
                // Der autorisierte Endgrund kommt unabhängig vom ersten Event
                // über completed. finish wartet nur noch den Producerabschluss;
                // die Medienverarbeitung beendet danach ihre eigene Reservierung.
                tasks.spawn(async move {
                    let first=tokio::select!{first=connection.next()=>first,_=media_stopped.changed()=>None};
                    let Some(first) = first else { let _=connection.finish_consumer().await; return; };
                    let Some(reservation) = first.authorization_retention().and_then(|value| value.downcast::<Reservation>().ok()) else { return; };
                    reservation.generation(first.identity.generation);
                    reservation.record(first.wire_body().len());
                    let (sender, receiver) = mpsc::channel(256);
                    let processing = processor.process(first, receiver);
                    tokio::pin!(processing);
                    loop {
                        tokio::select! {
                            _=media_stopped.changed()=>{
                                connection.stop_consumer();
                                drop(sender);
                                match tokio::time::timeout(std::time::Duration::from_secs(8),processing).await {
                                    Ok(Err(error)) => reservation.fail(error),
                                    Err(_) => reservation.fail("Medienausgabe konnte beim Dienststopp nicht rechtzeitig schließen."),
                                    Ok(Ok(())) => {}
                                }
                                break;
                            }
                            result = &mut processing => { if let Err(error)=result { reservation.fail(error); } break; }
                            event = connection.next() => {
                                let Some(event) = event else {
                                    drop(sender);
                                    match tokio::time::timeout(std::time::Duration::from_secs(8),processing).await {
                                        Ok(Err(error)) => reservation.fail(error),
                                        Err(_) => reservation.fail("Medienausgabe konnte nach dem Eingangsende nicht rechtzeitig schließen."),
                                        Ok(Ok(())) => {}
                                    }
                                    break;
                                };
                                reservation.record(event.wire_body().len());
                                if let Err(error) = sender.try_send(event) {
                                    let fallback = match error {
                                        mpsc::error::TrySendError::Closed(_) => "Medienverarbeitung hat den Eingang geschlossen.",
                                        mpsc::error::TrySendError::Full(_) => "Medienverarbeitung hat ihr Eingangsbudget ausgeschöpft.",
                                    };
                                    connection.stop_consumer();
                                    drop(sender);
                                    match tokio::time::timeout(std::time::Duration::from_secs(8),processing).await {
                                        Ok(Err(error)) => reservation.fail(error),
                                        _ => reservation.fail(fallback),
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    let _ = connection.finish_consumer().await;
                    reservation.ended();
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    let _ = stop_media.send(true);
    let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    if !drained {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    let _ = stop.send(());
    let recorded = authorizer.recorder.drained().await;
    if let Some(hub) = &chat {
        hub.shutdown().await;
    }
    match tokio::time::timeout(http_shutdown_grace, &mut http).await {
        Ok(Ok(Ok(()))) if drained => recorded,
        _ => {
            http.abort();
            let _ = http.await;
            Err("Steuerung konnte nicht sauber beendet werden.")
        }
    }
}
