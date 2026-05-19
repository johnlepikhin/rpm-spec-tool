//! Single-dependency lookup, decoupled from full closure resolution.
//!
//! The walker in [`crate::solver::solve`] is the right tool when you
//! need a transitive buildroot closure. RPM-REPO-* lints, by
//! contrast, ask "for THIS one `BuildRequires:` / `Requires:` atom,
//! does a satisfying package exist in the configured repos?". That's
//! a single-shot query — `lookup` provides it without the worklist
//! / conflict-chain machinery.
//!
//! The candidate-selection logic mirrors [`crate::solver::pick_provider`]:
//! prefer the lowest configured priority repo, then the highest EVR
//! (via `rpmvercmp`), then the lexicographically smallest package
//! name as a deterministic tiebreaker.

use std::cmp::Ordering;
use std::sync::Arc;

use rpm_spec_repo_core::{Dependency, EVR, NEVRA, ProviderRef, RepoError, RepoUniverse};

use crate::predicates::{REPO_PRIORITY_FALLBACK, gather_candidates};

/// Outcome of [`lookup`]. The three variants map 1:1 to the
/// RPM-REPO-001 / -002 / -003 lints in the analyzer.
#[derive(Debug, Clone)]
pub enum LookupOutcome {
    /// A provider was found AND its version satisfies the
    /// requirement's constraint (if any).
    Satisfied {
        /// Handle into the universe pointing at the package that
        /// satisfies the requirement. Resolvable via
        /// [`ProviderRef::resolve`].
        provider: ProviderRef,
        /// Concrete NEVRA of the chosen package, lifted out of the
        /// universe at lookup time so callers don't need to re-query
        /// through `ProviderRef` for trivial display.
        nevra: NEVRA,
    },
    /// No package in any configured repo provides the requirement's
    /// name (neither via the package's own name nor any `Provides:`).
    NoProvider,
    /// Providers exist for the name but none of their versions
    /// satisfy the constraint. `best_available` reports the highest
    /// EVR found among the candidates, so the lint can show "have
    /// 3.20, need >= 3.26".
    VersionUnsatisfied {
        best_available: EVR,
        best_provider: NEVRA,
    },
}

/// Look up a single dependency against the universe.
///
/// Each candidate triggers a full [`RepoUniverse::resolve`] (loads
/// the package's NEVRA + Provides + the rest) because
/// `provides_satisfies` needs the `Provides:` rows to handle
/// virtual-capability + version constraints. For dependency atoms
/// with a single hot candidate this is one disk query; for popular
/// virtual names like `glibc(libc.so.6)` with dozens of providers
/// the per-candidate load adds up — measure before optimising.
///
/// # Errors
///
/// Propagates the underlying [`RepoError`] when the per-repo SQLite
/// query fails — typically [`RepoError::Sqlite`] (driver-level failure)
/// or [`RepoError::Database`] (schema mismatch / corrupt manifest).
/// Lint code surfaces the error via the diagnostic sink rather than
/// retry: a snapshot that can't be queried is unusable, and the
/// offline-cache fallback already converts "no cache" into a silent
/// skip upstream.
pub fn lookup(universe: &RepoUniverse, dep: &Dependency) -> Result<LookupOutcome, RepoError> {
    // Combined one-shot candidate query: pulls every (pkg_id, NEVRA)
    // matching the dep name in one SQL round trip per repo, plus the
    // file-owner fallback for path deps. Shared with the solver via
    // `gather_candidates` so the candidate set can't diverge.
    let (candidates, file_path_dep) = gather_candidates(universe, dep)?;
    if candidates.is_empty() {
        return Ok(LookupOutcome::NoProvider);
    }

    let mut best_sat: Option<Candidate> = None;
    let mut best_any: Option<Candidate> = None;

    for (cand_ref, nevra) in &candidates {
        let priority = universe
            .priority_of(&cand_ref.repo_id)
            .unwrap_or(REPO_PRIORITY_FALLBACK);
        let cand = Candidate {
            priority,
            evr: nevra.evr(),
            name: nevra.name.clone(),
            nevra: nevra.clone(),
            provider_ref: cand_ref.clone(),
        };
        if best_any
            .as_ref()
            .is_none_or(|b| is_strictly_better(&cand, b))
        {
            best_any = Some(cand.clone());
        }
        // For file-path deps, `candidates_with_nevra` + `file_owner`
        // already established ownership; skip the expensive
        // satisfies check. For everything else, route through the
        // hot-path `universe.satisfies` which avoids loading the
        // full Package's caps + files (the JOIN that motivated
        // splitting `load_package` / `load_package_with_files`).
        let satisfies = if file_path_dep {
            true
        } else {
            universe.satisfies(cand_ref, dep)?
        };
        if satisfies
            && best_sat
                .as_ref()
                .is_none_or(|b| is_strictly_better(&cand, b))
        {
            best_sat = Some(cand);
        }
    }

    if let Some(sat) = best_sat {
        return Ok(LookupOutcome::Satisfied {
            provider: sat.provider_ref,
            nevra: sat.nevra,
        });
    }
    if let Some(any) = best_any {
        return Ok(LookupOutcome::VersionUnsatisfied {
            best_available: any.evr,
            best_provider: any.nevra,
        });
    }
    Ok(LookupOutcome::NoProvider)
}

#[derive(Debug, Clone)]
struct Candidate {
    priority: i32,
    evr: EVR,
    name: Arc<str>,
    nevra: NEVRA,
    provider_ref: ProviderRef,
}

/// Returns `true` when `a` is a STRICTLY better candidate than `b`
/// per the ordering `(priority asc, EVR desc, name lex)`.
fn is_strictly_better(a: &Candidate, b: &Candidate) -> bool {
    match a.priority.cmp(&b.priority) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => match a.evr.cmp(&b.evr) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => a.name < b.name,
        },
    }
}

#[cfg(test)]
mod tests {
    //! Direct unit coverage for the lookup primitives. Builds tiny
    //! [`RepoUniverse`] fixtures inline via
    //! [`RepoUniverse::from_indexes_for_tests`] (in-memory SQLite,
    //! no disk I/O) to exercise `lookup` and the `is_strictly_better`
    //! tiebreak ordering.
    use rpm_spec_repo_core::{CapFlags, Capability, Package, PkgChecksum, RepoIndex, RepoUniverse};
    use time::OffsetDateTime;

    use super::*;

    fn nevra(name: &str, version: &str, release: &str) -> NEVRA {
        NEVRA {
            name: Arc::from(name),
            epoch: 0,
            version: Arc::from(version),
            release: Arc::from(release),
            arch: Arc::from("x86_64"),
        }
    }

    fn cap_unversioned(name: &str) -> Capability {
        Capability {
            name: Arc::from(name),
            flags: CapFlags::None,
            evr: None,
        }
    }

    fn cap_ge(name: &str, version: &str, release: &str) -> Capability {
        Capability {
            name: Arc::from(name),
            flags: CapFlags::GE,
            evr: Some(EVR::new(Some(0), version, release)),
        }
    }

    fn pkg(
        repo_id: &str,
        name: &str,
        version: &str,
        release: &str,
        provides: Vec<Capability>,
    ) -> Package {
        Package {
            nevra: nevra(name, version, release),
            repo_id: Arc::from(repo_id),
            provides,
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: None,
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Sha256(format!("{name}-{version}-{release}")),
            location: Arc::from(""),
            files: Vec::new(),
        }
    }

    fn one_repo_universe(packages: Vec<Package>) -> RepoUniverse {
        let repo_id: Arc<str> = Arc::from("test-repo");
        let index = RepoIndex {
            repo_id,
            revision: "rev0".into(),
            fetched_at: OffsetDateTime::now_utc(),
            packages,
            advisories: Vec::new(),
        };
        RepoUniverse::from_indexes_for_tests("test-profile", vec![Arc::new(index)])
            .expect("build in-memory universe")
    }

    #[test]
    fn lookup_satisfied_for_direct_name_match() {
        let uni = one_repo_universe(vec![pkg("test-repo", "bash", "5.1.8", "9", vec![])]);
        let dep = cap_unversioned("bash");

        match lookup(&uni, &dep).expect("query") {
            LookupOutcome::Satisfied { nevra, .. } => {
                assert_eq!(nevra.name.as_ref(), "bash");
                assert_eq!(nevra.version.as_ref(), "5.1.8");
            }
            other => panic!("expected Satisfied, got {other:?}"),
        }
    }

    #[test]
    fn lookup_no_provider_for_missing_name() {
        let uni = one_repo_universe(vec![pkg("test-repo", "bash", "5.1.8", "9", vec![])]);
        let dep = cap_unversioned("nonexistent");

        match lookup(&uni, &dep).expect("query") {
            LookupOutcome::NoProvider => {}
            other => panic!("expected NoProvider, got {other:?}"),
        }
    }

    #[test]
    fn lookup_version_unsatisfied_when_evr_too_low() {
        let uni = one_repo_universe(vec![pkg("test-repo", "cmake", "3.20.0", "1", vec![])]);
        let dep = cap_ge("cmake", "3.26.0", "1");

        match lookup(&uni, &dep).expect("query") {
            LookupOutcome::VersionUnsatisfied {
                best_available,
                best_provider,
            } => {
                assert_eq!(best_provider.name.as_ref(), "cmake");
                assert_eq!(best_available.version, "3.20.0");
            }
            other => panic!("expected VersionUnsatisfied, got {other:?}"),
        }
    }

    #[test]
    fn is_strictly_better_priority_then_evr_then_name() {
        let mk = |priority: i32, version: &str, name: &str| Candidate {
            priority,
            evr: EVR::new(Some(0), version, "1"),
            name: Arc::from(name),
            nevra: nevra(name, version, "1"),
            provider_ref: ProviderRef {
                repo_id: Arc::from("test-repo"),
                pkg_id: 0,
            },
        };

        let lo = mk(0, "1.0", "foo");
        let hi = mk(10, "9.9", "foo");
        assert!(is_strictly_better(&lo, &hi));
        assert!(!is_strictly_better(&hi, &lo));

        let newer = mk(5, "2.0", "foo");
        let older = mk(5, "1.0", "foo");
        assert!(is_strictly_better(&newer, &older));
        assert!(!is_strictly_better(&older, &newer));

        let a = mk(5, "1.0", "alpha");
        let b = mk(5, "1.0", "beta");
        assert!(is_strictly_better(&a, &b));
        assert!(!is_strictly_better(&b, &a));

        let x = mk(5, "1.0", "foo");
        let y = mk(5, "1.0", "foo");
        assert!(!is_strictly_better(&x, &y));
    }
}
