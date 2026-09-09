use super::{ProbeResult, config::Config};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
use tokio::io::AsyncReadExt;
use uplink_media::flv::{FlvReader, FlvTag};

pub const MAX_TAG: usize = 2 * 1024 * 1024;
const MAX_SOURCE: u64 = 256 * 1024 * 1024;

pub struct Source {
    pub tags: Vec<FlvTag>,
    pub bytes: usize,
    pub video_frames: u64,
    pub audio_frames: u64,
}

impl Source {
    pub async fn read(config: &Config) -> ProbeResult<Self> {
        let file = super::open_regular(&config.source)?;
        let mut bytes = Vec::new();
        file.take(MAX_SOURCE + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Quelle kann nicht gelesen werden")?;
        if bytes.len() as u64 > MAX_SOURCE {
            return Err("Quelle überschreitet 256 MiB");
        }
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if digest != config.source_sha256.to_ascii_lowercase() {
            return Err("SHA-256 der synthetischen Quelle stimmt nicht");
        }
        let mut reader = FlvReader::new(bytes.as_slice(), MAX_TAG);
        let mut tags = Vec::new();
        while let Some(tag) = reader.next().await.map_err(|_| "Quell-FLV ist ungültig")? {
            if matches!(tag.kind(), 8 | 9) {
                tags.push(tag);
            }
            if tags.len() > 40_000 {
                return Err("Quelle überschreitet 40000 Medientags");
            }
        }
        let (video_frames, audio_frames) = validate_tags(&tags, config.repeat_ms)?;
        Ok(Self {
            tags,
            bytes: bytes.len(),
            video_frames,
            audio_frames,
        })
    }
}

fn validate_tags(tags: &[FlvTag], repeat_ms: u32) -> ProbeResult<(u64, u64)> {
    let mut last = BTreeMap::new();
    let mut headers = BTreeMap::new();
    let mut video_frames = 0;
    let mut audio_frames = 0;
    for tag in tags {
        let body = tag.body();
        let is_video = tag.kind() == 9;
        if is_video {
            if body.len() < 5 || body[0] & 0x80 == 0 || &body[1..5] != b"av01" {
                return Err("Quelle benötigt genau eine AV1-Videospur");
            }
            if body[0] & 0x0f == 2 {
                continue;
            }
            if !matches!(body[0] & 0x0f, 0 | 1 | 3) {
                return Err("AV1-Pakettyp der Quelle wird nicht unterstützt");
            }
        } else if body.len() < 2 || body[0] >> 4 != 10 || body[1] > 1 {
            return Err("Quelle benötigt genau eine klassische AAC-Spur");
        }
        if tag.is_sequence_header() {
            if headers.insert(tag.kind(), true).is_some() || tag.timestamp_ms() != 0 {
                return Err("Quelle braucht genau einen Header je Spur bei Zeitstempel null");
            }
            continue;
        }
        if !headers.contains_key(&tag.kind()) {
            return Err("Quellframe steht vor seinem Sequenzheader");
        }
        if is_video && video_frames == 0 && (body[0] >> 4) & 7 != 1 {
            return Err("Wiederholbare AV1-Quelle beginnt nicht mit einem Schlüsselbild");
        }
        if tag.timestamp_ms() >= repeat_ms
            || last
                .insert(tag.kind(), tag.timestamp_ms())
                .is_some_and(|previous| tag.timestamp_ms() <= previous)
        {
            return Err(
                "Quellzeitstempel sind nicht streng steigend oder überschreiten die Wiederholung",
            );
        }
        if is_video {
            video_frames += 1;
        } else {
            audio_frames += 1;
        }
    }
    if video_frames == 0 || audio_frames == 0 {
        return Err("Quelle enthält keine vollständigen AV1/AAC-Medien");
    }
    Ok((video_frames, audio_frames))
}

pub fn repeated(tag: &FlvTag, cycle: u32, repeat_ms: u32) -> ProbeResult<Option<FlvTag>> {
    if tag.kind() == 9 && tag.body()[0] & 0x0f == 2 || cycle > 0 && tag.is_sequence_header() {
        return Ok(None);
    }
    let timestamp = cycle
        .checked_mul(repeat_ms)
        .and_then(|base| base.checked_add(tag.timestamp_ms()))
        .ok_or("Wiederholungszeitstempel läuft über")?;
    Ok(Some(
        FlvTag::new(tag.kind(), timestamp, Arc::from(tag.body()), MAX_TAG)
            .map_err(|_| "Wiederholter Medientag ist ungültig")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(kind: u8, timestamp: u32, body: &[u8]) -> FlvTag {
        FlvTag::new(kind, timestamp, Arc::from(body), MAX_TAG).unwrap()
    }

    fn tags() -> Vec<FlvTag> {
        vec![
            tag(9, 0, b"\x90av01x"),
            tag(8, 0, b"\xaf\x00x"),
            tag(9, 0, b"\x91av01x"),
            tag(8, 0, b"\xaf\x01x"),
            tag(9, 40, b"\xa1av01x"),
            tag(8, 21, b"\xaf\x01x"),
        ]
    }

    #[test]
    fn loop_preserves_payload_and_increases_timestamps_without_repeated_headers() {
        let tags = tags();
        assert_eq!(validate_tags(&tags, 80).unwrap(), (2, 2));
        assert!(repeated(&tags[0], 1, 80).unwrap().is_none());
        let next = repeated(&tags[2], 1, 80).unwrap().unwrap();
        assert_eq!(next.timestamp_ms(), 80);
        assert_eq!(next.body(), tags[2].body());
        assert!(repeated(&tags[2], u32::MAX, 80).is_err());
    }

    #[test]
    fn rejects_regression_boundary_overlap_and_non_keyframe_start() {
        assert!(validate_tags(&tags(), 40).is_err());
        let mut duplicate = tags();
        duplicate.push(duplicate[4].clone());
        assert!(validate_tags(&duplicate, 80).is_err());
        let mut predicted = tags();
        predicted[2] = tag(9, 0, b"\xa1av01x");
        assert!(validate_tags(&predicted, 80).is_err());
    }
}
