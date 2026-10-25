//! Codec conformance: the hand-derived wire vectors and the committed corpus.
//!
//! The five vectors of the design's §9.1 are reproduced **verbatim** here as
//! byte constants, and every one is exercised in both directions: documented hex
//! → decoded cells, and cells → documented hex with byte equality. The corpus
//! (`corpus.json`, or `TESSELLIO_CORPUS`) is
//! consumed case by case when it is present; the gate's own step requires the
//! `codec-conformance:` marker, so a suite that stopped reading the file fails
//! there rather than passing quietly.

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
use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;
use tessellio_core::{
    AssembledUpdate, AssemblyOutcome, Codec, CodecConfig, Coverage, DecodeError, EncodedUpdate,
    Part, Region, ScreenStreamEncoder, SurfaceCanvas, UpdateAssembler, UpdateSplitter,
    decode_region_ops, deflate_ops, emit_end, emit_fill, emit_row, emit_span, validate_part,
};

/// Decode a hex string into bytes.
fn unhex(hex: &str) -> Vec<u8> {
    let hex: String = hex
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    assert!(hex.len() % 2 == 0, "odd hex length");
    (0..hex.len() / 2)
        .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex"))
        .collect()
}

/// Decode one region payload with the receiver oracle.
fn decode_payload(
    payload: &[u8],
    width: u32,
    height: u32,
    coverage: Coverage,
) -> Result<Vec<u16>, DecodeError> {
    let mut out = vec![0_u16; (width * height) as usize];
    let mut scratch = Vec::new();
    decode_region_ops(payload, width, height, coverage, &mut out, &mut scratch)?;
    Ok(out)
}

/// Compress an op stream at the vectors' stored-block level.
fn stored(ops: &[u8]) -> Vec<u8> {
    deflate_ops(ops, 0).expect("deflate")
}

/// The stored-block envelope of an op stream: `01 LEN ~LEN` + the ops.
fn envelope(ops: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x01];
    payload.extend_from_slice(&(ops.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(!(ops.len() as u16)).to_le_bytes());
    payload.extend_from_slice(ops);
    payload
}

/// The the 16-bit cell format cell hex the corpus uses (wire byte order per word).
fn surface_hex(canvas: &SurfaceCanvas) -> String {
    let mut out = String::with_capacity(canvas.cells().len() * 4);
    for value in canvas.cells() {
        out.push_str(&format!("{:02x}{:02x}", value & 0xff, value >> 8));
    }
    out
}

/// CRC32 of the canvas bytes.
fn canvas_crc(canvas: &SurfaceCanvas) -> u32 {
    let mut bytes = Vec::with_capacity(canvas.cells().len() * 2);
    for value in canvas.cells() {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Build the oracle's input frame from an assembled part sequence.
fn assembled_frame(assembled: AssembledUpdate) -> EncodedUpdate {
    EncodedUpdate {
        update_id: assembled.update_id,
        base_update_id: assembled.base_update_id,
        width: assembled.width,
        height: assembled.height,
        codec: Codec::Tessellio,
        regions: assembled.regions,
        changed_cells: 0,
        sent_cells: 0,
        backlog_cells: 0,
        changed_cell_indices: Vec::new(),
        reset_reasons: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// V1 — empty delta (idle frame)
// ---------------------------------------------------------------------------

#[test]
fn v1_empty_delta_advances_the_canvas_without_painting() {
    // Header: update_id=7, base_update_id=6, width=8, height=4, part_index=0,
    // part_count=1, update_bytes=0, codec=8, regions=[].
    // Documented proto bytes (12): 08 07 10 06 18 08 20 04 30 01 40 08
    // (the proto encoding itself belongs to the schema's generated types).
    let part = Part {
        update_id: 7,
        base_update_id: 6,
        width: 8,
        height: 4,
        part_index: 0,
        part_count: 1,
        update_bytes: 0,
        codec: Codec::Tessellio,
        regions: Vec::new(),
    };
    assert_eq!(part.codec.wire_id(), 8);
    assert_eq!(part.update_bytes, 0);
    assert_eq!(part.part_count, 1);
    assert_eq!(validate_part(&part), Ok(()));

    let mut assembler = UpdateAssembler::new();
    let assembled = match assembler.push(part) {
        AssemblyOutcome::Complete(assembled) => assembled,
        other => panic!("expected completion, got {other:?}"),
    };
    assert_eq!(assembled.update_id, 7);
    assert_eq!(assembled.base_update_id, 6);
    assert_eq!((assembled.width, assembled.height), (8, 4));
    assert!(assembled.regions.is_empty());

    // Expected: canvas unchanged; applied_update_id = 7; ack {update_id: 7}.
    // The receiver holds frame 6, so the test anchors the canvas there first.
    let mut canvas = SurfaceCanvas::new();
    canvas
        .apply(&EncodedUpdate {
            update_id: 6,
            base_update_id: 0,
            width: 8,
            height: 4,
            codec: Codec::Tessellio,
            regions: Vec::new(),
            changed_cells: 0,
            sent_cells: 0,
            backlog_cells: 0,
            changed_cell_indices: Vec::new(),
            reset_reasons: Default::default(),
        })
        .expect("the anchor frame applies");
    let before = canvas.cells().to_vec();
    canvas.apply(&assembled_frame(assembled)).expect("apply");
    assert_eq!(canvas.cells(), before, "an empty delta paints nothing");
    assert_eq!(
        canvas.applied_update_id(),
        7,
        "the idle frame advances the id"
    );
}

// ---------------------------------------------------------------------------
// V2 — literal run (one SPAN, sparse delta)
// ---------------------------------------------------------------------------

/// §9.1 V2 op stream (17 bytes).
const V2_OPS: [u8; 17] = [
    0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x04, 0x00, 0x00, 0xf8, 0xe0, 0x07, 0x1f, 0x00, 0xff, 0xff,
    0x00,
];
/// §9.1 V2 payload (22 bytes, stored DEFLATE).
const V2_PAYLOAD_HEX: &str = "011100eeff010000020000040000f8e0071f00ffff00";

#[test]
fn v2_literal_run_hex_to_cells_and_back() {
    let payload = unhex(V2_PAYLOAD_HEX);
    assert_eq!(payload.len(), 22);
    assert_eq!(payload, envelope(&V2_OPS), "the documented envelope");
    assert_eq!(payload, stored(&V2_OPS));

    // (c) hex → decoded cells: a 4×2 region whose first row carries the run.
    let cells = decode_payload(&payload, 4, 2, Coverage::Sparse).unwrap();
    assert_eq!(cells[0..4], [0xf800, 0x07e0, 0x001f, 0xffff]);
    assert_eq!(cells[4..8], [0, 0, 0, 0], "row 1 is untouched");

    // (b) cells → expected hex, byte equality, through the real encoder. The
    // vector's region box is 4×2 with only its first row written; exact
    // bounding boxes are this codec's change detection, so the encoder's box is
    // 4×1 and its payload is byte-identical.
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    let base = plain(8, 4, [0, 0, 0, 255]);
    let _ = encode_frame(&mut encoder, &base, 1);
    let mut current = base.clone();
    fill_rect(&mut current, 2, 1, 1, 1, [255, 0, 0, 255]);
    fill_rect(&mut current, 3, 1, 1, 1, [0, 255, 0, 255]);
    fill_rect(&mut current, 4, 1, 1, 1, [0, 0, 255, 255]);
    fill_rect(&mut current, 5, 1, 1, 1, [255, 255, 255, 255]);
    let encoded = encode_frame(&mut encoder, &current, 2);
    assert_eq!(encoded.base_update_id, 1);
    assert_eq!(encoded.regions.len(), 1);
    let region = &encoded.regions[0];
    assert_eq!(
        (region.x, region.y, region.width, region.height),
        (2, 1, 4, 1),
        "the encoder's box is the exact changed-cell bbox"
    );
    assert_eq!(
        region.payload, payload,
        "cells → the documented payload bytes"
    );
}

// ---------------------------------------------------------------------------
// V3 — full-coverage multi-row FILL (row-maximum run, reset frame)
// ---------------------------------------------------------------------------

/// §9.1 V3 op stream (31 bytes).
const V3_OPS: [u8; 31] = [
    0x01, 0x00, 0x00, 0x03, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x03, 0x00, 0x00,
    0x06, 0x00, 0xff, 0xff, 0x01, 0x02, 0x00, 0x03, 0x00, 0x00, 0x06, 0x00, 0x10, 0x84, 0x00,
];
/// §9.1 V3 payload (36 bytes, stored DEFLATE).
const V3_PAYLOAD_HEX: &str = "011f00e0ff\
01000003000006000000\
0101000300000600ffff\
01020003000006001084\
00";

#[test]
fn v3_full_coverage_fills_hex_to_cells_and_back() {
    let payload = unhex(V3_PAYLOAD_HEX);
    assert_eq!(payload.len(), 36);
    assert_eq!(payload, envelope(&V3_OPS));
    assert_eq!(payload, stored(&V3_OPS));

    // (c) hex → decoded cells; the full-coverage rule is enforced.
    let cells = decode_payload(&payload, 6, 3, Coverage::Full).unwrap();
    assert_eq!(cells[0..6], [0x0000; 6]);
    assert_eq!(cells[6..12], [0xffff; 6]);
    assert_eq!(cells[12..18], [0x8410; 6]);
    assert_eq!(
        decode_payload(&payload, 6, 3, Coverage::Sparse).unwrap(),
        cells,
        "the same stream is also valid as a sparse stream"
    );

    // (b) cells → documented hex through the crate's own op emitters.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 6, 0x0000);
    emit_row(&mut ops, 1);
    emit_fill(&mut ops, 0, 6, 0xffff);
    emit_row(&mut ops, 2);
    emit_fill(&mut ops, 0, 6, 0x8410);
    emit_end(&mut ops);
    assert_eq!(
        ops,
        V3_OPS.to_vec(),
        "the emitters reproduce the documented op stream"
    );
    assert_eq!(stored(&ops), payload);

    // The encoder paints the same cells with its own op form: a 6 px constant
    // run is below the FILL threshold, so it emits a literal SPAN instead. The
    // vector documents the format (a hand-made FILL), not the encoder's choice.
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    let mut frame = plain(6, 3, [0, 0, 0, 255]);
    fill_rect(&mut frame, 0, 1, 6, 1, [255, 255, 255, 255]);
    fill_rect(&mut frame, 0, 2, 6, 1, [132, 130, 132, 255]);
    let encoded = encode_frame(&mut encoder, &frame, 3);
    assert_eq!(encoded.update_id, 3);
    assert_eq!((encoded.width, encoded.height), (6, 3));
    assert_eq!(encoded.regions.len(), 1);
    let decoded = decode_payload(&encoded.regions[0].payload, 6, 3, Coverage::Full).unwrap();
    assert_eq!(decoded, cells, "the encoder's form paints the same cells");
}

#[test]
fn v3_coverage_mutants_are_rejected_with_coverage_invalid() {
    // A missing row.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 6, 0);
    emit_row(&mut ops, 2);
    emit_fill(&mut ops, 0, 6, 0);
    emit_end(&mut ops);
    let error = decode_payload(&stored(&ops), 6, 3, Coverage::Full).unwrap_err();
    assert_eq!(error, DecodeError::Coverage);
    assert_eq!(error.reason_code(), "COVERAGE_INVALID");

    // A gap inside a row.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 2, 0);
    emit_fill(&mut ops, 3, 3, 0);
    emit_end(&mut ops);
    let error = decode_payload(&stored(&ops), 6, 1, Coverage::Full).unwrap_err();
    assert_eq!(error, DecodeError::Coverage);
    assert_eq!(error.reason_code(), "COVERAGE_INVALID");
}

// ---------------------------------------------------------------------------
// V4 — multi-region sparse delta with a skipped row
// ---------------------------------------------------------------------------

/// §9.1 V4 region A op stream (25 bytes).
const V4_A_OPS: [u8; 25] = [
    0x01, 0x00, 0x00, 0x02, 0x01, 0x00, 0x02, 0x00, 0x1f, 0xf8, 0xff, 0x07, 0x01, 0x01, 0x00, 0x02,
    0x06, 0x00, 0x02, 0x00, 0x1f, 0xf8, 0xff, 0x07, 0x00,
];
/// §9.1 V4 region B op stream (11 bytes).
const V4_B_OPS: [u8; 11] = [
    0x01, 0x00, 0x00, 0x02, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
];

#[test]
fn v4_multi_region_sparse_delta_hex_to_cells_and_back() {
    let a_payload = stored(&V4_A_OPS);
    let b_payload = stored(&V4_B_OPS);
    assert_eq!(a_payload.len(), 30);
    assert_eq!(b_payload.len(), 16);

    // (c) hex → decoded cells, in each region's own coordinate space.
    let a = decode_payload(&a_payload, 8, 2, Coverage::Sparse).unwrap();
    assert_eq!(a[1..3], [0xf81f, 0x07ff], "row 0: (1,0) and (2,0)");
    assert_eq!(a[8 + 6..8 + 8], [0xf81f, 0x07ff], "row 1: (6,1) and (7,1)");
    let b = decode_payload(&b_payload, 8, 2, Coverage::Sparse).unwrap();
    assert_eq!(b[3], 0x0000, "row 0: (3,2) is black");

    // (b) cells → documented hex through the emitters.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 1, &[0xf81f, 0x07ff]);
    emit_row(&mut ops, 1);
    emit_span(&mut ops, 6, &[0xf81f, 0x07ff]);
    emit_end(&mut ops);
    assert_eq!(ops, V4_A_OPS.to_vec());
    assert_eq!(stored(&ops), a_payload);

    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 3, &[0x0000]);
    emit_end(&mut ops);
    assert_eq!(ops, V4_B_OPS.to_vec());
    assert_eq!(stored(&ops), b_payload);

    // (a) field-level decode: two disjoint regions, 30 + 16 = 46 frame bytes.
    assert_eq!(a_payload.len() + b_payload.len(), 46);
    let part = Part {
        update_id: 9,
        base_update_id: 8,
        width: 8,
        height: 4,
        part_index: 0,
        part_count: 1,
        update_bytes: 46,
        codec: Codec::Tessellio,
        regions: vec![
            Region {
                x: 0,
                y: 0,
                width: 8,
                height: 2,
                payload: a_payload.clone(),
            },
            Region {
                x: 0,
                y: 2,
                width: 8,
                height: 2,
                payload: b_payload.clone(),
            },
        ],
    };
    assert_eq!(validate_part(&part), Ok(()));

    // The encoder paints exactly the documented cells; it groups the three
    // changed rows into one region rather than the vector's two, because its
    // region boxes are change-derived and sparse rows may be skipped.
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    let base = plain(8, 4, [0, 0, 0, 255]);
    let reset = encode_frame(&mut encoder, &base, 8);
    let mut current = base.clone();
    fill_rect(&mut current, 1, 0, 1, 1, [255, 0, 255, 255]);
    fill_rect(&mut current, 2, 0, 1, 1, [0, 255, 255, 255]);
    fill_rect(&mut current, 6, 1, 1, 1, [255, 0, 255, 255]);
    fill_rect(&mut current, 7, 1, 1, 1, [0, 255, 255, 255]);
    fill_rect(&mut current, 3, 2, 1, 1, [0, 0, 0, 255]);
    let encoded = encode_frame(&mut encoder, &current, 9);
    let mut canvas = SurfaceCanvas::new();
    canvas.apply(&reset).unwrap();
    canvas.apply(&encoded).unwrap();
    assert_eq!(canvas.cells(), expect565(&current));
}

// ---------------------------------------------------------------------------
// V5 — parted frame (two parts) + field table
// ---------------------------------------------------------------------------

/// §9.1 V5 part 0 payload (16 bytes).
const V5_CHUNK0_HEX: &str = "010b00f4ff010000020000010000f800";
/// §9.1 V5 part 1 payload (16 bytes).
const V5_CHUNK1_HEX: &str = "010b00f4ff0100000200000100e00700";

#[test]
fn v5_parted_frame_hex_to_cells_and_back() {
    let part0_payload = unhex(V5_CHUNK0_HEX);
    let part1_payload = unhex(V5_CHUNK1_HEX);
    assert_eq!(part0_payload.len(), 16);
    assert_eq!(part1_payload.len(), 16);

    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 0, &[0xf800]);
    emit_end(&mut ops);
    assert_eq!(stored(&ops), part0_payload, "part 0 payload bytes");
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 0, &[0x07e0]);
    emit_end(&mut ops);
    assert_eq!(stored(&ops), part1_payload, "part 1 payload bytes");

    assert_eq!(
        decode_payload(&part0_payload, 1, 1, Coverage::Sparse).unwrap(),
        vec![0xf800]
    );
    assert_eq!(
        decode_payload(&part1_payload, 1, 1, Coverage::Sparse).unwrap(),
        vec![0x07e0]
    );

    let part0 = Part {
        update_id: 10,
        base_update_id: 9,
        width: 8,
        height: 4,
        part_index: 0,
        part_count: 2,
        update_bytes: 32,
        codec: Codec::Tessellio,
        regions: vec![Region {
            x: 1,
            y: 1,
            width: 1,
            height: 1,
            payload: part0_payload,
        }],
    };
    let part1 = Part {
        update_id: 10,
        base_update_id: 9,
        width: 8,
        height: 4,
        part_index: 1,
        part_count: 2,
        update_bytes: 32,
        codec: Codec::Tessellio,
        regions: vec![Region {
            x: 2,
            y: 0,
            width: 1,
            height: 1,
            payload: part1_payload,
        }],
    };
    assert_eq!(validate_part(&part0), Ok(()));
    assert_eq!(validate_part(&part1), Ok(()));

    // Expected: applied only after both parts (Σ payloads = 32 = update_bytes).
    let mut assembler = UpdateAssembler::new();
    assert_eq!(
        assembler.push(part0.clone()),
        AssemblyOutcome::Pending,
        "part 0 alone must not be applied"
    );
    let assembled = match assembler.push(part1.clone()) {
        AssemblyOutcome::Complete(assembled) => assembled,
        other => panic!("expected completion, got {other:?}"),
    };
    let sum: usize = assembled
        .regions
        .iter()
        .map(|region| region.payload.len())
        .sum();
    assert_eq!(sum as u64, assembled.update_bytes);
    assert_eq!(assembled.parts, 2);

    // A receiver that sees part 1 first refuses the frame.
    let mut assembler = UpdateAssembler::new();
    assert!(matches!(
        assembler.push(part1.clone()),
        AssemblyOutcome::Rejected(_)
    ));

    // The canvas: (1,1) red, (2,0) green.
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    let base = plain(8, 4, [0, 0, 0, 255]);
    let reset = encode_frame(&mut encoder, &base, 9);
    let mut current = base.clone();
    fill_rect(&mut current, 1, 1, 1, 1, [255, 0, 0, 255]);
    fill_rect(&mut current, 2, 0, 1, 1, [0, 255, 0, 255]);
    let frame = assembled_frame(assembled);
    let mut canvas = SurfaceCanvas::new();
    canvas.apply(&reset).unwrap();
    canvas.apply(&frame).unwrap();
    assert_eq!(canvas.cells(), expect565(&current));
}

// ---------------------------------------------------------------------------
// Receiver rules around the vectors
// ---------------------------------------------------------------------------

#[test]
fn the_frame_and_part_caps_reject_oversized_input() {
    // A region payload over the wire cap.
    let part = Part {
        update_id: 1,
        base_update_id: 0,
        width: 64,
        height: 48,
        part_index: 0,
        part_count: 1,
        update_bytes: 262_145,
        codec: Codec::Tessellio,
        regions: vec![Region {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
            payload: vec![0; 262_145],
        }],
    };
    assert!(validate_part(&part).is_err(), "payload over the wire cap");

    // A frame whose declared bytes exceed the frame cap.
    let mut part = part;
    part.regions[0].payload = vec![0; 16];
    part.update_bytes = (tessellio_core::MAX_UPDATE_WIRE_BYTES + 1) as u64;
    assert!(
        validate_part(&part).is_err(),
        "frame bytes over the frame cap"
    );

    // The splitter refuses a frame it cannot split inside its caps.
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    let frame = plain(32, 32, [0, 0, 0, 255]);
    let encoded = encode_frame(&mut encoder, &frame, 1);
    let mut encoded = encoded;
    encoded.regions[0].payload = vec![0; 1024 * 1024];
    let splitter = UpdateSplitter {
        max_part_bytes: 4096,
        max_regions_per_part: 32,
    };
    assert!(
        splitter.part(encoded).is_err(),
        "a region larger than the part cap is refused, never split"
    );
}

#[test]
fn the_coverage_modes_differ_on_the_same_op_stream() {
    // A gap in a row and a skipped row: full coverage rejects both, sparse
    // coverage accepts both.
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 2, 0x1111);
    emit_fill(&mut ops, 3, 3, 0x2222);
    emit_end(&mut ops);
    let payload = stored(&ops);
    assert_eq!(
        decode_payload(&payload, 6, 1, Coverage::Full).unwrap_err(),
        DecodeError::Coverage
    );
    let sparse = decode_payload(&payload, 6, 1, Coverage::Sparse).unwrap();
    assert_eq!(sparse[0..2], [0x1111, 0x1111]);
    assert_eq!(sparse[3..6], [0x2222, 0x2222, 0x2222]);

    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_fill(&mut ops, 0, 6, 0);
    emit_row(&mut ops, 2);
    emit_fill(&mut ops, 0, 6, 0);
    emit_end(&mut ops);
    let payload = stored(&ops);
    assert_eq!(
        decode_payload(&payload, 6, 3, Coverage::Full).unwrap_err(),
        DecodeError::Coverage
    );
    assert!(decode_payload(&payload, 6, 3, Coverage::Sparse).is_ok());
}

#[test]
fn the_canvas_rejects_a_frame_whose_base_it_does_not_hold() {
    let frame = |update_id: u64, base_update_id: u64| EncodedUpdate {
        update_id,
        base_update_id,
        width: 8,
        height: 4,
        codec: Codec::Tessellio,
        regions: Vec::new(),
        changed_cells: 0,
        sent_cells: 0,
        backlog_cells: 0,
        changed_cell_indices: Vec::new(),
        reset_reasons: Default::default(),
    };
    // Nothing applied yet: a delta with any base is refused.
    let mut canvas = SurfaceCanvas::new();
    assert!(canvas.apply(&frame(2, 1)).is_err());
    // After a reset, only the reset's id is an acceptable base.
    canvas.apply(&frame(1, 0)).expect("self-contained applies");
    assert!(canvas.apply(&frame(3, 2)).is_err());
    assert!(canvas.apply(&frame(2, 1)).is_ok());
    assert_eq!(canvas.applied_update_id(), 2);
}

#[test]
fn the_canvas_discards_a_stale_delta_without_a_resync() {
    let frame = |update_id: u64, base_update_id: u64, region: Option<Region>| EncodedUpdate {
        update_id,
        base_update_id,
        width: 8,
        height: 4,
        codec: Codec::Tessellio,
        regions: region.into_iter().collect(),
        changed_cells: 0,
        sent_cells: 0,
        backlog_cells: 0,
        changed_cell_indices: Vec::new(),
        reset_reasons: Default::default(),
    };
    let mut canvas = SurfaceCanvas::new();
    canvas
        .apply(&frame(1, 0, None))
        .expect("self-contained applies");
    canvas
        .apply(&frame(2, 1, Some(span_region(0, 0, 0xf800))))
        .expect("chained applies");
    // A delta at or below the applied id is stale: discarded, no error, and the
    // canvas keeps what it holds (R0).
    assert!(
        canvas
            .apply(&frame(2, 1, Some(span_region(0, 0, 0x001f))))
            .is_ok()
    );
    assert!(
        canvas
            .apply(&frame(1, 1, Some(span_region(0, 0, 0x07e0))))
            .is_ok()
    );
    assert_eq!(
        canvas.applied_update_id(),
        2,
        "a stale delta never regresses"
    );
    assert_eq!(canvas.cells()[0], 0xf800, "the stale paint never lands");
    // A reset frame is self-contained and geometry-authoritative, so an older
    // one is applied rather than discarded: after a sender id-space restart a
    // receiver must be able to re-anchor on it (R0's own note, §4.5).
    assert!(canvas.apply(&frame(1, 0, None)).is_ok());
    assert_eq!(canvas.applied_update_id(), 1, "a reset frame re-anchors");
    assert_eq!(
        canvas.cells()[0],
        0xf800,
        "an unchanged-geometry reset keeps the canvas (progressive resets)"
    );
}

/// One 1×1 region that paints a single green cell on row 0.
fn span_region(x: u32, y: u32, value: u16) -> Region {
    let mut ops = Vec::new();
    emit_row(&mut ops, 0);
    emit_span(&mut ops, 0, &[value]);
    emit_end(&mut ops);
    Region {
        x,
        y,
        width: 1,
        height: 1,
        payload: stored(&ops),
    }
}

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

/// Path of the canonical corpus, overridable for a local regeneration run.
fn corpus_path() -> PathBuf {
    if let Ok(path) = std::env::var("TESSELLIO_CORPUS") {
        return PathBuf::from(path);
    }
    repo_path("corpus.json")
}

/// Path of the canonical vectors file (written by the conformance workstream).
fn vectors_path() -> PathBuf {
    repo_path("vectors.json")
}

/// Resolve a path from the repository root.
fn repo_path(relative: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.push("tessellio-conformance");
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn u32_field(value: &Value, key: &str) -> u32 {
    value[key].as_u64().unwrap_or_else(|| panic!("{key}")) as u32
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or_else(|| panic!("{key}"))
}

/// One region, from either corpus shape.
fn region_from_json(region: &Value) -> Region {
    Region {
        x: u32_field(region, "x"),
        y: u32_field(region, "y"),
        width: u32_field(region, "width"),
        height: u32_field(region, "height"),
        payload: unhex(region["payload"].as_str().expect("payload")),
    }
}

/// A standalone part (the `expect-error` part shape).
fn part_from_json(part: &Value) -> Part {
    let regions: Vec<Region> = part["regions"]
        .as_array()
        .expect("regions")
        .iter()
        .map(region_from_json)
        .collect();
    Part {
        update_id: u64_field(part, "updateId"),
        base_update_id: u64_field(part, "baseUpdateId"),
        width: u32_field(part, "width"),
        height: u32_field(part, "height"),
        part_index: u32_field(part, "partIndex"),
        part_count: u32_field(part, "partCount"),
        update_bytes: u64_field(part, "updateBytes"),
        codec: Codec::from_wire_id(u32_field(part, "codec") as i32)
            .expect("the corpus carries tessellio"),
        regions,
    }
}

/// Run one corpus case; panics with the case id on any mismatch.
fn run_corpus_case(case: &Value) {
    let id = case["id"].as_str().expect("id");
    match case["mode"].as_str().expect("mode") {
        "roundtrip" => {
            let mut canvas = SurfaceCanvas::new();
            for frame in case["frames"].as_array().expect("frames") {
                let update_id = u64_field(frame, "updateId");
                let base = u64_field(frame, "baseUpdateId");
                let width = u32_field(frame, "width");
                let height = u32_field(frame, "height");
                // Inside a frame the part identity lives at the frame level
                // (the §9.4 schema); the standalone `part` case carries it
                // itself. Both are accepted here.
                let parts: Vec<Part> = frame["parts"]
                    .as_array()
                    .expect("parts")
                    .iter()
                    .map(|part| {
                        let regions: Vec<Region> = part["regions"]
                            .as_array()
                            .expect("regions")
                            .iter()
                            .map(region_from_json)
                            .collect();
                        Part {
                            update_id,
                            base_update_id: base,
                            width,
                            height,
                            part_index: part["index"].as_u64().unwrap_or(0) as u32,
                            part_count: part["count"].as_u64().unwrap_or(1) as u32,
                            update_bytes: part["updateBytes"].as_u64().expect("updateBytes"),
                            codec: Codec::Tessellio,
                            regions,
                        }
                    })
                    .collect();
                let recorded = parts.first().expect("a part").update_bytes;
                let sum: usize = parts
                    .iter()
                    .flat_map(|part| part.regions.iter())
                    .map(|region| region.payload.len())
                    .sum();
                assert_eq!(
                    sum as u64, recorded,
                    "{id}: update_bytes is the whole frame's payload sum"
                );

                let mut assembler = UpdateAssembler::new();
                let mut assembled = None;
                for part in parts {
                    assert_eq!(validate_part(&part), Ok(()), "{id}: part validates");
                    match assembler.push(part) {
                        AssemblyOutcome::Complete(frame) => assembled = Some(frame),
                        AssemblyOutcome::Pending => {}
                        other => panic!("{id}: unexpected assembly outcome {other:?}"),
                    }
                }
                let assembled = assembled.unwrap_or_else(|| panic!("{id}: frame assembles"));
                assert_eq!(assembled.update_id, update_id, "{id}");
                assert_eq!(assembled.base_update_id, base, "{id}");
                assert_eq!((assembled.width, assembled.height), (width, height), "{id}");
                let decoded = assembled_frame(assembled);
                canvas.apply(&decoded).unwrap_or_else(|error| {
                    panic!("{id}: frame {update_id} must apply ({error:?})")
                });
                assert_eq!(canvas.applied_update_id(), update_id, "{id}");
                let expect = &frame["expect"];
                assert_eq!(expect["ack"], Value::Bool(true), "{id}: ack");
                if let Some(hex) = expect["cells565"].as_str() {
                    assert_eq!(surface_hex(&canvas), hex, "{id}: canvas cells");
                } else {
                    let crc = expect["surfaceCrc32"].as_str().expect("surfaceCrc32");
                    let bytes = expect["surfaceBytes"].as_u64().expect("surfaceBytes");
                    assert_eq!(
                        canvas.cells().len() as u64 * 2,
                        bytes,
                        "{id}: canvas byte length"
                    );
                    assert_eq!(
                        format!("{:08x}", canvas_crc(&canvas)),
                        crc,
                        "{id}: canvas CRC32"
                    );
                }
            }
        }
        "expect-error" => {
            let reason = case["reason"].as_str().expect("reason");
            if let Some(payload) = case["payload"].as_str() {
                let width = u32_field(&case["region"], "width");
                let height = u32_field(&case["region"], "height");
                let coverage = match case["coverage"].as_str().unwrap_or("sparse") {
                    "full" => Coverage::Full,
                    _ => Coverage::Sparse,
                };
                let error = decode_payload(&unhex(payload), width, height, coverage)
                    .expect_err(&format!("{id}: must be rejected"));
                assert_eq!(error.reason_code(), reason, "{id}: rejection reason");
            } else if let Some(part) = case.get("part") {
                let part = part_from_json(part);
                let rejection = validate_part(&part).expect_err(&format!("{id}: must be rejected"));
                assert_eq!(rejection.reason_code(), reason, "{id}: rejection reason");
            } else if let Some(parts) = case["parts"].as_array() {
                let mut assembler = UpdateAssembler::new();
                let mut outcome = None;
                for part in parts {
                    outcome = Some(assembler.push(part_from_json(part)));
                }
                match outcome.expect("at least one part") {
                    AssemblyOutcome::Rejected(rejection) => {
                        assert_eq!(rejection.reason_code(), reason, "{id}: rejection reason")
                    }
                    other => panic!("{id}: expected a rejection, got {other:?}"),
                }
            } else {
                panic!("{id}: an expect-error case needs payload, part, or parts");
            }
        }
        other => panic!("{id}: unknown mode {other}"),
    }
}

#[test]
fn the_committed_corpus_is_consumed_case_by_case() {
    let path = corpus_path();
    let Some(text) = read_text(&path) else {
        println!("tessellio-corpus: no corpus");
        println!("codec-conformance: vectors=5 corpus=0 consumed=absent");
        return;
    };
    let corpus: Value = serde_json::from_str(&text).expect("corpus JSON");
    assert_eq!(corpus["schema"], Value::from(1), "corpus schema");
    let cases = corpus["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 64,
        "the corpus carries only {} cases",
        cases.len()
    );
    for case in cases {
        run_corpus_case(case);
    }
    println!(
        "codec-conformance: vectors=5 corpus={} consumed=ok",
        cases.len()
    );
}

#[test]
fn the_vectors_file_matches_the_embedded_vectors_when_present() {
    let path = vectors_path();
    let Some(text) = read_text(&path) else {
        println!("rm-screen-codec-vectors: no vectors file (the embedded vectors are in use)");
        return;
    };
    let vectors: Value = serde_json::from_str(&text).expect("vectors JSON");
    assert_eq!(vectors["schema"], Value::from(1), "vectors schema");
    // The documented payloads must be present byte for byte.
    let normalized: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let hex_of = |bytes: Vec<u8>| {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    assert!(normalized.contains(&hex_of(unhex(V2_PAYLOAD_HEX))));
    assert!(normalized.contains(&hex_of(unhex(V3_PAYLOAD_HEX))));
    assert!(normalized.contains(V5_CHUNK0_HEX));
    assert!(normalized.contains(V5_CHUNK1_HEX));
    println!("codec-conformance: vectors=5 corpus=0 consumed=ok");
}

#[test]
fn the_parter_replicates_frame_identity_in_every_part() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    let frame = office(96, 64, 3);
    let encoded = encode_frame(&mut encoder, &frame, 4);
    let parts = UpdateSplitter::new().part(encoded.clone()).unwrap();
    assert!(!parts.is_empty());
    let total = encoded.payload_bytes();
    for (index, part) in parts.iter().enumerate() {
        assert_eq!(part.part_index, index as u32);
        assert_eq!(part.part_count, parts.len() as u32);
        assert_eq!(part.update_bytes, total as u64);
        assert_eq!(part.update_id, 4);
        assert_eq!(part.base_update_id, 0);
        assert_eq!(part.codec, Codec::Tessellio);
        assert_eq!(validate_part(part), Ok(()));
    }
}

#[test]
fn the_encoder_is_deterministic_for_a_scripted_scene() {
    // The corpus's gate step compares the regenerated file byte for byte; this
    // is the same property inside the suite, so a nondeterministic encoder
    // fails here as well.
    let scene = |encoder: &mut ScreenStreamEncoder| {
        let mut frame = office(96, 64, 5);
        let mut out = Vec::new();
        let reset = encode_frame(encoder, &frame, 1);
        out.push((reset.update_id, reset.payload_bytes(), payloads(&reset)));
        for step in 2..6_u64 {
            text_line(&mut frame, 8, 20 + step as u32 * 4, 6, step as u32);
            let delta = encode_frame(encoder, &frame, step);
            out.push((delta.update_id, delta.payload_bytes(), payloads(&delta)));
        }
        out
    };
    let config = CodecConfig {
        delta_deflate_level: 6,
        reset_deflate_level: 3,
        encoder_threads: 3,
        ..CodecConfig::default()
    };
    let mut first = ScreenStreamEncoder::new(config.clone());
    let mut second = ScreenStreamEncoder::new(config);
    assert_eq!(scene(&mut first), scene(&mut second));
}

/// Every region payload of a frame, for byte-equality comparisons.
fn payloads(frame: &EncodedUpdate) -> Vec<Vec<u8>> {
    frame
        .regions
        .iter()
        .map(|region| region.payload.clone())
        .collect()
}
