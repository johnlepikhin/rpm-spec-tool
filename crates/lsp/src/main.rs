//! `rpm-spec-lsp` — Language Server Protocol entry point.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use lsp_server::Connection;

fn main() -> ExitCode {
    // Handle informational flags before any LSP / tracing setup so
    // packaging smoke-tests and editor configuration probes can run
    // without speaking the JSON-RPC framing.
    let mut args = std::env::args().skip(1);
    if let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("rpm-spec-lsp: unknown argument: {other}");
                eprintln!("hint: this binary speaks LSP on stdio; use --help to see flags");
                return ExitCode::from(2);
            }
        }
    }
    init_tracing();
    if let Err(e) = run() {
        eprintln!("rpm-spec-lsp: fatal: {e:#}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "{name} {version}
Language Server Protocol implementation for RPM .spec files.

Usage: rpm-spec-lsp [OPTIONS]

This binary speaks LSP over stdio when launched with no arguments. Editor
integrations should configure their LSP client to spawn it directly.

Options:
  -h, --help       Print this help and exit.
  -V, --version    Print version and exit.

Environment:
  RUST_LOG         Filter spec for tracing output to stderr (e.g. `info`,
                   `rpm_spec_lsp=debug`). Default: `info`.",
        name = env!("CARGO_PKG_NAME"),
        version = env!("CARGO_PKG_VERSION"),
    );
}

fn run() -> anyhow::Result<()> {
    tracing::info!("rpm-spec-lsp starting on stdio");
    let (connection, io_threads) = Connection::stdio();
    let server = rpm_spec_lsp::Server::new(connection);
    server.run()?;
    io_threads.join()?;
    tracing::info!("rpm-spec-lsp shut down cleanly");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Tracing goes to stderr; stdout is reserved for the LSP transport.
    if let Err(e) = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
    {
        eprintln!("warning: failed to initialize tracing: {e}");
    }
}
