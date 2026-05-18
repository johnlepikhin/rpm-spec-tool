//! RPM129 `bcond-on-non-fedora` — flag use of `%bcond_with` /
//! `%bcond_without` macros on distros where they aren't natively
//! supported.
//!
//! ## Profile gating
//!
//! `%bcond_with NAME` and `%bcond_without NAME` are Fedora/RHEL
//! macros — they define a configurable build option that's queried
//! later with `%{with NAME}`. RHEL clones (CentOS Stream, AlmaLinux,
//! Rocky) inherit the macro definitions from the RHEL macroset, so
//! they're covered too.
//!
//! On ALT Linux, openSUSE/SLES, Mageia and other distros the macro
//! either doesn't exist or behaves differently. A spec written for
//! Fedora that uses `%bcond_with` won't work portably without
//! reworking the conditional logic into `%define` + plain `%if`.
//!
//! Gate: the rule only emits diagnostics when `set_profile` has been
//! called with an *explicitly* non-Fedora/RHEL family
//! (`Family::Alt`, `Family::Opensuse`, `Family::Mageia`). Default
//! constructor leaves the rule inactive: when the profile is unknown
//! (default `Profile`, `Family::Generic`, or no `set_profile` call at
//! all — e.g. tests that bypass the session) we stay silent. The
//! cost of a false positive on a Fedora spec running through the
//! default pipeline outweighs the marginal benefit of catching a
//! true positive on an unknown profile. Real repro: llvm.spec is a
//! Fedora spec; the previous gate let `%bcond_with snapshot_build`
//! emit 38 diagnostics under the default profile.
//!
//! ## Trigger
//!
//! Source scan: any line whose first non-whitespace token is
//! `%bcond_with` or `%bcond_without`. We deliberately don't try to
//! parse the AST — `%bcond_*` produces no syntactic node we can
//! visit, since the parser treats it as a macro call that
//! disappears after expansion.

use rpm_spec::ast::{Span, SpecFile};
use rpm_spec_profile::{Family, Profile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM129",
    name: "bcond-on-non-fedora",
    description: "`%bcond_with` / `%bcond_without` are Fedora/RHEL-specific build-option macros; \
                  use `%define NAME 1` + plain `%if` on other distros.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// `%bcond_with` / `%bcond_without` are Fedora/RHEL-specific build-option macros; use `%define NAME 1` + plain `%if` on other distros.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BcondOnNonFedora {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
    /// `Some(true)` only after `set_profile` was called with an
    /// explicitly non-Fedora/RHEL family. `None` (the default) and
    /// `Some(false)` both keep the rule silent — see the module
    /// docs for the noise-prevention rationale.
    active: Option<bool>,
}

impl BcondOnNonFedora {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BcondOnNonFedora {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        // No profile set, or profile is Fedora-family / unknown:
        // stay silent. Real repro for the FP we're guarding against:
        // llvm.spec is a Fedora spec; the rule used to fire 38 times
        // under the default profile because `applies_to_profile` was
        // the only gate and tests / non-session callers bypassed it.
        if self.active != Some(true) {
            return;
        }
        let Some(source) = self.source.as_deref() else {
            return;
        };
        for (start, kind) in find_bcond_uses(source) {
            let end = start
                + match kind {
                    BcondKind::With => "%bcond_with".len(),
                    BcondKind::Without => "%bcond_without".len(),
                };
            let macro_name = match kind {
                BcondKind::With => "%bcond_with",
                BcondKind::Without => "%bcond_without",
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    METADATA.default_severity,
                    format!(
                        "`{macro_name}` is a Fedora/RHEL macro and isn't natively supported on this distro"
                    ),
                    Span::from_bytes(start, end),
                )
                .with_suggestion(Suggestion::new(
                    "rewrite as `%define NAME 1` (default-on) or `%define NAME 0` (default-off) \
                     and replace `%{with NAME}` with `%if %{NAME}` checks",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl Lint for BcondOnNonFedora {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = Some(source);
    }

    fn set_profile(&mut self, profile: &Profile) {
        // Mirror of `applies_to_profile` — kept in lock-step so a
        // caller that goes through the session (which gates via
        // `applies_to_profile`) and a caller that constructs the lint
        // directly (tests, plug-in hosts, future tooling) both reach
        // the same conclusion. The instance-level `active` flag is
        // the authoritative gate inside `visit_spec`.
        self.active = Some(is_non_fedora_family(profile));
    }

    fn applies_to_profile(&self, profile: &Profile) -> bool {
        is_non_fedora_family(profile)
    }
}

/// `true` only when the profile is *explicitly* a non-Fedora/RHEL
/// distro family that lacks native `%bcond_*` support. Returns
/// `false` for the default profile (`family = None`),
/// `Family::Generic` (explicit opt-out), and the Fedora/RHEL
/// families themselves.
///
/// `Family` is `#[non_exhaustive]`; we deliberately spell out every
/// arm rather than use a wildcard. When a new family lands upstream
/// the missing arm will force a deliberate "is this another RHEL
/// clone (silent) or a fourth-family openSUSE-style distro (fire)?"
/// review here.
fn is_non_fedora_family(profile: &Profile) -> bool {
    match profile.identity.family {
        Some(Family::Alt | Family::Opensuse | Family::Mageia) => true,
        // Fedora / RHEL natively support `%bcond_*` — silent.
        Some(Family::Fedora | Family::Rhel) => false,
        // `Family::Generic` is the explicit opt-out (custom internal
        // distro); we don't claim to know whether `%bcond_*` works
        // there. Stay silent — same noise-prevention rationale as for
        // the unknown-profile case.
        Some(Family::Generic) => false,
        // No family detected — pre-profile pipelines / default Profile.
        // Stay silent: an FP on Fedora costs more than a TP on
        // unknown.
        None => false,
        // `Family` is `#[non_exhaustive]`; future variants default
        // to silent until someone makes a deliberate call.
        Some(_) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BcondKind {
    With,
    Without,
}

/// Scan source bytes for `%bcond_with` / `%bcond_without` macro
/// invocations at the start of a logical line (after optional leading
/// whitespace). Skips comment lines. Returns `(byte_offset, kind)` for
/// each match.
fn find_bcond_uses(src: &str) -> Vec<(usize, BcondKind)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut line_start = 0;
    let mut i = 0;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'\n' {
            if let Some(hit) = scan_line(src, line_start, i) {
                out.push(hit);
            }
            line_start = i + 1;
        }
        i += 1;
    }
    out
}

/// Inspect one logical line (`src[start..end]`) for a leading
/// `%bcond_with` / `%bcond_without` token. Returns absolute byte
/// offset of the `%` character.
fn scan_line(src: &str, start: usize, end: usize) -> Option<(usize, BcondKind)> {
    let line = src.get(start..end)?;
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        // Comment line — `%bcond` inside a comment is intent, not use.
        return None;
    }
    let leading_ws = line.len() - trimmed.len();
    // `%bcond_without` is checked first (longer prefix), then
    // `%bcond_with` — must respect token boundary (next char can't be
    // alnum/_).
    if let Some(rest) = trimmed.strip_prefix("%bcond_without")
        && rest.chars().next().is_none_or(|c| !is_ident_char(c))
    {
        return Some((start + leading_ws, BcondKind::Without));
    }
    if let Some(rest) = trimmed.strip_prefix("%bcond_with")
        && rest.chars().next().is_none_or(|c| !is_ident_char(c))
    {
        return Some((start + leading_ws, BcondKind::With));
    }
    None
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::util::make_test_profile;
    use crate::session::parse;

    fn run(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = BcondOnNonFedora::new();
        // Mirror the production session order: gate via
        // `applies_to_profile` (cheap perf reorder), then push the
        // profile + source into the rule. The instance-level `active`
        // flag set by `set_profile` is what `visit_spec` actually
        // checks — covered by the `silent_when_no_profile_set` test.
        if !lint.applies_to_profile(profile) {
            return Vec::new();
        }
        lint.set_profile(profile);
        lint.set_source(std::sync::Arc::from(src));
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn fires_on_alt_with_bcond_with() {
        let profile = make_test_profile(Some(Family::Alt), None, &[], &[]);
        let src = "Name: x\n%bcond_with python3\n";
        let diags = run(src, &profile);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM129");
        assert!(diags[0].message.contains("%bcond_with"));
    }

    #[test]
    fn fires_on_opensuse_with_bcond_without() {
        let profile = make_test_profile(Some(Family::Opensuse), None, &[], &[]);
        let src = "Name: x\n%bcond_without gtk\n";
        let diags = run(src, &profile);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%bcond_without"));
    }

    #[test]
    fn silent_on_fedora() {
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc40"), &[], &[]);
        let src = "Name: x\n%bcond_with python3\n";
        assert!(
            run(src, &profile).is_empty(),
            "Fedora natively supports %bcond_*"
        );
    }

    #[test]
    fn silent_on_rhel() {
        let profile = make_test_profile(Some(Family::Rhel), Some(".el9"), &[], &[]);
        let src = "Name: x\n%bcond_with python3\n";
        assert!(
            run(src, &profile).is_empty(),
            "RHEL inherits %bcond_* from upstream"
        );
    }

    #[test]
    fn silent_on_alt_without_bcond() {
        let profile = make_test_profile(Some(Family::Alt), None, &[], &[]);
        let src = "Name: x\n%define with_python 1\n%if %{with_python}\n%endif\n";
        assert!(run(src, &profile).is_empty());
    }

    #[test]
    fn silent_when_no_family() {
        // Pre-profile pipelines / `generic` profile — stay silent to
        // avoid noise when distro is unknown.
        let profile = make_test_profile(None, None, &[], &[]);
        let src = "Name: x\n%bcond_with python3\n";
        assert!(run(src, &profile).is_empty());
    }

    #[test]
    fn skips_bcond_inside_comment() {
        let profile = make_test_profile(Some(Family::Alt), None, &[], &[]);
        let src = "Name: x\n# %bcond_with python3 is Fedora-only\n";
        assert!(run(src, &profile).is_empty(), "comments don't count");
    }

    #[test]
    fn fires_multiple_uses_independently() {
        let profile = make_test_profile(Some(Family::Mageia), None, &[], &[]);
        let src = "Name: x\n%bcond_with python3\n%bcond_without docs\n";
        let diags = run(src, &profile);
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn handles_leading_whitespace() {
        let profile = make_test_profile(Some(Family::Alt), None, &[], &[]);
        // Indented (rare but legal at the top of a section).
        let src = "Name: x\n    %bcond_with python3\n";
        let diags = run(src, &profile);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_when_no_profile_set() {
        // Default construction → no `set_profile` call → the rule
        // must stay inert. This guards against the FP described in
        // the module docs: callers that bypass the session pipeline
        // (tests, plug-in hosts, future tooling) used to receive
        // spurious diagnostics on Fedora specs.
        let outcome = parse("Name: x\n%bcond_with python3\n");
        let mut lint = BcondOnNonFedora::new();
        lint.set_source(std::sync::Arc::from("Name: x\n%bcond_with python3\n"));
        lint.visit_spec(&outcome.spec);
        let diags = lint.take_diagnostics();
        assert!(
            diags.is_empty(),
            "rule must be inert without an explicit non-Fedora set_profile; got {diags:?}"
        );
    }

    #[test]
    fn silent_under_fedora_profile() {
        // Real repro from the issue: llvm.spec is a Fedora spec that
        // uses `%bcond_with snapshot_build`. Running through the
        // production session path (`set_profile` with Fedora) must
        // not flag it. Bypasses `applies_to_profile` deliberately so
        // the instance-level `active` flag is the only gate under
        // test.
        let profile = make_test_profile(Some(Family::Fedora), Some(".fc40"), &[], &[]);
        let outcome = parse("Name: llvm\n%bcond_with snapshot_build\n");
        let mut lint = BcondOnNonFedora::new();
        lint.set_profile(&profile);
        lint.set_source(std::sync::Arc::from(
            "Name: llvm\n%bcond_with snapshot_build\n",
        ));
        lint.visit_spec(&outcome.spec);
        let diags = lint.take_diagnostics();
        assert!(
            diags.is_empty(),
            "Fedora profile must suppress RPM129; got {diags:?}"
        );
    }
}
