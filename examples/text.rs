//! Text example.

use tessellio_core::{CapturedFrame, scene};

fn main() {
    let mut frame = CapturedFrame::new(640, 360, vec![0u8; 640*360*4]).unwrap();
    scene::text_line(&mut frame, 10, 10, 20, 1);
    println!("text frame: {}x{}", frame.width(), frame.height());
}
