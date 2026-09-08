// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Complex handshake error type.

/// Errors that can occur during the complex handshake.
#[derive(Debug, thiserror::Error)]
pub enum ComplexHandshakeError {
    /// The digest length is not correct.
    #[error("digest length not correct")]
    DigestLengthNotCorrect,
    /// Cannot generate digest.
    #[error("cannot generate digest")]
    CannotGenerate,
}
