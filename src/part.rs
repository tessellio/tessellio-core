//! Frame parting, part identity, and the receiver-side validation/assembly
//! oracle.
//!
//! A frame is split at region boundaries only — a region is never split — and
//! every part carries the whole frame's identity (`update_id`,
//! `update_base`, geometry, `part_count`, `update_bytes`) so any receiver can
//! prove atomicity without holding partial frames. [`validate_part`] and
//! [`UpdateAssembler`] implement §4.5's receiver rules in Rust; they are the
//! model-test and corpus oracle, and mirror what the hops and the browser must
//! do.

use crate::encode::EncodedUpdate;
use crate::error::CodecError;
use crate::format::{
    Codec, MAX_PART_WIRE_BYTES, MAX_PARTS_PER_UPDATE, MAX_REGION_WIRE_BYTES, MAX_REGIONS_PER_PART,
    MAX_REGIONS_PER_UPDATE, MAX_SURFACE_CELLS, MAX_SURFACE_DIMENSION, MAX_UPDATE_WIRE_BYTES,
    Region,
};

/// One part of one frame, as it travels on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Frame id (`>= 1`), replicated in every part.
    pub update_id: u64,
    /// Frame this delta applies onto; `0` marks a reset frame.
    pub update_base: u64,
    /// Frame width in cells.
    pub width: u32,
    /// Frame height in cells.
    pub height: u32,
    /// Part index (`0..part_count`).
    pub part_index: u32,
    /// Parts in the frame.
    pub part_count: u32,
    /// Sum of region payload bytes over the whole frame.
    pub update_bytes: u64,
    /// Codec of every region.
    pub codec: Codec,
    /// This part's regions, ascending by `(y, x)`.
    pub regions: Vec<Region>,
}

impl Part {
    /// Payload bytes carried by this part.
    pub fn payload_bytes(&self) -> usize {
        self.regions.iter().map(|region| region.payload.len()).sum()
    }
}

/// Splits encoded frames into bounded parts at region boundaries.
#[derive(Debug, Clone)]
pub struct UpdateSplitter {
    /// Payload budget per part.
    pub max_part_bytes: usize,
    /// Region ceiling per part.
    pub max_regions_per_part: usize,
}

impl Default for UpdateSplitter {
    fn default() -> Self {
        Self {
            max_part_bytes: MAX_PART_WIRE_BYTES,
            max_regions_per_part: MAX_REGIONS_PER_PART,
        }
    }
}

impl UpdateSplitter {
    /// A splitter with the format's defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Split one encoded frame into parts, in order.
    ///
    /// The empty frame (`regions = []`) becomes a single empty part, which is
    /// all the idle-frame machinery needs.
    pub fn part(&self, frame: EncodedUpdate) -> Result<Vec<Part>, CodecError> {
        let total: usize = frame.regions.iter().map(Region::payload_bytes).sum();
        if total > MAX_UPDATE_WIRE_BYTES {
            return Err(CodecError::TooLarge {
                bytes: total,
                limit: MAX_UPDATE_WIRE_BYTES,
            });
        }
        if frame.regions.len() > MAX_REGIONS_PER_UPDATE {
            return Err(CodecError::TooLarge {
                bytes: frame.regions.len(),
                limit: MAX_REGIONS_PER_UPDATE,
            });
        }
        let update_bytes = total as u64;
        let mut groups: Vec<Vec<Region>> = Vec::new();
        let mut current: Vec<Region> = Vec::new();
        let mut current_bytes = 0_usize;
        for region in frame.regions {
            if region.payload.len() > self.max_part_bytes {
                return Err(CodecError::TooLarge {
                    bytes: region.payload.len(),
                    limit: self.max_part_bytes,
                });
            }
            let full = !current.is_empty()
                && (current_bytes + region.payload.len() > self.max_part_bytes
                    || current.len() >= self.max_regions_per_part);
            if full {
                groups.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            current_bytes += region.payload.len();
            current.push(region);
        }
        if !current.is_empty() || groups.is_empty() {
            groups.push(current);
        }
        if groups.len() > MAX_PARTS_PER_UPDATE {
            return Err(CodecError::TooLarge {
                bytes: groups.len(),
                limit: MAX_PARTS_PER_UPDATE,
            });
        }
        let part_count = groups.len() as u32;
        Ok(groups
            .into_iter()
            .enumerate()
            .map(|(index, regions)| Part {
                update_id: frame.update_id,
                update_base: frame.update_base,
                width: frame.width,
                height: frame.height,
                part_index: index as u32,
                part_count,
                update_bytes,
                codec: frame.codec,
                regions,
            })
            .collect())
    }
}

/// Why a part was rejected (§4.5 steps 1–10 and the assemble rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartRejection {
    /// Codec id is not `tessellio`.
    UnknownCodec,
    /// `update_id` is zero.
    BadId,
    /// `update_base` is neither zero nor below `update_id`.
    BadBase,
    /// Dimensions are outside the format limits.
    BadGeometry,
    /// `part_index`/`part_count` are inconsistent.
    BadPart,
    /// `update_bytes` is negative or beyond the frame cap.
    BadSize,
    /// More regions than a part may carry.
    TooManyRegions,
    /// A region is empty or lies outside the frame.
    BadRegion,
    /// A payload is empty or beyond the region wire cap.
    BadPayload,
    /// Regions are not ascending by `(y, x)` and pairwise disjoint.
    RegionOrder,
    /// Assembly produced fewer or more bytes than `update_bytes`.
    Incomplete,
    /// A frame's first part was never seen.
    MissedFirstPart,
    /// The sender restarted mid-frame (a different frame id before completion).
    Interrupted,
}

impl PartRejection {
    /// The receiver's resync reason this rejection maps to.
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::UnknownCodec => "UNKNOWN_CODEC",
            Self::BadBase => "BASE_MISMATCH",
            Self::BadGeometry => "GEOMETRY_MISMATCH",
            Self::BadPart | Self::MissedFirstPart => "PART_GAP",
            Self::Incomplete => "INCOMPLETE_UPDATE",
            Self::BadId
            | Self::BadSize
            | Self::TooManyRegions
            | Self::BadRegion
            | Self::BadPayload
            | Self::RegionOrder
            | Self::Interrupted => "DECODE_ERROR",
        }
    }
}

/// §4.5 steps 1–10: reject the whole frame on the first violation.
pub fn validate_part(part: &Part) -> Result<(), PartRejection> {
    if part.codec != Codec::Tessellio {
        return Err(PartRejection::UnknownCodec);
    }
    if part.update_id == 0 {
        return Err(PartRejection::BadId);
    }
    if part.update_base != 0 && part.update_base >= part.update_id {
        return Err(PartRejection::BadBase);
    }
    if part.width == 0
        || part.height == 0
        || part.width > MAX_SURFACE_DIMENSION
        || part.height > MAX_SURFACE_DIMENSION
        || u64::from(part.width) * u64::from(part.height) > MAX_SURFACE_CELLS
    {
        return Err(PartRejection::BadGeometry);
    }
    if part.part_count == 0
        || part.part_count as usize > MAX_PARTS_PER_UPDATE
        || part.part_index >= part.part_count
    {
        return Err(PartRejection::BadPart);
    }
    if part.update_bytes > MAX_UPDATE_WIRE_BYTES as u64 {
        return Err(PartRejection::BadSize);
    }
    if part.regions.len() > MAX_REGIONS_PER_PART {
        return Err(PartRejection::TooManyRegions);
    }
    for region in &part.regions {
        if region.width == 0
            || region.height == 0
            || region.x + region.width > part.width
            || region.y + region.height > part.height
        {
            return Err(PartRejection::BadRegion);
        }
        if region.payload.is_empty() || region.payload.len() > MAX_REGION_WIRE_BYTES {
            return Err(PartRejection::BadPayload);
        }
    }
    for (index, region) in part.regions.iter().enumerate() {
        for other in part.regions.iter().skip(index + 1) {
            let key = (region.y, region.x);
            if key >= (other.y, other.x) {
                return Err(PartRejection::RegionOrder);
            }
            let intersects = region.x < other.x + other.width
                && other.x < region.x + region.width
                && region.y < other.y + other.height
                && other.y < region.y + region.height;
            if intersects {
                return Err(PartRejection::RegionOrder);
            }
        }
    }
    Ok(())
}

/// A completed frame, assembled from its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledUpdate {
    /// Frame id.
    pub update_id: u64,
    /// Base frame id (`0` = reset frame).
    pub update_base: u64,
    /// Frame width in cells.
    pub width: u32,
    /// Frame height in cells.
    pub height: u32,
    /// Sum of region payload bytes.
    pub update_bytes: u64,
    /// Parts that carried the frame.
    pub parts: u32,
    /// Every region, in part order.
    pub regions: Vec<Region>,
}

/// Result of feeding one part to a [`UpdateAssembler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyOutcome {
    /// The frame is not complete yet.
    Pending,
    /// The frame is complete and byte-exact.
    Complete(AssembledUpdate),
    /// The frame (and any partial frame) was rejected.
    Rejected(PartRejection),
}

/// Receiver-side frame assembly (§4.5's assemble rule).
#[derive(Debug, Default)]
pub struct UpdateAssembler {
    pending: Option<PendingFrame>,
}

#[derive(Debug)]
struct PendingFrame {
    update_id: u64,
    update_base: u64,
    width: u32,
    height: u32,
    part_count: u32,
    update_bytes: u64,
    next_index: u32,
    regions: Vec<Region>,
    bytes: usize,
}

impl UpdateAssembler {
    /// An assembler holding no partial frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop any partial frame (a receiver reset).
    pub fn reset(&mut self) {
        self.pending = None;
    }

    /// The frame id currently being assembled, if any.
    pub fn pending_update_id(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.update_id)
    }

    /// Feed one part.
    ///
    /// A `part_index == 0` for a different frame id discards a partial frame
    /// and starts the new one; a later part of a different frame id is refused
    /// (`Interrupted`), and a later part with no first part seen is refused
    /// (`MissedFirstPart`).
    pub fn push(&mut self, part: Part) -> AssemblyOutcome {
        if let Err(rejection) = validate_part(&part) {
            self.pending = None;
            return AssemblyOutcome::Rejected(rejection);
        }
        if part.part_index == 0 {
            self.pending = Some(PendingFrame::start(&part));
            if part.part_count == 1 {
                return self.finish();
            }
            return AssemblyOutcome::Pending;
        }
        let Some(pending) = self.pending.as_mut() else {
            return AssemblyOutcome::Rejected(PartRejection::MissedFirstPart);
        };
        if pending.update_id != part.update_id {
            self.pending = None;
            return AssemblyOutcome::Rejected(PartRejection::Interrupted);
        }
        if part.part_index != pending.next_index {
            self.pending = None;
            return AssemblyOutcome::Rejected(PartRejection::MissedFirstPart);
        }
        if pending.width != part.width
            || pending.height != part.height
            || pending.part_count != part.part_count
            || pending.update_base != part.update_base
            || pending.update_bytes != part.update_bytes
        {
            self.pending = None;
            return AssemblyOutcome::Rejected(PartRejection::BadPart);
        }
        if pending.regions.len() + part.regions.len() > MAX_REGIONS_PER_UPDATE {
            self.pending = None;
            return AssemblyOutcome::Rejected(PartRejection::TooManyRegions);
        }
        let part_bytes = part.payload_bytes();
        if pending.bytes + part_bytes > pending.update_bytes as usize {
            self.pending = None;
            return AssemblyOutcome::Rejected(PartRejection::Incomplete);
        }
        pending.bytes += part_bytes;
        pending.regions.extend(part.regions);
        pending.next_index += 1;
        if pending.next_index == pending.part_count {
            return self.finish();
        }
        AssemblyOutcome::Pending
    }

    /// Complete the pending frame, enforcing `Σ payload == update_bytes`.
    fn finish(&mut self) -> AssemblyOutcome {
        let Some(pending) = self.pending.take() else {
            return AssemblyOutcome::Rejected(PartRejection::MissedFirstPart);
        };
        if pending.bytes != pending.update_bytes as usize {
            return AssemblyOutcome::Rejected(PartRejection::Incomplete);
        }
        AssemblyOutcome::Complete(AssembledUpdate {
            update_id: pending.update_id,
            update_base: pending.update_base,
            width: pending.width,
            height: pending.height,
            update_bytes: pending.update_bytes,
            parts: pending.part_count,
            regions: pending.regions,
        })
    }
}

impl PendingFrame {
    fn start(part: &Part) -> Self {
        Self {
            update_id: part.update_id,
            update_base: part.update_base,
            width: part.width,
            height: part.height,
            part_count: part.part_count,
            update_bytes: part.update_bytes,
            next_index: 1,
            regions: part.regions.clone(),
            bytes: part.payload_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::MAX_REGION_WIRE_BYTES;

    fn region(x: u32, y: u32, size: usize) -> Region {
        Region {
            x,
            y,
            width: 1,
            height: 1,
            payload: vec![0; size],
        }
    }

    fn frame(regions: Vec<Region>) -> EncodedUpdate {
        EncodedUpdate {
            update_id: 10,
            update_base: 9,
            width: 64,
            height: 48,
            codec: Codec::Tessellio,
            regions,
            changed_pixels: 0,
            sent_pixels: 0,
            backlog_pixels: 0,
            reset_reasons: Default::default(),
        }
    }

    #[test]
    fn frames_within_budget_produce_one_part() {
        let parts = UpdateSplitter::new()
            .part(frame(vec![region(0, 0, 100), region(2, 0, 100)]))
            .unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part_index, 0);
        assert_eq!(parts[0].part_count, 1);
        assert_eq!(parts[0].update_bytes, 200);
        assert_eq!(parts[0].regions.len(), 2);
    }

    #[test]
    fn oversized_frames_split_at_region_boundaries() {
        let splitter = UpdateSplitter {
            max_part_bytes: 1024,
            max_regions_per_part: 32,
        };
        let parts = splitter
            .part(frame(vec![
                region(0, 0, 1024),
                region(1, 0, 1024),
                region(2, 0, 1024),
            ]))
            .unwrap();
        assert_eq!(parts.len(), 3);
        for (index, part) in parts.iter().enumerate() {
            assert_eq!(part.part_index, index as u32);
            assert_eq!(part.part_count, 3);
            assert_eq!(part.update_bytes, 3072);
            assert!(part.payload_bytes() <= 1024);
            assert_eq!(part.regions.len(), 1);
        }
    }

    #[test]
    fn the_region_ceiling_splits_parts() {
        let splitter = UpdateSplitter {
            max_part_bytes: MAX_PART_WIRE_BYTES,
            max_regions_per_part: 2,
        };
        let regions = (0..5).map(|x| region(x, 0, 10)).collect();
        let parts = splitter.part(frame(regions)).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].regions.len(), 2);
        assert_eq!(parts[2].regions.len(), 1);
    }

    #[test]
    fn the_empty_frame_is_one_empty_part() {
        let parts = UpdateSplitter::new().part(frame(Vec::new())).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].update_bytes, 0);
        assert!(parts[0].regions.is_empty());
        assert_eq!(parts[0].part_count, 1);
    }

    #[test]
    fn a_single_oversized_region_is_refused() {
        let splitter = UpdateSplitter {
            max_part_bytes: 1024,
            max_regions_per_part: 32,
        };
        assert!(matches!(
            splitter.part(frame(vec![region(0, 0, 2048)])),
            Err(CodecError::TooLarge { .. })
        ));
    }

    #[test]
    fn validation_accepts_a_well_formed_part() {
        let parts = UpdateSplitter::new()
            .part(frame(vec![region(0, 0, 16), region(8, 2, 16)]))
            .unwrap();
        assert_eq!(validate_part(&parts[0]), Ok(()));
    }

    #[test]
    fn validation_rejects_each_documented_violation() {
        let base = UpdateSplitter::new()
            .part(frame(vec![region(0, 0, 16)]))
            .unwrap()
            .remove(0);
        assert_eq!(validate_part(&base), Ok(()));

        let mut part = base.clone();
        part.codec = Codec::Tessellio;
        assert_eq!(validate_part(&part), Ok(()));

        let mut part = base.clone();
        part.update_id = 0;
        assert_eq!(validate_part(&part), Err(PartRejection::BadId));

        let mut part = base.clone();
        part.update_base = 11;
        assert_eq!(validate_part(&part), Err(PartRejection::BadBase));

        let mut part = base.clone();
        part.width = 0;
        assert_eq!(validate_part(&part), Err(PartRejection::BadGeometry));

        let mut part = base.clone();
        part.part_count = 0;
        assert_eq!(validate_part(&part), Err(PartRejection::BadPart));

        let mut part = base.clone();
        part.part_index = 1;
        part.part_count = 1;
        assert_eq!(validate_part(&part), Err(PartRejection::BadPart));

        let mut part = base.clone();
        part.update_bytes = MAX_UPDATE_WIRE_BYTES as u64 + 1;
        assert_eq!(validate_part(&part), Err(PartRejection::BadSize));

        let mut part = base.clone();
        part.regions = vec![region(0, 0, 16); MAX_REGIONS_PER_PART + 1];
        assert_eq!(validate_part(&part), Err(PartRejection::TooManyRegions));

        let mut part = base.clone();
        part.regions = vec![Region {
            x: 60,
            y: 0,
            width: 8,
            height: 1,
            payload: vec![0; 4],
        }];
        assert_eq!(validate_part(&part), Err(PartRejection::BadRegion));

        let mut part = base.clone();
        part.regions = vec![Region {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            payload: Vec::new(),
        }];
        assert_eq!(validate_part(&part), Err(PartRejection::BadPayload));

        let mut part = base.clone();
        part.regions = vec![Region {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            payload: vec![0; MAX_REGION_WIRE_BYTES + 1],
        }];
        assert_eq!(validate_part(&part), Err(PartRejection::BadPayload));

        let mut part = base.clone();
        part.regions = vec![region(8, 0, 16), region(0, 0, 16)];
        assert_eq!(validate_part(&part), Err(PartRejection::RegionOrder));

        let mut part = base.clone();
        part.regions = vec![region(0, 0, 16), region(0, 0, 16)];
        assert_eq!(validate_part(&part), Err(PartRejection::RegionOrder));
    }

    #[test]
    fn rejection_reason_codes_match_the_resync_enum() {
        assert_eq!(PartRejection::UnknownCodec.reason_code(), "UNKNOWN_CODEC");
        assert_eq!(PartRejection::BadBase.reason_code(), "BASE_MISMATCH");
        assert_eq!(
            PartRejection::BadGeometry.reason_code(),
            "GEOMETRY_MISMATCH"
        );
        assert_eq!(PartRejection::BadPart.reason_code(), "PART_GAP");
        assert_eq!(PartRejection::MissedFirstPart.reason_code(), "PART_GAP");
        assert_eq!(PartRejection::Incomplete.reason_code(), "INCOMPLETE_UPDATE");
        assert_eq!(PartRejection::BadRegion.reason_code(), "DECODE_ERROR");
    }

    #[test]
    fn assembly_requires_every_part_in_order() {
        let splitter = UpdateSplitter {
            max_part_bytes: 8,
            max_regions_per_part: 32,
        };
        let parts = splitter
            .part(frame(vec![region(0, 0, 8), region(1, 0, 8)]))
            .unwrap();
        assert_eq!(parts.len(), 2);

        let mut assembler = UpdateAssembler::new();
        assert_eq!(assembler.push(parts[0].clone()), AssemblyOutcome::Pending);
        match assembler.push(parts[1].clone()) {
            AssemblyOutcome::Complete(assembled) => {
                assert_eq!(assembled.update_id, 10);
                assert_eq!(assembled.update_base, 9);
                assert_eq!(assembled.update_bytes, 16);
                assert_eq!(assembled.regions.len(), 2);
                assert_eq!(assembled.parts, 2);
            }
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn a_later_part_before_the_first_is_refused() {
        let mut assembler = UpdateAssembler::new();
        let part = Part {
            update_id: 10,
            update_base: 9,
            width: 64,
            height: 48,
            part_index: 1,
            part_count: 2,
            update_bytes: 16,
            codec: Codec::Tessellio,
            regions: vec![region(1, 0, 8)],
        };
        assert_eq!(
            assembler.push(part),
            AssemblyOutcome::Rejected(PartRejection::MissedFirstPart)
        );
    }

    #[test]
    fn a_byte_count_mismatch_is_incomplete() {
        let mut assembler = UpdateAssembler::new();
        let mut part = Part {
            update_id: 10,
            update_base: 9,
            width: 64,
            height: 48,
            part_index: 0,
            part_count: 1,
            update_bytes: 32,
            codec: Codec::Tessellio,
            regions: vec![region(0, 0, 16)],
        };
        assert_eq!(
            assembler.push(part.clone()),
            AssemblyOutcome::Rejected(PartRejection::Incomplete)
        );
        part.update_bytes = 16;
        assert!(matches!(assembler.push(part), AssemblyOutcome::Complete(_)));
    }

    #[test]
    fn a_gap_between_parts_is_a_part_gap() {
        let splitter = UpdateSplitter {
            max_part_bytes: 8,
            max_regions_per_part: 32,
        };
        let parts = splitter
            .part(frame(vec![
                region(0, 0, 8),
                region(1, 0, 8),
                region(2, 0, 8),
            ]))
            .unwrap();
        assert_eq!(parts.len(), 3);
        let mut assembler = UpdateAssembler::new();
        assert_eq!(assembler.push(parts[0].clone()), AssemblyOutcome::Pending);
        assert_eq!(
            assembler.push(parts[2].clone()),
            AssemblyOutcome::Rejected(PartRejection::MissedFirstPart)
        );
        assert_eq!(assembler.pending_update_id(), None);
    }

    #[test]
    fn a_new_update_id_discards_the_partial_update() {
        let mut assembler = UpdateAssembler::new();
        let mut part = Part {
            update_id: 10,
            update_base: 9,
            width: 64,
            height: 48,
            part_index: 0,
            part_count: 2,
            update_bytes: 16,
            codec: Codec::Tessellio,
            regions: vec![region(0, 0, 8)],
        };
        assert_eq!(assembler.push(part.clone()), AssemblyOutcome::Pending);
        // A later part of another frame interrupts the partial frame.
        part.update_id = 11;
        part.update_base = 10;
        part.part_index = 1;
        assert_eq!(
            assembler.push(part.clone()),
            AssemblyOutcome::Rejected(PartRejection::Interrupted)
        );
        assert_eq!(assembler.pending_update_id(), None);
        // A first part of another frame simply starts the new frame.
        part.part_index = 0;
        assert_eq!(assembler.push(part.clone()), AssemblyOutcome::Pending);
        assert_eq!(assembler.pending_update_id(), Some(11));
        part.part_index = 1;
        assert!(matches!(assembler.push(part), AssemblyOutcome::Complete(_)));
    }

    #[test]
    fn reset_clears_the_partial_frame() {
        let mut assembler = UpdateAssembler::new();
        let part = Part {
            update_id: 10,
            update_base: 9,
            width: 64,
            height: 48,
            part_index: 0,
            part_count: 2,
            update_bytes: 16,
            codec: Codec::Tessellio,
            regions: vec![region(0, 0, 8)],
        };
        assert_eq!(assembler.push(part), AssemblyOutcome::Pending);
        assembler.reset();
        assert_eq!(assembler.pending_update_id(), None);
    }
}
