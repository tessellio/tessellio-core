//! Bench example: encode office scene and measure.

use std::time::Instant;
use tessellio_core::{CodecConfig, ScreenStreamEncoder, scene};

fn main() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let frame = scene::office(640, 360, 42);
    let t0 = Instant::now();
    if let Ok(Some(planned)) = encoder.encode(&frame) {
        let update = encoder.commit(planned, 1);
        println!(
            "encoded {} bytes in {:.3} ms",
            update.payload_bytes(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
}
