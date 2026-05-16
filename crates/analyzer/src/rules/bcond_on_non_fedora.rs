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
//! Gate: `applies_to_profile` returns `true` only when
//!   `family` is set AND is NOT `Family::Fedora` or `Family::Rhel`.
//! When family is unknown (default Profile) the rule stays silent —
//! we don't want to spam warnings during pre-profile lint runs.
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

#[derive(Debug, Default)]
pub struct BcondOnNonFedora {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl BcondOnNonFedora {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BcondOnNonFedora {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
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

    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }

    fn applies_to_profile(&self, profile: &Profile) -> bool {
        // Explicit allowlist + explicit fallback arms. `Family` is
        // `#[non_exhaustive]`, so a wildcard `_` would silently absorb
        // future variants (e.g. `Family::Almalinux`) — defeating the
        // very compile-time audit this match exists to provide.
        // Splitting the fallback into `None` and `Some(_)` keeps both
        // intents visible to a reader running `cargo expand` or `git
        // blame` (auditor: "Is this new variant a RHEL clone, a SUSE
        // clone, or genuinely new? Pick one.").
        match profile.identity.family {
            Some(Family::Alt | Family::Opensuse | Family::Mageia | Family::Generic) => true,
            Some(Family::Fedora | Family::Rhel) => false,
            // No family detected — pre-profile pipelines / `generic`
            // profile. Stay silent: noise-prevention.
            None => false,
            // `Family` is `#[non_exhaustive]`; future variants default
            // to silent until someone makes a deliberate call. Audit
            // this arm whenever a new variant lands upstream.
            Some(_) => false,
        }
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
        if !lint.applies_to_profile(profile) {
            return Vec::new();
        }
        lint.set_source(src);
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
}
