//! AMF0 error type.
// Modified by Uplink: explicit resource and consumption errors; see ../PATCHES.md.

use std::io;
use std::num::TryFromIntError;
use std::str::Utf8Error;

use crate::Amf0Marker;

/// Result type.
pub type Result<T> = std::result::Result<T, Amf0Error>;

/// AMF0 error.
#[derive(thiserror::Error, Debug)]
pub enum Amf0Error {
    /// A configured decoder resource budget would be exceeded.
    #[error("AMF decode limit exceeded: {0}")]
    DecodeLimit(&'static str),
    /// A previous partial read failed; this decoder must not be reused.
    #[error("AMF decoder cannot continue after a failed read")]
    DecoderFailed,
    /// A complete-message API was given trailing data.
    #[error("trailing AMF data")]
    TrailingData,
    /// A Serde visitor did not consume its container or violated access order.
    #[error("AMF container was not fully consumed")]
    IncompleteContainer,
    /// IO error.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// Element (string or sequence) is too long.
    #[error("element is too long: {0}")]
    TooLong(#[from] TryFromIntError),
    /// Cannot serialize sequence with unknown length.
    #[error("cannot serialize sequence with unknown length")]
    UnknownLength,
    /// Cannot serialize map with non-string key.
    #[error("cannot serialize map with non-string key")]
    MapKeyNotString,
    /// Unknown marker.
    #[error("unknown marker: {0}")]
    UnknownMarker(u8),
    /// This marker cannot be deserialized.
    #[error("this marker cannot be deserialized: {0:?}")]
    UnsupportedMarker(Amf0Marker),
    /// String parse error.
    #[error("string parse error: {0}")]
    StringParseError(#[from] Utf8Error),
    /// Unexpected type.
    #[error("unexpected type: expected one of {expected:?}, got {got:?}")]
    UnexpectedType {
        /// The expected types.
        expected: &'static [Amf0Marker],
        /// The actual type.
        got: Amf0Marker,
    },
    /// Wrong array length.
    #[error("wrong array length: expected {expected}, got {got}")]
    WrongArrayLength {
        /// The expected length.
        expected: usize,
        /// The actual length.
        got: usize,
    },
    /// char deserialization is not supported.
    #[error("char deserialization is not supported")]
    CharNotSupported,
    /// Custom error message.
    #[cfg(feature = "serde")]
    #[error("{0}")]
    Custom(String),
}

#[cfg(feature = "serde")]
impl serde::ser::Error for Amf0Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Amf0Error::Custom(msg.to_string())
    }
}

#[cfg(feature = "serde")]
impl serde::de::Error for Amf0Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Amf0Error::Custom(msg.to_string())
    }
}
