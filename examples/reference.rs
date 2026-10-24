//! Reference example: surface of cells.

use tessellio_core::{Surface, rgba_to_cells};

fn main() {
    let width = 320u32;
    let height = 180u32;
    let rgba = vec![0u8; (width * height * 4) as usize];
    let mut surface = Surface::new(width, height).unwrap();
    rgba_to_cells(&rgba, width, height, &mut surface).unwrap();
    println!(
        "surface: {}x{}, cells: {}",
        surface.width(),
        surface.height(),
        surface.as_u16().len()
    );
}
