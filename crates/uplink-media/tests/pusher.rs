use uplink_media::pusher::RunningPusher;
use uplink_media::{MediaLimits, PublishSecret, PublishTarget};

fn target(endpoint: &str) -> PublishTarget {
    PublishTarget {
        id: "probe".into(),
        endpoint: endpoint.into(),
        playpath: PublishSecret::new(b"fixture-only-publish".to_vec()).unwrap(),
        tls: None,
        allowed_hosts: vec!["localhost".into()],
        allow_loopback: false,
        allow_unencrypted: false,
    }
}

#[tokio::test]
async fn insecure_or_private_endpoints_fail_before_network() {
    for endpoint in [
        "rtmp://localhost/live",
        "rtmps://127.0.0.1/live",
        "rtmps://localhost/live",
        "rtmps://localhost/live?key=fixture",
        "rtmps://fixture@localhost/live",
    ] {
        assert!(
            RunningPusher::start(target(endpoint), MediaLimits::default())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn zero_and_oversized_limits_fail_before_network() {
    let limits = MediaLimits {
        queue_bytes: 0,
        ..MediaLimits::default()
    };
    assert!(
        RunningPusher::start(target("rtmps://localhost/live"), limits)
            .await
            .is_err()
    );
    let limits = MediaLimits {
        max_tag_bytes: 0x1000000,
        ..MediaLimits::default()
    };
    assert!(
        RunningPusher::start(target("rtmps://localhost/live"), limits)
            .await
            .is_err()
    );
}

#[test]
fn publish_credentials_are_redacted() {
    let t = target("rtmps://localhost/live");
    assert!(!format!("{t:?}").contains("fixture-only-publish"));
}

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::timeout;
use uplink_ingest::{
    AuthorizedSession, Authorizer, EndReason, IngestLimits, IngestServer, MediaKind,
    rustls::{
        self,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use uplink_media::{
    MediaError, OutputState,
    flv::{FlvReader, FlvTag},
};

struct Auth {
    allow: bool,
    calls: AtomicUsize,
}
impl Authorizer for Auth {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.allow && app == "live" && stream == "fixture-only-publish" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}
fn tls(names: Vec<String>) -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(names).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap();
    (Arc::new(server), Arc::new(client))
}
async fn test_target(
    san: &str,
    allow: bool,
) -> (Arc<IngestServer<Auth>>, PublishTarget, Arc<Auth>) {
    let (tls_server, tls_client) = tls(vec![san.into()]);
    let auth = Arc::new(Auth {
        allow,
        calls: AtomicUsize::new(0),
    });
    let server = Arc::new(
        IngestServer::bind_loopback(0, tls_server, auth.clone(), IngestLimits::local_probe())
            .await
            .unwrap(),
    );
    let mut target = target(&format!(
        "rtmps://localhost:{}/live",
        server.local_addr().unwrap().port()
    ));
    target.allow_loopback = true;
    target.tls = Some(tls_client);
    (server, target, auth)
}

#[tokio::test]
async fn native_rtmps_preserves_av1_h264_and_two_aac_tracks_exactly() {
    for fixture in [
        include_bytes!("../../../experiments/scuffle-probe/fixtures/av1.flv").as_slice(),
        include_bytes!("../../../experiments/scuffle-probe/fixtures/h264.flv").as_slice(),
    ] {
        let (server, target, auth) = test_target("localhost", true).await;
        let capture = tokio::spawn(async move {
            let mut connection = server.accept().await.unwrap();
            let mut received = Vec::new();
            while let Some(event) = connection.next().await {
                let kind = match event.identity.track.kind {
                    MediaKind::Video => 9,
                    MediaKind::Audio => 8,
                };
                received.push((kind, event.dts_ms, event.wire_body().to_vec()));
            }
            (received, connection.finish().await)
        });
        let pusher = timeout(
            Duration::from_secs(5),
            RunningPusher::start(target, MediaLimits::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(pusher.status().state, OutputState::Publishing);
        let mut source = FlvReader::new(fixture, 65536);
        let mut expected = Vec::new();
        let mut tags = 0;
        while let Some(tag) = source.next().await.unwrap() {
            tags += 1;
            if tag.kind() != 18 {
                expected.push((tag.kind(), tag.timestamp_ms(), tag.body().to_vec()));
            }
            pusher.try_send(Arc::new(tag)).unwrap();
        }
        let report = timeout(Duration::from_secs(5), pusher.finish())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.state, OutputState::LocalEndUnconfirmed);
        assert_eq!(report.received_events, tags);
        let (received, report) = timeout(Duration::from_secs(5), capture)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.reason, EndReason::ExplicitStop);
        assert_eq!(report.track_count, 3);
        assert_eq!(received, expected);
        assert_eq!(auth.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn queued_short_stream_finishes_after_delayed_publish_without_losing_headers_or_audio() {
    let (server, target, _) = test_target("localhost", true).await;
    let (release, wait) = tokio::sync::oneshot::channel();
    let capture = tokio::spawn(async move {
        wait.await.unwrap();
        let mut connection = server.accept().await.unwrap();
        let mut received = Vec::new();
        while let Some(event) = connection.next().await {
            received.push((
                event.identity.track.kind,
                event.dts_ms,
                event.wire_body().to_vec(),
            ));
        }
        (received, connection.finish().await)
    });
    let pusher = RunningPusher::spawn(target, MediaLimits::default()).unwrap();
    assert_eq!(pusher.status().state, OutputState::Starting);
    let mut reader = FlvReader::new(
        include_bytes!("../../../experiments/scuffle-probe/fixtures/h264.flv").as_slice(),
        65536,
    );
    let mut expected = Vec::new();
    while let Some(tag) = reader.next().await.unwrap() {
        if tag.kind() != 18 {
            expected.push((
                if tag.kind() == 8 {
                    MediaKind::Audio
                } else {
                    MediaKind::Video
                },
                tag.timestamp_ms(),
                tag.body().to_vec(),
            ));
        }
        pusher.try_send(Arc::new(tag)).unwrap();
    }
    assert_eq!(pusher.status().state, OutputState::Starting);
    let finishing = tokio::spawn(pusher.finish());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !finishing.is_finished(),
        "finish must retain queued media while publish is still pending"
    );
    release.send(()).unwrap();
    let status = timeout(Duration::from_secs(2), finishing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(status.state, OutputState::LocalEndUnconfirmed);
    let (received, report) = timeout(Duration::from_secs(2), capture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.reason, EndReason::ExplicitStop);
    assert_eq!(received, expected);
}

#[tokio::test]
async fn connecting_target_queue_is_bounded_and_stop_releases_its_tcp_socket() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (connected, wait) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 1537];
        socket.read_exact(&mut request).await.unwrap();
        connected.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
    });
    let mut target = target(&format!("rtmp://localhost:{port}/live"));
    target.allow_loopback = true;
    target.allow_unencrypted = true;
    let limits = MediaLimits {
        queue_events: 2,
        startup_timeout: Duration::from_secs(20),
        ..MediaLimits::default()
    };
    let pusher = RunningPusher::spawn(target, limits).unwrap();
    timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap();
    let tag = Arc::new(FlvTag::new(8, 0, Arc::from(&b"\xaf\x00\x11\x90"[..]), 64).unwrap());
    pusher.try_send(tag.clone()).unwrap();
    pusher.try_send(tag.clone()).unwrap();
    assert_eq!(pusher.try_send(tag), Err(MediaError::Backpressure));
    assert_eq!(pusher.status().state, OutputState::Starting);
    assert!(matches!(
        timeout(Duration::from_secs(1), pusher.stop())
            .await
            .unwrap(),
        Err(MediaError::Cancelled)
    ));
    timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn immediate_stop_before_startup_poll_never_opens_a_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut target = target(&format!(
        "rtmp://localhost:{}/live",
        listener.local_addr().unwrap().port()
    ));
    target.allow_loopback = true;
    target.allow_unencrypted = true;
    let pusher = RunningPusher::spawn(target, MediaLimits::default()).unwrap();
    assert!(matches!(pusher.stop().await, Err(MediaError::Cancelled)));
    assert!(
        timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn finishing_a_stalled_start_reports_timeout_and_joins_transport_cleanup() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (connected, wait) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 1537];
        socket.read_exact(&mut request).await.unwrap();
        connected.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
    });
    let mut target = target(&format!("rtmp://localhost:{port}/live"));
    target.allow_loopback = true;
    target.allow_unencrypted = true;
    let limits = MediaLimits {
        startup_timeout: Duration::from_secs(20),
        shutdown_timeout: Duration::from_millis(100),
        ..MediaLimits::default()
    };
    let pusher = RunningPusher::spawn(target, limits).unwrap();
    pusher
        .try_send(Arc::new(
            FlvTag::new(8, 0, Arc::from(&b"\xaf\x00\x11\x90"[..]), 64).unwrap(),
        ))
        .unwrap();
    timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(1), pusher.finish())
            .await
            .unwrap(),
        Err(MediaError::StartTimeout)
    ));
    timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn wrong_ca_and_real_wrong_san_fail_before_publish_authorization() {
    for wrong_ca in [false, true] {
        let (server, mut target, auth) = test_target(
            if wrong_ca {
                "localhost"
            } else {
                "wrong.invalid"
            },
            true,
        )
        .await;
        if wrong_ca {
            let (_, other_ca) = tls(vec!["localhost".into()]);
            target.tls = Some(other_ca);
        }
        let capture = tokio::spawn(async move {
            let mut connection = server.accept().await.unwrap();
            while connection.next().await.is_some() {}
            connection.finish().await
        });
        let result = RunningPusher::start(target, MediaLimits::default()).await;
        assert!(matches!(result, Err(MediaError::TlsRejected)));
        assert_eq!(
            timeout(Duration::from_secs(5), capture)
                .await
                .unwrap()
                .unwrap()
                .reason,
            EndReason::TlsRejected
        );
        assert_eq!(auth.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn rejected_publish_never_reports_publishing() {
    let (server, target, auth) = test_target("localhost", false).await;
    let capture = tokio::spawn(async move {
        let mut connection = server.accept().await.unwrap();
        while connection.next().await.is_some() {}
        connection.finish().await
    });
    assert!(
        RunningPusher::start(target, MediaLimits::default())
            .await
            .is_err()
    );
    assert_eq!(
        timeout(Duration::from_secs(5), capture)
            .await
            .unwrap()
            .unwrap()
            .reason,
        EndReason::AuthorizationRejected
    );
    assert_eq!(auth.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropped_pusher_closes_transport_and_releases_server() {
    let (server, target, _) = test_target("localhost", true).await;
    let capture = tokio::spawn(async move {
        let mut connection = server.accept().await.unwrap();
        while connection.next().await.is_some() {}
        connection.finish().await
    });
    let pusher = RunningPusher::start(target, MediaLimits::default())
        .await
        .unwrap();
    drop(pusher);
    assert_eq!(
        timeout(Duration::from_secs(5), capture)
            .await
            .unwrap()
            .unwrap()
            .reason,
        EndReason::PeerClosed
    );
}

#[tokio::test]
async fn cancelled_start_drops_unresponsive_peer_transport() {
    use tokio::{io::AsyncReadExt, net::TcpListener};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut target = target(&format!(
        "rtmp://localhost:{}/live",
        listener.local_addr().unwrap().port()
    ));
    target.allow_loopback = true;
    target.allow_unencrypted = true;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 2048];
        let mut total = 0;
        loop {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    });
    assert!(
        timeout(
            Duration::from_millis(100),
            RunningPusher::start(target, MediaLimits::default())
        )
        .await
        .is_err()
    );
    assert_eq!(
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap(),
        1537
    );
}

use bytes::{Bytes, BytesMut};
use scuffle_amf0::{Amf0Decoder, Amf0Encoder, Amf0Value};
use scuffle_rtmp::{
    chunk::{Chunk, reader::ChunkReader, writer::ChunkWriter},
    handshake::simple::SimpleHandshakeServer,
    messages::MessageType,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Peer {
    socket: TcpStream,
    parser: ChunkReader,
    writer: ChunkWriter,
    input: BytesMut,
    read: u32,
    written: u32,
    ack_each_read: bool,
}
impl Peer {
    async fn accept(listener: TcpListener) -> Self {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut c0c1 = vec![0; 1537];
        socket.read_exact(&mut c0c1).await.unwrap();
        let mut handshake = SimpleHandshakeServer::default();
        let mut output = Vec::new();
        handshake
            .handshake(&mut std::io::Cursor::new(Bytes::from(c0c1)), &mut output)
            .unwrap();
        socket.write_all(&output).await.unwrap();
        let mut c2 = vec![0; 1536];
        socket.read_exact(&mut c2).await.unwrap();
        handshake
            .handshake(&mut std::io::Cursor::new(Bytes::from(c2)), &mut Vec::new())
            .unwrap();
        Self {
            socket,
            parser: ChunkReader::default(),
            writer: ChunkWriter::default(),
            input: BytesMut::new(),
            // Absolute RTMP-Zähler beginnen bei C0/S0 und schließen beide
            // 1536-Byte-Handshakeblöcke ein.
            read: 3073,
            written: 3073,
            ack_each_read: false,
        }
    }
    async fn next(&mut self) -> Option<Chunk> {
        loop {
            if let Some(chunk) = self.parser.read_chunk(&mut self.input).unwrap() {
                if chunk.message_header.msg_type_id.0 == 1 {
                    assert!(self.parser.update_max_chunk_size(u32::from_be_bytes(
                        chunk.payload.as_ref().try_into().unwrap()
                    ) as usize));
                }
                return Some(chunk);
            }
            let mut bytes = [0_u8; 2048];
            let read = self.socket.read(&mut bytes).await.unwrap();
            if read == 0 {
                return None;
            }
            self.read = self.read.wrapping_add(read as u32);
            self.input.extend_from_slice(&bytes[..read]);
            if self.ack_each_read {
                self.send(3, &self.read.to_be_bytes()).await;
            }
        }
    }
    fn wire(&self, kind: u8, body: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        self.writer
            .write_chunk(
                &mut wire,
                Chunk::new(
                    if kind <= 6 { 2 } else { 3 },
                    0,
                    MessageType(kind),
                    0,
                    Bytes::copy_from_slice(body),
                ),
            )
            .unwrap();
        wire
    }
    async fn send(&mut self, kind: u8, body: &[u8]) {
        let wire = self.wire(kind, body);
        self.socket.write_all(&wire).await.unwrap();
        self.written = self.written.wrapping_add(wire.len() as u32);
    }
    async fn command(&mut self, name: &str, transaction: f64, arguments: &[Amf0Value<'_>]) {
        let mut body = Vec::new();
        let mut encoder = Amf0Encoder::new(&mut body);
        encoder.encode_string(name).unwrap();
        encoder.encode_number(transaction).unwrap();
        for argument in arguments {
            argument.encode(&mut encoder).unwrap();
        }
        self.send(20, &body).await;
    }
    async fn expect_command(&mut self, name: &str) {
        loop {
            let chunk = self.next().await.unwrap();
            if chunk.message_header.msg_type_id.0 == 20 {
                let values = Amf0Decoder::from_slice(&chunk.payload)
                    .decode_all()
                    .unwrap();
                assert!(matches!(&values[0],Amf0Value::String(s) if s.as_str() == name));
                return;
            }
        }
    }
    async fn publish(&mut self) {
        self.expect_command("connect").await;
        self.command(
            "_result",
            1.0,
            &[
                Amf0Value::Null,
                status_object("NetConnection.Connect.Success", "status"),
            ],
        )
        .await;
        self.expect_command("createStream").await;
        self.command("_result", 2.0, &[Amf0Value::Null, Amf0Value::Number(1.0)])
            .await;
        self.expect_command("publish").await;
        self.command(
            "onStatus",
            0.0,
            &[
                Amf0Value::Null,
                status_object("NetStream.Publish.Start", "status"),
            ],
        )
        .await;
    }
}
fn status_object(code: &'static str, level: &'static str) -> Amf0Value<'static> {
    Amf0Value::Object(
        [
            ("code".into(), Amf0Value::String(code.into())),
            ("level".into(), Amf0Value::String(level.into())),
        ]
        .into_iter()
        .collect(),
    )
}
async fn raw_published() -> (Peer, RunningPusher) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut target = target(&format!(
        "rtmp://localhost:{}/live",
        listener.local_addr().unwrap().port()
    ));
    target.allow_loopback = true;
    target.allow_unencrypted = true;
    let (peer, pusher) = timeout(Duration::from_secs(3), async {
        tokio::join!(
            async {
                let mut peer = Peer::accept(listener).await;
                peer.publish().await;
                peer
            },
            RunningPusher::start(target, MediaLimits::default())
        )
    })
    .await
    .unwrap();
    (peer, pusher.unwrap())
}
fn packet(size: usize) -> Arc<FlvTag> {
    Arc::new(FlvTag::new(9, 123, vec![0x17; size].into(), 2 * 1024 * 1024).unwrap())
}

#[tokio::test]
async fn handshake_acknowledgements_accept_both_peer_origins_but_not_unsent_bytes() {
    // OBS zählt den Handshake mit; einige andere Publisher/Server beginnen
    // ihren ACK-Zähler erst bei den RTMP-Nachrichten.
    for peer_offset in [0, 3073] {
        let (mut peer, pusher) = raw_published().await;
        let acknowledged = peer.read - peer_offset;
        for _ in 0..2 {
            peer.send(3, &acknowledged.to_be_bytes()).await;
            peer.send(4, &[0, 6, 0, 1, 2, 3]).await;
            let pong = timeout(Duration::from_secs(1), peer.next())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(pong.message_header.msg_type_id.0, 4);
            assert_eq!(pong.payload.as_ref(), &[0, 7, 0, 1, 2, 3]);
            assert_eq!(pusher.status().state, OutputState::Publishing);
        }
        // Auch nach einem bestätigten Handshake ist ein einziges tatsächlich
        // ungesendetes Byte kein gültiger ACK; kein pauschales Toleranzfenster.
        peer.send(3, &(peer.read + 1).to_be_bytes()).await;
        assert!(
            timeout(Duration::from_secs(1), peer.next())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            pusher.status().state,
            OutputState::Failed(MediaError::ProtocolRejected)
        );
        assert!(matches!(
            pusher.finish().await,
            Err(MediaError::ProtocolRejected)
        ));
    }
}

#[tokio::test]
async fn lowered_ack_window_is_acknowledged_before_any_further_peer_byte() {
    let (mut peer, pusher) = raw_published().await;
    peer.send(5, &1_u32.to_be_bytes()).await;
    let expected = peer.written;
    let ack = timeout(Duration::from_secs(1), peer.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ack.message_header.msg_type_id.0, 3);
    assert_eq!(
        u32::from_be_bytes(ack.payload.as_ref().try_into().unwrap()),
        expected
    );
    drop(pusher);
}

#[tokio::test]
async fn partial_control_input_survives_concurrent_media_send() {
    let (mut peer, pusher) = raw_published().await;
    let ping = [0, 6, 0, 1, 2, 3];
    let wire = peer.wire(4, &ping);
    peer.socket.write_all(&wire[..3]).await.unwrap();
    pusher.try_send(packet(1000)).unwrap();
    let media = timeout(Duration::from_secs(1), peer.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media.message_header.msg_type_id.0, 9);
    assert_eq!(media.payload.len(), 1000);
    peer.socket.write_all(&wire[3..]).await.unwrap();
    let pong = timeout(Duration::from_secs(1), peer.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pong.message_header.msg_type_id.0, 4);
    assert_eq!(pong.payload.as_ref(), &[0, 7, 0, 1, 2, 3]);
    drop(pusher);
}

#[tokio::test]
async fn hard_peer_bandwidth_allows_large_message_through_incremental_acks() {
    let (mut peer, pusher) = raw_published().await;
    peer.send(6, &[0, 0, 0x10, 0, 0]).await; // 4096 bytes, hard
    let ack_window = timeout(Duration::from_secs(1), peer.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ack_window.message_header.msg_type_id.0, 5);
    peer.ack_each_read = true;
    pusher.try_send(packet(60000)).unwrap();
    let media = timeout(Duration::from_secs(3), peer.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media.message_header.msg_type_id.0, 9);
    assert_eq!(media.payload.len(), 60000);
    peer.ack_each_read = false;
    let finish = pusher.finish();
    let (report, ()) = tokio::join!(finish, peer.expect_command("deleteStream"));
    assert_eq!(report.unwrap().received_events, 1);
}

#[tokio::test]
async fn malformed_control_and_remote_error_remain_visible_and_redacted() {
    for malformed in [true, false] {
        let (mut peer, pusher) = raw_published().await;
        if malformed {
            peer.send(5, &[0, 0, 1]).await;
        } else {
            peer.command(
                "onStatus",
                0.0,
                &[
                    Amf0Value::Null,
                    status_object("fixture-only-publish", "error"),
                ],
            )
            .await;
        }
        assert!(
            timeout(Duration::from_secs(1), peer.next())
                .await
                .unwrap()
                .is_none()
        );
        let expected = if malformed {
            MediaError::ProtocolRejected
        } else {
            MediaError::PublishRejected
        };
        assert_eq!(pusher.status().state, OutputState::Failed(expected));
        assert!(!format!("{:?}", pusher.status()).contains("fixture-only-publish"));
        assert!(matches!(pusher.finish().await,Err(e) if e == expected));
    }
}

#[tokio::test]
async fn cancellation_is_joined_and_one_failed_target_does_not_stop_another() {
    let (mut slow_peer, slow) = raw_published().await;
    let (mut fast_peer, fast) = raw_published().await;
    slow.cancel();
    assert_eq!(slow.try_send(packet(100)), Err(MediaError::Cancelled));
    assert_eq!(slow.stop().await.unwrap_err(), MediaError::Cancelled);
    assert!(
        timeout(Duration::from_secs(1), slow_peer.next())
            .await
            .unwrap()
            .is_none()
    );
    fast.try_send(packet(100)).unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), fast_peer.next())
            .await
            .unwrap()
            .unwrap()
            .payload
            .len(),
        100
    );
    let (report, ()) = tokio::join!(fast.finish(), fast_peer.expect_command("deleteStream"));
    assert_eq!(report.unwrap().state, OutputState::LocalEndUnconfirmed);
}

#[tokio::test]
async fn cancelled_finish_aborts_a_write_waiting_for_peer_ack() {
    let (mut peer, pusher) = raw_published().await;
    peer.send(6, &[0, 0, 0x10, 0, 0]).await;
    assert_eq!(
        timeout(Duration::from_secs(1), peer.next())
            .await
            .unwrap()
            .unwrap()
            .message_header
            .msg_type_id
            .0,
        5
    );
    pusher.try_send(packet(60000)).unwrap();
    assert!(
        timeout(Duration::from_millis(100), pusher.finish())
            .await
            .is_err()
    );
    // next retains the partial RTMP packet until EOF. A detached producer would
    // leave this read blocked waiting for the missing ACK window indefinitely.
    assert!(
        timeout(Duration::from_secs(1), peer.next())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn late_peer_close_and_unacknowledged_media_fail_visibly() {
    let (peer, pusher) = raw_published().await;
    drop(peer);
    let failure = timeout(Duration::from_secs(1), async {
        loop {
            if let OutputState::Failed(error) = pusher.status().state {
                return error;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(failure, MediaError::Io);
    assert!(matches!(pusher.finish().await, Err(MediaError::Io)));
}

#[tokio::test]
async fn fragmented_rejection_after_delete_stream_is_not_a_successful_finish() {
    let (mut peer, pusher) = raw_published().await;
    let (result, ()) = tokio::join!(pusher.finish(), async {
        peer.expect_command("deleteStream").await;
        let mut body = Vec::new();
        let mut encoder = Amf0Encoder::new(&mut body);
        encoder.encode_string("_error").unwrap();
        encoder.encode_number(0.0).unwrap();
        let wire = peer.wire(20, &body);
        peer.socket.write_all(&wire[..5]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        peer.socket.write_all(&wire[5..]).await.unwrap();
    });
    assert!(matches!(result, Err(MediaError::PublishRejected)));
}

#[tokio::test]
async fn silent_peer_and_close_after_delete_are_only_local_unconfirmed_ends() {
    for close in [false, true] {
        let (mut peer, pusher) = raw_published().await;
        let started = tokio::time::Instant::now();
        let (result, ()) = tokio::join!(pusher.finish(), async {
            peer.expect_command("deleteStream").await;
            if close {
                peer.socket.shutdown().await.unwrap();
            }
        });
        let status = result.unwrap();
        assert_eq!(status.state, OutputState::LocalEndUnconfirmed);
        assert_eq!(
            serde_json::to_value(status.state).unwrap(),
            "local_end_unconfirmed"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

#[tokio::test]
async fn requested_unpublish_response_is_not_media_or_publication_confirmation() {
    let (mut peer, pusher) = raw_published().await;
    let (result, ()) = tokio::join!(pusher.finish(), async {
        peer.expect_command("deleteStream").await;
        peer.command(
            "onStatus",
            0.0,
            &[
                Amf0Value::Null,
                status_object("NetStream.Unpublish.Success", "status"),
            ],
        )
        .await;
    });
    assert_eq!(result.unwrap().state, OutputState::LocalEndUnconfirmed);
}

#[tokio::test]
async fn delete_stream_uses_zero_transaction_id_on_the_connection_stream() {
    let (mut peer, pusher) = raw_published().await;
    let (result, ()) = tokio::join!(pusher.finish(), async {
        let command = timeout(Duration::from_secs(1), peer.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(command.message_header.msg_type_id.0, 20);
        assert_eq!(command.message_header.msg_stream_id, 0);
        let values = Amf0Decoder::from_slice(&command.payload)
            .decode_all()
            .unwrap();
        assert!(matches!(&values[0], Amf0Value::String(name) if name.as_str() == "deleteStream"));
        assert!(matches!(values[1], Amf0Value::Number(0.0)));
        assert!(matches!(values[2], Amf0Value::Null));
        assert!(matches!(values[3], Amf0Value::Number(1.0)));
    });
    assert_eq!(result.unwrap().state, OutputState::LocalEndUnconfirmed);
}
