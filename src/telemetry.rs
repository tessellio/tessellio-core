//! What the codec measures, and how a caller collects it.
//!
//! A codec is a library, and a library does not decide where its
//! measurements go: the application embedding it does. The codec therefore
//! reports what it observed through a [`Telemetry`] hook the caller installs,
//! and the default hook discards everything.
//!
//! The measurements are the ones an operator needs in order to answer
//! "why is the stream slow?": where the encode time went, and whether the
//! encoder's own self-check ever rejected an update.

use std::sync::Arc;
use std::time::Duration;

/// The phases of one encode, in the order the encoder performs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncodePhase {
    /// Reducing the content to the codec's cell format.
    Convert,
    /// Comparing the content against the reference.
    Compare,
    /// Finding the changed spans.
    Spans,
    /// Packing the spans into regions.
    Pack,
    /// Compressing the regions.
    Compress,
    /// Advancing the reference.
    Reference,
}

impl EncodePhase {
    /// The phase's stable name, as a metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Convert => "convert",
            Self::Compare => "compare",
            Self::Spans => "spans",
            Self::Pack => "pack",
            Self::Compress => "compress",
            Self::Reference => "reference",
        }
    }
}

/// What the codec reports to its caller.
#[derive(Debug, Clone, PartialEq)]
pub enum CodecEvent {
    /// One phase of one encode took this long.
    PhaseElapsed {
        /// Which phase.
        phase: EncodePhase,
        /// How long it took.
        elapsed: Duration,
    },
    /// The encoder's own decoder rejected an update before it was sent.
    ///
    /// This is a codec defect, not a transport event: a non-zero rate means
    /// the encoder produced something the decoder refuses.
    SelfCheckFailed {
        /// What was being encoded.
        what: &'static str,
        /// The detail, when the failure carried one.
        detail: Option<String>,
    },
}

/// Where the codec's measurements go.
///
/// The default implementation discards everything, so a caller that does not
/// care pays nothing.
pub trait Telemetry: Send + Sync {
    /// One measurement.
    fn record(&self, event: CodecEvent);

    /// Whether the hook wants the phase timings at all.
    ///
    /// The encoder shapes its hot loops around this answer: a hook that
    /// answers `false` gets no per-row clock reads (the phases are not
    /// recorded, and the loops run without them). A hook that wants the
    /// histogram answers `true`. The default is `true`, so a hook that only
    /// implements [`Telemetry::record`] keeps receiving every event.
    fn wants_phases(&self) -> bool {
        true
    }
}

/// The default: nothing is measured.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTelemetry;

impl Telemetry for NoTelemetry {
    fn record(&self, _event: CodecEvent) {}

    fn wants_phases(&self) -> bool {
        false
    }
}

/// A shared telemetry hook.
pub type SharedTelemetry = Arc<dyn Telemetry>;
