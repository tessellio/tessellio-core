//! The content the codec encodes: a captured surface and the pointer over it.
//!
//! The codec is deliberately strict about what it accepts. Content that does
//! not describe a rectangle of the right size is rejected at the boundary,
//! so that no later stage has to wonder whether its inputs are consistent.

use crate::error::{CodecError, CodecResult};

/// A captured surface: packed 8-bit channels, row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    /// Width in cells.
    pub width: u32,
    /// Height in cells.
    pub height: u32,
    /// The cells, `width * height * 4` bytes: R, G, B, A per cell.
    pub rgba: Vec<u8>,
}

impl CapturedFrame {
    /// A captured frame of the given size.
    ///
    /// # Errors
    ///
    /// A zero dimension, or a byte count that does not match the
    /// dimensions, is rejected.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> CodecResult<Self> {
        if width == 0 || height == 0 {
            return Err(CodecError::InvalidSurface(format!(
                "a {width}x{height} capture has a zero dimension"
            )));
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|cells| cells.checked_mul(4))
            .ok_or_else(|| CodecError::InvalidSurface(format!("{width}x{height} overflows")))?;
        if rgba.len() != expected {
            return Err(CodecError::InvalidSurface(format!(
                "a {width}x{height} capture needs {expected} bytes, got {}",
                rgba.len()
            )));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// The width in cells.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The height in cells.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The cells' bytes.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The four channels of the cell at `(x, y)`.
    ///
    /// A position off the surface yields zeroes rather than panicking: a
    /// pointer that hangs off the edge is normal during a shape change, and a
    /// codec must not panic on a caller's arithmetic.
    #[must_use]
    pub fn cell(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let offset = ((y as usize) * (self.width as usize) + x as usize) * 4;
        let mut out = [0_u8; 4];
        out.copy_from_slice(&self.rgba[offset..offset + 4]);
        out
    }
}

/// A pointer shape drawn over a surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerShape {
    /// The pointer's hotspot, in cells.
    pub x: i32,
    /// The pointer's hotspot, in cells.
    pub y: i32,
    /// The pointer's width, in cells.
    pub width: u32,
    /// The pointer's height, in cells.
    pub height: u32,
    /// The pointer's cells, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl PointerShape {
    /// A pointer shape.
    ///
    /// # Errors
    ///
    /// The byte count must match the dimensions, and neither dimension may
    /// be zero.
    pub fn new(x: i32, y: i32, width: u32, height: u32, rgba: Vec<u8>) -> CodecResult<Self> {
        if width == 0 || height == 0 {
            return Err(CodecError::InvalidSurface(
                "a pointer shape has a zero dimension".to_string(),
            ));
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|cells| cells.checked_mul(4))
            .ok_or_else(|| CodecError::InvalidSurface("the shape overflows".to_string()))?;
        if rgba.len() != expected {
            return Err(CodecError::InvalidSurface(format!(
                "a {width}x{height} shape needs {expected} bytes, got {}",
                rgba.len()
            )));
        }
        Ok(Self {
            x,
            y,
            width,
            height,
            rgba,
        })
    }
}
