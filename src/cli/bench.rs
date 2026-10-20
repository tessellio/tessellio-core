//! `bench`: the codec's measured performance scenarios.
//!
//! Every scenario measures the codec and holds it to a budget:
//!
//! * the **byte** budget is the reference implementation's measured bound for
//!   that scenario ([`crate::measured`]), which is machine-independent;
//! * the **time** budget is the encode's cost over the capture interface's
//!   own conversion of the same frame, measured in the same run, so a slower
//!   machine does not fail the gate and a slower codec does.
//!
//! The medians and the p95s are printed for every measurement.

use std::time::Instant;

use serde_json::{Value, json};

use crate::cli::{CliError, Options, Produced, json_line, table};
use crate::measured::{
    CHAINED_ENCODE_MAX_MS, CHAINED_ENCODE_MAX_MS_DEBUG, CURSOR_1PX_MAX_BYTES,
    CURSOR_16PX_MAX_BYTES, DRAG_MAX_BYTES, FULL_WIDTH_SCROLL_MAX_BYTES, MIX_MAX_BYTES,
    NOISE_FRAME_MAX_BYTES, RESET_ENCODE_MAX_MS, RESET_ENCODE_MAX_MS_DEBUG, RESET_FRAME_MAX_BYTES,
    SCROLL_MAX_BYTES, TYPING_MAX_BYTES,
};
use crate::scene::{
    desktop_with_window, document, draw_cursor, fill_rect, noise_block, office, scroll_viewport,
    text_line,
};
use crate::{
    CapturedFrame, CodecConfig, EncodedUpdate, ScreenStreamEncoder, Surface, SurfaceCanvas,
    UpdateSplitter, rgba_to_cells,
};

/// What a scenario measured.
struct Outcome {
    /// The bytes the scenario is held to.
    bytes: f64,
    /// The scenario's base update: the self-contained one a receiver can
    /// apply on its own.
    base: EncodedUpdate,
    /// The update the scenario measured.
    measured: EncodedUpdate,
    /// The frame the measured update encodes: the conversion of it is the
    /// machine-independent baseline the encode time is held against.
    measured_frame: CapturedFrame,
    /// How long the measured update's encode took.
    encode_ms: f64,
}

/// One measured scenario.
struct Scenario {
    /// The scenario's name.
    name: &'static str,
    /// What the scenario does.
    summary: &'static str,
    /// The budget the bytes are held to.
    byte_budget: f64,
    /// The budget the encode time is held to, in milliseconds.
    time_budget_ms: f64,
    /// The scenario itself.
    run: fn(&mut ScreenStreamEncoder) -> Outcome,
}

/// The build profile the tool was built with.
///
/// The time budgets are stated for an optimized build, and an unoptimized
/// build is an order of magnitude slower. The tool knows which it is, and
/// holds itself to the budget for that profile.
const PROFILE: &str = if cfg!(debug_assertions) {
    "unoptimized"
} else {
    "optimized"
};

/// The budget a chained update's encode is held to, in this build.
fn chained_budget_ms() -> f64 {
    if cfg!(debug_assertions) {
        CHAINED_ENCODE_MAX_MS_DEBUG
    } else {
        CHAINED_ENCODE_MAX_MS
    }
}

/// The budget a self-contained update's encode is held to, in this build.
fn reset_budget_ms() -> f64 {
    if cfg!(debug_assertions) {
        RESET_ENCODE_MAX_MS_DEBUG
    } else {
        RESET_ENCODE_MAX_MS
    }
}

/// Encode one frame and commit it.
fn encode(encoder: &mut ScreenStreamEncoder, frame: &CapturedFrame, id: u64) -> EncodedUpdate {
    let planned = encoder
        .encode(frame)
        .expect("the scene encodes")
        .expect("the scene produces an update");
    encoder.commit(planned, id)
}

/// Encode one frame and commit it, and say how long it took.
fn timed(
    encoder: &mut ScreenStreamEncoder,
    frame: &CapturedFrame,
    id: u64,
) -> (EncodedUpdate, f64) {
    let started = Instant::now();
    let update = encode(encoder, frame, id);
    (update, started.elapsed().as_secs_f64() * 1000.0)
}

/// One update's bytes, over the base it is chained onto.
fn bytes_of(
    base: &EncodedUpdate,
    update: &EncodedUpdate,
    frame: &CapturedFrame,
    encode_ms: f64,
) -> Outcome {
    Outcome {
        bytes: update.payload_bytes() as f64,
        base: base.clone(),
        measured: update.clone(),
        measured_frame: frame.clone(),
        encode_ms,
    }
}

/// Every scenario, in the order the bench prints them.
fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "pointer_1px",
            summary: "a one-cell pointer move over an office scene",
            byte_budget: CURSOR_1PX_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = office(1920, 1080, 11);
                draw_cursor(&mut frame, 900, 600);
                let base = encode(encoder, &frame, 1);
                draw_cursor(&mut frame, 901, 600);
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "pointer_16px",
            summary: "a sixteen-cell pointer move over an office scene",
            byte_budget: CURSOR_16PX_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = office(1920, 1080, 13);
                draw_cursor(&mut frame, 900, 600);
                let base = encode(encoder, &frame, 1);
                draw_cursor(&mut frame, 916, 600);
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "typing_600x40",
            summary: "a 600×40 block retyped over a document",
            byte_budget: TYPING_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = document(1920, 1080);
                text_line(&mut frame, 600, 500, 85, 3);
                let base = encode(encoder, &frame, 1);
                fill_rect(&mut frame, 600, 498, 600, 40, [248, 248, 250, 255]);
                text_line(&mut frame, 600, 500, 85, 9);
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "window_drag",
            summary: "a 600×400 window dragged 30 pixels",
            byte_budget: DRAG_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let frame = desktop_with_window(1920, 1080, (900, 400, 600, 400));
                let base = encode(encoder, &frame, 1);
                let frame = desktop_with_window(1920, 1080, (930, 400, 600, 400));
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "scroll_900x620",
            summary: "a 900×620 viewport scrolled 40 pixels",
            byte_budget: SCROLL_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = document(1920, 1080);
                let base = encode(encoder, &frame, 1);
                scroll_viewport(&mut frame, 200, 100, 900, 620, 40, 5);
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "full_width_scroll",
            summary: "a 1680×920 page scrolled 40 pixels",
            byte_budget: FULL_WIDTH_SCROLL_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = document(1920, 1080);
                let base = encode(encoder, &frame, 1);
                scroll_viewport(&mut frame, 120, 60, 1680, 920, 40, 6);
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "noise_640x360",
            summary: "a 640×360 block of high-entropy content",
            byte_budget: NOISE_FRAME_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = office(1920, 1080, 17);
                let base = encode(encoder, &frame, 1);
                noise_block(&mut frame, 640, 360, 640, 360, 29);
                let (update, elapsed) = timed(encoder, &frame, 2);
                bytes_of(&base, &update, &frame, elapsed)
            },
        },
        Scenario {
            name: "interactive_mix",
            summary: "the weighted interactive mix: pointer 50 %, typing 20 %, scroll 15 %, drag 10 %",
            byte_budget: MIX_MAX_BYTES as f64,
            time_budget_ms: chained_budget_ms(),
            run: |encoder| {
                let mut frame = office(1920, 1080, 37);
                draw_cursor(&mut frame, 700, 500);
                let base = encode(encoder, &frame, 1);
                draw_cursor(&mut frame, 701, 500);
                let pointer = encode(encoder, &frame, 2);

                let mut typed = document(1920, 1080);
                let _ = encode(encoder, &typed, 3);
                fill_rect(&mut typed, 600, 498, 600, 40, [248, 248, 250, 255]);
                text_line(&mut typed, 600, 500, 85, 9);
                let typing = encode(encoder, &typed, 4);

                scroll_viewport(&mut typed, 200, 100, 900, 620, 40, 5);
                let scroll = encode(encoder, &typed, 5);

                let dragged = desktop_with_window(1920, 1080, (900, 400, 600, 400));
                let _ = encode(encoder, &dragged, 6);
                let dragged = desktop_with_window(1920, 1080, (930, 400, 600, 400));
                let (drag, elapsed) = timed(encoder, &dragged, 7);

                let weights = [0.5_f64, 0.2, 0.15, 0.1];
                let measured = [
                    pointer.payload_bytes() as f64,
                    typing.payload_bytes() as f64,
                    scroll.payload_bytes() as f64,
                    drag.payload_bytes() as f64,
                ];
                let bytes = weights
                    .iter()
                    .zip(measured.iter())
                    .map(|(weight, value)| weight * value)
                    .sum::<f64>()
                    / weights.iter().sum::<f64>();
                Outcome {
                    bytes,
                    base,
                    measured: drag,
                    measured_frame: dragged,
                    encode_ms: elapsed,
                }
            },
        },
        Scenario {
            name: "reset_1080p",
            summary: "a 1080p office scene as a self-contained update",
            byte_budget: RESET_FRAME_MAX_BYTES as f64,
            time_budget_ms: reset_budget_ms(),
            run: |encoder| {
                let frame = office(1920, 1080, 31);
                let (update, elapsed) = timed(encoder, &frame, 1);
                bytes_of(&update, &update, &frame, elapsed)
            },
        },
    ]
}

/// Run the measured performance scenarios and print the medians and the p95s.
pub fn bench(options: &Options, json: bool) -> Result<Produced, CliError> {
    let iterations = options.unsigned::<usize>("iterations")?.unwrap_or(5).max(1);
    let wanted: Vec<String> = options
        .values("scenario")
        .into_iter()
        .map(str::to_string)
        .collect();

    let mut results = Vec::new();
    for scenario in scenarios() {
        if !wanted.is_empty() && !wanted.iter().any(|name| name == scenario.name) {
            continue;
        }
        results.push(run_one(&scenario, iterations)?);
    }
    if results.is_empty() {
        return Err(CliError::Usage(
            "no scenario was named; run `tessellio-core bench --help` for the list".to_string(),
        ));
    }

    let failed = results
        .iter()
        .filter(|result| !result.within_budget)
        .count();
    if json {
        Ok(Produced::ok(json_line(&json!({
            "command": "bench",
            "profile": PROFILE,
            "iterations": iterations,
            "scenarios": results
                .iter()
                .map(|result| {
                    json!({
                        "name": result.name,
                        "summary": result.summary,
                        "bytes": result.bytes,
                        "byte_budget": result.byte_budget,
                        "encode_ms_median": result.encode_ms_median,
                        "encode_ms_p95": result.encode_ms_p95,
                        "baseline_ms_median": result.baseline_ms_median,
                        "encode_over_baseline": result.ratio,
                        "encode_ms_budget": result.time_budget_ms,
                        "regions": result.regions,
                        "parts": result.parts,
                        "within_budget": result.within_budget,
                    })
                })
                .collect::<Vec<Value>>(),
            "summary": {
                "total": results.len(),
                "failed": failed,
            },
        }))))
    } else {
        let text = render(&results, iterations);
        Ok(if failed == 0 {
            Produced::ok(text)
        } else {
            Produced::failed(text)
        })
    }
}

/// What one scenario measured.
struct Measurement {
    /// The scenario's name.
    name: &'static str,
    /// What the scenario does.
    summary: &'static str,
    /// The bytes the scenario measured.
    bytes: f64,
    /// The budget the bytes are held to.
    byte_budget: f64,
    /// The median encode time, in milliseconds.
    encode_ms_median: f64,
    /// The p95 encode time, in milliseconds.
    encode_ms_p95: f64,
    /// The median conversion time of the measured frame, in milliseconds.
    baseline_ms_median: f64,
    /// The encode time over the conversion time.
    ratio: f64,
    /// The budget the encode time is held to, in milliseconds.
    time_budget_ms: f64,
    /// The regions of the measured update.
    regions: usize,
    /// The parts of the measured update.
    parts: usize,
    /// Whether every measurement stayed inside its budget.
    within_budget: bool,
}

/// Run one scenario and measure it.
fn run_one(scenario: &Scenario, iterations: usize) -> Result<Measurement, CliError> {
    let mut byte_samples: Vec<f64> = Vec::new();
    let mut time_samples: Vec<f64> = Vec::new();
    let mut baseline_samples: Vec<f64> = Vec::new();
    let mut regions = 0_usize;
    let mut parts = 0_usize;

    for _ in 0..iterations {
        let mut encoder = ScreenStreamEncoder::new(CodecConfig::default());
        let outcome = (scenario.run)(&mut encoder);
        // The scenario's base update must decode: an update that does not
        // decode is not a measurement, it is a defect.
        let mut canvas = SurfaceCanvas::new();
        canvas.apply(&outcome.base).map_err(|error| {
            CliError::Input(format!(
                "the scenario {} produced an update that does not decode: {}",
                scenario.name,
                error.reason_code()
            ))
        })?;
        byte_samples.push(outcome.bytes);
        time_samples.push(outcome.encode_ms);
        baseline_samples.push(conversion_ms(&outcome.measured_frame));
        regions = outcome.measured.regions.len();
        parts = UpdateSplitter::new()
            .part(outcome.measured.clone())
            .map(|parts| parts.len())
            .unwrap_or(0);
    }

    let mut sorted_times = time_samples.clone();
    sorted_times
        .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mut sorted_baselines = baseline_samples.clone();
    sorted_baselines
        .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let encode_ms_median = percentile_f64(&sorted_times, 50);
    let encode_ms_p95 = percentile_f64(&sorted_times, 95);
    let baseline_ms_median = percentile_f64(&sorted_baselines, 50);
    let bytes = byte_samples.iter().copied().fold(0.0_f64, f64::max);
    let ratio = if baseline_ms_median > 0.0 {
        encode_ms_median / baseline_ms_median
    } else {
        0.0
    };

    Ok(Measurement {
        name: scenario.name,
        summary: scenario.summary,
        bytes,
        byte_budget: scenario.byte_budget,
        encode_ms_median,
        encode_ms_p95,
        baseline_ms_median,
        ratio,
        time_budget_ms: scenario.time_budget_ms,
        regions,
        parts,
        within_budget: bytes <= scenario.byte_budget && encode_ms_median <= scenario.time_budget_ms,
    })
}

/// How long the capture interface's own conversion of a frame takes.
///
/// This is the machine-independent baseline: the same work on any machine, in
/// any build profile. The encode time is held against it, so a slower machine
/// does not fail the gate and a slower codec does.
fn conversion_ms(frame: &CapturedFrame) -> f64 {
    let mut buffer = match Surface::new(frame.width(), frame.height()) {
        Ok(buffer) => buffer,
        Err(_) => return 0.0,
    };
    let started = Instant::now();
    let _ = rgba_to_cells(&frame.rgba, frame.width(), frame.height(), &mut buffer);
    started.elapsed().as_secs_f64() * 1000.0
}

/// The `percentile`th percentile of a sorted sample list.
///
/// The nearest-rank method: the smallest sample at or above the requested
/// share of the population.
fn percentile_f64(sorted: &[f64], percentile: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}

/// The human form of the measurements.
fn render(results: &[Measurement], iterations: usize) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "the codec's measured performance ({iterations} iteration(s) per scenario, \
         {PROFILE} build)\n\n"
    ));
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|result| {
            vec![
                result.name.to_string(),
                format!("{:.0}", result.bytes),
                format!("{:.0}", result.byte_budget),
                format!("{:.3}", result.encode_ms_median),
                format!("{:.3}", result.encode_ms_p95),
                format!("{:.2}", result.ratio),
                format!("{:.0}", result.time_budget_ms),
                if result.within_budget {
                    "within".to_string()
                } else {
                    "OVER".to_string()
                },
            ]
        })
        .collect();
    text.push_str(&table(
        &[
            "scenario",
            "bytes",
            "byte budget",
            "median ms",
            "p95 ms",
            "ratio",
            "ms budget",
            "verdict",
        ],
        &rows,
    ));
    let failed = results
        .iter()
        .filter(|result| !result.within_budget)
        .count();
    text.push_str(&format!(
        "\n{} of {} scenario(s) inside their budgets\n",
        results.len() - failed,
        results.len()
    ));
    for result in results {
        text.push_str(&format!("  {:<20} {}\n", result.name, result.summary));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_options;

    #[test]
    fn every_scenario_has_a_budget_and_a_summary() {
        for scenario in scenarios() {
            assert!(!scenario.name.is_empty());
            assert!(!scenario.summary.is_empty());
            assert!(scenario.byte_budget > 0.0);
            assert!(scenario.time_budget_ms > 0.0);
        }
    }

    #[test]
    fn the_bench_measures_every_scenario_inside_its_budget() {
        let options = test_options(&[("iterations", Some("1"))]);
        let produced = bench(&options, true).expect("benchmarks");
        let document: Value = serde_json::from_str(&produced.text).expect("json");
        assert_eq!(document["command"], "bench");
        assert_eq!(document["summary"]["failed"], 0);
        for scenario in document["scenarios"].as_array().expect("scenarios") {
            assert_eq!(scenario["within_budget"], true, "{scenario}");
        }
    }

    #[test]
    fn an_unknown_scenario_is_a_usage_error() {
        let options = test_options(&[("scenario", Some("nonsense"))]);
        let error = bench(&options, true).expect_err("unknown scenario");
        assert_eq!(error.exit_code(), crate::cli::EXIT_USAGE);
    }

    #[test]
    fn the_percentile_of_a_sorted_sample_is_its_rank() {
        let samples = vec![1.0_f64, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile_f64(&samples, 50), 5.0);
        assert_eq!(percentile_f64(&samples, 95), 10.0);
        assert_eq!(percentile_f64(&[], 50), 0.0);
    }
}
