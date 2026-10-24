//! Parts example: split an update into parts.

use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder, UpdateSplitter};

fn main() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    if let Ok(Some(planned)) = encoder.encode(&frame) {
        let update = encoder.commit(planned, 1);
        let mut splitter = UpdateSplitter::new();
        match splitter.part(update.clone()) {
            Ok(parts) => println!("parts: {}", parts.len()),
            Err(e) => println!("split err: {:?}", e),
        }
    }
}
