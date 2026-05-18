//! Visitor-based static analyzer for RPM `.spec` files.
//!
//! Built on top of [`rpm_spec`]'s AST: lint rules implement [`Visit`] over
//! `SpecFile<Span>` and surface findings as [`Diagnostic`]s.
//!
//! Entry point: [`session::LintSession`].

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
// TODO(pre-1.0): document the public surface and remove this expect.
// Currently 537 items lack `///` doc comments — chiefly per-rule
// structs in `rules/` and per-layer config types. Tracked separately
// from publication.
#![expect(
    missing_docs,
    reason = "pre-1.0: 537 items lack /// — track and reduce; expect form fires loudly when the backlog reaches zero"
)]

pub mod baseline;
pub mod bcond;
pub mod branch_aware;
pub mod branch_coverage;
pub mod classes;
pub mod config;
pub mod dep_walk;
pub mod impact;
pub mod config_cache;
pub mod contract;
pub mod diagnostic;
pub mod error_format;
pub(crate) mod files;
pub mod lint;
pub mod macro_usage;
pub mod matrix;
pub mod portability;
pub(crate) mod policy;
pub mod registry;
pub mod rules;
pub mod session;
pub(crate) mod shell;
pub mod visit;

pub use baseline::{Baseline, BaselineEntry, BaselineError};
pub use bcond::{BcondEntry, BcondMap, BcondOverrides};
pub use branch_aware::{IndeterminatePolicy, ProfileBranchSelection, SelectedBody};
pub use classes::{ClassesReport, DepBucket, EquivalenceClass, ProfileSignature};
pub use impact::{COMPARED_TAGS, ChangeSet, ImpactReport, ProfileImpact, TagImpact};
pub use dep_walk::{for_each_dep_atom, render_text_with_macros};
pub use branch_coverage::{
    BranchActivity, BranchCoverage, CollectedBranch, CollectedConditional, CoverageEntry,
    CoverageReport, EvalError,
};
pub use contract::{
    Contract, ContractError, ContractProfileStatus, ContractReport, ContractViolation,
    ProfileContract, ProfileContractReport,
};
pub use diagnostic::{Applicability, Diagnostic, Edit, Label, LintCategory, Severity, Suggestion};
pub use macro_usage::MacroUsageCollector;
pub use portability::{PortabilityEntry, PortabilityReport, PortabilityStatus, StatusCounts};
pub use lint::{Lint, LintMetadata};
pub use matrix::{
    AggregatedDiagnostic, MatrixResult, MatrixSignature, MatrixSignatureParseError, ProfileResult,
    SIGNATURE_HEX_LEN, run_matrix,
};
pub use session::{
    LintSession, ParseOutcome, ParserDiagnostic, ParserSeverity, analyze, analyze_with_profile,
    analyze_with_profile_at, parse,
};
pub use visit::Visit;

pub use rpm_spec::ast::Span;
pub use rpm_spec_profile as profile;
