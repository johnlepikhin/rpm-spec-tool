//! `config` subcommand — generate and validate `.rpmspec.toml`.
//!
//! Two actions:
//! * `config init` — serialize [`Config::default`] to TOML and write
//!   it. With `--all-lints`, every built-in lint is emitted on its own
//!   line with its default severity (commented out so the file
//!   round-trips through deserialization without change in behaviour).
//! * `config validate` — load and parse the named file (or the
//!   `$XDG_CONFIG_HOME/rpm-spec-tool/rpmspec.toml` default), report
//!   the first deserialization error with file:line pointing at the
//!   offending span.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand};

mod doc;
mod init;
mod schema;
mod validate;

pub use doc::DocOpts;
pub use init::InitOpts;
pub use schema::SchemaOpts;
pub use validate::ValidateOpts;

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Write a starter `rpmspec.toml` (defaults to the XDG config
    /// path: `~/.config/rpm-spec-tool/rpmspec.toml`).
    Init(InitOpts),
    /// Parse an `rpmspec.toml` and report any deserialization errors.
    Validate(ValidateOpts),
    /// Emit the JSON Schema for `.rpmspec.toml`. Pipe to a file and
    /// point your TOML editor (taplo / VS Code / Helix / Zed) at it
    /// for completion + validation.
    Schema(SchemaOpts),
    /// Render a markdown reference page for every config field,
    /// generated from the same JSON Schema. Use `--field NAME` to
    /// narrow it to one section.
    Doc(DocOpts),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        match self.action {
            Action::Init(opts) => init::run(opts),
            Action::Validate(opts) => validate::run(opts, color),
            Action::Schema(opts) => schema::run(opts),
            Action::Doc(opts) => doc::run(opts),
        }
    }
}
