// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Command error type.

/// Errors that can occur when processing command messages.
#[derive(thiserror::Error)]
pub enum CommandError {
    /// Amf0 error.
    #[error("invalid AMF command")]
    Amf0(#[from] scuffle_amf0::Amf0Error),
    /// Received an invalid onStatus info object.
    #[error("invalid onStatus info object")]
    InvalidOnStatusInfoObject,
    /// The RTMP client is not implemented yet.
    #[error("the rtmp client is not implemented yet")]
    NoClientImplementation,
}

// Serde custom errors can include peer strings such as publish credentials.
// Keep underlying errors available for classification, never reflect them in Debug.
impl std::fmt::Debug for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amf0(_) => f.write_str("Amf0(<redacted>)"),
            Self::InvalidOnStatusInfoObject => f.write_str("InvalidOnStatusInfoObject"),
            Self::NoClientImplementation => f.write_str("NoClientImplementation"),
        }
    }
}
