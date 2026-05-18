//! `matrix` subcommand — multi-profile (release matrix) analysis.
//!
//! Phase 1 shipped `matrix check` + `matrix baseline create`. Phase 2
//! added `matrix portability` and `matrix coverage`. Phase 3 adds
//! `matrix explain` for line-specific and macro-specific introspection.
//! diff / impact remain follow-ups (see `doc/matrix.md` "Limitations").

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{
    ProfileSection, ResolveOptions, ResolvedTargetSet, TargetEntry, resolve_target_set,
};

pub mod baseline;
pub mod check;
pub mod coverage;
pub mod explain;
pub mod portability;

pub use check::{AD_HOC_TARGET_SET_ID, CheckOpts};

/// Resolve a matrix source — either a config-defined `[targets.<name>]`
/// set or an ad-hoc `--profiles a,b,c` list — into one
/// [`ResolvedTargetSet`]. Shared by every `matrix` subcommand so the
/// resolution policy and observability events stay consistent.
///
/// `target_set` and `profiles` correspond directly to the clap-parsed
/// CLI flags; the caller is responsible for the `ArgGroup` invariant
/// that exactly one of them is set.
pub(crate) fn resolve_matrix_source(
    config: &Config,
    base_dir: &Path,
    target_set: Option<&str>,
    profiles: &[String],
    defines: &[String],
) -> Result<ResolvedTargetSet> {
    let section = ProfileSection::new(config.profile.clone(), config.profiles.clone());
    let resolve_opts = ResolveOptions::default().with_defines(defines);

    if let Some(name) = target_set {
        tracing::info!(branch = "target_set", name = %name, "resolving matrix");
        let target = config
            .targets
            .get(name)
            .with_context(|| format!("target set `{name}` is not defined in .rpmspec.toml"))?;
        resolve_target_set(&section, name, target, base_dir, resolve_opts)
            .with_context(|| format!("failed to resolve target set `{name}`"))
    } else {
        tracing::info!(
            branch = "ad-hoc",
            profiles = ?profiles,
            "resolving matrix from CLI --profiles"
        );
        let target = TargetEntry::from_profiles(profiles.to_vec());
        resolve_target_set(&section, AD_HOC_TARGET_SET_ID, &target, base_dir, resolve_opts)
            .with_context(|| "failed to resolve ad-hoc target set from --profiles")
    }
}

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
    /// Report which macros referenced by the spec are defined on
    /// which member profiles. Surfaces platform-specific macros
    /// that often need `%{?guard}` wrappers.
    Portability(portability::PortabilityOpts),
    /// For each `%if` / `%ifarch` branch in the spec, list which
    /// member profiles activate it. Surfaces dead branches and
    /// distro-only branches.
    Coverage(coverage::CoverageOpts),
    /// Explain why a specific spec line is active on some profiles
    /// and inactive on others, or report the per-profile value of a
    /// named macro.
    Explain(explain::ExplainOpts),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        match self.action {
            Action::Check(opts) => check::run(opts, self.config.as_deref(), color),
            Action::Baseline(cmd) => cmd.run(self.config.as_deref()),
            Action::Portability(opts) => portability::run(opts, self.config.as_deref()),
            Action::Coverage(opts) => coverage::run(opts, self.config.as_deref()),
            Action::Explain(opts) => explain::run(opts, self.config.as_deref()),
        }
    }
}
