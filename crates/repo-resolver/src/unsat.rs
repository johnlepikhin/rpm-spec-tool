//! Unsat-core representation. Compact by default (1-2 chains shown);
//! callers render `--verbose` form by walking [`UnsatCore`] themselves.

use std::sync::Arc;

use rpm_spec_repo_core::{Dependency, NEVRA};

/// Origin of a dependency the walker was asked to satisfy.
///
/// `Spec` items came in via the caller's initial dep slices
/// (spec's `BuildRequires:`, the profile's `base_packages`, the
/// platform's `implicit_buildrequires`). `Transitive` items were
/// pulled in by an already-pinned package's `Requires:` — the
/// chain field walks bottom-up: nearest puller first, root cause
/// last. The empty chain encodes "directly from the request".
///
/// `chain` is `Arc<[NEVRA]>` rather than `Vec<NEVRA>` because the
/// solver clones the provenance once per worklist child — for a
/// transitive closure of a few thousand deps each cloning a chain of
/// depth ~5 the difference between cloning a Vec (heap copy of the
/// elements) and bumping an Arc refcount is measurable in wall time.
#[derive(Debug, Clone)]
pub struct DepProvenance {
    /// Packages that transitively pulled this dep, nearest first.
    /// Empty when the dep was in the caller's initial slices.
    pub chain: Arc<[NEVRA]>,
}

impl DepProvenance {
    /// The walker's initial dep slices (spec / base / implicit).
    #[must_use]
    pub fn from_spec() -> Self {
        Self {
            chain: Arc::from([] as [NEVRA; 0]),
        }
    }

    /// Same chain extended with `parent` — used when pinning a
    /// package pushes its `Requires:` back onto the worklist.
    #[must_use]
    pub fn pushed_by(&self, parent: &NEVRA) -> Self {
        let mut next: Vec<NEVRA> = Vec::with_capacity(self.chain.len() + 1);
        next.push(parent.clone());
        next.extend(self.chain.iter().cloned());
        Self {
            chain: Arc::from(next),
        }
    }

    /// `true` when the dep was in the caller's initial slices —
    /// renderers use this to elide "required by: spec" decorations
    /// that would otherwise repeat on every top-level item.
    #[must_use]
    pub fn is_from_spec(&self) -> bool {
        self.chain.is_empty()
    }

    /// Build a provenance from an already-materialised chain. Used by
    /// the solver when it has to reconstruct a pinned package's
    /// ancestry from its own bookkeeping.
    #[must_use]
    pub fn from_chain(chain: Arc<[NEVRA]>) -> Self {
        Self { chain }
    }
}

/// One dep the walker couldn't satisfy, paired with the chain of
/// packages that pulled it in.
#[derive(Debug, Clone)]
pub struct UnsatItem {
    /// The unsatisfied requirement as declared (name + flags + EVR
    /// from the source `Requires:` / `BuildRequires:` line).
    pub dep: Dependency,
    /// Ancestry chain — empty for top-level `BuildRequires:`, populated
    /// for transitive `Requires:` (nearest puller first, root cause last).
    pub provenance: DepProvenance,
}

/// One conflict between two pinned packages, attributed to the
/// capability that triggered the rejection. Both sides carry full
/// [`DepProvenance`] so renderers can attribute the conflict to the
/// top-level BR that ultimately pulled each package in, not just the
/// leaf NEVRAs that physically clash.
#[derive(Debug, Clone)]
pub struct ConflictChain {
    /// Package whose `Conflicts:` declaration fired.
    pub cause: NEVRA,
    /// Ancestry that pulled `cause` into the closure (nearest first).
    pub cause_provenance: DepProvenance,
    /// Package matched by `cause.conflicts` and thus rejected.
    pub victim: NEVRA,
    /// Ancestry that pulled `victim` into the closure (nearest first).
    pub victim_provenance: DepProvenance,
    /// Capability name from `cause.conflicts` that matched `victim.provides`.
    pub via_capability: Arc<str>,
}

/// Why the walker couldn't satisfy a request. Contains the
/// unsatisfied deps plus any conflict chains discovered along the
/// way; lints render this into `matrix deps explain` output.
#[derive(Debug, Clone)]
pub struct UnsatCore {
    pub unsatisfied: Vec<UnsatItem>,
    pub conflict_chains: Vec<ConflictChain>,
    /// Optional human-friendly suggestion, populated by the lint
    /// layer (which knows about distro-specific package renames).
    pub suggestion: Option<String>,
    /// Count of rich dep expressions the walker skipped. Surfaces as
    /// `RPM-REPO-INFO-RICH-DEP` from the lint integration.
    pub rich_deps_skipped: usize,
}

impl UnsatCore {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.unsatisfied.is_empty() && self.conflict_chains.is_empty()
    }
}
