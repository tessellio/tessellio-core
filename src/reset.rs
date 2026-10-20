//! Reset-frame encoder: full-coverage bands, encoded band-parallel.
//!
//! A reset frame is self-contained: bands of [`RESET_BAND_ROWS`] rows, each a
//! full-coverage region (`ROW` per row, `FILL` for constant runs), deflated at
//! the reset level. Bands that would exceed the per-region caps are split in
//! half and re-encoded, and the frame carries only the prefix that fits the
//! reset payload budget; the remainder is the next frame's work.

use std::thread;

use crate::buffer::Surface;
use crate::decode::{Coverage, validate_region_ops};
use crate::error::CodecError;
use crate::format::{MAX_REGION_OPS_BYTES, MAX_REGION_WIRE_BYTES, RESET_BAND_ROWS};
use crate::ops::{Deflater, emit_end, emit_full_row};

/// One encoded band region (always full width, `height` rows at `y`).
#[derive(Debug)]
pub struct BandRegion {
    /// First row of the band.
    pub y: u32,
    /// Band height in rows.
    pub height: u32,
    /// Raw-DEFLATE op-stream payload.
    pub payload: Vec<u8>,
    /// Uncompressed op-stream size (the size the split policy reasons about).
    pub op_bytes: usize,
    /// Op-build time of this band (aggregate across the parallel workers when
    /// the frame was encoded band-parallel).
    pub pack_ns: u64,
    /// DEFLATE time of this band (aggregate across workers likewise).
    pub compress_ns: u64,
}

/// Op-stream size the pre-compress split targets: half the decompression cap, so
/// a deflated band stays inside the wire cap for incompressible content.
const BAND_OPS_SPLIT_BYTES: usize = MAX_REGION_OPS_BYTES / 2;

/// Encode the rows `from_y..height` as reset bands.
///
/// `max_bytes` is the payload budget for this frame (the caller passes the reset
/// frame budget); the returned bands cover a prefix of the requested rows, in
/// ascending order. `threads` is the worker ceiling; `0` and `1` mean sequential.
pub fn encode_bands(
    buffer: &Surface,
    from_y: u32,
    level: u8,
    threads: usize,
    max_bytes: usize,
) -> Result<Vec<BandRegion>, CodecError> {
    let height = buffer.height();
    if from_y >= height {
        return Ok(Vec::new());
    }
    // The band grid for this frame's remaining rows.
    let mut bands: Vec<(u32, u32)> = Vec::new();
    let mut y = from_y;
    while y < height {
        let band_height = RESET_BAND_ROWS.min(height - y);
        bands.push((y, band_height));
        y += band_height;
    }

    let workers = worker_count(threads).min(bands.len().max(1));
    let mut regions: Vec<BandRegion> = Vec::new();
    let mut op_bytes = 0_usize;
    let mut index = 0_usize;
    while index < bands.len() && op_bytes <= max_bytes {
        let wave_end = (index + workers).min(bands.len());
        let wave = encode_wave(buffer, &bands[index..wave_end], level, workers)?;
        for band_regions in wave {
            for region in band_regions {
                op_bytes += region.op_bytes;
                regions.push(region);
            }
        }
        index = wave_end;
    }

    // Carry the longest prefix whose payload fits the frame budget.
    let mut total = 0_usize;
    let mut keep = 0_usize;
    for region in &regions {
        if total + region.payload.len() > max_bytes {
            break;
        }
        total += region.payload.len();
        keep += 1;
    }
    regions.truncate(keep);
    Ok(regions)
}

/// Encode one wave of bands, in order, across `workers` threads.
fn encode_wave(
    buffer: &Surface,
    bands: &[(u32, u32)],
    level: u8,
    workers: usize,
) -> Result<Vec<Vec<BandRegion>>, CodecError> {
    if workers <= 1 || bands.len() <= 1 {
        // One deflater for the whole wave: its state is reused across the
        // bands (and across the recursive splits inside one band). The op
        // stream buffer is reused the same way: a band holds up to a
        // megabyte of ops, and a fresh buffer per band is a fresh mapping
        // and a fresh set of page faults per band.
        let mut deflater = Deflater::new(level);
        let mut ops = Vec::new();
        let mut all = Vec::with_capacity(bands.len());
        for (y, height) in bands {
            let mut regions = Vec::new();
            encode_band(
                buffer,
                *y,
                *height,
                level,
                &mut deflater,
                &mut ops,
                &mut regions,
            )?;
            all.push(regions);
        }
        return Ok(all);
    }
    let part = bands.len().div_ceil(workers).max(1);
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for group in bands.chunks(part) {
            handles.push(scope.spawn(move || {
                // One deflater and one op stream buffer per worker, reused
                // across that worker's bands.
                let mut deflater = Deflater::new(level);
                let mut ops = Vec::new();
                let mut results = Vec::with_capacity(group.len());
                for (y, height) in group {
                    let mut regions = Vec::new();
                    encode_band(
                        buffer,
                        *y,
                        *height,
                        level,
                        &mut deflater,
                        &mut ops,
                        &mut regions,
                    )?;
                    results.push(regions);
                }
                Ok::<_, CodecError>(results)
            }));
        }
        let mut all = Vec::with_capacity(bands.len());
        for handle in handles {
            let part = handle.join().map_err(|_| {
                CodecError::Encode("self-contained band encoder panicked".to_string())
            })??;
            all.extend(part);
        }
        Ok(all)
    })
}

/// Encode one band, splitting recursively until it fits both region caps.
///
/// `ops` is the caller's op stream buffer: it holds the band's ops when the
/// band is deflated, and it is empty again when the band is done.
fn encode_band(
    buffer: &Surface,
    y: u32,
    height: u32,
    level: u8,
    deflater: &mut Deflater,
    ops: &mut Vec<u8>,
    out: &mut Vec<BandRegion>,
) -> Result<(), CodecError> {
    let pack_started = std::time::Instant::now();
    build_ops(buffer, y, height, ops);
    let pack_ns = pack_started.elapsed().as_nanos() as u64;
    if ops.len() > BAND_OPS_SPLIT_BYTES && height > 1 {
        return split_band(buffer, y, height, level, deflater, ops, out);
    }
    let width = buffer.width();
    validate_region_ops(ops, width, height, Coverage::Full).map_err(|error| {
        CodecError::Encode(format!("self-contained band self-check failed: {error:?}"))
    })?;
    let compress_started = std::time::Instant::now();
    let mut payload = Vec::with_capacity(ops.len() / 2 + 64);
    deflater.deflate_into(ops, level, &mut payload)?;
    let compress_ns = compress_started.elapsed().as_nanos() as u64;
    if payload.len() > MAX_REGION_WIRE_BYTES && height > 1 {
        return split_band(buffer, y, height, level, deflater, ops, out);
    }
    if payload.len() > MAX_REGION_WIRE_BYTES {
        return Err(CodecError::Budget {
            kind: crate::format::UpdateKind::Reset,
            cells: u64::from(width) * u64::from(height),
        });
    }
    out.push(BandRegion {
        y,
        height,
        payload,
        op_bytes: ops.len(),
        pack_ns,
        compress_ns,
    });
    Ok(())
}

/// Split one band in half and encode both halves.
fn split_band(
    buffer: &Surface,
    y: u32,
    height: u32,
    level: u8,
    deflater: &mut Deflater,
    ops: &mut Vec<u8>,
    out: &mut Vec<BandRegion>,
) -> Result<(), CodecError> {
    let half = height / 2;
    encode_band(buffer, y, half, level, deflater, ops, out)?;
    encode_band(buffer, y + half, height - half, level, deflater, ops, out)
}

/// Build the full-coverage op stream for `height` rows starting at `y`.
fn build_ops(buffer: &Surface, y: u32, height: u32, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(height as usize * buffer.width() as usize * 2);
    for (local_row, source_row) in (y..y + height).enumerate() {
        emit_full_row(out, local_row as u32, buffer.row(source_row));
    }
    emit_end(out);
}

/// Worker ceiling: the configured value (0 = auto), the machine's parallelism,
/// and the documented maximum of eight.
pub(crate) fn worker_count(configured: usize) -> usize {
    let configured = if configured == 0 {
        thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
    } else {
        configured
    };
    configured.clamp(1, 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{Coverage, decode_region_ops};
    use crate::format::{RESET_BAND_ROWS, RESET_PAYLOAD_BUDGET_BYTES};

    fn gradient(width: u32, height: u32) -> Surface {
        let mut buffer = Surface::new(width, height).unwrap();
        for y in 0..height {
            let row = buffer.row_mut(y);
            for (x, value) in row.iter_mut().enumerate() {
                *value = ((x as u16) << 3) ^ (y as u16);
            }
        }
        buffer
    }

    fn noise(width: u32, height: u32, seed: u32) -> Surface {
        let mut buffer = Surface::new(width, height).unwrap();
        let mut state = seed | 1;
        for value in buffer.as_mut_u16() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *value = (state >> 16) as u16;
        }
        buffer
    }

    #[test]
    fn bands_cover_every_row_exactly_once() {
        let buffer = gradient(320, 300);
        let bands = encode_bands(&buffer, 0, 3, 2, RESET_PAYLOAD_BUDGET_BYTES).unwrap();
        let mut expected_y = 0;
        for band in &bands {
            assert_eq!(band.y, expected_y);
            assert!(band.height as u32 <= RESET_BAND_ROWS);
            expected_y = band.y + band.height;
        }
        assert_eq!(expected_y, 300);
        assert!(!bands.is_empty());
    }

    #[test]
    fn every_band_decodes_to_its_source_rows() {
        let buffer = gradient(97, 130);
        let bands = encode_bands(&buffer, 0, 6, 4, RESET_PAYLOAD_BUDGET_BYTES).unwrap();
        let mut scratch = Vec::new();
        for band in &bands {
            let mut out = vec![0_u16; (buffer.width() * band.height) as usize];
            decode_region_ops(
                &band.payload,
                buffer.width(),
                band.height,
                Coverage::Full,
                &mut out,
                &mut scratch,
            )
            .unwrap();
            let source = buffer.full_rows(band.y, band.height).unwrap();
            assert_eq!(out, source, "band at y={}", band.y);
        }
    }

    #[test]
    fn noisy_bands_split_until_they_fit_the_wire_cap() {
        let buffer = noise(1920, 256, 9);
        let bands = encode_bands(&buffer, 0, 3, 4, RESET_PAYLOAD_BUDGET_BYTES).unwrap();
        assert!(bands.len() > 1, "noise splits a band");
        for band in &bands {
            assert!(band.payload.len() <= MAX_REGION_WIRE_BYTES);
            assert!(band.op_bytes <= BAND_OPS_SPLIT_BYTES);
        }
        let covered: u32 = bands.iter().map(|band| band.height).sum();
        assert_eq!(covered, 256);
    }

    #[test]
    fn the_frame_budget_truncates_a_prefix() {
        let buffer = noise(1920, 768, 3);
        let budget = 200_000;
        let bands = encode_bands(&buffer, 0, 0, 1, budget).unwrap();
        let total: usize = bands.iter().map(|band| band.payload.len()).sum();
        assert!(total <= budget, "{total} > {budget}");
        let covered: u32 = bands.iter().map(|band| band.height).sum();
        assert!(covered < 768, "the rest is the next frame's work");
    }

    #[test]
    fn a_from_y_offset_restarts_mid_frame() {
        let buffer = gradient(64, 600);
        let bands =
            encode_bands(&buffer, RESET_BAND_ROWS, 1, 1, RESET_PAYLOAD_BUDGET_BYTES).unwrap();
        assert_eq!(bands.first().unwrap().y, RESET_BAND_ROWS);
        let covered: u32 = bands.iter().map(|band| band.height).sum();
        assert_eq!(covered, 600 - RESET_BAND_ROWS);
    }

    #[test]
    fn threads_do_not_change_the_bytes() {
        let buffer = noise(512, 300, 7);
        let sequential = encode_bands(&buffer, 0, 6, 1, RESET_PAYLOAD_BUDGET_BYTES).unwrap();
        let parallel = encode_bands(&buffer, 0, 6, 4, RESET_PAYLOAD_BUDGET_BYTES).unwrap();
        assert_eq!(sequential.len(), parallel.len());
        for (left, right) in sequential.iter().zip(&parallel) {
            assert_eq!(left.y, right.y);
            assert_eq!(left.height, right.height);
            assert_eq!(left.payload, right.payload);
        }
    }

    #[test]
    fn worker_count_respects_the_documented_ceiling() {
        assert!(worker_count(0) >= 1);
        assert!(worker_count(0) <= 8);
        assert_eq!(worker_count(3), 3);
        assert_eq!(worker_count(99), 8);
    }
}
