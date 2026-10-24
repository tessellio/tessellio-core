//! Scroll example.

use tessellio_core::{CapturedFrame, scene};

fn main() {
    let mut frame = scene::office(640, 360, 42);
    scene::scroll_region(&mut frame, 0, 0, 100, 100, 5);
    println!("scrolled frame: {}x{}", frame.width(), frame.height());
    let _ = CapturedFrame::new(320, 180, vec![0u8; 320*180*4]).unwrap();
}
