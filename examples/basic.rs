//! Basic usage of tessellio-core: encode a surface.

use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder};

fn main() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    if let Ok(Some(planned)) = encoder.encode(&frame) {
        let update = encoder.commit(planned, 1);
        println!("encoded update: {} bytes", update.payload_bytes());
    }
}
