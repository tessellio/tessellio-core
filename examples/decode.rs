//! Decode example.

use std::time::Instant;
use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder, SurfaceCanvas};

fn main() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    if let Ok(Some(planned)) = encoder.encode(&frame) {
        let update = encoder.commit(planned, 1);
        let mut canvas = SurfaceCanvas::new();
        if canvas.apply(&update).is_ok() {
            println!("decoded {} cells", canvas.cells().len());
        }
    }
    let _ = Instant::now();
}
