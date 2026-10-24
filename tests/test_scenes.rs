use tessellio_core::{CodecConfig, ScreenStreamEncoder, scene};

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
    let frame = scene::terminal(128, 96, 7);
    let _ = frame;
}
