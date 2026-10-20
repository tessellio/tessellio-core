//! A surface's cells at wire fidelity: the 16-bit cell format, row-major, no padding.
//!
//! Conversion, change detection, the encoder reference, and the rebase scratch
//! all speak exactly one representation, so "what changed" and "what is drawn"
//! cannot disagree.

use crate::error::CodecError;
use crate::format::{MAX_SURFACE_CELLS, MAX_SURFACE_DIMENSION};

/// One frame's cells as the 16-bit cell format words (`R[15:11] G[10:5] B[4:0]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    width: u32,
    height: u32,
    cells: Vec<u16>,
}

impl Surface {
    /// Allocate a zero-filled buffer, validating the format limits.
    pub fn new(width: u32, height: u32) -> Result<Self, CodecError> {
        if width == 0 || height == 0 {
            return Err(CodecError::InvalidSurface(format!(
                "screen buffer {width}x{height} is empty"
            )));
        }
        if width > MAX_SURFACE_DIMENSION || height > MAX_SURFACE_DIMENSION {
            return Err(CodecError::InvalidSurface(format!(
                "screen buffer {width}x{height} exceeds the {MAX_SURFACE_DIMENSION} px dimension limit"
            )));
        }
        let cells = u64::from(width) * u64::from(height);
        if cells > MAX_SURFACE_CELLS {
            return Err(CodecError::InvalidSurface(format!(
                "screen buffer {width}x{height} exceeds the {MAX_SURFACE_CELLS} px frame limit"
            )));
        }
        Ok(Self {
            width,
            height,
            cells: vec![0; cells as usize],
        })
    }

    /// Buffer width in cells.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Buffer height in cells.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Cell count.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Whether the buffer has the same geometry as `other`.
    pub fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width && self.height == height
    }

    /// Read-only cell words.
    pub fn as_u16(&self) -> &[u16] {
        &self.cells
    }

    /// Mutable cell words.
    pub fn as_mut_u16(&mut self) -> &mut [u16] {
        &mut self.cells
    }

    /// One row of cells.
    pub fn row(&self, y: u32) -> &[u16] {
        let start = y as usize * self.width as usize;
        &self.cells[start..start + self.width as usize]
    }

    /// One mutable row of cells.
    pub fn row_mut(&mut self, y: u32) -> &mut [u16] {
        let start = y as usize * self.width as usize;
        let end = start + self.width as usize;
        &mut self.cells[start..end]
    }

    /// One row of cells, or an empty slice when `y` is out of bounds.
    pub fn row_or_empty(&self, y: u32) -> &[u16] {
        if y >= self.height {
            return &[];
        }
        self.row(y)
    }

    /// A rectangle of rows as one contiguous slice, when the rectangle spans a
    /// whole row width (`x == 0`), else `None`.
    pub fn full_rows(&self, y: u32, height: u32) -> Option<&[u16]> {
        if height == 0 || y + height > self.height {
            return None;
        }
        let start = y as usize * self.width as usize;
        let end = start + height as usize * self.width as usize;
        Some(&self.cells[start..end])
    }

    /// Fill every cell with `value`.
    pub fn fill(&mut self, value: u16) {
        self.cells.fill(value);
    }

    /// Copy every cell from an identically sized buffer.
    pub fn copy_from(&mut self, other: &Self) -> Result<(), CodecError> {
        if !self.matches(other.width, other.height) {
            return Err(CodecError::InvalidSurface(format!(
                "copy_from geometry mismatch: {}x{} vs {}x{}",
                self.width, self.height, other.width, other.height
            )));
        }
        self.cells.copy_from_slice(&other.cells);
        Ok(())
    }

    /// Overwrite `values` at cell `offset` (masked reference update).
    ///
    /// The offset is a flat cell index; callers derive it from a region/row
    /// pair. An out-of-range span is rejected rather than clamped: a reference
    /// update that silently writes the wrong cells is exactly the failure this
    /// codec removes.
    pub fn blit_span(&mut self, offset: usize, values: &[u16]) -> Result<(), CodecError> {
        let end = offset.saturating_add(values.len());
        if end > self.cells.len() {
            return Err(CodecError::InvalidSurface(format!(
                "blit [{offset}..{end}) exceeds {} cells",
                self.cells.len()
            )));
        }
        self.cells[offset..end].copy_from_slice(values);
        Ok(())
    }

    /// Flat cell offset of `(x, y)`.
    pub fn offset(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    /// The the 16-bit cell format value at `(x, y)`.
    pub fn cell(&self, x: u32, y: u32) -> u16 {
        self.cells[self.offset(x, y)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_is_validated_against_the_format_limits() {
        assert!(Surface::new(0, 4).is_err());
        assert!(Surface::new(4, 0).is_err());
        assert!(Surface::new(9000, 1).is_err());
        assert!(Surface::new(8192, 4096).is_err());
        let buffer = Surface::new(8192, 2048).unwrap();
        assert_eq!(buffer.cell_count(), 16_777_216);
    }

    #[test]
    fn fill_and_blit_write_exact_cells() {
        let mut buffer = Surface::new(4, 2).unwrap();
        buffer.fill(0x1234);
        assert!(buffer.as_u16().iter().all(|value| *value == 0x1234));
        buffer.blit_span(1, &[1, 2, 3]).unwrap();
        assert_eq!(
            buffer.as_u16(),
            &[0x1234, 1, 2, 3, 0x1234, 0x1234, 0x1234, 0x1234]
        );
        assert_eq!(buffer.cell(1, 0), 1);
        assert_eq!(buffer.cell(3, 0), 3);
        assert!(buffer.blit_span(6, &[1, 2, 3]).is_err());
    }

    #[test]
    fn copy_from_requires_matching_geometry() {
        let mut target = Surface::new(4, 2).unwrap();
        let source = Surface::new(4, 2).unwrap();
        target.copy_from(&source).unwrap();
        assert!(target.copy_from(&Surface::new(2, 2).unwrap()).is_err());
        assert!(target.matches(4, 2));
        assert!(!target.matches(2, 4));
    }

    #[test]
    fn rows_are_contiguous_and_bounded() {
        let mut buffer = Surface::new(4, 3).unwrap();
        buffer.row_mut(1).fill(7);
        assert_eq!(buffer.row(1), &[7, 7, 7, 7]);
        assert_eq!(buffer.row_or_empty(9), &[] as &[u16]);
        assert_eq!(buffer.full_rows(1, 2).unwrap().len(), 8);
        assert!(buffer.full_rows(2, 2).is_none());
        assert!(buffer.full_rows(0, 0).is_none());
    }
}
