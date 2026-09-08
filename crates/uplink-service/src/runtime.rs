use crate::{
    api::{ServiceState, router},
    registry::Reservation,
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinSet};
use uplink_ingest::{AuthorizedSession, Authorizer, IngestLimits, IngestServer, MediaEvent};

pub struct ServiceAuthorizer {
    state: Arc<ServiceState>,
    reservations: Mutex<HashMap<AuthorizedSession, Arc<Reservation>>>,
}
impl ServiceAuthorizer {
    pub fn new(state: Arc<ServiceState>) -> Self {
        Self {
            state,
            reservations: Mutex::new(HashMap::new()),
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
        let tenant = self
            .state
            .store
            .authenticate_ingest(stream)
            .await
            .map_err(|_| ())?;
        if !self.state.config.permits_tenant(tenant) {
            return Err(());
        }
        let reservation = Arc::new(self.state.registry.reserve(tenant).map_err(|_| ())?);
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
            reservation.generation(report.generation);
            reservation.ingest_ended(&report.reason);
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
    let mut limits = IngestLimits::local_probe();
    limits.max_connections = state.config.max_sessions;
    limits.max_event_bytes = state.config.media.max_event_bytes;
    limits.max_header_bytes = (64 * 1024).min(limits.max_event_bytes);
    limits.max_queued_bytes = state.config.media.max_queued_bytes;
    limits.max_queued_events = state.config.media.max_queued_events;
    limits.max_tracks = state.config.media.max_tracks;
    limits.rtmp.chunk.max_message_bytes = limits.max_event_bytes;
    limits.rtmp.chunk.max_partial_bytes = limits.max_event_bytes.saturating_mul(4);
    limits.rtmp.max_read_buffer_bytes = limits.max_event_bytes.saturating_mul(2);
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
                    let Some(first) = first else { let _=connection.finish().await; return; };
                    let Some(reservation) = first.authorization_retention().and_then(|value| value.downcast::<Reservation>().ok()) else { return; };
                    reservation.generation(first.identity.generation);
                    reservation.record(first.wire_body().len());
                    let (sender, receiver) = mpsc::channel(256);
                    let processing = processor.process(first, receiver);
                    tokio::pin!(processing);
                    loop {
                        tokio::select! {
                            _=media_stopped.changed()=>{drop(sender);if !matches!(tokio::time::timeout(std::time::Duration::from_secs(8),processing).await,Ok(Ok(()))){reservation.fail("Medienausgabe konnte beim Dienststopp nicht rechtzeitig schließen.");}break;}
                            result = &mut processing => { if let Err(error)=result { reservation.fail(error); } break; }
                            event = connection.next() => {
                                let Some(event) = event else { drop(sender); if let Err(error)=processing.await{reservation.fail(error);} break; };
                                reservation.record(event.wire_body().len());
                                if sender.try_send(event).is_err() { reservation.fail("Medienverarbeitung hat ihr Eingangsbudget ausgeschöpft."); break; }
                            }
                        }
                    }
                    let _ = connection.finish().await;
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
    if let Some(hub) = &chat {
        hub.shutdown().await;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(5), &mut http).await {
        Ok(Ok(Ok(()))) if drained => Ok(()),
        _ => {
            http.abort();
            let _ = http.await;
            Err("Steuerung konnte nicht sauber beendet werden.")
        }
    }
}
