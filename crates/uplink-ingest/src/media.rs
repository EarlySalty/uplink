use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaKind {
    Audio,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WireTrack {
    pub kind: MediaKind,
    pub wire_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireCodec {
    Aac,
    Av1,
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    SequenceHeader,
    Frame,
    SequenceEnd,
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    Truncated,
    UnsupportedWireForm,
    HeaderLimit,
    EmptyPayload,
    FrameBeforeHeader,
    TrackLimit,
    CodecChangedWithoutHeader,
    TimestampRegression,
    RevisionExhausted,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Inspected {
    pub track: WireTrack,
    pub codec: WireCodec,
    pub event_kind: EventKind,
    pub payload: Range<usize>,
    pub pts_ms: i64,
}

/// Prüft ausschließlich den kleinen E-RTMP-Transportkopf, niemals SPS/AVCC,
/// AV1-Bitstream oder AudioSpecificConfig. Header und Bilder bleiben opaque.
pub(crate) fn inspect(
    kind: MediaKind,
    body: &[u8],
    dts: u32,
    max_header: usize,
) -> Result<Inspected, MediaError> {
    let first = *body.first().ok_or(MediaError::Truncated)?;
    let (codec, wire_id, packet_type, prefix, cts) = match kind {
        MediaKind::Audio if first >> 4 == 10 => {
            if body.len() < 2 {
                return Err(MediaError::Truncated);
            }
            if body[1] > 1 {
                return Err(MediaError::UnsupportedWireForm);
            }
            (WireCodec::Aac, 0, body[1], 2, 0)
        }
        MediaKind::Audio => {
            if body.len() < 7 {
                return Err(MediaError::Truncated);
            }
            if first != 0x95
                || body[1] >> 4 != 0
                || &body[2..6] != b"mp4a"
                || !matches!(body[1], 0..=2 | 4)
            {
                return Err(MediaError::UnsupportedWireForm);
            }
            if body[1] == 4 {
                validate_native_audio_channels(&body[7..])?;
            }
            (WireCodec::Aac, body[6], body[1], 7, 0)
        }
        MediaKind::Video if first & 0x80 == 0 => {
            if body.len() < 5 {
                return Err(MediaError::Truncated);
            }
            if first & 15 != 7 || body[1] > 2 {
                return Err(MediaError::UnsupportedWireForm);
            }
            let offset = i32::from_be_bytes([body[2], body[3], body[4], 0]) >> 8;
            (
                WireCodec::H264,
                0,
                body[1],
                5,
                if body[1] == 1 { i64::from(offset) } else { 0 },
            )
        }
        MediaKind::Video => {
            if body.len() < 5 {
                return Err(MediaError::Truncated);
            }
            if &body[1..5] != b"av01" || first & 15 > 4 {
                return Err(MediaError::UnsupportedWireForm);
            }
            (WireCodec::Av1, 0, first & 15, 5, 0)
        }
    };
    let event_kind = match packet_type {
        0 => EventKind::SequenceHeader,
        1 | 3 => EventKind::Frame,
        2 => EventKind::SequenceEnd,
        4 => EventKind::Metadata,
        _ => return Err(MediaError::UnsupportedWireForm),
    };
    let payload_len = body.len() - prefix;
    if matches!(event_kind, EventKind::SequenceHeader | EventKind::Frame) && payload_len == 0 {
        return Err(MediaError::EmptyPayload);
    }
    if event_kind == EventKind::SequenceHeader && payload_len > max_header {
        return Err(MediaError::HeaderLimit);
    }
    Ok(Inspected {
        track: WireTrack { kind, wire_id },
        codec,
        event_kind,
        payload: prefix..body.len(),
        pts_ms: i64::from(dts) + cts,
    })
}

/// Only the measured native channel-order form is admitted. Enhanced RTMP
/// defines a one-byte order, channel count and a four-byte big-endian mask
/// with 24 defined speaker positions.
/// This metadata does not establish an AudioProfile or infer live/VOD roles.
fn validate_native_audio_channels(payload: &[u8]) -> Result<(), MediaError> {
    if payload.len() < 6 {
        return Err(MediaError::Truncated);
    }
    if payload.len() != 6 || payload[0] != 1 || payload[1] == 0 {
        return Err(MediaError::UnsupportedWireForm);
    }
    let flags = u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]);
    if flags & 0xff00_0000 != 0 || flags.count_ones() != u32::from(payload[1]) {
        return Err(MediaError::UnsupportedWireForm);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_and_video_wire_zero_are_different_tracks() {
        let audio = inspect(MediaKind::Audio, &[0xaf, 0, 0x11, 0x90], 0, 64).unwrap();
        let video = inspect(
            MediaKind::Video,
            &[0x90, b'a', b'v', b'0', b'1', 0x81],
            0,
            64,
        )
        .unwrap();
        assert_ne!(audio.track, video.track);
        assert_eq!(audio.track.wire_id, 0);
        assert_eq!(video.track.wire_id, 0);
    }

    #[test]
    fn second_aac_feed_preserves_wire_id_and_opaque_header() {
        let body = [0x95, 0, b'm', b'p', b'4', b'a', 1, 0x11, 0x90];
        let result = inspect(MediaKind::Audio, &body, 17, 64).unwrap();
        assert_eq!(result.track.wire_id, 1);
        assert_eq!(result.event_kind, EventKind::SequenceHeader);
        assert_eq!(&body[result.payload], &[0x11, 0x90]);
        assert_eq!(result.pts_ms, 17);
    }

    #[test]
    fn native_aac_channel_configuration_is_metadata_not_a_frame() {
        // Enhanced RTMP: OneTrack / MultichannelConfig / AAC / track 1,
        // AudioChannelOrder::Native, one channel, FrontCenter speaker mask.
        let body = [0x95, 4, b'm', b'p', b'4', b'a', 1, 1, 1, 0, 0, 0, 4];
        let result = inspect(MediaKind::Audio, &body, 17, 64).unwrap();
        assert_eq!(result.track.wire_id, 1);
        assert_eq!(result.event_kind, EventKind::Metadata);
        assert_eq!(&body[result.payload], &[1, 1, 0, 0, 0, 4]);
        assert_eq!(result.pts_ms, 17);
    }

    #[test]
    fn malformed_or_unmeasured_aac_channel_configuration_is_rejected() {
        let valid = [0x95, 4, b'm', b'p', b'4', b'a', 1, 1, 1, 0, 0, 0, 4];
        for length in 7..valid.len() {
            assert!(inspect(MediaKind::Audio, &valid[..length], 0, 64).is_err());
        }
        let mut trailing = valid.to_vec();
        trailing.push(0);
        assert!(inspect(MediaKind::Audio, &trailing, 0, 64).is_err());
        for (index, value) in [(7, 2), (8, 0), (8, 2), (9, 1)] {
            let mut invalid = valid;
            invalid[index] = value;
            assert!(inspect(MediaKind::Audio, &invalid, 0, 64).is_err());
        }
    }

    #[test]
    fn avc_composition_offset_is_signed_and_body_is_not_codec_parsed() {
        let body = [0x27, 1, 0xff, 0xff, 0xff, 0xaa];
        let result = inspect(MediaKind::Video, &body, 10, 64).unwrap();
        assert_eq!(result.pts_ms, 9);
        assert_eq!(&body[result.payload], &[0xaa]);
        // Opaque AVC config; this path must never call the vulnerable SPS parser.
        let suspicious_header = [0x17, 0, 0, 0, 0, 1, 100, 0, 0];
        assert!(inspect(MediaKind::Video, &suspicious_header, 0, 64).is_ok());
    }

    #[test]
    fn unsupported_multi_track_mode_and_short_tags_fail_without_panicking() {
        for length in 0..7 {
            let bytes = vec![0x95; length];
            assert!(inspect(MediaKind::Audio, &bytes, 0, 64).is_err());
        }
        let many_tracks = [0x95, 0x10, b'm', b'p', b'4', b'a', 1, 0x11, 0x90];
        assert_eq!(
            inspect(MediaKind::Audio, &many_tracks, 0, 64),
            Err(MediaError::UnsupportedWireForm)
        );
    }

    #[test]
    fn sequence_header_limit_is_checked_before_retention() {
        let header = [0xaf, 0, 1, 2, 3, 4, 5];
        assert_eq!(
            inspect(MediaKind::Audio, &header, 0, 4),
            Err(MediaError::HeaderLimit)
        );
    }
}
