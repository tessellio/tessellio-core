//! `encode`: the codec's own verb.

use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

use crate::cli::wire::{Envelope, encode_envelope, sha256_hex};
use crate::cli::{CliError, Options, Produced, hex, json_line, table};
use crate::format::Region;
#[cfg(feature = "scenes")]
use crate::scene::{draw_cursor, office};
use crate::telemetry::{CodecEvent, EncodePhase, SharedTelemetry, Telemetry};
use crate::{CapturedFrame, CodecConfig, EncodedUpdate, ScreenStreamEncoder, UpdateSplitter};

/// What the encoder measured, phase by phase.
#[derive(Debug, Default)]
struct Collector {
    phases: Mutex<Vec<(EncodePhase, u64)>>,
}

impl Collector {
    fn new() -> Self {
        Self::default()
    }

    fn timings(&self) -> Vec<(EncodePhase, Vec<u64>)> {
        let guard = self.phases.lock().expect("the collector's lock");
        let mut order: Vec<EncodePhase> = Vec::new();
        for (phase, _) in guard.iter() {
            if !order.contains(phase) {
                order.push(*phase);
            }
        }
        order
            .into_iter()
            .map(|phase| {
                let samples: Vec<u64> = guard
                    .iter()
                    .filter(|(candidate, _)| *candidate == phase)
                    .map(|(_, micros)| *micros)
                    .collect();
                (phase, samples)
            })
            .collect()
    }
}

impl Telemetry for Collector {
    fn record(&self, event: CodecEvent) {
        match event {
            CodecEvent::PhaseElapsed { phase, elapsed } => {
                let mut guard = self.phases.lock().expect("the collector's lock");
                guard.push((phase, elapsed.as_micros() as u64));
            }
            CodecEvent::SelfCheckFailed { .. } => {}
        }
    }
}

/// Encode a surface into the codec's update and print its bytes.
pub fn encode(options: &Options, json: bool) -> Result<Produced, CliError> {
    let frame = read_surface(options)?;
    let config = read_config(options)?;
    let update_id = options.unsigned::<u64>("update-id")?.unwrap_or(1);

    let collector = std::sync::Arc::new(Collector::new());
    let telemetry: SharedTelemetry = collector.clone();
    let mut encoder = ScreenStreamEncoder::with_telemetry(config, telemetry);

    let started = Instant::now();
    let update = if options.flag("chain").is_some() {
        // The first update establishes the surface; the second is chained onto
        // it, and carries the one-cell pointer move that changed it.
        let base = plan(&mut encoder, &frame)?;
        encoder.commit(base, update_id);
        let mut moved = frame.clone();
        let (x, y) = (frame.width() as i32 / 2, frame.height() as i32 / 2);
        #[cfg(feature = "scenes")]
        draw_cursor(&mut moved, x, y);
        #[cfg(feature = "scenes")]
        draw_cursor(&mut moved, x + 1, y);
        let planned = plan(&mut encoder, &moved)?;
        encoder.commit(planned, update_id + 1)
    } else {
        let planned = plan(&mut encoder, &frame)?;
        encoder.commit(planned, update_id)
    };
    let encode_ms = started.elapsed().as_secs_f64() * 1000.0;

    let parts = UpdateSplitter::new()
        .part(update.clone())
        .map_err(|error| CliError::Input(format!("the update could not be parted: {error}")))?;

    let envelopes: Vec<Envelope> = parts
        .iter()
        .map(|part| Envelope {
            update_id: part.update_id,
            base_update_id: part.base_update_id,
            width: part.width,
            height: part.height,
            part_index: part.part_index,
            part_count: part.part_count,
            update_bytes: part.update_bytes,
            codec: 8,
            regions: part.regions.clone(),
        })
        .collect();

    let regions: Vec<Value> = update
        .regions
        .iter()
        .enumerate()
        .map(|(index, region)| region_value(index, region))
        .collect();

    if json {
        Ok(Produced::ok(json_line(&json!({
            "command": "encode",
            "update": update_value(&update),
            "parts": envelopes
                .iter()
                .map(|envelope| part_value(envelope, &encode_envelope(envelope)))
                .collect::<Vec<Value>>(),
            "regions": regions,
            "phases": phase_values(&collector.timings()),
            "encode_ms": encode_ms,
        }))))
    } else {
        Ok(Produced::ok(render(
            &update,
            &envelopes,
            &regions,
            &collector.timings(),
            encode_ms,
        )))
    }
}

/// Plan one update for a surface.
fn plan(
    encoder: &mut ScreenStreamEncoder,
    frame: &CapturedFrame,
) -> Result<crate::encode::PlannedUpdate, CliError> {
    let planned = encoder
        .encode(frame)
        .map_err(|error| CliError::Input(format!("the surface could not be encoded: {error}")))?;
    planned.ok_or_else(|| {
        CliError::Input(
            "the surface produced no update: it is unchanged and the idle interval has not passed"
                .to_string(),
        )
    })
}

/// Read the surface to encode: a synthetic scene, or a raw RGBA file.
fn read_surface(options: &Options) -> Result<CapturedFrame, CliError> {
    let width = options.unsigned::<u32>("width")?;
    let height = options.unsigned::<u32>("height")?;
    if let Some(path) = options.value("file") {
        let width = width.ok_or_else(|| {
            CliError::Usage("flag `--width` is required with `--file`".to_string())
        })?;
        let height = height.ok_or_else(|| {
            CliError::Usage("flag `--height` is required with `--file`".to_string())
        })?;
        let rgba = std::fs::read(path).map_err(|error| {
            CliError::Input(format!("the file {path:?} could not be read: {error}"))
        })?;
        let expected = (width as usize) * (height as usize) * 4;
        if rgba.len() != expected {
            return Err(CliError::Input(format!(
                "the file {path:?} holds {} bytes; a {width}x{height} surface needs {expected}",
                rgba.len()
            )));
        }
        return CapturedFrame::new(width, height, rgba)
            .map_err(|error| CliError::Input(format!("the surface is not valid: {error}")));
    }
    #[cfg(not(feature = "scenes"))]
    {
        let _ = width;
        let _ = height;
        return Err(CliError::Usage("scenes feature not enabled".to_string()));
    }
    #[cfg(feature = "scenes")]
    {
        let scene_name = options.value("scene").unwrap_or("office");
        if scene_name != "office" {
            return Err(CliError::Usage(format!(
                "unknown scene {scene_name:?}; the scene is `office`"
            )));
        }
        let width = width.unwrap_or(640);
        let height = height.unwrap_or(360);
        if width == 0 || height == 0 {
            return Err(CliError::Usage(
                "the surface's dimensions must not be zero".to_string(),
            ));
        }
        Ok(office(width, height, 29))
    }
}

/// Read the codec's configuration.
fn read_config(options: &Options) -> Result<CodecConfig, CliError> {
    let mut config = CodecConfig::default();
    if let Some(level) = options.unsigned::<u8>("deflate-level")? {
        if level > 9 {
            return Err(CliError::Usage(
                "flag `--deflate-level` is outside 0..=9".to_string(),
            ));
        }
        config.delta_deflate_level = level;
        config.reset_deflate_level = level;
    }
    if let Some(threads) = options.unsigned::<usize>("threads")? {
        config.encoder_threads = threads;
    }
    Ok(config.clamped())
}

/// The JSON form of the update's own fields.
fn update_value(update: &EncodedUpdate) -> Value {
    json!({
        "update_id": update.update_id,
        "base_update_id": update.base_update_id,
        "kind": update.kind().as_str(),
        "width": update.width,
        "height": update.height,
        "changed_cells": update.changed_cells,
        "sent_cells": update.sent_cells,
        "backlog_cells": update.backlog_cells,
        "payload_bytes": update.payload_bytes(),
        "regions": update.regions.len(),
        "reset_reasons": update
            .reset_reasons
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<&str>>(),
    })
}

/// The JSON form of one part.
fn part_value(envelope: &Envelope, bytes: &[u8]) -> Value {
    json!({
        "part_index": envelope.part_index,
        "part_count": envelope.part_count,
        "update_bytes": envelope.update_bytes,
        "regions": envelope.regions.len(),
        "payload_bytes": envelope
            .regions
            .iter()
            .map(|region| region.payload.len())
            .sum::<usize>(),
        "bytes_len": bytes.len(),
        "bytes_hex": hex(bytes),
    })
}

/// The JSON form of one region: its geometry, its size, and its op stream.
fn region_value(index: usize, region: &Region) -> Value {
    let ops = op_counts(&region.payload);
    json!({
        "index": index,
        "x": region.x,
        "y": region.y,
        "width": region.width,
        "height": region.height,
        "cells": u64::from(region.width) * u64::from(region.height),
        "payload_bytes": region.payload.len(),
        "payload_sha256": sha256_hex(&region.payload),
        "ops": ops,
    })
}

/// Count the ops in a region's op stream.
///
/// A region that does not decode is reported as it is: the counts stay
/// `null`, and the tool's own decode command is what refuses it.
fn op_counts(payload: &[u8]) -> Value {
    let mut scratch = Vec::new();
    match crate::inflate_region(payload, &mut scratch) {
        Ok(()) => {
            let mut rows = 0_usize;
            let mut spans = 0_usize;
            let mut fills = 0_usize;
            let mut cells = 0_u64;
            let mut pointer = 0_usize;
            while pointer < scratch.len() {
                match scratch[pointer] {
                    crate::OP_ROW => {
                        rows += 1;
                        pointer += 3;
                    }
                    crate::OP_SPAN => {
                        spans += 1;
                        let length = u16::from_le_bytes([
                            *scratch.get(pointer + 3).unwrap_or(&0),
                            *scratch.get(pointer + 4).unwrap_or(&0),
                        ]);
                        cells += u64::from(length);
                        pointer += 5 + (length as usize) * 2;
                    }
                    crate::OP_FILL => {
                        fills += 1;
                        let length = u16::from_le_bytes([
                            *scratch.get(pointer + 3).unwrap_or(&0),
                            *scratch.get(pointer + 4).unwrap_or(&0),
                        ]);
                        cells += u64::from(length);
                        pointer += 7;
                    }
                    crate::OP_END => {
                        pointer += 1;
                    }
                    _ => {
                        pointer += 1;
                    }
                }
            }
            json!({
                "rows": rows,
                "spans": spans,
                "fills": fills,
                "cells": cells,
            })
        }
        Err(_) => Value::Null,
    }
}

/// The JSON form of the phase timings.
fn phase_values(timings: &[(EncodePhase, Vec<u64>)]) -> Value {
    let mut phases = Vec::new();
    for (phase, samples) in timings {
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        let total: u64 = sorted.iter().sum();
        let median = percentile(&sorted, 50);
        let p95 = percentile(&sorted, 95);
        phases.push(json!({
            "phase": phase.as_str(),
            "count": sorted.len(),
            "total_ms": total as f64 / 1000.0,
            "median_ms": median as f64 / 1000.0,
            "p95_ms": p95 as f64 / 1000.0,
        }));
    }
    Value::Array(phases)
}

/// The `percentile`th percentile of a sorted sample list.
///
/// The nearest-rank method: the smallest sample at or above the requested
/// share of the population.
pub fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}

/// The human form of an encode.
fn render(
    update: &EncodedUpdate,
    envelopes: &[Envelope],
    regions: &[Value],
    timings: &[(EncodePhase, Vec<u64>)],
    encode_ms: f64,
) -> String {
    let mut text = String::new();
    text.push_str("update encoded\n");
    text.push_str(&format!("  update id        {}\n", update.update_id));
    text.push_str(&format!("  update base      {}\n", update.base_update_id));
    text.push_str(&format!("  kind             {}\n", update.kind().as_str()));
    text.push_str(&format!(
        "  geometry         {}x{}\n",
        update.width, update.height
    ));
    text.push_str(&format!("  changed cells    {}\n", update.changed_cells));
    text.push_str(&format!("  sent cells       {}\n", update.sent_cells));
    text.push_str(&format!("  backlog cells    {}\n", update.backlog_cells));
    text.push_str(&format!(
        "  payload          {} bytes\n",
        update.payload_bytes()
    ));
    text.push_str(&format!("  regions          {}\n", update.regions.len()));
    text.push_str(&format!("  parts            {}\n", envelopes.len()));
    text.push_str(&format!("  encode           {encode_ms:.3} ms\n"));

    if !regions.is_empty() {
        text.push('\n');
        let rows: Vec<Vec<String>> = regions
            .iter()
            .map(|region| {
                vec![
                    region["index"].to_string(),
                    format!(
                        "{},{} {}x{}",
                        region["x"].as_u64().unwrap_or(0),
                        region["y"].as_u64().unwrap_or(0),
                        region["width"].as_u64().unwrap_or(0),
                        region["height"].as_u64().unwrap_or(0)
                    ),
                    region["payload_bytes"].to_string(),
                    region["payload_sha256"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ]
            })
            .collect();
        text.push_str(&table(&["region", "at", "bytes", "sha256"], &rows));
    }

    if !timings.is_empty() {
        text.push('\n');
        let rows: Vec<Vec<String>> = timings
            .iter()
            .map(|(phase, samples)| {
                let mut sorted = samples.clone();
                sorted.sort_unstable();
                vec![
                    phase.as_str().to_string(),
                    sorted.len().to_string(),
                    format!("{:.3}", percentile(&sorted, 50) as f64 / 1000.0),
                    format!("{:.3}", percentile(&sorted, 95) as f64 / 1000.0),
                ]
            })
            .collect();
        text.push_str(&table(&["phase", "count", "median ms", "p95 ms"], &rows));
    }

    text.push('\n');
    for envelope in envelopes {
        let bytes = encode_envelope(envelope);
        text.push_str(&format!(
            "  part {} of {}: {} bytes, {} region(s)\n",
            envelope.part_index,
            envelope.part_count,
            bytes.len(),
            envelope.regions.len()
        ));
        text.push_str(&format!("    {}\n", hex(&bytes)));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_options;

    #[test]
    fn an_encoded_update_reports_its_parts_and_its_regions() {
        let options = test_options(&[("width", Some("160")), ("height", Some("120"))]);
        let produced = encode(&options, true).expect("encodes");
        let document: Value = serde_json::from_str(&produced.text).expect("json");
        assert_eq!(document["command"], "encode");
        assert_eq!(document["update"]["kind"], "self-contained");
        assert_eq!(document["update"]["width"], 160);
        assert!(!document["parts"].as_array().expect("parts").is_empty());
        assert!(!document["regions"].as_array().expect("regions").is_empty());
    }

    #[test]
    fn the_encoded_part_is_the_schema_s_envelope() {
        let options = test_options(&[("width", Some("64")), ("height", Some("48"))]);
        let produced = encode(&options, true).expect("encodes");
        let document: Value = serde_json::from_str(&produced.text).expect("json");
        let bytes = document["parts"][0]["bytes_hex"].as_str().expect("hex");
        let envelope =
            crate::cli::wire::decode_envelope(&crate::cli::wire::parse_hex(bytes).expect("hex"))
                .expect("decodes");
        assert_eq!(envelope.width, 64);
        assert_eq!(envelope.height, 48);
        assert_eq!(envelope.codec, 8);
    }

    #[test]
    fn an_unknown_scene_is_a_usage_error() {
        let options = test_options(&[("scene", Some("nonsense"))]);
        let error = encode(&options, true).expect_err("unknown scene");
        assert_eq!(error.exit_code(), crate::cli::EXIT_USAGE);
    }

    #[test]
    fn a_file_without_its_dimensions_is_a_usage_error() {
        let options = test_options(&[("file", Some("/nowhere/at/all"))]);
        let error = encode(&options, true).expect_err("no dimensions");
        assert_eq!(error.exit_code(), crate::cli::EXIT_USAGE);
    }

    #[test]
    fn the_percentile_of_a_sorted_sample_is_its_rank() {
        let samples = vec![1_u64, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&samples, 50), 5);
        assert_eq!(percentile(&samples, 95), 10);
        assert_eq!(percentile(&samples, 100), 10);
        assert_eq!(percentile(&[], 50), 0);
    }
}
