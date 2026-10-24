//! Noise block example.

use tessellio_core::{CapturedFrame, scene};

fn main() {
    let mut frame = CapturedFrame::new(640, 360, vec![0u8; 640 * 360 * 4]).unwrap();
    scene::noise_block(&mut frame, 10, 10, 50, 50, 42);
    println!("noise frame: {}x{}", frame.width(), frame.height());
}
