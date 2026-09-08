// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Local resource and deadline policy for an RTMP connection.

use super::ServerSessionError;
use crate::chunk::reader::ChunkReaderLimits;
use std::time::Duration;

/// Limits are validated before network processing; they are not product presets.
#[derive(Clone, Copy, Debug)]
pub struct ServerSessionLimits {
    /// Chunk/message accumulation limits.
    pub chunk: ChunkReaderLimits,
    /// AMF decoder limits, including the Serde path.
    pub amf: scuffle_amf0::DecodeLimits,
    /// Maximum unread plaintext bytes.
    pub max_read_buffer_bytes: usize,
    /// Maximum serialized response bytes awaiting transport flush.
    pub max_write_buffer_bytes: usize,
    /// Maximum simultaneous authorized publishing streams.
    pub max_publishing_streams: usize,
    /// Yield to the runtime after this many complete messages.
    pub max_messages_per_turn: usize,
    /// Absolute RTMP handshake duration (TLS is the caller's responsibility).
    pub handshake_timeout: Duration,
    /// Absolute interval from RTMP handshake completion to connect.
    pub connect_timeout: Duration,
    /// Absolute interval from RTMP handshake completion to first publish.
    pub publish_timeout: Duration,
    /// Maximum wait for one incoming read.
    pub read_timeout: Duration,
    /// Maximum combined write and flush duration.
    pub write_timeout: Duration,
    /// Maximum complete message processing and callback duration.
    pub handler_timeout: Duration,
}

impl Default for ServerSessionLimits {
    fn default() -> Self {
        Self {
            chunk: ChunkReaderLimits::default(),
            amf: scuffle_amf0::DecodeLimits::default(),
            max_read_buffer_bytes: 128 * 1024,
            max_write_buffer_bytes: 64 * 1024,
            max_publishing_streams: 1,
            max_messages_per_turn: 32,
            handshake_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(5),
            publish_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(3),
            write_timeout: Duration::from_secs(2),
            handler_timeout: Duration::from_secs(2),
        }
    }
}

impl ServerSessionLimits {
    /// Reject inconsistent or unbounded local policy before constructing parser state.
    pub fn validate(&self) -> Result<(), ServerSessionError> {
        self.chunk
            .validate()
            .map_err(|_| ServerSessionError::InvalidLimits("chunk limits"))?;
        if self.max_read_buffer_bytes < self.chunk.max_chunk_size + 18
            || self.max_write_buffer_bytes < 4096
            || self.max_publishing_streams == 0
            || self.max_messages_per_turn == 0
            || self.max_messages_per_turn > 1024
            || self.amf.max_input_bytes == 0
            || self.amf.max_string_bytes == 0
            || self.amf.max_container_entries == 0
            || self.amf.max_total_values == 0
            || self.amf.max_depth == 0
            || self.amf.max_depth > 128
            || self.chunk.max_command_bytes > self.amf.max_input_bytes
            || [
                self.handshake_timeout,
                self.connect_timeout,
                self.publish_timeout,
                self.read_timeout,
                self.write_timeout,
                self.handler_timeout,
            ]
            .iter()
            .any(|t| t.is_zero() || *t > Duration::from_secs(86_400))
        {
            return Err(ServerSessionError::InvalidLimits("session limits"));
        }
        Ok(())
    }
}
