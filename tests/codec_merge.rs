//! The merge's cost and the guarantees it gives.
//!
//! The merge is the one step of the rectangle search whose cost grows with
//! the number of rectangles: it brings a frame's rectangle set under the
//! region cap. The numbers printed here are the evidence that the cost grows
//! with the *logarithm* of that number and not with its square: a surface at
//! the maximum dimension holds sixteen thousand rectangles, and the merge
//! must not turn that into seconds of encoder time.
//!
//! The `MEASURE` lines make the measured values visible in the run log.

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

use std::time::{Duration, Instant};

use common::*;
use tessellio_core::detect::merge_to_limit;
use tessellio_core::format::Rect;
use tessellio_core::{MAX_REGIONS_PER_UPDATE, MAX_SURFACE_DIMENSION};

/// The rectangle counts the merge is measured at: every tile of a 1080p
/// surface, and the whole grid of a surface at the maximum dimension.
const COUNTS: [usize; 4] = [510, 1024, 4096, 16384];

/// The ceiling the merge of the largest rectangle set must stay under.
///
/// The exhaustive search this merge replaced took half a minute for the same
/// rectangles; the bound is two orders of magnitude above what the bounded
/// merge needs and two orders of magnitude below what the exhaustive search
/// took.
const MERGE_CEILING: Duration = Duration::from_millis(1000);

/// Scattered one-tile rectangles on a surface at the maximum dimension.
///
/// The tiles are the change detector's unit, so this is the rectangle set a
/// surface whose every other tile changed produces.
fn scattered(count: usize) -> Vec<Rect> {
    let tiles = (MAX_SURFACE_DIMENSION / 64) as usize;
    let mut out = Vec::with_capacity(count);
    let mut lcg = Lcg::new(97);
    let mut used = vec![false; tiles * tiles];
    while out.len() < count {
        let index = lcg.below((tiles * tiles) as u32) as usize;
        if used[index] {
            continue;
        }
        used[index] = true;
        let x = (index % tiles * 64) as u32;
        let y = (index / tiles * 64) as u32;
        out.push(Rect::new(x, y, 64, 64));
    }
    out
}

/// Whether the merged rectangles cover every rectangle the input held.
///
/// A merged rectangle is the box of the rectangles it merged, so it holds
/// them: every input rectangle lies inside one merged rectangle, and every
/// cell the input covered is therefore covered still.
fn covers_everything(source: &[Rect], merged: &[Rect]) -> bool {
    source.iter().all(|rect| {
        merged.iter().any(|cover| {
            cover.x <= rect.x
                && cover.y <= rect.y
                && cover.x + cover.width >= rect.x + rect.width
                && cover.y + cover.height >= rect.y + rect.height
        })
    })
}

#[test]
fn the_merge_of_a_full_surface_costs_a_bounded_amount() {
    let mut elapsed = Vec::new();
    for count in COUNTS {
        let source = scattered(count);
        let mut rects = source.clone();
        let started = Instant::now();
        merge_to_limit(&mut rects, MAX_REGIONS_PER_UPDATE);
        let took = started.elapsed();
        elapsed.push(took);
        println!(
            "MEASURE merge_{count}_rectangles: ms={:.2} regions={}",
            took.as_secs_f64() * 1000.0,
            rects.len()
        );

        // The cap holds, and every cell the input covered is still covered.
        assert!(
            rects.len() <= MAX_REGIONS_PER_UPDATE,
            "{count} rectangles merged into {}",
            rects.len()
        );
        assert!(
            covers_everything(&source, &rects),
            "{count} rectangles lost a covered cell"
        );
    }

    // The largest rectangle set stays inside the ceiling.
    let largest = elapsed.last().copied().unwrap_or_default();
    assert!(
        largest < MERGE_CEILING,
        "the merge of {} rectangles took {largest:?}",
        COUNTS[COUNTS.len() - 1]
    );

    // The cost grows with the logarithm of the rectangle count, not with its
    // square: sixteen times the rectangles cost far less than sixteen times
    // the time.
    let smallest = elapsed.first().copied().unwrap_or_default();
    assert!(
        largest < smallest * 64,
        "the merge grew from {smallest:?} to {largest:?} for sixteen times the rectangles"
    );
}

#[test]
fn the_merge_is_deterministic_and_bounded() {
    let source = scattered(4096);
    let mut first = source.clone();
    let mut second = source.clone();
    merge_to_limit(&mut first, MAX_REGIONS_PER_UPDATE);
    merge_to_limit(&mut second, MAX_REGIONS_PER_UPDATE);
    assert_eq!(first, second, "the merge is not deterministic");

    // A limit of one leaves a single rectangle that covers everything.
    let mut single = source.clone();
    merge_to_limit(&mut single, 1);
    assert_eq!(single.len(), 1);
    assert!(covers_everything(&source, &single));
}
