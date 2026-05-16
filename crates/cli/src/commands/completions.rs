//! `completions` subcommand — emit a shell completion script.
//!
//! The script is written to stdout so users can pipe it into the
//! correct directory for their shell. See `--help` for per-shell
//! installation hints.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, CommandFactory};
use clap_complete::{Shell, generate};

#[derive(Debug, Args)]
#[command(after_help = "\
Installation examples:
  # Bash (system-wide):
  rpm-spec-tool completions bash | sudo tee /etc/bash_completion.d/rpm-spec-tool > /dev/null

  # Zsh (per-user; ensure the target dir is on $fpath):
  rpm-spec-tool completions zsh  > ~/.zsh/completions/_rpm-spec-tool

  # Fish (per-user):
  rpm-spec-tool completions fish > ~/.config/fish/completions/rpm-spec-tool.fish

  # PowerShell:
  rpm-spec-tool completions powershell >> $PROFILE")]
pub struct Cmd {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: Shell,
}

impl Cmd {
    pub fn run(self) -> Result<ExitCode> {
        let mut cmd = crate::app::Application::command();
        let bin = cmd.get_name().to_string();
        generate(self.shell, &mut cmd, bin, &mut std::io::stdout());
        Ok(ExitCode::SUCCESS)
    }
}
