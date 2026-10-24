//! Ops example: deflate ops.

use tessellio_core::ops::{Deflater, emit_end, emit_span};

fn main() {
    let mut ops = Vec::new();
    emit_span(&mut ops, 0, &[1u16, 2, 3]);
    emit_end(&mut ops);
    let mut payload = Vec::new();
    let mut deflater = Deflater::new(6);
    deflater.deflate_into(&ops, 6, &mut payload).unwrap();
    println!("ops: {} bytes, payload: {} bytes", ops.len(), payload.len());
}
