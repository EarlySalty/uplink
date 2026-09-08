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
    Hevc,
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

/// Prüft ausschließlich den kleinen E-RTMP-Transportkopf, niemals SPS/AVCC/HVCC,
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
            let multitrack = first & 15 == 6;
            let (packet_type, fourcc_start, wire_id, mut prefix) = if multitrack {
                if body.len() < 7 {
                    return Err(MediaError::Truncated);
                }
                // OneTrack only: a complete message carries one track. The
                // upper nibble selects the multitrack mode; nesting is forbidden.
                if body[1] > 4 {
                    return Err(MediaError::UnsupportedWireForm);
                }
                (body[1], 2, body[6], 7)
            } else {
                (first & 15, 1, 0, 5)
            };
            if body.len() < prefix {
                return Err(MediaError::Truncated);
            }
            if packet_type > 4 || (packet_type != 4 && !matches!((first >> 4) & 7, 1 | 2 | 4)) {
                return Err(MediaError::UnsupportedWireForm);
            }
            // These FourCC values are defined by E-RTMP and emitted by OBS.
            // The MP4 sample entry spelling "hev1" is not admitted as an alias.
            let codec = match &body[fourcc_start..fourcc_start + 4] {
                b"av01" => WireCodec::Av1,
                b"avc1" => WireCodec::H264,
                b"hvc1" => WireCodec::Hevc,
                _ => return Err(MediaError::UnsupportedWireForm),
            };
            let cts = if packet_type == 1 && matches!(codec, WireCodec::H264 | WireCodec::Hevc) {
                let offset = body.get(prefix..prefix + 3).ok_or(MediaError::Truncated)?;
                prefix += 3;
                i64::from(i32::from_be_bytes([offset[0], offset[1], offset[2], 0]) >> 8)
            } else {
                // CodedFramesX has an implicit zero offset. AV1 CodedFrames,
                // configuration, metadata and sequence ends do not carry SI24.
                0
            };
            (codec, wire_id, packet_type, prefix, cts)
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

    fn enhanced_video(fourcc: &[u8; 4], packet: u8, track: Option<u8>, data: &[u8]) -> Vec<u8> {
        let mut body = if track.is_some() {
            vec![0x96, packet]
        } else {
            vec![0x90 | packet]
        };
        body.extend_from_slice(fourcc);
        body.extend(track);
        body.extend_from_slice(data);
        body
    }

    #[test]
    fn hevc_configuration_is_opaque_on_default_and_explicit_tracks() {
        for track in [None, Some(0), Some(7), Some(255)] {
            // Deliberately not a valid HVCC record: the transport must not decode it.
            let body = enhanced_video(b"hvc1", 0, track, &[0xde, 0xad]);
            let parsed = inspect(MediaKind::Video, &body, 42, 2).unwrap();
            assert_eq!(parsed.codec, WireCodec::Hevc);
            assert_eq!(parsed.track.wire_id, track.unwrap_or(0));
            assert_eq!(parsed.event_kind, EventKind::SequenceHeader);
            assert_eq!(&body[parsed.payload], &[0xde, 0xad]);
            assert_eq!(parsed.pts_ms, 42);
            assert_eq!(
                inspect(MediaKind::Video, &body, 42, 1),
                Err(MediaError::HeaderLimit)
            );
        }
    }

    #[test]
    fn enhanced_avc_and_hevc_signed_cts_is_separate_from_coded_payload() {
        for fourcc in [b"avc1", b"hvc1"] {
            for track in [None, Some(0), Some(12), Some(255)] {
                for offset in [-8_388_608_i32, -137, -1, 0, 137, 8_388_607] {
                    let mut payload = offset.to_be_bytes()[1..].to_vec();
                    payload.extend_from_slice(&[0xa5, 0x5a]);
                    let body = enhanced_video(fourcc, 1, track, &payload);
                    let parsed = inspect(MediaKind::Video, &body, 100, 64).unwrap();
                    assert_eq!(
                        parsed.codec,
                        if fourcc == b"hvc1" {
                            WireCodec::Hevc
                        } else {
                            WireCodec::H264
                        }
                    );
                    assert_eq!(parsed.pts_ms, 100 + i64::from(offset));
                    assert_eq!(parsed.track.wire_id, track.unwrap_or(0));
                    assert_eq!(parsed.event_kind, EventKind::Frame);
                    assert_eq!(&body[parsed.payload], &[0xa5, 0x5a]);
                }
            }
        }
    }

    #[test]
    fn enhanced_avc_and_hevc_frames_x_do_not_consume_cts_bytes() {
        for fourcc in [b"avc1", b"hvc1"] {
            for track in [None, Some(7)] {
                let body = enhanced_video(fourcc, 3, track, &[0xff, 0xff, 0xfe, 0xab]);
                let parsed = inspect(MediaKind::Video, &body, u32::MAX, 64).unwrap();
                assert_eq!(parsed.pts_ms, i64::from(u32::MAX));
                assert_eq!(&body[parsed.payload], &[0xff, 0xff, 0xfe, 0xab]);
            }
        }
    }

    #[test]
    fn multivideo_av1_preserves_payload_without_inventing_cts() {
        let body = enhanced_video(b"av01", 1, Some(12), &[0xff, 0xff, 0xff, 0x22]);
        let parsed = inspect(MediaKind::Video, &body, 100, 64).unwrap();
        assert_eq!(parsed.codec, WireCodec::Av1);
        assert_eq!(parsed.pts_ms, 100);
        assert_eq!(parsed.track.wire_id, 12);
        assert_eq!(&body[parsed.payload], &[0xff, 0xff, 0xff, 0x22]);
    }

    #[test]
    fn enhanced_video_short_headers_and_empty_frames_are_rejected() {
        for fourcc in [b"avc1", b"hvc1", b"av01"] {
            for track in [None, Some(7)] {
                for packet in [0, 1, 3] {
                    let data = if packet == 1 && fourcc != b"av01" {
                        &[0, 0, 0, 0x55][..]
                    } else {
                        &[0x55][..]
                    };
                    let body = enhanced_video(fourcc, packet, track, data);
                    for length in 0..body.len() {
                        assert!(inspect(MediaKind::Video, &body[..length], 0, 64).is_err());
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_video_modes_fourcc_and_recursive_wrappers_are_rejected() {
        for fourcc in [b"hev1", b"HVC1", b"mp4a", b"vp09"] {
            for track in [None, Some(7)] {
                let body = enhanced_video(fourcc, 0, track, &[1]);
                assert_eq!(
                    inspect(MediaKind::Video, &body, 0, 64),
                    Err(MediaError::UnsupportedWireForm)
                );
            }
        }
        for packet in [5, 6, 7, 15, 0x10, 0x20, 0xf0] {
            let body = enhanced_video(b"hvc1", packet, Some(7), &[1]);
            assert_eq!(
                inspect(MediaKind::Video, &body, 0, 64),
                Err(MediaError::UnsupportedWireForm)
            );
        }
    }

    #[test]
    fn video_metadata_and_sequence_end_do_not_consume_a_cts_field() {
        for fourcc in [b"avc1", b"hvc1", b"av01"] {
            for track in [None, Some(7)] {
                for packet in [2, 4] {
                    let body = enhanced_video(fourcc, packet, track, &[]);
                    let parsed = inspect(MediaKind::Video, &body, 33, 64).unwrap();
                    assert_eq!(parsed.pts_ms, 33);
                    assert!(parsed.payload.is_empty());
                    assert_eq!(
                        parsed.event_kind,
                        if packet == 2 {
                            EventKind::SequenceEnd
                        } else {
                            EventKind::Metadata
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn enhanced_video_rejects_reserved_frame_types_and_seek_commands() {
        for fourcc in [b"avc1", b"hvc1", b"av01"] {
            for track in [None, Some(7)] {
                for frame_type in [0, 3, 5, 6, 7] {
                    let mut body = enhanced_video(fourcc, 0, track, &[1]);
                    body[0] = (body[0] & 0x8f) | (frame_type << 4);
                    assert_eq!(
                        inspect(MediaKind::Video, &body, 0, 64),
                        Err(MediaError::UnsupportedWireForm)
                    );
                }
                // Metadata explicitly ignores FrameType in the specification.
                let mut metadata = enhanced_video(fourcc, 4, track, &[]);
                metadata[0] &= 0x8f;
                assert!(inspect(MediaKind::Video, &metadata, 0, 64).is_ok());
            }
        }
    }

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
