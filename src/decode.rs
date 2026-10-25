//! The `tessellio` decoder: bounded raw-DEFLATE inflate plus the op-stream walk.
//!
//! This is the conformance oracle and the only second implementation of the
//! format (the receiver's TypeScript decoder is the other side of the same
//! contract, cross-checked through the committed corpus). The walk is shared by
//! validation and application, so "the decoder accepts it" and "the decoder
//! paints it" can never disagree, and it never panics on arbitrary input.

use flate2::{Decompress, FlushDecompress, Status};

use crate::buffer::Surface;
use crate::encode::EncodedUpdate;
use crate::format::MAX_REGION_OPS_BYTES;
use crate::ops::{OP_END, OP_FILL, OP_ROW, OP_SPAN};

/// Coverage rule a region is decoded under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// `base_update_id == 0`: every row is written exactly once and completely.
    Full,
    /// Delta frame: rows and runs may be sparse, but order rules still hold.
    Sparse,
}

/// Why a region payload could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload is not a valid raw-DEFLATE stream.
    Deflate,
    /// The decompressed op stream exceeds [`MAX_REGION_OPS_BYTES`].
    TooLarge,
    /// The stream or an op operand ends early.
    Truncated,
    /// Bytes follow the first `END` of the region.
    TrailingBytes,
    /// A `ROW` op is missing, out of range, or not strictly increasing; also a
    /// writing op before the first `ROW`.
    BadRow,
    /// A `SPAN`/`FILL` run is empty, out of the region, overlapping, or out of
    /// ascending `x` order.
    BadSpan,
    /// An unknown op byte, or a caller output buffer of the wrong length.
    BadValue,
    /// The full-coverage rule of a reset frame is violated.
    Coverage,
}

impl DecodeError {
    /// The receiver's resync reason this failure maps to.
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Coverage => "COVERAGE_INVALID",
            Self::Deflate
            | Self::TooLarge
            | Self::Truncated
            | Self::TrailingBytes
            | Self::BadRow
            | Self::BadSpan
            | Self::BadValue => "DECODE_ERROR",
        }
    }
}

/// Inflate one region payload into `scratch`, bounded by
/// [`MAX_REGION_OPS_BYTES`].
pub fn inflate_region(payload: &[u8], scratch: &mut Vec<u8>) -> Result<(), DecodeError> {
    if payload.is_empty() {
        return Err(DecodeError::Deflate);
    }
    scratch.clear();
    let mut decompress = Decompress::new(false);
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let consumed = decompress.total_in() as usize;
        let produced_before = decompress.total_out();
        let status = decompress
            .decompress(&payload[consumed..], &mut buffer, FlushDecompress::None)
            .map_err(|_| DecodeError::Deflate)?;
        let produced = (decompress.total_out() - produced_before) as usize;
        if produced > 0 {
            if scratch.len() + produced > MAX_REGION_OPS_BYTES {
                return Err(DecodeError::TooLarge);
            }
            scratch.extend_from_slice(&buffer[..produced]);
        }
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {
                let progressed = produced > 0 || decompress.total_in() as usize > consumed;
                if !progressed {
                    return Err(if (decompress.total_in() as usize) < payload.len() {
                        DecodeError::Deflate
                    } else {
                        DecodeError::Truncated
                    });
                }
            }
        }
    }
    if decompress.total_in() as usize != payload.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(())
}

/// Validate an op stream without painting it (the encoder's self-check and the
/// corpus error cases).
pub fn validate_region_ops(
    ops: &[u8],
    region_width: u32,
    region_height: u32,
    coverage: Coverage,
) -> Result<(), DecodeError> {
    if ops.len() > MAX_REGION_OPS_BYTES {
        return Err(DecodeError::TooLarge);
    }
    walk_ops(ops, region_width, region_height, coverage, None)
}

/// Decode one region's op stream into `out` (row-major the 16-bit cell format of the region's
/// own box).
///
/// `out` holds whatever the caller pre-filled it with (canvas content for
/// sparse coverage); written cells overwrite it. `scratch` is the caller-owned
/// reusable decompression buffer.
pub fn decode_region_ops(
    payload: &[u8],
    region_width: u32,
    region_height: u32,
    coverage: Coverage,
    out: &mut [u16],
    scratch: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    if region_width == 0 || region_height == 0 {
        return Err(DecodeError::BadValue);
    }
    if out.len() != (region_width as usize) * (region_height as usize) {
        return Err(DecodeError::BadValue);
    }
    inflate_region(payload, scratch)?;
    walk_ops(scratch, region_width, region_height, coverage, Some(out))
}

/// A receiver-side frame canvas: the Rust mirror of the receiver's apply path.
///
/// This is the conformance and model-test oracle, not a product path: it
/// implements §4.5's apply rules (`R0` stale discard, `R1` geometry, `R2` base
/// match, `R3` validate-then-apply, `R5` applied id) on top of
/// [`decode_region_ops`], so a Rust test can assert what the browser must paint
/// without re-implementing the rules.
#[derive(Debug, Default)]
pub struct SurfaceCanvas {
    buffer: Option<Surface>,
    stage: Option<Surface>,
    scratch: Vec<u8>,
    applied_update_id: u64,
}

impl SurfaceCanvas {
    /// An empty canvas that holds no frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// The frame id the canvas holds (`0` = nothing applied yet).
    pub fn applied_update_id(&self) -> u64 {
        self.applied_update_id
    }

    /// The canvas cells, or an empty slice before the first frame.
    pub fn cells(&self) -> &[u16] {
        match self.buffer.as_ref() {
            Some(buffer) => buffer.as_u16(),
            None => &[],
        }
    }

    /// Canvas geometry.
    pub fn geometry(&self) -> Option<(u32, u32)> {
        self.buffer
            .as_ref()
            .map(|buffer| (buffer.width(), buffer.height()))
    }

    /// Resize the canvas, clearing it to opaque black (the 16-bit cell format zero).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.buffer = Some(match Surface::new(width, height) {
            Ok(buffer) => buffer,
            Err(_) => return,
        });
        self.stage = None;
    }

    /// Apply one encoded frame, or update the applied frame id for a stale or
    /// empty frame.
    ///
    /// Returns `Ok(())` when the frame was applied (or legitimately discarded as
    /// stale); a validation failure returns the matching [`DecodeError`] with the
    /// canvas untouched.
    pub fn apply(&mut self, frame: &EncodedUpdate) -> Result<(), DecodeError> {
        // R0: a delta for a frame the canvas already holds or passes is stale.
        if frame.base_update_id != 0 && frame.update_id <= self.applied_update_id {
            return Ok(());
        }
        let geometry = (frame.width, frame.height);
        if frame.base_update_id == 0 {
            // R1: a reset frame is geometry-authoritative. An unchanged geometry
            // keeps the existing content; a changed one clears to black.
            if self.geometry() != Some(geometry) {
                self.resize(frame.width, frame.height);
            }
        } else if self.geometry() != Some(geometry) {
            return Err(DecodeError::BadValue);
        }
        // R2: a delta must apply onto exactly the frame the canvas holds.
        if frame.base_update_id != 0 && frame.base_update_id != self.applied_update_id {
            return Err(DecodeError::BadValue);
        }
        if frame.regions.is_empty() {
            self.applied_update_id = frame.update_id;
            return Ok(());
        }
        // Stage the frame: copy the canvas, decode every region into the copy,
        // then swap. A failure anywhere leaves the canvas untouched (R3/R4).
        let needs_stage = !self
            .stage
            .as_ref()
            .is_some_and(|stage| stage.matches(frame.width, frame.height));
        if needs_stage {
            let mut stage =
                Surface::new(frame.width, frame.height).map_err(|_| DecodeError::BadValue)?;
            if let Some(buffer) = self.buffer.as_ref()
                && buffer.matches(frame.width, frame.height)
            {
                stage.copy_from(buffer).map_err(|_| DecodeError::BadValue)?;
            }
            self.stage = Some(stage);
        }
        let Some(stage) = self.stage.as_mut() else {
            return Err(DecodeError::BadValue);
        };
        let coverage = if frame.base_update_id == 0 {
            Coverage::Full
        } else {
            Coverage::Sparse
        };
        for region in &frame.regions {
            if region.width == 0 || region.height == 0 {
                return Err(DecodeError::BadValue);
            }
            let width = region.width as usize;
            let height = region.height as usize;
            let mut local = vec![0_u16; width * height];
            if coverage == Coverage::Sparse {
                for (row, target) in local.chunks_exact_mut(width).enumerate() {
                    let source_y = region.y as usize + row;
                    if source_y >= stage.height() as usize {
                        return Err(DecodeError::BadValue);
                    }
                    let source = stage.row(source_y as u32);
                    let start = region.x as usize;
                    if start + width > source.len() {
                        return Err(DecodeError::BadValue);
                    }
                    target.copy_from_slice(&source[start..start + width]);
                }
            }
            decode_region_ops(
                &region.payload,
                region.width,
                region.height,
                coverage,
                &mut local,
                &mut self.scratch,
            )?;
            for (row, source) in local.chunks_exact(width).enumerate() {
                let target_y = region.y as usize + row;
                if target_y >= stage.height() as usize {
                    return Err(DecodeError::BadValue);
                }
                let target = stage.row_mut(target_y as u32);
                let start = region.x as usize;
                if start + width > target.len() {
                    return Err(DecodeError::BadValue);
                }
                target[start..start + width].copy_from_slice(source);
            }
        }
        // R5: commit the staged frame and advance the applied id.
        if let Some(stage) = self.stage.take() {
            self.buffer = Some(stage);
        }
        self.applied_update_id = frame.update_id;
        Ok(())
    }
}

/// The op walk, shared by validation (no `out`) and application.
fn walk_ops(
    ops: &[u8],
    width: u32,
    height: u32,
    coverage: Coverage,
    mut out: Option<&mut [u16]>,
) -> Result<(), DecodeError> {
    let full = coverage == Coverage::Full;
    let mut pointer = 0_usize;
    let mut current_row: Option<u32> = None;
    let mut last_end_x = 0_u32;
    let mut rows_complete = 0_u32;
    let mut row_closed = false;
    loop {
        let op = *ops.get(pointer).ok_or(DecodeError::Truncated)?;
        pointer += 1;
        match op {
            OP_END => {
                if pointer != ops.len() {
                    return Err(DecodeError::TrailingBytes);
                }
                if full && rows_complete != height {
                    return Err(DecodeError::Coverage);
                }
                return Ok(());
            }
            OP_ROW => {
                let row = u32::from(read_u16(ops, &mut pointer)?);
                if row >= height {
                    return Err(DecodeError::BadRow);
                }
                if let Some(previous) = current_row {
                    if row <= previous {
                        return Err(DecodeError::BadRow);
                    }
                    if full && !row_closed {
                        // The previous row was left incomplete; coverage is
                        // contiguous, so the walk cannot come back to it.
                        return Err(DecodeError::Coverage);
                    }
                }
                if full && row != rows_complete {
                    return Err(DecodeError::Coverage);
                }
                current_row = Some(row);
                last_end_x = 0;
                row_closed = false;
            }
            OP_SPAN | OP_FILL => {
                let row = match current_row {
                    Some(row) => row,
                    None => return Err(DecodeError::BadRow),
                };
                let x = u32::from(read_u16(ops, &mut pointer)?);
                let len = u32::from(read_u16(ops, &mut pointer)?);
                if len == 0 || x.checked_add(len).is_none_or(|end| end > width) {
                    return Err(DecodeError::BadSpan);
                }
                if full {
                    if row_closed || x != last_end_x {
                        return Err(DecodeError::Coverage);
                    }
                    last_end_x = x + len;
                    if last_end_x == width {
                        row_closed = true;
                        rows_complete += 1;
                    }
                } else {
                    if x < last_end_x {
                        return Err(DecodeError::BadSpan);
                    }
                    last_end_x = x + len;
                }
                let value = if op == OP_SPAN {
                    None
                } else {
                    Some(read_u16(ops, &mut pointer)?)
                };
                if let Some(target) = out.as_mut() {
                    let base = (row as usize) * (width as usize) + (x as usize);
                    let end = base + len as usize;
                    match value {
                        Some(value) => target[base..end].fill(value),
                        None => {
                            for (index, slot) in target[base..end].iter_mut().enumerate() {
                                *slot = u16::from_le_bytes([
                                    *ops.get(pointer + index * 2).ok_or(DecodeError::Truncated)?,
                                    *ops.get(pointer + index * 2 + 1)
                                        .ok_or(DecodeError::Truncated)?,
                                ]);
                            }
                        }
                    }
                }
                if value.is_none() {
                    let bytes = (len as usize) * 2;
                    if pointer + bytes > ops.len() {
                        return Err(DecodeError::Truncated);
                    }
                    pointer += bytes;
                }
            }
            _ => return Err(DecodeError::BadValue),
        }
    }
}

/// Read one little-endian `u16` operand.
fn read_u16(ops: &[u8], pointer: &mut usize) -> Result<u16, DecodeError> {
    let low = *ops.get(*pointer).ok_or(DecodeError::Truncated)?;
    let high = *ops.get(*pointer + 1).ok_or(DecodeError::Truncated)?;
    *pointer += 2;
    Ok(u16::from_le_bytes([low, high]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{emit_end, emit_fill, emit_full_row, emit_row, emit_span, emit_values};
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    /// Raw-DEFLATE a hand-built op stream at the given level.
    fn deflate(ops: &[u8], level: u32) -> Vec<u8> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
        encoder.write_all(ops).unwrap();
        encoder.finish().unwrap()
    }

    fn decode(
        payload: &[u8],
        width: u32,
        height: u32,
        coverage: Coverage,
    ) -> Result<Vec<u16>, DecodeError> {
        let mut out = vec![0xeeee_u16; (width * height) as usize];
        let mut scratch = Vec::new();
        decode_region_ops(payload, width, height, coverage, &mut out, &mut scratch)?;
        Ok(out)
    }

    #[test]
    fn v2_literal_run_decodes_to_the_documented_cells() {
        // §9.1 V2 op stream: ROW 0; SPAN x=0 len=4; 0xF800 0x07E0 0x001F 0xFFFF; END.
        let ops = vec![
            0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x04, 0x00, 0x00, 0xf8, 0xe0, 0x07, 0x1f, 0x00,
            0xff, 0xff, 0x00,
        ];
        assert_eq!(ops.len(), 17);
        let payload = deflate(&ops, 0);
        assert_eq!(
            payload,
            vec![
                0x01, 0x11, 0x00, 0xee, 0xff, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x04, 0x00, 0x00,
                0xf8, 0xe0, 0x07, 0x1f, 0x00, 0xff, 0xff, 0x00,
            ],
            "stored-block envelope of V2"
        );
        let cells = decode(&payload, 4, 2, Coverage::Sparse).unwrap();
        assert_eq!(cells[0..4], [0xf800, 0x07e0, 0x001f, 0xffff]);
        assert_eq!(cells[4..8], [0xeeee; 4], "row 1 is untouched");
    }

    #[test]
    fn v3_full_coverage_fills_decode_to_the_documented_cells() {
        // §9.1 V3 op stream: three rows, each ROW + FILL of the full width.
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_fill(&mut ops, 0, 6, 0x0000);
        emit_row(&mut ops, 1);
        emit_fill(&mut ops, 0, 6, 0xffff);
        emit_row(&mut ops, 2);
        emit_fill(&mut ops, 0, 6, 0x8410);
        emit_end(&mut ops);
        assert_eq!(ops.len(), 31);
        let payload = deflate(&ops, 0);
        assert_eq!(payload.len(), 36);
        assert_eq!(&payload[..5], &[0x01, 0x1f, 0x00, 0xe0, 0xff]);
        let cells = decode(&payload, 6, 3, Coverage::Full).unwrap();
        assert_eq!(cells[0..6], [0x0000; 6]);
        assert_eq!(cells[6..12], [0xffff; 6]);
        assert_eq!(cells[12..18], [0x8410; 6]);
    }

    #[test]
    fn v3_coverage_mutants_are_rejected() {
        // A missing row: rows 0 and 2 only.
        let mut ops = Vec::new();
        emit_full_row(&mut ops, 0, &[0_u16; 6]);
        emit_full_row(&mut ops, 2, &[0x8410_u16; 6]);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 6, 3, Coverage::Full),
            Err(DecodeError::Coverage)
        );
        // A gap inside a row.
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_fill(&mut ops, 0, 2, 0);
        emit_fill(&mut ops, 3, 3, 0);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 6, 1, Coverage::Full),
            Err(DecodeError::Coverage)
        );
        // A row that is written twice is not reachable: the second ROW is not
        // strictly increasing.
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_fill(&mut ops, 0, 6, 0);
        emit_row(&mut ops, 0);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 6, 1, Coverage::Full),
            Err(DecodeError::BadRow)
        );
        // Sparse mode accepts the same gap and the skipped row.
        let mut ops = Vec::new();
        emit_full_row(&mut ops, 0, &[1_u16; 6]);
        emit_full_row(&mut ops, 2, &[2_u16; 6]);
        emit_end(&mut ops);
        assert_eq!(validate_region_ops(&ops, 6, 3, Coverage::Sparse), Ok(()));
    }

    #[test]
    fn restarting_a_closed_row_is_a_coverage_error() {
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_fill(&mut ops, 0, 6, 0);
        emit_fill(&mut ops, 0, 1, 0);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 6, 1, Coverage::Full),
            Err(DecodeError::Coverage)
        );
        // In sparse mode the same stream is an overlapping pair of runs.
        assert_eq!(
            validate_region_ops(&ops, 6, 1, Coverage::Sparse),
            Err(DecodeError::BadSpan)
        );
    }

    #[test]
    fn writing_before_a_row_is_rejected() {
        let mut ops = Vec::new();
        emit_fill(&mut ops, 0, 4, 0);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::BadRow)
        );
    }

    #[test]
    fn malformed_op_streams_are_classified() {
        // Trailing bytes after END.
        let mut ops = Vec::new();
        emit_full_row(&mut ops, 0, &[0_u16; 4]);
        emit_end(&mut ops);
        ops.push(0x02);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::TrailingBytes)
        );
        // Truncated operand.
        assert_eq!(
            validate_region_ops(&[OP_ROW, 0x00], 4, 1, Coverage::Sparse),
            Err(DecodeError::Truncated)
        );
        // Missing END.
        let mut ops = Vec::new();
        emit_full_row(&mut ops, 0, &[0_u16; 4]);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::Truncated)
        );
        // Unknown op byte.
        assert_eq!(
            validate_region_ops(&[0x7f], 4, 1, Coverage::Sparse),
            Err(DecodeError::BadValue)
        );
        // Zero-length run.
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        ops.push(OP_FILL);
        ops.extend_from_slice(&0_u16.to_le_bytes());
        ops.extend_from_slice(&0_u16.to_le_bytes());
        ops.extend_from_slice(&0_u16.to_le_bytes());
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::BadSpan)
        );
        // Run past the region edge.
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_fill(&mut ops, 3, 2, 0);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::BadSpan)
        );
        // Overlapping spans: a writing op before any ROW is a row error.
        let mut ops = Vec::new();
        emit_span(&mut ops, 0, &[1, 2]);
        emit_row(&mut ops, 0);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::BadRow)
        );
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_span(&mut ops, 0, &[1, 2]);
        emit_span(&mut ops, 1, &[3]);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 4, 1, Coverage::Sparse),
            Err(DecodeError::BadSpan)
        );
        // Row out of range.
        let mut ops = Vec::new();
        emit_row(&mut ops, 4);
        emit_end(&mut ops);
        assert_eq!(
            validate_region_ops(&ops, 4, 4, Coverage::Sparse),
            Err(DecodeError::BadRow)
        );
    }

    #[test]
    fn deflate_envelopes_are_classified() {
        // Garbage is not deflate.
        let mut out = vec![0_u16; 4];
        let mut scratch = Vec::new();
        assert_eq!(
            decode_region_ops(
                &[0xff, 0xff, 0xff, 0xff],
                4,
                1,
                Coverage::Sparse,
                &mut out,
                &mut scratch
            ),
            Err(DecodeError::Deflate)
        );
        // An empty payload never decodes.
        assert_eq!(
            decode_region_ops(&[], 4, 1, Coverage::Sparse, &mut out, &mut scratch),
            Err(DecodeError::Deflate)
        );
        // A valid stream followed by trailing bytes.
        let mut ops = Vec::new();
        emit_full_row(&mut ops, 0, &[0_u16; 4]);
        emit_end(&mut ops);
        let mut payload = deflate(&ops, 6);
        payload.push(0x00);
        assert_eq!(
            decode_region_ops(&payload, 4, 1, Coverage::Sparse, &mut out, &mut scratch),
            Err(DecodeError::TrailingBytes)
        );
        // A truncated stream.
        let payload = deflate(&ops, 6);
        let truncated = &payload[..payload.len() / 2];
        assert!(
            decode_region_ops(truncated, 4, 1, Coverage::Sparse, &mut out, &mut scratch).is_err()
        );
    }

    #[test]
    fn bounded_decompression_rejects_oversized_op_streams() {
        // A deflate stream of MAX_REGION_OPS_BYTES + 1 zero bytes expands past
        // the cap without allocating more than the final part.
        let payload = deflate(&vec![0_u8; MAX_REGION_OPS_BYTES + 1], 0);
        let mut scratch = Vec::new();
        assert_eq!(
            inflate_region(&payload, &mut scratch),
            Err(DecodeError::TooLarge)
        );
        let mut out = vec![0_u16; 4];
        assert_eq!(
            decode_region_ops(&payload, 4, 1, Coverage::Sparse, &mut out, &mut scratch),
            Err(DecodeError::TooLarge)
        );
    }

    #[test]
    fn deflate_block_variants_all_decode_identically() {
        let mut ops = Vec::new();
        emit_row(&mut ops, 0);
        emit_values(
            &mut ops,
            0,
            &[0x1234_u16, 0x1234, 0xabcd, 0x1234, 0x1234, 0x1234],
        );
        emit_end(&mut ops);
        let expected = decode(&deflate(&ops, 0), 6, 1, Coverage::Sparse).unwrap();
        for level in [0, 1, 3, 6, 9] {
            let payload = deflate(&ops, level);
            assert_eq!(
                decode(&payload, 6, 1, Coverage::Sparse).unwrap(),
                expected,
                "level {level}"
            );
        }
    }

    #[test]
    fn an_empty_region_paints_nothing() {
        let payload = deflate(&[OP_END], 0);
        let cells = decode(&payload, 3, 2, Coverage::Sparse).unwrap();
        assert_eq!(cells, vec![0xeeee_u16; 6]);
        assert_eq!(
            validate_region_ops(&[OP_END], 3, 2, Coverage::Sparse),
            Ok(())
        );
    }

    #[test]
    fn a_wrong_output_length_is_rejected() {
        let payload = deflate(&[OP_END], 0);
        let mut out = vec![0_u16; 3];
        let mut scratch = Vec::new();
        assert_eq!(
            decode_region_ops(&payload, 3, 2, Coverage::Sparse, &mut out, &mut scratch),
            Err(DecodeError::BadValue)
        );
    }

    #[test]
    fn reason_codes_match_the_resync_enum() {
        assert_eq!(DecodeError::Deflate.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::TooLarge.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::Truncated.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::TrailingBytes.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::BadRow.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::BadSpan.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::BadValue.reason_code(), "DECODE_ERROR");
        assert_eq!(DecodeError::Coverage.reason_code(), "COVERAGE_INVALID");
    }
}
