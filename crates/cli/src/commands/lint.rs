//! `lint` subcommand.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Result;
use clap::{Args, ValueEnum};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::Profile;
use rpm_spec_analyzer::{Diagnostic, Severity, analyze_with_profile_at};

use crate::app::ColorChoice;
use crate::config::{self as cli_config, ConfigCacheCliExt as _};
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
    /// Repeatable. The special name `warnings` (clippy convention)
    /// promotes every `warn`-level rule to `deny` — `--deny warnings`
    /// makes lint exit non-zero on any warning while still allowing
    /// `--allow LINT` to silence specific rules individually.
    #[arg(long = "deny", value_name = "LINT")]
    pub deny: Vec<String>,

    /// Override the configured severity to `warn` for the named lint.
    /// Repeatable.
    #[arg(long = "warn", value_name = "LINT")]
    pub warn: Vec<String>,

    /// Override the configured severity to `allow` for the named lint.
    /// Repeatable. The special name `warnings` clears any earlier
    /// `--deny warnings` promotion.
    #[arg(long = "allow", value_name = "LINT")]
    pub allow: Vec<String>,

    /// Apply machine-applicable fixes back to the source file.
    #[arg(long)]
    pub fix: bool,

    /// Also apply suggestion-grade (maybe-incorrect) fixes when `--fix` is set.
    #[arg(long)]
    pub fix_suggested: bool,

    /// Override the active distribution profile. Wins over the
    /// `profile = …` key in `.rpmspec.toml`.
    #[arg(long = "profile", value_name = "NAME")]
    pub profile: Option<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

impl Cmd {
    pub fn run(self, color: ColorChoice) -> Result<ExitCode> {
        // Validate `--define` args once, before opening any file or
        // touching the profile cache. Otherwise the same parse error
        // would be repeated per spec in a batch (resolve happens
        // inside the per-source loop), drowning the user in noise and
        // delaying the fix-the-typo signal.
        if let Err(e) = self.defines.validate() {
            eprintln!("error: {e}");
            return Ok(ExitCode::from(2));
        }

        let sources = io::read_sources(&self.input.paths)?;
        let mut config_cache = cli_config::ConfigCache::new(self.input.config.clone());

        let mut any_deny = false;
        let mut any_io_error = false;
        let mut all_diagnostics: Vec<(io::Source, Vec<Diagnostic>)> = Vec::new();

        let has_overrides =
            !self.deny.is_empty() || !self.warn.is_empty() || !self.allow.is_empty();

        // Profile resolution is memoised by `(base_dir, cli_override)`
        // — profiles depend only on the config and `--profile` flag, not
        // on the spec source. Resolving once per `(config, base_dir)`
        // saves N parses of a 700+-entry showrc dump for batches that
        // share a config.
        let mut profile_cache: HashMap<PathBuf, Arc<Profile>> = HashMap::new();

        for mut source in sources {
            let Some((cached, base_dir)) =
                config_cache.load_with_base_dir_or_report(&source.path, &mut any_io_error)
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
                        eprintln!("error: fix failed for {}: {e:#}", source.display_name());
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
                                eprintln!("error: failed to write fixed stdin: {e:#}");
                                any_io_error = true;
                                continue;
                            }
                        }
                    } else if let Err(e) = io::write_atomic(&source.path, &source.contents) {
                        eprintln!("error: failed to write {}: {e:#}", source.display_name());
                        any_io_error = true;
                        continue;
                    }
                }
            }

            // Resolve the profile relative to the directory the
            // `.rpmspec.toml` was discovered in, so `showrc-file =
            // "vendor/..."` paths are interpreted consistently
            // regardless of where the lint command was launched from.
            // Memoised per `base_dir` so we don't reparse showrc once
            // per spec in a batch.
            let profile = match profile_cache.get(&base_dir) {
                Some(p) => Arc::clone(p),
                None => {
                    let resolved = match config.resolve_profile(
                        &base_dir,
                        rpm_spec_analyzer::profile::ResolveOptions::with_override(
                            self.profile.as_deref(),
                        )
                        .with_defines(&self.defines.raw),
                    ) {
                        Ok(p) => Arc::new(p),
                        Err(e) => {
                            eprintln!(
                                "error: failed to resolve profile (base_dir={}): {e:#}",
                                base_dir.display()
                            );
                            any_io_error = true;
                            continue;
                        }
                    };
                    profile_cache.insert(base_dir.clone(), Arc::clone(&resolved));
                    resolved
                }
            };

            let source_path = if source.is_stdin {
                None
            } else {
                Some(source.path.as_path())
            };
            let (_outcome, diags) =
                analyze_with_profile_at(&source.contents, source_path, config, (*profile).clone());
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

