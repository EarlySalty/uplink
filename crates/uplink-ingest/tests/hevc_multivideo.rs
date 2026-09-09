//! Opaque transport vectors over real TLS/RTMP. These tests do not claim that
//! their deliberately minimal codec payloads are decodable media.
#[path = "support/tls.rs"]
mod tls;

use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{TlsConnector, client::TlsStream};
use uplink_ingest::*;

type Peer = TlsStream<TcpStream>;

struct Auth;
impl Authorizer for Auth {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        if app == "live" && stream == "transport-vector" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

struct Tag {
    kind: u8,
    dts: u32,
    body: Vec<u8>,
}

fn video(fourcc: &[u8; 4], packet: u8, track: u8, dts: u32, payload: &[u8]) -> Tag {
    let mut body = vec![0x96, packet];
    body.extend_from_slice(fourcc);
    body.push(track);
    body.extend_from_slice(payload);
    Tag { kind: 9, dts, body }
}

fn audio(track: u8, packet: u8, dts: u32) -> Tag {
    let mut body = if track == 0 {
        vec![0xaf, packet]
    } else {
        vec![0x95, packet, b'm', b'p', b'4', b'a', track]
    };
    body.extend_from_slice(&[0x11, 0x90]);
    Tag { kind: 8, dts, body }
}

async fn send(peer: &mut Peer, kind: u8, dts: u32, stream: u32, body: &[u8]) -> io::Result<()> {
    assert!(body.len() < 0xffffff && dts < 0xffffff);
    let mut message = vec![3];
    message.extend_from_slice(&dts.to_be_bytes()[1..]);
    message.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    message.push(kind);
    message.extend_from_slice(&stream.to_le_bytes());
    for (index, chunk) in body.chunks(128).enumerate() {
        if index > 0 {
            message.push(0xc3);
        }
        message.extend_from_slice(chunk);
    }
    peer.write_all(&message).await?;
    peer.flush().await
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

async fn publish(peer: &mut Peer) {
    let mut handshake = vec![0; 1537];
    handshake[0] = 3;
    peer.write_all(&handshake).await.unwrap();
    peer.flush().await.unwrap();
    let mut response = vec![0; 3073];
    peer.read_exact(&mut response).await.unwrap();
    assert_eq!(response[0], 3);
    peer.write_all(&response[1..1537]).await.unwrap();
    peer.flush().await.unwrap();

    let mut connect = amf_string("connect");
    connect.extend(amf_number(1.0));
    connect.extend_from_slice(&[3, 0, 3, b'a', b'p', b'p']);
    connect.extend(amf_string("live"));
    connect.extend_from_slice(&[0, 0, 9]);
    send(peer, 20, 0, 0, &connect).await.unwrap();

    let mut command = amf_string("publish");
    command.extend(amf_number(0.0));
    command.push(5);
    command.extend(amf_string("transport-vector"));
    command.extend(amf_string("live"));
    send(peer, 20, 0, 1, &command).await.unwrap();
}

async fn collect(tags: &[Tag], limits: IngestLimits) -> (Vec<MediaEvent>, SessionReport) {
    timeout(Duration::from_secs(5), async {
        let certificates = tls::test_tls();
        assert!(
            certificates
                .certificate_pem
                .starts_with("-----BEGIN CERTIFICATE-----")
        );
        let server = IngestServer::bind_loopback(0, certificates.server, Arc::new(Auth), limits)
            .await
            .unwrap();
        let address = server.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            TlsConnector::from(certificates.client)
                .connect(
                    "localhost".try_into().unwrap(),
                    TcpStream::connect(address).await.unwrap(),
                )
                .await
                .unwrap()
        });
        let mut incoming = server.accept().await.unwrap();
        let mut peer = peer.await.unwrap();
        publish(&mut peer).await;
        for tag in tags {
            if send(&mut peer, tag.kind, tag.dts, 1, &tag.body)
                .await
                .is_err()
            {
                // A rejected vector may close the connection before later writes.
                break;
            }
        }
        let mut end = amf_string("deleteStream");
        end.extend(amf_number(0.0));
        end.push(5);
        end.extend(amf_number(1.0));
        let _ = send(&mut peer, 20, 0, 1, &end).await;
        let mut events = Vec::new();
        while let Some(event) = incoming.next().await {
            events.push(event);
        }
        let report = incoming.finish().await;
        (events, report)
    })
    .await
    .expect("Transportprobe muss innerhalb der festen Frist enden")
}

#[tokio::test]
async fn hevc_avc_av1_and_two_aac_feeds_keep_distinct_tracks_and_timestamps() {
    let tags = [
        audio(0, 0, 0),
        audio(1, 0, 0),
        video(b"hvc1", 0, 0, 0, &[0x01]),
        video(b"avc1", 0, 7, 0, &[0x02]),
        video(b"av01", 0, 12, 0, &[0x03]),
        video(b"hvc1", 1, 0, 300, &[0xff, 0xff, 0x77, 0xa1]),
        audio(0, 1, 100),
        video(b"avc1", 1, 7, 100, &[0, 0, 137, 0xa2]),
        audio(1, 1, 100),
        video(b"av01", 1, 12, 200, &[0xff, 0xff, 0xff, 0xa3]),
        video(b"hvc1", 3, 0, 340, &[0xa4]),
        video(b"avc1", 3, 7, 140, &[0xa5]),
    ];
    let (events, report) = collect(&tags, IngestLimits::local_probe()).await;
    assert_eq!(report.reason, EndReason::ExplicitStop);
    assert_eq!(report.track_count, 5);
    assert_eq!(events.len(), tags.len());
    for (event, tag) in events.iter().zip(&tags) {
        assert_eq!(event.wire_body(), tag.body);
        assert_eq!(event.configuration_revision, 1);
        assert_eq!(event.dts_ms, tag.dts);
        assert_eq!(event.identity.generation, report.generation);
    }
    for (index, kind, id, codec, pts, payload) in [
        (5, MediaKind::Video, 0, WireCodec::Hevc, 163, &[0xa1][..]),
        (
            6,
            MediaKind::Audio,
            0,
            WireCodec::Aac,
            100,
            &[0x11, 0x90][..],
        ),
        (7, MediaKind::Video, 7, WireCodec::H264, 237, &[0xa2][..]),
        (
            8,
            MediaKind::Audio,
            1,
            WireCodec::Aac,
            100,
            &[0x11, 0x90][..],
        ),
        (
            9,
            MediaKind::Video,
            12,
            WireCodec::Av1,
            200,
            &[0xff, 0xff, 0xff, 0xa3][..],
        ),
        (10, MediaKind::Video, 0, WireCodec::Hevc, 340, &[0xa4][..]),
        (11, MediaKind::Video, 7, WireCodec::H264, 140, &[0xa5][..]),
    ] {
        let event = &events[index];
        assert_eq!(event.identity.track, WireTrack { kind, wire_id: id });
        assert_eq!(event.codec, codec);
        assert_eq!(event.pts_ms, pts);
        assert_eq!(event.payload(), payload);
    }
}

#[tokio::test]
async fn video_track_revisions_and_codec_changes_remain_independent() {
    let tags = [
        video(b"hvc1", 0, 7, 0, &[1]),
        video(b"av01", 0, 12, 0, &[2]),
        video(b"hvc1", 3, 7, 400, &[3]),
        video(b"av01", 1, 12, 200, &[4]),
        video(b"avc1", 0, 7, 0, &[5]),
        video(b"avc1", 3, 7, 10, &[6]),
        video(b"av01", 1, 12, 240, &[7]),
    ];
    let (events, report) = collect(&tags, IngestLimits::local_probe()).await;
    assert_eq!(report.reason, EndReason::ExplicitStop);
    assert_eq!(report.track_count, 2);
    assert_eq!(
        events
            .iter()
            .map(|event| event.configuration_revision)
            .collect::<Vec<_>>(),
        [1, 1, 1, 1, 2, 2, 1]
    );
    assert_eq!(events[5].codec, WireCodec::H264);
    assert_eq!(events[6].codec, WireCodec::Av1);
}

#[tokio::test]
async fn multivideo_cannot_change_codec_or_borrow_another_tracks_header() {
    for (frame, expected) in [
        (
            video(b"avc1", 3, 7, 10, &[1]),
            MediaError::CodecChangedWithoutHeader,
        ),
        (
            video(b"hvc1", 3, 12, 10, &[1]),
            MediaError::FrameBeforeHeader,
        ),
    ] {
        let (events, report) = collect(
            &[video(b"hvc1", 0, 7, 0, &[1]), frame],
            IngestLimits::local_probe(),
        )
        .await;
        assert_eq!(report.reason, EndReason::MediaRejected(expected));
        assert_eq!(events.len(), 1);
    }
    let (events, report) = collect(
        &[audio(7, 0, 0), video(b"hvc1", 3, 7, 10, &[1])],
        IngestLimits::local_probe(),
    )
    .await;
    assert_eq!(
        report.reason,
        EndReason::MediaRejected(MediaError::FrameBeforeHeader)
    );
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn default_enhanced_and_legacy_avc_share_the_same_video_track_zero() {
    let tags = [
        Tag {
            kind: 9,
            dts: 0,
            body: vec![0x17, 0, 0, 0, 0, 1],
        },
        video(b"avc1", 3, 0, 10, &[2]),
        video(b"hvc1", 0, 0, 0, &[3]),
        Tag {
            kind: 9,
            dts: 20,
            body: vec![0x93, b'h', b'v', b'c', b'1', 4],
        },
    ];
    let (events, report) = collect(&tags, IngestLimits::local_probe()).await;
    assert_eq!(report.reason, EndReason::ExplicitStop);
    assert_eq!(report.track_count, 1);
    assert_eq!(
        events
            .iter()
            .map(|event| event.configuration_revision)
            .collect::<Vec<_>>(),
        [1, 1, 2, 2]
    );
    assert_eq!(events[3].codec, WireCodec::Hevc);
}

#[tokio::test]
async fn sequence_end_and_timestamp_regression_apply_only_to_the_selected_track() {
    for last in [video(b"hvc1", 3, 7, 5_000, &[1])] {
        let (_, report) = collect(
            &[
                video(b"hvc1", 0, 7, 0, &[1]),
                video(b"hvc1", 3, 7, 10_000, &[2]),
                last,
            ],
            IngestLimits::local_probe(),
        )
        .await;
        assert_eq!(
            report.reason,
            EndReason::MediaRejected(MediaError::TimestampRegression)
        );
        let diagnostic = format!("{report:?}");
        for expected in ["kind: Video", "wire_id: 7", "last_dts_ms: 10000", "new_dts_ms: 5000", "delta_ms: 5000", "session_duration_ms:", "server.rs", "event_kind: Frame"] {
            assert!(diagnostic.contains(expected), "fehlende Diagnose {expected}: {diagnostic}");
        }
    }
    let (events, report) = collect(
        &[
            video(b"hvc1", 0, 7, 0, &[1]),
            video(b"av01", 0, 12, 0, &[2]),
            video(b"hvc1", 2, 7, 10, &[]),
            video(b"av01", 1, 12, 20, &[3]),
            video(b"hvc1", 3, 7, 20, &[4]),
        ],
        IngestLimits::local_probe(),
    )
    .await;
    assert_eq!(
        report.reason,
        EndReason::MediaRejected(MediaError::FrameBeforeHeader)
    );
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].identity.track.wire_id, 12);
}

#[tokio::test]
async fn multivideo_header_metadata_and_event_limits_apply_before_retention() {
    for packet in [0, 4] {
        let mut limits = IngestLimits::local_probe();
        limits.max_tracks = 2;
        let (events, report) = collect(
            &[
                audio(0, 0, 0),
                video(b"hvc1", 0, 7, 0, &[1]),
                video(b"av01", packet, 12, 0, &[2]),
            ],
            limits,
        )
        .await;
        assert_eq!(
            report.reason,
            EndReason::MediaRejected(MediaError::TrackLimit)
        );
        assert_eq!(report.track_count, 2);
        assert_eq!(events.len(), 2);
    }
    let mut limits = IngestLimits::local_probe();
    limits.max_header_bytes = 2;
    let (events, report) = collect(&[video(b"hvc1", 0, 7, 0, &[1, 2, 3])], limits).await;
    assert_eq!(
        report.reason,
        EndReason::MediaRejected(MediaError::HeaderLimit)
    );
    assert!(events.is_empty());
    assert_eq!(report.track_count, 0);

    let mut limits = IngestLimits::local_probe();
    limits.max_header_bytes = 2;
    limits.max_event_bytes = 8;
    let (events, report) = collect(
        &[
            video(b"hvc1", 0, 7, 0, &[1]),
            video(b"hvc1", 3, 7, 10, &[2, 3]),
        ],
        limits,
    )
    .await;
    assert_eq!(report.reason, EndReason::Backpressure);
    assert_eq!(events.len(), 1);
    assert_eq!(report.track_count, 1);
}

#[tokio::test]
async fn malformed_multivideo_is_rejected_without_publishing_an_event() {
    for body in [
        vec![0x96, 0, b'h', b'v', b'c', b'1'],
        vec![0x96, 0x10, b'h', b'v', b'c', b'1', 7, 1],
        vec![0x96, 6, b'h', b'v', b'c', b'1', 7, 1],
        vec![0x96, 0, b'h', b'e', b'v', b'1', 7, 1],
        vec![0x96, 1, b'h', b'v', b'c', b'1', 7, 0, 0],
        vec![0x96, 3, b'h', b'v', b'c', b'1', 7],
    ] {
        let (events, report) = collect(
            &[Tag {
                kind: 9,
                dts: 0,
                body,
            }],
            IngestLimits::local_probe(),
        )
        .await;
        assert!(matches!(report.reason, EndReason::MediaRejected(_)));
        assert!(events.is_empty());
        assert_eq!(report.track_count, 0);
    }
}

#[tokio::test]
async fn timestamp_regression_small_frames_remain_monotone_and_keep_cts() {
    for delta in [1, 2000] {
        let tags = [
            video(b"hvc1", 0, 7, 0, &[1]),
            audio(1, 0, 0),
            video(b"hvc1", 1, 7, 3000, &[0, 0, 137, 1]),
            video(b"hvc1", 1, 7, 3000 - delta, &[0, 0, 137, 2]),
            audio(1, 1, 1),
            video(b"hvc1", 1, 7, 3001, &[0, 0, 137, 3]),
        ];
        let (events, report) = collect(&tags, IngestLimits::local_probe()).await;
        assert_eq!(report.reason, EndReason::ExplicitStop);
        assert_eq!(events.len(), tags.len());
        assert_eq!((events[3].dts_ms, events[3].pts_ms), (3000, 3137));
        assert_eq!(events[4].dts_ms, 1);
        assert_eq!(events[5].dts_ms, 3001);
    }
}

#[tokio::test]
async fn timestamp_regression_clamp_limit_is_fifty_per_session() {
    for count in [50, 51] {
        let mut tags = vec![audio(0, 0, 0), audio(1, 0, 0), audio(0, 1, 100), audio(1, 1, 100)];
        for index in 0..count {
            tags.push(audio((index % 2) as u8, 1, 99));
        }
        let (events, report) = collect(&tags, IngestLimits::local_probe()).await;
        assert_eq!(events.len(), 4 + count.min(50));
        assert_eq!(report.reason, if count == 50 { EndReason::ExplicitStop } else { EndReason::MediaRejected(MediaError::TimestampRegression) });
    }
}

#[tokio::test]
async fn timestamp_regression_obs_sequence_end_keeps_streamer_stop_reason() {
    for fourcc in [b"hvc1", b"av01", b"avc1"] {
        let tags = [
            video(fourcc, 0, 7, 0, &[1]),
            video(fourcc, 3, 7, 29_548, &[1]),
            video(fourcc, 2, 7, 0, &[]),
        ];
        let (events, report) = collect(&tags, IngestLimits::local_probe()).await;
        assert_eq!(report.reason, EndReason::ExplicitStop);
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].dts_ms, 29_548);
        let diagnostic = format!("{report:?}");
        for expected in ["event_kind: SequenceEnd", "last_dts_ms: 29548", "new_dts_ms: 0", "delta_ms: 29548", "wire_id: 7"] {
            assert!(diagnostic.contains(expected), "fehlende Schlussdiagnose {expected}: {diagnostic}");
        }
    }
}
