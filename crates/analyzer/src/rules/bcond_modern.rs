//! RPM400 `prefer-bcond-new-syntax` — `%bcond_with NAME` and
//! `%bcond_without NAME` declarations on profiles where the
//! `%bcond NAME DEFAULT` syntax (rpm ≥ 4.17.1) is available.
//!
//! The modern `%bcond` form takes a default value explicitly, which
//! makes the conditional's intent visible at the declaration site:
//!
//! ```text
//! %bcond_with  systemd       # default off, --with systemd flips it
//! %bcond_without docs        # default on,  --without docs flips it
//! ```
//! versus
//! ```text
//! %bcond systemd 0
//! %bcond docs 1
//! ```
//!
//! The latter avoids the `_with` / `_without` polarity quirk
//! (`%bcond_with` defaults to off, `%bcond_without` to on — opposite
//! of what the names suggest at first read) and pairs naturally with
//! `%{with NAME}` / `%{without NAME}` checks.
//!
//! Profile-gated to families that ship a modern rpm. We trust the
//! `Family` marker rather than parsing a version string from
//! `rpm --showrc` — Fedora has been on rpm ≥ 4.17 since Fedora 36
//! (2022), Mageia since Mageia 9. On profiles outside Fedora/Mageia
//! the rule is silent (`applies_to_profile` returns `false`, so it
//! does not produce diagnostics that can then be allowed). To opt in
//! on a non-default family, override `applies_to_profile` via lint
//! configuration or set the active profile's family to one of the
//! supported values.
//!
//! ## Relation to RPM129 `bcond-on-non-fedora`
//!
//! RPM129 targets ALT/openSUSE/Mageia/Generic and warns "don't use
//! `%bcond_with` / `%bcond_without` at all — they're Fedora/RHEL
//! macros". RPM400 includes Mageia in its gate because Mageia 9+
//! ships rpm 4.17+, so the modern `%bcond NAME DEFAULT` form works
//! natively. The two rules overlap on Mageia by design: RPM129
//! reflects the older "bcond is Fedora-only" recommendation, while
//! RPM400 is the canonical guidance once you accept Mageia 9+'s rpm.
//! If both fire on a Mageia spec, prefer RPM400's advice and silence
//! RPM129 via lint configuration.

use rpm_spec::ast::{BuildCondStyle, BuildCondition, Conditional, Span, SpecFile, SpecItem};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::{Family, Profile};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM400",
    name: "prefer-bcond-new-syntax",
    description: "`%bcond_with NAME` / `%bcond_without NAME` are pre-rpm-4.17 declarations. The \
                  modern `%bcond NAME DEFAULT` form makes the polarity explicit and is preferred \
                  on profiles that ship rpm ≥ 4.17.1.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint state for RPM400. The `enabled` flag is set in `set_profile`
/// based on the active family (see `family_supports_modern_bcond`).
#[derive(Debug, Default)]
pub struct PreferBcondNewSyntax {
    diagnostics: Vec<Diagnostic>,
    enabled: bool,
}

impl PreferBcondNewSyntax {
    /// Construct an empty lint instance with no profile bound.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Families known to ship rpm ≥ 4.17.1 across their current supported
/// releases. Fedora ≥ 36 ships rpm 4.18+, Mageia ≥ 9 ships rpm 4.17+.
/// Other families (RHEL 9 / 8 stay on rpm 4.16, openSUSE Leap on
/// 4.14, ALT varies) are intentionally excluded; users who know
/// their target ships modern rpm can opt in via config.
fn family_supports_modern_bcond(profile: &Profile) -> bool {
    matches!(
        profile.identity.family,
        Some(Family::Fedora | Family::Mageia)
    )
}

impl<'ast> Visit<'ast> for PreferBcondNewSyntax {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if !self.enabled {
            return;
        }
        walk_items(&spec.items, &mut self.diagnostics);
    }
}

fn walk_items(items: &[SpecItem<Span>], out: &mut Vec<Diagnostic>) {
    for item in items {
        match item {
            SpecItem::BuildCondition(b) => check_bcond(b, out),
            SpecItem::Conditional(c) => walk_cond(c, out),
            _ => {}
        }
    }
}

fn walk_cond(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<Diagnostic>) {
    for branch in &cond.branches {
        walk_items(&branch.body, out);
    }
    if let Some(els) = &cond.otherwise {
        walk_items(els, out);
    }
}

/// Legacy bcond form encountered in the AST. Carries the rendered
/// token (for diagnostic messages) and the default value that the
/// modern `%bcond NAME DEFAULT` form would need to preserve the same
/// polarity.
#[derive(Debug, Clone, Copy)]
enum LegacyForm {
    With,
    Without,
}

impl LegacyForm {
    fn token(self) -> &'static str {
        match self {
            Self::With => "%bcond_with",
            Self::Without => "%bcond_without",
        }
    }

    fn suggested_default(self) -> &'static str {
        match self {
            // `%bcond_with NAME` defaults to off → modern equivalent is `0`.
            Self::With => "0",
            // `%bcond_without NAME` defaults to on → modern equivalent is `1`.
            Self::Without => "1",
        }
    }
}

fn check_bcond(node: &BuildCondition<Span>, out: &mut Vec<Diagnostic>) {
    let form = match node.style {
        BuildCondStyle::BcondWith => LegacyForm::With,
        BuildCondStyle::BcondWithout => LegacyForm::Without,
        // `%bcond NAME DEFAULT` is already the modern form — skip.
        _ => return,
    };
    let legacy_form = form.token();
    let suggested_default = form.suggested_default();
    let name = &node.name;
    out.push(Diagnostic::new(
        &METADATA,
        Severity::Warn,
        format!(
            "`{legacy_form} {name}` uses the legacy bcond syntax; the active profile ships rpm \
             ≥ 4.17.1, prefer the explicit form `%bcond {name} {suggested_default}`"
        ),
        node.data,
    ));
}

impl Lint for PreferBcondNewSyntax {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        family_supports_modern_bcond(profile)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.enabled = family_supports_modern_bcond(profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn fedora() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        p
    }

    fn run_with(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = PreferBcondNewSyntax::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_bcond_with_on_fedora() {
        let src = "%bcond_with systemd\nName: x\n";
        let diags = run_with(src, &fedora());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM400");
        assert!(diags[0].message.contains("%bcond systemd 0"));
    }

    #[test]
    fn flags_bcond_without_on_fedora() {
        let src = "%bcond_without docs\nName: x\n";
        let diags = run_with(src, &fedora());
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%bcond docs 1"));
    }

    #[test]
    fn silent_for_modern_bcond_form() {
        let src = "%bcond systemd 0\nName: x\n";
        assert!(run_with(src, &fedora()).is_empty());
    }

    #[test]
    fn silent_on_rhel_profile() {
        // RHEL 9 ships rpm 4.16 — modern bcond unavailable.
        let mut p = Profile::default();
        p.identity.family = Some(Family::Rhel);
        let src = "%bcond_with systemd\nName: x\n";
        assert!(run_with(src, &p).is_empty());
    }

    #[test]
    fn silent_on_opensuse_profile() {
        // openSUSE Leap is on rpm 4.14; out of scope.
        let mut p = Profile::default();
        p.identity.family = Some(Family::Opensuse);
        let src = "%bcond_with systemd\nName: x\n";
        assert!(run_with(src, &p).is_empty());
    }

    #[test]
    fn silent_on_alt_profile() {
        // ALT's rpm version varies across branches and isn't uniformly
        // ≥ 4.17.1 — the rule stays silent rather than assuming.
        let mut p = Profile::default();
        p.identity.family = Some(Family::Alt);
        let src = "%bcond_with systemd\nName: x\n";
        assert!(run_with(src, &p).is_empty());
    }

    #[test]
    fn silent_on_generic_profile() {
        let src = "%bcond_with systemd\nName: x\n";
        assert!(run_with(src, &Profile::default()).is_empty());
    }

    #[test]
    fn flags_inside_conditional() {
        let src = "Name: x\n%if 0%{?fedora}\n%bcond_with extra\n%endif\n";
        assert_eq!(run_with(src, &fedora()).len(), 1);
    }

    #[test]
    fn flags_multiple_declarations() {
        let src = "%bcond_with a\n%bcond_without b\n%bcond c 1\nName: x\n";
        let diags = run_with(src, &fedora());
        // First two flagged; `%bcond c 1` already modern.
        assert_eq!(diags.len(), 2, "{diags:?}");
    }
}
