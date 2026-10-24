//! Fill example.

use tessellio_core::scene;

fn main() {
    let mut frame = scene::office(640, 360, 42);
    scene::fill_rect(&mut frame, 10, 10, 50, 50, [255, 0, 0, 255]);
    println!("filled frame: {}x{}", frame.width(), frame.height());
}
