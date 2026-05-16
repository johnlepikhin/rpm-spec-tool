//! Minimal end-to-end lint runner using the analyzer's library API.
//!
//! Reads a `.spec` source from a file path passed as the first
//! argument (or from stdin when the argument is missing or `-`),
//! runs the default lint set, and prints one `severity id message`
//! line per finding. Exit code mirrors the `lint` subcommand:
//! `0` if no deny-severity diagnostics, `1` otherwise, `2` on I/O
//! failure.
//!
//! Run with:
//!
//! ```text
//! cargo run --example lint_file -p rpm-spec-analyzer -- path/to/foo.spec
//! ```
use std::io::{self, Read};
use std::process::ExitCode;

use rpm_spec_analyzer::{Diagnostic, Severity, analyze, config::Config};

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let source = match arg.as_deref() {
        None | Some("-") => {
            let mut buf = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut buf) {
                eprintln!("lint_file: failed to read stdin: {e}");
                return ExitCode::from(2);
            }
            buf
        }
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("lint_file: failed to read {path}: {e}");
                return ExitCode::from(2);
            }
        },
    };
    let (_outcome, diags) = analyze(&source, &Config::default());
    for Diagnostic {
        severity,
        lint_id,
        message,
        ..
    } in &diags
    {
        println!("{severity:?} {lint_id} {message}");
    }
    if diags.iter().any(|d| d.severity == Severity::Deny) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
