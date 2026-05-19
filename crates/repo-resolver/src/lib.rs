//! P0 resolver: walks the Provides/Requires/Conflicts/Obsoletes
//! indexes of a [`rpm_spec_repo_core::RepoUniverse`] to answer
//! "given these BuildRequires + base buildroot, is the closure
//! satisfiable, and if not why not?".
//!
//! Algorithm intentionally simple — no SAT, no backtracking. Picks
//! the best-priority highest-EVR provider for each unmet dep, walks
//! transitive Requires, records conflicts. The walker is the only
//! backend in M1; Rich Deps surface as `RPM-REPO-INFO-RICH-DEP`
//! diagnostics from the lint layer. SAT-backed resolution (rich
//! deps, complex conflicts) is tracked separately for a later
//! milestone.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
#![expect(
    missing_docs,
    reason = "pre-1.0: data shapes mirror RPM metadata format; doc backlog tracked separately"
)]

pub mod solver;
pub mod unsat;

pub use solver::{BuildrootClosure, SolveRequest, Solution, solve};
pub use unsat::{ConflictChain, UnsatCore};
