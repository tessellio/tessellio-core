//! Detect example: dirty tiles.

use tessellio_core::detect::TILE;
use tessellio_core::{CapturedFrame, Surface};

fn main() {
    let width = 320u32;
    let height = 180u32;
    let rgba = vec![0u8; (width * height * 4) as usize];
    let frame = CapturedFrame::new(width, height, rgba).unwrap();
    let base = Surface::new(width, height).unwrap();
    let workspace = Surface::new(width, height).unwrap();
    // Just demonstrate TILE constant
    println!("TILE size: {}", TILE);
    let _ = (frame, base, workspace);
}
