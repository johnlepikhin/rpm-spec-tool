//! Unsat-core representation. Compact by default (1-2 chains shown);
//! callers render `--verbose` form by walking [`UnsatCore`] themselves.

use std::sync::Arc;

use rpm_spec_repo_core::{Dependency, NEVRA};

/// One conflict between two pinned packages, attributed to the
/// capability that triggered the rejection.
#[derive(Debug, Clone)]
pub struct ConflictChain {
    pub cause: NEVRA,
    pub victim: NEVRA,
    pub via_capability: Arc<str>,
}

/// Why the walker couldn't satisfy a request. Contains the
/// unsatisfied deps plus any conflict chains discovered along the
/// way; lints render this into `matrix deps explain` output.
#[derive(Debug, Clone)]
pub struct UnsatCore {
    pub unsatisfied: Vec<Dependency>,
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
