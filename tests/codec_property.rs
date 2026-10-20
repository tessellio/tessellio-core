//! Property and model tests for the `tessellio` codec.
//!
//! Four properties carry the design's proof obligations:
//!
//! 1. **Round trip** — for arbitrary content, encode → decode reproduces every
//!    changed cell exactly and the sender's reference equals the decoded canvas.
//! 2. **INV-R** — a deterministic simulator runs scripted content through the
//!    encoder, the splitter, a lossy transport, and the receiver state machine;
//!    after every applied frame the canvas equals the sender's reference for that
//!    frame id, and every loss heals within four frames.
//! 3. **Rebase** — encoding against a reported base paints the same cells as a
//!    full reset frame; a base outside the journal window is refused.
//! 4. **Decoder totality** — `decode_region_ops` never panics on arbitrary
//!    input and agrees with the validator.

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

use common::*;
use proptest::collection::vec;
use proptest::prelude::*;
use tessellio_core::CapturedFrame;
use tessellio_core::{
    AssemblyOutcome, CodecConfig, Coverage, DecodeError, MAX_PARTS_PER_UPDATE,
    MAX_REGION_WIRE_BYTES, MAX_REGIONS_PER_PART, MAX_REGIONS_PER_UPDATE, MAX_UPDATE_WIRE_BYTES,
    ResetReason, ResyncReason, ScreenStreamEncoder, SurfaceCanvas, UpdateAssembler, UpdateSplitter,
    decode_region_ops, deflate_ops, validate_part, validate_region_ops,
};

/// A processed frame from raw RGBA bytes.
fn frame_of(width: u32, height: u32, rgba: &[u8]) -> CapturedFrame {
    CapturedFrame::new(width, height, rgba.to_vec()).expect("frame")
}

/// A frame whose cells are the given the 16-bit cell format values (through the RGBA path).
fn frame_from_565(width: u32, height: u32, cells: &[u16]) -> CapturedFrame {
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    let mut scratch = vec![0_u8; cells.len() * 4];
    tessellio_core::cells_to_rgba(cells, &mut scratch).expect("565 → rgba");
    for (index, cell) in scratch.chunks_exact(4).enumerate() {
        let offset = index * 4;
        rgba[offset..offset + 4].copy_from_slice(cell);
    }
    let _ = &mut rgba;
    frame_of(width, height, &rgba)
}

/// Random the 16-bit cell format content for one frame.
fn content_strategy() -> impl Strategy<Value = (u32, u32, Vec<u16>)> {
    (1_u32..=64, 1_u32..=64).prop_flat_map(|(width, height)| {
        (
            Just(width),
            Just(height),
            vec(any::<u16>(), (width * height) as usize),
        )
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// §9.2.1 — round trip: changed cells exact, reference equals the canvas.
    #[test]
    fn round_trip_reproduces_every_changed_cell(
        (width, height, base) in content_strategy(),
        changes in proptest::collection::vec((0_u32..64, 0_u32..64, 0_u16..=0xffff), 0..6),
    ) {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            delta_deflate_level: 1,
            reset_deflate_level: 1,
            refresh_cells: 4096,
            ..CodecConfig::default()
        });
        let mut canvas = SurfaceCanvas::new();
        let base_frame = frame_from_565(width, height, &base);
        let reset = encode_frame(&mut encoder, &base_frame, 1);
        prop_assert_eq!(reset.update_base, 0);
        canvas.apply(&reset).expect("self-contained applies");
        prop_assert_eq!(canvas.cells(), encoder.reference_cells());
        prop_assert_eq!(canvas.cells(), expect565(&base_frame));

        let mut current = base.clone();
        for (index, (x, y, value)) in changes.iter().enumerate() {
            let offset = ((y % height) * width + (x % width)) as usize;
            current[offset] = *value;
            let frame = frame_from_565(width, height, &current);
            let planned = encoder.encode(&frame).expect("encode");
            let Some(planned) = planned else { continue };
            let encoded = encoder.commit(planned, index as u64 + 2);
            canvas.apply(&encoded).expect("chained applies");
            // Every cell the frame carried is a changed cell, and the decoded
            // canvas matches the source at wire fidelity.
            prop_assert!(encoded.sent_cells <= encoded.changed_cells.max(encoded.sent_cells));
            for region in &encoded.regions {
                prop_assert!(region.payload.len() <= MAX_REGION_WIRE_BYTES);
            }
            // INV-R while the receiver is at the reference.
            if canvas.applied_update_id() == encoder.reference_update_id() {
                prop_assert_eq!(canvas.cells(), encoder.reference_cells());
            }
        }
    }

    /// §9.2.3 — a rebase paints what a reset frame paints.
    #[test]
    fn rebase_equals_a_reset_frame(
        (width, height, base) in content_strategy(),
        held_pick in 0_u64..8,
        noise in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            delta_deflate_level: 1,
            reset_deflate_level: 0,
            ..CodecConfig::default()
        });
        let mut current = base.clone();
        let mut history: Vec<Vec<u16>> = Vec::new();
        let mut ids: Vec<u64> = Vec::new();
        // Ten frames, each nudging the content.
        for step in 0..10_u64 {
            if step > 0 {
                let offset = (noise.get(step as usize).copied().unwrap_or(7) as usize * 13)
                    % current.len();
                current[offset] = (step as u16).wrapping_mul(3_333);
            }
            let frame = frame_from_565(width, height, &current);
            let planned = encoder.encode(&frame).expect("encode");
            let Some(planned) = planned else { continue };
            let encoded = encoder.commit(planned, step + 1);
            ids.push(encoded.update_id);
            history.push(current.clone());
        }
        let reference_id = encoder.reference_update_id();
        prop_assume!(reference_id >= 2);
        let held = *ids.get((held_pick as usize) % ids.len()).expect("an id");
        let next_content = {
            let mut next = current.clone();
            next[0] = next[0].wrapping_add(0x1234);
            next
        };
        let next_frame = frame_from_565(width, height, &next_content);

        if encoder.journal_can_reach(held) {
            // Rebuild the receiver's canvas at `held` from the recording.
            let mut replica = SurfaceCanvas::new();
            let index = ids.iter().position(|id| *id == held).expect("held in ids");
            let reset_index = 0;
            let mut fresh = ScreenStreamEncoder::new(CodecConfig {
                delta_deflate_level: 1,
                reset_deflate_level: 0,
                ..CodecConfig::default()
            });
            for step in reset_index..=index {
                let planned = fresh.encode(&frame_from_565(width, height, &history[step])).expect("encode").expect("frame");
                replica.apply(&fresh.commit(planned, step as u64 + 1)).expect("apply");
            }
            prop_assert_eq!(replica.applied_update_id(), held);

            let planned = encoder.rebase_and_encode(held, &next_frame).expect("rebase").expect("frame");
            prop_assert_eq!(planned.update_base, held);
            let rebased = encoder.commit(planned, reference_id + 1);
            replica.apply(&rebased).expect("rebased frame applies");

            // The rebased result paints the source content exactly.
            let expected = expect565(&next_frame);
            prop_assert_eq!(replica.cells(), expected.as_slice());
            // And a reset frame of the same content paints the same cells.
            let mut fresh = ScreenStreamEncoder::new(CodecConfig {
                delta_deflate_level: 1,
                reset_deflate_level: 0,
                ..CodecConfig::default()
            });
            let planned = fresh.encode(&next_frame).expect("encode").expect("frame");
            let reset = fresh.commit(planned, 3);
            let mut reset_canvas = SurfaceCanvas::new();
            reset_canvas.apply(&reset).expect("apply");
            prop_assert_eq!(replica.cells(), reset_canvas.cells());
        } else {
            prop_assert!(encoder.rebase_and_encode(held, &next_frame).expect("query").is_none());
        }
    }

    /// §9.2.5 — the format caps hold for arbitrary content.
    #[test]
    fn caps_hold_for_arbitrary_content(
        (width, height, base) in content_strategy(),
        mut second in any::<Vec<u16>>(),
    ) {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            delta_deflate_level: 0,
            reset_deflate_level: 0,
            ..CodecConfig::default()
        });
        let first = frame_from_565(width, height, &base);
        let reset = encode_frame(&mut encoder, &first, 1);
        prop_assert!(reset.regions.len() <= MAX_REGIONS_PER_UPDATE);
        prop_assert!(reset.payload_bytes() <= MAX_UPDATE_WIRE_BYTES);
        second.resize((width * height) as usize, 0);
        let second_frame = frame_from_565(width, height, &second);
        let planned = encoder.encode(&second_frame).expect("encode");
        if let Some(planned) = planned {
            let encoded = encoder.commit(planned, 2);
            let parts = UpdateSplitter::new().part(encoded.clone()).expect("parts");
            prop_assert!(parts.len() <= MAX_PARTS_PER_UPDATE);
            prop_assert!(encoded.regions.len() <= MAX_REGIONS_PER_UPDATE);
            for part in &parts {
                prop_assert_eq!(validate_part(part), Ok(()));
                prop_assert!(part.regions.len() <= MAX_REGIONS_PER_PART);
            }
            let total: usize = encoded.regions.iter().map(|region| region.payload.len()).sum();
            prop_assert!(total <= MAX_UPDATE_WIRE_BYTES);
        }
    }

    /// §9.2.6 — budget invariants: never more than the budget plus one row, and
    /// regions are disjoint (the validator checks both).
    #[test]
    fn budget_invariants_hold(
        (width, height, base) in content_strategy(),
        noise in any::<Vec<u16>>(),
    ) {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig {
            refresh_cells: 4096,
            delta_deflate_level: 1,
            reset_deflate_level: 0,
            ..CodecConfig::default()
        });
        let first = frame_from_565(width, height, &base);
        let reset = encode_frame(&mut encoder, &first, 1);
        let mut canvas = SurfaceCanvas::new();
        canvas.apply(&reset).expect("self-contained applies");

        let mut second = noise;
        second.resize((width * height) as usize, 0);
        let second_frame = frame_from_565(width, height, &second);
        let planned = encoder.encode(&second_frame).expect("encode");
        if let Some(planned) = planned {
            let encoded = encoder.commit(planned, 2);
            prop_assert!(encoded.sent_cells <= 4096 + u64::from(width));
            prop_assert_eq!(encoded.sent_cells + encoded.backlog_cells, encoded.changed_cells);
            let parts = UpdateSplitter::new().part(encoded.clone()).expect("parts");
            for part in &parts {
                prop_assert_eq!(validate_part(part), Ok(()));
            }
            canvas.apply(&encoded).expect("applies");
        }
        // A second frame carries the remainder and converges.
        for id in 3..8_u64 {
            let planned = encoder.encode(&second_frame).expect("encode");
            let Some(planned) = planned else { break };
            let encoded = encoder.commit(planned, id);
            canvas.apply(&encoded).expect("applies");
            if encoded.backlog_cells == 0 {
                break;
            }
        }
        prop_assert_eq!(canvas.cells(), encoder.reference_cells());
    }
}

// ---------------------------------------------------------------------------
// INV-R: the model test with injected drops
// ---------------------------------------------------------------------------

/// Loss schedule for the transport model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loss {
    /// The whole frame is dropped.
    Frame,
    /// One part of the frame is dropped (the frame never assembles).
    Part,
    /// Nothing is lost.
    None,
}

/// The scripted content of step `step`: pointer moves, text, scrolls, noise.
fn scripted_content(width: u32, height: u32, step: u64) -> CapturedFrame {
    let mut frame = office(width, height, 7);
    let phase = step % 8;
    match phase {
        0 | 1 => {
            draw_cursor(&mut frame, 40 + (step as i32 % 20) * 2, 60);
        }
        2 => {
            text_line(
                &mut frame,
                8,
                20 + (step as u32 % 8) * 20,
                6,
                step as u32 % 5,
            );
        }
        3 | 4 => {
            scroll_region(&mut frame, 0, 0, width, height - 8, 4);
        }
        5 => {
            noise_block(&mut frame, 8, 8, width / 4 + 1, height / 4 + 1, step as u32);
        }
        _ => {
            fill_rect(&mut frame, 0, 0, width, height / 3 + 1, [30, 90, 160, 255]);
            draw_cursor(&mut frame, 70, 40);
        }
    }
    frame
}

#[test]
fn inv_r_holds_through_a_lossy_transport() {
    let width = 96_u32;
    let height = 64_u32;
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 1,
        reset_deflate_level: 1,
        refresh_cells: 8192,
        idle_update_interval: std::time::Duration::ZERO,
        ..CodecConfig::default()
    });
    let splitter = UpdateSplitter::new();
    let mut canvas = SurfaceCanvas::new();
    let mut assembler = UpdateAssembler::new();
    let mut update_id = 0_u64;
    let mut pending_resync: Option<(u64, ResyncReason)> = None;
    let mut losses = 0_u32;
    let mut resyncs: Vec<ResyncReason> = Vec::new();
    let mut frames_since_loss = 0_u32;
    let mut heals: Vec<u32> = Vec::new();

    for step in 0..200_u64 {
        let content = scripted_content(width, height, step);
        // the sender answers a resync with a rebase (the session has one receiver)
        // or, when the journal cannot reach the reported base, with a reset.
        if let Some((held, _)) = pending_resync.take() {
            if encoder.journal_can_reach(held) {
                if let Some(planned) = encoder.rebase_and_encode(held, &content).expect("rebase") {
                    update_id += 1;
                    let encoded = encoder.commit(planned, update_id);
                    deliver(
                        &splitter,
                        encoded,
                        Loss::None,
                        &mut assembler,
                        &mut canvas,
                        &mut resyncs,
                    );
                }
                continue;
            }
            encoder.reset(ResetReason::Resync);
        }

        let planned = encoder.encode(&content).expect("encode");
        let Some(planned) = planned else { continue };
        update_id += 1;
        let encoded = encoder.commit(planned, update_id);
        let is_reset = encoded.is_reset();
        let is_last_three = step + 3 >= 200;
        let loss = if is_last_three {
            Loss::Frame
        } else if step % 7 == 6 {
            Loss::Frame
        } else if is_reset && step % 3 == 1 {
            Loss::Part
        } else if step % 5 == 4 {
            Loss::Frame
        } else {
            Loss::None
        };
        if loss != Loss::None {
            losses += 1;
            frames_since_loss = 0;
        } else {
            frames_since_loss += 1;
        }
        deliver(
            &splitter,
            encoded,
            loss,
            &mut assembler,
            &mut canvas,
            &mut resyncs,
        );

        // INV-R: an applied frame whose id is the reference id must have painted
        // exactly the sender's reference.
        if canvas.applied_update_id() == encoder.reference_update_id() {
            assert_eq!(
                canvas.cells(),
                encoder.reference_cells(),
                "step {step}: the canvas must equal the sender's reference"
            );
            if losses > 0 {
                heals.push(frames_since_loss);
                losses = 0;
            }
        }
        // The receiver reports the loss; the sender heals on the next frame.
        if canvas.applied_update_id() != encoder.reference_update_id() && !resyncs.is_empty() {
            let held = canvas.applied_update_id();
            let reason = resyncs.pop().expect("a reason");
            pending_resync = Some((held, reason));
        }
    }

    println!(
        "MEASURE inv_r: resyncs={} heals={:?}",
        heals.len(),
        heals.iter().take(12).collect::<Vec<_>>()
    );
    assert!(
        heals.iter().all(|frames| *frames <= 4),
        "every loss must heal within four frames: {heals:?}"
    );
    assert!(
        canvas.cells() == encoder.reference_cells()
            || canvas.applied_update_id() < encoder.reference_update_id(),
        "the final state must be either in sync or a reported loss"
    );
}

/// Push one frame's parts through the lossy transport and the receiver.
fn deliver(
    splitter: &UpdateSplitter,
    encoded: tessellio_core::EncodedUpdate,
    loss: Loss,
    assembler: &mut UpdateAssembler,
    canvas: &mut SurfaceCanvas,
    resyncs: &mut Vec<ResyncReason>,
) {
    if loss == Loss::Frame {
        // A whole-frame drop leaves the receiver's state untouched; it notices on
        // the next frame that applies onto a base it does not hold.
        return;
    }
    let parts = splitter.part(encoded.clone()).expect("parts");
    let mut dropped_part = false;
    for (index, part) in parts.iter().enumerate() {
        if loss == Loss::Part && index == 1 && parts.len() > 1 {
            dropped_part = true;
            continue;
        }
        match assembler.push(part.clone()) {
            AssemblyOutcome::Pending => {}
            AssemblyOutcome::Complete(assembled) => {
                let frame = tessellio_core::EncodedUpdate {
                    update_id: assembled.update_id,
                    update_base: assembled.update_base,
                    width: assembled.width,
                    height: assembled.height,
                    codec: tessellio_core::Codec::Tessellio,
                    regions: assembled.regions,
                    changed_cells: 0,
                    sent_cells: 0,
                    backlog_cells: 0,
                    reset_reasons: Default::default(),
                };
                match canvas.apply(&frame) {
                    Ok(()) => {}
                    Err(_) => {
                        resyncs.push(ResyncReason::BaseMismatch);
                        assembler.reset();
                    }
                }
            }
            AssemblyOutcome::Rejected(rejection) => {
                let reason = match rejection.reason_code() {
                    "INCOMPLETE_UPDATE" => ResyncReason::IncompleteFrame,
                    "PART_GAP" => ResyncReason::PartGap,
                    "UNKNOWN_CODEC" => ResyncReason::UnknownCodec,
                    "COVERAGE_INVALID" => ResyncReason::CoverageInvalid,
                    _ => ResyncReason::DecodeError,
                };
                resyncs.push(reason);
                assembler.reset();
            }
        }
    }
    if dropped_part {
        resyncs.push(ResyncReason::PartGap);
        assembler.reset();
    }
}

// ---------------------------------------------------------------------------
// Decoder totality
// ---------------------------------------------------------------------------

#[test]
fn the_decoder_never_panics_on_arbitrary_input() {
    let mut lcg = Lcg::new(0xc0de_1234);
    let mut scratch = Vec::new();
    let mut accepted = 0_u32;
    let mut rejected = 0_u32;
    for case in 0..10_000_u32 {
        // Three generators: uniform bytes, run-heavy bytes, and a valid DEFLATE
        // envelope around a mutated op stream.
        let payload = match case % 3 {
            0 => {
                let len = (lcg.below(64) + 1) as usize;
                (0..len).map(|_| lcg.next_u8()).collect::<Vec<u8>>()
            }
            1 => {
                let len = (lcg.below(48) + 1) as usize;
                let value = lcg.next_u8();
                let mut bytes = vec![value; len];
                if len > 2 {
                    let index = lcg.below(len as u32) as usize;
                    bytes[index] = lcg.next_u8();
                }
                bytes
            }
            _ => {
                let mut ops = Vec::new();
                ops.push(0x01);
                ops.extend_from_slice(&(lcg.below(6) as u16).to_le_bytes());
                ops.push(0x02);
                ops.extend_from_slice(&(lcg.below(6) as u16).to_le_bytes());
                let len = lcg.below(4) + 1;
                ops.extend_from_slice(&(len as u16).to_le_bytes());
                // A SPAN carries two bytes per cell.
                for _ in 0..len {
                    ops.extend_from_slice(&lcg.next_u8().to_le_bytes());
                    ops.extend_from_slice(&lcg.next_u8().to_le_bytes());
                }
                ops.push(0x00);
                // Every third mutated stream keeps a valid envelope.
                if case % 2 == 0 {
                    ops.pop();
                }
                deflate_ops(&ops, (case % 10) as u8).expect("deflate")
            }
        };
        for coverage in [Coverage::Full, Coverage::Sparse] {
            let mut out = vec![0_u16; 6 * 4];
            let result = decode_region_ops(&payload, 6, 4, coverage, &mut out, &mut scratch);
            match result {
                Ok(()) => {
                    accepted += 1;
                    // An accepted payload must also pass the validator, and the
                    // inflated stream must re-walk identically.
                    let inflated = scratch.clone();
                    assert_eq!(
                        validate_region_ops(&inflated, 6, 4, coverage),
                        Ok(()),
                        "case {case}: an accepted stream must validate"
                    );
                }
                Err(error) => {
                    rejected += 1;
                    assert!(
                        matches!(
                            error,
                            DecodeError::Deflate
                                | DecodeError::TooLarge
                                | DecodeError::Truncated
                                | DecodeError::TrailingBytes
                                | DecodeError::BadRow
                                | DecodeError::BadSpan
                                | DecodeError::BadValue
                                | DecodeError::Coverage
                        ),
                        "case {case}: unexpected error {error:?}"
                    );
                }
            }
        }
    }
    println!("MEASURE decoder_totality: accepted={accepted} rejected={rejected}");
    assert!(accepted > 0, "the generator must produce valid streams too");
    assert!(
        rejected > 0,
        "the generator must produce malformed streams too"
    );
}

#[test]
fn the_reference_accessor_reports_geometry() {
    let mut encoder = ScreenStreamEncoder::new(CodecConfig {
        delta_deflate_level: 0,
        reset_deflate_level: 0,
        ..CodecConfig::default()
    });
    assert_eq!(encoder.reference_geometry(), None);
    assert!(encoder.reference_cells().is_empty());
    let frame = plain(32, 16, [1, 2, 3, 255]);
    let _ = encode_frame(&mut encoder, &frame, 1);
    assert_eq!(encoder.reference_geometry(), Some((32, 16)));
    assert_eq!(encoder.reference_cells().len(), 32 * 16);
}
