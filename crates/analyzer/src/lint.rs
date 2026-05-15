//! Lint trait and metadata.

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::visit::Visit;

/// Static metadata describing a lint rule.
///
/// `id` is the stable identifier used in tooling and SARIF output; `name` is
/// the kebab-case configuration key (more diff-friendly than the numeric id).
#[derive(Debug, Clone, Copy)]
pub struct LintMetadata {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_severity: Severity,
    pub category: LintCategory,
}

/// A lint rule.
///
/// Implementors provide visitor methods that record findings into internal
/// state; [`Lint::take_diagnostics`] drains that state at the end of a pass.
/// Rules are run by [`crate::session::LintSession`], one pass each, on a
/// fully-parsed `SpecFile<Span>`.
pub trait Lint: for<'ast> Visit<'ast> + Send {
    fn metadata(&self) -> &'static LintMetadata;

    fn take_diagnostics(&mut self) -> Vec<Diagnostic>;
}
