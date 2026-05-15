//! `lint` subcommand.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};
use rpm_spec_analyzer::{Diagnostic, LintSession, Severity, parse};

use crate::app::ColorChoice;
use crate::config as cli_config;
use crate::io;
use crate::output;
use crate::fixer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    Human,
    Json,
    Sarif,
}

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    /// Output format for the diagnostics.
    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    /// Override the configured severity to `deny` for the named lint.
    /// Repeatable.
    #[arg(long = "deny", value_name = "LINT")]
    pub deny: Vec<String>,

    /// Override the configured severity to `warn` for the named lint.
    /// Repeatable.
    #[arg(long = "warn", value_name = "LINT")]
    pub warn: Vec<String>,

    /// Override the configured severity to `allow` for the named lint.
    /// Repeatable.
    #[arg(long = "allow", value_name = "LINT")]
    pub allow: Vec<String>,

    /// Apply machine-applicable fixes back to the source file.
    #[arg(long)]
    pub fix: bool,

    /// Also apply suggestion-grade (maybe-incorrect) fixes when `--fix` is set.
    #[arg(long)]
    pub fix_suggested: bool,
}

impl Cmd {
    pub fn run(self, color: ColorChoice) -> Result<ExitCode> {
        let sources = io::read_sources(&self.input.paths)?;

        let mut any_deny = false;
        let mut any_diagnostic = false;
        let mut all_diagnostics: Vec<(io::Source, Vec<Diagnostic>)> = Vec::new();

        for mut source in sources {
            let cfg_path = self.input.config.as_deref();
            let mut config = cli_config::load(cfg_path, Some(&source.path))?;
            apply_overrides(&mut config, &self.deny, Severity::Deny);
            apply_overrides(&mut config, &self.warn, Severity::Warn);
            apply_overrides(&mut config, &self.allow, Severity::Allow);

            if self.fix {
                let level = if self.fix_suggested {
                    fixer::FixLevel::Suggested
                } else {
                    fixer::FixLevel::Safe
                };
                let report = fixer::fix_in_place(&mut source, &config, level)?;
                if report.applied > 0 && !source.is_stdin {
                    std::fs::write(&source.path, &source.contents)?;
                }
                // Re-run after fixing to surface remaining diagnostics.
                let outcome = parse(&source.contents);
                let mut session = LintSession::from_config(&config);
                let diags = session.run(&outcome.spec);
                any_deny |= diags.iter().any(|d| d.severity == Severity::Deny);
                any_diagnostic |= !diags.is_empty();
                all_diagnostics.push((source, diags));
            } else {
                let outcome = parse(&source.contents);
                let mut session = LintSession::from_config(&config);
                let diags = session.run(&outcome.spec);
                any_deny |= diags.iter().any(|d| d.severity == Severity::Deny);
                any_diagnostic |= !diags.is_empty();
                all_diagnostics.push((source, diags));
            }
        }

        match self.format {
            OutputFormat::Human => output::human::render(&all_diagnostics, color)?,
            OutputFormat::Json => output::json::render(&all_diagnostics)?,
            OutputFormat::Sarif => output::sarif::render(&all_diagnostics)?,
        }

        let exit = if any_deny {
            ExitCode::from(1)
        } else if any_diagnostic {
            // Warnings: success exit, but the user has been notified.
            ExitCode::SUCCESS
        } else {
            ExitCode::SUCCESS
        };
        Ok(exit)
    }
}

fn apply_overrides(
    config: &mut rpm_spec_analyzer::config::Config,
    names: &[String],
    severity: Severity,
) {
    for n in names {
        config.lints.insert(n.clone(), severity);
    }
}
