# Changelog

## Unreleased

### Added
- `changed_cell_indices` in `EncodedUpdate` and `PlannedUpdate` for transport
  cell accounting.
- Builder methods for `CodecConfig`.
- 16+ usage examples in `examples/`.

### Changed
- Hybrid DEFLATE backend: the deep levels ride zlib-ng (flate2 feature
  swap), the fast level rides `miniz_oxide`'s raw stream (the fast path
  the pure-Rust backend is the faster machine on), and level 0 writes
  its stored blocks by hand. The reset default level is 6 (what 3
  produced on the old backend, byte for byte on the office reset); the
  conformance corpus regenerated with it.
- **RGBA shadow**: per-row RGBA mirror of the reference; a row the mirror
  vouches for and the capture matches byte-for-byte needs neither
  conversion nor comparison. Validity is earned (reset bands establish
  rows, deltas extend them column-wise, a rebase withdraws everything);
  output is byte-identical with and without it (corpus regenerates
  bit-for-bit). New module `shadow`.
- Performance, encode hot path (bytes on the wire are unchanged):
  - fat-LTO release profile (`lto = "fat"`, `codegen-units = 1`);
  - the op-stream self-check walk is a debug/test build check — an
    optimized build no longer re-walks every op byte it just packed;
  - SPAN payloads are staged through a stack buffer (one `extend` per 128
    cells instead of one per cell);
  - cell indices are built from contiguous ranges instead of per-cell
    push loops (a 1080p reset no longer does 2 073 600 pushes);
  - adjacent write spans merge into one, which also merges undo segments;
  - the row loop reads the clock only when the telemetry hook asked for
    phase timings (`Telemetry::wants_phases`, default true, `NoTelemetry`
    false) — a 1080p frame spent 2 160 `Instant::now()` calls on it;
  - the parallel detector reuses its workers' maps and row scratch across
    frames instead of allocating per worker per frame.
- Adaptive DEFLATE level: each region/band compresses a 16 KiB prefix at
  levels 1 and configured, and takes level 1 when it stays within 2 % of
  the deep level — high-entropy content (noise) encodes ~2.5× faster for
  the same bytes; structured content keeps the deep level.
- `Telemetry` gained `wants_phases` (breaking: trait implementors may need
  to consider it; the default keeps every event flowing).
