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

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use rpm_spec_repo_core::{Dependency, EVR, NEVRA, Package, ProviderRef, RepoError, RepoUniverse};

use crate::predicates::{REPO_PRIORITY_FALLBACK, provides_satisfies};
use crate::unsat::{ConflictChain, UnsatCore};

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
///
/// # Errors
///
/// Returns [`RepoError::Database`] when a DB-backed lookup fails
/// (corrupted SQLite snapshot, disk full, etc). A caller that needs
/// just an "is this universe ready?" check should treat any `Err`
/// the same as an unsatisfiable solution.
pub fn solve(req: SolveRequest<'_>) -> Result<Solution, RepoError> {
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

    let mut state = SolverState::default();
    let mut work: Vec<Dependency> = Vec::new();
    work.extend(base_packages.iter().cloned());
    work.extend(implicit_brs.iter().cloned());
    work.extend(requirements.iter().cloned());

    let mut unsatisfied: Vec<Dependency> = Vec::new();
    let mut conflicts: Vec<ConflictChain> = Vec::new();
    let mut rich_deps_skipped = 0usize;

    while let Some(dep) = work.pop() {
        if is_rich_expression(&dep.name) {
            rich_deps_skipped += 1;
            tracing::debug!(expr = ?dep.name, "skipping rich dep");
            continue;
        }
        if state.is_met(&dep) {
            continue;
        }

        let Some(provider) = pick_provider(universe, &dep)? else {
            tracing::debug!(req = ?dep.name, "no provider");
            unsatisfied.push(dep);
            continue;
        };

        if let Some(chain) = state.would_conflict(universe, &provider)? {
            conflicts.push(chain);
            unsatisfied.push(dep);
            continue;
        }

        let newly_pinned = state.pin(universe, provider.clone())?;
        if newly_pinned
            && let Some(pkg) = provider.resolve(universe)?
        {
            for r in &pkg.requires {
                if !state.is_met(r) {
                    work.push(r.clone());
                }
            }
        }
    }

    if unsatisfied.is_empty() && conflicts.is_empty() {
        Ok(Solution::Ok(state.into_closure()))
    } else {
        Ok(Solution::Unsatisfiable(UnsatCore {
            unsatisfied,
            conflict_chains: conflicts,
            suggestion: None,
            rich_deps_skipped,
        }))
    }
}

#[derive(Default)]
struct SolverState {
    pinned: Vec<(ProviderRef, NEVRA)>,
    pinned_names: HashSet<Arc<str>>,
    met_caps: HashSet<Arc<str>>,
    provider_for: BTreeMap<Arc<str>, NEVRA>,
    install_size_total: u64,
}

impl SolverState {
    fn is_met(&self, dep: &Dependency) -> bool {
        self.met_caps.contains(&dep.name)
    }

    fn pin(&mut self, universe: &RepoUniverse, pref: ProviderRef) -> Result<bool, RepoError> {
        let Some(pkg) = pref.resolve(universe)? else {
            return Ok(false);
        };
        if !self.pinned_names.insert(pkg.nevra.name.clone()) {
            return Ok(false);
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
        Ok(true)
    }

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
    ) -> Result<Option<ConflictChain>, RepoError> {
        let Some(cand_pkg) = candidate.resolve(universe)? else {
            return Ok(None);
        };
        for (pref, _nevra) in &self.pinned {
            let Some(p) = pref.resolve(universe)? else {
                continue;
            };
            if let Some(chain) = conflict_between(&cand_pkg, &p) {
                return Ok(Some(chain));
            }
            if let Some(chain) = conflict_between(&p, &cand_pkg) {
                return Ok(Some(chain));
            }
        }
        Ok(None)
    }
}

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

fn pick_provider(
    universe: &RepoUniverse,
    dep: &Dependency,
) -> Result<Option<ProviderRef>, RepoError> {
    // Gather by-name first, then by-provides (insertion order matters
    // for the deterministic-name tiebreak below).
    let mut seen: HashSet<ProviderRef> = HashSet::new();
    let mut candidates: Vec<ProviderRef> = Vec::new();
    for r in universe.by_name(&dep.name)? {
        if seen.insert(r.clone()) {
            candidates.push(r);
        }
    }
    for r in universe.provides_by_name(&dep.name)? {
        if seen.insert(r.clone()) {
            candidates.push(r);
        }
    }

    let mut best: Option<(i32, std::cmp::Reverse<EVR>, Arc<str>, ProviderRef)> = None;
    for cand_ref in candidates {
        let Some(pkg) = cand_ref.resolve(universe)? else {
            continue;
        };
        if !provides_satisfies(&pkg, dep) {
            continue;
        }
        let priority = universe
            .priority_of(&cand_ref.repo_id)
            .unwrap_or(REPO_PRIORITY_FALLBACK);
        let key = (
            priority,
            std::cmp::Reverse(pkg.nevra.evr()),
            pkg.nevra.name.clone(),
            cand_ref,
        );
        match &best {
            None => best = Some(key),
            Some(current) => {
                let cur_key = (&current.0, &current.1, &current.2);
                let new_key = (&key.0, &key.1, &key.2);
                if new_key < cur_key {
                    best = Some(key);
                }
            }
        }
    }
    Ok(best.map(|(_, _, _, pref)| pref))
}

fn is_rich_expression(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('(')
}
