//! Frame helpers for the codec test suites.
//!
//! The scenes themselves live in the crate, in `tessellio_core::scene`: the
//! suites and the crate's command-line tool must measure the same surfaces,
//! and two copies of a fixture drift.

#![allow(dead_code)]
#![allow(unreachable_pub, clippy::needless_range_loop)]

use tessellio_core::CapturedFrame;
use tessellio_core::{
    CodecConfig, Coverage, DecodeError, EncodedUpdate, Part, ScreenStreamEncoder, Surface,
    SurfaceCanvas, UpdateSplitter, decode_region_ops, rgba_to_cells,
};

// The reference scenes live in the crate: the codec's suites and its
// command-line tool must measure the same surfaces, and two copies of a
// fixture drift.
pub use tessellio_core::scene::*;

/// the 16-bit cell format conversion of a frame (the wire-fidelity expectation).
pub fn expect565(frame: &CapturedFrame) -> Vec<u16> {
    let mut buffer = Surface::new(frame.width, frame.height).expect("buffer");
    rgba_to_cells(&frame.rgba, frame.width, frame.height, &mut buffer).expect("convert");
    buffer.as_u16().to_vec()
}

/// Codec configuration with the documented defaults.
pub fn codec_config() -> CodecConfig {
    CodecConfig::default()
}

/// Encode one frame and commit it with `update_id`.
pub fn encode_frame(
    encoder: &mut ScreenStreamEncoder,
    frame: &CapturedFrame,
    update_id: u64,
) -> EncodedUpdate {
    let planned = encoder
        .encode(frame)
        .expect("encode")
        .expect("a planned frame");
    encoder.commit(planned, update_id)
}

/// Encode one frame, tolerating the idle path (returns the committed frame).
pub fn encode_frame_opt(
    encoder: &mut ScreenStreamEncoder,
    frame: &CapturedFrame,
    update_id: u64,
) -> Option<EncodedUpdate> {
    encoder
        .encode(frame)
        .expect("encode")
        .map(|planned| encoder.commit(planned, update_id))
}

/// Split an encoded frame into its parts.
pub fn parts_of(frame: &EncodedUpdate) -> Vec<Part> {
    UpdateSplitter::new().part(frame.clone()).expect("parts")
}

/// Decode a frame with the receiver oracle.
pub fn decode_frame(frame: &EncodedUpdate) -> SurfaceCanvas {
    let mut canvas = SurfaceCanvas::new();
    canvas.apply(frame).expect("apply");
    canvas
}

/// Decode a frame onto an existing canvas.
pub fn apply_frame(canvas: &mut SurfaceCanvas, frame: &EncodedUpdate) -> Result<(), DecodeError> {
    canvas.apply(frame)
}

/// Decode one region payload (sparse) into a fresh buffer.
pub fn decode_region(region: &tessellio_core::Region) -> Vec<u16> {
    let mut out = vec![0_u16; (region.width * region.height) as usize];
    let mut scratch = Vec::new();
    decode_region_ops(
        &region.payload,
        region.width,
        region.height,
        Coverage::Sparse,
        &mut out,
        &mut scratch,
    )
    .expect("region decodes");
    out
}

/// Total payload bytes of a frame.
pub fn update_bytes(frame: &EncodedUpdate) -> usize {
    frame.payload_bytes()
}

/// Cell count of all regions of a frame.
pub fn region_cells(frame: &EncodedUpdate) -> u64 {
    frame
        .regions
        .iter()
        .map(|region| u64::from(region.width) * u64::from(region.height))
        .sum()
}

/// Number of CPU cores available to the process.
pub fn cores() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}
