//! Pointer example: draw cursor on surface.

use tessellio_core::{Surface, scene};

fn main() {
    let mut frame = scene::office(640, 360, 42);
    scene::draw_cursor(&mut frame, 100, 100);
    println!("frame with cursor: {}x{}", frame.width(), frame.height());
    let _ = Surface::new(320, 180).unwrap();
}
