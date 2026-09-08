// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Reading protocol control messages.

use std::io;

use super::{ProtocolControlMessageSetChunkSize, ProtocolControlMessageWindowAcknowledgementSize};

// These readers receive a complete RTMP message body, not a transport stream.
// A wrong body length is invalid protocol data, never a peer disconnect.
fn read_control_u32(data: &[u8]) -> io::Result<u32> {
    let bytes = data.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid control-message body length",
        )
    })?;
    Ok(u32::from_be_bytes(bytes))
}

impl ProtocolControlMessageSetChunkSize {
    /// Reads a [`ProtocolControlMessageSetChunkSize`] from the given data.
    pub fn read(data: &[u8]) -> io::Result<Self> {
        let chunk_size = read_control_u32(data)?;

        Ok(Self { chunk_size })
    }
}

impl ProtocolControlMessageWindowAcknowledgementSize {
    /// Reads a [`ProtocolControlMessageWindowAcknowledgementSize`] from the given data.
    pub fn read(data: &[u8]) -> io::Result<Self> {
        let acknowledgement_window_size = read_control_u32(data)?;

        Ok(Self {
            acknowledgement_window_size,
        })
    }
}

#[cfg(test)]
#[cfg_attr(all(test, coverage_nightly), coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn read_set_chunk_size() {
        let data = vec![0x00, 0x00, 0x00, 0x01];
        let chunk_size = ProtocolControlMessageSetChunkSize::read(&data).unwrap();
        assert_eq!(chunk_size.chunk_size, 1);
    }

    #[test]
    fn read_window_acknowledgement_size() {
        let data = vec![0x00, 0x00, 0x00, 0x01];
        let window_acknowledgement_size =
            ProtocolControlMessageWindowAcknowledgementSize::read(&data).unwrap();
        assert_eq!(window_acknowledgement_size.acknowledgement_window_size, 1);
    }

    #[test]
    fn control_bodies_require_exactly_four_bytes() {
        for length in [0, 1, 2, 3, 5, 6, 7, 8] {
            let body = vec![0; length];
            assert_eq!(
                ProtocolControlMessageSetChunkSize::read(&body)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                ProtocolControlMessageWindowAcknowledgementSize::read(&body)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
}
