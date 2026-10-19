//! `tessellio` frame format: codec identity, limits, regions, and reset reasons.
//!
//! Every constant in this module is normative for the wire format: the encoder
//! asserts them, the decoder validates them, and the splitter enforces them, so
//! they exist exactly once.

/// Canonical codec wire id (`CODEC_TESSELLIO`).
pub const CODEC_WIRE_ID: i32 = 8;
/// Canonical operator-facing codec name.
pub const CODEC_NAME: &str = "tessellio";

/// Largest frame dimension the format accepts.
pub const MAX_SURFACE_DIMENSION: u32 = 8192;
/// Largest frame cell count the format accepts.
pub const MAX_SURFACE_CELLS: u64 = 16_777_216;
/// Largest region count in one frame.
pub const MAX_REGIONS_PER_UPDATE: usize = 64;
/// Largest region count in one part.
pub const MAX_REGIONS_PER_PART: usize = 32;
/// Largest compressed payload of one region.
pub const MAX_REGION_WIRE_BYTES: usize = 262_144;
/// Largest decompressed op stream of one region (the bounded-decompress cap).
pub const MAX_REGION_OPS_BYTES: usize = 1_048_576;
/// Largest payload carried by one part.
pub const MAX_PART_WIRE_BYTES: usize = 491_520;
/// Largest part count of one frame.
pub const MAX_PARTS_PER_UPDATE: usize = 8;
/// Largest total payload of one frame.
pub const MAX_UPDATE_WIRE_BYTES: usize = 3_932_160;
/// Part count a reset frame may fill before the remainder becomes the next
/// frame's work (progressive refresh).
pub const RESET_MAX_PARTS: usize = 4;
/// Payload budget of one reset frame (`RESET_MAX_PARTS * MAX_PART_WIRE_BYTES`).
pub const RESET_PAYLOAD_BUDGET_BYTES: usize = RESET_MAX_PARTS * MAX_PART_WIRE_BYTES;
/// Default delta changed-cell budget per frame.
pub const DELTA_REFRESH_CELLS: u32 = 131_072;
/// Progress guarantee: a frame whose budget is exhausted still carries this many
/// cells before stopping.
pub const DELTA_REFRESH_MIN_CELLS: u64 = 4_096;
/// Delta op-byte budget per frame (bounds the deflate input).
pub const DELTA_OPS_BUDGET_BYTES: usize = 393_216;
/// Rows per reset band.
pub const RESET_BAND_ROWS: u32 = 256;
/// Shortest constant run the op encoder expresses as a `FILL`.
pub const FILL_MIN_RUN: u32 = 8;
/// Journal byte ceiling.
pub const JOURNAL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Journal frame ceiling.
pub const JOURNAL_MAX_UPDATES: usize = 240;
/// Idle frame interval for an unchanged desktop.
pub const IDLE_UPDATE_INTERVAL_MS: u64 = 1_000;
/// Receiver resync rate limit.
pub const RESYNC_MIN_INTERVAL_MS: u64 = 200;
/// Pipeline-wide reset-frame rate limit for resync-triggered resets.
pub const RESET_MIN_INTERVAL_MS: u64 = 250;

/// The one supported codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Codec {
    /// The `tessellio` op-stream codec.
    #[default]
    Tessellio,
}

impl Codec {
    /// Wire id carried by every frame.
    pub const WIRE_ID: i32 = CODEC_WIRE_ID;
    /// Operator-facing name.
    pub const NAME: &'static str = CODEC_NAME;

    /// Wire id of this codec.
    pub fn wire_id(self) -> i32 {
        Self::WIRE_ID
    }

    /// Operator-facing name of this codec.
    pub fn name(self) -> &'static str {
        Self::NAME
    }

    /// The codec with the given wire id, when one exists.
    ///
    /// There is no alias, no name lookup, and no advertised list: the codec is
    /// not selectable, and an unknown id is a hard frame rejection.
    pub fn from_wire_id(id: i32) -> Option<Self> {
        (id == Self::WIRE_ID).then_some(Self::Tessellio)
    }
}

/// Kind of one encoded frame, used for metrics and error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateKind {
    /// Self-contained frame (`update_base == 0`).
    Reset,
    /// Frame that applies onto exactly one base frame.
    Delta,
    /// Delta frame with no regions: the idle heartbeat.
    Empty,
}

impl UpdateKind {
    /// Metric label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "self-contained",
            Self::Delta => "chained",
            Self::Empty => "empty",
        }
    }
}

/// Why a reset frame was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetReason {
    /// No reference exists yet.
    First,
    /// A receiver registered.
    Join,
    /// The last receiver left.
    Leave,
    /// Frame dimensions changed.
    Geometry,
    /// The operator changed the selected display.
    Display,
    /// A receiver reported a base the journal cannot reach.
    Resync,
    /// A receiver reported a base below the journal floor.
    JournalMiss,
    /// Receivers stopped acknowledging updates.
    Stall,
    /// A session transport ended, or the content moved to another path.
    SessionEnd,
    /// An explicit sender-side request.
    Operator,
}

impl ResetReason {
    /// Every reason, in the order the metric label set documents.
    pub const ALL: [Self; 10] = [
        Self::First,
        Self::Join,
        Self::Leave,
        Self::Geometry,
        Self::Display,
        Self::Resync,
        Self::JournalMiss,
        Self::Stall,
        Self::SessionEnd,
        Self::Operator,
    ];

    /// Metric label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Join => "join",
            Self::Leave => "leave",
            Self::Geometry => "geometry",
            Self::Display => "display",
            Self::Resync => "resync",
            Self::JournalMiss => "journal_miss",
            Self::Stall => "stall",
            Self::SessionEnd => "session_end",
            Self::Operator => "operator",
        }
    }
}

/// The set of reasons attached to one frame.
///
/// A frame records why it is a reset frame; the pipeline exports one counter
/// increment per reason. The set is empty on delta frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ResetReasons {
    bits: u32,
}

impl ResetReasons {
    /// An empty set.
    pub fn none() -> Self {
        Self { bits: 0 }
    }

    /// Whether the set is empty.
    pub fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Add one reason.
    pub fn add(&mut self, reason: ResetReason) {
        self.bits |= 1_u32 << (reason as u32);
    }

    /// Whether the set contains `reason`.
    pub fn contains(self, reason: ResetReason) -> bool {
        self.bits & (1_u32 << (reason as u32)) != 0
    }

    /// Iterate the reasons in [`ResetReason::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = ResetReason> {
        ResetReason::ALL
            .into_iter()
            .filter(move |reason| self.contains(*reason))
    }
}

/// Why a receiver reported that it could not apply a frame (§4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResyncReason {
    /// The frame applies onto a base the receiver does not hold.
    BaseMismatch,
    /// Not every part of the frame arrived.
    IncompleteFrame,
    /// A region payload could not be decoded.
    DecodeError,
    /// A reset frame's coverage rule was violated.
    CoverageInvalid,
    /// The frame carries a codec id the receiver does not implement.
    UnknownCodec,
    /// The receiver's ready queue overflowed.
    QueueOverflow,
    /// A delta's geometry differs from the canvas.
    GeometryMismatch,
    /// A part gap was detected before assembly.
    PartGap,
}

impl ResyncReason {
    /// Every reason, in the order the wire enum declares them.
    pub const ALL: [Self; 8] = [
        Self::BaseMismatch,
        Self::IncompleteFrame,
        Self::DecodeError,
        Self::CoverageInvalid,
        Self::UnknownCodec,
        Self::QueueOverflow,
        Self::GeometryMismatch,
        Self::PartGap,
    ];

    /// Metric label value (and the string the receiver sends).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaseMismatch => "BASE_MISMATCH",
            Self::IncompleteFrame => "INCOMPLETE_UPDATE",
            Self::DecodeError => "DECODE_ERROR",
            Self::CoverageInvalid => "COVERAGE_INVALID",
            Self::UnknownCodec => "UNKNOWN_CODEC",
            Self::QueueOverflow => "QUEUE_OVERFLOW",
            Self::GeometryMismatch => "GEOMETRY_MISMATCH",
            Self::PartGap => "PART_GAP",
        }
    }

    /// Wire id of the reason (the receiver's enum value).
    pub fn wire_id(self) -> i32 {
        match self {
            Self::BaseMismatch => 1,
            Self::IncompleteFrame => 2,
            Self::DecodeError => 3,
            Self::CoverageInvalid => 4,
            Self::UnknownCodec => 5,
            Self::QueueOverflow => 6,
            Self::GeometryMismatch => 7,
            Self::PartGap => 8,
        }
    }

    /// The reason with the given wire id, when one exists.
    pub fn from_wire_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.wire_id() == id)
    }
}

/// One rectangular region inside a frame, ready for the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Region x offset in frame cells.
    pub x: u32,
    /// Region y offset in frame cells.
    pub y: u32,
    /// Region width in cells.
    pub width: u32,
    /// Region height in cells.
    pub height: u32,
    /// Raw-DEFLATE op-stream payload.
    pub payload: Vec<u8>,
}

impl Region {
    /// Payload bytes carried by this region.
    pub fn payload_bytes(&self) -> usize {
        self.payload.len()
    }
}

/// Cell rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    /// X offset.
    pub x: u32,
    /// Y offset.
    pub y: u32,
    /// Width in cells.
    pub width: u32,
    /// Height in cells.
    pub height: u32,
}

impl Rect {
    /// Build a rectangle; a zero width or height makes it empty.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Cell area.
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Whether the rectangle contains no cells.
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Whether the two rectangles share at least one cell.
    pub fn intersects(self, other: Self) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// Smallest rectangle containing both inputs.
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self::new(x, y, right - x, bottom - y)
    }

    /// Added area of replacing the two rectangles by their bounding box.
    pub fn added_area_when_unioned(self, other: Self) -> u64 {
        self.union(other)
            .area()
            .saturating_sub(self.area() + other.area())
    }

    /// Whether the rectangle fits inside a `width`×`height` frame.
    pub fn is_inside(self, width: u32, height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && self.x + self.width <= width
            && self.y + self.height <= height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_identity_is_exact() {
        assert_eq!(Codec::Tessellio.wire_id(), 8);
        assert_eq!(Codec::Tessellio.name(), "tessellio");
        assert_eq!(Codec::from_wire_id(8), Some(Codec::Tessellio));
        for other in [0, 1, 2, 3, 7, 9, -1, i32::MAX] {
            assert_eq!(Codec::from_wire_id(other), None, "id {other}");
        }
    }

    #[test]
    fn constants_match_the_format_table() {
        assert_eq!(MAX_SURFACE_DIMENSION, 8192);
        assert_eq!(MAX_SURFACE_CELLS, 16_777_216);
        assert_eq!(MAX_REGIONS_PER_UPDATE, 64);
        assert_eq!(MAX_REGIONS_PER_PART, 32);
        assert_eq!(MAX_REGION_WIRE_BYTES, 262_144);
        assert_eq!(MAX_REGION_OPS_BYTES, 1_048_576);
        assert_eq!(MAX_PART_WIRE_BYTES, 491_520);
        assert_eq!(MAX_PARTS_PER_UPDATE, 8);
        assert_eq!(MAX_UPDATE_WIRE_BYTES, 3_932_160);
        assert_eq!(RESET_MAX_PARTS, 4);
        assert_eq!(RESET_PAYLOAD_BUDGET_BYTES, 1_966_080);
        assert_eq!(DELTA_REFRESH_CELLS, 131_072);
        assert_eq!(DELTA_REFRESH_MIN_CELLS, 4_096);
        assert_eq!(DELTA_OPS_BUDGET_BYTES, 393_216);
        assert_eq!(RESET_BAND_ROWS, 256);
        assert_eq!(FILL_MIN_RUN, 8);
        assert_eq!(JOURNAL_MAX_BYTES, 8 * 1024 * 1024);
        assert_eq!(JOURNAL_MAX_UPDATES, 240);
        assert_eq!(IDLE_UPDATE_INTERVAL_MS, 1_000);
        assert_eq!(RESYNC_MIN_INTERVAL_MS, 200);
        assert_eq!(RESET_MIN_INTERVAL_MS, 250);
    }

    #[test]
    fn reset_reason_labels_are_stable() {
        let labels: Vec<&str> = ResetReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "first",
                "join",
                "leave",
                "geometry",
                "display",
                "resync",
                "journal_miss",
                "stall",
                "session_end",
                "operator",
            ]
        );
        let mut reasons = ResetReasons::none();
        assert!(reasons.is_empty());
        reasons.add(ResetReason::Join);
        reasons.add(ResetReason::Geometry);
        reasons.add(ResetReason::Join);
        assert_eq!(
            reasons.iter().collect::<Vec<_>>(),
            vec![ResetReason::Join, ResetReason::Geometry]
        );
        assert!(ResetReasons::none().iter().next().is_none());
    }

    #[test]
    fn resync_reasons_match_the_wire_enum() {
        for (index, reason) in ResyncReason::ALL.iter().enumerate() {
            assert_eq!(reason.wire_id(), index as i32 + 1);
            assert_eq!(ResyncReason::from_wire_id(reason.wire_id()), Some(*reason));
        }
        assert_eq!(ResyncReason::BaseMismatch.as_str(), "BASE_MISMATCH");
        assert_eq!(ResyncReason::CoverageInvalid.as_str(), "COVERAGE_INVALID");
        assert_eq!(ResyncReason::from_wire_id(0), None);
        assert_eq!(ResyncReason::from_wire_id(9), None);
    }

    #[test]
    fn frame_kind_labels_are_stable() {
        assert_eq!(UpdateKind::Reset.as_str(), "self-contained");
        assert_eq!(UpdateKind::Delta.as_str(), "chained");
        assert_eq!(UpdateKind::Empty.as_str(), "empty");
    }

    #[test]
    fn rect_geometry_helpers_are_exact() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 0, 10, 10);
        assert!(!a.intersects(b));
        assert_eq!(a.union(b), Rect::new(0, 0, 30, 10));
        assert_eq!(a.added_area_when_unioned(b), 100);
        let c = Rect::new(5, 5, 5, 5);
        assert!(a.intersects(c));
        assert_eq!(a.added_area_when_unioned(c), 0);
        assert!(Rect::new(0, 0, 0, 4).is_empty());
        assert!(!a.intersects(Rect::new(3, 3, 0, 4)));
        assert!(a.is_inside(10, 10));
        assert!(!Rect::new(0, 0, 11, 10).is_inside(10, 10));
        assert_eq!(
            a.added_area_when_unioned(a),
            0,
            "identical rects add no area"
        );
    }
}
