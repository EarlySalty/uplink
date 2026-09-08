// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
use super::*;

fn initial(csid: u8, timestamp: u32, length: usize, payload: &[u8]) -> BytesMut {
    let mut bytes = vec![csid];
    let short = timestamp.min(0xff_ffff);
    bytes.extend_from_slice(&short.to_be_bytes()[1..]);
    bytes.extend_from_slice(&(length as u32).to_be_bytes()[1..]);
    bytes.extend_from_slice(&[9, 1, 0, 0, 0]);
    if timestamp >= 0xff_ffff {
        bytes.extend_from_slice(&timestamp.to_be_bytes());
    }
    bytes.extend_from_slice(payload);
    BytesMut::from(bytes.as_slice())
}

#[test]
fn uplink_rejects_replacement_header_during_partial_message() {
    let mut reader = ChunkReader::default();
    let mut first = initial(3, 0, 200, &[1; 128]);
    assert!(reader.read_chunk(&mut first).unwrap().is_none());
    let mut replacement = initial(3, 0, 4, &[2; 4]);
    assert!(reader.read_chunk(&mut replacement).is_err());
}

#[test]
fn uplink_type_one_timestamp_wraps() {
    let mut reader = ChunkReader::default();
    reader
        .read_chunk(&mut initial(3, u32::MAX, 1, &[1]))
        .unwrap()
        .unwrap();
    let mut second = BytesMut::from(&[0x43, 0, 0, 1, 0, 0, 1, 9, 2][..]);
    let message = reader.read_chunk(&mut second).unwrap().unwrap();
    assert_eq!(message.message_header.timestamp, 0);
}

#[test]
fn uplink_type_two_timestamp_wraps() {
    let mut reader = ChunkReader::default();
    reader
        .read_chunk(&mut initial(3, u32::MAX, 1, &[1]))
        .unwrap()
        .unwrap();
    let mut second = BytesMut::from(&[0x83, 0, 0, 1, 2][..]);
    let message = reader.read_chunk(&mut second).unwrap().unwrap();
    assert_eq!(message.message_header.timestamp, 0);
}

#[test]
fn uplink_interleaved_streams_keep_separate_payloads() {
    let mut reader = ChunkReader::default();
    reader
        .read_chunk(&mut initial(3, 0, 200, &[1; 128]))
        .unwrap();
    let other = reader
        .read_chunk(&mut initial(4, 0, 4, &[2; 4]))
        .unwrap()
        .unwrap();
    assert_eq!(other.payload.as_ref(), &[2; 4]);
    let mut tail = BytesMut::from(&[0xc3][..]);
    tail.extend_from_slice(&[1; 72]);
    let first = reader.read_chunk(&mut tail).unwrap().unwrap();
    assert_eq!(first.payload.as_ref(), &[1; 200]);
}

#[test]
fn uplink_message_claim_is_rejected_before_payload_or_maps() {
    let limits = ChunkReaderLimits {
        max_message_bytes: 256,
        max_command_bytes: 64,
        ..Default::default()
    };
    let mut reader = ChunkReader::with_limits(limits).unwrap();
    assert!(reader.read_chunk(&mut initial(3, 0, 257, &[])).is_err());
    assert!(reader.previous_chunk_headers.is_empty());
    assert!(reader.partial_chunks.is_empty());
    let mut amf = initial(3, 0, 65, &[]);
    amf[7] = 20;
    assert!(reader.read_chunk(&mut amf).is_err());
    assert!(reader.previous_chunk_headers.is_empty());
    assert_eq!(reader.partial_reserved_bytes, 0);
}

#[test]
fn uplink_csid_budget_precedes_header_insertion() {
    let limits = ChunkReaderLimits {
        max_chunk_streams: 1,
        max_partial_messages: 1,
        ..Default::default()
    };
    let mut reader = ChunkReader::with_limits(limits).unwrap();
    reader
        .read_chunk(&mut initial(3, 0, 1, &[1]))
        .unwrap()
        .unwrap();
    assert!(reader.read_chunk(&mut initial(4, 0, 1, &[])).is_err());
    assert_eq!(reader.previous_chunk_headers.len(), 1);
    assert_eq!(reader.timestamp_deltas.len(), 1);
}

#[test]
fn uplink_partial_reservations_obey_sum_and_release_on_completion() {
    let limits = ChunkReaderLimits {
        max_partial_bytes: 399,
        ..Default::default()
    };
    let mut reader = ChunkReader::with_limits(limits).unwrap();
    reader
        .read_chunk(&mut initial(3, 0, 200, &[1; 128]))
        .unwrap();
    assert_eq!(reader.partial_reserved_bytes, 200);
    assert!(reader.read_chunk(&mut initial(4, 0, 200, &[])).is_err());
    assert_eq!(reader.partial_reserved_bytes, 200);
    assert_eq!(reader.partial_chunks.len(), 1);
    let mut tail = BytesMut::from(&[0xc3][..]);
    tail.extend_from_slice(&[1; 72]);
    reader.read_chunk(&mut tail).unwrap().unwrap();
    assert_eq!(reader.partial_reserved_bytes, 0);
    assert!(reader.partial_chunks.is_empty());
    reader
        .read_chunk(&mut initial(4, 0, 200, &[1; 128]))
        .unwrap();
    assert_eq!(reader.partial_reserved_bytes, 200);
}

#[test]
fn uplink_partial_count_is_checked_before_new_reservation() {
    let limits = ChunkReaderLimits {
        max_partial_messages: 1,
        ..Default::default()
    };
    let mut reader = ChunkReader::with_limits(limits).unwrap();
    reader
        .read_chunk(&mut initial(3, 0, 200, &[1; 128]))
        .unwrap();
    assert!(reader.read_chunk(&mut initial(4, 0, 200, &[])).is_err());
    assert_eq!(reader.partial_chunks.len(), 1);
    assert_eq!(reader.partial_reserved_bytes, 200);
}

#[test]
fn uplink_chunk_negotiation_is_rejected_not_clamped() {
    let limits = ChunkReaderLimits {
        max_chunk_size: 256,
        ..Default::default()
    };
    let mut reader = ChunkReader::with_limits(limits).unwrap();
    assert!(!reader.update_max_chunk_size(0));
    assert!(!reader.update_max_chunk_size(257));
    assert_eq!(reader.max_chunk_size, 128);
    assert!(reader.update_max_chunk_size(256));
    assert_eq!(reader.max_chunk_size, 256);
}

#[test]
fn uplink_type_three_new_message_reuses_delta_but_continuation_does_not() {
    let mut reader = ChunkReader::default();
    reader
        .read_chunk(&mut initial(3, 10, 1, &[1]))
        .unwrap()
        .unwrap();
    let mut type_one = BytesMut::from(&[0x43, 0, 0, 3, 0, 0, 200, 9][..]);
    type_one.extend_from_slice(&[1; 128]);
    assert!(reader.read_chunk(&mut type_one).unwrap().is_none());
    let mut tail = BytesMut::from(&[0xc3][..]);
    tail.extend_from_slice(&[1; 72]);
    assert_eq!(
        reader
            .read_chunk(&mut tail)
            .unwrap()
            .unwrap()
            .message_header
            .timestamp,
        13
    );
    let mut next = BytesMut::from(&[0xc3][..]);
    next.extend_from_slice(&[2; 128]);
    assert!(reader.read_chunk(&mut next).unwrap().is_none());
    let mut tail = BytesMut::from(&[0xc3][..]);
    tail.extend_from_slice(&[2; 72]);
    assert_eq!(
        reader
            .read_chunk(&mut tail)
            .unwrap()
            .unwrap()
            .message_header
            .timestamp,
        16
    );
}
