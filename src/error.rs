//! The codec's error type.
//!
//! Every failure the codec reports is one of a small, closed set: the
//! caller can match on it and act, rather than parsing a message. A message
//! is carried for a human, not for a program.

use std::fmt;

use crate::format::UpdateKind;

/// Why a codec operation could not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// A surface's dimensions, or the length of its content, are invalid.
    InvalidSurface(String),
    /// A region lies partly outside the surface it belongs to.
    RegionOutOfBounds(String),
    /// A region's op stream does not obey the grammar.
    InvalidRegion(String),
    /// The content would exceed one of the format's caps.
    TooLarge {
        /// How many bytes it was.
        bytes: usize,
        /// The cap it exceeded.
        limit: usize,
    },
    /// The update would not fit the budget the caller gave it.
    Budget {
        /// The kind of update that ran out of budget.
        kind: UpdateKind,
        /// How many cells it had covered when it stopped.
        cells: u64,
    },
    /// The content could not be encoded.
    Encode(String),
    /// The compressed envelope could not be produced or read.
    Envelope(String),
    /// The content's cell format is not the one the codec encodes.
    UnsupportedFormat(String),
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSurface(detail) => write!(formatter, "invalid surface: {detail}"),
            Self::RegionOutOfBounds(detail) => {
                write!(formatter, "region out of bounds: {detail}")
            }
            Self::InvalidRegion(detail) => write!(formatter, "invalid region: {detail}"),
            Self::TooLarge { bytes, limit } => {
                write!(formatter, "{bytes} bytes exceeds the {limit} byte limit")
            }
            Self::Budget { kind, cells } => write!(
                formatter,
                "the encode budget was exhausted for a {kind:?} update after {cells} cells"
            ),
            Self::Encode(detail) => write!(formatter, "cannot encode: {detail}"),
            Self::Envelope(detail) => write!(formatter, "envelope: {detail}"),
            Self::UnsupportedFormat(detail) => {
                write!(formatter, "unsupported format: {detail}")
            }
        }
    }
}

impl std::error::Error for CodecError {}

/// The result of a codec operation.
pub type CodecResult<T> = Result<T, CodecError>;
