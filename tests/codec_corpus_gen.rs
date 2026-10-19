//! The committed screen-codec corpus: the enumerator and regenerator.
//!
//! Run with `TESSELLIO_CORPUS_OUT=<path>` to write the corpus and print
//! `codec-corpus: cases=N bytes=M regions=R`; run without it to print the absent
//! marker `tessellio-corpus: no corpus` and do nothing. The gate runs both
//! halves: the generator must reproduce the committed file byte for byte, and a
//! suite that stopped reading it must fail.
//!
//! Schema (one canonical copy, read by the Rust conformance test and by the
//! receiver's TypeScript conformance test):
//!
//! ```text
//! { "schema": 1,
//!   "generator": "tessellio-core/tests/codec_corpus_gen.rs",
//!   "cases": [
//!     { "id": "…", "mode": "roundtrip",
//!       "frames": [
//!         { "updateId": 1, "baseUpdateId": 0, "width": 64, "height": 48,
//!           "chunks": [ { "index": 0, "count": 1, "updateBytes": 1234,
//!                         "regions": [ { "x": 32, "y": 16, "width": 8, "height": 8,
//!                                        "payload": "01e600f4…" } ] } ],
//!           "expect": { "cells565": "0000f800…", "ack": true } } ] },
//!     { "id": "…", "mode": "roundtrip", …,
//!       "expect": { "surfaceCrc32": "1a2b3c4d", "surfaceBytes": 524288, "ack": true } },
//!     { "id": "corrupt-trailing-bytes", "mode": "expect-error",
//!       "reason": "DECODE_ERROR", "coverage": "sparse",
//!       "payload": "…", "region": { "width": 4, "height": 1 } },
//!     { "id": "chunk-unknown-codec", "mode": "expect-error",
//!       "reason": "UNKNOWN_CODEC", "chunk": { …one chunk… } },
//!     { "id": "assembly-incomplete", "mode": "expect-error",
//!       "reason": "INCOMPLETE_FRAME", "chunks": [ …chunks fed in order… ] }
//!   ] }
//! ```
//!
//! Large canvases carry a CRC32 and the canvas byte length instead of the full
//! cell hex; at least forty cases carry full `pixels565`. The wire-cap case
//! (`payload` over `MAX_REGION_WIRE_BYTES`) lives in the unit suites rather than
//! here: its 256 KiB payload would be 512 KiB of hex and consume half the corpus
//! budget to test one `>` comparison.

// The crate's lint table does not apply to an integration test,
// so the lints it allows are allowed here too.
#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::if_same_then_else,
    clippy::manual_clamp,
    clippy::manual_is_multiple_of,
    clippy::manual_range_contains,
    clippy::needless_update,
    clippy::type_complexity,
    clippy::unnecessary_cast,
    clippy::useless_vec,
    clippy::while_let_loop,
    unreachable_pub,
    unused_qualifications
)]
mod common;

use std::fs;

use common::*;
use tessellio_core::CapturedFrame;
use tessellio_core::{
    AssemblyOutcome, Chunk, Codec, CodecConfig, Coverage, EncodedUpdate, OP_END, OP_FILL, OP_ROW,
    Region, ScreenStreamEncoder, SurfaceCanvas, UpdateAssembler, UpdateChunker, decode_region_ops,
    deflate_ops, emit_end, emit_fill, emit_full_row, emit_row, emit_span, validate_chunk,
};

use serde_json::{Value, json};

/// Cases the corpus must carry at least.
const MIN_CASES: usize = 64;
/// The corpus file's size budget.
const MAX_BYTES: usize = 1024 * 1024;
/// Cases that carry full `pixels565` (design floor).
const MIN_FULL_PIXEL_CASES: usize = 40;

/// Lowercase hex of a byte string.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The little-endian wire bytes of the 16-bit cell format words.
fn surface_bytes(values: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Lowercase hex of the 16-bit cell format words in wire order.
fn hex_u16(values: &[u16]) -> String {
    hex(&surface_bytes(values))
}

/// IEEE CRC32 (the corpus's canvas checksum for large frames).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn region_json(region: &Region) -> Value {
    json!({
        "x": region.x,
        "y": region.y,
        "width": region.width,
        "height": region.height,
        "payload": hex(&region.payload),
    })
}

fn chunk_json(chunk: &Chunk) -> Value {
    json!({
        "updateId": chunk.update_id,
        "baseUpdateId": chunk.update_base,
        "width": chunk.width,
        "height": chunk.height,
        "partIndex": chunk.part_index,
        "partCount": chunk.part_count,
        "updateBytes": chunk.update_bytes,
        "codec": chunk.codec.wire_id(),
        "regions": chunk.regions.iter().map(region_json).collect::<Vec<_>>(),
    })
}

/// One frame's JSON, with its canvas expectation.
fn frame_json(
    update_id: u64,
    update_base: u64,
    chunks: Vec<Chunk>,
    canvas: &SurfaceCanvas,
    full_pixels: bool,
) -> Value {
    let update_bytes: u64 = chunks
        .iter()
        .flat_map(|chunk| chunk.regions.iter())
        .map(|region| region.payload.len() as u64)
        .sum();
    let expect = if full_pixels {
        json!({ "cells565": hex_u16(canvas.cells()), "ack": true })
    } else {
        json!({
            "surfaceCrc32": format!("{:08x}", crc32(&surface_bytes(canvas.cells()))),
            "surfaceBytes": canvas.cells().len() * 2,
            "ack": true,
        })
    };
    json!({
        "updateId": update_id,
        "baseUpdateId": update_base,
        "width": chunks.first().map_or(0, |chunk| chunk.width),
        "height": chunks.first().map_or(0, |chunk| chunk.height),
        "chunks": chunks
            .iter()
            .map(|chunk| {
                json!({
                    "index": chunk.part_index,
                    "count": chunk.part_count,
                    "updateBytes": update_bytes,
                    "regions": chunk.regions.iter().map(region_json).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        "expect": expect,
    })
}

/// Encode a scripted scene into corpus frames.
fn scene_case(steps: Vec<CapturedFrame>, full_pixels: bool) -> (Vec<Value>, usize) {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 6,
        reset_deflate_level: 3,
        encoder_threads: 2,
        // Every scripted step must produce a frame, including an unchanged one:
        // the idle case is a corpus case in its own right.
        idle_update_interval: std::time::Duration::ZERO,
        ..CodecConfig::default()
    });
    let mut canvas = SurfaceCanvas::new();
    let chunker = UpdateChunker::new();
    let mut frames = Vec::new();
    for (index, frame) in steps.iter().enumerate() {
        let planned = encoder
            .encode(frame)
            .expect("encode")
            .expect("a planned frame");
        let encoded = encoder.commit(planned, index as u64 + 1);
        canvas.apply(&encoded).expect("apply");
        let chunks = chunker.chunk(encoded).expect("chunks");
        frames.push(frame_json(
            index as u64 + 1,
            if index == 0 { 0 } else { index as u64 },
            chunks,
            &canvas,
            full_pixels,
        ));
    }
    let full = if full_pixels { frames.len() } else { 0 };
    (frames, full)
}

/// Hand-built op-stream case (for forms the encoder never emits).
fn hand_built_case(
    id: &str,
    width: u32,
    height: u32,
    coverage: Coverage,
    ops: &[u8],
    level: u8,
) -> Value {
    let payload = deflate_ops(ops, level).expect("deflate");
    let mut out = vec![0_u16; (width * height) as usize];
    let mut scratch = Vec::new();
    decode_region_ops(&payload, width, height, coverage, &mut out, &mut scratch)
        .expect("hand-built case decodes");
    json!({
        "id": id,
        "mode": "roundtrip",
        "frames": [ {
            "updateId": 1,
            "baseUpdateId": 0,
            "width": width,
            "height": height,
            "chunks": [ {
                "index": 0,
                "count": 1,
                "updateBytes": payload.len() as u64,
                "regions": [ { "x": 0, "y": 0, "width": width, "height": height,
                               "payload": hex(&payload) } ],
            } ],
            "expect": { "cells565": hex_u16(&out), "ack": true },
        } ],
    })
}

/// Hand-built two-frame case: a full-coverage base and a sparse delta.
fn hand_built_delta_case(
    id: &str,
    width: u32,
    height: u32,
    base_ops: &[u8],
    delta_ops: &[u8],
    level: u8,
) -> Value {
    let base_payload = deflate_ops(base_ops, level).expect("deflate");
    let delta_payload = deflate_ops(delta_ops, level).expect("deflate");
    let mut canvas = SurfaceCanvas::new();
    // The oracle derives the coverage mode from `update_base`, exactly as
    // §4.5 requires: full for a reset frame, sparse for a delta.
    for (update_id, update_base, payload) in [
        (1_u64, 0_u64, base_payload.clone()),
        (2, 1, delta_payload.clone()),
    ] {
        let frame = EncodedUpdate {
            update_id,
            update_base,
            width,
            height,
            codec: Codec::Tessellio,
            regions: vec![Region {
                x: 0,
                y: 0,
                width,
                height,
                payload: payload.clone(),
            }],
            changed_pixels: 0,
            sent_pixels: 0,
            backlog_pixels: 0,
            reset_reasons: Default::default(),
        };
        canvas.apply(&frame).expect("hand-built frame applies");
    }
    let ops_json = |payload: &[u8]| {
        json!([ {
            "index": 0,
            "count": 1,
            "updateBytes": payload.len() as u64,
            "regions": [ { "x": 0, "y": 0, "width": width, "height": height,
                           "payload": hex(payload) } ],
        } ])
    };
    json!({
        "id": id,
        "mode": "roundtrip",
        "frames": [
            {
                "updateId": 1,
                "baseUpdateId": 0,
                "width": width,
                "height": height,
                "chunks": ops_json(&base_payload),
                "expect": { "cells565": hex_u16(&decode_full(&base_payload, width, height)),
                            "ack": true },
            },
            {
                "updateId": 2,
                "baseUpdateId": 1,
                "width": width,
                "height": height,
                "chunks": ops_json(&delta_payload),
                "expect": { "cells565": hex_u16(canvas.cells()), "ack": true },
            },
        ],
    })
}

/// Decode a full-coverage payload into cells.
fn decode_full(payload: &[u8], width: u32, height: u32) -> Vec<u16> {
    let mut out = vec![0_u16; (width * height) as usize];
    let mut scratch = Vec::new();
    decode_region_ops(
        payload,
        width,
        height,
        Coverage::Full,
        &mut out,
        &mut scratch,
    )
    .expect("full coverage decodes");
    out
}

/// Region-level error case.
fn error_case(
    id: &str,
    reason: &str,
    coverage: &str,
    width: u32,
    height: u32,
    payload: &[u8],
) -> Value {
    json!({
        "id": id,
        "mode": "expect-error",
        "reason": reason,
        "coverage": coverage,
        "region": { "width": width, "height": height },
        "payload": hex(payload),
    })
}

/// Chunk-level error case.
fn chunk_error_case(id: &str, reason: &str, chunk: Chunk) -> Value {
    json!({
        "id": id,
        "mode": "expect-error",
        "reason": reason,
        "chunk": chunk_json(&chunk),
    })
}

/// Assembly-level error case (a chunk sequence the assembler must refuse).
fn assembly_error_case(id: &str, reason: &str, chunks: &[Chunk]) -> Value {
    json!({
        "id": id,
        "mode": "expect-error",
        "reason": reason,
        "chunks": chunks.iter().map(chunk_json).collect::<Vec<_>>(),
    })
}

/// A base frame and one change to it.
fn scene_pair(seed: u32, width: u32, height: u32) -> (CapturedFrame, CapturedFrame) {
    let base = office(width, height, seed);
    let mut changed = base.clone();
    fill_rect(
        &mut changed,
        width / 4,
        height / 4,
        width / 8,
        height / 8,
        [10, 200, 30, 255],
    );
    (base, changed)
}

fn corpus() -> Value {
    let mut cases: Vec<Value> = Vec::new();
    let mut full_pixels = 0_usize;

    // -- resets, small enough to carry full cells -------------------------
    for (id, seed, size) in [
        ("reset-flat-32x24", 1_u32, (32_u32, 24_u32)),
        ("reset-flat-1x1", 2, (1, 1)),
        ("reset-flat-2x2", 3, (2, 2)),
        ("reset-flat-64x1", 4, (64, 1)),
        ("reset-flat-1x64", 5, (1, 64)),
        ("reset-office-64x48", 6, (64, 48)),
        ("reset-office-96x64", 7, (96, 64)),
        ("reset-office-33x17", 8, (33, 17)),
        ("reset-office-97x63", 9, (97, 63)),
        ("reset-noise-96x64", 10, (96, 64)),
        ("reset-flat-16x16", 101, (16, 16)),
        ("reset-flat-3x5", 102, (3, 5)),
        ("reset-flat-96x64", 103, (96, 64)),
        ("reset-flat-64x16", 104, (64, 16)),
    ] {
        let frame = if id.starts_with("reset-noise") {
            let mut frame = plain(size.0, size.1, [0, 0, 0, 255]);
            noise_block(&mut frame, 0, 0, size.0, size.1, seed);
            frame
        } else if id.starts_with("reset-flat") {
            plain(size.0, size.1, [70, 90, 110, 255])
        } else {
            office(size.0, size.1, seed)
        };
        let (frames, full) = scene_case(vec![frame], true);
        full_pixels += full;
        cases.push(json!({ "id": id, "mode": "roundtrip", "frames": frames }));
    }

    // -- resets with large canvases (CRC expectations) ---------------------
    for (id, seed, size) in [
        ("reset-office-256x256", 21_u32, (256_u32, 256_u32)),
        ("reset-office-48x300-two-bands", 22, (48, 300)),
        ("reset-office-320x192", 23, (320, 192)),
    ] {
        let frame = office(size.0, size.1, seed);
        let (frames, _) = scene_case(vec![frame], false);
        cases.push(json!({ "id": id, "mode": "roundtrip", "frames": frames }));
    }

    // -- hand-built op forms the encoder never emits -----------------------
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 6, 0x0000);
    emit_row(&mut ops, 1);
    emit_fill(&mut ops, 0, 6, 0xffff);
    emit_row(&mut ops, 2);
    emit_fill(&mut ops, 0, 6, 0x8410);
    emit_end(&mut ops);
    cases.push(hand_built_case(
        "hand-built-6x3-fills",
        6,
        3,
        Coverage::Full,
        &ops,
        0,
    ));

    // A FILL below the encoder's run threshold, plus a row-maximum FILL.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 1, 0xf800);
    emit_fill(&mut ops, 1, 3, 0x001f);
    emit_end(&mut ops);
    cases.push(hand_built_case(
        "hand-built-fill-len-1",
        4,
        1,
        Coverage::Full,
        &ops,
        0,
    ));

    // A 1-px SPAN is sparse, so the case is a delta onto a full-coverage base.
    let mut base_ops = Vec::new();
    emit_row(&mut base_ops, 0);
    emit_fill(&mut base_ops, 0, 64, 0x0000);
    emit_end(&mut base_ops);
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 0, &[0x07e0]);
    emit_end(&mut ops);
    cases.push(hand_built_delta_case(
        "hand-built-span-len-1",
        64,
        1,
        &base_ops,
        &ops,
        0,
    ));

    // The same ops through every DEFLATE variant: stored, fixed, dynamic.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(
        &mut ops,
        0,
        &[0x1234, 0x1234, 0xabcd, 0x1234, 0x1234, 0x1234],
    );
    emit_end(&mut ops);
    for level in [0_u8, 1, 6, 9] {
        cases.push(hand_built_case(
            &format!("deflate-level-{level}"),
            6,
            1,
            Coverage::Sparse,
            &ops,
            level,
        ));
    }

    // A full-coverage frame with every row filled: the reset band form.
    let mut ops = Vec::new();
    for row in 0..8_u32 {
        emit_full_row(&mut ops, row, &[(row * 1000) as u16; 32]);
    }
    emit_end(&mut ops);
    cases.push(hand_built_case(
        "hand-built-full-coverage-32x8",
        32,
        8,
        Coverage::Full,
        &ops,
        3,
    ));

    // -- cursors -----------------------------------------------------------
    for (id, seed, size, shift) in [
        ("delta-pointer-1px-64x48", 31_u32, (64_u32, 48_u32), 1_i32),
        ("delta-pointer-16px-64x48", 32, (64, 48), 16),
        ("delta-pointer-1px-320x256", 41, (320, 256), 1),
        ("delta-pointer-16px-320x256", 42, (320, 256), 16),
    ] {
        let mut base = office(size.0, size.1, seed);
        let hot = (size.0 as i32 / 4, size.1 as i32 / 4);
        draw_cursor(&mut base, hot.0, hot.1);
        let mut moved = base.clone();
        draw_cursor(&mut moved, hot.0 + shift, hot.1);
        let full = size.0 <= 96 && size.1 <= 64;
        let (frames, count) = scene_case(vec![base, moved], full);
        full_pixels += count;
        cases.push(json!({ "id": id, "mode": "roundtrip", "frames": frames }));
    }

    // -- scene deltas ------------------------------------------------------
    for (id, seed, size, full) in [
        ("delta-typing-line-96x64", 33_u32, (96_u32, 64_u32), true),
        ("delta-flat-change-64x48", 35, (64, 48), true),
        ("delta-multi-region-96x64", 36, (96, 64), true),
        ("delta-odd-size-97x63", 37, (97, 63), true),
        ("delta-33x17", 38, (33, 17), true),
        ("delta-scroll-strip-96x64", 39, (96, 64), true),
        ("delta-noise-block-96x64", 40, (96, 64), true),
        ("delta-typing-320x256", 43, (320, 256), false),
        ("delta-flat-change-32x24", 44, (32, 24), true),
        ("delta-typing-line-64x48", 45, (64, 48), true),
    ] {
        let (base, mut changed) = scene_pair(seed, size.0, size.1);
        match id {
            id if id.starts_with("delta-noise") => {
                noise_block(
                    &mut changed,
                    size.0 / 4,
                    size.1 / 4,
                    size.0 / 2,
                    size.1 / 2,
                    seed,
                );
            }
            id if id.starts_with("delta-scroll") => {
                scroll_region(&mut changed, 0, 0, size.0, size.1, 8);
            }
            id if id.starts_with("delta-typing") => {
                text_line(&mut changed, 8, size.1 / 2, 6, seed.wrapping_add(5));
            }
            id if id.starts_with("delta-multi-region") => {
                fill_rect(&mut changed, 2, 2, 6, 6, [200, 20, 20, 255]);
                fill_rect(
                    &mut changed,
                    size.0 - 10,
                    size.1 - 10,
                    6,
                    6,
                    [20, 20, 200, 255],
                );
            }
            id if id.starts_with("delta-flat") => {
                fill_rect(&mut changed, 0, 0, size.0, size.1 / 2, [12, 34, 56, 255]);
            }
            _ => {}
        }
        let (frames, count) = scene_case(vec![base, changed], full);
        full_pixels += count;
        cases.push(json!({ "id": id, "mode": "roundtrip", "frames": frames }));
    }

    // -- single-cell, single-row, and single-column regions ---------------
    let single_cases: [(&str, fn(&mut CapturedFrame)); 4] = [
        (
            "delta-single-cell-1x1-region",
            |frame: &mut CapturedFrame| {
                fill_rect(frame, 3, 4, 1, 1, [0, 0, 0, 255]);
            },
        ),
        ("delta-single-row-region", |frame: &mut CapturedFrame| {
            fill_rect(frame, 5, 7, 40, 1, [255, 0, 0, 255]);
        }),
        ("delta-single-column-region", |frame: &mut CapturedFrame| {
            fill_rect(frame, 20, 5, 1, 30, [0, 255, 0, 255]);
        }),
        ("delta-row-maximum-fill", |frame: &mut CapturedFrame| {
            fill_rect(frame, 0, 10, 64, 1, [9, 9, 9, 255]);
        }),
    ];
    for (id, mutate) in single_cases {
        let base = plain(64, 48, [200, 200, 200, 255]);
        let mut changed = base.clone();
        mutate(&mut changed);
        let (frames, count) = scene_case(vec![base, changed], true);
        full_pixels += count;
        cases.push(json!({ "id": id, "mode": "roundtrip", "frames": frames }));
    }

    // -- the idle frame ----------------------------------------------------
    let base = office(64, 48, 51);
    let (frames, count) = scene_case(vec![base.clone(), base], true);
    full_pixels += count;
    cases.push(json!({ "id": "delta-empty-idle", "mode": "roundtrip", "frames": frames }));

    // -- progressive refresh: 128×128 noise at a 2048 px budget ------------
    {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            refresh_cells: 2048,
            delta_deflate_level: 6,
            reset_deflate_level: 3,
            encoder_threads: 1,
            idle_update_interval: std::time::Duration::ZERO,
            ..CodecConfig::default()
        });
        let mut canvas = SurfaceCanvas::new();
        let mut frame = plain(128, 128, [240, 240, 240, 255]);
        let mut frames = Vec::new();
        for step in 0..4_u64 {
            if step == 1 {
                noise_block(&mut frame, 0, 0, 128, 128, 77);
            }
            let planned = encoder.encode(&frame).expect("encode").expect("frame");
            let encoded = encoder.commit(planned, step + 1);
            canvas.apply(&encoded).expect("apply");
            let chunks = UpdateChunker::new().chunk(encoded).expect("chunks");
            frames.push(frame_json(
                step + 1,
                if step == 0 { 0 } else { step },
                chunks,
                &canvas,
                false,
            ));
        }
        cases.push(json!({
            "id": "progressive-noise-128x128-refresh-2048",
            "mode": "roundtrip",
            "frames": frames,
        }));
    }

    // -- a 64-region frame (the merge cap) ---------------------------------
    {
        let mut frame = plain(512, 512, [250, 250, 250, 255]);
        for tile_y in 0..8_u32 {
            for tile_x in 0..8_u32 {
                fill_rect(
                    &mut frame,
                    tile_x * 64 + 8,
                    tile_y * 64 + 8,
                    3,
                    3,
                    [(tile_x * 8) as u8, (tile_y * 8) as u8, 128, 255],
                );
            }
        }
        let (frames, _) = scene_case(vec![frame], false);
        cases.push(json!({
            "id": "reset-64-regions-512x512",
            "mode": "roundtrip",
            "frames": frames,
        }));
    }

    // -- one reset frame split across four chunks --------------------------
    {
        let frame = office(64, 1080, 61);
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            reset_deflate_level: 3,
            idle_update_interval: std::time::Duration::ZERO,
            ..CodecConfig::default()
        });
        let planned = encoder.encode(&frame).expect("encode").expect("frame");
        let encoded = encoder.commit(planned, 1);
        let whole = UpdateChunker::new().chunk(encoded).expect("chunks");
        let regions: Vec<Region> = whole
            .iter()
            .flat_map(|chunk| chunk.regions.clone())
            .collect();
        let update_bytes: u64 = regions
            .iter()
            .map(|region| region.payload.len() as u64)
            .sum();
        let groups = regions.len().min(4).max(1);
        let mut chunks = Vec::new();
        let mut start = 0_usize;
        for index in 0..groups {
            let size = regions.len() / groups + usize::from(index < regions.len() % groups);
            chunks.push(Chunk {
                update_id: 1,
                update_base: 0,
                width: frame.width,
                height: frame.height,
                part_index: index as u32,
                part_count: groups as u32,
                update_bytes,
                codec: Codec::Tessellio,
                regions: regions[start..start + size].to_vec(),
            });
            start += size;
        }
        // Assemble the four chunks with the receiver oracle, then decode the
        // assembled frame to compute the expected canvas.
        let mut assembler = UpdateAssembler::new();
        let mut assembled = None;
        for chunk in &chunks {
            if let AssemblyOutcome::Complete(frame) = assembler.push(chunk.clone()) {
                assembled = Some(frame);
            }
        }
        let assembled = assembled.expect("the four chunks assemble");
        let encoded = EncodedUpdate {
            update_id: 1,
            update_base: 0,
            width: frame.width,
            height: frame.height,
            codec: Codec::Tessellio,
            regions: assembled.regions,
            changed_pixels: 0,
            sent_pixels: 0,
            backlog_pixels: 0,
            reset_reasons: Default::default(),
        };
        let mut canvas = SurfaceCanvas::new();
        canvas.apply(&encoded).expect("apply");
        cases.push(json!({
            "id": "reset-64x1080-four-chunks",
            "mode": "roundtrip",
            "frames": [ {
                "updateId": 1,
                "baseUpdateId": 0,
                "width": frame.width,
                "height": frame.height,
                "chunks": chunks.iter().map(|chunk| json!({
                    "index": chunk.part_index,
                    "count": chunk.part_count,
                    "updateBytes": chunk.update_bytes,
                    "regions": chunk.regions.iter().map(region_json).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "expect": {
                    "surfaceCrc32": format!("{:08x}", crc32(&surface_bytes(canvas.cells()))),
                    "surfaceBytes": canvas.cells().len() * 2,
                    "ack": true,
                },
            } ],
        }));
    }

    // -- region-level error cases -----------------------------------------
    let mut valid = Vec::new();
    emit_full_row(&mut valid, 0, &[0_u16; 4]);
    emit_end(&mut valid);
    let valid_payload = deflate_ops(&valid, 6).expect("deflate");

    let mut ops = valid.clone();
    ops.pop();
    cases.push(error_case(
        "corrupt-truncated-op-stream",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = valid.clone();
    ops.push(0x00);
    cases.push(error_case(
        "corrupt-trailing-bytes",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = Vec::new();
    emit_row(&mut ops, 1);
    emit_fill(&mut ops, 0, 4, 0);
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 4, 0);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-row-backwards",
        "DECODE_ERROR",
        "sparse",
        4,
        2,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 0, &[1, 2]);
    emit_span(&mut ops, 1, &[3]);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-span-overlap",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 3, 2, 0);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-span-past-width",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    // A zero-length run is not expressible through the emitters (they assert
    // it away), so the corrupt stream is spelled out byte for byte.
    let ops = vec![
        OP_ROW, 0x00, 0x00, OP_FILL, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, OP_END,
    ];
    cases.push(error_case(
        "corrupt-zero-length-span",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = Vec::new();
    emit_row(&mut ops, 4);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-row-past-height",
        "DECODE_ERROR",
        "sparse",
        4,
        4,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = valid.clone();
    ops.insert(0, 0x7f);
    cases.push(error_case(
        "corrupt-unknown-op",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    cases.push(error_case(
        "corrupt-deflate-garbage",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &[0xff, 0xff, 0xff, 0xff],
    ));

    cases.push(error_case(
        "corrupt-deflate-truncated",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &valid_payload[..valid_payload.len() / 2],
    ));

    cases.push(error_case(
        "corrupt-empty-payload",
        "DECODE_ERROR",
        "sparse",
        4,
        1,
        &[],
    ));

    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 2, 0);
    emit_fill(&mut ops, 3, 3, 0);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-coverage-gap",
        "COVERAGE_INVALID",
        "full",
        6,
        1,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = Vec::new();
    emit_full_row(&mut ops, 0, &[0_u16; 6]);
    emit_full_row(&mut ops, 2, &[0_u16; 6]);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-coverage-missing-row",
        "COVERAGE_INVALID",
        "full",
        6,
        3,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    let mut ops = Vec::new();
    emit_full_row(&mut ops, 1, &[0_u16; 6]);
    emit_end(&mut ops);
    cases.push(error_case(
        "corrupt-coverage-first-row-missing",
        "COVERAGE_INVALID",
        "full",
        6,
        2,
        &deflate_ops(&ops, 6).expect("deflate"),
    ));

    // -- chunk-level error cases ------------------------------------------
    let good = Chunk {
        update_id: 10,
        update_base: 9,
        width: 64,
        height: 48,
        part_index: 0,
        part_count: 1,
        update_bytes: 16,
        codec: Codec::Tessellio,
        regions: vec![Region {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            payload: vec![1; 16],
        }],
    };
    assert_eq!(validate_chunk(&good), Ok(()), "the control chunk validates");

    let mut chunk = good.clone();
    chunk.update_id = 0;
    cases.push(chunk_error_case("chunk-bad-id", "DECODE_ERROR", chunk));

    let mut chunk = good.clone();
    chunk.update_base = 99;
    cases.push(chunk_error_case("chunk-bad-base", "BASE_MISMATCH", chunk));

    let mut chunk = good.clone();
    chunk.width = 0;
    cases.push(chunk_error_case(
        "chunk-bad-geometry",
        "GEOMETRY_MISMATCH",
        chunk,
    ));

    let mut chunk = good.clone();
    chunk.part_count = 0;
    cases.push(chunk_error_case("chunk-bad-chunk", "CHUNK_GAP", chunk));

    let mut chunk = good.clone();
    chunk.part_index = 2;
    chunk.part_count = 2;
    cases.push(chunk_error_case(
        "chunk-index-out-of-range",
        "CHUNK_GAP",
        chunk,
    ));

    let mut chunk = good.clone();
    chunk.regions[0].x = 63;
    chunk.regions[0].width = 8;
    cases.push(chunk_error_case(
        "chunk-region-outside-frame",
        "DECODE_ERROR",
        chunk,
    ));

    let mut chunk = good.clone();
    chunk.regions[0].payload = Vec::new();
    cases.push(chunk_error_case(
        "chunk-empty-payload",
        "DECODE_ERROR",
        chunk,
    ));

    let mut chunk = good.clone();
    chunk.regions = vec![
        Region {
            x: 8,
            y: 0,
            width: 2,
            height: 2,
            payload: vec![1; 16],
        },
        Region {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            payload: vec![1; 16],
        },
    ];
    cases.push(chunk_error_case(
        "chunk-region-order",
        "DECODE_ERROR",
        chunk,
    ));

    // -- assembly error cases ---------------------------------------------
    // A single-chunk frame whose declared `update_bytes` disagrees with the
    // payload it carries: the assembler must reject it as incomplete.
    let mut mismatched = good.clone();
    mismatched.update_bytes = 32;
    cases.push(assembly_error_case(
        "assembly-incomplete-frame-bytes",
        "INCOMPLETE_FRAME",
        &[mismatched],
    ));

    let mut first = good.clone();
    first.part_count = 2;
    first.update_bytes = 32;
    let mut second = good.clone();
    second.part_index = 1;
    second.part_count = 2;
    second.update_bytes = 32;
    second.regions = vec![Region {
        x: 8,
        y: 0,
        width: 2,
        height: 2,
        payload: vec![1; 16],
    }];

    cases.push(assembly_error_case(
        "assembly-chunk-gap",
        "CHUNK_GAP",
        &[second.clone()],
    ));
    {
        let mut other = first.clone();
        other.update_id = 11;
        other.update_base = 10;
        other.part_index = 1;
        cases.push(assembly_error_case(
            "assembly-interrupted",
            "DECODE_ERROR",
            &[first.clone(), other],
        ));
    }

    // Sanity: the assembler accepts the good pair.
    let mut assembler = UpdateAssembler::new();
    assert!(matches!(
        assembler.push(first.clone()),
        AssemblyOutcome::Pending
    ));
    assert!(matches!(
        assembler.push(second.clone()),
        AssemblyOutcome::Complete(_)
    ));

    assert!(
        cases.len() >= MIN_CASES,
        "only {} cases in the corpus",
        cases.len()
    );
    assert!(
        full_pixels >= MIN_FULL_PIXEL_CASES,
        "only {full_pixels} cases carry full pixels565"
    );

    json!({
        "schema": 1,
        "generator": "tessellio-core/tests/codec_corpus_gen.rs",
        "cases": cases,
    })
}

#[test]
fn the_corpus_is_generated_when_requested() {
    let Some(path) = std::env::var("TESSELLIO_CORPUS_OUT").ok() else {
        println!("tessellio-corpus: no corpus");
        return;
    };
    let corpus = corpus();
    let cases = corpus["cases"].as_array().expect("cases").len();
    let regions: usize = corpus["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            case["frames"]
                .as_array()
                .map(|frames| {
                    frames
                        .iter()
                        .map(|frame| {
                            frame["chunks"]
                                .as_array()
                                .map(|chunks| {
                                    chunks
                                        .iter()
                                        .map(|chunk| {
                                            chunk["regions"].as_array().map_or(0, Vec::len)
                                        })
                                        .sum::<usize>()
                                })
                                .unwrap_or(0)
                        })
                        .sum::<usize>()
                })
                .unwrap_or(0)
        })
        .sum();
    let text = serde_json::to_string_pretty(&corpus).expect("serialize");
    assert!(cases >= MIN_CASES, "only {cases} cases");
    assert!(
        text.len() <= MAX_BYTES,
        "corpus is {} bytes, over the {MAX_BYTES} budget",
        text.len()
    );
    fs::write(&path, format!("{text}\n")).expect("write corpus");
    println!(
        "codec-corpus: cases={cases} bytes={} regions={regions}",
        text.len() + 1
    );
}
