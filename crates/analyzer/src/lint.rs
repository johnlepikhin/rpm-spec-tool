//! Lint trait and metadata.

use crate::config::Config;
use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::visit::Visit;

/// Static metadata describing a lint rule.
///
/// `id` is the stable identifier used in tooling and SARIF output; `name` is
/// the kebab-case configuration key (more diff-friendly than the numeric id).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
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

    /// Called by [`crate::LintSession::run`] before each visit pass.
    /// Rules that need access to the original source bytes — e.g. for
    /// whitespace / tab-indent checks that don't survive the AST round
    /// trip — store the slice here. The default is a no-op, so existing
    /// rules don't need to opt in.
    fn set_source(&mut self, source: &str) {
        let _ = source;
    }

    /// Called once by [`crate::session::LintSession::from_config`] after
    /// the rule is constructed and before any visit pass. Rules that
    /// read out-of-band configuration (e.g. external-tool paths, code
    /// disable lists) copy the relevant fields here. Default is a no-op.
    fn set_config(&mut self, config: &Config) {
        let _ = config;
    }
}
