# Tessellio Core

The Rust reference implementation of the **Tessellio** screen codec, and the
codec's command-line tool.

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
| `scene` | the reference scenes the codec is measured on |
| `measured` | the reference implementation's measured byte bounds |
| `cli` | the command-line tool: the grammar, the JSON shapes, the bench |

The crate is sans-I/O: it never opens a socket, never reads a clock for
anything but a measurement, and never touches the filesystem. The only
dependency is `flate2`, for the DEFLATE envelope; the command-line tool adds
`serde_json`, for the JSON it prints.

## The command-line tool

The crate ships `tessellio-core`, the codec's tool: it is how a person and a
script use the codec without writing Rust.

```console
$ tessellio-core encode --scene office --width 640 --height 360
update encoded
  update id        1
  update base      0
  kind             self-contained
  geometry         640x360
  changed cells    230400
  sent cells       230400
  backlog cells    0
  payload          12874 bytes
  regions          4
  parts            1
  encode           21.464 ms
```

| command | what it does |
|---|---|
| `encode` | encode a surface into the codec's update and print its bytes |
| `inspect` | read an update's bytes and print the envelope's fields and the regions' geometry |
| `decode` | decode an update's bytes and report the regions, the cells and the surface |
| `bench` | run the measured performance scenarios and print the medians and the p95s |
| `limits` | print the codec's limits, as a table and as JSON |

Every command prints a table for a person and, with `--json`, a documented
machine contract; the shapes are in [`docs/cli.md`](docs/cli.md). The exit
status is a contract too: `0` succeeded, `1` the check failed, `2` the usage
was wrong, `3` the input could not be read.

```console
$ tessellio-core bench --iterations 3
the codec's measured performance (3 iteration(s) per scenario)

scenario           bytes   byte budget  median ms  p95 ms   ratio  ratio budget  verdict
pointer_1px        92      128          15.802     16.132   0.21   3.0           within
…
9 of 9 scenario(s) inside their budgets
```

The bench holds the codec to the reference implementation's measured byte
bounds and to a machine-independent encode-time ratio, so it is a gate: a
scenario that leaves its budget makes the command exit with the status `1`.

Build it with `cargo build --release`; the binary is
`target/release/tessellio-core`.

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
cargo test                             # the unit suites and the integration suites
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The wire format's own evidence lives in
[`tessellio-conformance`](https://github.com/tessellio/tessellio-conformance):
the hand-derived vectors, the generated corpus, and the gate that requires
both to be reproducible.

## Status

In development. One moving version, `v0.1.0`, naming the current commit. The
wire format is frozen by the [specification](../tessellio); the tool, the
corpus and the encoder are still moving.

## Licence

PolyForm Noncommercial License 1.0.0. See [LICENSE](LICENSE).

## Sibling repositories

| repository | what it holds |
|---|---|
| [`tessellio`](https://github.com/tessellio/tessellio) | the normative specification |
| [`tessellio-ts`](https://github.com/tessellio/tessellio-ts) | the browser implementation |
| [`tessellio-proto`](https://github.com/tessellio/tessellio-proto) | the wire schema |
| [`tessellio-conformance`](https://github.com/tessellio/tessellio-conformance) | the conformance corpus and its gate |
