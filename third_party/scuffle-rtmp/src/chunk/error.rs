// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Error types for chunk processing.

/// Errors that can occur when reading or writing chunks.
#[derive(Debug, thiserror::Error)]
pub enum ChunkReadError {
    /// Invalid local resource limits.
    #[error("invalid chunk limits")]
    InvalidLimits,
    /// New message header arrived before the same chunk stream completed.
    #[error("message header replaced an incomplete message")]
    IncompleteMessageReplaced,
    /// Aggregate reservation for incomplete messages would exceed the limit.
    #[error("partial message reservation limit")]
    PartialBytesLimit,
    /// Missing previous chunk header.
    #[error("missing previous chunk header: {0}")]
    MissingPreviousChunkHeader(u32),
    /// Too many partial chunks.
    #[error("too many partial chunks")]
    TooManyPartialChunks,
    /// There are too many previous chunk headers stored in
    /// memory. The client is probably trying to DoS us.
    #[error("too many previous chunk headers")]
    TooManyPreviousChunkHeaders,
    #[error("partial chunk too large: {0}")]
    /// The length of a single chunk is larger than the max partial chunk size.
    /// The client is probably trying to DoS us.
    PartialChunkTooLarge(usize),
}
