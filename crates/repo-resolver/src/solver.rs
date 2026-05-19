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

use crate::predicates::{REPO_PRIORITY_FALLBACK, gather_candidates, provides_satisfies};
use crate::unsat::{ConflictChain, DepProvenance, UnsatCore, UnsatItem};

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
    // Worklist entries carry provenance — the chain of packages
    // that pulled this dep in. Initial items (from base /
    // implicit / spec slices) get an empty chain ("from spec").
    // Transitive Requires inherit the parent's chain + parent
    // NEVRA prepended, so an UnsatItem's first chain entry is
    // the package whose Requires line failed.
    let mut work: Vec<(Dependency, DepProvenance)> = Vec::new();
    work.extend(
        base_packages
            .iter()
            .cloned()
            .map(|d| (d, DepProvenance::from_spec())),
    );
    work.extend(
        implicit_brs
            .iter()
            .cloned()
            .map(|d| (d, DepProvenance::from_spec())),
    );
    work.extend(
        requirements
            .iter()
            .cloned()
            .map(|d| (d, DepProvenance::from_spec())),
    );

    let mut unsatisfied: Vec<UnsatItem> = Vec::new();
    let mut conflicts: Vec<ConflictChain> = Vec::new();
    let mut rich_deps_skipped = 0usize;

    while let Some((dep, provenance)) = work.pop() {
        if is_rich_expression(&dep.name) {
            rich_deps_skipped += 1;
            tracing::debug!(expr = ?dep.name, "skipping rich dep");
            continue;
        }
        if state.is_met(&dep) {
            continue;
        }
        // File-path deps need a per-pinned-package lookup against
        // the `files` table — we deliberately don't fold file
        // ownership into `met_caps` (millions of paths). Without
        // this check, alternative providers of the same file
        // (`pkgconfig` vs `pkgconf-pkg-config`, both owning
        // `/usr/bin/pkg-config` while declaring mutual
        // `Conflicts:`) trip a false unsat: the first one wins
        // the virtual-cap satisfaction round, the second is
        // picked for the file-path round and clashes.
        if dep.is_file_path() && state.any_pinned_owns(universe, &dep.name)? {
            continue;
        }

        let Some(provider) = pick_provider(universe, &dep)? else {
            tracing::debug!(req = ?dep.name, "no provider");
            unsatisfied.push(UnsatItem { dep, provenance });
            continue;
        };

        if let Some(chain) = state.would_conflict(universe, &provider, &provenance)? {
            conflicts.push(chain);
            unsatisfied.push(UnsatItem { dep, provenance });
            continue;
        }

        let newly_pinned = state.pin(universe, provider.clone(), &provenance)?;
        if newly_pinned
            && let Some(pkg) = provider.resolve(universe)?
        {
            // Each transitive Require inherits this provider's
            // ancestry so its eventual unmet report can trace
            // back to a top-level BR.
            let child_prov = provenance.pushed_by(&pkg.nevra);
            for r in &pkg.requires {
                if !state.is_met(r) {
                    work.push((r.clone(), child_prov.clone()));
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
    /// Provenance chain for each pinned package — pinned-name →
    /// chain of parents that transitively pulled it in. Used by
    /// `would_conflict` to attribute both sides of a clash to the
    /// top-level BR that ultimately required them. Stored as
    /// `Arc<[NEVRA]>` so the conflict path can hand it straight to
    /// `DepProvenance::from_chain` without re-allocating.
    provenance: std::collections::HashMap<Arc<str>, Arc<[NEVRA]>>,
}

impl SolverState {
    fn is_met(&self, dep: &Dependency) -> bool {
        self.met_caps.contains(&dep.name)
    }

    fn pin(
        &mut self,
        universe: &RepoUniverse,
        pref: ProviderRef,
        provenance: &DepProvenance,
    ) -> Result<bool, RepoError> {
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
        self.provenance
            .insert(pkg.nevra.name.clone(), provenance.chain.clone());
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

    /// True iff any package already pinned owns `path` in its
    /// filelist. Used to short-circuit alternative providers of
    /// the same file path (`/usr/bin/pkg-config`, owned by both
    /// `pkgconfig` and `pkgconf-pkg-config`) once one of them is
    /// already in the closure.
    fn any_pinned_owns(
        &self,
        universe: &RepoUniverse,
        path: &str,
    ) -> Result<bool, RepoError> {
        for (pref, _) in &self.pinned {
            let Some(db) = universe.db_for(&pref.repo_id) else {
                continue;
            };
            if db.owns_file(pref.pkg_id, path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn would_conflict(
        &self,
        universe: &RepoUniverse,
        candidate: &ProviderRef,
        cand_provenance: &DepProvenance,
    ) -> Result<Option<ConflictChain>, RepoError> {
        let Some(cand_pkg) = candidate.resolve(universe)? else {
            return Ok(None);
        };
        // Need full packages with `conflicts` data loaded (the
        // resolver's hot-path `load_package` skips files but keeps
        // caps, so `cause.conflicts` is populated).
        for (pref, _nevra) in &self.pinned {
            let Some(p) = pref.resolve(universe)? else {
                continue;
            };
            let pinned_prov = self
                .provenance
                .get(p.nevra.name.as_ref())
                .cloned()
                .map(DepProvenance::from_chain)
                .unwrap_or_else(DepProvenance::from_spec);
            if let Some(chain) =
                conflict_between(&cand_pkg, &p, cand_provenance.clone(), pinned_prov.clone())
            {
                return Ok(Some(chain));
            }
            if let Some(chain) =
                conflict_between(&p, &cand_pkg, pinned_prov, cand_provenance.clone())
            {
                return Ok(Some(chain));
            }
        }
        Ok(None)
    }
}

fn conflict_between(
    cause: &Package,
    victim: &Package,
    cause_provenance: DepProvenance,
    victim_provenance: DepProvenance,
) -> Option<ConflictChain> {
    for c in &cause.conflicts {
        if provides_satisfies(victim, c) {
            return Some(ConflictChain {
                cause: cause.nevra.clone(),
                cause_provenance,
                victim: victim.nevra.clone(),
                victim_provenance,
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
    // Combined `(pref, NEVRA)` candidate query — one round-trip per
    // repo (was three: by_name + by_provides + per-candidate
    // resolve_nevra). For file-path deps tack on the file owner.
    // Shared with `lookup` via `gather_candidates` so the set
    // definition can't drift between the two consumers.
    let (candidates, file_path_dep) = gather_candidates(universe, dep)?;

    let mut best: Option<(i32, std::cmp::Reverse<EVR>, Arc<str>, ProviderRef)> = None;
    for (cand_ref, nevra) in candidates {
        // File-path deps were already proven via `file_owner`;
        // other deps go through the hot-path `universe.satisfies`
        // (one or two indexed SELECTs, no full `load_package`).
        let satisfies = if file_path_dep {
            true
        } else {
            universe.satisfies(&cand_ref, dep)?
        };
        if !satisfies {
            continue;
        }
        let priority = universe
            .priority_of(&cand_ref.repo_id)
            .unwrap_or(REPO_PRIORITY_FALLBACK);
        let key = (
            priority,
            std::cmp::Reverse(nevra.evr()),
            nevra.name.clone(),
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
