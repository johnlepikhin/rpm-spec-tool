//! `lint` subcommand.

use std::borrow::Cow;
use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::{Diagnostic, Severity, analyze};

use crate::app::ColorChoice;
use crate::config as cli_config;
use crate::fixer;
use crate::io;
use crate::output;

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
        let mut config_cache = cli_config::ConfigCache::new(self.input.config.clone());

        let mut any_deny = false;
        let mut any_io_error = false;
        let mut all_diagnostics: Vec<(io::Source, Vec<Diagnostic>)> = Vec::new();

        let has_overrides =
            !self.deny.is_empty() || !self.warn.is_empty() || !self.allow.is_empty();

        for mut source in sources {
            let Some(cached) =
                config_cache.load_or_report(&source.path, &mut any_io_error)
            else {
                continue;
            };
            // Only pay the clone when CLI overrides actually require
            // mutation; otherwise share the cached `Arc<Config>` by
            // borrowing read-only.
            let config: Cow<'_, Config> = if has_overrides {
                let mut c: Config = (*cached).clone();
                c.apply_cli_overrides(&self.allow, &self.warn, &self.deny);
                Cow::Owned(c)
            } else {
                Cow::Borrowed(&cached)
            };
            let config: &Config = &config;

            if self.fix {
                let level = if self.fix_suggested {
                    fixer::FixLevel::Suggested
                } else {
                    fixer::FixLevel::Safe
                };
                // `fix_in_place` already emits a `tracing::warn!` when it
                // saturates — no need to surface it again at the CLI level.
                let report = match fixer::fix_in_place(&mut source, config, level) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error fixing {}: {e:#}", source.display_name());
                        any_io_error = true;
                        continue;
                    }
                };
                if report.applied > 0 {
                    if source.is_stdin {
                        // Without this, the fixed text would be silently lost
                        // — the user has no file to write back to. Lock and
                        // flush so a `... | rpm-spec-tool --fix | tee` pipe
                        // gets the bytes before the process exits.
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        if let Err(e) = out
                            .write_all(source.contents.as_bytes())
                            .and_then(|()| out.flush())
                        {
                            // `head` / `less` closing the pipe is a normal
                            // termination signal, not an error — exit codes
                            // shouldn't flip just because the reader walked
                            // away early.
                            if e.kind() == std::io::ErrorKind::BrokenPipe {
                                tracing::debug!(
                                    path = %source.display_name(),
                                    "broken pipe on stdout; downstream consumer closed early"
                                );
                            } else {
                                eprintln!("error writing fixed stdin: {e:#}");
                                any_io_error = true;
                                continue;
                            }
                        }
                    } else if let Err(e) =
                        io::write_atomic(&source.path, &source.contents)
                    {
                        eprintln!("error writing {}: {e:#}", source.display_name());
                        any_io_error = true;
                        continue;
                    }
                }
            }

            let (_outcome, diags) = analyze(&source.contents, config);
            any_deny |= diags.iter().any(|d| d.severity == Severity::Deny);
            all_diagnostics.push((source, diags));
        }

        match self.format {
            OutputFormat::Human => output::human::render(&all_diagnostics, color)?,
            OutputFormat::Json => output::json::render(&all_diagnostics)?,
            OutputFormat::Sarif => output::sarif::render(&all_diagnostics)?,
        }

        Ok(if any_io_error {
            ExitCode::from(2)
        } else if any_deny {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }
}
