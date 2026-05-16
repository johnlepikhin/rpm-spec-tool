//! Visitor-based static analyzer for RPM `.spec` files.
//!
//! Built on top of [`rpm_spec`]'s AST: lint rules implement [`Visit`] over
//! `SpecFile<Span>` and surface findings as [`Diagnostic`]s.
//!
//! Entry point: [`session::LintSession`].

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod config;
pub mod diagnostic;
pub mod lint;
pub mod registry;
pub mod rules;
pub mod session;
pub mod visit;

pub use diagnostic::{Applicability, Diagnostic, Edit, Label, LintCategory, Severity, Suggestion};
pub use lint::{Lint, LintMetadata};
pub use session::{
    LintSession, ParseOutcome, ParserDiagnostic, ParserSeverity, analyze, analyze_with_profile,
    analyze_with_profile_at, parse,
};
pub use visit::Visit;

pub use rpm_spec::ast::Span;
pub use rpm_spec_profile as profile;
