//! Shared cache → `RepoUniverse` loader for the matrix repo-aware
//! commands (`matrix deps check`, `matrix buildroot solve`, future
//! `matrix runtime-deps`, `matrix upgrade-sim`, etc).
//!
//! Lives at the matrix-module level rather than inside a specific
//! command so adding a new repo-aware subcommand is a one-line
//! `use super::universe::build_universe_from_cache;` away.

use std::sync::Arc;

use anyhow::{Context, Result};

use rpm_spec_analyzer::profile::Profile;
use rpm_spec_repo_core::RepoUniverse;
use rpm_spec_repo_core::db::RepoDb;
use rpm_spec_repo_metadata::cache::CacheDirs;

/// Open every enabled, baseurl-carrying repo from `profile`'s
/// `[profiles.X.repos.*]` block, build a `RepoUniverse`. Returns
/// `Ok(None)` when no repo has a usable `repo.db` — caller surfaces
/// a one-time INFO note and skips repo-aware lint passes.
///
/// # Errors
///
/// Returns `Err` when:
/// - `$basearch` / `$arch` / `$releasever` interpolation fails for
///   a repo's `baseurl` (typically SSRF-guard rejection — a
///   configuration error, not a runtime glitch).
/// - `RepoUniverse::from_dbs` fails to seed its repo-id index
///   (corrupt `meta` table in one of the opened DBs).
///
/// Missing `current` symlinks, missing `repo.db` files, and
/// per-DB open errors are logged at `debug`/`warn` and skipped —
/// callers see a smaller universe but no propagated error.
pub(super) fn build_universe_from_cache(
    profile: &Profile,
    dirs: &CacheDirs,
) -> Result<Option<Arc<RepoUniverse>>> {
    let Some(repo_set) = &profile.repos else {
        return Ok(None);
    };
    if repo_set.repos.is_empty() {
        return Ok(None);
    }
    let mut dbs = Vec::new();
    for (repo_id, cfg) in &repo_set.repos {
        if !cfg.enabled {
            tracing::debug!(
                repo = ?repo_id,
                profile = ?profile.identity.name,
                "repo disabled in profile config; skipping"
            );
            continue;
        }
        let Some(baseurl) = &cfg.baseurl else {
            tracing::debug!(
                repo = ?repo_id,
                profile = ?profile.identity.name,
                "no baseurl configured; skipping"
            );
            continue;
        };
        let interpolated = crate::commands::repo::url::interpolate_url(baseurl, profile)
            .map_err(|e| anyhow::anyhow!(e))
            .with_context(|| {
                format!(
                    "interpolating baseurl for repo `{repo_id}` in profile `{}`",
                    profile.identity.name
                )
            })?;
        let repo_dir = dirs.repo_dir(&interpolated);
        let current = repo_dir.join("current");
        if !current.exists() {
            tracing::debug!(repo = ?repo_id, "no cached snapshot; skipping repo");
            continue;
        }
        let db_path = current.join(RepoDb::file_name());
        if !db_path.exists() {
            tracing::debug!(
                repo = ?repo_id,
                path = %db_path.display(),
                "cached snapshot present but repo.db missing; run `repo sync` to upgrade",
            );
            continue;
        }
        match RepoDb::open(&db_path) {
            Ok(db) => dbs.push(db),
            Err(e) => {
                tracing::warn!(
                    repo = ?repo_id,
                    error = %e,
                    path = %db_path.display(),
                    "failed to open repo.db; skipping",
                );
                continue;
            }
        }
    }
    if dbs.is_empty() {
        return Ok(None);
    }
    // Pin the universe's arch filter to the profile's build_arch +
    // noarch. The resolver, file_owner, and binaries_built_from
    // queries all respect it — without this an x86_64 profile would
    // happily pin i686 multilib binaries from the same repo (correct
    // for `dnf install` only because dnf has its own arch filter;
    // the matrix solver has to do its own).
    let mut acceptable_archs: Vec<String> = Vec::with_capacity(2);
    if let Some(arch) = profile.arch.build_arch.as_deref() {
        acceptable_archs.push(arch.to_string());
    }
    acceptable_archs.push("noarch".to_string());
    let universe =
        RepoUniverse::from_dbs_with_arch(profile.identity.name.clone(), dbs, acceptable_archs)
            .context("building per-profile RepoUniverse")?;
    Ok(Some(Arc::new(universe)))
}

/// Build the per-profile universe cache so `(spec × profile)`
/// loops don't re-open DBs per spec. Returns a `HashMap` keyed by
/// `profile.identity.name`.
///
/// Takes `&[&Profile]` rather than the heavier
/// `rpm_spec_profile::ResolvedTarget` so callers from other matrix
/// subcommands (which may have their own resolved-target shapes)
/// don't have to construct one just to satisfy the loader.
pub(super) fn cache_universes(
    profiles: &[&Profile],
    dirs: &CacheDirs,
) -> Result<std::collections::HashMap<String, Option<Arc<RepoUniverse>>>> {
    use std::collections::HashMap;
    use std::collections::hash_map::Entry;
    let mut universe_cache: HashMap<String, Option<Arc<RepoUniverse>>> = HashMap::new();
    for profile in profiles {
        let profile_name = profile.identity.name.clone();
        if let Entry::Vacant(slot) = universe_cache.entry(profile_name) {
            let universe = build_universe_from_cache(profile, dirs)?;
            slot.insert(universe);
        }
    }
    Ok(universe_cache)
}
