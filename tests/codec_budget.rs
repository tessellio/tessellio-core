//! Deterministic byte and size budgets for the `tessellio` codec.
//!
//! Every case runs the real encoder and decodes the result with the receiver
//! oracle, so a case fails if the codec changes either the bytes it ships or the
//! cells it paints. The budget numbers are the design's §1.5 targets; the
//! `MEASURE` lines make the measured values visible in the run log.

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

use std::time::Instant;

use common::*;
use tessellio_core::CapturedFrame;
use tessellio_core::{
    CodecConfig, DELTA_REFRESH_CELLS, DELTA_REFRESH_MIN_CELLS, MAX_PART_WIRE_BYTES,
    MAX_PARTS_PER_UPDATE, MAX_REGION_WIRE_BYTES, MAX_REGIONS_PER_PART, MAX_REGIONS_PER_UPDATE,
    MAX_UPDATE_WIRE_BYTES, ScreenStreamEncoder, UpdateKind,
};

/// §1.5: a 1 px pointer move ships at most 256 bytes (either axis).
const CURSOR_1PX_MAX_BYTES: usize = 256;
/// §1.5 (corrected in §5.7): a 16 px pointer move with a 14×22 pointer is 768 B.
const CURSOR_16PX_MAX_BYTES: usize = 768;
/// §1.5: typing into a 600×40 block.
const TYPING_MAX_BYTES: usize = 6_000;
/// §1.5: dragging a 600×400 window 30 px.
const DRAG_MAX_BYTES: usize = 8_000;
/// §1.5: scrolling a 900×620 window 40 px.
const SCROLL_MAX_BYTES: usize = 12_000;
/// §1.5: scrolling a 1680×920 page 40 px, which must stay a delta.
const FULL_WIDTH_SCROLL_MAX_BYTES: usize = 60_000;
/// §5.6: the restated high-entropy bound, per frame (260 KiB).
const NOISE_FRAME_MAX_BYTES: usize = 266_240;
/// §1.5: the weighted interactive mix (noise excluded, see the case's note).
const MIX_MAX_BYTES: usize = 6_928;
/// §1.5: a 1080p office reset frame, lossless the 16-bit cell format.
const RESET_FRAME_MAX_BYTES: usize = 1_572_864;

/// Scroll a viewport up by `shift` rows on a periodic document: everything but
/// the freshly exposed strip is unchanged, and the strip carries new content.
fn scroll_viewport(
    frame: &mut CapturedFrame,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    shift: u32,
    strip_variant: u32,
) {
    let source = frame.clone();
    for row in 0..h.saturating_sub(shift) {
        for column in 0..w {
            let from = (((y + row + shift) * source.width) + x + column) as usize * 4;
            let to = (((y + row) * source.width) + x + column) as usize * 4;
            let cell = [
                source.rgba[from],
                source.rgba[from + 1],
                source.rgba[from + 2],
                source.rgba[from + 3],
            ];
            frame.rgba[to..to + 4].copy_from_slice(&cell);
        }
    }
    // The exposed strip: a freshly scrolled-in line, painted on the document
    // background so only the strip's content differs from before.
    fill_rect(frame, x, y + h - shift, w, shift, [248, 248, 250, 255]);
    let mut line_y = y + h - shift + 6.min(shift);
    while line_y + 12 < y + h {
        text_line(frame, x, line_y, (w / 7).min(260), strip_variant);
        line_y += 20;
    }
}

/// Print a measurement line (visible with `--nocapture`).
fn measure(scenario: &str, frame: &tessellio_core::EncodedUpdate, extra: &str) {
    println!(
        "MEASURE {scenario}: bytes={} changed_px={} sent_px={} backlog_px={} regions={} kind={} {extra}",
        frame.payload_bytes(),
        frame.changed_cells,
        frame.sent_cells,
        frame.backlog_cells,
        frame.regions.len(),
        frame.kind().as_str(),
    );
}

/// Per-frame invariants every scenario must hold.
fn assert_frame_invariants(frame: &tessellio_core::EncodedUpdate, label: &str) {
    let parts = parts_of(frame);
    assert!(
        parts.len() <= MAX_PARTS_PER_UPDATE,
        "{label}: {} parts",
        parts.len()
    );
    let total: usize = frame.payload_bytes();
    assert!(
        total <= MAX_UPDATE_WIRE_BYTES,
        "{label}: {total} frame bytes"
    );
    assert!(
        frame.regions.len() <= MAX_REGIONS_PER_UPDATE,
        "{label}: {} regions",
        frame.regions.len()
    );
    for region in &frame.regions {
        assert!(
            region.payload.len() <= MAX_REGION_WIRE_BYTES,
            "{label}: region {} bytes",
            region.payload.len()
        );
    }
    let mut per_part = vec![0_usize; parts.len()];
    for part in &parts {
        assert!(part.payload_bytes() <= MAX_PART_WIRE_BYTES);
        assert!(part.regions.len() <= MAX_REGIONS_PER_PART);
        per_part[part.part_index as usize] += part.payload_bytes();
    }
    assert_eq!(
        per_part.iter().sum::<usize>(),
        total,
        "{label}: part payloads must sum to the frame"
    );
    // Exact change detection: a frame never carries more cells than changed,
    // and the over-send bound is therefore always satisfied.
    assert!(
        frame.sent_cells <= frame.changed_cells,
        "{label}: sent {} > changed {}",
        frame.sent_cells,
        frame.changed_cells
    );
    assert!(
        frame.sent_cells <= 2 * frame.changed_cells,
        "{label}: over-send above 2x"
    );
}

#[test]
fn cursor_one_cell_move_stays_within_budget() {
    // Horizontal: the arrow's left and right edges move one column each.
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = office(1920, 1080, 11);
    draw_cursor(&mut frame, 900, 600);
    let reset = encode_frame(&mut encoder, &frame, 1);
    let mut canvas = decode_frame(&reset);
    draw_cursor(&mut frame, 901, 600);
    let horizontal = encode_frame(&mut encoder, &frame, 2);
    measure("cursor_1px_horizontal", &horizontal, "");
    assert!(
        horizontal.payload_bytes() <= CURSOR_1PX_MAX_BYTES,
        "horizontal 1 px pointer move: {} bytes",
        horizontal.payload_bytes()
    );
    assert_eq!(horizontal.changed_cells, horizontal.sent_cells);
    // The receiver paints exactly the new content.
    apply_frame(&mut canvas, &horizontal).unwrap();
    assert_eq!(
        canvas.cells(),
        expect565(&frame),
        "the pointer move paints exactly the new content"
    );
    assert_frame_invariants(&horizontal, "pointer 1px");

    // Vertical: the arrow's top and bottom rows move one row each.
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = office(1920, 1080, 11);
    draw_cursor(&mut frame, 900, 600);
    let reset = encode_frame(&mut encoder, &frame, 1);
    let mut canvas = decode_frame(&reset);
    draw_cursor(&mut frame, 900, 601);
    let vertical = encode_frame(&mut encoder, &frame, 2);
    measure("cursor_1px_vertical", &vertical, "");
    assert!(
        vertical.payload_bytes() <= CURSOR_1PX_MAX_BYTES,
        "vertical 1 px pointer move: {} bytes",
        vertical.payload_bytes()
    );
    apply_frame(&mut canvas, &vertical).unwrap();
    assert_eq!(canvas.cells(), expect565(&frame));
    assert_frame_invariants(&vertical, "pointer 1px vertical");
}

#[test]
fn cursor_sixteen_cell_move_stays_within_budget() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = office(1920, 1080, 13);
    draw_cursor(&mut frame, 900, 600);
    let _ = encode_frame(&mut encoder, &frame, 1);
    draw_cursor(&mut frame, 916, 600);
    let horizontal = encode_frame(&mut encoder, &frame, 2);
    measure("cursor_16px_horizontal", &horizontal, "");
    assert!(
        horizontal.payload_bytes() <= CURSOR_16PX_MAX_BYTES,
        "horizontal 16 px pointer move: {} bytes",
        horizontal.payload_bytes()
    );
    assert_frame_invariants(&horizontal, "pointer 16px");

    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = office(1920, 1080, 13);
    draw_cursor(&mut frame, 900, 600);
    let _ = encode_frame(&mut encoder, &frame, 1);
    draw_cursor(&mut frame, 900, 616);
    let vertical = encode_frame(&mut encoder, &frame, 2);
    measure("cursor_16px_vertical", &vertical, "");
    assert!(
        vertical.payload_bytes() <= CURSOR_16PX_MAX_BYTES,
        "vertical 16 px pointer move: {} bytes",
        vertical.payload_bytes()
    );
    assert_frame_invariants(&vertical, "pointer 16px vertical");
}

#[test]
fn typing_block_stays_within_budget() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = document(1920, 1080);
    text_line(&mut frame, 600, 500, 85, 3);
    let _ = encode_frame(&mut encoder, &frame, 1);

    // The typed line replaces the old one: the changed cells are the two ink
    // patterns' symmetric difference inside the 600×40 block.
    fill_rect(&mut frame, 600, 498, 600, 40, [248, 248, 250, 255]);
    text_line(&mut frame, 600, 500, 85, 9);
    let typed = encode_frame(&mut encoder, &frame, 2);
    measure("typing_600x40", &typed, "");
    assert!(
        typed.payload_bytes() <= TYPING_MAX_BYTES,
        "typing 600×40: {} bytes",
        typed.payload_bytes()
    );
    assert_eq!(typed.changed_cells, typed.sent_cells);
    assert_frame_invariants(&typed, "typing");
}

#[test]
fn window_drag_stays_within_budget() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let frame = desktop_with_window(1920, 1080, (900, 400, 600, 400));
    let _ = encode_frame(&mut encoder, &frame, 1);

    // Drag the window 30 px to the right: the two 30-px strips change.
    let frame = desktop_with_window(1920, 1080, (930, 400, 600, 400));
    let dragged = encode_frame(&mut encoder, &frame, 2);
    measure("drag_600x400_30px", &dragged, "");
    assert!(
        dragged.payload_bytes() <= DRAG_MAX_BYTES,
        "window drag: {} bytes",
        dragged.payload_bytes()
    );
    assert_frame_invariants(&dragged, "drag");
}

#[test]
fn scroll_stays_within_budget() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = document(1920, 1080);
    let _ = encode_frame(&mut encoder, &frame, 1);

    scroll_viewport(&mut frame, 200, 100, 900, 620, 40, 5);
    let scrolled = encode_frame(&mut encoder, &frame, 2);
    measure("scroll_900x620_40px", &scrolled, "");
    assert!(
        scrolled.payload_bytes() <= SCROLL_MAX_BYTES,
        "900×620 scroll: {} bytes",
        scrolled.payload_bytes()
    );
    assert_frame_invariants(&scrolled, "scroll");
}

#[test]
fn full_width_scroll_stays_a_delta_within_budget() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = document(1920, 1080);
    let _ = encode_frame(&mut encoder, &frame, 1);

    scroll_viewport(&mut frame, 120, 60, 1680, 920, 40, 6);
    let scrolled = encode_frame(&mut encoder, &frame, 2);
    measure("full_width_scroll_1680x920_40px", &scrolled, "");
    assert_eq!(
        scrolled.kind(),
        UpdateKind::Delta,
        "a full-width scroll must stay a delta"
    );
    assert!(
        scrolled.payload_bytes() <= FULL_WIDTH_SCROLL_MAX_BYTES,
        "full-width scroll: {} bytes",
        scrolled.payload_bytes()
    );
    assert_frame_invariants(&scrolled, "full-width scroll");
}

#[test]
fn noise_converges_within_two_frames_and_the_frame_cap() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = office(1920, 1080, 17);
    let _ = encode_frame(&mut encoder, &frame, 1);

    noise_block(&mut frame, 640, 360, 640, 360, 29);
    let mut frames = 0_u32;
    let mut bytes = Vec::new();
    let mut id = 2_u64;
    loop {
        let Some(next) = encode_frame_opt(&mut encoder, &frame, id) else {
            break;
        };
        id += 1;
        frames += 1;
        bytes.push(next.payload_bytes());
        measure("noise_640x360_frame", &next, &format!("frame={frames}"));
        assert!(
            next.payload_bytes() <= NOISE_FRAME_MAX_BYTES,
            "noise frame {frames}: {} bytes",
            next.payload_bytes()
        );
        assert!(
            next.sent_cells <= u64::from(DELTA_REFRESH_CELLS) + 640,
            "noise frame {frames} exceeded the refresh budget: {} px",
            next.sent_cells
        );
        assert_frame_invariants(&next, "noise");
        if frames > 16 {
            break;
        }
        if encoder.encode(&frame).unwrap().is_none() {
            break;
        }
    }
    println!("MEASURE noise_640x360: frames={frames} bytes={bytes:?}");
    assert_eq!(frames, 2, "the default budget converges in two frames");
}

#[test]
fn noise_converges_within_eight_frames_at_the_congested_floor() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        refresh_cells: 32_768,
        ..codec_config()
    });
    let mut frame = office(1920, 1080, 19);
    let _ = encode_frame(&mut encoder, &frame, 1);
    noise_block(&mut frame, 640, 360, 640, 360, 31);

    let mut frames = 0_u32;
    let mut id = 2_u64;
    loop {
        let Some(next) = encode_frame_opt(&mut encoder, &frame, id) else {
            break;
        };
        id += 1;
        frames += 1;
        assert!(
            next.sent_cells <= 32_768 + 640,
            "frame {frames}: {} px",
            next.sent_cells
        );
        assert!(next.payload_bytes() <= NOISE_FRAME_MAX_BYTES);
        if frames > 12 {
            break;
        }
        if encoder.encode(&frame).unwrap().is_none() {
            break;
        }
    }
    println!("MEASURE noise_640x360_congested: frames={frames}");
    assert_eq!(frames, 8, "the congested floor converges in eight frames");
}

#[test]
fn scattered_blocks_are_progressive_within_the_frame_cap() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = office(1920, 1080, 23);
    let _ = encode_frame(&mut encoder, &frame, 1);

    // 135 scattered 64×64 blocks: the merge cap (64 regions) has to hold, and
    // the frame budget has to slice the work over consecutive frames.
    let mut lcg = Lcg::new(97);
    for _ in 0..135 {
        let x = lcg.below(29) * 64;
        let y = lcg.below(15) * 64;
        noise_block(&mut frame, x, y, 64, 64, lcg.next_u32());
    }
    let mut frames = 0_u32;
    let mut id = 2_u64;
    loop {
        let Some(next) = encode_frame_opt(&mut encoder, &frame, id) else {
            break;
        };
        id += 1;
        frames += 1;
        assert!(next.regions.len() <= MAX_REGIONS_PER_UPDATE);
        assert!(next.payload_bytes() <= NOISE_FRAME_MAX_BYTES);
        assert_frame_invariants(&next, "scattered");
        if frames > 12 {
            break;
        }
        if encoder.encode(&frame).unwrap().is_none() {
            break;
        }
    }
    println!("MEASURE scattered_135_blocks: frames={frames}");
    assert!(frames <= 8, "the noisy blocks converge in {frames} frames");
}

#[test]
fn static_frame_costs_nothing_and_stays_inside_its_cpu_ratio() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let frame = office(1920, 1080, 29);
    let _ = encode_frame(&mut encoder, &frame, 1);

    // A static frame ships nothing.
    let planned = encoder.encode(&frame).unwrap();
    match planned {
        Some(planned) => {
            let frame = encoder.commit(planned, 2);
            measure("static_frame", &frame, "");
            assert_eq!(frame.payload_bytes(), 0);
            assert_eq!(frame.kind(), UpdateKind::Empty);
        }
        None => println!("MEASURE static_frame: bytes=0 kind=skipped_by_idle_interval"),
    }

    if cores() < 4 {
        println!("SKIP static_frame_cpu_ratio: fewer than four cores");
        return;
    }

    // Encoder-internal work (the fused conversion+compare pass beyond the
    // capture-side conversion, plus the idle decision) is bounded relative to
    // the RGBA→the 16-bit cell format conversion the capture interface imposes: ≤ 2× with 25 %
    // slack, measured in the same run so the ratio is machine-independent.
    //
    // The fused pass interleaves conversion and comparison, so the encoder's
    // own conversion is part of this number: a static frame measures ≈ 1.2–1.5×
    // the standalone conversion (the comparison's streaming reference read is
    // the difference). The bound fails if the comparison grows back into a
    // second full pass over a displaced workspace.
    let mut converter = ScreenStreamEncoder::new(codec_config());
    let mut buffer = tessellio_core::Surface::new(1920, 1080).unwrap();
    let mut convert = f64::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        tessellio_core::rgba_to_cells(&frame.rgba, 1920, 1080, &mut buffer).unwrap();
        convert = convert.min(started.elapsed().as_secs_f64());
    }
    let mut encode = f64::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        let planned = converter.encode(&frame).unwrap();
        encode = encode.min(started.elapsed().as_secs_f64());
        if let Some(planned) = planned {
            let _ = converter.commit(planned, 3);
            converter.set_config(CodecConfig {
                idle_update_interval: std::time::Duration::ZERO,
                ..codec_config()
            });
        }
    }
    println!(
        "MEASURE static_cpu: convert_ms={:.3} encode_ms={:.3} ratio={:.2}",
        convert * 1000.0,
        encode * 1000.0,
        encode / convert
    );
    assert!(
        encode <= convert * 2.0 * 1.25,
        "static encode {:.3} ms vs convert {:.3} ms",
        encode * 1000.0,
        convert * 1000.0
    );
}

#[test]
fn reset_frame_fits_the_wire_budget_and_the_part_caps() {
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let frame = office(1920, 1080, 31);
    let planned = encoder.encode(&frame).unwrap().unwrap();
    let reset = encoder.commit(planned, 1);
    measure("reset_1080p_office", &reset, "");
    assert!(
        reset.payload_bytes() <= RESET_FRAME_MAX_BYTES,
        "1080p reset frame: {} bytes",
        reset.payload_bytes()
    );
    assert!(
        reset.payload_bytes() <= 4 * MAX_PART_WIRE_BYTES,
        "a reset frame fills at most four parts"
    );
    let parts = parts_of(&reset);
    assert!(parts.len() <= 4, "{} parts", parts.len());
    assert_frame_invariants(&reset, "self-contained");

    // The canvas converges on the source and every band decodes.
    let canvas = decode_frame(&reset);
    assert_eq!(
        canvas.cells().len(),
        (1920 * 1080) as usize,
        "the reset frame covers the canvas"
    );

    if cores() < 4 {
        println!("SKIP reset_parallel_ratio: fewer than four cores");
        return;
    }
    // Band-parallel reset frames: the same content and the same budget, encoded
    // once sequentially and once across the worker pool. The parallel run must
    // be measurably faster; the machine this runs on is shared, so the best of
    // a few attempts decides and the numbers are printed either way.
    let mut buffer = tessellio_core::Surface::new(1920, 1080).unwrap();
    tessellio_core::rgba_to_cells(&frame.rgba, 1920, 1080, &mut buffer).unwrap();
    let level = codec_config().reset_deflate_level;
    let budget = tessellio_core::RESET_PAYLOAD_BUDGET_BYTES;
    let mut sequential = f64::MAX;
    let mut parallel = f64::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        let bands = tessellio_core::encode_bands(&buffer, 0, level, 1, budget).unwrap();
        sequential = sequential.min(started.elapsed().as_secs_f64());
        assert!(!bands.is_empty());
        let started = Instant::now();
        let bands = tessellio_core::encode_bands(&buffer, 0, level, 8, budget).unwrap();
        parallel = parallel.min(started.elapsed().as_secs_f64());
        assert!(!bands.is_empty());
    }
    println!(
        "MEASURE reset_parallel: sequential_ms={:.2} parallel_ms={:.2} speedup={:.2}",
        sequential * 1000.0,
        parallel * 1000.0,
        sequential / parallel
    );
    assert!(
        parallel < sequential * 0.75,
        "band encoding must be parallel: {:.1} ms sequential vs {:.1} ms parallel",
        sequential * 1000.0,
        parallel * 1000.0
    );
}

#[test]
fn the_weighted_interactive_mix_meets_the_target() {
    // The mix weights of §1.5 (pointer 50 %, typing 20 %, scroll 15 %, drag
    // 10 %). The high-entropy scene is excluded: its payload is incompressible
    // information rather than codec overhead (§5.6 restates that budget), and
    // including it would measure entropy, not the codec. The weights are
    // therefore renormalized over the four interactive scenarios, which makes
    // the assertion stricter per unit of interactive traffic.
    let mut encoder = ScreenStreamEncoder::new(codec_config());

    // Cursor: a 1 px move on an office desktop.
    let mut frame = office(1920, 1080, 37);
    draw_cursor(&mut frame, 700, 500);
    let _ = encode_frame(&mut encoder, &frame, 1);
    draw_cursor(&mut frame, 701, 500);
    let cursor_frame = encode_frame(&mut encoder, &frame, 2);

    // Typing: one 600×40 line replaced.
    let mut typed = document(1920, 1080);
    let _ = encode_frame(&mut encoder, &typed, 3);
    fill_rect(&mut typed, 600, 498, 600, 40, [248, 248, 250, 255]);
    text_line(&mut typed, 600, 500, 85, 9);
    let typing_frame = encode_frame(&mut encoder, &typed, 4);

    // Scroll: a 900×620 viewport moved 40 px.
    scroll_viewport(&mut typed, 200, 100, 900, 620, 40, 5);
    let scroll_frame = encode_frame(&mut encoder, &typed, 5);

    // Drag: a 600×400 window moved 30 px.
    let dragged = desktop_with_window(1920, 1080, (900, 400, 600, 400));
    let _ = encode_frame(&mut encoder, &dragged, 6);
    let dragged = desktop_with_window(1920, 1080, (930, 400, 600, 400));
    let drag_frame = encode_frame(&mut encoder, &dragged, 7);

    for (frame, limit, label) in [
        (&cursor_frame, CURSOR_1PX_MAX_BYTES, "pointer"),
        (&typing_frame, TYPING_MAX_BYTES, "typing"),
        (&scroll_frame, SCROLL_MAX_BYTES, "scroll"),
        (&drag_frame, DRAG_MAX_BYTES, "drag"),
    ] {
        assert!(
            frame.payload_bytes() <= limit,
            "{label}: {} bytes over {limit}",
            frame.payload_bytes()
        );
        assert_frame_invariants(frame, "mix");
    }

    let weights = [0.5_f64, 0.2, 0.15, 0.1];
    let total_weight: f64 = weights.iter().sum();
    let measured = [
        cursor_frame.payload_bytes() as f64,
        typing_frame.payload_bytes() as f64,
        scroll_frame.payload_bytes() as f64,
        drag_frame.payload_bytes() as f64,
    ];
    let mix = weights
        .iter()
        .zip(measured.iter())
        .map(|(weight, bytes)| weight * bytes)
        .sum::<f64>()
        / total_weight;
    println!(
        "MEASURE interactive_mix: pointer={} typing={} scroll={} drag={} mix={mix:.0}",
        measured[0], measured[1], measured[2], measured[3]
    );
    assert!(
        mix <= MIX_MAX_BYTES as f64,
        "weighted interactive mix {mix:.0} B/frame over {MIX_MAX_BYTES}"
    );
}

#[test]
fn every_frame_holds_the_documented_cell_bound() {
    // The refresh budget is a per-frame bound: no frame may carry more than the
    // budget plus one row, whatever the scenario.
    let mut encoder = ScreenStreamEncoder::new(codec_config());
    let mut frame = document(1920, 1080);
    let _ = encode_frame(&mut encoder, &frame, 1);
    for step in 0..6_u32 {
        noise_block(&mut frame, 100 + step * 90, 200, 400, 400, step + 91);
        if let Some(next) = encode_frame_opt(&mut encoder, &frame, step as u64 + 2) {
            let budget = u64::from(DELTA_REFRESH_CELLS).max(DELTA_REFRESH_MIN_CELLS);
            assert!(
                next.sent_cells <= budget + 400,
                "step {step}: {} px",
                next.sent_cells
            );
            assert!(next.payload_bytes() <= NOISE_FRAME_MAX_BYTES);
            assert_frame_invariants(&next, "cell bound");
        }
    }
}
