use crate::{MediaError, Result};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uplink_ingest::{MediaEvent, MediaKind};

pub const HEADER: &[u8; 13] = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00";

/// Opaque, begrenzte FLV-Nutzdaten. Kein SPS-, AVCC- oder AMF-Decoder.
#[derive(Debug, Clone)]
pub struct FlvTag {
    kind: u8,
    timestamp_ms: u32,
    body: Arc<[u8]>,
}

impl FlvTag {
    pub fn new(kind: u8, timestamp_ms: u32, body: Arc<[u8]>, max_bytes: usize) -> Result<Self> {
        if !matches!(kind, 8 | 9 | 18)
            || body.is_empty()
            || body.len() > max_bytes
            || body.len() > 0xff_ffff
        {
            return Err(MediaError::InvalidMedia);
        }
        Ok(Self {
            kind,
            timestamp_ms,
            body,
        })
    }
    pub fn from_event(event: &MediaEvent, max_bytes: usize) -> Result<Self> {
        Self::new(
            match event.identity.track.kind {
                MediaKind::Audio => 8,
                MediaKind::Video => 9,
            },
            event.dts_ms,
            Arc::from(event.wire_body()),
            max_bytes,
        )
    }
    pub fn kind(&self) -> u8 {
        self.kind
    }
    pub fn timestamp_ms(&self) -> u32 {
        self.timestamp_ms
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn wire_len(&self) -> usize {
        self.body.len() + 15
    }
    pub fn is_sequence_header(&self) -> bool {
        match self.kind {
            8 if self.body[0] >> 4 == 10 => self.body.get(1) == Some(&0),
            8 if self.body[0] == 0x95 => self.body.get(1) == Some(&0),
            9 if self.body[0] & 0x80 == 0 => self.body.get(1) == Some(&0),
            9 if self.body[0] & 0x0f == 6 => self.body.get(1) == Some(&0),
            9 => self.body[0] & 0x0f == 0,
            _ => false,
        }
    }
    pub fn audio_track(&self) -> Result<Option<u8>> {
        if self.kind != 8 {
            return Ok(None);
        }
        if self.body[0] >> 4 == 10 && self.body.len() >= 2 {
            return Ok(Some(0));
        }
        if self.body[0] == 0x95 && self.body.len() >= 7 && self.body[1] >> 4 == 0 {
            return Ok(Some(self.body[6]));
        }
        Err(MediaError::UnsupportedProfile)
    }
    pub fn with_audio_track(&self, track: u8, max_bytes: usize) -> Result<Self> {
        if self.kind != 8 {
            return Err(MediaError::InvalidMedia);
        }
        self.audio_track()?;
        let mut body = Vec::with_capacity(self.body.len() + 5);
        if self.body[0] >> 4 == 10 {
            if track == 0 {
                return Ok(self.clone());
            }
            body.extend_from_slice(&[0x95, self.body[1]]);
            body.extend_from_slice(b"mp4a");
            body.push(track);
            body.extend_from_slice(&self.body[2..]);
        } else {
            body.extend_from_slice(&self.body);
            body[6] = track;
        }
        Self::new(8, self.timestamp_ms, body.into(), max_bytes)
    }
    pub fn with_video_track(&self, track: u8, max_bytes: usize) -> Result<Self> {
        if self.kind != 9 || self.body.len() < 5 {
            return Err(MediaError::InvalidMedia);
        }
        let mut body = Vec::with_capacity(self.body.len() + 6);
        let frame_type = self.body[0] & 0x70;
        body.push(0x80 | frame_type | 6); // Enhanced VideoPacketType::Multitrack
        if self.body[0] & 0x80 == 0 {
            if self.body[0] & 15 != 7 || self.body[1] > 2 {
                return Err(MediaError::UnsupportedProfile);
            }
            if track == 0 {
                return Ok(self.clone());
            }
            body.push(self.body[1]); // OneTrack, ursprünglicher Pakettyp
            body.extend_from_slice(b"avc1");
            body.push(track);
            if self.body[1] == 1 {
                body.extend_from_slice(&self.body[2..5]);
            }
            body.extend_from_slice(&self.body[5..]);
        } else {
            let packet_type = self.body[0] & 15;
            if packet_type == 6 {
                if self.body.len() < 7
                    || self.body[1] > 4
                    || ![b"avc1", b"hvc1", b"av01"].contains(
                        &self.body[2..6]
                            .try_into()
                            .map_err(|_| MediaError::InvalidMedia)?,
                    )
                {
                    return Err(MediaError::UnsupportedProfile);
                }
                let mut remapped = self.body.to_vec();
                remapped[6] = track;
                return Self::new(9, self.timestamp_ms, remapped.into(), max_bytes);
            }
            if packet_type > 4 {
                return Err(MediaError::UnsupportedProfile);
            }
            if track == 0 {
                return Ok(self.clone());
            }
            body.push(packet_type);
            body.extend_from_slice(&self.body[1..5]);
            body.push(track);
            body.extend_from_slice(&self.body[5..]);
        }
        Self::new(9, self.timestamp_ms, body.into(), max_bytes)
    }
    pub async fn write_to(&self, writer: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        let mut header = [0_u8; 11];
        header[0] = self.kind;
        let length = self.body.len() as u32;
        header[1..4].copy_from_slice(&length.to_be_bytes()[1..]);
        header[4..7].copy_from_slice(&self.timestamp_ms.to_be_bytes()[1..]);
        header[7] = self.timestamp_ms.to_be_bytes()[0];
        writer
            .write_all(&header)
            .await
            .map_err(|_| MediaError::Io)?;
        writer
            .write_all(&self.body)
            .await
            .map_err(|_| MediaError::Io)?;
        writer
            .write_all(&(length + 11).to_be_bytes())
            .await
            .map_err(|_| MediaError::Io)?;
        Ok(())
    }
}

pub struct FlvReader<R> {
    reader: R,
    max_tag_bytes: usize,
    started: bool,
}
impl<R: AsyncRead + Unpin> FlvReader<R> {
    pub fn new(reader: R, max_tag_bytes: usize) -> Self {
        Self {
            reader,
            max_tag_bytes,
            started: false,
        }
    }
    pub async fn next(&mut self) -> Result<Option<FlvTag>> {
        if !self.started {
            let mut header = [0_u8; 9];
            self.reader
                .read_exact(&mut header)
                .await
                .map_err(|_| MediaError::InvalidMedia)?;
            let offset = u32::from_be_bytes(
                header[5..9]
                    .try_into()
                    .map_err(|_| MediaError::InvalidMedia)?,
            ) as usize;
            if &header[..4] != b"FLV\x01" || !(9..=4096).contains(&offset) || header[4] & !5 != 0 {
                return Err(MediaError::InvalidMedia);
            }
            let mut remainder = vec![0; offset - 9 + 4];
            self.reader
                .read_exact(&mut remainder)
                .await
                .map_err(|_| MediaError::InvalidMedia)?;
            if remainder[remainder.len() - 4..] != [0, 0, 0, 0] {
                return Err(MediaError::InvalidMedia);
            }
            self.started = true;
        }
        let mut header = [0_u8; 11];
        if self
            .reader
            .read(&mut header[..1])
            .await
            .map_err(|_| MediaError::Io)?
            == 0
        {
            return Ok(None);
        }
        self.reader
            .read_exact(&mut header[1..])
            .await
            .map_err(|_| MediaError::InvalidMedia)?;
        let length = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        if length == 0
            || length > self.max_tag_bytes
            || header[8..11] != [0, 0, 0]
            || !matches!(header[0], 8 | 9 | 18)
        {
            return Err(MediaError::InvalidMedia);
        }
        let timestamp = u32::from_be_bytes([header[7], header[4], header[5], header[6]]);
        let mut body = vec![0; length];
        self.reader
            .read_exact(&mut body)
            .await
            .map_err(|_| MediaError::InvalidMedia)?;
        let mut previous = [0_u8; 4];
        self.reader
            .read_exact(&mut previous)
            .await
            .map_err(|_| MediaError::InvalidMedia)?;
        if u32::from_be_bytes(previous) as usize != length + 11 {
            return Err(MediaError::InvalidMedia);
        }
        Ok(Some(FlvTag::new(
            header[0],
            timestamp,
            body.into(),
            self.max_tag_bytes,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounded_roundtrip_preserves_extended_timestamp_and_payload() {
        let tag = FlvTag::new(
            9,
            0xfe12_3456,
            Arc::from(&b"\x17\x01\xff\xff\xffabc"[..]),
            1024,
        )
        .unwrap();
        let mut bytes = HEADER.to_vec();
        tag.write_to(&mut bytes).await.unwrap();
        let mut reader = FlvReader::new(bytes.as_slice(), 1024);
        let restored = reader.next().await.unwrap().unwrap();
        assert_eq!(restored.timestamp_ms(), tag.timestamp_ms());
        assert_eq!(restored.body(), tag.body());
        assert!(reader.next().await.unwrap().is_none());
        let mut reader = FlvReader::new(bytes.as_slice(), 2);
        assert!(reader.next().await.is_err());
    }
    #[test]
    fn role_remap_preserves_opaque_aac_data_and_video_cts() {
        let audio = FlvTag::new(8, 0, Arc::from(&b"\xaf\x00\x11\x90"[..]), 64).unwrap();
        let changed = audio.with_audio_track(7, 64).unwrap();
        assert_eq!(changed.audio_track().unwrap(), Some(7));
        assert_eq!(&changed.body()[7..], &audio.body()[2..]);
        let video = FlvTag::new(9, 10, Arc::from(&b"\x27\x01\xff\xff\xffabc"[..]), 64).unwrap();
        assert_eq!(
            video.with_video_track(4, 64).unwrap().body(),
            b"\xa6\x01avc1\x04\xff\xff\xffabc"
        );
    }
    #[test]
    fn existing_multivideo_remaps_only_wire_id_and_preserves_cts() {
        let tag = FlvTag::new(
            9,
            137,
            Arc::from(&b"\xa6\x01hvc1\x07\xff\xff\xfeopaque"[..]),
            64,
        )
        .unwrap();
        assert_eq!(
            tag.with_video_track(12, 64).unwrap().body(),
            b"\xa6\x01hvc1\x0c\xff\xff\xfeopaque"
        );
        let header = FlvTag::new(9, 0, Arc::from(&b"\x96\x00hvc1\x07opaque"[..]), 64).unwrap();
        assert!(header.is_sequence_header());
        let legacy =
            FlvTag::new(9, 137, Arc::from(&b"\x27\x01\xff\xff\xfeopaque"[..]), 64).unwrap();
        assert_eq!(
            legacy.with_video_track(0, 64).unwrap().body(),
            legacy.body()
        );
    }
}
