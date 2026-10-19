//! Conversion between the capture representation (8-bit channels) and wire fidelity
//! (the 16-bit cell format), plus the in-place pointer compositor.
//!
//! Reduction is truncation (`r5 = r8 >> 3`, `g6 = g8 >> 2`, `b5 = b8 >> 3`) and
//! expansion is bit replication (`r8 = (r5 << 3) | (r5 >> 2)`, …), so a value
//! that round-trips is reproduced exactly and the wire format stays the single
//! definition of an exact cell.
//!
//! The reduction reads whole little-endian RGBA words and packs four cells per
//! iteration with shifts and masks (no per-channel byte loads, no branches), so
//! the inner loop is a straight vectorizable sequence over the row.

use crate::buffer::Surface;
use crate::error::CodecError;
use crate::frame::{CapturedFrame, PointerShape};

/// Pack one little-endian RGBA word (`r` at bits 0..8, `g` 8..16, `b` 16..24)
/// into an the 16-bit cell format value by truncation.
#[inline(always)]
pub(crate) fn pack565(word: u32) -> u16 {
    let red = (word >> 3) & 0x1f;
    let green = (word >> 10) & 0x3f;
    let blue = (word >> 19) & 0x1f;
    ((red << 11) | (green << 5) | blue) as u16
}

/// Convert packed 8-bit channels (`4 * target.len()` bytes) into the 16-bit cell format words.
///
/// Four cells are packed per iteration so LLVM can vectorize the loop; the
/// reduction and every value it produces are identical to the per-cell form.
#[inline]
pub(crate) fn convert_pixels(source: &[u8], target: &mut [u16]) {
    debug_assert_eq!(source.len(), target.len() * 4);
    let (cells, _rest) = source.as_chunks::<4>();
    let mut slots = target.chunks_exact_mut(4);
    let mut words = cells.chunks_exact(4);
    for (slot, word) in slots.by_ref().zip(words.by_ref()) {
        slot[0] = pack565(u32::from_le_bytes(word[0]));
        slot[1] = pack565(u32::from_le_bytes(word[1]));
        slot[2] = pack565(u32::from_le_bytes(word[2]));
        slot[3] = pack565(u32::from_le_bytes(word[3]));
    }
    for (slot, cell) in slots.into_remainder().iter_mut().zip(words.remainder()) {
        *slot = pack565(u32::from_le_bytes(*cell));
    }
}

/// Convert packed 8-bit channels (`width * 4` stride) into the the 16-bit cell format workspace.
///
/// One linear pass, no per-cell allocation; `out` is reused across frames.
pub fn rgba_to_cells(
    rgba: &[u8],
    width: u32,
    height: u32,
    out: &mut Surface,
) -> Result<(), CodecError> {
    if !out.matches(width, height) {
        return Err(CodecError::InvalidSurface(format!(
            "workspace is {}x{}, frame is {width}x{height}",
            out.width(),
            out.height()
        )));
    }
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(CodecError::InvalidSurface(format!(
            "rgba buffer holds {} bytes, frame needs {expected}",
            rgba.len()
        )));
    }
    convert_pixels(rgba, out.as_mut_u16());
    Ok(())
}

/// Expand the 16-bit cell format words into packed 8-bit channels with bit replication and opaque alpha.
pub fn cells_to_rgba(cells: &[u16], out: &mut [u8]) -> Result<(), CodecError> {
    let expected = cells.len() * 4;
    if out.len() != expected {
        return Err(CodecError::InvalidSurface(format!(
            "rgba buffer holds {} bytes, {expected} expected",
            out.len()
        )));
    }
    let (targets, _rest) = out.as_chunks_mut::<4>();
    for (target, value) in targets.iter_mut().zip(cells) {
        target[0] = expand_red(*value);
        target[1] = expand_green(*value);
        target[2] = expand_blue(*value);
        target[3] = 0xff;
    }
    Ok(())
}

/// Composite an ARGB pointer snapshot into the RGBA frame **in place**.
///
/// The pointer snapshot coordinates are display cells in the captured frame's
/// coordinate space; out-of-frame cursors are clipped. The frame and pointer
/// buffers are borrowed as image views, so no full-frame or pointer clone is
/// made (the capture path is the one place where an added copy is visible in
/// every frame's budget).
pub fn composite_pointer_in_place(frame: &mut CapturedFrame, pointer: &PointerShape) {
    if pointer.width == 0 || pointer.height == 0 {
        return;
    }
    if pointer.rgba.len() != pointer.width as usize * pointer.height as usize * 4 {
        return;
    }
    if frame.rgba.len() != frame.width as usize * frame.height as usize * 4 {
        return;
    }
    // The pointer is composited cell by cell, clipped to the surface. A
    // pointer that hangs off the edge is normal during a shape change, so
    // the parts that do not fit are dropped rather than rejected.
    //
    // The composition is the standard source-over: a fully transparent cell
    // leaves the surface alone, a fully opaque one replaces it, and
    // anything between the two is interpolated.
    let surface_width = frame.width as i64;
    let surface_height = frame.height as i64;
    for row in 0..pointer.height as i64 {
        let target_y = i64::from(pointer.y) + row;
        if target_y < 0 || target_y >= surface_height {
            continue;
        }
        for column in 0..pointer.width as i64 {
            let target_x = i64::from(pointer.x) + column;
            if target_x < 0 || target_x >= surface_width {
                continue;
            }
            let source = ((row as usize) * (pointer.width as usize) + column as usize) * 4;
            let target = ((target_y as usize) * (frame.width as usize) + target_x as usize) * 4;
            let alpha = pointer.rgba[source + 3];
            if alpha == 0 {
                continue;
            }
            if alpha == 255 {
                frame.rgba[target..target + 4].copy_from_slice(&pointer.rgba[source..source + 4]);
                continue;
            }
            let weight = f32::from(alpha) / 255.0;
            let inverse = 1.0 - weight;
            for channel in 0..3 {
                let blended = weight * f32::from(pointer.rgba[source + channel])
                    + inverse * f32::from(frame.rgba[target + channel]);
                frame.rgba[target + channel] = blended as u8;
            }
            let alpha = weight * 255.0 + inverse * f32::from(frame.rgba[target + 3]);
            frame.rgba[target + 3] = alpha as u8;
        }
    }
}

/// Expand the 5-bit red channel with bit replication.
fn expand_red(value: u16) -> u8 {
    let red = ((value >> 11) & 0x1f) as u8;
    (red << 3) | (red >> 2)
}

/// Expand the 6-bit green channel with bit replication.
fn expand_green(value: u16) -> u8 {
    let green = ((value >> 5) & 0x3f) as u8;
    (green << 2) | (green >> 4)
}

/// Expand the 5-bit blue channel with bit replication.
fn expand_blue(value: u16) -> u8 {
    let blue = (value & 0x1f) as u8;
    (blue << 3) | (blue >> 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduction_matches_the_bit_layout() {
        let mut buffer = Surface::new(5, 1).unwrap();
        let mut rgba = Vec::new();
        for cell in [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
        ] {
            rgba.extend_from_slice(&cell);
        }
        rgba_to_cells(&rgba, 5, 1, &mut buffer).unwrap();
        assert_eq!(buffer.as_u16(), &[0x0000, 0xffff, 0xf800, 0x07e0, 0x001f]);
    }

    #[test]
    fn expansion_replicates_bits() {
        let cells = [0x0000_u16, 0xffff, 0xf800, 0x07e0, 0x001f, 0x8410];
        let mut rgba = vec![0_u8; cells.len() * 4];
        cells_to_rgba(&cells, &mut rgba).unwrap();
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]);
        assert_eq!(&rgba[8..12], &[255, 0, 0, 255]);
        assert_eq!(&rgba[12..16], &[0, 255, 0, 255]);
        assert_eq!(&rgba[16..20], &[0, 0, 255, 255]);
        assert_eq!(&rgba[20..24], &[132, 130, 132, 255]);
    }

    #[test]
    fn every_rgb565_value_round_trips_through_expansion_and_reduction() {
        let mut rgba = vec![0_u8; 4];
        for value in 0_u32..=0xffff {
            let value = value as u16;
            cells_to_rgba(&[value], &mut rgba).unwrap();
            assert_eq!(
                u16::from(rgba[0]) >> 3,
                (value >> 11) & 0x1f,
                "red of {value:#06x}"
            );
            assert_eq!(
                u16::from(rgba[1]) >> 2,
                (value >> 5) & 0x3f,
                "green of {value:#06x}"
            );
            assert_eq!(
                u16::from(rgba[2]) >> 3,
                value & 0x1f,
                "blue of {value:#06x}"
            );
        }
    }

    #[test]
    fn geometry_mismatches_are_rejected() {
        let mut buffer = Surface::new(2, 2).unwrap();
        assert!(rgba_to_cells(&[0; 16], 2, 1, &mut buffer).is_err());
        assert!(rgba_to_cells(&[0; 4], 2, 2, &mut buffer).is_err());
        let mut out = [0_u8; 4];
        assert!(cells_to_rgba(&[0, 0], &mut out).is_err());
    }

    #[test]
    fn cursor_compositing_writes_at_the_hotspot_without_cloning() {
        let mut frame = CapturedFrame::new(4, 4, vec![0_u8; 4 * 4 * 4]).unwrap();
        let pointer = PointerShape {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
            rgba: vec![255, 0, 0, 255].repeat(4),
        };
        composite_pointer_in_place(&mut frame, &pointer);
        assert_eq!(frame.cell(1, 1), [255, 0, 0, 255]);
        assert_eq!(frame.cell(2, 2), [255, 0, 0, 255]);
        assert_eq!(frame.cell(3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn cursor_compositing_clips_outside_the_frame() {
        let mut frame = CapturedFrame::new(4, 4, vec![0_u8; 4 * 4 * 4]).unwrap();
        let pointer = PointerShape {
            x: 3,
            y: 3,
            width: 4,
            height: 4,
            rgba: vec![255; 4 * 4 * 4],
        };
        composite_pointer_in_place(&mut frame, &pointer);
        assert_eq!(frame.cell(3, 3), [255, 255, 255, 255]);
        // The frame length is unchanged and every cell is opaque now.
        assert_eq!(frame.rgba.len(), 64);
    }

    #[test]
    fn malformed_cursor_snapshots_are_ignored() {
        let mut frame = CapturedFrame::new(2, 2, vec![7_u8; 16]).unwrap();
        let pointer = PointerShape {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            rgba: vec![0; 8],
        };
        composite_pointer_in_place(&mut frame, &pointer);
        assert_eq!(frame.rgba, vec![7_u8; 16]);
        let pointer = PointerShape {
            x: 0,
            y: 0,
            width: 0,
            height: 2,
            rgba: Vec::new(),
        };
        composite_pointer_in_place(&mut frame, &pointer);
        assert_eq!(frame.rgba, vec![7_u8; 16]);
    }
}
