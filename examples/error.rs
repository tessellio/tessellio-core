//! Error example: handle codec errors.

use tessellio_core::{CodecConfig, ScreenStreamEncoder, CapturedFrame};

fn main() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    match encoder.encode(&frame) {
        Ok(Some(planned)) => {
            let update = encoder.commit(planned, 1);
            println!("encoded: {} bytes", update.payload_bytes());
        }
        Ok(None) => println!("no update (idle)"),
        Err(e) => println!("error: {:?}", e),
    }
}
