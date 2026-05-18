//! `check` subcommand — lint + format --check rolled into one CI invocation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Result;
use clap::Args;
use rpm_spec::printer::print_with;
use rpm_spec_analyzer::profile::Profile;
use rpm_spec_analyzer::{Severity, analyze_with_profile_at};

use crate::app::ColorChoice;
use crate::config::{self as cli_config, ConfigCacheCliExt as _};
use crate::io;
use crate::output;

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    /// Override the active distribution profile. Wins over the
    /// `profile = …` key in `.rpmspec.toml`.
    #[arg(long = "profile", value_name = "NAME")]
    pub profile: Option<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

impl Cmd {
    pub fn run(self, color: ColorChoice) -> Result<ExitCode> {
        // Same fail-fast contract as `lint`: bad `--define` shouldn't
        // print one error per spec in a batch.
        if let Err(e) = self.defines.validate() {
            eprintln!("error: {e}");
            return Ok(ExitCode::from(2));
        }

        let sources = io::read_sources(&self.input.paths)?;
        let mut config_cache = cli_config::ConfigCache::new(self.input.config.clone());

        let mut any_failure = false;
        let mut any_io_error = false;
        let mut all_diagnostics = Vec::new();

        // Profile resolution is memoised by `base_dir` so a batch with a
        // shared `.rpmspec.toml` reparses showrc only once. Mirrors the
        // approach used by the `lint` subcommand.
        let mut profile_cache: HashMap<PathBuf, Arc<Profile>> = HashMap::new();

        for source in sources {
            let Some((analyzer_cfg, base_dir)) =
                config_cache.load_with_base_dir_or_report(&source.path, &mut any_io_error)
            else {
                continue;
            };

            let profile = match profile_cache.get(&base_dir) {
                Some(p) => Arc::clone(p),
                None => {
                    let resolved = match analyzer_cfg.resolve_profile(
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
            let (outcome, diags) = analyze_with_profile_at(
                &source.contents,
                source_path,
                &analyzer_cfg,
                (*profile).clone(),
            );

            if diags.iter().any(|d| d.severity == Severity::Deny) {
                any_failure = true;
            }

            let pcfg = analyzer_cfg.format.to_printer_config();
            let formatted = print_with(&outcome.spec, &pcfg);
            if formatted != source.contents {
                eprintln!("would reformat: {}", source.display_name());
                any_failure = true;
            }

            all_diagnostics.push((source, diags));
        }

        output::human::render(&all_diagnostics, color)?;
        Ok(if any_io_error {
            ExitCode::from(2)
        } else if any_failure {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }
}

