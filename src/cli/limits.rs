//! `limits`: the codec's limits, as a table and as JSON.
//!
//! Every value in the table is the crate's own constant, so the table cannot
//! drift from the code that enforces it.

use serde_json::{Value, json};

use crate::cli::{CliError, Options, Produced, json_line, table};
use crate::format::{
    CODEC_NAME, CODEC_WIRE_ID, DELTA_OPS_BUDGET_BYTES, DELTA_REFRESH_CELLS,
    DELTA_REFRESH_MIN_CELLS, FILL_MIN_RUN, IDLE_UPDATE_INTERVAL_MS, JOURNAL_MAX_BYTES,
    JOURNAL_MAX_UPDATES, MAX_PART_WIRE_BYTES, MAX_PARTS_PER_UPDATE, MAX_REGION_OPS_BYTES,
    MAX_REGION_WIRE_BYTES, MAX_REGIONS_PER_PART, MAX_REGIONS_PER_UPDATE, MAX_SURFACE_CELLS,
    MAX_SURFACE_DIMENSION, MAX_UPDATE_WIRE_BYTES, RESET_BAND_ROWS, RESET_MAX_PARTS,
    RESET_MIN_INTERVAL_MS, RESET_PAYLOAD_BUDGET_BYTES, RESYNC_MIN_INTERVAL_MS,
};

/// What an entry holds: a number, or a rule's own text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntryValue {
    /// A whole number.
    Number(u64),
    /// A rule: the specification's own text.
    Rule(&'static str),
}

impl EntryValue {
    /// The JSON form of the value.
    fn to_json(self) -> Value {
        match self {
            Self::Number(value) => Value::from(value),
            Self::Rule(value) => Value::String(value.to_string()),
        }
    }

    /// The human form of the value.
    fn to_text(self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Rule(value) => value.to_string(),
        }
    }
}

/// One entry of the limits table.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// The constant's name.
    pub name: &'static str,
    /// The constant's value.
    pub value: EntryValue,
    /// The unit the value is counted in.
    pub unit: Option<&'static str>,
    /// The section that states it.
    pub spec: &'static str,
}

impl Entry {
    /// A numeric cap.
    const fn number(
        name: &'static str,
        value: u64,
        unit: &'static str,
        spec: &'static str,
    ) -> Self {
        Self {
            name,
            value: EntryValue::Number(value),
            unit: Some(unit),
            spec,
        }
    }

    /// A rule: the specification's own text.
    const fn rule(name: &'static str, value: &'static str, spec: &'static str) -> Self {
        Self {
            name,
            value: EntryValue::Rule(value),
            unit: None,
            spec,
        }
    }
}

/// One group of the limits table.
#[derive(Debug, Clone)]
pub struct Group {
    /// The group's name.
    pub name: &'static str,
    /// What the group holds.
    pub spec: &'static str,
    /// The group's entries.
    pub entries: Vec<Entry>,
}

/// Every group, in the order the table prints them.
pub fn groups() -> Vec<Group> {
    vec![
        Group {
            name: "identity",
            spec: "§3.2",
            entries: vec![
                Entry::number("CODEC_WIRE_ID", CODEC_WIRE_ID as u64, "id", "§3.2"),
                Entry::rule("CODEC_NAME", CODEC_NAME, "§3.2"),
            ],
        },
        Group {
            name: "surface",
            spec: "§7",
            entries: vec![
                Entry::number(
                    "MAX_SURFACE_DIMENSION",
                    u64::from(MAX_SURFACE_DIMENSION),
                    "cells",
                    "§7",
                ),
                Entry::number("MAX_SURFACE_CELLS", MAX_SURFACE_CELLS, "cells", "§7"),
            ],
        },
        Group {
            name: "regions",
            spec: "§7",
            entries: vec![
                Entry::number(
                    "MAX_REGIONS_PER_UPDATE",
                    MAX_REGIONS_PER_UPDATE as u64,
                    "regions",
                    "§7",
                ),
                Entry::number(
                    "MAX_REGIONS_PER_PART",
                    MAX_REGIONS_PER_PART as u64,
                    "regions",
                    "§7",
                ),
                Entry::number(
                    "MAX_REGION_WIRE_BYTES",
                    MAX_REGION_WIRE_BYTES as u64,
                    "bytes",
                    "§7",
                ),
                Entry::number(
                    "MAX_REGION_OPS_BYTES",
                    MAX_REGION_OPS_BYTES as u64,
                    "bytes",
                    "§7",
                ),
            ],
        },
        Group {
            name: "parts",
            spec: "§7",
            entries: vec![
                Entry::number(
                    "MAX_PART_WIRE_BYTES",
                    MAX_PART_WIRE_BYTES as u64,
                    "bytes",
                    "§7",
                ),
                Entry::number(
                    "MAX_PARTS_PER_UPDATE",
                    MAX_PARTS_PER_UPDATE as u64,
                    "parts",
                    "§7",
                ),
            ],
        },
        Group {
            name: "updates",
            spec: "§7",
            entries: vec![
                Entry::number(
                    "MAX_UPDATE_WIRE_BYTES",
                    MAX_UPDATE_WIRE_BYTES as u64,
                    "bytes",
                    "§7",
                ),
                Entry::number("RESET_MAX_PARTS", RESET_MAX_PARTS as u64, "parts", "§7"),
                Entry::number(
                    "RESET_PAYLOAD_BUDGET_BYTES",
                    RESET_PAYLOAD_BUDGET_BYTES as u64,
                    "bytes",
                    "§7",
                ),
                Entry::number(
                    "DELTA_REFRESH_CELLS",
                    u64::from(DELTA_REFRESH_CELLS),
                    "cells",
                    "§7",
                ),
                Entry::number(
                    "DELTA_REFRESH_MIN_CELLS",
                    DELTA_REFRESH_MIN_CELLS,
                    "cells",
                    "§7",
                ),
                Entry::number(
                    "DELTA_OPS_BUDGET_BYTES",
                    DELTA_OPS_BUDGET_BYTES as u64,
                    "bytes",
                    "§7",
                ),
                Entry::number("RESET_BAND_ROWS", u64::from(RESET_BAND_ROWS), "rows", "§7"),
                Entry::number("FILL_MIN_RUN", u64::from(FILL_MIN_RUN), "cells", "§7"),
            ],
        },
        Group {
            name: "journal",
            spec: "§7",
            entries: vec![
                Entry::number("JOURNAL_MAX_BYTES", JOURNAL_MAX_BYTES as u64, "bytes", "§7"),
                Entry::number(
                    "JOURNAL_MAX_UPDATES",
                    JOURNAL_MAX_UPDATES as u64,
                    "updates",
                    "§7",
                ),
            ],
        },
        Group {
            name: "timing",
            spec: "§7",
            entries: vec![
                Entry::number(
                    "IDLE_UPDATE_INTERVAL_MS",
                    IDLE_UPDATE_INTERVAL_MS,
                    "ms",
                    "§7",
                ),
                Entry::number("RESYNC_MIN_INTERVAL_MS", RESYNC_MIN_INTERVAL_MS, "ms", "§7"),
                Entry::number("RESET_MIN_INTERVAL_MS", RESET_MIN_INTERVAL_MS, "ms", "§7"),
            ],
        },
        Group {
            name: "relations",
            spec: "§7.1",
            entries: vec![
                Entry::rule(
                    "MAX_UPDATE_WIRE_BYTES",
                    "= MAX_PARTS_PER_UPDATE * MAX_PART_WIRE_BYTES",
                    "§7.1",
                ),
                Entry::rule(
                    "RESET_PAYLOAD_BUDGET_BYTES",
                    "= RESET_MAX_PARTS * MAX_PART_WIRE_BYTES",
                    "§7.1",
                ),
                Entry::rule(
                    "MAX_SURFACE_CELLS",
                    "= MAX_SURFACE_DIMENSION squared",
                    "§7.1",
                ),
            ],
        },
    ]
}

/// Print the codec's limits.
pub fn limits(options: &Options, json: bool) -> Result<Produced, CliError> {
    if !options.positionals().is_empty() {
        return Err(CliError::Usage(format!(
            "the limits command takes no argument, but {} were given",
            options.positionals().len()
        )));
    }
    let groups = groups();
    if json {
        let rendered: Vec<Value> = groups
            .iter()
            .map(|group| {
                json!({
                    "name": group.name,
                    "spec": group.spec,
                    "entries": group.entries.iter().map(|entry| json!({
                        "name": entry.name,
                        "value": entry.value.to_json(),
                        "unit": entry.unit,
                        "spec": entry.spec,
                    })).collect::<Vec<Value>>(),
                })
            })
            .collect();
        Ok(Produced::ok(json_line(&json!({
            "command": "limits",
            "codec": CODEC_NAME,
            "wire_id": CODEC_WIRE_ID,
            "groups": rendered,
        }))))
    } else {
        Ok(Produced::ok(render(&groups)))
    }
}

/// The human form of the limits table.
fn render(groups: &[Group]) -> String {
    let mut text = String::new();
    for group in groups {
        text.push_str(&format!("{} ({})\n", group.name, group.spec));
        let rows: Vec<Vec<String>> = group
            .entries
            .iter()
            .map(|entry| {
                vec![
                    entry.name.to_string(),
                    entry.value.to_text(),
                    entry.unit.unwrap_or("").to_string(),
                    entry.spec.to_string(),
                ]
            })
            .collect();
        text.push_str(&table(&["name", "value", "unit", "spec"], &rows));
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_numeric_caps_are_the_crates_own_constants() {
        let groups = groups();
        let surface = &groups[1];
        assert_eq!(
            surface.entries[0].value,
            EntryValue::Number(u64::from(MAX_SURFACE_DIMENSION))
        );
        let updates = &groups[4];
        assert_eq!(
            updates.entries[0].value,
            EntryValue::Number(MAX_UPDATE_WIRE_BYTES as u64)
        );
    }

    #[test]
    fn the_table_holds_every_cap_the_specification_states() {
        let names: Vec<&str> = groups()
            .iter()
            .flat_map(|group| group.entries.iter())
            .map(|entry| entry.name)
            .collect();
        for expected in [
            "MAX_SURFACE_DIMENSION",
            "MAX_SURFACE_CELLS",
            "MAX_REGIONS_PER_UPDATE",
            "MAX_REGIONS_PER_PART",
            "MAX_REGION_WIRE_BYTES",
            "MAX_REGION_OPS_BYTES",
            "MAX_PART_WIRE_BYTES",
            "MAX_PARTS_PER_UPDATE",
            "MAX_UPDATE_WIRE_BYTES",
            "RESET_MAX_PARTS",
            "DELTA_REFRESH_CELLS",
            "DELTA_REFRESH_MIN_CELLS",
            "DELTA_OPS_BUDGET_BYTES",
            "RESET_BAND_ROWS",
            "FILL_MIN_RUN",
            "JOURNAL_MAX_BYTES",
            "JOURNAL_MAX_UPDATES",
            "IDLE_UPDATE_INTERVAL_MS",
            "RESYNC_MIN_INTERVAL_MS",
            "RESET_MIN_INTERVAL_MS",
        ] {
            assert!(names.contains(&expected), "{expected} is missing");
        }
    }

    #[test]
    fn the_json_form_carries_every_group() {
        let options = crate::cli::test_options(&[]);
        let produced = limits(&options, true).expect("limits");
        let document: Value = serde_json::from_str(&produced.text).expect("json");
        let rendered = document["groups"].as_array().expect("groups");
        assert_eq!(rendered.len(), groups().len());
        assert_eq!(document["command"], "limits");
        assert_eq!(document["codec"], "tessellio");
        assert_eq!(document["wire_id"], 8);
    }
}
