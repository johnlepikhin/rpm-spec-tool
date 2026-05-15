//! `rpm-spec-tool` CLI entry point.

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
    let _ = fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();
}
