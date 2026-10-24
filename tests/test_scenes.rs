use tessellio_core::{scene, CodecConfig, ScreenStreamEncoder};
use tessellio_core::CapturedFrame;

#[test]
fn new_scenes_encode() {
    for (w, h) in [(640u32, 360u32), (320, 240), (1920, 1080)] {
        for scene_fn in [scene::terminal, scene::dashboard] {
            let frame = scene_fn(w, h, 42);
            let mut enc = ScreenStreamEncoder::new(CodecConfig::default());
            let _ = enc.encode(&frame);
        }
    }
}

#[test]
fn new_scenes_roundtrip() {
    use tessellio_core::{CapturedFrame, rgba_to_cells, cells_to_rgba};
    let frame = scene::terminal(128, 96, 7);
    let _ = frame;
}
