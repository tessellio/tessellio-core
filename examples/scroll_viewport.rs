//! Scroll viewport example.

use tessellio_core::{CapturedFrame, scene};

fn main() {
    let mut frame = scene::office(640, 360, 42);
    scene::scroll_viewport(&mut frame, 0, 0, 200, 200, 5, 1);
    println!("scrolled viewport: {}x{}", frame.width(), frame.height());
    let _ = CapturedFrame::new(320, 180, vec![0u8; 320*180*4]).unwrap();
}
