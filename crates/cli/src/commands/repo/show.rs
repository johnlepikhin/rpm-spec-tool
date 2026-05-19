//! `repo show` — summarise a cached repository snapshot.
//!
//! Default output: revision, fetched_at, package count, top-10
//! packages by installed size. `--full` dumps every package;
//! `--package NAME` zooms to one package; `--provides PAT`
//! grep-searches the Provides index.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{ProfileSection, ResolveOptions, resolve_profile};
use rpm_spec_repo_metadata::cache::{self, CacheDirs};

use super::{DEFAULT_PROFILE_NAME, RepoArgs};
use crate::commands::profile::style::Style;
use crate::commands::repo::url::interpolate_url;

#[derive(Debug, Args)]
pub struct ShowOpts {
    /// Profile to inspect (defaults to the active profile in
    /// `.rpmspec.toml`).
    #[arg(long = "profile", value_name = "NAME")]
    pub profile: Option<String>,

    /// Limit the dump to one repo id from the profile. When unset
    /// all of the profile's enabled repos are shown sequentially.
    #[arg(long = "repo", value_name = "ID")]
    pub repo: Option<String>,

    /// Full dump: list every package (default shows only the
    /// top-10 by installed size).
    #[arg(long)]
    pub full: bool,

    /// Show information for one specific package by name.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["full", "provides"])]
    pub package: Option<String>,

    /// Filter packages whose Provides contains this substring.
    #[arg(long, value_name = "PAT", conflicts_with_all = ["full", "package"])]
    pub provides: Option<String>,
}

pub fn run(
    opts: ShowOpts,
    repo_args: RepoArgs,
    config: &Config,
    base_dir: &Path,
    style: &Style,
) -> Result<ExitCode> {
    use rpm_spec_repo_metadata::http::NetMode;
    let _mode = repo_args.resolve_mode(NetMode::Offline);
    let cache_root = repo_args.resolve_cache_root()?;
    let dirs = CacheDirs::ensure(cache_root)?;

    let section = ProfileSection::new(config.profile.clone(), config.profiles.clone());
    let active = opts
        .profile
        .clone()
        .or_else(|| config.profile.clone())
        .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_string());
    let profile = resolve_profile(
        &section,
        base_dir,
        ResolveOptions::with_override(Some(active.as_str())),
    )?;

    let repos = match &profile.repos {
        Some(rs) => &rs.repos,
        None => {
            eprintln!("info: profile `{active}` has no repos configured");
            return Ok(ExitCode::SUCCESS);
        }
    };

    let mut stdout = std::io::stdout().lock();
    let mut printed_anything = false;

    for (repo_id, cfg) in repos {
        if let Some(filter) = &opts.repo {
            if filter != repo_id {
                continue;
            }
        }
        if !cfg.enabled {
            continue;
        }
        let Some(baseurl) = &cfg.baseurl else {
            continue;
        };
        let interpolated = match interpolate_url(baseurl, &profile) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("error: profile `{active}` repo `{repo_id}`: {e}");
                continue;
            }
        };

        let repo_dir = dirs.repo_dir(&interpolated);
        let current = repo_dir.join("current");
        if !current.exists() {
            writeln!(
                stdout,
                "{active}/{repo_id}: no cached snapshot — run `rpm-spec-tool repo sync --allow-fetch` first"
            )?;
            continue;
        }

        // Read manifest from the resolved snapshot to get the
        // revision, then load the parsed index through the cache
        // helper which applies the bincode size limit and reports
        // corruption uniformly.
        let manifest_path = current.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "reading snapshot manifest for {active}/{repo_id} at {}",
                manifest_path.display()
            )
        })?;
        let manifest: cache::SnapshotManifest =
            serde_json::from_str(&manifest_text).with_context(|| {
                format!(
                    "parsing snapshot manifest for {active}/{repo_id} at {}",
                    manifest_path.display()
                )
            })?;

        let index = match cache::try_load_snapshot(&dirs, &interpolated, &manifest.revision)
            .with_context(|| {
                format!(
                    "loading snapshot for {active}/{repo_id} at {}",
                    current.display()
                )
            })? {
            Some(idx) => idx,
            None => {
                writeln!(
                    stdout,
                    "{active}/{repo_id}: snapshot exists but index.bincode is missing or corrupt — re-run `rpm-spec-tool repo sync --allow-fetch`"
                )?;
                continue;
            }
        };

        printed_anything = true;
        writeln!(stdout)?;
        writeln!(
            stdout,
            "== {} ==",
            style.bold_cyan(&format!("{active} / {repo_id}"))
        )?;
        writeln!(stdout, "  kind:      {}", manifest.backend_kind)?;
        writeln!(stdout, "  url:       {interpolated}")?;
        writeln!(stdout, "  revision:  {}", index.revision)?;
        writeln!(stdout, "  fetched:   {}", manifest.fetched_at)?;
        writeln!(stdout, "  bytes:     {}", manifest.bytes_fetched)?;
        writeln!(stdout, "  packages:  {}", index.packages.len())?;

        if let Some(name) = &opts.package {
            let matches: Vec<_> = index
                .packages
                .iter()
                .filter(|p| p.nevra.name.as_ref() == name.as_str())
                .collect();
            if matches.is_empty() {
                writeln!(stdout, "  package `{name}` not found in this repo")?;
            } else {
                for p in matches {
                    writeln!(
                        stdout,
                        "  {} (size={}, location={})",
                        p.nevra, p.size_installed, p.location
                    )?;
                }
            }
        } else if let Some(pat) = &opts.provides {
            let matches: Vec<_> = index
                .packages
                .iter()
                .filter(|p| p.provides.iter().any(|cap| cap.name.contains(pat.as_str())))
                .take(50)
                .collect();
            writeln!(stdout, "  packages providing `{pat}` ({}):", matches.len())?;
            for p in matches {
                writeln!(stdout, "    {}", p.nevra)?;
            }
        } else if opts.full {
            for p in &index.packages {
                writeln!(stdout, "  {}", p.nevra)?;
            }
        } else {
            // Default: top-10 by installed size.
            let mut by_size: Vec<_> = index.packages.iter().collect();
            by_size.sort_by_key(|p| std::cmp::Reverse(p.size_installed));
            writeln!(stdout, "  top-10 packages by installed size:")?;
            for p in by_size.iter().take(10) {
                writeln!(stdout, "    {:>12}  {}", p.size_installed, p.nevra)?;
            }
        }
    }

    if !printed_anything {
        writeln!(stdout, "no matching cached snapshots for profile `{active}`")?;
    }

    Ok(ExitCode::SUCCESS)
}
