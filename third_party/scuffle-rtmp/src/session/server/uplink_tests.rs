// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
use super::*;
use crate::messages::MessageType;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

struct Handler;
impl SessionHandler for Handler {
    async fn on_publish(&mut self, _: u32, _: &str, _: &str) -> Result<(), ServerSessionError> {
        Ok(())
    }
    async fn on_unpublish(&mut self, _: u32) -> Result<(), ServerSessionError> {
        Ok(())
    }
    async fn on_data(&mut self, _: u32, _: SessionData) -> Result<(), ServerSessionError> {
        Ok(())
    }
}

#[derive(Default)]
struct BufferedIo {
    reads: usize,
    pending: Vec<u8>,
    flushed: Vec<u8>,
}
impl AsyncRead for BufferedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.reads += 1;
        if self.reads > 2 {
            return Poll::Ready(Err(io::Error::other("test prevents EOF spin")));
        }
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for BufferedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.pending.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let pending = std::mem::take(&mut self.pending);
        self.flushed.extend(pending);
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn uplink_flush_reaches_buffered_transport() {
    let mut session = ServerSession::new(BufferedIo::default(), Handler);
    session.write_buf.extend_from_slice(&[1, 2, 3]);
    session.flush().await.unwrap();
    assert_eq!(session.io.flushed, [1, 2, 3]);
    assert!(session.io.pending.is_empty());
}

#[tokio::test]
async fn uplink_handshake_eof_terminates_after_first_read() {
    let mut session = ServerSession::new(BufferedIo::default(), Handler);
    assert!(
        session
            .drive_handshake(&mut HandshakeServer::default())
            .await
            .is_err()
    );
    assert_eq!(session.io.reads, 1);
}

#[test]
fn uplink_zero_ack_window_is_rejected() {
    let mut session = ServerSession::new(BufferedIo::default(), Handler);
    assert!(session.on_acknowledgement_window_size(0).is_err());
}

#[tokio::test]
async fn uplink_media_before_publish_is_rejected() {
    let mut session = ServerSession::new(BufferedIo::default(), Handler);
    assert!(
        session
            .process_message(
                MessageData::AudioData {
                    data: bytes::Bytes::from_static(&[0xaf, 1, 42])
                },
                1,
                0
            )
            .await
            .is_err()
    );
}

#[test]
fn uplink_output_limit_precedes_vec_growth() {
    use std::io::Write;
    let mut data = vec![1, 2];
    let mut writer = BoundedWriter {
        buffer: &mut data,
        limit: 3,
    };
    writer.write_all(&[3]).unwrap();
    assert!(writer.write_all(&[4]).is_err());
    assert_eq!(data, [1, 2, 3]);
}

#[tokio::test(start_paused = true)]
async fn uplink_handshake_total_deadline_beats_one_byte_slowloris() {
    use std::time::Duration;
    let (server, mut client) = tokio::io::duplex(64);
    let sender = tokio::spawn(async move {
        for _ in 0..100 {
            if client.write_all(&[3]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let limits = ServerSessionLimits {
        handshake_timeout: Duration::from_millis(50),
        read_timeout: Duration::from_secs(1),
        ..Default::default()
    };
    let start = tokio::time::Instant::now();
    let result = ServerSession::new(server, Handler)
        .with_limits(limits)
        .unwrap()
        .run()
        .await;
    assert!(matches!(
        result,
        Err(crate::error::RtmpError::Session(
            ServerSessionError::Timeout(_)
        ))
    ));
    assert!(start.elapsed() < Duration::from_millis(100));
    sender.abort();
    let _ = sender.await;
}

#[tokio::test(start_paused = true)]
async fn uplink_read_idle_timeout_is_not_peer_eof() {
    let (server, _client) = tokio::io::duplex(64);
    let mut session = ServerSession::new(server, Handler);
    let error = session.read_input(8).await.unwrap_err();
    assert!(matches!(
        error,
        crate::error::RtmpError::Session(ServerSessionError::Timeout(_))
    ));
    assert!(!error.is_client_closed());
}

#[tokio::test]
async fn uplink_c2_requires_exactly_1536_bytes() {
    let (server, mut client) = tokio::io::duplex(8192);
    let mut c0c1 = vec![0; 1537];
    c0c1[0] = 3;
    client.write_all(&c0c1).await.unwrap();
    let mut session = ServerSession::new(server, Handler);
    let mut handshaker = HandshakeServer::default();
    assert!(!session.drive_handshake(&mut handshaker).await.unwrap());
    session.flush().await.unwrap();
    let mut response = [0; 3073];
    client.read_exact(&mut response).await.unwrap();
    client.write_all(&response[1..1537]).await.unwrap();
    assert!(session.drive_handshake(&mut handshaker).await.unwrap());
    assert!(session.read_buf.is_empty());
}

#[test]
fn uplink_invalid_limits_are_rejected_before_run() {
    let limits = ServerSessionLimits {
        max_read_buffer_bytes: 1,
        ..Default::default()
    };
    assert!(
        ServerSession::new(BufferedIo::default(), Handler)
            .with_limits(limits)
            .is_err()
    );
}

#[tokio::test]
async fn uplink_amf_bomb_is_rejected_before_handler() {
    let mut session = ServerSession::new(BufferedIo::default(), Handler);
    let mut payload = Vec::new();
    let mut amf = scuffle_amf0::encoder::Amf0Encoder::new(&mut payload);
    amf.encode_string("connect").unwrap();
    amf.encode_number(1.0).unwrap();
    payload.extend_from_slice(&[8, 255, 255, 255, 255]);
    let chunk = crate::chunk::Chunk::new(3, 0, MessageType::CommandAMF0, 0, payload.into());
    let writer = ChunkWriter::default();
    writer.write_chunk(&mut session.write_buf, chunk).unwrap();
    session.read_buf = BytesMut::from(session.write_buf.as_slice());
    session.write_buf.clear();
    assert!(session.process_chunks().await.is_err());
    assert!(session.app_name.is_none());
    assert!(session.publishing_stream_ids.is_empty());
}

struct FlushBlocked;
impl AsyncRead for FlushBlocked {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}
impl AsyncWrite for FlushBlocked {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(start_paused = true)]
async fn uplink_flush_is_covered_by_write_deadline() {
    let mut session = ServerSession::new(FlushBlocked, Handler);
    session.write_buf.push(1);
    let start = tokio::time::Instant::now();
    assert!(matches!(
        session.flush().await,
        Err(crate::error::RtmpError::Session(
            ServerSessionError::Timeout(_)
        ))
    ));
    assert_eq!(start.elapsed(), session.limits.write_timeout);
    assert_eq!(session.write_buf, [1]);
}

#[test]
fn uplink_command_errors_do_not_reflect_peer_strings() {
    let error =
        crate::error::RtmpError::Command(crate::command_messages::error::CommandError::Amf0(
            scuffle_amf0::Amf0Error::Custom("synthetic-credential-marker".into()),
        ));
    assert!(!format!("{error}").contains("synthetic-credential-marker"));
    assert!(!format!("{error:?}").contains("synthetic-credential-marker"));
}

#[tokio::test]
async fn uplink_ack_reports_received_bytes_after_u32_wrap() {
    let (server, mut client) = tokio::io::duplex(1024);
    let mut incoming = Vec::new();
    ProtocolControlMessageAcknowledgement { sequence_number: 0 }
        .write(&mut incoming, &ChunkWriter::default())
        .unwrap();
    let mut session = ServerSession::new(server, Handler);
    session.acknowledgement_window_size = u32::MAX;
    session.sequence_number = u32::MAX - incoming.len() as u32 + 1;
    session.bytes_since_ack = u64::from(session.sequence_number);
    client.write_all(&incoming).await.unwrap();
    session.drive().await.unwrap();
    let mut received = vec![0; incoming.len()];
    client.read_exact(&mut received).await.unwrap();
    let ack = ChunkReader::default()
        .read_chunk(&mut BytesMut::from(received.as_slice()))
        .unwrap()
        .unwrap();
    assert_eq!(ack.payload.as_ref(), 0_u32.to_be_bytes());
}

async fn uplink_session_after_control_message(
    message_type: MessageType,
    body: &'static [u8],
    close_transport: bool,
) -> Result<bool, crate::error::RtmpError> {
    let (server, mut client) = tokio::io::duplex(8192);
    let session = tokio::spawn(ServerSession::new(server, Handler).run());
    let mut c0c1 = [0; 1537];
    c0c1[0] = 3;
    client.write_all(&c0c1).await.unwrap();
    let mut response = [0; 3073];
    client.read_exact(&mut response).await.unwrap();
    client.write_all(&response[1..1537]).await.unwrap();
    let mut frame = Vec::new();
    ChunkWriter::default()
        .write_chunk(
            &mut frame,
            crate::chunk::Chunk::new(2, 0, message_type, 0, bytes::Bytes::from_static(body)),
        )
        .unwrap();
    client.write_all(&frame).await.unwrap();
    if close_transport {
        client.shutdown().await.unwrap();
    }
    // In malformed-body cases the peer remains open until run() has returned.
    // An end here cannot have been caused by transport EOF.
    let result = session.await.unwrap();
    drop(client);
    result
}

#[tokio::test]
async fn uplink_short_set_chunk_size_is_protocol_error_while_transport_open() {
    let error = uplink_session_after_control_message(MessageType::SetChunkSize, &[0, 0, 0], false)
        .await
        .expect_err("a complete malformed message is not peer EOF");
    assert!(
        matches!(&error, crate::error::RtmpError::Io(io) if io.kind() == io::ErrorKind::InvalidData)
    );
    assert!(!error.is_client_closed());
}

#[tokio::test]
async fn uplink_short_ack_window_is_protocol_error_while_transport_open() {
    let error = uplink_session_after_control_message(
        MessageType::WindowAcknowledgementSize,
        &[0, 0, 0],
        false,
    )
    .await
    .expect_err("a complete malformed message is not peer EOF");
    assert!(
        matches!(&error, crate::error::RtmpError::Io(io) if io.kind() == io::ErrorKind::InvalidData)
    );
    assert!(!error.is_client_closed());
}

#[tokio::test]
async fn uplink_valid_control_then_transport_eof_remains_peer_close() {
    for message_type in [
        MessageType::SetChunkSize,
        MessageType::WindowAcknowledgementSize,
    ] {
        let result =
            uplink_session_after_control_message(message_type, &[0, 0, 0, 128], true).await;
        assert!(matches!(result, Ok(true)));
    }
}
