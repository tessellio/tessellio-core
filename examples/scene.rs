//! Scene example: use reference scenes.

use tessellio_core::scene;

fn main() {
    let frame = scene::office(640, 360, 42);
    println!("office scene: {}x{}", frame.width(), frame.height());
    let doc = scene::document(640, 360);
    println!("document scene: {}x{}", doc.width(), doc.height());
}
