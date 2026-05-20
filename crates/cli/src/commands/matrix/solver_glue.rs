//! Shared scaffolding for the three matrix subcommands that drive
//! [`rpm_spec_repo_resolver::solve`].
//!
//! `matrix buildroot solve`, `matrix buildroot diff` and `matrix
//! deps explain` all do the same first three steps: parse the spec,
//! pull the active `BuildRequires:` set (conditional-aware), project
//! the profile's `buildroot.base_packages` / `implicit_buildrequires`
//! into resolver shape, and invoke `solve()` with the result. Until
//! now each command had its own private copy of that prelude — when
//! the resolver gained a `SolveRequest` field in iteration 2 the
//! call sites had to be updated in lock-step or one would silently
//! lag.
//!
//! This module consolidates the prelude into [`solve_for`] and the
//! commonly-needed unsat-core dedup into [`dedup_unsat_core`]. Each
//! subcommand keeps its own *rendering* but stops re-inventing the
//! resolver wiring.

use std::collections::HashSet;
#[cfg(test)]
use std::sync::Arc;

use anyhow::{Context, Result};

use rpm_spec_analyzer::profile::Profile;
use rpm_spec_analyzer::session::parse;
use rpm_spec_repo_core::{Dependency, NEVRA, RepoUniverse};
use rpm_spec_repo_resolver::{ConflictChain, SolveRequest, Solution, UnsatCore, solve};
use serde::Serialize;

/// Run the buildroot solver for one (spec source, profile) pair.
///
/// Conditional-aware BR extraction is via
/// [`rpm_spec_analyzer::rules::repo::shared::active_buildrequires`]
/// — the same path `matrix deps check` uses, so cross-distro
/// `%if "%_vendor" == "rosa"` arms don't pollute the closure on a
/// profile where the arm is inactive.
///
/// # Errors
///
/// Propagates SQLite / I/O failures from the resolver via `anyhow`.
/// Callers that want to render a per-row "skipped because of infra"
/// verdict should match on the `Err` arm and inject their own
/// degraded row rather than `?`-propagating (which would poison the
/// whole report on one corrupt snapshot).
pub fn solve_for(
    source: &str,
    profile: &Profile,
    universe: &RepoUniverse,
) -> Result<Solution> {
    let outcome = parse(source);
    let requirements = rpm_spec_analyzer::rules::repo::shared::active_buildrequires(
        &outcome.spec,
        profile,
    );
    let (base_packages, implicit_brs) = match profile.repos.as_ref() {
        Some(rs) => (
            literal_dependencies(&rs.buildroot.base_packages),
            literal_dependencies(&rs.buildroot.implicit_buildrequires),
        ),
        None => (Vec::new(), Vec::new()),
    };
    solve(SolveRequest {
        universe,
        requirements: &requirements,
        base_packages: &base_packages,
        implicit_brs: &implicit_brs,
    })
    .with_context(|| format!("solver for profile `{}`", profile.identity.name))
}

/// Project literal package names from `profile.buildroot.base_packages`
/// (and the implicit BR list) into the resolver's [`Dependency`] shape.
/// These are simple names without version constraints — the resolver
/// itself does the EVR work. Returns `Vec<Dependency>` (not
/// `Vec<Capability>`) because `SolveRequest::base_packages` /
/// `implicit_brs` are require-side slots.
#[must_use]
pub fn literal_dependencies(names: &[String]) -> Vec<Dependency> {
    names.iter().map(|n| Dependency::unversioned(n.as_str())).collect()
}

/// Convert the resolver's raw `UnsatCore` into the dedup'd shape
/// every matrix command renders (one row per dep text, one row per
/// `(cause, victim, via)` conflict triple).
///
/// The walker re-pushes the same unmet atom every time a different
/// parent re-discovers it; without dedup the human render shows
/// dozens of identical lines per dep. The dedup keys are the dep
/// `Display` text and the cause/victim/via triple — these are the
/// fields operators read, so collisions on them are by construction
/// "same issue, different walk path" rather than "two distinct issues".
#[must_use]
pub fn dedup_unsat_core(core: &UnsatCore) -> DedupedUnsat {
    let mut seen_unsat: HashSet<String> = HashSet::new();
    let mut unsatisfied: Vec<UnmetEntry> = Vec::new();
    for item in &core.unsatisfied {
        let dep_text = item.dep.display();
        if !seen_unsat.insert(dep_text.clone()) {
            continue;
        }
        unsatisfied.push(UnmetEntry {
            dep: dep_text,
            required_by: item.provenance.chain.iter().cloned().collect(),
        });
    }
    let mut seen_conf: HashSet<String> = HashSet::new();
    let mut conflicts: Vec<ConflictEntry> = Vec::new();
    for c in &core.conflict_chains {
        if !seen_conf.insert(conflict_dedup_key(c)) {
            continue;
        }
        conflicts.push(ConflictEntry {
            cause: c.cause.clone(),
            cause_chain: c.cause_provenance.chain.iter().cloned().collect(),
            victim: c.victim.clone(),
            victim_chain: c.victim_provenance.chain.iter().cloned().collect(),
            via: c.via_capability.to_string(),
        });
    }
    DedupedUnsat {
        unsatisfied,
        conflicts,
    }
}

/// Key used to dedup repeated `ConflictChain` rows. The `|` separator
/// is safe: NEVRA `Display` and capability names use the
/// `[A-Za-z0-9.+_-]` character class plus `:` (epoch) and `-`
/// (segments), never `|`.
fn conflict_dedup_key(c: &ConflictChain) -> String {
    format!("{}|{}|{}", c.cause, c.victim, c.via_capability)
}

/// Three-way verdict shared across all matrix solver commands.
/// Promoted from per-command duplicates (`buildroot.rs::Verdict`,
/// `buildroot_diff.rs::SideVerdict`, `deps_explain.rs::ExplainVerdict`)
/// so the JSON-token set is a single source of truth — adding a
/// fourth variant (e.g. `Error`) is a one-file edit.
#[derive(Debug, Clone, Copy, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SolveVerdict {
    /// Solver returned a satisfying closure.
    Ok,
    /// Solver returned `Unsatisfiable` with at least one unmet dep
    /// or conflict chain.
    Unsat,
    /// No cached repo metadata for this profile / spec — the solver
    /// wasn't invoked. CI typically treats this as neutral, NOT a
    /// failure, so the dep-checker pipeline keeps reporting on other
    /// rows.
    Skipped,
    /// Per-row infrastructure failure (corrupt snapshot, SQL error).
    /// Distinct from `Skipped` (which is the absence of cache) so
    /// CI tooling can alert on real breakage without false-firing
    /// on routine "cache not synced" rows.
    Error,
}

/// Dedup'd unsat-core slice ready for rendering. Both buckets are
/// `Vec` (not `Set`) because dedup happened during construction and
/// downstream renderers depend on insertion order.
#[derive(Debug, Clone)]
pub struct DedupedUnsat {
    pub unsatisfied: Vec<UnmetEntry>,
    pub conflicts: Vec<ConflictEntry>,
}

/// One row in the `UNMET` bucket — the unmet dep text plus the
/// provenance chain that pulled it in (nearest parent first, root
/// cause last; empty chain = directly from spec).
#[derive(Debug, Clone, Serialize)]
pub struct UnmetEntry {
    pub dep: String,
    pub required_by: Vec<NEVRA>,
}

/// One row in the `CONFLICTS` bucket — the two clashing pinned
/// packages plus the capability that triggered the rejection. Both
/// sides carry their own provenance chains so renderers can show
/// *who* pulled each side in.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictEntry {
    pub cause: NEVRA,
    pub cause_chain: Vec<NEVRA>,
    pub victim: NEVRA,
    pub victim_chain: Vec<NEVRA>,
    pub via: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_dependencies_produces_unversioned_deps() {
        let deps = literal_dependencies(&["bash".into(), "make".into()]);
        assert_eq!(deps.len(), 2);
        // Field access uses the inherent forwarders on `Dependency`
        // (the newtype has no `Deref<Capability>` — deliberate).
        assert_eq!(deps[0].name().as_ref(), "bash");
        assert!(deps[0].version().is_unversioned());
    }

    #[test]
    fn verdict_serialises_as_snake_case() {
        assert_eq!(serde_json::to_string(&SolveVerdict::Ok).unwrap(), "\"ok\"");
        assert_eq!(serde_json::to_string(&SolveVerdict::Unsat).unwrap(), "\"unsat\"");
        assert_eq!(serde_json::to_string(&SolveVerdict::Skipped).unwrap(), "\"skipped\"");
        assert_eq!(serde_json::to_string(&SolveVerdict::Error).unwrap(), "\"error\"");
    }

    #[test]
    fn dedup_collapses_repeated_unmet_atoms() {
        use rpm_spec_repo_core::Dependency;
        use rpm_spec_repo_resolver::{DepProvenance, UnsatItem};
        let dep = Dependency::unversioned("foo");
        let mut core = UnsatCore {
            unsatisfied: vec![
                UnsatItem {
                    dep: dep.clone(),
                    provenance: DepProvenance::from_spec(),
                },
                UnsatItem {
                    dep: dep.clone(),
                    provenance: DepProvenance::from_spec(),
                },
            ],
            conflict_chains: Vec::new(),
            suggestion: None,
            rich_deps_skipped: 0,
        };
        core.unsatisfied[1].provenance = DepProvenance::from_spec();
        let out = dedup_unsat_core(&core);
        assert_eq!(out.unsatisfied.len(), 1, "{out:?}");
        assert_eq!(out.unsatisfied[0].dep, "foo");
    }

    #[test]
    fn dedup_collapses_repeated_conflict_chains() {
        use rpm_spec_repo_core::NEVRA;
        use rpm_spec_repo_resolver::{ConflictChain, DepProvenance};
        let nevra = |name: &str| NEVRA {
            name: Arc::from(name),
            epoch: 0,
            version: Arc::from("1.0"),
            release: Arc::from("1.el9"),
            arch: Arc::from("x86_64"),
        };
        let chain = ConflictChain {
            cause: nevra("foo"),
            cause_provenance: DepProvenance::from_spec(),
            victim: nevra("bar"),
            victim_provenance: DepProvenance::from_spec(),
            via_capability: Arc::from("/usr/bin/widget"),
        };
        let core = UnsatCore {
            unsatisfied: Vec::new(),
            conflict_chains: vec![chain.clone(), chain.clone()],
            suggestion: None,
            rich_deps_skipped: 0,
        };
        let out = dedup_unsat_core(&core);
        assert_eq!(out.conflicts.len(), 1, "{out:?}");
        assert_eq!(out.conflicts[0].cause.name.as_ref(), "foo");
        assert_eq!(out.conflicts[0].victim.name.as_ref(), "bar");
        assert_eq!(out.conflicts[0].via, "/usr/bin/widget");
    }
}
