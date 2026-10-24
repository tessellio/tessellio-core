# Quickstart (Tessellio)

## Build

```sh
cargo build
cargo test
```

## Basic usage

```rust
use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder};

let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
let frame = tessellio_core::scene::office(640, 360, 1);
if let Ok(Some(planned)) = encoder.encode(&frame) {
    let update = encoder.commit(planned, 1);
    println!("encoded {} bytes", update.payload_bytes());
}
```

## Feature gates

```sh
cargo build --no-default-features --features cli
cargo build --features parallel
```

## Examples

```sh
cargo run --example basic
cargo run --example scroll_viewport
```
