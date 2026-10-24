//! Cursor at example.

use tessellio_core::scene;

fn main() {
    let cursor = scene::cursor_at(100, 100);
    println!("cursor shape: {:?}", cursor);
}
