//! Isolierter Offline-Versuch, kein Produktionsparser.

use bytes::Bytes;
use scuffle_flv::{
    audio::{
        AudioData,
        body::{
            AudioTagBody,
            enhanced::{AudioPacket, ExAudioTagBody},
            legacy::{LegacyAudioTagBody, aac::AacAudioData},
        },
        header::enhanced::AudioFourCc,
    },
    video::{
        VideoData,
        body::{
            VideoTagBody,
            enhanced::{
                ExVideoTagBody, VideoPacket, VideoPacketCodedFrames, VideoPacketSequenceStart,
            },
            legacy::LegacyVideoTagBody,
        },
        header::{
            VideoTagHeaderData,
            enhanced::VideoFourCc,
            legacy::{LegacyVideoTagHeader, LegacyVideoTagHeaderAvcPacket},
        },
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    path::Path,
};

pub const FILE_MAX: usize = 2 * 1024 * 1024;
pub const TAG_MAX: usize = 64 * 1024;
pub const HEADER_MAX: usize = 4096;
pub const TAGS_MAX: usize = 4096;
pub const TRACKS_MAX: usize = 16;
// Ausschließlich das kurze, leere ColorInfo-Objekt unserer FFmpeg-Testquelle.
// Beliebige fremde AMF-Bäume bleiben vor Scuffle abgewiesen.
const FIXTURE_COLOR_INFO: &[u8] =
    b"\xd4av01\x02\x00\x09colorInfo\x03\x00\x0bcolorConfig\x03\x00\x00\x09\x00\x00\x09";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Video,
    Audio,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(pub Kind, pub u8);
#[derive(Debug, Default)]
pub struct Track {
    pub headers: Vec<String>,
    pub count: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Packet {
    pub key: Key,
    pub dts: i64,
    pub pts: i64,
    pub size: usize,
    pub hash: String,
}
#[derive(Debug, Default)]
pub struct Report {
    pub tracks: BTreeMap<Key, Track>,
    pub packets: Vec<Packet>,
    pub demux_calls: usize,
    pub color_metadata: usize,
}

fn hash(data: &[u8]) -> String {
    format!("SHA256:{:x}", Sha256::digest(data))
}
fn u24(b: &[u8]) -> usize {
    (usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2])
}
fn signed_cts(value: u32) -> i64 {
    i64::from(((value << 8) as i32) >> 8)
}

pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut input = file.take((FILE_MAX + 1) as u64);
    let mut data = vec![0; FILE_MAX + 1];
    let mut used = 0;
    while used < data.len() {
        let n = input.read(&mut data[used..]).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        used += n;
    }
    if used > FILE_MAX {
        return Err("file limit".into());
    }
    data.truncate(used);
    Ok(data)
}

impl Report {
    fn header(&mut self, key: Key, data: &[u8]) -> Result<()> {
        if data.is_empty() || data.len() > HEADER_MAX {
            return Err("sequence header limit".into());
        }
        if !self.tracks.contains_key(&key) && self.tracks.len() >= TRACKS_MAX {
            return Err("track limit".into());
        }
        let track = self.tracks.entry(key).or_default();
        if track.headers.len() >= TAGS_MAX {
            return Err("header count limit".into());
        }
        track.headers.push(hash(data));
        Ok(())
    }
    fn frame(&mut self, key: Key, dts: u32, cts: i64, data: &[u8]) -> Result<()> {
        let track = self
            .tracks
            .get_mut(&key)
            .ok_or("frame without sequence header")?;
        if self.packets.len() >= TAGS_MAX {
            return Err("packet limit".into());
        }
        track.count += 1;
        self.packets.push(Packet {
            key,
            dts: i64::from(dts),
            pts: i64::from(dts) + cts,
            size: data.len(),
            hash: hash(data),
        });
        Ok(())
    }
}

/// Begrenzte OneTrack-Teilmenge; kein allgemeines AMF oder ModEx.
/// Einzige AMF-Ausnahme sind die exakten leeren Testmetadaten.
fn preflight(kind: Kind, data: &[u8]) -> Result<()> {
    if data.is_empty() || data.len() > TAG_MAX {
        return Err("tag limit".into());
    }
    if kind == Kind::Video && data[0] & 0x8f == 0x84 {
        return if data == FIXTURE_COLOR_INFO {
            Ok(())
        } else {
            Err("metadata outside exact test fixture".into())
        };
    }
    let (packet, prefix) = match kind {
        Kind::Audio if data[0] >> 4 == 10 => {
            if data.len() < 3 || data[1] > 1 {
                return Err("legacy AAC header".into());
            }
            (data[1], 2)
        }
        Kind::Audio => {
            if data.len() < 7 || data[0] != 0x95 || data[1] >> 4 != 0 {
                return Err("outside audio OneTrack subset".into());
            }
            if &data[2..6] != b"mp4a" || !matches!(data[1], 0 | 1 | 2 | 4) {
                return Err("audio codec or packet type".into());
            }
            (data[1], 7)
        }
        Kind::Video if data[0] & 0x80 == 0 => {
            if data.len() < 5 || data[0] & 15 != 7 || data[1] > 2 {
                return Err("legacy AVC header".into());
            }
            // Nur das getestete Baseline-Profil. Andere Profile können einen
            // Extended-SPS-Parser mit nachgewiesenem Exp-Golomb-Panic erreichen.
            if data[1] == 0 && data.get(6) != Some(&66) {
                return Err("AVC configuration outside baseline fixture".into());
            }
            (data[1], 5)
        }
        Kind::Video => {
            if data.len() < 5 || &data[1..5] != b"av01" || data[0] & 15 > 3 {
                return Err("outside video AV1 subset".into());
            }
            (data[0] & 15, 5)
        }
    };
    if packet == 0 && (data.len() <= prefix || data.len() > HEADER_MAX) {
        return Err("sequence header limit".into());
    }
    Ok(())
}

pub fn demux_media(report: &mut Report, kind: Kind, data: &[u8], dts: u32) -> Result<()> {
    preflight(kind, data)?;
    let mut reader = Cursor::new(Bytes::copy_from_slice(data));
    report.demux_calls += 1;
    match kind {
        Kind::Audio => match AudioData::demux(&mut reader)
            .map_err(|e| e.to_string())?
            .body
        {
            AudioTagBody::Legacy(LegacyAudioTagBody::Aac(AacAudioData::SequenceHeader(b))) => {
                report.header(Key(kind, 0), &b)?
            }
            AudioTagBody::Legacy(LegacyAudioTagBody::Aac(AacAudioData::Raw(b))) => {
                report.frame(Key(kind, 0), dts, 0, &b)?
            }
            AudioTagBody::Enhanced(ExAudioTagBody::ManyTracks(tracks)) => {
                if tracks.len() != 1 {
                    return Err("unexpected OneTrack count".into());
                }
                for track in tracks {
                    if track.audio_four_cc != AudioFourCc::Aac {
                        return Err("unexpected audio codec".into());
                    }
                    let key = Key(kind, track.audio_track_id);
                    match track.packet {
                        AudioPacket::SequenceStart { header_data } => {
                            report.header(key, &header_data)?
                        }
                        AudioPacket::CodedFrames { data } => report.frame(key, dts, 0, &data)?,
                        AudioPacket::MultichannelConfig { .. } | AudioPacket::SequenceEnd => {}
                        _ => return Err("unknown audio packet".into()),
                    }
                }
            }
            _ => return Err("unexpected audio form".into()),
        },
        Kind::Video => {
            let video = VideoData::demux(&mut reader).map_err(|e| e.to_string())?;
            match (video.header.data, video.body) {
                (_, VideoTagBody::Legacy(LegacyVideoTagBody::AvcVideoPacketSeqHdr(config))) => {
                    let mut b = Vec::new();
                    config.build(&mut b).map_err(|e| e.to_string())?;
                    report.header(Key(kind, 0), &b)?;
                }
                (
                    VideoTagHeaderData::Legacy(LegacyVideoTagHeader::AvcPacket(
                        LegacyVideoTagHeaderAvcPacket::Nalu {
                            composition_time_offset,
                        },
                    )),
                    VideoTagBody::Legacy(LegacyVideoTagBody::Other { data }),
                ) => report.frame(
                    Key(kind, 0),
                    dts,
                    signed_cts(composition_time_offset),
                    &data,
                )?,
                (
                    VideoTagHeaderData::Legacy(LegacyVideoTagHeader::AvcPacket(
                        LegacyVideoTagHeaderAvcPacket::EndOfSequence,
                    )),
                    _,
                ) => {}
                (
                    _,
                    VideoTagBody::Enhanced(ExVideoTagBody::NoMultitrack {
                        video_four_cc,
                        packet,
                    }),
                ) if video_four_cc == VideoFourCc::Av1 => match packet {
                    VideoPacket::SequenceStart(VideoPacketSequenceStart::Av1(config)) => {
                        let mut b = Vec::new();
                        config.mux(&mut b).map_err(|e| e.to_string())?;
                        report.header(Key(kind, 0), &b)?;
                    }
                    VideoPacket::CodedFrames(VideoPacketCodedFrames::Other(b))
                    | VideoPacket::CodedFramesX { data: b } => {
                        report.frame(Key(kind, 0), dts, 0, &b)?
                    }
                    VideoPacket::SequenceEnd => {}
                    VideoPacket::Metadata(entries) if entries.len() == 1 => {
                        report.color_metadata += 1;
                    }
                    _ => return Err("unexpected enhanced video form".into()),
                },
                _ => return Err("unexpected video form".into()),
            }
        }
    }
    if reader.position() != data.len() as u64 {
        return Err("unconsumed bytes".into());
    }
    Ok(())
}

pub fn analyze(data: &[u8]) -> Result<Report> {
    if data.len() > FILE_MAX {
        return Err("file limit".into());
    }
    if data.len() < 13 || &data[..3] != b"FLV" || data[3] != 1 || data[4] & !5 != 0 {
        return Err("invalid FLV header".into());
    }
    let offset = u32::from_be_bytes(data[5..9].try_into().map_err(|_| "offset")?) as usize;
    if !(9..=HEADER_MAX).contains(&offset) || offset + 4 > data.len() {
        return Err("FLV header size".into());
    }
    scuffle_flv::header::FlvHeader::demux(&mut Cursor::new(Bytes::copy_from_slice(
        &data[..offset],
    )))
    .map_err(|e| e.to_string())?;
    if data[offset..offset + 4] != [0; 4] {
        return Err("first previous tag size".into());
    }
    let mut pos = offset + 4;
    let mut count = 0;
    let mut report = Report::default();
    while pos < data.len() {
        count += 1;
        if count > TAGS_MAX || data.len() - pos < 15 {
            return Err("tag count or truncated header".into());
        }
        let raw = &data[pos..];
        let size = u24(&raw[1..4]);
        if size > TAG_MAX {
            return Err("tag limit before allocation".into());
        }
        if size + 15 > raw.len() || raw[8..11] != [0; 3] {
            return Err("truncated body or FLV StreamID".into());
        }
        let prev = u32::from_be_bytes(raw[11 + size..15 + size].try_into().map_err(|_| "prev")?);
        if prev as usize != size + 11 {
            return Err("previous tag size mismatch".into());
        }
        let dts = u24(&raw[4..7]) as u32 | (u32::from(raw[7]) << 24);
        match raw[0] {
            8 => demux_media(&mut report, Kind::Audio, &raw[11..11 + size], dts)?,
            9 => demux_media(&mut report, Kind::Video, &raw[11..11 + size], dts)?,
            18 => {} // AMF-Baum bewusst nicht durch Scuffle parsen.
            _ => return Err("unknown or encrypted tag".into()),
        }
        pos += size + 15;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_key(index: u64) -> Key {
        match index {
            0 => Key(Kind::Video, 0),
            1 => Key(Kind::Audio, 0),
            2 => Key(Kind::Audio, 1),
            _ => panic!("fixture index"),
        }
    }
    fn actual_file(codec: &str) {
        let path = format!("fixtures/{codec}.flv");
        let report = analyze(&read_bounded(Path::new(&path)).unwrap()).unwrap();
        assert_eq!(report.tracks.len(), 3);
        for (key, count) in [
            (Key(Kind::Video, 0), 50),
            (Key(Kind::Audio, 0), 95),
            (Key(Kind::Audio, 1), 95),
        ] {
            assert_eq!(report.tracks[&key].headers.len(), 1);
            assert_eq!(report.tracks[&key].count, count);
        }
        let reference_path = format!("fixtures/{codec}.ffprobe.json");
        let expected: serde_json::Value =
            serde_json::from_slice(&read_bounded(Path::new(&reference_path)).unwrap()).unwrap();
        let packets = expected["packets"].as_array().unwrap();
        assert_eq!(report.packets.len(), packets.len());
        for (actual, p) in report.packets.iter().zip(packets) {
            assert_eq!(actual.key, fixture_key(p["stream_index"].as_u64().unwrap()));
            assert_eq!(actual.dts, p["dts"].as_i64().unwrap());
            assert_eq!(actual.pts, p["pts"].as_i64().unwrap());
            assert_eq!(
                actual.size,
                p["size"].as_str().unwrap().parse::<usize>().unwrap()
            );
            assert_eq!(actual.hash, p["data_hash"].as_str().unwrap());
        }
        for s in expected["streams"].as_array().unwrap() {
            assert_eq!(
                report.tracks[&fixture_key(s["index"].as_u64().unwrap())].headers[0],
                s["extradata_hash"].as_str().unwrap()
            );
        }
        let numeric_ids: std::collections::BTreeSet<_> =
            report.tracks.keys().map(|k| k.1).collect();
        assert_eq!(
            numeric_ids.len(),
            2,
            "three tracks collide under numeric-only ID"
        );
        assert_eq!(report.color_metadata, usize::from(codec == "av1"));
        println!(
            "{codec}: 3 track keys, 3 matching headers, 240 matching packets; {} Scuffle calls; {} empty ColorInfo",
            report.demux_calls, report.color_metadata
        );
    }
    #[test]
    fn av1_two_aac_match_ffprobe() {
        actual_file("av1");
    }
    #[test]
    fn h264_two_aac_match_ffprobe() {
        actual_file("h264");
    }
    #[test]
    fn fixture_assets_match_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_slice(&read_bounded(Path::new("fixtures/manifest.json")).unwrap())
                .unwrap();
        for entry in manifest["files"].as_array().unwrap() {
            let name = entry["path"].as_str().unwrap();
            assert!(matches!(
                name,
                "av1.flv" | "h264.flv" | "av1.ffprobe.json" | "h264.ffprobe.json"
            ));
            let data = read_bounded(Path::new(&format!("fixtures/{name}"))).unwrap();
            assert_eq!(data.len() as u64, entry["bytes"].as_u64().unwrap());
            assert_eq!(
                hash(&data),
                format!("SHA256:{}", entry["sha256"].as_str().unwrap())
            );
        }
    }
    #[test]
    fn limits_and_unknown_forms_apply_before_scuffle() {
        for (kind, data) in [
            (Kind::Audio, vec![0; TAG_MAX + 1]),
            (Kind::Audio, vec![0x95, 1, b'n', b'o', b'p', b'e', 1]),
            (Kind::Audio, vec![0x95, 0x11, b'm', b'p', b'4', b'a', 1]),
            (Kind::Audio, vec![0x95, 15, b'm', b'p', b'4', b'a', 1]),
            (Kind::Audio, vec![0xaf, 7, 0]),
            (Kind::Video, vec![0x97, 0, 0]),
        ] {
            let mut report = Report::default();
            assert!(demux_media(&mut report, kind, &data, 0).is_err());
            assert_eq!(report.demux_calls, 0);
        }
        let mut data = vec![0; HEADER_MAX + 1];
        data[0] = 0xaf;
        let mut report = Report::default();
        assert!(demux_media(&mut report, Kind::Audio, &data, 0).is_err());
        assert_eq!(report.demux_calls, 0);
    }
    #[test]
    fn broken_codec_headers_error_without_panic() {
        for (kind, data) in [
            (Kind::Audio, vec![0x95, 4, b'm', b'p', b'4', b'a', 1]),
            (Kind::Video, vec![0x90, b'a', b'v', b'0', b'1', 0, 0, 0, 0]),
            (Kind::Video, vec![0x17, 0, 0, 0, 0, 1, 66]),
        ] {
            let mut report = Report::default();
            assert!(demux_media(&mut report, kind, &data, 0).is_err());
            assert_eq!(report.demux_calls, 1);
        }
    }
    #[test]
    fn aac_sequence_header_is_opaque() {
        let parsed =
            AudioData::demux(&mut Cursor::new(Bytes::from_static(&[0xaf, 0, 0, 0]))).unwrap();
        assert!(
            matches!(parsed.body, AudioTagBody::Legacy(LegacyAudioTagBody::Aac(AacAudioData::SequenceHeader(ref b))) if b.as_ref() == [0,0])
        );
    }
    #[test]
    fn scuffle_preserves_unknown_audio_fourcc() {
        let parsed = AudioData::demux(&mut Cursor::new(Bytes::from_static(&[
            0x95, 1, b'n', b'o', b'p', b'e', 1, 42,
        ])))
        .unwrap();
        let AudioTagBody::Enhanced(ExAudioTagBody::ManyTracks(tracks)) = parsed.body else {
            panic!("expected enhanced audio track");
        };
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].audio_four_cc, AudioFourCc::from(*b"nope"));
    }
    #[test]
    fn negative_legacy_avc_cts_needs_sign_extension() {
        let video = VideoData::demux(&mut Cursor::new(Bytes::from_static(&[
            0x27, 1, 255, 255, 255, 42,
        ])))
        .unwrap();
        let VideoTagHeaderData::Legacy(LegacyVideoTagHeader::AvcPacket(
            LegacyVideoTagHeaderAvcPacket::Nalu {
                composition_time_offset,
            },
        )) = video.header.data
        else {
            panic!("AVC NALU")
        };
        assert_eq!(composition_time_offset, 16_777_215);
        assert_eq!(signed_cts(composition_time_offset), -1);
    }
    #[test]
    fn frames_require_sequence_header() {
        let mut report = Report::default();
        assert!(demux_media(&mut report, Kind::Audio, &[0xaf, 1, 42], 0).is_err());
        assert!(report.tracks.is_empty());
    }
    const EXTENDED_SPS_OVERFLOW: &[u8] = &[
        0x17, 0, 0, 0, 0, 1, 100, 0, 40, 0xff, 0xe0, 0, 0xfc, 0xf8, 0xf8, 1, 0, 17, 0, 0, 0, 0, 0,
        0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    #[test]
    fn extended_sps_is_rejected_before_scuffle() {
        let mut report = Report::default();
        assert_eq!(EXTENDED_SPS_OVERFLOW.len(), 35);
        assert!(demux_media(&mut report, Kind::Video, EXTENDED_SPS_OVERFLOW, 0).is_err());
        assert_eq!(report.demux_calls, 0);
    }
    #[test]
    #[cfg(debug_assertions)]
    fn direct_scuffle_extended_sps_panics_with_overflow_checks() {
        // Bibliothekslücke belegen; kein Catch-Unwind-Konzept für Produktion.
        let observed = std::panic::catch_unwind(|| {
            VideoData::demux(&mut Cursor::new(Bytes::from_static(EXTENDED_SPS_OVERFLOW)))
        });
        assert!(
            observed.is_err(),
            "pinned dependency no longer reproduces the panic"
        );
    }
    #[test]
    #[cfg(not(debug_assertions))]
    fn direct_scuffle_extended_sps_returns_range_error_in_release() {
        let observed = std::panic::catch_unwind(|| {
            VideoData::demux(&mut Cursor::new(Bytes::from_static(EXTENDED_SPS_OVERFLOW)))
        });
        let parsed = observed.expect("release fixture must not panic");
        assert_eq!(
            parsed.unwrap_err().to_string(),
            "io: chroma_format_idc is out of range [0, 3]: 18446744073709551615"
        );
    }
    #[test]
    fn damaged_envelope_and_oversized_claim_fail() {
        let original = read_bounded(Path::new("fixtures/av1.flv")).unwrap();
        let mut bad = original.clone();
        bad[0] = 0;
        assert!(analyze(&bad).is_err());
        let mut bad = original.clone();
        bad[5..9].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(analyze(&bad).is_err());
        let mut bad = original.clone();
        bad[14..17].copy_from_slice(&[255; 3]);
        assert_eq!(analyze(&bad).unwrap_err(), "tag limit before allocation");
        let mut bad = original.clone();
        bad[13] |= 0x20;
        assert!(analyze(&bad).is_err());
        let mut bad = original.clone();
        bad.pop();
        assert!(analyze(&bad).is_err());
        let mut bad = original;
        bad[12] = 1;
        assert!(analyze(&bad).is_err());
    }
    #[test]
    fn file_and_track_limits_apply() {
        assert!(analyze(&vec![0; FILE_MAX + 1]).is_err());
        let mut report = Report::default();
        for id in 0..TRACKS_MAX as u8 {
            report.header(Key(Kind::Audio, id), &[1, 2]).unwrap();
        }
        assert!(
            report
                .header(Key(Kind::Audio, TRACKS_MAX as u8), &[1, 2])
                .is_err()
        );
        assert_eq!(report.tracks.len(), TRACKS_MAX);
    }
}
