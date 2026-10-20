//! The `tessellio-core` command-line tool: the screen codec's front door.
//!
//! The tool is a thin shell around [`tessellio_core::cli`]: it hands the
//! argument vector to the tool's dispatcher, prints what it produced, and
//! exits with the status the dispatcher decided.
//!
//! ```text
//! tessellio-core --help
//! tessellio-core encode --scene office --width 640 --height 360
//! tessellio-core inspect --hex 08021001180820043001
//! tessellio-core bench --iterations 5
//! ```

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let outcome = tessellio_core::cli::run(&args);
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    ExitCode::from(outcome.code)
}
