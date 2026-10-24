//! Format example: constants.

use tessellio_core::format::{CODEC_NAME, CODEC_WIRE_ID, MAX_SURFACE_DIMENSION};

fn main() {
    println!("codec: {}", CODEC_NAME);
    println!("wire id: {}", CODEC_WIRE_ID);
    println!("max dimension: {}", MAX_SURFACE_DIMENSION);
}
