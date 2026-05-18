//! `matrix` subcommand — multi-profile (release matrix) analysis.
//!
//! Phase 1 ships one action: `matrix check`. Future actions (explain,
//! coverage, portability, diff, impact) sit alongside it.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand};

pub mod baseline;
pub mod check;

pub use check::CheckOpts;

#[derive(Debug, Args)]
pub struct Cmd {
    /// Explicit path to `.rpmspec.toml`. Without this flag the nearest
    /// `.rpmspec.toml` walking upward from each input file is used.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Run all active lint rules against every member profile of a
    /// release target set and aggregate findings by affected profiles.
    Check(CheckOpts),
    /// Baseline management — record / inspect the set of currently
    /// known findings so CI can fail only on new ones.
    Baseline(baseline::Cmd),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        match self.action {
            Action::Check(opts) => check::run(opts, self.config.as_deref(), color),
            Action::Baseline(cmd) => cmd.run(self.config.as_deref()),
        }
    }
}
