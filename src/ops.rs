//! The op stream: `ROW`/`SPAN`/`FILL`/`END`.
//!
//! A region's ops reference the region's own coordinate space, `(0,0)` at its
//! top-left. Rows are strictly increasing and a row's runs are strictly
//! ascending and non-overlapping, which is what lets the decoder walk once
//! without a cell map. Constant runs of at least [`FILL_MIN_RUN`] cells become
//! `FILL` ops; everything else is a literal `SPAN` (with long constant sub-runs
//! split out as their own `FILL`).

use flate2::{Compress, Compression, FlushCompress, Status};

use crate::detect::Span;
use crate::error::CodecError;
use crate::format::FILL_MIN_RUN;

/// End of the op stream.
pub const OP_END: u8 = 0x00;
/// Set the current row: `row: u16`.
pub const OP_ROW: u8 = 0x01;
/// Write literal cells: `x: u16`, `len: u16`, `len × u16le`.
pub const OP_SPAN: u8 = 0x02;
/// Write one value `len` times: `x: u16`, `len: u16`, `value: u16le`.
pub const OP_FILL: u8 = 0x03;

/// Append a `ROW` op.
pub fn emit_row(out: &mut Vec<u8>, row: u32) {
    out.push(OP_ROW);
    out.extend_from_slice(&(row as u16).to_le_bytes());
}

/// Compress an op stream into the region's raw-DEFLATE envelope.
///
/// The level is an encoder-side cost/ratio decision, not a format variant: both
/// decoders accept any valid stream, and level `0` produces stored blocks (the
/// form the hand-derived conformance vectors are written in).
pub fn deflate_ops(ops: &[u8], level: u8) -> Result<Vec<u8>, CodecError> {
    let mut deflater = Deflater::new(level);
    let mut output = Vec::with_capacity(ops.len() / 2 + 64);
    deflater.deflate_into(ops, level, &mut output)?;
    Ok(output)
}

/// A reusable raw-DEFLATE compressor for op streams.
///
/// One delta frame carries one to a few regions and the compressor state is a
/// few hundred kilobytes at the mid levels, so constructing a fresh compressor
/// per region is that much allocator traffic per frame. Resetting a retained
/// instance per region emits exactly the bytes a fresh instance of the same
/// level emits (the reset restores the whole stream state), which keeps the
/// wire output identical while the steady-state path allocates nothing for the
/// compressor itself.
#[derive(Debug)]
pub struct Deflater {
    compressor: Compress,
    level: u8,
    buffer: Vec<u8>,
}

impl Deflater {
    /// Build a deflater for `level` (`0..=9`; higher values clamp to 9).
    pub fn new(level: u8) -> Self {
        let level = level.min(9);
        Self {
            compressor: Compress::new(Compression::new(u32::from(level)), false),
            level,
            buffer: vec![0; DEFLATE_SCRATCH_BYTES],
        }
    }

    /// Compress `ops` at `level`, replacing the contents of `out`.
    ///
    /// The stream is finished and flushed in one `FlushCompress::Finish` loop,
    /// exactly like [`deflate_ops`]; only the compressor object is reused.
    pub fn deflate_into(
        &mut self,
        ops: &[u8],
        level: u8,
        out: &mut Vec<u8>,
    ) -> Result<(), CodecError> {
        let level = level.min(9);
        if level != self.level {
            // `reset` keeps the level, so a level change needs a new compressor.
            self.compressor = Compress::new(Compression::new(u32::from(level)), false);
            self.level = level;
        } else {
            self.compressor.reset();
        }
        out.clear();
        let Self {
            compressor, buffer, ..
        } = self;
        let mut stalled = 0_u8;
        loop {
            let consumed = compressor.total_in() as usize;
            let produced_before = compressor.total_out();
            let status = compressor
                .compress(&ops[consumed..], buffer, FlushCompress::Finish)
                .map_err(|error| CodecError::Encode(format!("deflate: {error}")))?;
            let produced = (compressor.total_out() - produced_before) as usize;
            if produced > 0 {
                out.extend_from_slice(&buffer[..produced]);
            }
            if status == Status::StreamEnd {
                break;
            }
            if produced == 0 && compressor.total_in() as usize == consumed {
                stalled += 1;
                if stalled > 2 {
                    return Err(CodecError::Encode("deflate made no progress".to_string()));
                }
            } else {
                stalled = 0;
            }
        }
        Ok(())
    }
}

/// Output staging buffer of one deflate pass (see [`Deflater::deflate_into`]).
const DEFLATE_SCRATCH_BYTES: usize = 16 * 1024;

/// Append an `END` op.
pub fn emit_end(out: &mut Vec<u8>) {
    out.push(OP_END);
}

/// Append a `FILL` op.
pub fn emit_fill(out: &mut Vec<u8>, x: u32, len: u32, value: u16) {
    debug_assert!(len >= 1, "FILL length must be at least one cell");
    out.push(OP_FILL);
    out.extend_from_slice(&(x as u16).to_le_bytes());
    out.extend_from_slice(&(len as u16).to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
}

/// Append a `SPAN` op writing `values` at column `x`.
pub fn emit_span(out: &mut Vec<u8>, x: u32, values: &[u16]) {
    debug_assert!(!values.is_empty(), "SPAN must carry at least one cell");
    out.push(OP_SPAN);
    out.extend_from_slice(&(x as u16).to_le_bytes());
    out.extend_from_slice(&(values.len() as u16).to_le_bytes());
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

/// Append ops covering `values` at columns `x..x + values.len()`.
///
/// Maximal constant runs of at least [`FILL_MIN_RUN`] cells become `FILL` ops;
/// the remaining literal stretches become `SPAN` ops, so a literal span never
/// carries a long flat tail.
pub fn emit_values(out: &mut Vec<u8>, x: u32, values: &[u16]) {
    let mut index = 0_usize;
    while index < values.len() {
        let value = values[index];
        let mut run = 1_usize;
        while index + run < values.len() && values[index + run] == value {
            run += 1;
        }
        if run >= FILL_MIN_RUN as usize {
            emit_fill(out, x + index as u32, run as u32, value);
            index += run;
            continue;
        }
        // Literal: extend until a long constant run starts, including the
        // short runs already measured.
        let start = index;
        index += run;
        while index < values.len() {
            let candidate = values[index];
            let mut candidate_run = 1_usize;
            while index + candidate_run < values.len() && values[index + candidate_run] == candidate
            {
                candidate_run += 1;
            }
            if candidate_run >= FILL_MIN_RUN as usize {
                break;
            }
            index += candidate_run;
        }
        emit_span(out, x + start as u32, &values[start..index]);
    }
}

/// Append a full-coverage row (`ROW` + the whole row's ops).
pub fn emit_full_row(out: &mut Vec<u8>, row: u32, values: &[u16]) {
    emit_row(out, row);
    emit_values(out, 0, values);
}

/// Append ops for one row's changed spans, in ascending span order.
///
/// `values` is the region's row; span coordinates are region-local. Returns the
/// number of cells written. When `max_cells` is set, the output stops at that
/// many cells (the delta budget's progress guarantee) and no partial op is
/// left behind.
pub fn emit_spans(
    out: &mut Vec<u8>,
    row: u32,
    values: &[u16],
    spans: &[Span],
    max_cells: Option<u64>,
) -> u64 {
    let mut emitted = 0_u64;
    let mut wrote_row = false;
    for span in spans {
        if let Some(limit) = max_cells
            && emitted >= limit
        {
            break;
        }
        let mut len = span.len;
        if let Some(limit) = max_cells {
            let remaining = limit - emitted;
            if u64::from(len) > remaining {
                len = remaining as u32;
            }
        }
        if len == 0 {
            break;
        }
        let start = span.x as usize;
        let end = start + len as usize;
        debug_assert!(end <= values.len(), "span exceeds the row");
        if !wrote_row {
            emit_row(out, row);
            wrote_row = true;
        }
        emit_values(out, span.x, &values[start..end]);
        emitted += u64::from(len);
    }
    emitted
}

/// Truncate a span list at `max_cells`, writing the result into `out`.
///
/// Used by the delta budget's progress guarantee: a frame whose budget is
/// exhausted before its first row still carries the first
/// [`DELTA_REFRESH_MIN_CELLS`](crate::format::DELTA_REFRESH_MIN_CELLS)
/// cells of that row.
pub fn truncate_spans(spans: &[Span], max_cells: u64, out: &mut Vec<Span>) -> u64 {
    out.clear();
    let mut emitted = 0_u64;
    for span in spans {
        if emitted >= max_cells {
            break;
        }
        let mut len = u64::from(span.len);
        if emitted + len > max_cells {
            len = max_cells - emitted;
        }
        out.push(Span {
            x: span.x,
            len: len as u32,
        });
        emitted += len;
    }
    emitted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_codes_and_operands_are_little_endian() {
        let mut out = Vec::new();
        emit_row(&mut out, 0x1234);
        assert_eq!(out, vec![OP_ROW, 0x34, 0x12]);
        out.clear();
        emit_fill(&mut out, 2, 300, 0xf800);
        assert_eq!(out, vec![OP_FILL, 0x02, 0x00, 0x2c, 0x01, 0x00, 0xf8]);
        out.clear();
        emit_span(&mut out, 1, &[0x07e0, 0x001f]);
        assert_eq!(
            out,
            vec![OP_SPAN, 0x01, 0x00, 0x02, 0x00, 0xe0, 0x07, 0x1f, 0x00]
        );
        out.clear();
        emit_end(&mut out);
        assert_eq!(out, vec![OP_END]);
    }

    #[test]
    fn long_constant_runs_become_fills() {
        let values = vec![7_u16; 10];
        let mut out = Vec::new();
        emit_values(&mut out, 0, &values);
        assert_eq!(out.len(), 7);
        assert_eq!(out[0], OP_FILL);
        assert_eq!(&out[3..5], &10_u16.to_le_bytes());
    }

    #[test]
    fn short_runs_stay_literals_and_split_around_long_runs() {
        let mut values = vec![1_u16, 2, 3];
        values.extend_from_slice(&[9_u16; 8]);
        values.extend_from_slice(&[4, 5]);
        let mut out = Vec::new();
        emit_values(&mut out, 4, &values);
        // SPAN [1,2,3] at x=4, FILL 9 ×8 at x=7, SPAN [4,5] at x=15.
        assert_eq!(out[0], OP_SPAN);
        assert_eq!(&out[1..3], &4_u16.to_le_bytes());
        assert_eq!(&out[3..5], &3_u16.to_le_bytes());
        assert_eq!(&out[5..11], &[1, 0, 2, 0, 3, 0]);
        let fill_at = 11;
        assert_eq!(out[fill_at], OP_FILL);
        assert_eq!(&out[fill_at + 1..fill_at + 3], &7_u16.to_le_bytes());
        assert_eq!(&out[fill_at + 3..fill_at + 5], &8_u16.to_le_bytes());
        assert_eq!(&out[fill_at + 5..fill_at + 7], &9_u16.to_le_bytes());
        let span_at = fill_at + 7;
        assert_eq!(out[span_at], OP_SPAN);
        assert_eq!(&out[span_at + 1..span_at + 3], &15_u16.to_le_bytes());
        assert_eq!(&out[span_at + 3..span_at + 5], &2_u16.to_le_bytes());
        assert_eq!(out.len(), span_at + 9);
    }

    #[test]
    fn exactly_fill_min_run_cells_become_a_fill() {
        let values = vec![5_u16; FILL_MIN_RUN as usize];
        let mut out = Vec::new();
        emit_values(&mut out, 0, &values);
        assert_eq!(out[0], OP_FILL);
        assert_eq!(out.len(), 7);
    }

    #[test]
    fn spans_are_emitted_in_order_with_a_single_row_op() {
        let values: Vec<u16> = (0..16).collect();
        let spans = vec![
            Span { x: 2, len: 2 },
            Span { x: 8, len: 1 },
            Span { x: 12, len: 3 },
        ];
        let mut out = Vec::new();
        let emitted = emit_spans(&mut out, 3, &values, &spans, None);
        assert_eq!(emitted, 6);
        assert_eq!(out[0], OP_ROW);
        assert_eq!(&out[1..3], &3_u16.to_le_bytes());
        assert_eq!(out[3], OP_SPAN);
        assert_eq!(&out[4..6], &2_u16.to_le_bytes());
    }

    #[test]
    fn truncation_stops_at_the_cell_limit() {
        let values: Vec<u16> = (0..16).collect();
        let spans = vec![Span { x: 0, len: 10 }, Span { x: 12, len: 4 }];
        let mut out = Vec::new();
        let emitted = emit_spans(&mut out, 0, &values, &spans, Some(6));
        assert_eq!(emitted, 6);
        assert_eq!(out[0], OP_ROW);
        assert_eq!(out[3], OP_SPAN);
        assert_eq!(&out[6..8], &6_u16.to_le_bytes());
        // A zero limit writes nothing at all.
        let mut empty = Vec::new();
        assert_eq!(emit_spans(&mut empty, 0, &values, &spans, Some(0)), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn full_rows_cover_every_cell() {
        let values = vec![42_u16; 6];
        let mut out = Vec::new();
        emit_full_row(&mut out, 0, &values);
        assert_eq!(out[0], OP_ROW);
        // Six cells is below the fill threshold, so the row is one literal.
        assert_eq!(out[3], OP_SPAN);
        assert_eq!(&out[4..6], &0_u16.to_le_bytes());
        assert_eq!(&out[6..8], &6_u16.to_le_bytes());
    }

    #[test]
    fn a_reused_deflater_emits_the_same_bytes_as_a_fresh_one() {
        // Content with flat runs and literals, compressed at three levels.
        let mut ops = Vec::new();
        for row in 0..64_u32 {
            let values: Vec<u16> = (0..1296_u16).map(|x| (x ^ (row as u16)) >> 1).collect();
            emit_full_row(&mut ops, row, &values);
        }
        emit_end(&mut ops);
        let mut deflater = Deflater::new(6);
        let mut out = Vec::new();
        for level in [6_u8, 6, 0, 6, 3, 6] {
            deflater.deflate_into(&ops, level, &mut out).unwrap();
            assert_eq!(
                out,
                deflate_ops(&ops, level).unwrap(),
                "reused deflater at level {level}"
            );
        }
        // A second, different stream on the same deflater (a delta region after
        // a reset band, in the encoder's real usage).
        let mut small = Vec::new();
        emit_row(&mut small, 3);
        emit_fill(&mut small, 7, 40, 0x1234);
        emit_end(&mut small);
        deflater.deflate_into(&small, 6, &mut out).unwrap();
        assert_eq!(out, deflate_ops(&small, 6).unwrap());
    }
}
