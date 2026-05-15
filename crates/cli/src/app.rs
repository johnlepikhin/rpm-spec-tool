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

impl Application {
    pub fn run(self) -> anyhow::Result<ExitCode> {
        let color = self.color;
        match self.command {
            Command::Lint(cmd) => cmd.run(color),
            Command::Format(cmd) => cmd.run(color),
            Command::Pretty(cmd) => cmd.run(color),
            Command::Ast(cmd) => cmd.run(),
            Command::Check(cmd) => cmd.run(color),
        }
    }
}
