//! Frame example: captured frame.

use tessellio_core::CapturedFrame;

fn main() {
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    println!("frame: {}x{}", frame.width(), frame.height());
}
