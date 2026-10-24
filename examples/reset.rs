//! Reset example: band-parallel self-contained update.

use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder};

fn main() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    encoder.reset(tessellio_core::format::ResetReason::Operator);
    if let Ok(Some(planned)) = encoder.encode(&frame) {
        let update = encoder.commit(planned, 1);
        println!(
            "reset update: {} bytes, base={}",
            update.payload_bytes(),
            update.update_base
        );
    }
}
