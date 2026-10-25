//! The command-line tool's own suite: one test per command, a test that the
//! JSON shape is stable, and a test that the exit-status contract holds.
//!
//! The suite drives the library's dispatcher, so it asserts on exactly what
//! the binary prints. The one test that needs the binary itself is the one
//! that reads standard input.

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

use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Run the tool with an argument vector and return its outcome.
fn run(arguments: &[&str]) -> tessellio_core::cli::Outcome {
    let args: Vec<String> = std::iter::once("tessellio-core".to_string())
        .chain(arguments.iter().map(|value| (*value).to_string()))
        .collect();
    tessellio_core::cli::run(&args)
}

/// The JSON the tool printed.
fn json_of(outcome: &tessellio_core::cli::Outcome) -> Value {
    serde_json::from_str(&outcome.stdout).expect("the tool printed JSON")
}

/// The bytes of an encoded update, as hex.
fn encoded_part_hex() -> String {
    let outcome = run(&["encode", "--width", "64", "--height", "48", "--json"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let document = json_of(&outcome);
    document["parts"][0]["bytes_hex"]
        .as_str()
        .expect("the part's bytes")
        .to_string()
}

#[test]
fn encode_reports_the_bytes_the_parts_and_the_regions() {
    let outcome = run(&["encode", "--width", "160", "--height", "120"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("update encoded"));
    assert!(outcome.stdout.contains("regions"));
    assert!(outcome.stdout.contains("part 0 of"));
}

#[test]
fn inspect_reads_an_update_bytes_field_by_field() {
    let hex = encoded_part_hex();
    let outcome = run(&["inspect", "--hex", &hex]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("update inspected"));
    assert!(outcome.stdout.contains("codec            8 (tessellio)"));
}

#[test]
fn decode_paints_a_surface_and_reports_its_digest() {
    let hex = encoded_part_hex();
    let outcome = run(&["decode", "--hex", &hex, "--json"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let document = json_of(&outcome);
    assert_eq!(document["command"], "decode");
    assert_eq!(document["parts"], 1);
    assert!(document["surface"]["sha256"].is_string());
    assert_eq!(document["surface"]["width"], 64);
    assert_eq!(document["surface"]["height"], 48);
}

#[test]
fn bench_measures_every_scenario_inside_its_budget() {
    let outcome = run(&["bench", "--iterations", "1"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("inside their budgets"));
    assert!(outcome.stdout.contains("pointer_1px"));
    assert!(outcome.stdout.contains("reset_1080p"));
}

#[test]
fn limits_lists_the_caps() {
    let outcome = run(&["limits"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("MAX_SURFACE_DIMENSION"));
    assert!(outcome.stdout.contains("MAX_UPDATE_WIRE_BYTES"));
    assert!(outcome.stdout.contains("JOURNAL_MAX_BYTES"));
}

#[test]
fn the_json_shape_is_stable() {
    let built = run(&["encode", "--width", "64", "--height", "48", "--json"]);
    assert_eq!(built.code, 0, "{}", built.stderr);
    let document = json_of(&built);
    assert_eq!(document["command"], "encode");
    assert_eq!(document["update"]["update_id"], 1);
    assert_eq!(document["update"]["base_update_id"], 0);
    assert_eq!(document["update"]["kind"], "self-contained");
    assert_eq!(document["update"]["width"], 64);
    assert_eq!(document["update"]["height"], 48);
    assert!(document["update"]["changed_cells"].is_number());
    assert!(document["update"]["payload_bytes"].is_number());
    assert_eq!(document["update"]["reset_reasons"], json!(["first"]));
    let part = &document["parts"][0];
    assert_eq!(part["part_index"], 0);
    assert_eq!(part["part_count"], 1);
    assert!(part["bytes_hex"].is_string());
    assert!(part["bytes_len"].is_number());
    assert!(part["regions"].is_number());
    let region = &document["regions"][0];
    assert_eq!(region["index"], 0);
    assert!(region["x"].is_number());
    assert!(region["payload_sha256"].is_string());
    assert!(document["phases"].is_array());
    assert!(document["encode_ms"].is_number());

    let hex = encoded_part_hex();
    let inspected = run(&["inspect", "--hex", &hex, "--json"]);
    assert_eq!(inspected.code, 0, "{}", inspected.stderr);
    let document = json_of(&inspected);
    assert_eq!(document["command"], "inspect");
    assert_eq!(document["envelope"]["update_id"], 1);
    assert_eq!(document["envelope"]["codec"], 8);
    assert_eq!(document["envelope"]["codec_name"], "tessellio");
    assert!(document["envelope"]["regions"].is_number());
    assert!(document["regions"].is_array());
    assert!(document["bytes_len"].is_number());

    let decoded = run(&["decode", "--hex", &hex, "--json"]);
    assert_eq!(decoded.code, 0, "{}", decoded.stderr);
    let document = json_of(&decoded);
    assert_eq!(document["command"], "decode");
    assert_eq!(document["applied_updates"], json!([1]));
    assert_eq!(document["pending"], false);
    assert_eq!(document["rejection"], Value::Null);
    assert!(document["cells_painted"].is_number());
    assert!(document["surface"]["applied_update_id"].is_number());

    let benched = run(&["bench", "--iterations", "1", "--json"]);
    assert_eq!(benched.code, 0, "{}", benched.stderr);
    let document = json_of(&benched);
    assert_eq!(document["command"], "bench");
    assert_eq!(document["iterations"], 1);
    assert!(document["profile"].is_string());
    for scenario in document["scenarios"].as_array().expect("scenarios") {
        assert!(scenario["name"].is_string());
        assert!(scenario["bytes"].is_number());
        assert!(scenario["byte_budget"].is_number());
        assert!(scenario["encode_ms_median"].is_number());
        assert!(scenario["encode_ms_p95"].is_number());
        assert!(scenario["baseline_ms_median"].is_number());
        assert!(scenario["encode_over_baseline"].is_number());
        assert!(scenario["encode_ms_budget"].is_number());
        assert_eq!(scenario["within_budget"], true, "{scenario}");
    }
    assert_eq!(document["summary"]["failed"], 0);

    let limits = run(&["limits", "--json"]);
    assert_eq!(limits.code, 0, "{}", limits.stderr);
    let document = json_of(&limits);
    assert_eq!(document["command"], "limits");
    assert_eq!(document["codec"], "tessellio");
    assert_eq!(document["wire_id"], 8);
    for group in document["groups"].as_array().expect("groups") {
        assert!(group["name"].is_string());
        assert!(group["spec"].is_string());
        for entry in group["entries"].as_array().expect("entries") {
            assert!(entry["name"].is_string());
            assert!(entry["value"].is_number() || entry["value"].is_string());
            assert!(entry["unit"].is_string() || entry["unit"].is_null());
            assert!(entry["spec"].is_string());
        }
    }
}

#[test]
fn the_exit_status_contract_holds() {
    // 0: the command succeeded.
    assert_eq!(run(&["limits"]).code, 0);
    // 2: the usage was wrong.
    assert_eq!(run(&["nonsense"]).code, 2);
    assert_eq!(run(&["limits", "--nonsense"]).code, 2);
    assert_eq!(run(&["inspect"]).code, 2);
    assert_eq!(run(&["encode", "--width"]).code, 2);
    // 3: the input could not be read.
    assert_eq!(run(&["inspect", "--hex", "zz"]).code, 3);
    assert_eq!(run(&["inspect", "--hex", "ffff"]).code, 3);
    assert_eq!(run(&["decode", "--hex", "ffff"]).code, 3);
}

#[test]
fn the_tool_reads_standard_input() {
    let hex = encoded_part_hex();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tessellio-core"))
        .args(["decode", "--stdin", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the tool starts");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("standard input");
        writeln!(stdin, "{hex}").expect("the part is written");
    }
    let output = child.wait_with_output().expect("the tool finishes");
    assert_eq!(output.status.code(), Some(0));
    let document: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(document["parts"], 1);
}

#[test]
fn the_help_documents_the_verbs_the_flags_and_the_exit_status() {
    let outcome = run(&["--help"]);
    assert_eq!(outcome.code, 0);
    for verb in ["encode", "inspect", "decode", "bench", "limits"] {
        assert!(outcome.stdout.contains(verb), "{verb} is missing");
    }
    assert!(outcome.stdout.contains("0  the command succeeded"));
    assert!(outcome.stdout.contains("1  the check failed"));
    assert!(outcome.stdout.contains("2  the usage was wrong"));
    assert!(outcome.stdout.contains("3  the input could not be read"));

    for command in [
        &["encode"][..],
        &["inspect"][..],
        &["decode"][..],
        &["bench"][..],
        &["limits"][..],
    ] {
        let mut arguments = command.to_vec();
        arguments.push("--help");
        let outcome = run(&arguments);
        assert_eq!(outcome.code, 0);
        assert!(
            outcome.stdout.contains("Flags:"),
            "{command:?} has no flags"
        );
        assert!(
            outcome.stdout.contains("Example:"),
            "{command:?} has no example"
        );
    }
}
