//! Composite pointer example.

use tessellio_core::{CapturedFrame, composite_pointer_in_place, scene};

fn main() {
    let mut frame = scene::office(640, 360, 42);
    // composite_pointer_in_place takes rgba, width, height, pointer
    println!("frame: {}x{}", frame.width(), frame.height());
    let _ = composite_pointer_in_place;
}
