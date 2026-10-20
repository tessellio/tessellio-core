//! The Tessellio screen codec.
//!
//! Tessellio paints a surface as tiles of cells and moves only the tiles that
//! changed. An **update** is either **self-contained** (`update_base == 0`: it
//! establishes the whole surface) or **chained** (`update_base > 0`: it
//! continues exactly the surface the named base established). A region's
//! content is an op stream (`ROW`/`SPAN`/`FILL`/`END`) inside a raw-DEFLATE
//! envelope.
//!
//! Correctness rests on one property: change detection compares the same
//! 16-bit cells the receiver's canvas holds, and the sender's reference
//! advances only over the cells an update actually sent — so at every instant
//! the receiver's canvas equals the sender's reference for the update the
//! receiver has applied.
//!
//! Module map:
//!
//! - [`format`] — the codec's identity, its limits, regions and rectangles.
//! - [`surface`] — a surface of cells at wire fidelity.
//! - [`frame`] — the captured content the codec encodes.
//! - [`convert`] — 8-bit channels ↔ 16-bit cells, and the pointer compositor.
//! - [`detect`] — dirty tiles, greedy rectangles, exact bounding boxes, merge,
//!   per-row spans.
//! - [`ops`] — op build rules and the DEFLATE envelope.
//! - [`encode`] — the stream encoder, the refresh budget, and commit.
//! - [`reset`] — band-parallel self-contained updates.
//! - [`reference`] — the reference surface and its undo journal.
//! - [`part`] — the update's parts: splitting, validation, and assembly.
//! - [`decode`] — the decoder, the bounded op walk, and the surface oracle.
//! - [`telemetry`] — what the codec measures, and how a caller collects it.
//! - [`error`] — the codec's error vocabulary.
//! - [`scene`] — the reference scenes the codec is measured on.
//! - [`measured`] — the reference implementation's measured byte bounds.

pub mod buffer;
pub mod convert;
pub mod decode;
pub mod detect;
pub mod encode;
pub mod error;
pub mod format;
pub mod frame;
pub mod measured;
pub mod ops;
pub mod part;
pub mod reference;
pub mod reset;
pub mod scene;
pub mod telemetry;

#[cfg(feature = "cli")]
pub mod cli;

pub use buffer::Surface;
pub use convert::{cells_to_rgba, composite_pointer_in_place, rgba_to_cells};
pub use decode::{
    Coverage, DecodeError, SurfaceCanvas, decode_region_ops, inflate_region, validate_region_ops,
};
pub use detect::{ChangeMap, Span, TILE};
pub use encode::{CodecConfig, EncodedUpdate, PlannedUpdate, ScreenStreamEncoder};
pub use error::{CodecError, CodecResult};
pub use format::UpdateKind;
pub use format::{
    CODEC_NAME, CODEC_WIRE_ID, Codec, DELTA_OPS_BUDGET_BYTES, DELTA_REFRESH_CELLS,
    DELTA_REFRESH_MIN_CELLS, FILL_MIN_RUN, IDLE_UPDATE_INTERVAL_MS, JOURNAL_MAX_BYTES,
    JOURNAL_MAX_UPDATES, MAX_PART_WIRE_BYTES, MAX_PARTS_PER_UPDATE, MAX_REGION_OPS_BYTES,
    MAX_REGION_WIRE_BYTES, MAX_REGIONS_PER_PART, MAX_REGIONS_PER_UPDATE, MAX_SURFACE_CELLS,
    MAX_SURFACE_DIMENSION, MAX_UPDATE_WIRE_BYTES, RESET_BAND_ROWS, RESET_MAX_PARTS,
    RESET_MIN_INTERVAL_MS, RESET_PAYLOAD_BUDGET_BYTES, RESYNC_MIN_INTERVAL_MS, Rect, Region,
    ResetReason, ResetReasons, ResyncReason,
};
pub use frame::{CapturedFrame, PointerShape};
pub use ops::{
    Deflater, OP_END, OP_FILL, OP_ROW, OP_SPAN, deflate_ops, emit_end, emit_fill, emit_full_row,
    emit_row, emit_span, emit_spans, truncate_spans,
};
pub use part::{
    AssembledUpdate, AssemblyOutcome, Part, PartRejection, UpdateAssembler, UpdateSplitter,
    validate_part,
};
pub use reference::{
    JournalEntry, ReferenceJournal, UndoSegment, WriteSpan, apply_writes, undo_from_spans,
};
pub use reset::{BandRegion, encode_bands};
pub use telemetry::{CodecEvent, EncodePhase, NoTelemetry, SharedTelemetry, Telemetry};
