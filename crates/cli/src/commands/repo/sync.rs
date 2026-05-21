//! `repo sync` — fetch and cache metadata for one or more profiles.
//!
//! M1: rpm-md only. Apt-rpm is wired in M3 (PR 6). Parallel fetch
//! lands in M3.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{Profile, ProfileSection, ResolveOptions, resolve_profile};
use rpm_spec_repo_metadata::backend::{RepoBackend, detect_backend};
use rpm_spec_repo_metadata::cache::{self, CacheDirs};
use rpm_spec_repo_metadata::http::{HttpCache, NetMode};
use rpm_spec_repo_metadata::locks;

use super::{DEFAULT_PROFILE_NAME, RepoArgs};
use crate::commands::profile::style::Style;

#[derive(Debug, Args)]
pub struct SyncOpts {
    /// Profile to sync. Repeatable; mutually exclusive with
    /// `--all-profiles` and `--target-set`. When none are given the
    /// active profile from `.rpmspec.toml` is used.
    #[arg(long = "profile", value_name = "NAME")]
    pub profiles: Vec<String>,

    /// Sync every profile declared in `[profiles.X]` of the loaded
    /// config. Useful for warming the cache in CI.
    #[arg(long, conflicts_with_all = ["profiles", "target_set"])]
    pub all_profiles: bool,

    /// Sync the profiles named by a `[targets.<id>]` set. Mirrors
    /// the existing `matrix check --target-set` selector.
    #[arg(long = "target-set", value_name = "ID", conflicts_with_all = ["profiles", "all_profiles"])]
    pub target_set: Option<String>,

    /// Don't exit with code 1 on per-repo failures. Each error is still
    /// printed to stderr, but the command exits 0 as long as the run
    /// itself reached the end (no setup failure). Useful when a few
    /// repos in a large target set are temporarily unreachable and you
    /// want the rest to land.
    #[arg(long)]
    pub keep_going: bool,
}

pub fn run(
    opts: SyncOpts,
    repo_args: RepoArgs,
    config: &Config,
    base_dir: &Path,
    _style: &Style,
) -> Result<ExitCode> {
    let mode = repo_args.resolve_mode(NetMode::Online);
    if !matches!(mode, NetMode::Online) {
        eprintln!(
            "error: `repo sync` needs network access — pass `--allow-fetch` to enable fetching"
        );
        return Ok(ExitCode::from(2));
    }

    let cache_root = repo_args.resolve_cache_root()?;
    let dirs = CacheDirs::ensure(cache_root.clone())?;
    let http = HttpCache::new_with_tls(cache_root.clone(), mode, !repo_args.insecure_tls)?;

    let targets = pick_profiles(&opts, config, base_dir)?;
    if targets.is_empty() {
        eprintln!(
            "info: no profiles selected (none of the chosen profiles declare a `[profiles.X.repos.*]` block)"
        );
        return Ok(ExitCode::SUCCESS);
    }

    let mut had_error = false;

    for (profile_name, profile) in targets {
        let repos = match &profile.repos {
            Some(rs) => &rs.repos,
            None => continue,
        };

        for (repo_id, repo_cfg) in repos {
            if !repo_cfg.enabled {
                tracing::debug!(repo = ?repo_id, "repo disabled — skipping");
                continue;
            }
            let Some(baseurl) = &repo_cfg.baseurl else {
                eprintln!(
                    "warn: profile `{profile_name}` repo `{repo_id}` has no baseurl — skipping"
                );
                continue;
            };

            let interpolated = match crate::commands::repo::url::interpolate_url(baseurl, &profile)
            {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("error: profile `{profile_name}` repo `{repo_id}`: {e}");
                    had_error = true;
                    continue;
                }
            };

            let span = tracing::info_span!(
                "repo.sync",
                profile = ?profile_name,
                repo = ?repo_id,
                url = ?interpolated,
            );
            let _g = span.enter();

            // Per-repo exclusive lock — concurrent processes
            // serialise here.
            let repo_dir = dirs.repo_dir(&interpolated);
            let _lock = locks::acquire(&repo_dir)?;

            // `detect_backend` still takes the full RepoConfig because
            // future auto-sniff variants will inspect the URL too. The
            // backend trait, however, only needs the resolved
            // baseurl — that's all the wire protocol cares about, and
            // it keeps the trait narrow + testable.
            let backend: Box<dyn RepoBackend> = detect_backend(repo_cfg)?;
            eprintln!(
                "syncing {profile_name}/{repo_id} ({}) {interpolated}",
                backend.kind().as_str()
            );

            let rev = match backend.fetch_revision(&http, &interpolated) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {profile_name}/{repo_id}: {e}");
                    had_error = true;
                    continue;
                }
            };

            // Pass the TOML repo id (logical name like `baseos`) as the
            // stable identifier in the returned `RepoIndex.repo_id` so
            // diagnostics attribute findings to a human-friendly name
            // rather than the resolved URL.
            let repo_id_typed = rpm_spec_repo_core::RepoId::from(repo_id.as_str());
            let index = match backend.fetch_index(&http, &interpolated, &rev, &repo_id_typed) {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("error: {profile_name}/{repo_id} index: {e}");
                    had_error = true;
                    continue;
                }
            };

            let snap_dir = cache::write_snapshot(
                &dirs,
                &interpolated,
                backend.kind(),
                &index,
                rev.raw_bytes.len() as u64,
            )?;

            eprintln!(
                "  → revision {}, {} packages, snapshot at {}",
                index.revision,
                index.packages.len(),
                snap_dir.display(),
            );
        }
    }

    if had_error {
        if opts.keep_going {
            eprintln!("warning: some repos failed; exit 0 because --keep-going was set");
            Ok(ExitCode::SUCCESS)
        } else {
            Ok(ExitCode::from(1))
        }
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn pick_profiles(
    opts: &SyncOpts,
    config: &Config,
    base_dir: &Path,
) -> Result<Vec<(String, Profile)>> {
    let section = ProfileSection::new(config.profile.clone(), config.profiles.clone());

    let mut names: Vec<String> = if !opts.profiles.is_empty() {
        opts.profiles.clone()
    } else if opts.all_profiles {
        config.profiles.keys().cloned().collect()
    } else if let Some(id) = &opts.target_set {
        let target = config.targets.get(id).ok_or_else(|| {
            anyhow::anyhow!(
                "target set `{id}` is not defined in the loaded config (`[targets.{id}]`)"
            )
        })?;
        target.profiles.clone()
    } else {
        // Fall back to the active profile from config (or built-in
        // default). `resolve_profile` with no override does the
        // right thing — but we need a name for reporting.
        vec![
            config
                .profile
                .clone()
                .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_string()),
        ]
    };
    names.sort();
    names.dedup();

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let opts = ResolveOptions::with_override(Some(name.as_str()));
        let profile = resolve_profile(&section, base_dir, opts)
            .with_context(|| format!("resolving profile `{name}`"))?;
        out.push((name, profile));
    }
    Ok(out)
}
