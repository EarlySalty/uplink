#[path = "support/tls.rs"]
mod tls;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{TlsConnector, client::TlsStream};
use uplink_ingest::*;

struct Auth {
    allow: bool,
}

struct LeaseAuth {
    released: std::sync::atomic::AtomicUsize,
}

struct RetainedAuth {
    lease: std::sync::Mutex<Option<Arc<()>>>,
}
impl Authorizer for RetainedAuth {
    async fn authorize(&self, _: &str, _: &str) -> Result<AuthorizedSession, ()> {
        Ok(AuthorizedSession::new(11, 22).unwrap())
    }
    fn retention(&self, _: AuthorizedSession) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.lease
            .lock()
            .unwrap()
            .as_ref()
            .map(|value| value.clone() as Arc<dyn std::any::Any + Send + Sync>)
    }
    fn release(&self, _: AuthorizedSession) {
        self.lease.lock().unwrap().take();
    }
}

#[tokio::test]
async fn queued_media_keeps_tenant_reservation_after_short_publish_ends() {
    let certificates = tls::test_tls();
    let lease = Arc::new(());
    let weak = Arc::downgrade(&lease);
    let auth = Arc::new(RetainedAuth {
        lease: std::sync::Mutex::new(Some(lease)),
    });
    let server = IngestServer::bind_loopback(
        0,
        certificates.server,
        auth.clone(),
        IngestLimits::local_probe(),
    )
    .await
    .unwrap();
    let (mut incoming, mut peer) = connect(&server, certificates.client).await;
    publish(&mut peer).await;
    message(&mut peer, 8, 0, 1, &[0xaf, 0, 0x12, 0x10]).await;
    end(&mut peer).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        auth.lease.lock().unwrap().is_none(),
        "Producer muss bereits beendet sein"
    );
    assert!(
        weak.upgrade().is_some(),
        "Eventqueue muss die Reservierung halten"
    );
    let event = incoming.next().await.unwrap();
    let retained = event.authorization_retention().unwrap();
    drop(event);
    assert_eq!(incoming.finish().await.reason, EndReason::ExplicitStop);
    assert!(weak.upgrade().is_some());
    drop(retained);
    assert!(weak.upgrade().is_none());
}
impl Authorizer for LeaseAuth {
    async fn authorize(&self, _: &str, _: &str) -> Result<AuthorizedSession, ()> {
        AuthorizedSession::new(11, 22).map_err(|_| ())
    }
    fn release(&self, session: AuthorizedSession) {
        assert_eq!(session.tenant_id(), 11);
        assert_eq!(session.session_id(), 22);
        self.released
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn explicit_tls_admission_releases_authorization_on_finish_and_cancellation() {
    for cancel in [false, true] {
        let certificates = tls::test_tls();
        let auth = Arc::new(LeaseAuth {
            released: std::sync::atomic::AtomicUsize::new(0),
        });
        let server = IngestServer::bind_tls(
            "127.0.0.1:0".parse().unwrap(),
            certificates.server,
            auth.clone(),
            IngestLimits::local_probe(),
        )
        .await
        .unwrap();
        let (mut incoming, mut peer) = connect(&server, certificates.client).await;
        publish(&mut peer).await;
        message(&mut peer, 8, 0, 1, &[0xaf, 0, 0x12, 0x10]).await;
        drop(incoming.next().await.unwrap());
        if cancel {
            drop(incoming);
        } else {
            end(&mut peer).await;
            assert_eq!(incoming.finish().await.reason, EndReason::ExplicitStop);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(auth.released.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
impl Authorizer for Auth {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        if self.allow && app == "live" && stream == "probe" {
            Ok(AuthorizedSession::new(1, 1).unwrap())
        } else {
            Err(())
        }
    }
}

async fn server(
    allow: bool,
    limits: IngestLimits,
) -> (IngestServer<Auth>, Arc<rustls::ClientConfig>) {
    let tls = tls::test_tls();
    assert!(
        tls.certificate_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    (
        IngestServer::bind_loopback(0, tls.server, Arc::new(Auth { allow }), limits)
            .await
            .unwrap(),
        tls.client,
    )
}

async fn connect<A: Authorizer>(
    server: &IngestServer<A>,
    config: Arc<rustls::ClientConfig>,
) -> (RunningConnection, TlsStream<TcpStream>) {
    let address = server.local_addr().unwrap();
    let task = tokio::spawn(async move {
        TlsConnector::from(config)
            .connect(
                "localhost".try_into().unwrap(),
                TcpStream::connect(address).await.unwrap(),
            )
            .await
            .unwrap()
    });
    let connection = server.accept().await.unwrap();
    (connection, task.await.unwrap())
}

async fn handshake(client: &mut TlsStream<TcpStream>) {
    let mut c0c1 = vec![0; 1537];
    c0c1[0] = 3;
    client.write_all(&c0c1).await.unwrap();
    client.flush().await.unwrap();
    let mut reply = vec![0; 3073];
    timeout(Duration::from_secs(2), client.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply[0], 3);
    client.write_all(&reply[1..1537]).await.unwrap();
    client.flush().await.unwrap();
}

fn amf_string(value: &str) -> Vec<u8> {
    let mut data = vec![2];
    data.extend_from_slice(&(value.len() as u16).to_be_bytes());
    data.extend_from_slice(value.as_bytes());
    data
}
fn amf_number(value: f64) -> Vec<u8> {
    let mut data = vec![0];
    data.extend_from_slice(&value.to_be_bytes());
    data
}

async fn message(
    client: &mut TlsStream<TcpStream>,
    kind: u8,
    timestamp: u32,
    stream: u32,
    body: &[u8],
) {
    assert!(body.len() < 0xffffff && timestamp < 0xffffff);
    let mut wire = vec![3];
    wire.extend_from_slice(&timestamp.to_be_bytes()[1..]);
    wire.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    wire.push(kind);
    wire.extend_from_slice(&stream.to_le_bytes());
    for (index, chunk) in body.chunks(128).enumerate() {
        if index > 0 {
            wire.push(0xc3);
        }
        wire.extend_from_slice(chunk);
    }
    client.write_all(&wire).await.unwrap();
    client.flush().await.unwrap();
}

async fn publish(client: &mut TlsStream<TcpStream>) {
    handshake(client).await;
    let mut connect = amf_string("connect");
    connect.extend(amf_number(1.0));
    connect.push(3);
    connect.extend_from_slice(&[0, 3, b'a', b'p', b'p']);
    connect.extend(amf_string("live"));
    connect.extend_from_slice(&[0, 0, 9]);
    message(client, 20, 0, 0, &connect).await;
    let mut publish = amf_string("publish");
    publish.extend(amf_number(0.0));
    publish.push(5);
    publish.extend(amf_string("probe"));
    publish.extend(amf_string("live"));
    message(client, 20, 0, 1, &publish).await;
}

async fn end(client: &mut TlsStream<TcpStream>) {
    let mut stop = amf_string("deleteStream");
    stop.extend(amf_number(0.0));
    stop.push(5);
    stop.extend(amf_number(1.0));
    message(client, 20, 0, 1, &stop).await;
}

#[tokio::test]
async fn tls_rejects_wrong_hostname_and_untrusted_certificate() {
    for wrong_hostname in [true, false] {
        let (server, trusted) = server(true, IngestLimits::local_probe()).await;
        let address = server.local_addr().unwrap();
        let client_config = if wrong_hostname {
            trusted
        } else {
            tls::test_tls().client
        };
        let name = if wrong_hostname {
            "wrong.invalid"
        } else {
            "localhost"
        };
        let task = tokio::spawn(async move {
            TlsConnector::from(client_config)
                .connect(
                    name.try_into().unwrap(),
                    TcpStream::connect(address).await.unwrap(),
                )
                .await
                .is_err()
        });
        let connection = server.accept().await.unwrap();
        assert!(task.await.unwrap());
        assert_eq!(
            timeout(Duration::from_secs(2), connection.finish())
                .await
                .unwrap()
                .reason,
            EndReason::TlsRejected
        );
    }
}

#[tokio::test]
async fn explicit_stop_and_reconnect_keep_payloads_but_use_fresh_generations() {
    let (server, config) = server(true, IngestLimits::local_probe()).await;
    let mut generations = Vec::new();
    for _ in 0..2 {
        let (mut connection, mut client) = connect(&server, config.clone()).await;
        publish(&mut client).await;
        message(&mut client, 8, 0, 1, &[0xaf, 0, 0x11, 0x90]).await;
        message(&mut client, 8, 21, 1, &[0xaf, 1, 1, 2, 3]).await;
        let header = timeout(Duration::from_secs(2), connection.next())
            .await
            .unwrap()
            .unwrap();
        let frame = connection.next().await.unwrap();
        assert_eq!(header.event_kind, EventKind::SequenceHeader);
        assert_eq!(header.payload(), &[0x11, 0x90]);
        assert_eq!(frame.payload(), &[1, 2, 3]);
        assert_eq!((frame.dts_ms, frame.pts_ms), (21, 21));
        assert_eq!(header.identity.generation, frame.identity.generation);
        generations.push(frame.identity.generation);
        end(&mut client).await;
        assert!(connection.next().await.is_none());
        let report = connection.finish().await;
        assert_eq!(report.reason, EndReason::ExplicitStop);
        assert_eq!(report.track_count, 1);
    }
    assert_ne!(generations[0], generations[1]);
}

#[tokio::test]
async fn denied_publish_yields_no_media_and_no_echoed_access_data() {
    let (server, config) = server(false, IngestLimits::local_probe()).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    assert!(
        timeout(Duration::from_secs(2), connection.next())
            .await
            .unwrap()
            .is_none()
    );
    let report = connection.finish().await;
    assert_eq!(report.reason, EndReason::AuthorizationRejected);
    assert_eq!(report.received_events, 0);
}

#[tokio::test]
async fn metadata_tracks_consume_the_track_budget_before_any_sequence_header() {
    let mut limits = IngestLimits::local_probe();
    limits.max_tracks = 1;
    let (server, config) = server(true, limits).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    let mut metadata = [0x95, 4, b'm', b'p', b'4', b'a', 1, 1, 1, 0, 0, 0, 4];
    message(&mut client, 8, 0, 1, &metadata).await;
    let first = connection.next().await.unwrap();
    assert_eq!(first.configuration_revision, 0);
    metadata[6] = 2;
    message(&mut client, 8, 0, 1, &metadata).await;
    assert!(connection.next().await.is_none());
    let report = connection.finish().await;
    assert_eq!(
        report.reason,
        EndReason::MediaRejected(MediaError::TrackLimit)
    );
    assert_eq!(report.track_count, 1);
}

#[tokio::test]
async fn metadata_cannot_inherit_another_codecs_header_revision() {
    let (server, config) = server(true, IngestLimits::local_probe()).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    message(&mut client, 9, 0, 1, &[0x17, 0, 0, 0, 0, 1, 100]).await;
    assert_eq!(connection.next().await.unwrap().configuration_revision, 1);
    message(&mut client, 9, 0, 1, &[0x94, b'a', b'v', b'0', b'1', 0]).await;
    assert!(connection.next().await.is_none());
    assert_eq!(
        connection.finish().await.reason,
        EndReason::MediaRejected(MediaError::CodecChangedWithoutHeader)
    );
}

#[tokio::test]
async fn metadata_cannot_replace_a_header_or_restart_an_ended_sequence() {
    for ended in [false, true] {
        let (server, config) = server(true, IngestLimits::local_probe()).await;
        let (mut connection, mut client) = connect(&server, config).await;
        publish(&mut client).await;
        if ended {
            message(&mut client, 9, 0, 1, &[0x90, b'a', b'v', b'0', b'1', 1]).await;
            assert!(connection.next().await.is_some());
            message(&mut client, 9, 0, 1, &[0x92, b'a', b'v', b'0', b'1']).await;
            assert!(connection.next().await.is_some());
        }
        message(&mut client, 9, 0, 1, &[0x94, b'a', b'v', b'0', b'1', 0]).await;
        if !ended {
            assert_eq!(connection.next().await.unwrap().configuration_revision, 0);
            message(&mut client, 9, 0, 1, &[0x91, b'a', b'v', b'0', b'1', 1]).await;
        }
        assert!(connection.next().await.is_none());
        assert_eq!(
            connection.finish().await.reason,
            EndReason::MediaRejected(MediaError::FrameBeforeHeader)
        );
    }
}

#[tokio::test]
async fn metadata_then_header_then_frame_has_one_track_and_explicit_revisions() {
    let (server, config) = server(true, IngestLimits::local_probe()).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    for (packet_type, revision) in [(4, 0), (0, 1), (1, 1)] {
        message(
            &mut client,
            9,
            0,
            1,
            &[0x90 | packet_type, b'a', b'v', b'0', b'1', 1],
        )
        .await;
        assert_eq!(
            connection.next().await.unwrap().configuration_revision,
            revision
        );
    }
    end(&mut client).await;
    assert!(connection.next().await.is_none());
    let report = connection.finish().await;
    assert_eq!(report.track_count, 1);
    assert_eq!(report.reason, EndReason::ExplicitStop);
}

#[tokio::test]
async fn tls_start_deadline_and_handshake_eof_are_distinct() {
    let mut limits = IngestLimits::local_probe();
    limits.start_timeout = Duration::from_millis(100);
    let (server, config) = server(true, limits).await;
    let silent = TcpStream::connect(server.local_addr().unwrap())
        .await
        .unwrap();
    let connection = server.accept().await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), connection.finish())
            .await
            .unwrap()
            .reason,
        EndReason::StartTimeout
    );
    drop(silent);
    let (connection, mut client) = connect(&server, config).await;
    client.shutdown().await.unwrap();
    drop(client);
    assert_eq!(
        timeout(Duration::from_secs(2), connection.finish())
            .await
            .unwrap()
            .reason,
        EndReason::PeerClosed
    );
}

#[tokio::test]
async fn cancelled_finish_closes_silent_producer_and_releases_connection_slot() {
    let mut limits = IngestLimits::local_probe();
    limits.max_connections = 1;
    limits.start_timeout = Duration::from_secs(60);
    let (server, config) = server(true, limits).await;
    let mut silent = TcpStream::connect(server.local_addr().unwrap())
        .await
        .unwrap();
    let connection = server.accept().await.unwrap();
    assert!(
        timeout(Duration::from_millis(20), connection.finish())
            .await
            .is_err()
    );
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_millis(500), silent.read(&mut byte))
            .await
            .expect("cancelled finish must close producer before the 60-second start deadline")
            .unwrap(),
        0
    );
    let (replacement, client) = connect(&server, config).await;
    drop(replacement);
    drop(client);
}

#[tokio::test]
async fn missing_media_reaches_idle_timeout_after_authorized_publish() {
    let mut limits = IngestLimits::local_probe();
    limits.media_idle_timeout = Duration::from_millis(100);
    let (server, config) = server(true, limits).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    assert!(
        timeout(Duration::from_secs(2), connection.next())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(connection.finish().await.reason, EndReason::MediaTimeout);
}

#[tokio::test]
async fn retained_events_exhaust_byte_budget_without_unbounded_queue() {
    let mut limits = IngestLimits::local_probe();
    limits.max_queued_bytes = 8;
    limits.max_event_bytes = 8;
    limits.max_header_bytes = 4;
    let (server, config) = server(true, limits).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    message(&mut client, 8, 0, 1, &[0xaf, 0, 0x11, 0x90]).await;
    let retained = connection.next().await.unwrap();
    message(&mut client, 8, 1, 1, &[0xaf, 1, 1, 2, 3]).await;
    assert!(
        timeout(Duration::from_secs(2), connection.next())
            .await
            .unwrap()
            .is_none()
    );
    let report = connection.finish().await;
    assert_eq!(report.reason, EndReason::Backpressure);
    assert_eq!(report.received_events, 1);
    assert!(report.max_queued_bytes <= 8);
    assert_eq!(retained.payload(), &[0x11, 0x90]);
}

#[tokio::test]
async fn connection_capacity_is_reserved_before_tls_work() {
    let mut limits = IngestLimits::local_probe();
    limits.max_connections = 1;
    let (server, _) = server(true, limits).await;
    let _one = TcpStream::connect(server.local_addr().unwrap())
        .await
        .unwrap();
    let connection = server.accept().await.unwrap();
    let _two = TcpStream::connect(server.local_addr().unwrap())
        .await
        .unwrap();
    assert!(matches!(server.accept().await, Err(IngestError::Capacity)));
    drop(connection);
}

#[tokio::test]
async fn completed_session_keeps_reservation_until_last_event_is_released() {
    let mut limits = IngestLimits::local_probe();
    limits.max_connections = 1;
    let (server, config) = server(true, limits).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    message(&mut client, 8, 0, 1, &[0xaf, 0, 0x11, 0x90]).await;
    let retained = connection.next().await.unwrap();
    end(&mut client).await;
    assert!(connection.next().await.is_none());
    assert_eq!(connection.finish().await.reason, EndReason::ExplicitStop);
    let _blocked = TcpStream::connect(server.local_addr().unwrap())
        .await
        .unwrap();
    assert!(matches!(server.accept().await, Err(IngestError::Capacity)));
    drop(retained);
    let _accepted = TcpStream::connect(server.local_addr().unwrap())
        .await
        .unwrap();
    drop(server.accept().await.unwrap());
}

#[tokio::test]
async fn event_count_budget_includes_already_received_events() {
    let mut limits = IngestLimits::local_probe();
    limits.max_queued_events = 1;
    let (server, config) = server(true, limits).await;
    let (mut connection, mut client) = connect(&server, config).await;
    publish(&mut client).await;
    message(&mut client, 8, 0, 1, &[0xaf, 0, 0x11, 0x90]).await;
    let retained = connection.next().await.unwrap();
    message(&mut client, 8, 1, 1, &[0xaf, 1, 1]).await;
    assert!(connection.next().await.is_none());
    assert_eq!(connection.finish().await.reason, EndReason::Backpressure);
    assert_eq!(retained.payload(), &[0x11, 0x90]);
}

#[tokio::test]
async fn frame_before_header_and_excess_tracks_are_rejected() {
    for excess_tracks in [false, true] {
        let mut limits = IngestLimits::local_probe();
        limits.max_tracks = 1;
        let (server, config) = server(true, limits).await;
        let (mut connection, mut client) = connect(&server, config).await;
        publish(&mut client).await;
        if excess_tracks {
            message(&mut client, 8, 0, 1, &[0xaf, 0, 0x11, 0x90]).await;
            drop(connection.next().await.unwrap());
            message(&mut client, 9, 0, 1, &[0x90, b'a', b'v', b'0', b'1', 0x81]).await;
        } else {
            message(&mut client, 8, 1, 1, &[0xaf, 1, 1]).await;
        }
        assert!(connection.next().await.is_none());
        let reason = if excess_tracks {
            MediaError::TrackLimit
        } else {
            MediaError::FrameBeforeHeader
        };
        assert_eq!(
            connection.finish().await.reason,
            EndReason::MediaRejected(reason)
        );
    }
}

#[tokio::test]
async fn authorization_cannot_extend_total_start_deadline() {
    struct PendingAuth;
    impl Authorizer for PendingAuth {
        async fn authorize(&self, _: &str, _: &str) -> Result<AuthorizedSession, ()> {
            std::future::pending().await
        }
    }
    let tls = tls::test_tls();
    let mut limits = IngestLimits::local_probe();
    limits.start_timeout = Duration::from_millis(100);
    let server = IngestServer::bind_loopback(0, tls.server, Arc::new(PendingAuth), limits)
        .await
        .unwrap();
    let (mut connection, mut client) = connect(&server, tls.client).await;
    publish(&mut client).await;
    assert!(
        timeout(Duration::from_secs(2), connection.next())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(connection.finish().await.reason, EndReason::StartTimeout);
}
