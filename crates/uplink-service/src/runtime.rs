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
    reservations: Mutex<HashMap<AuthorizedSession, ActiveReservation>>,
    recorder: SessionRecorder,
}
struct ActiveReservation {
    reservation: Arc<Reservation>,
    ingest_report: Arc<Mutex<Option<uplink_ingest::SessionReport>>>,
}

fn ingest_completion_diagnostic(
    reason: uplink_ingest::EndReason,
    timestamp_clamps: u64,
    timestamp_rejection: Option<uplink_ingest::TimestampDiagnostic>,
    timestamp_tail: &std::collections::VecDeque<uplink_ingest::TimestampDiagnostic>,
    media_gaps: &std::collections::VecDeque<uplink_ingest::MediaGapDiagnostic>,
    media_gap_count: u64,
    max_media_gap_ms: u128,
) -> String {
    let ending = match reason {
        uplink_ingest::EndReason::ExplicitStop => "Streamer hat ausdrücklich beendet",
        uplink_ingest::EndReason::PeerClosed => "Transport unerwartet getrennt",
        uplink_ingest::EndReason::MediaTimeout => "Medienfluss abgerissen",
        _ => "Eingang beendet",
    };
    let media_gap_tail: Vec<_> = media_gaps.iter().rev().take(8).copied().collect();
    format!(
        " Abschluss={ending}; timestamp_clamps={timestamp_clamps} timestamp_rejection={timestamp_rejection:?} timestamp_tail={timestamp_tail:?} media_gap_count={} max_media_gap_ms={max_media_gap_ms} media_gap_tail={media_gap_tail:?}",
        media_gap_count
    )
}

#[derive(Clone)]
struct SessionRecorder {
    tasks: Arc<Mutex<JoinSet<()>>>,
    store: Arc<crate::store::Store>,
    slots: Arc<tokio::sync::Semaphore>,
    capacity: u32,
    failed: Arc<AtomicBool>,
}
impl SessionRecorder {
    fn new(state: &ServiceState) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(JoinSet::new())),
            store: state.store.clone(),
            slots: Arc::new(tokio::sync::Semaphore::new(state.config.max_sessions)),
            capacity: state.config.max_sessions as u32,
            failed: Arc::new(AtomicBool::new(false)),
        }
    }
    fn joined(&self, result: Result<(), tokio::task::JoinError>) {
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
            eprintln!("Uplink-Abschlussaufgabe wurde vor ihrem bestätigten Ende abgebrochen.");
        }
    }
    fn record(&self, record: SessionCompletion, permit: tokio::sync::OwnedSemaphorePermit) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        while let Some(result) = tasks.try_join_next() {
            self.joined(result);
        }
        let store = self.store.clone();
        let worker_failed = self.failed.clone();
        tasks.spawn(async move {
                let _permit = permit;
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
        });
    }
    async fn drained(&self) -> Result<(), &'static str> {
        let deadline = tokio::time::Instant::now()
            + crate::store::QUERY_LIMIT * 2
            + crate::store::CLEANUP_GRACE * 2
            + std::time::Duration::from_secs(1);
        let _idle = tokio::time::timeout_at(
            deadline,
            self.slots.clone().acquire_many_owned(self.capacity),
        )
        .await
        .map_err(|_| "Streamabschlüsse konnten nicht rechtzeitig gespeichert werden.")?
        .map_err(|_| "Streamabschluss ist nicht verfügbar.")?;
        let mut tasks =
            std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|error| error.into_inner()));
        tokio::time::timeout_at(deadline, async {
            while let Some(result) = tasks.join_next().await {
                self.joined(result);
            }
        })
        .await
        .map_err(|_| "Streamabschlussaufgaben konnten nicht rechtzeitig beendet werden.")?;
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
        self.reservations
            .lock()
            .ok()?
            .get(&session)
            .map(|active| active.reservation.clone())
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
        let recorder = self.recorder.clone();
        let ingest_report = Arc::new(Mutex::new(None));
        let completion_report = ingest_report.clone();
        reservation.on_completion(move |record| {
            let diagnostic = completion_report
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .map(|report: &uplink_ingest::SessionReport| {
                    ingest_completion_diagnostic(
                        report.reason,
                        report.timestamp_clamps,
                        report.timestamp_rejection,
                        &report.timestamp_tail,
                        &report.media_gaps,
                        report.media_gap_count,
                        report.max_media_gap_ms,
                    )
                })
                .unwrap_or_default();
            eprintln!("Uplink-Eingang beendet: streamer_id={} duration_ms={} source_tracks={} EndReason={}{}",
                record.streamer_id, record.duration.as_millis(), record.source_tracks, record.end_reason, diagnostic);
            recorder.record(record, permit);
        });
        let reservation = Arc::new(reservation);
        let session = AuthorizedSession::new(tenant, reservation.id()).map_err(|_| ())?;
        self.reservations.lock().map_err(|_| ())?.insert(
            session,
            ActiveReservation {
                reservation,
                ingest_report,
            },
        );
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
        if let Some(active) = self
            .reservations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&session)
        {
            *active
                .ingest_report
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(report.clone());
            active.reservation.ingest_report(report);
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
        source_termination: tokio::sync::watch::Receiver<uplink_media::SourceTermination>,
    ) -> impl Future<Output = Result<(), &'static str>> + Send;
}

fn source_termination(reason: uplink_ingest::EndReason) -> uplink_media::SourceTermination {
    match reason {
        uplink_ingest::EndReason::PeerClosed
        | uplink_ingest::EndReason::MediaTimeout
        | uplink_ingest::EndReason::ProtocolTimeout
        | uplink_ingest::EndReason::TaskFailed => uplink_media::SourceTermination::Interrupted,
        _ => uplink_media::SourceTermination::Graceful,
    }
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
    let media_limits = state.config.media_limits();
    let coordinator_grace = media_limits.startup_timeout * 3
        + media_limits.write_timeout
        + media_limits.shutdown_timeout * (media_limits.max_encode_groups as u32 + 6)
        + crate::store::QUERY_LIMIT
        + crate::store::CLEANUP_GRACE
        + std::time::Duration::from_secs(1);
    let tasks_grace = coordinator_grace + std::time::Duration::from_secs(1);
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
                    let (termination_tx, termination_rx) =
                        tokio::sync::watch::channel(uplink_media::SourceTermination::Graceful);
                    let processing = processor.process(first, receiver, termination_rx);
                    tokio::pin!(processing);
                    let mut connection = Some(connection);
                    loop {
                        tokio::select! {
                            _=media_stopped.changed()=>{
                                if let Some(connection) = connection.as_ref() {
                                    connection.stop_consumer();
                                }
                                drop(sender);
                                match tokio::time::timeout(coordinator_grace,processing).await {
                                    Ok(Err(error)) => reservation.fail(error),
                                    Err(_) => reservation.fail("Medienausgabe konnte beim Dienststopp nicht rechtzeitig schließen."),
                                    Ok(Ok(())) => {}
                                }
                                break;
                            }
                            result = &mut processing => { if let Err(error)=result { reservation.fail(error); } break; }
                            event = connection.as_mut().expect("aktive Ingestverbindung").next() => {
                                let Some(event) = event else {
                                    let report = connection
                                        .take()
                                        .expect("beendete Ingestverbindung")
                                        .finish()
                                        .await;
                                    termination_tx.send_replace(source_termination(report.reason));
                                    drop(sender);
                                    match tokio::time::timeout(coordinator_grace,processing).await {
                                        Ok(Err(error)) => reservation.fail(error),
                                        Err(_) => reservation.fail("Medienausgabe konnte nach dem Eingangsende nicht rechtzeitig schließen."),
                                        Ok(Ok(())) => {}
                                    }
                                    reservation.ended();
                                    return;
                                };
                                reservation.record(event.wire_body().len());
                                if let Err(error) = sender.try_send(event) {
                                    if matches!(error, mpsc::error::TrySendError::Full(_)) {
                                        reservation.input_backpressure();
                                    }
                                    let fallback = match error {
                                        mpsc::error::TrySendError::Closed(_) => "Medienverarbeitung hat den Eingang geschlossen.",
                                        mpsc::error::TrySendError::Full(_) => "Medienverarbeitung hat ihr Eingangsbudget ausgeschöpft.",
                                    };
                                    if let Some(connection) = connection.as_ref() {
                                        connection.stop_consumer();
                                    }
                                    drop(sender);
                                    match tokio::time::timeout(coordinator_grace,processing).await {
                                        Ok(Err(error)) => reservation.fail(error),
                                        _ => reservation.fail(fallback),
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    if let Some(connection) = connection {
                        let _ = connection.finish_consumer().await;
                    }
                    reservation.ended();
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    let _ = stop_media.send(true);
    let drained = tokio::time::timeout(tasks_grace, async {
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

#[cfg(test)]
mod timestamp_diagnostic_tests {
    use super::*;
    use uplink_ingest::{
        EndReason, EventKind, MediaError, MediaKind, TimestampDiagnostic, WireTrack,
    };

    #[test]
    fn only_an_explicit_obs_stop_is_a_graceful_platform_end() {
        for reason in [
            EndReason::PeerClosed,
            EndReason::MediaTimeout,
            EndReason::ProtocolTimeout,
            EndReason::TaskFailed,
        ] {
            assert_eq!(
                source_termination(reason),
                uplink_media::SourceTermination::Interrupted,
                "{reason:?} darf der Plattform kein absichtliches Streamende signalisieren"
            );
        }
        for reason in [
            EndReason::ExplicitStop,
            EndReason::MediaRejected(MediaError::TimestampRegression),
            EndReason::Backpressure,
            EndReason::ConsumerClosed,
            EndReason::ProtocolRejected,
        ] {
            assert_eq!(
                source_termination(reason),
                uplink_media::SourceTermination::Graceful,
                "{reason:?} ist kein reconnect-fähiger Transport-Hickup"
            );
        }
    }

    #[test]
    fn completion_line_keeps_full_numeric_packet_diagnostic_and_distinguishes_stops() {
        let diagnostic = TimestampDiagnostic {
            track: WireTrack {
                kind: MediaKind::Video,
                wire_id: 7,
            },
            event_kind: EventKind::Frame,
            last_dts_ms: Some(10000),
            new_dts_ms: 5000,
            delta_ms: 5000,
            session_duration_ms: 29548,
            path: "server.rs",
        };
        let tail = [diagnostic].into();
        let gaps = std::collections::VecDeque::new();
        let line = ingest_completion_diagnostic(
            EndReason::MediaRejected(MediaError::TimestampRegression),
            50,
            Some(diagnostic),
            &tail,
            &gaps,
            0,
            0,
        );
        for expected in [
            "kind: Video",
            "wire_id: 7",
            "last_dts_ms: Some(10000)",
            "new_dts_ms: 5000",
            "delta_ms: 5000",
            "session_duration_ms: 29548",
            "server.rs",
            "timestamp_clamps=50",
            "timestamp_tail=",
            "timestamp_rejection=Some",
        ] {
            assert!(
                line.contains(expected),
                "fehlendes Abschlussfeld {expected}: {line}"
            );
        }
        assert!(!line.contains("Streamer hat ausdrücklich beendet"));
        assert!(
            ingest_completion_diagnostic(EndReason::ExplicitStop, 0, None, &tail, &gaps, 0, 0,)
                .contains("Streamer hat ausdrücklich beendet")
        );
        assert!(
            ingest_completion_diagnostic(EndReason::PeerClosed, 0, None, &tail, &gaps, 0, 0,)
                .contains("Transport unerwartet getrennt")
        );
    }
}
