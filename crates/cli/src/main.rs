//! `rpm-spec-tool` CLI entry point.

#![forbid(unsafe_code)]

mod app;
mod commands;
mod config;
mod fixer;
mod io;
mod output;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let app = app::Application::parse();
    init_tracing();

    match app.run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // `try_init` can fail only if a global subscriber is already installed
    // (e.g. in tests). Surface the error rather than swallow it silently.
    if let Err(e) = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
    {
        eprintln!("warning: failed to initialize tracing: {e}");
    }
}
