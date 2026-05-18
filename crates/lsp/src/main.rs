//! `rpm-spec-lsp` — Language Server Protocol entry point.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use lsp_server::Connection;

fn main() -> ExitCode {
    init_tracing();
    if let Err(e) = run() {
        eprintln!("rpm-spec-lsp: fatal: {e:#}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
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
