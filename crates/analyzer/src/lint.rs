//! Lint trait and metadata.

use rpm_spec_profile::Profile;

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

    /// Called once after [`Self::set_config`] with the resolved
    /// distribution profile (identity, macros, rpmlib features, license
    /// and group whitelists). Rules that are profile-aware (RPM024,
    /// RPM025, RPM050, … — to be added in follow-up PRs) copy the
    /// fields they need from here. Default is a no-op so existing rules
    /// don't need to opt in.
    fn set_profile(&mut self, profile: &Profile) {
        let _ = profile;
    }

    /// Called once with the on-disk path of the source being linted,
    /// or `None` when the source is stdin / an in-memory string. Rules
    /// that need to inspect the *file name* (e.g. RPM312
    /// `spec-filename-mismatch`) store it here; AST-only rules ignore
    /// it. Default is a no-op so existing rules don't need to opt in.
    fn set_source_path(&mut self, path: Option<&std::path::Path>) {
        let _ = path;
    }

    /// Called once by [`crate::session::LintSession::from_config_with_profile`]
    /// **before** [`Self::set_config`] / [`Self::set_profile`] — gating
    /// happens first so we don't pay the (often non-trivial)
    /// initialisation cost for rules that are about to be dropped. The
    /// implementation therefore must read everything it needs from the
    /// `profile` parameter and cannot rely on prior `set_*` state.
    ///
    /// Rules whose semantic applicability depends on the active distro
    /// (e.g. "Fedora-only convention", "openSUSE requires `Group:`")
    /// return `false` to be dropped from the active set entirely —
    /// saves the visit pass and avoids polluting output with
    /// inapplicable diagnostics.
    ///
    /// Default returns `true` so most rules don't need to opt in. This
    /// is **distinct** from emit-time gating (`if !condition { return }`
    /// inside `visit_*`): use `applies_to_profile` for "rule logically
    /// doesn't apply here", and keep emit-time checks for "rule applies
    /// but severity/suggestion varies per profile".
    fn applies_to_profile(&self, _profile: &Profile) -> bool {
        true
    }
}
