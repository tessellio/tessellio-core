//! The `tessellio` stream encoder.
//!
//! One encoder serves every receiver of a sender process. Per captured frame it
//! converts RGBA into its own the 16-bit cell format workspace (one pass, buffer reused),
//! compares that against the reference at wire fidelity, derives exact changed
//! rectangles and spans, applies the changed-cell budget, builds the op stream,
//! deflates each region, and records the undo the journal needs. The reference
//! advances only over the cells actually sent, and only when the caller
//! commits the plan — a frame that is never committed leaves no trace.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::buffer::Surface;
use crate::convert::{convert_pixels, rgba_to_cells};
use crate::decode::{Coverage, validate_region_ops};
use crate::detect::{
    ChangeMap, Span, bounding_box, make_disjoint, merge_to_limit, rects_from_map, row_spans,
    shrink_all,
};
use crate::error::CodecError;
use crate::format::DELTA_REFRESH_CELLS;
use crate::format::{
    Codec, DELTA_OPS_BUDGET_BYTES, DELTA_REFRESH_MIN_CELLS, MAX_REGION_WIRE_BYTES,
    MAX_REGIONS_PER_UPDATE, RESET_PAYLOAD_BUDGET_BYTES, Rect, Region, ResetReason, ResetReasons,
    UpdateKind,
};
use crate::frame::CapturedFrame;
use crate::ops::{Deflater, emit_end, emit_spans, truncate_spans};
use crate::reference::{
    JournalEntry, ReferenceJournal, UndoSegment, WriteSpan, apply_writes, undo_from_spans,
};
use crate::reset::encode_bands;
use crate::telemetry::{CodecEvent, EncodePhase, NoTelemetry, SharedTelemetry, Telemetry};

/// Per-region op-stream ceiling for a delta frame.
///
/// The wire cap applies to the *deflated* region, and raw DEFLATE never expands
/// an op stream beyond a few bytes per block, so capping the op stream slightly
/// below the wire cap keeps every delta region legal without a second
/// compression pass.
const REGION_OPS_CAP: usize = MAX_REGION_WIRE_BYTES - 1024;

/// Encoder configuration; every field is a documented knob with a default.
#[derive(Debug, Clone, PartialEq)]
pub struct CodecConfig {
    /// Changed-cell budget of one delta frame.
    pub refresh_cells: u32,
    /// DEFLATE level for delta regions (`0..=9`).
    pub delta_deflate_level: u8,
    /// DEFLATE level for reset bands (`0..=9`).
    pub reset_deflate_level: u8,
    /// Band-parallel worker ceiling; `0` means the machine's parallelism.
    pub encoder_threads: usize,
    /// Idle heartbeat interval for an unchanged desktop.
    pub idle_update_interval: Duration,
}

impl Default for CodecConfig {
    fn default() -> Self {
        Self {
            refresh_cells: DELTA_REFRESH_CELLS,
            delta_deflate_level: 6,
            reset_deflate_level: 3,
            encoder_threads: 0,
            idle_update_interval: Duration::from_millis(crate::format::IDLE_UPDATE_INTERVAL_MS),
        }
    }
}

impl CodecConfig {
    /// Lowest accepted changed-cell budget.
    pub const MIN_REFRESH_PIXELS: u32 = DELTA_REFRESH_MIN_CELLS as u32;
    /// Highest accepted changed-cell budget.
    pub const MAX_REFRESH_PIXELS: u32 = 1_048_576;

    /// Clamp every knob into its documented bounds.
    pub fn clamped(mut self) -> Self {
        self.refresh_cells = self
            .refresh_cells
            .clamp(Self::MIN_REFRESH_PIXELS, Self::MAX_REFRESH_PIXELS);
        self.delta_deflate_level = self.delta_deflate_level.min(9);
        self.reset_deflate_level = self.reset_deflate_level.min(9);
        self.encoder_threads = self.encoder_threads.min(8);
        self
    }

    /// Read the codec knobs from the environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = Self::default();
        let read_u32 = |key: &str| lookup(key).and_then(|value| value.trim().parse::<u32>().ok());
        let read_u8 = |key: &str| lookup(key).and_then(|value| value.trim().parse::<u8>().ok());
        Self {
            refresh_cells: read_u32("TESSELLIO_REFRESH_CELLS").unwrap_or(defaults.refresh_cells),
            delta_deflate_level: read_u8("TESSELLIO_DELTA_DEFLATE_LEVEL")
                .unwrap_or(defaults.delta_deflate_level),
            reset_deflate_level: read_u8("TESSELLIO_RESET_DEFLATE_LEVEL")
                .unwrap_or(defaults.reset_deflate_level),
            encoder_threads: read_u32("TESSELLIO_ENCODER_THREADS")
                .map(|value| value as usize)
                .unwrap_or(defaults.encoder_threads),
            idle_update_interval: Duration::from_millis(
                read_u32("TESSELLIO_IDLE_UPDATE_MS")
                    .map(u64::from)
                    .unwrap_or(crate::format::IDLE_UPDATE_INTERVAL_MS),
            ),
        }
        .clamped()
    }
}

/// One encoded frame before parting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedUpdate {
    /// Monotonic frame id assigned by the pipeline.
    pub update_id: u64,
    /// Frame this delta applies onto; `0` marks a reset frame.
    pub update_base: u64,
    /// Frame width in cells.
    pub width: u32,
    /// Frame height in cells.
    pub height: u32,
    /// Codec of every region.
    pub codec: Codec,
    /// Encoded regions, ascending by `(y, x)`.
    pub regions: Vec<Region>,
    /// Exact changed cells vs the base (the band area on a reset frame).
    pub changed_pixels: u64,
    /// Cells this frame carries.
    pub sent_pixels: u64,
    /// Cells still differing from the reference after this frame.
    pub backlog_pixels: u64,
    /// Non-empty only on reset frames.
    pub reset_reasons: ResetReasons,
}

impl EncodedUpdate {
    /// Frame kind used by metrics and the splitter.
    pub fn kind(&self) -> UpdateKind {
        if self.update_base == 0 {
            UpdateKind::Reset
        } else if self.regions.is_empty() {
            UpdateKind::Empty
        } else {
            UpdateKind::Delta
        }
    }

    /// Total region payload bytes.
    pub fn payload_bytes(&self) -> usize {
        self.regions.iter().map(|region| region.payload.len()).sum()
    }

    /// Whether this frame is self-contained.
    pub fn is_reset(&self) -> bool {
        self.update_base == 0
    }
}

/// Which buffer the planned writes must be applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateTarget {
    /// The reference buffer, which already holds `update_base`.
    Masked,
    /// The rebase scratch, which must then become the reference.
    RebaseScratch,
    /// A fresh zero-filled reference (geometry-authoritative reset).
    Fresh,
}

/// A frame that has been planned and encoded but not yet assigned an id.
pub struct PlannedUpdate {
    /// Base frame the receiver must hold.
    pub update_base: u64,
    regions: Vec<Region>,
    writes: Vec<WriteSpan>,
    undo: Vec<UndoSegment>,
    changed_pixels: u64,
    sent_pixels: u64,
    backlog_pixels: u64,
    reset_reasons: ResetReasons,
    update: UpdateTarget,
    width: u32,
    height: u32,
}

impl std::fmt::Debug for PlannedUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlannedUpdate")
            .field("update_base", &self.update_base)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("regions", &self.regions.len())
            .field("writes", &self.writes.len())
            .field("changed_pixels", &self.changed_pixels)
            .field("sent_pixels", &self.sent_pixels)
            .field("backlog_pixels", &self.backlog_pixels)
            .field("reset_reasons", &self.reset_reasons)
            .finish()
    }
}

/// Which buffer a delta plan compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeltaBase {
    /// The current reference.
    Reference,
    /// The rebase scratch, built from the journal.
    Scratch,
}

/// The encoder. One instance per pipeline; no interior mutability, no OS calls,
/// no `unsafe`.
pub struct ScreenStreamEncoder {
    config: CodecConfig,
    telemetry: SharedTelemetry,
    workspace: Option<Surface>,
    reference: Option<Surface>,
    reference_update_id: u64,
    scratch: Option<Surface>,
    journal: ReferenceJournal,
    reset_reasons: ResetReasons,
    reset_from_y: Option<u32>,
    last_frame_at: Option<Instant>,
    change_map: ChangeMap,
    rects: Vec<Rect>,
    visited: Vec<bool>,
    spans: Vec<Span>,
    limited: Vec<Span>,
    region_ops: Vec<u8>,
    row_ops: Vec<u8>,
    row_scratch: Vec<u16>,
    stale_rows: Vec<u64>,
    writes: Vec<WriteSpan>,
    deflater: Deflater,
    selfcheck_failures: u64,
    /// Test-only switch that forces every delta plan through the self-check
    /// failure path, so the pipeline's recovery contract for a skipped frame
    /// can be exercised without corrupting the encoder's real validation.
    #[cfg(test)]
    injected_selfcheck_failure: bool,
}

impl Default for ScreenStreamEncoder {
    fn default() -> Self {
        Self::new(CodecConfig::default())
    }
}

impl ScreenStreamEncoder {
    /// The telemetry hook the encoder reports to.
    fn telemetry(&self) -> &dyn Telemetry {
        self.telemetry.as_ref()
    }

    /// Build an encoder with the given configuration.
    pub fn new(config: CodecConfig) -> Self {
        Self::with_telemetry(config, Arc::new(NoTelemetry))
    }

    /// An encoder that reports what it measured to `telemetry`.
    pub fn with_telemetry(config: CodecConfig, telemetry: SharedTelemetry) -> Self {
        let deflater = Deflater::new(config.delta_deflate_level);
        Self {
            config: config.clamped(),
            telemetry,
            workspace: None,
            reference: None,
            reference_update_id: 0,
            scratch: None,
            journal: ReferenceJournal::new(),
            reset_reasons: ResetReasons::none(),
            reset_from_y: None,
            last_frame_at: None,
            change_map: ChangeMap::new(),
            rects: Vec::new(),
            visited: Vec::new(),
            spans: Vec::new(),
            limited: Vec::new(),
            region_ops: Vec::new(),
            row_ops: Vec::new(),
            row_scratch: Vec::new(),
            stale_rows: Vec::new(),
            writes: Vec::new(),
            deflater,
            selfcheck_failures: 0,
            #[cfg(test)]
            injected_selfcheck_failure: false,
        }
    }

    /// Current configuration.
    pub fn config(&self) -> &CodecConfig {
        &self.config
    }

    /// Replace the configuration (clamped).
    pub fn set_config(&mut self, config: CodecConfig) {
        self.config = config.clamped();
    }

    /// Reset the reference and the journal; the next frame is a reset frame.
    pub fn reset(&mut self, reason: ResetReason) {
        self.reset_reasons.add(reason);
        self.reset_from_y = Some(0);
    }

    /// Whether the next frame must be a reset frame.
    pub fn reset_pending(&self) -> bool {
        self.reset_from_y.is_some()
    }

    /// Encode one captured frame.
    ///
    /// Returns `Ok(None)` when the frame produces no work: the content is
    /// unchanged and an idle frame was planned within the idle interval.
    pub fn encode(&mut self, frame: &CapturedFrame) -> Result<Option<PlannedUpdate>, CodecError> {
        if frame.width == 0 || frame.height == 0 {
            return Err(CodecError::InvalidSurface("empty frame".to_string()));
        }
        // A reference that does not match the frame's geometry can never be a
        // delta base: the next frame is a full repaint for this geometry.
        let usable = self
            .reference
            .as_ref()
            .is_some_and(|buffer| buffer.matches(frame.width, frame.height));
        if !usable {
            if self.reference_update_id == 0 {
                self.reset_reasons.add(ResetReason::First);
            } else {
                self.reset_reasons.add(ResetReason::Geometry);
            }
            self.reset_from_y = Some(0);
        }
        match self.reset_from_y {
            Some(from_y) => {
                let convert_started = Instant::now();
                self.prepare_workspace(frame)?;
                record_phase(
                    self.telemetry(),
                    EncodePhase::Convert,
                    convert_started.elapsed(),
                );
                self.plan_reset(from_y)
            }
            None => {
                self.convert_and_detect(frame, DeltaBase::Reference)?;
                self.plan_delta(frame, self.reference_update_id, DeltaBase::Reference)
            }
        }
    }

    /// Encode against an older base that a receiver reported holding.
    ///
    /// Returns `Ok(None)` when the base is not reachable (below the journal
    /// floor, ahead of the reference, or superseded by a pending reset), which
    /// the pipeline answers with a reset frame.
    pub fn rebase_and_encode(
        &mut self,
        held_update_id: u64,
        frame: &CapturedFrame,
    ) -> Result<Option<PlannedUpdate>, CodecError> {
        if !self.journal_can_reach(held_update_id) {
            return Ok(None);
        }
        if frame.width == 0 || frame.height == 0 {
            return Err(CodecError::InvalidSurface("empty frame".to_string()));
        }
        if held_update_id == self.reference_update_id {
            self.convert_and_detect(frame, DeltaBase::Reference)?;
            return self.plan_delta(frame, held_update_id, DeltaBase::Reference);
        }
        if !self.rebuild_scratch(held_update_id)? {
            return Ok(None);
        }
        self.convert_and_detect(frame, DeltaBase::Scratch)?;
        self.plan_delta(frame, held_update_id, DeltaBase::Scratch)
    }

    /// Assign the frame id, commit the reference update, and journal the undo.
    pub fn commit(&mut self, planned: PlannedUpdate, update_id: u64) -> EncodedUpdate {
        let PlannedUpdate {
            update_base,
            regions,
            writes,
            undo,
            changed_pixels,
            sent_pixels,
            backlog_pixels,
            reset_reasons,
            update,
            width,
            height,
        } = planned;
        let reference_started = Instant::now();
        match update {
            UpdateTarget::Masked => {
                let source = self.workspace.as_ref();
                if let (Some(dest), Some(source)) = (self.reference.as_mut(), source)
                    && let Err(error) = apply_writes(dest, source, &writes)
                {
                    self.selfcheck_failures += 1;
                    self.telemetry().record(CodecEvent::SelfCheckFailed {
                        what: "the reference update",
                        detail: Some(error.to_string()),
                    });
                }
            }
            UpdateTarget::RebaseScratch => {
                let source = self.workspace.as_ref();
                if let (Some(dest), Some(source)) = (self.scratch.as_mut(), source)
                    && let Err(error) = apply_writes(dest, source, &writes)
                {
                    self.selfcheck_failures += 1;
                    self.telemetry().record(CodecEvent::SelfCheckFailed {
                        what: "the rebase scratch update",
                        detail: Some(error.to_string()),
                    });
                }
                if let Some(scratch) = self.scratch.take() {
                    self.scratch = self.reference.replace(scratch);
                }
            }
            UpdateTarget::Fresh => {
                if let Ok(mut dest) = Surface::new(width, height) {
                    if let Some(source) = self.workspace.as_ref() {
                        let _ = apply_writes(&mut dest, source, &writes);
                    }
                    let previous = self.reference.replace(dest);
                    self.scratch = previous.filter(|buffer| buffer.matches(width, height));
                }
            }
        }
        if update_base == 0 {
            self.journal.clear(update_id);
        } else {
            self.journal
                .push(JournalEntry::new(update_id, update_base, undo));
        }
        self.reference_update_id = update_id;
        // Return the write staging buffer to the encoder (clear, do not free).
        self.writes = writes;
        self.writes.clear();
        record_phase(
            self.telemetry(),
            EncodePhase::Reference,
            reference_started.elapsed(),
        );
        EncodedUpdate {
            update_id,
            update_base,
            width,
            height,
            codec: Codec::Tessellio,
            regions,
            changed_pixels,
            sent_pixels,
            backlog_pixels,
            reset_reasons,
        }
    }

    /// Journal size in undo value bytes.
    pub fn journal_bytes(&self) -> usize {
        self.journal.bytes()
    }

    /// Journal frame count.
    pub fn journal_frames(&self) -> usize {
        self.journal.frames()
    }

    /// Oldest frame id a receiver may report holding, when reachable at all.
    pub fn journal_floor_update_id(&self) -> u64 {
        self.journal.floor_update_id()
    }

    /// Frame id the reference buffer holds (`0` = no reference yet).
    pub fn reference_update_id(&self) -> u64 {
        self.reference_update_id
    }

    /// The reference cells, as the receiver's canvas must hold them after the
    /// frame the reference belongs to (empty before the first frame).
    ///
    /// This is the inspection surface the model tests assert INV-R against; no
    /// production path reads the reference back.
    pub fn reference_pixels(&self) -> &[u16] {
        match self.reference.as_ref() {
            Some(reference) => reference.as_u16(),
            None => &[],
        }
    }

    /// Reference geometry, if a reference exists.
    pub fn reference_geometry(&self) -> Option<(u32, u32)> {
        self.reference
            .as_ref()
            .map(|reference| (reference.width(), reference.height()))
    }

    /// Whether `held_update_id` can be reconstructed by the journal.
    pub fn journal_can_reach(&self, held_update_id: u64) -> bool {
        self.reset_from_y.is_none()
            && self
                .journal
                .can_reach(held_update_id, self.reference_update_id)
    }

    /// Frames skipped by the encoder self-check over this encoder's lifetime.
    pub fn selfcheck_failures(&self) -> u64 {
        self.selfcheck_failures
    }

    /// Convert the frame into the the 16-bit cell format workspace and compare it against the
    /// delta base, one converted row at a time.
    ///
    /// The interleaving is the point: each row is converted into a row scratch,
    /// compared against the reference row while it is still in L1, and copied
    /// into the workspace only when a tile covering it is dirty. A static frame
    /// therefore writes nothing at all into the workspace, and a small delta
    /// writes only the rows it carries — instead of materializing the whole
    /// 1080p conversion (4.15 MB of stores, plus the read-back they displace)
    /// before the comparison can start.
    ///
    /// The comparison itself is exactly the one [`ChangeMap::detect`] performs
    /// (same tiles, same dirtiness, same upper bound), so every rectangle and
    /// every byte downstream is unchanged. Rows kept in the scratch are recorded
    /// as stale and repaired by `repair_stale_rows` if a later merge
    /// pulls them into a rectangle. Both phases are recorded as their own
    /// metrics.
    fn convert_and_detect(
        &mut self,
        frame: &CapturedFrame,
        base: DeltaBase,
    ) -> Result<(), CodecError> {
        if frame.width == 0 || frame.height == 0 {
            return Err(CodecError::InvalidSurface("empty frame".to_string()));
        }
        let matches = self
            .workspace
            .as_ref()
            .is_some_and(|buffer| buffer.matches(frame.width, frame.height));
        if !matches {
            self.workspace = Some(Surface::new(frame.width, frame.height)?);
        }
        let width = frame.width as usize;
        if self.row_scratch.len() < width {
            self.row_scratch.resize(width, 0);
        }
        self.stale_rows.clear();
        self.stale_rows
            .resize((frame.height as usize).div_ceil(64), 0);
        let change_map = &mut self.change_map;
        let base_buffer = match base {
            DeltaBase::Reference => self.reference.as_ref(),
            DeltaBase::Scratch => self.scratch.as_ref(),
        };
        let Some(workspace) = self.workspace.as_mut() else {
            return Err(CodecError::InvalidSurface(
                "workspace unavailable".to_string(),
            ));
        };
        let reference_usable = change_map.prepare(workspace, base_buffer).is_some();
        // One borrow of the delta base for the whole pass (the workspace and
        // the change map are disjoint fields).
        let reference = match base {
            DeltaBase::Reference => self.reference.as_ref(),
            DeltaBase::Scratch => self.scratch.as_ref(),
        };
        let reference = reference.filter(|_| reference_usable);
        let mut convert_time = Duration::ZERO;
        let mut compare_time = Duration::ZERO;
        let row_bytes = width * 4;
        for y in 0..frame.height {
            let start = y as usize * row_bytes;
            let started = Instant::now();
            convert_pixels(
                &frame.rgba[start..start + row_bytes],
                &mut self.row_scratch[..width],
            );
            convert_time += started.elapsed();
            let started = Instant::now();
            let needed = match reference {
                // `needed` is `true` when no usable base exists: the row is
                // part of a full repaint and must reach the workspace.
                Some(reference) => change_map.compare_row(&self.row_scratch[..width], reference, y),
                None => true,
            };
            compare_time += started.elapsed();
            let word = y as usize / 64;
            let bit = 1_u64 << (y as usize % 64);
            if needed {
                workspace
                    .row_mut(y)
                    .copy_from_slice(&self.row_scratch[..width]);
                self.stale_rows[word] &= !bit;
            } else {
                self.stale_rows[word] |= bit;
            }
        }
        record_phase(self.telemetry(), EncodePhase::Convert, convert_time);
        record_phase(self.telemetry(), EncodePhase::Compare, compare_time);
        Ok(())
    }

    /// Convert the frame into the the 16-bit cell format workspace (allocating on geometry
    /// change only).
    fn prepare_workspace(&mut self, frame: &CapturedFrame) -> Result<(), CodecError> {
        if frame.width == 0 || frame.height == 0 {
            return Err(CodecError::InvalidSurface("empty frame".to_string()));
        }
        let matches = self
            .workspace
            .as_ref()
            .is_some_and(|buffer| buffer.matches(frame.width, frame.height));
        if !matches {
            self.workspace = Some(Surface::new(frame.width, frame.height)?);
        }
        let Some(workspace) = self.workspace.as_mut() else {
            return Err(CodecError::InvalidSurface(
                "workspace unavailable".to_string(),
            ));
        };
        rgba_to_cells(&frame.rgba, frame.width, frame.height, workspace)
    }

    /// Rebuild the scratch so it holds the state of `held_update_id`.
    fn rebuild_scratch(&mut self, held_update_id: u64) -> Result<bool, CodecError> {
        let (width, height) = match self.reference.as_ref() {
            Some(reference) => (reference.width(), reference.height()),
            None => return Ok(false),
        };
        let matches = self
            .scratch
            .as_ref()
            .is_some_and(|buffer| buffer.matches(width, height));
        if !matches {
            self.scratch = Some(Surface::new(width, height)?);
        }
        let Some(scratch) = self.scratch.as_mut() else {
            return Ok(false);
        };
        if let Some(reference) = self.reference.as_ref() {
            scratch.copy_from(reference)?;
        }
        Ok(self
            .journal
            .apply_undo(held_update_id, self.reference_update_id, scratch))
    }

    /// Plan a reset frame starting at row `from_y`.
    fn plan_reset(&mut self, from_y: u32) -> Result<Option<PlannedUpdate>, CodecError> {
        let Some(workspace) = self.workspace.as_ref() else {
            return Ok(None);
        };
        let (width, height) = (workspace.width(), workspace.height());
        let fresh = !self
            .reference
            .as_ref()
            .is_some_and(|buffer| buffer.matches(width, height));

        let started = Instant::now();
        let bands = encode_bands(
            workspace,
            from_y,
            self.config.reset_deflate_level,
            self.config.encoder_threads,
            RESET_PAYLOAD_BUDGET_BYTES,
        )?;
        let elapsed = started.elapsed();
        let mut pack_ns = 0_u64;
        let mut compress_ns = 0_u64;
        for band in &bands {
            pack_ns += band.pack_ns;
            compress_ns += band.compress_ns;
        }
        // Band-parallel work: the phase values are the aggregate worker time of
        // this frame's bands, which is what the CPU-budget histogram is for.
        record_phase(
            self.telemetry(),
            EncodePhase::Pack,
            Duration::from_nanos(pack_ns).max(elapsed),
        );
        record_phase(
            self.telemetry(),
            EncodePhase::Compress,
            Duration::from_nanos(compress_ns),
        );

        if bands.is_empty() {
            return Ok(None);
        }
        let mut regions = Vec::with_capacity(bands.len());
        let mut writes = std::mem::take(&mut self.writes);
        writes.clear();
        let mut sent_pixels = 0_u64;
        let mut covered_to = from_y;
        for band in bands {
            let rows = u64::from(band.height);
            sent_pixels += rows * u64::from(width);
            covered_to = band.y + band.height;
            writes.push(WriteSpan::new(band.y * width, band.height * width));
            regions.push(Region {
                x: 0,
                y: band.y,
                width,
                height: band.height,
                payload: band.payload,
            });
        }
        if regions.len() > MAX_REGIONS_PER_UPDATE {
            self.selfcheck_failures += 1;
            self.telemetry().record(CodecEvent::SelfCheckFailed {
                what: "the region cap",
                detail: None,
            });
            return Ok(None);
        }
        self.reset_from_y = (covered_to < height).then_some(covered_to);
        self.last_frame_at = Some(Instant::now());
        let reset_reasons = std::mem::take(&mut self.reset_reasons);
        Ok(Some(PlannedUpdate {
            update_base: 0,
            regions,
            writes,
            undo: Vec::new(),
            changed_pixels: sent_pixels,
            sent_pixels,
            backlog_pixels: 0,
            reset_reasons,
            update: if fresh {
                UpdateTarget::Fresh
            } else {
                UpdateTarget::Masked
            },
            width,
            height,
        }))
    }

    /// Plan a delta frame against `base`.
    ///
    /// Expects the workspace to hold the converted frame's rows that the final
    /// rectangle set needs and the change map to be prepared against `base`
    /// (both done by [`Self::convert_and_detect`]).
    fn plan_delta(
        &mut self,
        frame: &CapturedFrame,
        update_base: u64,
        base: DeltaBase,
    ) -> Result<Option<PlannedUpdate>, CodecError> {
        let reference_update_id = self.reference_update_id;
        let Some(workspace) = self.workspace.as_mut() else {
            return Ok(None);
        };
        let width = workspace.width();
        let height = workspace.height();
        let base_buffer = match base {
            DeltaBase::Reference => self.reference.as_ref(),
            DeltaBase::Scratch => self.scratch.as_ref(),
        };
        let Some(base_buffer) = base_buffer else {
            return Ok(None);
        };
        if !base_buffer.matches(width, height) {
            return Ok(None);
        }

        if self.change_map.is_empty() {
            // An unchanged desktop produces one heartbeat per idle interval,
            // not one per capture.
            let within_idle = self
                .last_frame_at
                .is_some_and(|at| at.elapsed() < self.config.idle_update_interval);
            if update_base == reference_update_id && within_idle {
                return Ok(None);
            }
            self.last_frame_at = Some(Instant::now());
            let mut writes = std::mem::take(&mut self.writes);
            writes.clear();
            return Ok(Some(PlannedUpdate {
                update_base,
                regions: Vec::new(),
                writes,
                undo: Vec::new(),
                changed_pixels: 0,
                sent_pixels: 0,
                backlog_pixels: 0,
                reset_reasons: ResetReasons::none(),
                update: match base {
                    DeltaBase::Reference => UpdateTarget::Masked,
                    DeltaBase::Scratch => UpdateTarget::RebaseScratch,
                },
                width,
                height,
            }));
        }

        // Steps C–E — rectangles, exact bounding boxes, merge to the cap.
        let detect_started = Instant::now();
        rects_from_map(
            &self.change_map,
            width,
            height,
            &mut self.visited,
            &mut self.rects,
        );
        shrink_all(workspace, base_buffer, &mut self.rects);
        if self.rects.len() > MAX_REGIONS_PER_UPDATE {
            merge_to_limit(&mut self.rects, MAX_REGIONS_PER_UPDATE);
            if !make_disjoint(&mut self.rects) {
                collapse_to_bounding_box(&mut self.rects);
            }
            shrink_all(workspace, base_buffer, &mut self.rects);
            if self.rects.len() > MAX_REGIONS_PER_UPDATE {
                collapse_to_bounding_box(&mut self.rects);
            }
        }
        let spans_elapsed = detect_started.elapsed();
        let telemetry = self.telemetry.clone();
        record_phase(telemetry.as_ref(), EncodePhase::Spans, spans_elapsed);

        // The fused pass may have kept clean rows in its scratch; a merge can
        // make such a row part of a rectangle, so those rows are converted here
        // (a no-op unless the final rectangle set reaches a row the fused pass
        // did not write).
        repair_stale_rows(workspace, &mut self.stale_rows, frame, &self.rects);

        // Steps F–G — spans, op build, refresh budget.
        let pack_started = Instant::now();
        let budget_px = u64::from(self.config.refresh_cells);
        let mut changed_pixels = 0_u64;
        let mut differing_sent = 0_u64;
        let mut accepted_ops = 0_usize;
        let mut stopped = false;
        let mut regions: Vec<Region> = Vec::with_capacity(self.rects.len());
        let mut writes = std::mem::take(&mut self.writes);
        writes.clear();
        let mut compress_time = Duration::ZERO;
        let mut selfcheck_failed = false;
        #[cfg(test)]
        if self.injected_selfcheck_failure {
            // The test-only injection reports through the same handler below
            // (count, log, return the write staging buffer) as a real
            // self-check failure.
            selfcheck_failed = true;
            // Injected by a test: the failure path is exercised below.
        }

        for rect in self.rects.iter().copied() {
            let mut first_written: Option<u32> = None;
            let mut last_written = 0_u32;
            let mut ops = std::mem::take(&mut self.region_ops);
            ops.clear();
            for row in rect.y..rect.y + rect.height {
                let changed = row_spans(
                    workspace,
                    base_buffer,
                    row,
                    rect.x,
                    rect.width,
                    &mut self.spans,
                );
                changed_pixels += changed;
                if stopped || self.spans.is_empty() {
                    continue;
                }
                let values = workspace.row(row);
                let local = &values[rect.x as usize..(rect.x + rect.width) as usize];
                let row_px: u64 = self.spans.iter().map(|span| u64::from(span.len)).sum();
                self.row_ops.clear();
                let region_row = match first_written {
                    Some(first) => row - first,
                    None => 0,
                };
                emit_spans(&mut self.row_ops, region_row, local, &self.spans, None);
                let row_ops = self.row_ops.len();
                let over_budget = differing_sent + row_px > budget_px
                    || accepted_ops + row_ops > DELTA_OPS_BUDGET_BYTES
                    || ops.len() + row_ops > REGION_OPS_CAP;
                if over_budget {
                    if differing_sent == 0 {
                        // Progress guarantee: always carry the first row, cut
                        // at the minimum-cells floor.
                        let truncated = truncate_spans(
                            &self.spans,
                            DELTA_REFRESH_MIN_CELLS.min(budget_px),
                            &mut self.limited,
                        );
                        self.row_ops.clear();
                        emit_spans(&mut self.row_ops, 0, local, &self.limited, None);
                        ops.extend_from_slice(&self.row_ops);
                        let emitted = truncated;
                        differing_sent += emitted;
                        accepted_ops += self.row_ops.len();
                        first_written = Some(row);
                        last_written = row;
                        record_writes(row, width, rect.x, &self.limited, &mut writes);
                    }
                    stopped = true;
                    continue;
                }
                if first_written.is_none() {
                    first_written = Some(row);
                }
                last_written = row;
                ops.extend_from_slice(&self.row_ops);
                differing_sent += row_px;
                accepted_ops += row_ops;
                record_writes(row, width, rect.x, &self.spans, &mut writes);
            }

            let Some(first_written) = first_written else {
                self.region_ops = ops;
                continue;
            };
            let region_height = last_written - first_written + 1;
            emit_end(&mut ops);
            if let Err(error) =
                validate_region_ops(&ops, rect.width, region_height, Coverage::Sparse)
            {
                selfcheck_failed = true;
                self.selfcheck_failures += 1;
                self.telemetry().record(CodecEvent::SelfCheckFailed {
                    what: "the delta region's op stream",
                    detail: Some(format!("{error:?}")),
                });
                self.region_ops = ops;
                break;
            }
            let compress_started = Instant::now();
            let mut payload = Vec::with_capacity(ops.len() / 2 + 64);
            self.deflater
                .deflate_into(&ops, self.config.delta_deflate_level, &mut payload)?;
            compress_time += compress_started.elapsed();
            if payload.len() > MAX_REGION_WIRE_BYTES {
                selfcheck_failed = true;
                self.selfcheck_failures += 1;
                self.region_ops = ops;
                break;
            }
            regions.push(Region {
                x: rect.x,
                y: first_written,
                width: rect.width,
                height: region_height,
                payload,
            });
            self.region_ops = ops;
        }
        record_phase(self.telemetry(), EncodePhase::Pack, pack_started.elapsed());
        record_phase(self.telemetry(), EncodePhase::Compress, compress_time);

        if selfcheck_failed {
            self.selfcheck_failures += 1;
            self.telemetry().record(CodecEvent::SelfCheckFailed {
                what: "the op-stream self-check",
                detail: None,
            });
            self.writes = writes;
            self.writes.clear();
            return Ok(None);
        }

        let backlog_pixels = if stopped {
            changed_pixels.saturating_sub(differing_sent)
        } else {
            0
        };
        self.last_frame_at = Some(Instant::now());
        let undo = undo_from_spans(base_buffer, &writes)?;
        Ok(Some(PlannedUpdate {
            update_base,
            regions,
            writes,
            undo,
            changed_pixels,
            sent_pixels: differing_sent,
            backlog_pixels,
            reset_reasons: ResetReasons::none(),
            update: match base {
                DeltaBase::Reference => UpdateTarget::Masked,
                DeltaBase::Scratch => UpdateTarget::RebaseScratch,
            },
            width,
            height,
        }))
    }
}

/// Record the cell runs one row's spans write, in frame coordinates.
fn record_writes(row: u32, width: u32, x0: u32, spans: &[Span], out: &mut Vec<WriteSpan>) {
    let row_base = row * width + x0;
    for span in spans {
        out.push(WriteSpan::new(row_base + span.x, span.len));
    }
}

/// Convert every stale row the final rectangle set covers.
///
/// [`ScreenStreamEncoder::convert_and_detect`] keeps a converted row out of the
/// workspace when no tile of its band is dirty. Merging (or the last-resort
/// bounding box) can still pull such a row into a rectangle's box, so any stale
/// row inside the rectangle set is converted here before spans are read from
/// it. On the ordinary path no rectangle reaches a stale row and this does
/// nothing.
fn repair_stale_rows(
    workspace: &mut Surface,
    stale: &mut [u64],
    frame: &CapturedFrame,
    rects: &[Rect],
) {
    if stale.iter().all(|word| *word == 0) {
        return;
    }
    let row_bytes = frame.width as usize * 4;
    for rect in rects {
        for y in rect.y..rect.y + rect.height {
            let index = y as usize;
            let word = index / 64;
            let bit = 1_u64 << (index % 64);
            if stale[word] & bit == 0 {
                continue;
            }
            let start = index * row_bytes;
            convert_pixels(&frame.rgba[start..start + row_bytes], workspace.row_mut(y));
            stale[word] &= !bit;
        }
    }
}

/// Collapse the rectangle set into one bounding box (the last-resort cap case).
fn collapse_to_bounding_box(rects: &mut Vec<Rect>) {
    if let Some(bounding) = bounding_box(rects) {
        rects.clear();
        rects.push(bounding);
    }
}

/// Record one encode phase.
fn record_phase(telemetry: &dyn Telemetry, phase: EncodePhase, elapsed: Duration) {
    telemetry.record(CodecEvent::PhaseElapsed { phase, elapsed });
}

impl std::fmt::Debug for ScreenStreamEncoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScreenStreamEncoder")
            .field("config", &self.config)
            .field("reference_update_id", &self.reference_update_id)
            .field("selfcheck_failures", &self.selfcheck_failures)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::SurfaceCanvas;
    use crate::ops::{emit_end, emit_fill, emit_row};

    /// A deterministic office-like frame: flat surfaces and text-like ink.
    fn office(width: u32, height: u32, seed: u32) -> CapturedFrame {
        let mut rgba = vec![0_u8; (width * height * 4) as usize];
        for cell in rgba.chunks_exact_mut(4) {
            cell[3] = 0xff;
        }
        let mut state = seed | 1;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        for y in 0..height {
            for x in 0..width {
                let color = if x < 240.min(width) {
                    [32, 34, 38]
                } else {
                    [244, 244, 246]
                };
                let offset = ((y * width + x) * 4) as usize;
                rgba[offset] = color[0];
                rgba[offset + 1] = color[1];
                rgba[offset + 2] = color[2];
            }
        }
        let mut y = 20;
        while y + 12 < height {
            let mut x = 260;
            while x + 8 < width {
                if next() % 100 < 22 {
                    for glyph_y in 0..12 {
                        for glyph_x in 0..6 {
                            let offset = (((y + glyph_y) * width) + x + glyph_x) as usize * 4;
                            rgba[offset..offset + 3].copy_from_slice(&[20, 20, 24]);
                        }
                    }
                }
                x += 9;
            }
            y += 19;
        }
        CapturedFrame::new(width, height, rgba).unwrap()
    }

    fn fill_rect(frame: &mut CapturedFrame, x: u32, y: u32, w: u32, h: u32, color: [u8; 4]) {
        for row in y..(y + h).min(frame.height) {
            for column in x..(x + w).min(frame.width) {
                let offset = ((row * frame.width + column) * 4) as usize;
                frame.rgba[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }

    fn codec_config() -> CodecConfig {
        CodecConfig {
            delta_deflate_level: 0,
            reset_deflate_level: 0,
            ..CodecConfig::default()
        }
    }

    fn new_encoder() -> ScreenStreamEncoder {
        ScreenStreamEncoder::new(codec_config())
    }

    /// The the 16-bit cell format form of a processed frame (the wire-fidelity expectation).
    fn expect565(frame: &CapturedFrame) -> Vec<u16> {
        let mut buffer = Surface::new(frame.width, frame.height).unwrap();
        rgba_to_cells(&frame.rgba, frame.width, frame.height, &mut buffer).unwrap();
        buffer.as_u16().to_vec()
    }

    /// First differing index and the two values, for a compact failure.
    fn first_mismatch(left: &[u16], right: &[u16]) -> Option<(usize, u16, u16)> {
        if left.len() != right.len() {
            return Some((left.len().min(right.len()), 0, 0));
        }
        left.iter()
            .zip(right)
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(index, (a, b))| (index, *a, *b))
    }

    #[test]
    fn first_frame_is_a_reset_that_decodes_to_the_source() {
        let mut encoder = new_encoder();
        let frame = office(320, 240, 1);
        let planned = encoder.encode(&frame).unwrap().expect("first frame");
        let encoded = encoder.commit(planned, 1);
        assert!(encoded.is_reset());
        assert_eq!(encoded.kind(), UpdateKind::Reset);
        assert!(encoded.reset_reasons.contains(ResetReason::First));
        let mut canvas = SurfaceCanvas::new();
        canvas.apply(&encoded).unwrap();
        assert_eq!(canvas.cells(), expect565(&frame));
        assert_eq!(canvas.applied_update_id(), 1);
    }

    #[test]
    fn unchanged_frame_plans_an_idle_frame_once_per_interval() {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            idle_update_interval: Duration::from_secs(60),
            ..codec_config()
        });
        let frame = office(64, 48, 2);
        let planned = encoder.encode(&frame).unwrap().expect("self-contained");
        let _ = encoder.commit(planned, 1);
        assert!(
            encoder.encode(&frame).unwrap().is_none(),
            "an unchanged frame inside the idle interval produces no work"
        );

        encoder.set_config(CodecConfig {
            idle_update_interval: Duration::ZERO,
            ..codec_config()
        });
        let planned = encoder.encode(&frame).unwrap().expect("idle frame");
        let encoded = encoder.commit(planned, 2);
        assert_eq!(encoded.kind(), UpdateKind::Empty);
        assert!(encoded.regions.is_empty());
        assert_eq!(encoded.update_base, 1);
        assert_eq!(encoded.payload_bytes(), 0);
    }

    #[test]
    fn a_cursor_move_sends_only_the_changed_pixels() {
        let mut encoder = new_encoder();
        let mut frame = office(640, 480, 3);
        fill_rect(&mut frame, 100, 100, 14, 22, [255, 255, 255, 255]);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);

        fill_rect(&mut frame, 101, 100, 14, 22, [255, 255, 255, 255]);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        assert_eq!(planned.update_base, 1);
        let encoded = encoder.commit(planned, 2);
        assert!(!encoded.is_reset());
        assert!(encoded.changed_pixels > 0);
        assert_eq!(
            encoded.changed_pixels, encoded.sent_pixels,
            "exact cells: nothing but changed cells is carried"
        );
        assert!(encoded.changed_pixels < 400, "{}", encoded.changed_pixels);
        assert!(encoded.payload_bytes() < 800, "{}", encoded.payload_bytes());
    }

    #[test]
    fn every_delta_decodes_to_the_source_and_reference_equality_holds() {
        let mut encoder = new_encoder();
        let mut canvas = SurfaceCanvas::new();
        let mut frame = office(128, 96, 5);
        for step in 0..10_u64 {
            match step {
                0 => {}
                1 => fill_rect(&mut frame, 0, 0, 128, 40, [12, 14, 20, 255]),
                2 => fill_rect(&mut frame, 30, 20, 60, 30, [250, 250, 250, 255]),
                _ => fill_rect(
                    &mut frame,
                    4 * step as u32,
                    3,
                    9,
                    9,
                    [step as u8, 90, 200, 255],
                ),
            }
            let planned = encoder.encode(&frame).unwrap().expect("change");
            let encoded = encoder.commit(planned, step + 1);
            canvas.apply(&encoded).unwrap();
            assert_eq!(canvas.applied_update_id(), step + 1);
            assert_eq!(
                canvas.cells(),
                encoder
                    .reference
                    .as_ref()
                    .expect("reference after commit")
                    .as_u16(),
                "INV-R: the receiver canvas equals the sender's reference after every frame"
            );
        }
    }

    #[test]
    fn rebase_decodes_to_the_same_pixels_as_a_reset_frame() {
        let mut encoder = new_encoder();
        let mut receiver = SurfaceCanvas::new();
        let mut frame = office(96, 64, 11);
        for step in 1..=5_u64 {
            if step > 1 {
                fill_rect(
                    &mut frame,
                    5 * step as u32,
                    4,
                    12,
                    10,
                    [10, 20, step as u8, 255],
                );
            }
            let planned = encoder.encode(&frame).unwrap().unwrap();
            let encoded = encoder.commit(planned, step);
            receiver.apply(&encoded).unwrap();
        }
        // The receiver holds frame 3; the loss is healed without a full repaint.
        let held = 3;
        let mut receiver_at_held = SurfaceCanvas::new();
        for step in 1..=held {
            let mut fresh = new_encoder();
            let content = {
                // Rebuild the content of that step from the same script.
                let mut content = office(96, 64, 11);
                for inner in 2..=step {
                    fill_rect(
                        &mut content,
                        5 * inner as u32,
                        4,
                        12,
                        10,
                        [10, 20, inner as u8, 255],
                    );
                }
                content
            };
            let planned = fresh.encode(&content).unwrap().unwrap();
            receiver_at_held
                .apply(&fresh.commit(planned, step))
                .unwrap();
        }

        let next = {
            let mut content = office(96, 64, 11);
            for step in 2..=6_u64 {
                fill_rect(
                    &mut content,
                    5 * step as u32,
                    4,
                    12,
                    10,
                    [10, 20, step as u8, 255],
                );
            }
            content
        };
        assert!(encoder.journal_can_reach(held));
        let planned = encoder.rebase_and_encode(held, &next).unwrap().unwrap();
        assert_eq!(planned.update_base, held);
        let rebased = encoder.commit(planned, 6);
        assert_eq!(rebased.update_base, held);
        receiver_at_held.apply(&rebased).unwrap();

        // The rebased frame paints exactly the source content, cell for cell.
        let expected = expect565(&next);
        assert_eq!(
            first_mismatch(receiver_at_held.cells(), &expected),
            None,
            "the rebased frame must paint the source content"
        );

        // The same content as a reset frame paints the same cells.
        let mut fresh = new_encoder();
        let planned = fresh.encode(&next).unwrap().unwrap();
        let reset = fresh.commit(planned, 6);
        let mut reset_canvas = SurfaceCanvas::new();
        reset_canvas.apply(&reset).unwrap();
        assert_eq!(
            first_mismatch(receiver_at_held.cells(), reset_canvas.cells()),
            None,
            "rebase and reset must agree"
        );
        assert!(
            receiver.applied_update_id() == 5,
            "the live receiver stopped at frame 5"
        );
    }

    #[test]
    fn unreachable_bases_return_none() {
        let mut encoder = new_encoder();
        let frame = office(64, 48, 13);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);
        assert!(!encoder.journal_can_reach(0));
        assert!(!encoder.journal_can_reach(5));
        assert!(encoder.journal_can_reach(1));
        assert!(encoder.rebase_and_encode(0, &frame).unwrap().is_none());
        assert!(encoder.rebase_and_encode(9, &frame).unwrap().is_none());
    }

    #[test]
    fn a_pending_reset_suppresses_rebases() {
        let mut encoder = new_encoder();
        let frame = office(64, 48, 17);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);
        encoder.reset(ResetReason::Geometry);
        assert!(encoder.reset_pending());
        assert!(!encoder.journal_can_reach(0));
        assert!(!encoder.journal_can_reach(1));
        assert!(encoder.rebase_and_encode(0, &frame).unwrap().is_none());
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let encoded = encoder.commit(planned, 2);
        assert!(encoded.is_reset());
        assert!(!encoder.reset_pending());
        assert!(encoder.journal_frames() == 0, "a reset clears the journal");
    }

    #[test]
    fn geometry_changes_produce_a_reset_for_the_new_size() {
        let mut encoder = new_encoder();
        let planned = encoder.encode(&office(80, 60, 19)).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);
        let planned = encoder.encode(&office(40, 30, 19)).unwrap().unwrap();
        let encoded = encoder.commit(planned, 2);
        assert!(encoded.is_reset());
        assert_eq!((encoded.width, encoded.height), (40, 30));
        assert!(encoded.reset_reasons.contains(ResetReason::Geometry));
    }

    #[test]
    fn the_budget_bounds_a_frame_and_reports_the_backlog() {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            refresh_cells: DELTA_REFRESH_MIN_CELLS as u32,
            ..codec_config()
        });
        let mut frame = office(240, 160, 23);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);
        fill_rect(&mut frame, 0, 0, 240, 160, [90, 10, 10, 255]);

        let planned = encoder.encode(&frame).unwrap().unwrap();
        let first = encoder.commit(planned, 2);
        assert!(
            first.sent_pixels <= DELTA_REFRESH_MIN_CELLS + 240,
            "sent {}",
            first.sent_pixels
        );
        assert!(first.changed_pixels > first.sent_pixels);
        assert_eq!(
            first.sent_pixels + first.backlog_pixels,
            first.changed_pixels
        );
        // A second frame makes progress again and stays inside the budget
        // plus one row (the documented §9.2 bound).
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let second = encoder.commit(planned, 3);
        assert!(second.sent_pixels > 0);
        assert!(second.sent_pixels <= DELTA_REFRESH_MIN_CELLS + 240);
        assert!(second.backlog_pixels < first.backlog_pixels);
    }

    #[test]
    fn caps_hold_for_every_region() {
        let mut encoder = new_encoder();
        let mut frame = office(320, 240, 29);
        for step in 0..6_u64 {
            fill_rect(
                &mut frame,
                step as u32 * 7,
                5,
                30,
                30,
                [step as u8, 200, 30, 255],
            );
            let Some(planned) = encoder.encode(&frame).unwrap() else {
                continue;
            };
            let encoded = encoder.commit(planned, step + 1);
            assert!(encoded.regions.len() <= MAX_REGIONS_PER_UPDATE);
            let mut scratch = Vec::new();
            for region in &encoded.regions {
                assert!(region.payload.len() <= MAX_REGION_WIRE_BYTES);
                assert!(region.width > 0 && region.height > 0);
                assert!(region.x + region.width <= encoded.width);
                assert!(region.y + region.height <= encoded.height);
                let mut out = vec![0_u16; (region.width * region.height) as usize];
                crate::decode::decode_region_ops(
                    &region.payload,
                    region.width,
                    region.height,
                    Coverage::Sparse,
                    &mut out,
                    &mut scratch,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn merged_rectangles_decode_to_the_source() {
        // A checkerboard of single-cell changes across a 20x15 tile grid: 150
        // disjoint rectangles, far above the 64-region cap. Merging them makes
        // bounding boxes that span rows, tiles and whole bands no change ever
        // touched — exactly the rows the fused pass keeps in its row scratch.
        let mut encoder = new_encoder();
        let mut frame = office(1280, 960, 41);
        for ty in 0..15_u32 {
            for tx in 0..20_u32 {
                if (tx + ty) % 2 == 0 {
                    fill_rect(
                        &mut frame,
                        tx * 64 + 7,
                        ty * 64 + 3,
                        1,
                        1,
                        [255, 0, 255, 255],
                    );
                }
            }
        }
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let reset = encoder.commit(planned, 1);
        let mut canvas = SurfaceCanvas::new();
        canvas.apply(&reset).unwrap();

        // One more cell on a different tile, so the frame is a delta whose
        // rectangle list hits the merge path.
        fill_rect(&mut frame, 1200, 900, 1, 1, [0, 255, 0, 255]);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let delta = encoder.commit(planned, 2);
        assert!(delta.regions.len() <= MAX_REGIONS_PER_UPDATE);
        canvas.apply(&delta).unwrap();
        assert_eq!(
            first_mismatch(canvas.cells(), &expect565(&frame)),
            None,
            "a merged rectangle must still paint the source frame exactly"
        );
    }

    #[test]
    fn stale_rows_inside_a_rectangle_are_converted_and_others_are_left_alone() {
        // The fused pass keeps a clean row in its scratch (marking it stale)
        // when no tile of its band is dirty. A merged or collapsed rectangle
        // can still cover such a row; the repair converts exactly those rows.
        let frame = office(256, 256, 43);
        let mut workspace = Surface::new(256, 256).unwrap();
        workspace.fill(0xdead);
        let mut stale = vec![0_u64; 4];
        for y in [3_u32, 64, 65, 200] {
            stale[(y / 64) as usize] |= 1 << (y % 64);
        }
        let rects = vec![
            // Covers stale rows 64 and 65; row 66 is not stale.
            Rect::new(0, 64, 256, 4),
            // Covers no stale row at all.
            Rect::new(0, 128, 256, 2),
        ];
        let mut expected = Surface::new(256, 256).unwrap();
        rgba_to_cells(&frame.rgba, 256, 256, &mut expected).unwrap();

        repair_stale_rows(&mut workspace, &mut stale, &frame, &rects);

        assert_eq!(workspace.row(64), expected.row(64), "stale row 64 repaired");
        assert_eq!(workspace.row(65), expected.row(65), "stale row 65 repaired");
        // A row inside a rectangle that the fused pass did write is untouched
        // by the repair (the sentinel stands for the fresh converted row).
        assert!(workspace.row(66).iter().all(|value| *value == 0xdead));
        assert!(workspace.row(128).iter().all(|value| *value == 0xdead));
        // Stale rows outside the rectangle set stay as they were (they are
        // never read) and their bits stay set.
        assert!(workspace.row(3).iter().all(|value| *value == 0xdead));
        assert!(workspace.row(200).iter().all(|value| *value == 0xdead));
        assert_ne!(stale[0] & (1 << 3), 0);
        assert_ne!(stale[3] & (1 << (200 % 64)), 0);
        assert_eq!(stale[1] & (1 << (64 % 64)), 0);
        assert_eq!(stale[1] & (1 << (65 % 64)), 0);
    }

    #[test]
    fn the_shared_validator_rejects_malformed_op_streams() {
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_fill(&mut ops, 0, 4, 1);
        emit_fill(&mut ops, 2, 2, 2);
        emit_end(&mut ops);
        assert!(validate_region_ops(&ops, 8, 1, Coverage::Sparse).is_err());
        let mut encoder = new_encoder();
        let frame = office(64, 48, 31);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);
        assert_eq!(encoder.selfcheck_failures(), 0);
    }

    #[test]
    fn config_knobs_are_clamped_and_read_from_the_environment() {
        let config = CodecConfig {
            refresh_cells: 1,
            delta_deflate_level: 200,
            reset_deflate_level: 200,
            encoder_threads: 99,
            idle_update_interval: Duration::ZERO,
        }
        .clamped();
        assert_eq!(config.refresh_cells, CodecConfig::MIN_REFRESH_PIXELS);
        assert_eq!(config.delta_deflate_level, 9);
        assert_eq!(config.reset_deflate_level, 9);
        assert_eq!(config.encoder_threads, 8);
        let config = CodecConfig::from_lookup(|key| match key {
            "TESSELLIO_REFRESH_CELLS" => Some("32768".to_string()),
            "TESSELLIO_DELTA_DEFLATE_LEVEL" => Some("1".to_string()),
            "TESSELLIO_RESET_DEFLATE_LEVEL" => Some("2".to_string()),
            "TESSELLIO_ENCODER_THREADS" => Some("3".to_string()),
            "TESSELLIO_IDLE_UPDATE_MS" => Some("500".to_string()),
            _ => None,
        });
        assert_eq!(config.refresh_cells, 32768);
        assert_eq!(config.delta_deflate_level, 1);
        assert_eq!(config.reset_deflate_level, 2);
        assert_eq!(config.encoder_threads, 3);
        assert_eq!(config.idle_update_interval, Duration::from_millis(500));
    }

    #[test]
    fn the_journal_reports_its_bounds() {
        let mut encoder = new_encoder();
        let mut frame = office(64, 48, 37);
        let planned = encoder.encode(&frame).unwrap().unwrap();
        let _ = encoder.commit(planned, 1);
        assert_eq!(encoder.journal_frames(), 0, "a reset clears the journal");
        for step in 2..=4_u64 {
            fill_rect(&mut frame, step as u32, 1, 5, 5, [200, 10, 10, 255]);
            let planned = encoder.encode(&frame).unwrap().unwrap();
            let _ = encoder.commit(planned, step);
        }
        assert_eq!(encoder.journal_frames(), 3);
        assert!(encoder.journal_bytes() > 0);
        assert_eq!(encoder.reference_update_id(), 4);
    }
}
