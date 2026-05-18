//! `target` subcommand — inspect release target sets defined in
//! `[targets.<name>]` of `.rpmspec.toml`.
//!
//! Modes:
//! * `target list` — tabular listing of every target set in the config.
//! * `target show <NAME>` — resolve a target set and pretty-print
//!   its member profiles with effective defines.
//!
//! ## Exit codes
//!
//! * `0` — success.
//! * `2` — soft user error: unknown target name in `show`.
//! * `1` — anyhow-bubbled error (config parse, profile resolution).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rpm_spec_analyzer::profile::{ResolveOptions, resolve_target_set};

mod list;
mod show;

pub use list::ListOpts;
pub use show::ShowOpts;

#[derive(Debug, Args)]
pub struct Cmd {
    /// Explicit path to `.rpmspec.toml`. Without this flag the nearest
    /// `.rpmspec.toml` walking upward from the current directory is used.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// List every `[targets.<name>]` set in the loaded config.
    List(ListOpts),
    /// Resolve one target set and pretty-print its member profiles.
    Show(ShowOpts),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        let (config, base_dir) = crate::commands::config_loader::load_config(self.config.as_deref())?;
        let style = crate::commands::profile::style::Style::new(color);
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        match self.action {
            Action::List(opts) => {
                let _span = tracing::debug_span!("target_list").entered();
                list::render_list(&mut out, &config, opts, &style)
            }
            Action::Show(opts) => {
                let _span = tracing::info_span!("target_show", name = %opts.name).entered();
                let target = match config.targets.get(&opts.name) {
                    Some(t) => t,
                    None => {
                        eprintln!(
                            "error: target set `{}` is not defined in .rpmspec.toml",
                            opts.name
                        );
                        return Ok(ExitCode::from(2));
                    }
                };
                let section = rpm_spec_analyzer::profile::ProfileSection::new(
                    config.profile.clone(),
                    config.profiles.clone(),
                );
                let resolved = resolve_target_set(
                    &section,
                    &opts.name,
                    target,
                    &base_dir,
                    ResolveOptions::default().with_defines(&opts.defines.raw),
                )
                .with_context(|| format!("failed to resolve target set `{}`", opts.name))?;
                show::render(&mut out, &resolved, target, &style)?;
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

