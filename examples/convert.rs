//! Convert example: RGBA to cells and back.

use tessellio_core::{cells_to_rgba, rgba_to_cells, Surface};

fn main() {
    let width = 320u32;
    let height = 180u32;
    let rgba = vec![128u8; (width * height * 4) as usize];
    let mut surface = Surface::new(width, height).unwrap();
    rgba_to_cells(&rgba, width, height, &mut surface).unwrap();
    let mut out = vec![0u8; (width * height * 4) as usize];
    cells_to_rgba(surface.as_u16(), &mut out).unwrap();
    println!("converted {} cells", surface.as_u16().len());
}
