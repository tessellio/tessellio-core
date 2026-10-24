//! Full example: encode + decode + verify.

use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder, Surface, SurfaceCanvas, rgba_to_cells};

fn main() {
    let width = 320u32;
    let height = 180u32;
    let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
    let rgba = vec![128u8; (width * height * 4) as usize];
    let frame = CapturedFrame::new(width, height, rgba.clone()).unwrap();
    if let Ok(Some(planned)) = encoder.encode(&frame) {
        let update = encoder.commit(planned, 1);
        let mut canvas = SurfaceCanvas::new();
        canvas.apply(&update).unwrap();
        let mut expected = Surface::new(width, height).unwrap();
        rgba_to_cells(&rgba, width, height, &mut expected).unwrap();
        if canvas.cells() == expected.as_u16() {
            println!("round-trip OK: {} cells", canvas.cells().len());
        } else {
            println!("MISMATCH");
        }
    }
}
