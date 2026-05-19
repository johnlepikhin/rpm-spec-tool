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
// Narrow blanket: only `solver::{Solution,BuildrootClosure,SolveRequest}` and
// `unsat::{ConflictChain,UnsatCore}` still carry undocumented public fields
// (mirrors of the wire-level resolver output). New gaps outside that set
// should be flagged at review time — promote individual items to `///` and
// shrink the silenced surface as the backlog is worked down.
#![expect(
    missing_docs,
    reason = "pre-1.0: walker-output fields in solver.rs / unsat.rs are doc-backlogged; lookup.rs + predicates.rs already documented"
)]

pub mod lookup;
pub mod predicates;
pub mod solver;
pub mod unsat;

pub use lookup::{LookupOutcome, lookup};
pub use predicates::{evr_matches, matches_flag, provides_satisfies};
pub use solver::{BuildrootClosure, SolveRequest, Solution, solve};
pub use unsat::{ConflictChain, DepProvenance, UnsatCore, UnsatItem};
