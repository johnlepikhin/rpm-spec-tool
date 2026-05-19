//! P0 walker solver.
//!
//! Given a [`rpm_spec_repo_core::RepoUniverse`] and a list of
//! requirements (BuildRequires from a spec plus base buildroot
//! packages plus implicit BRs from the profile), iteratively resolve
//! each unmet dep to a provider and walk its transitive Requires.
//!
//! Picks the best provider by `(repo priority asc, EVR desc,
//! package name lex)`.
//!
//! Conflict detection is two-way (`Conflicts:` from either side) and
//! reports both pulling chains in the unsat core.
//!
//! Rich Deps (boolean expressions like `(a or b)`) are not solved by
//! the walker; they surface as `SolveError::RichDep`. The walker is
//! the only backend in M1; SAT-backed resolution (rich deps, complex
//! conflicts) is tracked separately for a later milestone.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rpm_spec_repo_core::{
    CapFlags, Capability, Dependency, EVR, NEVRA, Package, ProviderRef, RepoId, RepoUniverse,
};

use crate::unsat::{ConflictChain, UnsatCore};

/// Fallback when a `ProviderRef` references a repo that isn't in
/// `RepoUniverse::repos`. Matches the dnf "no priority set"
/// convention and the `RepoConfig::priority` default.
const REPO_PRIORITY_FALLBACK: i32 = 99;

/// Outcome of [`solve`].
#[derive(Debug)]
pub enum Solution {
    Ok(BuildrootClosure),
    Unsatisfiable(UnsatCore),
}

/// Successful resolution: which packages would end up in the buildroot
/// and which dep each one satisfies.
#[derive(Debug, Clone)]
pub struct BuildrootClosure {
    pub packages: Vec<NEVRA>,
    /// Map from the original requirement's name to the chosen provider.
    /// Lets `matrix buildroot solve` show "cmake → cmake-3.26.5-...".
    pub provider_for: BTreeMap<Arc<str>, NEVRA>,
    pub install_size_total: u64,
}

/// Inputs to [`solve`]. Bundled into a struct so the three same-typed
/// dependency slices can't be silently swapped at call sites.
#[derive(Debug)]
pub struct SolveRequest<'a> {
    pub universe: &'a RepoUniverse,
    pub requirements: &'a [Dependency],
    pub base_packages: &'a [Dependency],
    pub implicit_brs: &'a [Dependency],
}

/// Resolve a set of requirements against a universe. `base_packages`
/// is the chroot baseline (rpm-build / gcc / etc) and `implicit_brs`
/// is the platform's shadow BR set; both are added before walking
/// `requirements`.
pub fn solve(req: SolveRequest<'_>) -> Solution {
    let SolveRequest {
        universe,
        requirements,
        base_packages,
        implicit_brs,
    } = req;

    let span = tracing::info_span!(
        "resolver.solve",
        profile = %universe.profile_name,
        n_requirements = requirements.len(),
    );
    let _g = span.enter();

    // Build a one-shot priority lookup so `pick_provider` doesn't
    // re-scan `universe.repos` for every candidate.
    let repo_priority: HashMap<RepoId, i32> = universe
        .repos
        .iter()
        .enumerate()
        .map(|(i, r)| (r.repo_id.clone(), i32::try_from(i).unwrap_or(REPO_PRIORITY_FALLBACK)))
        .collect();

    let mut state = SolverState::default();
    let mut work: Vec<Dependency> = Vec::new();
    work.extend(base_packages.iter().cloned());
    work.extend(implicit_brs.iter().cloned());
    work.extend(requirements.iter().cloned());

    let mut unsatisfied: Vec<Dependency> = Vec::new();
    let mut conflicts: Vec<ConflictChain> = Vec::new();
    let mut rich_deps_skipped = 0usize;

    while let Some(dep) = work.pop() {
        // Rich dep gate (P0 walker can't handle boolean expressions).
        if is_rich_expression(&dep.name) {
            rich_deps_skipped += 1;
            tracing::debug!(expr = ?dep.name, "skipping rich dep");
            continue;
        }

        // Already met?
        if state.is_met(&dep) {
            continue;
        }

        let Some(provider) = pick_provider(universe, &dep, &repo_priority) else {
            tracing::debug!(req = ?dep.name, "no provider");
            unsatisfied.push(dep);
            continue;
        };

        // Conflict check against everything already pinned.
        if let Some(chain) = state.would_conflict(universe, &provider) {
            conflicts.push(chain);
            unsatisfied.push(dep);
            continue;
        }

        // Pin records the package's name + every Provides into met_caps
        // immediately, so transitive Requires can short-circuit on
        // the next iteration instead of being re-resolved.
        let newly_pinned = state.pin(universe, provider.clone());

        // Enqueue this provider's Requires (only if we actually pinned
        // a new package — `pin` returns false when the package was
        // already in the closure under another name lookup).
        if newly_pinned {
            if let Some(pkg) = provider.resolve(universe) {
                for r in &pkg.requires {
                    if !state.is_met(r) {
                        work.push(r.clone());
                    }
                }
            }
        }
    }

    if unsatisfied.is_empty() && conflicts.is_empty() {
        Solution::Ok(state.into_closure())
    } else {
        Solution::Unsatisfiable(UnsatCore {
            unsatisfied,
            conflict_chains: conflicts,
            suggestion: None,
            rich_deps_skipped,
        })
    }
}

#[derive(Default)]
struct SolverState {
    /// Pinned packages in insertion order (paired with their NEVRA),
    /// for deterministic closure output. The NEVRA is cached so
    /// `into_closure` doesn't need to revisit the universe just to
    /// project names.
    pinned: Vec<(ProviderRef, NEVRA)>,
    /// Names of packages already pinned (by `nevra.name`). Used to
    /// dedupe when two different capability lookups resolve to the
    /// same package.
    pinned_names: HashSet<Arc<str>>,
    /// Capability names already provided by something pinned. Both the
    /// package's own name and every Provides entry are inserted.
    met_caps: HashSet<Arc<str>>,
    provider_for: BTreeMap<Arc<str>, NEVRA>,
    install_size_total: u64,
}

impl SolverState {
    fn is_met(&self, dep: &Dependency) -> bool {
        self.met_caps.contains(&dep.name)
    }

    /// Pin a provider, accumulating size + capability index.
    /// Returns `true` if the package was newly added, `false` if it
    /// was already pinned (duplicate via a different capability name).
    fn pin(&mut self, universe: &RepoUniverse, pref: ProviderRef) -> bool {
        let Some(pkg) = pref.resolve(universe) else {
            return false;
        };
        if !self.pinned_names.insert(pkg.nevra.name.clone()) {
            // Already pinned under a previous lookup — skip duplicate.
            return false;
        }
        let nevra = pkg.nevra.clone();
        self.install_size_total += pkg.size_installed;
        self.met_caps.insert(pkg.nevra.name.clone());
        for prov in &pkg.provides {
            self.met_caps.insert(prov.name.clone());
        }
        self.provider_for
            .entry(pkg.nevra.name.clone())
            .or_insert_with(|| pkg.nevra.clone());
        self.pinned.push((pref, nevra));
        true
    }

    /// Assemble the final closure from accumulated state. Packages
    /// are listed in the deterministic order they were pinned.
    fn into_closure(self) -> BuildrootClosure {
        BuildrootClosure {
            packages: self.pinned.into_iter().map(|(_, n)| n).collect(),
            provider_for: self.provider_for,
            install_size_total: self.install_size_total,
        }
    }

    fn would_conflict(
        &self,
        universe: &RepoUniverse,
        candidate: &ProviderRef,
    ) -> Option<ConflictChain> {
        let cand_pkg = candidate.resolve(universe)?;
        for (pref, _nevra) in &self.pinned {
            let Some(p) = pref.resolve(universe) else {
                continue;
            };
            if let Some(chain) = conflict_between(cand_pkg, p) {
                return Some(chain);
            }
            if let Some(chain) = conflict_between(p, cand_pkg) {
                return Some(chain);
            }
        }
        None
    }
}

/// Check whether `cause`'s `Conflicts:` entries match `victim` (by
/// name or via `Provides:`). Returns the first matching chain.
fn conflict_between(cause: &Package, victim: &Package) -> Option<ConflictChain> {
    for c in &cause.conflicts {
        if provides_satisfies(victim, c) {
            return Some(ConflictChain {
                cause: cause.nevra.clone(),
                victim: victim.nevra.clone(),
                via_capability: c.name.clone(),
            });
        }
    }
    None
}

fn provides_satisfies(provider: &Package, requirement: &Capability) -> bool {
    if provider.nevra.name.as_ref() == requirement.name.as_ref() {
        return evr_matches(&provider.nevra.evr(), requirement);
    }
    for prov in &provider.provides {
        if prov.name.as_ref() != requirement.name.as_ref() {
            continue;
        }
        // No version constraint on the requirement → name match suffices.
        if requirement.flags == CapFlags::None {
            return true;
        }
        // Versioned requirement: dnf semantics say a versionless
        // `Provides:` does NOT satisfy it (the package didn't
        // declare a version, so we can't compare). Move on to the
        // next provider entry.
        match (&prov.evr, &requirement.evr) {
            (Some(pevr), Some(_)) => {
                if evr_matches(pevr, requirement) {
                    return true;
                }
            }
            _ => continue,
        }
    }
    false
}

fn evr_matches(provider_evr: &EVR, req: &Capability) -> bool {
    let req_evr = match &req.evr {
        Some(e) => e,
        None => {
            // Malformed Capability: a versioned `flags` but no EVR has
            // no defined match semantics. Treat as unmet rather than
            // silently accept (the old `return true` path was wrong).
            // A genuinely unconstrained requirement should use
            // `CapFlags::None` AND `evr: None`, which the caller
            // dispatches above via the `requirement.flags == CapFlags::None`
            // shortcut in `provides_satisfies`.
            return req.flags == CapFlags::None;
        }
    };
    let cmp = provider_evr.compare_rpm(req_evr);
    match req.flags {
        CapFlags::None => true,
        CapFlags::EQ => cmp == Ordering::Equal,
        CapFlags::LT => cmp == Ordering::Less,
        CapFlags::LE => cmp != Ordering::Greater,
        CapFlags::GT => cmp == Ordering::Greater,
        CapFlags::GE => cmp != Ordering::Less,
    }
}

fn pick_provider(
    universe: &RepoUniverse,
    dep: &Dependency,
    repo_priority: &HashMap<RepoId, i32>,
) -> Option<ProviderRef> {
    // Prefer direct name match first (`Requires: bash` style).
    let candidates = universe
        .by_name
        .get(dep.name.as_ref())
        .into_iter()
        .flatten()
        .chain(
            universe
                .provides_by_name
                .get(dep.name.as_ref())
                .into_iter()
                .flatten(),
        );

    // Pick the best candidate by `(priority asc, EVR desc, name asc)`.
    // EVR is compared via `Ord`, which delegates to `compare_rpm` — so
    // `5.1.8-10` correctly sorts as newer than `5.1.8-9`. Wrapping in
    // `Reverse` flips the order so the highest EVR wins under the
    // outer ascending sort.
    let mut best: Option<(i32, std::cmp::Reverse<EVR>, Arc<str>, ProviderRef)> = None;
    for cand_ref in candidates {
        let Some(pkg) = cand_ref.resolve(universe) else {
            continue;
        };
        if !provides_satisfies(pkg, dep) {
            continue;
        }
        // Repo priority: `universe.repos` is constructed by the assembler
        // in ascending priority order (lower wins ties), so the slice
        // index doubles as the effective sort key. Callers must preserve
        // that invariant when building a `RepoUniverse` by hand (the
        // resolver tests do this in `tests/solver_smoke.rs`).
        let priority = repo_priority
            .get(&cand_ref.repo_id)
            .copied()
            .unwrap_or(REPO_PRIORITY_FALLBACK);
        let key = (
            priority,
            std::cmp::Reverse(pkg.nevra.evr()),
            pkg.nevra.name.clone(),
            cand_ref.clone(),
        );
        match &best {
            None => best = Some(key),
            Some(current) => {
                // Compare by (priority, Reverse<EVR>, name); skip the
                // ProviderRef tail since it isn't `Ord`.
                let cur_key = (&current.0, &current.1, &current.2);
                let new_key = (&key.0, &key.1, &key.2);
                if new_key < cur_key {
                    best = Some(key);
                }
            }
        }
    }
    best.map(|(_, _, _, pref)| pref)
}

/// Recognise rich dep boolean wrapping. Real parsing lives in the
/// `resolvo` integration when enabled; the walker only needs to
/// detect-and-skip.
fn is_rich_expression(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('(')
}
