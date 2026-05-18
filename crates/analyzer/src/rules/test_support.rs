//! Shared `#[cfg(test)]` helpers used by rule unit tests to avoid
//! the ~137-fold duplication of `fn run(src) -> Vec<Diagnostic>`.
//!
//! Each rule's `mod tests` used to define its own `fn run` with the
//! same five-line body (`parse` → construct lint → `visit_spec` →
//! `take_diagnostics`). These helpers collapse that down to a single
//! call site (`run_lint::<MyLint>(src)`).
//!
//! The lint is constructed via `Default::default()` because every
//! built-in rule derives `Default`; the convenience `MyLint::new()`
//! constructors that some rules add are unconditional aliases for it.

use std::sync::Arc;

use rpm_spec_profile::Profile;

use crate::config::Config;
use crate::diagnostic::Diagnostic;
use crate::lint::Lint;
use crate::session::parse;

/// Parse `src` and run a single `Lint` against it, returning the
/// lint's diagnostics. Always calls `set_source` — rules that don't
/// override it inherit the default no-op, so this is cheap and safe.
pub(crate) fn run_lint<L>(src: &str) -> Vec<Diagnostic>
where
    L: Default + Lint,
{
    let outcome = parse(src);
    let mut lint = L::default();
    lint.set_source(Arc::from(src));
    lint.visit_spec(&outcome.spec);
    lint.take_diagnostics()
}

/// Variant of [`run_lint`] for profile-aware rules — calls
/// `set_profile` between construction and the visit pass.
pub(crate) fn run_lint_with_profile<L>(src: &str, profile: &Profile) -> Vec<Diagnostic>
where
    L: Default + Lint,
{
    let outcome = parse(src);
    let mut lint = L::default();
    lint.set_source(Arc::from(src));
    lint.set_profile(profile);
    lint.visit_spec(&outcome.spec);
    lint.take_diagnostics()
}

/// Variant of [`run_lint`] for config-aware rules — calls
/// `set_config` between construction and the visit pass.
#[allow(dead_code)]
pub(crate) fn run_lint_with_config<L>(src: &str, config: &Config) -> Vec<Diagnostic>
where
    L: Default + Lint,
{
    let outcome = parse(src);
    let mut lint = L::default();
    lint.set_source(Arc::from(src));
    lint.set_config(config);
    lint.visit_spec(&outcome.spec);
    lint.take_diagnostics()
}
