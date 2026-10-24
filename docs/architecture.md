# Architecture (Tessellio)

## Crate layout

- `src/lib.rs` — crate root, module map.
- `src/buffer.rs` — Surface (cell grid at wire fidelity).
- `src/convert.rs` — 8-bit channels ↔ 16-bit cells, pointer compositor.
- `src/detect.rs` — dirty tiles, greedy rectangles, exact bounding boxes, merge, per-row spans.
- `src/ops.rs` — op build rules and DEFLATE envelope.
- `src/encode.rs` — stream encoder, refresh budget, commit.
- `src/decode.rs` — decoder, bounded op walk, surface oracle.
- `src/part.rs` — update parts: splitting, validation, assembly.
- `src/reference.rs` — reference surface and undo journal.
- `src/reset.rs` — band-parallel self-contained updates.
- `src/scene.rs` — reference scenes.
- `src/telemetry.rs` — measurements.
- `src/cli/` — command-line tool (behind `cli` feature).

## Sans-IO

No sockets, no clocks (except via parameters), no runtime allocations beyond buffers.

## Feature gates

- `cli` — command-line tool.
- `metrics` — Prometheus metrics export.
- `parallel` — parallel dirty-tile detection (rayon).

## Codec pipeline

1. Convert RGBA → cells.
2. Detect dirty tiles vs reference.
3. Plan regions (greedy rectangles, merge).
4. Build op stream (ROW/SPAN/FILL/END).
5. Deflate op stream.
6. Split into parts if needed.
7. Commit reference update.
