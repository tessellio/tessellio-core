//! `inspect` and `decode`: the codec's own verbs.

use serde_json::{Value, json};

use crate::cli::wire::{Envelope, decode_envelope, parse_hex, sha256_hex};
use crate::cli::{CliError, Options, Produced, hex, json_line, table};
use crate::format::Codec;
use crate::part::{AssemblyOutcome, Part, UpdateAssembler};
use crate::{EncodedUpdate, SurfaceCanvas};

/// Read an update's bytes and print the envelope's fields and the regions'
/// geometry.
pub fn inspect(options: &Options, json: bool) -> Result<Produced, CliError> {
    let bytes = read_one(options)?;
    let envelope = decode_envelope(&bytes)?;
    if json {
        Ok(Produced::ok(json_line(&json!({
            "command": "inspect",
            "envelope": envelope_value(&envelope),
            "regions": envelope
                .regions
                .iter()
                .enumerate()
                .map(|(index, region)| region_value(index, region))
                .collect::<Vec<Value>>(),
            "bytes_len": bytes.len(),
        }))))
    } else {
        Ok(Produced::ok(render_envelope(&envelope, bytes.len())))
    }
}

/// Decode an update's bytes and report the regions, the cells and the surface.
pub fn decode(options: &Options, json: bool) -> Result<Produced, CliError> {
    let parts = read_parts(options)?;
    let mut envelopes: Vec<Envelope> = Vec::new();
    for (index, bytes) in parts.iter().enumerate() {
        let envelope = decode_envelope(bytes).map_err(|error| {
            CliError::Input(format!(
                "part {index} could not be read: {}",
                error.message()
            ))
        })?;
        envelopes.push(envelope);
    }

    let mut assembler = UpdateAssembler::new();
    let mut canvas = SurfaceCanvas::new();
    let mut applied: Vec<u64> = Vec::new();
    let mut regions: Vec<Value> = Vec::new();
    let mut cells_painted = 0_u64;
    let mut rejection: Option<String> = None;
    let mut pending = false;

    for envelope in &envelopes {
        let part = Part {
            update_id: envelope.update_id,
            update_base: envelope.update_base,
            width: envelope.width,
            height: envelope.height,
            part_index: envelope.part_index,
            part_count: envelope.part_count,
            update_bytes: envelope.update_bytes,
            codec: Codec::Tessellio,
            regions: envelope.regions.clone(),
        };
        match assembler.push(part) {
            AssemblyOutcome::Pending => pending = true,
            AssemblyOutcome::Complete(assembled) => {
                let update = EncodedUpdate {
                    update_id: assembled.update_id,
                    update_base: assembled.update_base,
                    width: assembled.width,
                    height: assembled.height,
                    codec: Codec::Tessellio,
                    regions: assembled.regions.clone(),
                    changed_cells: 0,
                    sent_cells: 0,
                    backlog_cells: 0,
                    changed_cell_indices: Vec::new(),
                    reset_reasons: Default::default(),
                };
                for (index, region) in update.regions.iter().enumerate() {
                    let value = region_value(index, region);
                    cells_painted += value["cells"].as_u64().unwrap_or(0);
                    regions.push(value);
                }
                canvas.apply(&update).map_err(|error| {
                    CliError::Input(format!(
                        "update {} could not be applied: {}",
                        update.update_id,
                        error.reason_code()
                    ))
                })?;
                applied.push(update.update_id);
            }
            AssemblyOutcome::Rejected(reason) => {
                rejection = Some(reason.reason_code().to_string());
                break;
            }
        }
    }

    let surface = canvas.geometry().map(|(width, height)| {
        let cells = canvas.cells();
        let mut bytes = Vec::with_capacity(cells.len() * 2);
        for cell in cells {
            bytes.extend_from_slice(&cell.to_le_bytes());
        }
        json!({
            "width": width,
            "height": height,
            "applied_update_id": canvas.applied_update_id(),
            "cells": cells.len(),
            "sha256": sha256_hex(&bytes),
        })
    });

    if json {
        Ok(Produced::ok(json_line(&json!({
            "command": "decode",
            "parts": envelopes.len(),
            "applied_updates": applied,
            "pending": pending,
            "rejection": rejection,
            "regions": regions,
            "cells_painted": cells_painted,
            "surface": surface,
        }))))
    } else {
        Ok(Produced::ok(render_decode(
            &envelopes,
            &applied,
            pending,
            rejection.as_deref(),
            &regions,
            cells_painted,
            surface.as_ref(),
        )))
    }
}

/// The JSON form of the envelope's fields.
fn envelope_value(envelope: &Envelope) -> Value {
    json!({
        "update_id": envelope.update_id,
        "update_base": envelope.update_base,
        "width": envelope.width,
        "height": envelope.height,
        "part_index": envelope.part_index,
        "part_count": envelope.part_count,
        "update_bytes": envelope.update_bytes,
        "codec": envelope.codec,
        "codec_name": envelope.codec_name(),
        "regions": envelope.regions.len(),
    })
}

/// The JSON form of one region.
fn region_value(index: usize, region: &crate::format::Region) -> Value {
    let cells = u64::from(region.width) * u64::from(region.height);
    json!({
        "index": index,
        "x": region.x,
        "y": region.y,
        "width": region.width,
        "height": region.height,
        "cells": cells,
        "payload_bytes": region.payload.len(),
        "payload_sha256": sha256_hex(&region.payload),
    })
}

/// Read one update's bytes.
fn read_one(options: &Options) -> Result<Vec<u8>, CliError> {
    let mut sources = 0usize;
    if options.value("hex").is_some() {
        sources += 1;
    }
    if options.value("file").is_some() {
        sources += 1;
    }
    if options.flag("stdin").is_some() {
        sources += 1;
    }
    if sources == 0 {
        return Err(CliError::Usage(
            "one of `--hex`, `--file` or `--stdin` is required".to_string(),
        ));
    }
    if sources > 1 {
        return Err(CliError::Usage(
            "only one of `--hex`, `--file` and `--stdin` may be given".to_string(),
        ));
    }
    if let Some(text) = options.value("hex") {
        return parse_hex(text);
    }
    if let Some(path) = options.value("file") {
        let text = std::fs::read_to_string(path).map_err(|error| {
            CliError::Input(format!("the file {path:?} could not be read: {error}"))
        })?;
        return parse_hex(&text);
    }
    let text = crate::cli::read_stdin()?;
    parse_hex(&text)
}

/// Read every part's bytes.
fn read_parts(options: &Options) -> Result<Vec<Vec<u8>>, CliError> {
    let mut parts = Vec::new();
    for text in options.values("hex") {
        parts.push(parse_hex(text)?);
    }
    if let Some(path) = options.value("file") {
        let text = std::fs::read_to_string(path).map_err(|error| {
            CliError::Input(format!("the file {path:?} could not be read: {error}"))
        })?;
        parts.extend(read_lines(&text)?);
    }
    if options.flag("stdin").is_some() {
        let text = crate::cli::read_stdin()?;
        parts.extend(read_lines(&text)?);
    }
    if parts.is_empty() {
        return Err(CliError::Usage(
            "no part was given; use `--hex`, `--file` or `--stdin`".to_string(),
        ));
    }
    Ok(parts)
}

/// Read one part's hex per line, ignoring blank lines.
fn read_lines(text: &str) -> Result<Vec<Vec<u8>>, CliError> {
    let mut parts = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        parts.push(parse_hex(line).map_err(|error| {
            CliError::Input(format!("line {}: {}", number + 1, error.message()))
        })?);
    }
    Ok(parts)
}

/// The human form of an envelope.
fn render_envelope(envelope: &Envelope, bytes_len: usize) -> String {
    let mut text = String::new();
    text.push_str("update inspected\n");
    text.push_str(&format!("  bytes            {bytes_len}\n"));
    text.push_str(&format!("  update id        {}\n", envelope.update_id));
    text.push_str(&format!("  update base      {}\n", envelope.update_base));
    text.push_str(&format!(
        "  geometry         {}x{}\n",
        envelope.width, envelope.height
    ));
    text.push_str(&format!(
        "  part             {} of {}\n",
        envelope.part_index, envelope.part_count
    ));
    text.push_str(&format!("  update bytes     {}\n", envelope.update_bytes));
    text.push_str(&format!(
        "  codec            {} ({})\n",
        envelope.codec,
        envelope.codec_name()
    ));
    text.push_str(&format!("  regions          {}\n", envelope.regions.len()));
    if !envelope.regions.is_empty() {
        text.push('\n');
        let rows: Vec<Vec<String>> = envelope
            .regions
            .iter()
            .enumerate()
            .map(|(index, region)| {
                vec![
                    index.to_string(),
                    format!("{},{}", region.x, region.y),
                    format!("{}x{}", region.width, region.height),
                    region.payload.len().to_string(),
                    sha256_hex(&region.payload),
                ]
            })
            .collect();
        text.push_str(&table(&["region", "at", "size", "bytes", "sha256"], &rows));
    }
    text
}

/// The human form of a decode.
fn render_decode(
    envelopes: &[Envelope],
    applied: &[u64],
    pending: bool,
    rejection: Option<&str>,
    regions: &[Value],
    cells_painted: u64,
    surface: Option<&Value>,
) -> String {
    let mut text = String::new();
    text.push_str("update decoded\n");
    text.push_str(&format!("  parts            {}\n", envelopes.len()));
    text.push_str(&format!(
        "  applied updates  {}\n",
        applied
            .iter()
            .map(u64::to_string)
            .collect::<Vec<String>>()
            .join(", ")
    ));
    if pending {
        text.push_str("  pending          the update is not complete\n");
    }
    if let Some(reason) = rejection {
        text.push_str(&format!("  rejection        {reason}\n"));
    }
    text.push_str(&format!("  regions          {}\n", regions.len()));
    text.push_str(&format!("  cells painted    {cells_painted}\n"));
    match surface {
        Some(surface) => {
            text.push_str(&format!(
                "  surface          {}x{}\n",
                surface["width"].as_u64().unwrap_or(0),
                surface["height"].as_u64().unwrap_or(0)
            ));
            text.push_str(&format!(
                "  applied update   {}\n",
                surface["applied_update_id"].as_u64().unwrap_or(0)
            ));
            text.push_str(&format!(
                "  surface sha256   {}\n",
                surface["sha256"].as_str().unwrap_or_default()
            ));
        }
        None => {
            text.push_str("  surface          none: the update carries no surface\n");
        }
    }
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
                ]
            })
            .collect();
        text.push_str(&table(&["region", "at", "bytes"], &rows));
    }
    let _ = hex;
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_options;

    fn part_hex(update_id: u64, base: u64, width: u32, height: u32) -> String {
        let envelope = Envelope {
            update_id,
            update_base: base,
            width,
            height,
            part_index: 0,
            part_count: 1,
            update_bytes: 0,
            codec: 8,
            regions: Vec::new(),
        };
        hex(&crate::cli::wire::encode_envelope(&envelope))
    }

    #[test]
    fn an_update_is_inspected_field_by_field() {
        let options = test_options(&[("hex", Some(&part_hex(7, 0, 64, 48)))]);
        let produced = inspect(&options, true).expect("inspects");
        let document: Value = serde_json::from_str(&produced.text).expect("json");
        assert_eq!(document["command"], "inspect");
        assert_eq!(document["envelope"]["update_id"], 7);
        assert_eq!(document["envelope"]["width"], 64);
        assert_eq!(document["envelope"]["codec_name"], "tessellio");
    }

    #[test]
    fn a_self_contained_update_is_decoded_onto_a_surface() {
        let options = test_options(&[("hex", Some(&part_hex(7, 0, 64, 48)))]);
        let produced = decode(&options, true).expect("decodes");
        let document: Value = serde_json::from_str(&produced.text).expect("json");
        assert_eq!(document["command"], "decode");
        assert_eq!(document["applied_updates"], json!([7]));
        assert_eq!(document["surface"]["width"], 64);
        assert!(document["surface"]["sha256"].is_string());
    }

    #[test]
    fn a_chained_update_without_its_base_is_refused_with_its_reason() {
        let options = test_options(&[("hex", Some(&part_hex(7, 6, 64, 48)))]);
        let error = decode(&options, true).expect_err("no base");
        assert_eq!(error.exit_code(), crate::cli::EXIT_INPUT);
        assert!(error.message().contains("DECODE_ERROR"));
    }

    #[test]
    fn a_bad_hex_is_an_input_error() {
        let options = test_options(&[("hex", Some("zz"))]);
        let error = inspect(&options, true).expect_err("bad hex");
        assert_eq!(error.exit_code(), crate::cli::EXIT_INPUT);
    }

    #[test]
    fn no_input_at_all_is_a_usage_error() {
        let options = test_options(&[]);
        let error = inspect(&options, true).expect_err("no input");
        assert_eq!(error.exit_code(), crate::cli::EXIT_USAGE);
    }
}
