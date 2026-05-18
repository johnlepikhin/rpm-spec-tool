//! CLI argument parsing.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::commands;

#[derive(Debug, Parser)]
#[command(name = "rpm-spec-tool", version, about)]
pub struct Application {
    #[command(subcommand)]
    pub command: Command,

    /// When/how to emit ANSI colours. Default `auto` follows TTY detection.
    #[arg(long, global = true, default_value_t = ColorChoice::Auto, value_enum)]
    pub color: ColorChoice,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run lint rules against one or more spec files.
    Lint(commands::lint::Cmd),
    /// Pretty-print spec files (with --check / --in-place / --diff modes).
    Format(commands::format::Cmd),
    /// Pretty-print spec files to stdout with ANSI syntax highlighting.
    Pretty(commands::pretty::Cmd),
    /// Dump the parsed AST.
    Ast(commands::ast::Cmd),
    /// Lint and format-check in one invocation (CI shorthand).
    Check(commands::check::Cmd),
    /// Inspect the resolved distribution profile.
    Profile(commands::profile::Cmd),
    /// Inspect release target sets — collections of profiles used by `matrix`.
    Target(commands::target::Cmd),
    /// Multi-profile (release matrix) analysis.
    Matrix(commands::matrix::Cmd),
    /// List every built-in lint rule with a short description.
    Lints(commands::lints::Cmd),
    /// Emit a shell completion script for the given shell on stdout.
    Completions(commands::completions::Cmd),
}

#[derive(Debug, Args)]
pub struct CommonInput {
    /// Spec files to process. Use `-` (or omit) to read from stdin.
    pub paths: Vec<PathBuf>,

    /// Explicit path to `.rpmspec.toml`. Without this flag the nearest
    /// `.rpmspec.toml` walking upward from each file is used.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
}

/// Ad-hoc macro definitions, mirroring `rpmbuild --define`. Shared by
/// every subcommand that resolves a [`rpm_spec_analyzer::profile::Profile`]
/// (lint, check, every `profile` action). Flattened into each command's
/// `Args` struct via `#[command(flatten)]`.
///
/// The inner field is named `raw` (not `defines`) so call sites read as
/// `opts.defines.raw` instead of the stutter-y `opts.defines.defines`.
/// The values are raw argv strings; the resolver parses them via
/// [`rpm_spec_analyzer::profile::parse_define`].
#[derive(Debug, Args, Default, Clone)]
pub struct MacroDefinesArg {
    /// Define a macro at lint time, mirroring `rpmbuild --define`.
    /// Form: `--define 'NAME VALUE'` — a single argument with the name
    /// and value separated by whitespace. Use shell quoting to keep
    /// the pair as one argv element. Repeatable: `-D 'a 1' -D 'b 2'`.
    ///
    /// CLI defines outrank both the bundled distribution profile and
    /// any `[profiles.*.macros]` overrides in `.rpmspec.toml`.
    #[arg(long = "define", short = 'D', value_name = "NAME VALUE")]
    pub raw: Vec<String>,
}

impl MacroDefinesArg {
    /// Validate every raw `--define` argument up-front, before any
    /// per-source loop runs. The resolver re-parses these inside its
    /// own pipeline, so the early check exists only to fail-fast
    /// with a stable exit code (2 — soft user error) and to avoid
    /// repeating the same error per spec in a batch.
    pub fn validate(
        &self,
    ) -> Result<(), rpm_spec_analyzer::profile::DefineParseError> {
        for raw in &self.raw {
            rpm_spec_analyzer::profile::parse_define(raw)?;
        }
        Ok(())
    }
}

impl Application {
    pub fn run(self) -> anyhow::Result<ExitCode> {
        let color = self.color;
        match self.command {
            Command::Lint(cmd) => cmd.run(color),
            Command::Format(cmd) => cmd.run(color),
            Command::Pretty(cmd) => cmd.run(color),
            Command::Ast(cmd) => cmd.run(),
            Command::Check(cmd) => cmd.run(color),
            Command::Profile(cmd) => cmd.run(color),
            Command::Target(cmd) => cmd.run(color),
            Command::Matrix(cmd) => cmd.run(color),
            Command::Lints(cmd) => cmd.run(color),
            Command::Completions(cmd) => cmd.run(),
        }
    }
}
