//! `repo show` — summarise a cached repository snapshot.
//!
//! Default output: kind, url, revision, fetched_at, package count,
//! top-10 packages by installed size. `--full` lists every package,
//! `--package NAME` zooms to one package, `--provides PAT`
//! substring-matches the Provides index.
//!
//! All queries hit the per-snapshot `repo.db` directly — no bincode
//! deserialisation, no full package materialisation in RAM.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{ProfileSection, ResolveOptions, resolve_profile};
use rpm_spec_repo_core::db::RepoDb;
use rpm_spec_repo_metadata::cache::CacheDirs;

use super::{DEFAULT_PROFILE_NAME, RepoArgs};
use crate::commands::profile::style::Style;
use crate::commands::repo::url::interpolate_url;

/// Default top-N package count for the no-filter view of `repo show` — keeps the human output one screen.
const TOP_N_DEFAULT: u32 = 10;
/// Cap on rows returned by `--provides` / `--provides-like` to keep the operator from drowning in a million-row wildcard hit; the renderer surfaces "capped at N; refine the pattern" when the limit hits.
const PROVIDES_LIMIT: u32 = 50;

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
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with_all = ["full", "provides", "provides_like", "file"],
    )]
    pub package: Option<String>,

    /// Find packages whose `Provides:` includes a capability with
    /// EXACTLY this name. Use for canonical lookups
    /// (`--provides cmake` returns the package literally providing
    /// the `cmake` capability, nothing else — no `cmake(Box2D)`
    /// virtuals, no `cmake-data` substring hits).
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with_all = ["full", "package", "provides_like", "file"],
    )]
    pub provides: Option<String>,

    /// Find packages whose `Provides:` includes any capability
    /// CONTAINING this substring. Useful for discovery
    /// (`--provides-like cmake` surfaces every `cmake(*)` virtual
    /// capability too). Capped at the first ~50 hits — refine the
    /// pattern when you see the cap notice.
    #[arg(
        long = "provides-like",
        value_name = "PAT",
        conflicts_with_all = ["full", "package", "provides", "file"],
    )]
    pub provides_like: Option<String>,

    /// Find the package that owns a specific file path (e.g.
    /// `--file /usr/bin/xsltproc`). Resolves through the per-repo
    /// `files` index — same lookup the resolver uses when a spec
    /// declares `Requires: /usr/bin/foo`-style file-path atoms.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["full", "package", "provides", "provides_like"],
    )]
    pub file: Option<String>,
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
        if let Some(filter) = &opts.repo
            && filter != repo_id
        {
            continue;
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
        let db_path = current.join(RepoDb::file_name());
        if !db_path.exists() {
            writeln!(
                stdout,
                "{active}/{repo_id}: snapshot exists but `repo.db` is missing (legacy bincode-only snapshot) — re-run `rpm-spec-tool repo sync --allow-fetch`"
            )?;
            continue;
        }
        let db = match RepoDb::open(&db_path) {
            Ok(d) => d,
            Err(e) => {
                writeln!(
                    stdout,
                    "{active}/{repo_id}: failed to open repo.db ({e}); re-run `rpm-spec-tool repo sync --allow-fetch`"
                )?;
                continue;
            }
        };

        let backend_kind = db
            .meta("backend_kind")
            .with_context(|| format!("reading backend_kind for {active}/{repo_id}"))?
            .unwrap_or_else(|| "unknown".to_string());
        let revision = db
            .revision()
            .with_context(|| format!("reading revision for {active}/{repo_id}"))?;
        let fetched_at = db
            .fetched_at()
            .with_context(|| format!("reading fetched_at for {active}/{repo_id}"))?;
        let package_count = db
            .package_count()
            .with_context(|| format!("counting packages in {active}/{repo_id}"))?;

        printed_anything = true;
        writeln!(stdout)?;
        writeln!(
            stdout,
            "== {} ==",
            style.bold_cyan(&format!("{active} / {repo_id}"))
        )?;
        writeln!(stdout, "  kind:      {backend_kind}")?;
        writeln!(stdout, "  url:       {interpolated}")?;
        writeln!(stdout, "  revision:  {revision}")?;
        writeln!(stdout, "  fetched:   {fetched_at}")?;
        writeln!(stdout, "  packages:  {package_count}")?;

        if let Some(name) = &opts.package {
            let matches = db
                .packages_by_name_brief(name)
                .with_context(|| format!("looking up `{name}` in {active}/{repo_id}"))?;
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
        } else if let Some(name) = &opts.provides {
            // EXACT match — the canonical "what owns `cmake`?"
            // query. Distinct from `--provides-like`: this won't
            // surface every `cmake(*)` virtual capability.
            let matches = db
                .packages_providing_exact(name, PROVIDES_LIMIT)
                .with_context(|| format!("exact provides `{name}` in {active}/{repo_id}"))?;
            writeln!(
                stdout,
                "  packages providing exactly `{name}` ({}):",
                matches.len(),
            )?;
            for (p, cap_name) in matches {
                writeln!(
                    stdout,
                    "    {} {}",
                    p.nevra,
                    style.dim(&format!("[{cap_name}]"))
                )?;
            }
        } else if let Some(pat) = &opts.provides_like {
            // `%{pat}%` substring match via SQL LIKE. The exact
            // form is passed separately so the SQL ORDER BY can
            // promote the canonical `c.name = pat` provider above
            // virtual capabilities (`cmake(Box2D)`) that merely
            // contain the pattern.
            let like = format!("%{}%", escape_like(pat));
            let matches = db
                .packages_providing_like(&like, pat, PROVIDES_LIMIT)
                .with_context(|| format!("scanning Provides in {active}/{repo_id}"))?;
            let suffix = if matches.len() >= PROVIDES_LIMIT as usize {
                format!(" — capped at {PROVIDES_LIMIT}; refine the pattern for the rest")
            } else {
                String::new()
            };
            writeln!(
                stdout,
                "  packages whose Provides contains `{pat}` ({}){suffix}:",
                matches.len(),
            )?;
            for (p, cap_name) in matches {
                writeln!(
                    stdout,
                    "    {} {}",
                    p.nevra,
                    style.dim(&format!("[{cap_name}]"))
                )?;
            }
        } else if let Some(path) = &opts.file {
            if path.is_empty() {
                eprintln!(
                    "error: --file requires a non-empty absolute path \
                     (e.g. `--file /usr/bin/xsltproc`)"
                );
                return Ok(ExitCode::from(2));
            }
            if !path.starts_with('/') {
                eprintln!(
                    "error: --file expects an absolute path \
                     (got `{path}`, must start with `/`)"
                );
                return Ok(ExitCode::from(2));
            }
            // Indexed `files` lookup — same path the resolver uses
            // for file-path `Requires:` atoms. Exact match only;
            // the per-package filelist isn't fuzzy-searchable
            // without scanning every row in the table.
            match db
                .file_owner(path)
                .with_context(|| format!("file owner lookup for `{path}` in {active}/{repo_id}"))?
            {
                Some(pkg_id) => {
                    if let Some(brief) = db
                        .package_brief(pkg_id)
                        .with_context(|| format!("loading owner brief for pkg_id={pkg_id}"))?
                    {
                        writeln!(
                            stdout,
                            "  `{path}` owned by:  {} (size={}, location={})",
                            brief.nevra, brief.size_installed, brief.location
                        )?;
                    } else {
                        writeln!(stdout, "  `{path}`: owner pkg_id={pkg_id} not found")?;
                    }
                }
                None => writeln!(
                    stdout,
                    "  `{path}` is not owned by any package in this repo"
                )?,
            }
        } else if opts.full {
            let all = db
                .all_packages_brief()
                .with_context(|| format!("listing packages in {active}/{repo_id}"))?;
            for p in all {
                writeln!(stdout, "  {}", p.nevra)?;
            }
        } else {
            let top = db
                .top_n_by_size(TOP_N_DEFAULT)
                .with_context(|| format!("computing top-{TOP_N_DEFAULT} for {active}/{repo_id}"))?;
            writeln!(stdout, "  top-{TOP_N_DEFAULT} packages by installed size:")?;
            for p in top {
                writeln!(stdout, "    {:>12}  {}", p.size_installed, p.nevra)?;
            }
        }
    }

    if !printed_anything {
        writeln!(
            stdout,
            "no matching cached snapshots for profile `{active}`"
        )?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Escape a user-supplied substring so it's safe to splice into a
/// `LIKE ?1 ESCAPE '\'` parameter. Without this, `lib_foo` would
/// match `lib1foo`/`libXfoo` (the `_` LIKE wildcard) and `100%` would
/// match anything starting with `100`.
///
/// `\\` MUST run first — otherwise the backslashes we insert in the
/// subsequent `%` and `_` replacements would themselves get
/// double-escaped, leaving the literal sequence `\\%` (matched as
/// "backslash followed by anything") instead of `\%` (literal `%`).
fn escape_like(pat: &str) -> String {
    pat.replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_")
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn escape_like_passes_through_safe_input() {
        assert_eq!(escape_like("cmake"), "cmake");
        assert_eq!(escape_like("pkgconfig(foo)"), "pkgconfig(foo)");
    }

    #[test]
    fn escape_like_quotes_wildcards() {
        assert_eq!(escape_like("100%"), r"100\%");
        assert_eq!(escape_like("lib_foo"), r"lib\_foo");
        assert_eq!(escape_like("a%b_c"), r"a\%b\_c");
    }

    #[test]
    fn escape_like_quotes_backslash_first() {
        // `\\` must run before `%` / `_` — otherwise we'd see `\\%`
        // (literal backslash then any-string) instead of `\%`
        // (literal percent).
        assert_eq!(escape_like(r"foo\bar"), r"foo\\bar");
        assert_eq!(escape_like(r"a\%"), r"a\\\%");
    }
}
