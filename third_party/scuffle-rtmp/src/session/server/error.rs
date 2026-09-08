// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Error type for server sessions.

/// Errors that can occur during a server session.
#[derive(Debug, thiserror::Error)]
pub enum ServerSessionError {
    /// Invalid local policy, without input data in the error.
    #[error("invalid session limits: {0}")]
    InvalidLimits(&'static str),
    /// The handler denied an operation. Details remain with the caller.
    #[error("handler rejected the operation")]
    HandlerRejected,
    /// A peer attempted to use an unauthorized media stream.
    #[error("media arrived before authorized publish")]
    MediaBeforePublish,
    /// Invalid or duplicate publishing stream.
    #[error("invalid publishing stream")]
    InvalidPublish,
    /// A configured buffer or stream limit was reached.
    #[error("session resource limit reached")]
    ResourceLimit,
    /// An acknowledgement window of zero cannot be used in RTMP accounting.
    #[error("acknowledgement window must be nonzero")]
    InvalidAcknowledgementWindow,
    /// Timeout.
    #[error("timeout: {0}")]
    Timeout(#[from] tokio::time::error::Elapsed),
    /// Received publish command before connect command.
    #[error("received publish command before connect command")]
    PublishBeforeConnect,
    /// Play not supported.
    #[error("play not supported")]
    PlayNotSupported,
    /// Invalid chunk size.
    #[error("invalid chunk size: {0}")]
    InvalidChunkSize(usize),
}
