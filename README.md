# Tessellio Core

The Rust reference implementation of the **Tessellio** screen codec.

Tessellio paints a surface as tiles of cells and moves only the tiles that
changed. An **update** is either **self-contained** (it establishes the whole
surface) or **chained** (it continues exactly the surface the named base
established). A region's content is an op stream inside a raw-DEFLATE
envelope.

Correctness rests on one property: change detection compares the same 16-bit
cells the receiver's canvas holds, and the sender's reference advances only
over the cells an update actually sent — so at every instant the receiver's
canvas equals the sender's reference for the update the receiver has applied.

## What is in the crate

| Module | What it holds |
|---|---|
| `format` | the codec's identity, its limits, regions and rectangles |
| `buffer` | a surface of cells at wire fidelity |
| `frame` | the captured content the codec encodes |
| `convert` | 8-bit channels ↔ 16-bit cells, and the pointer compositor |
| `detect` | dirty tiles, greedy rectangles, exact bounding boxes, merge, per-row spans |
| `ops` | op build rules and the DEFLATE envelope |
| `encode` | the stream encoder, the refresh budget, and commit |
| `reset` | band-parallel self-contained updates |
| `reference` | the reference surface and its undo journal |
| `part` | the update's parts: splitting, validation, and assembly |
| `decode` | the decoder, the bounded op walk, and the surface oracle |
| `telemetry` | what the codec measures, and how a caller collects it |
| `error` | the codec's error vocabulary |

The crate is sans-I/O: it never opens a socket, never reads a clock for
anything but a measurement, and never touches the filesystem. The only
dependency is `flate2`, for the DEFLATE envelope.

## Using it

```rust
use tessellio_core::{CodecConfig, ScreenStreamEncoder};

let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
let update = encoder.encode(&captured, 1)?;
```

The encoder is the only stateful part: it keeps the reference surface, and it
is what makes a lost update heal. Everything else is a pure function over its
arguments.

## Measurements

The codec reports what it observed through a `Telemetry` hook the caller
installs. The default hook discards everything, so a caller that does not
care pays nothing:

```rust
use std::sync::Arc;
use tessellio_core::{NoTelemetry, ScreenStreamEncoder, SharedTelemetry};

let telemetry: SharedTelemetry = Arc::new(MyMetrics);
let encoder = ScreenStreamEncoder::with_telemetry(config, telemetry);
```

A library does not decide where its measurements go: the application embedding
it does.

## Verifying it

```sh
cargo test          # the unit suites and the integration suites
cargo clippy --all-targets
cargo fmt --check
```

The wire format's own evidence lives in
[`tessellio-conformance`](https://github.com/tessellio/tessellio-conformance):
the hand-derived vectors, the generated corpus, and the gate that requires
both to be reproducible.

## Licence

PolyForm Noncommercial License 1.0.0. See [LICENSE](LICENSE).
