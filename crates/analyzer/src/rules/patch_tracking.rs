//! RPM064 `patch-defined-not-applied` — flag `PatchN:` tags whose
//! patch never gets applied in `%prep`.
//!
//! Recognised application forms:
//! - `%patch -P N` / `%patch -PN` — explicit numbered application
//! - `%patchN` — legacy numbered form
//! - `%autopatch` or `%autosetup` — applies all declared patches
//!
//! Bail-out behaviour:
//! - If `%prep` is missing entirely, the rule stays silent (the spec
//!   has bigger problems — `missing-prep-section` covers that).
//! - If `%autopatch`/`%autosetup` is invoked, the rule treats every
//!   declared patch as applied.
//! - If a `%patch` call passes a macro for its number, the rule
//!   conservatively considers that call to apply *all* patches (we
//!   can't statically resolve which one).

use std::collections::HashSet;

use rpm_spec::ast::{Section, ShellBody, Span, SpecFile, SpecItem, Text, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{
    MACRO_AUTOPATCH, MACRO_AUTOSETUP, MACRO_PATCH_PREFIX, collect_declared_patches,
};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM064",
    name: "patch-defined-not-applied",
    description:
        "`PatchN:` is declared but never applied in `%prep`; declare or apply, don't dangle.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct PatchDefinedNotApplied {
    diagnostics: Vec<Diagnostic>,
}

impl PatchDefinedNotApplied {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for PatchDefinedNotApplied {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let declared = collect_declared_patches(spec);
        if declared.is_empty() {
            return;
        }
        let Some(prep_body) = find_prep_body(spec) else {
            // No `%prep` — `missing-prep-section` will already warn.
            return;
        };
        let applied = collect_applied(prep_body, declared.len());
        for decl in declared {
            if !applied.matches(decl.number) {
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!("Patch{} is declared but not applied in %prep", decl.number),
                    decl.span,
                ));
            }
        }
    }
}

impl Lint for PatchDefinedNotApplied {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

/// Locate the first top-level `%prep` body. Subpackage prep sections
/// don't exist in RPM (it's a global step), so a flat scan suffices.
fn find_prep_body(spec: &SpecFile<Span>) -> Option<&ShellBody> {
    for item in &spec.items {
        if let SpecItem::Section(boxed) = item
            && let Section::BuildScript { kind, body, .. } = boxed.as_ref()
            && *kind == rpm_spec::ast::BuildScriptKind::Prep
        {
            return Some(body);
        }
    }
    None
}

/// Result of scanning a `%prep` body. `All` short-circuits the check
/// (every declared patch is treated as applied); `Set` keeps the
/// explicit numbers we saw.
#[derive(Debug)]
enum Applied {
    All,
    Set(HashSet<u32>),
}

impl Applied {
    fn matches(&self, n: u32) -> bool {
        match self {
            Applied::All => true,
            Applied::Set(s) => s.contains(&n),
        }
    }
}

/// Effect of scanning one `%prep` line on the running `Applied::Set`.
enum LineEffect {
    /// `%autopatch` / `%autosetup` / `%patch -P %{macro}` seen — every
    /// declared patch counts as applied. The caller must short-circuit.
    ShortCircuitAll,
    /// No short-circuit; any numbers collected stay in the running set.
    Continue,
}

fn collect_applied(body: &ShellBody, declared_count: usize) -> Applied {
    let mut set: HashSet<u32> = HashSet::with_capacity(declared_count);
    for line in &body.lines {
        if matches!(scan_line(line, &mut set), LineEffect::ShortCircuitAll) {
            return Applied::All;
        }
    }
    Applied::Set(set)
}

/// Walk one `%prep` line; record explicit patch numbers and signal
/// short-circuit to [`Applied::All`] when warranted.
fn scan_line(line: &Text, applied: &mut HashSet<u32>) -> LineEffect {
    for (i, seg) in line.segments.iter().enumerate() {
        let TextSegment::Macro(m) = seg else { continue };
        let name = m.name.as_str();
        if name == MACRO_AUTOPATCH || name == MACRO_AUTOSETUP {
            return LineEffect::ShortCircuitAll;
        }
        if let Some(rest) = name.strip_prefix(MACRO_PATCH_PREFIX) {
            if rest.is_empty() {
                // `%patch ...` — look at trailing literals for `-P N`.
                let trailing = &line.segments[i + 1..];
                match parse_dash_p(trailing) {
                    DashP::Number(n) => {
                        applied.insert(n);
                    }
                    DashP::Missing => {
                        // Bare `%patch` is treated as `%patch -P 0` for
                        // legacy compatibility (older RPM). Modern RPM
                        // ≥ 4.18 deprecates this form but accepts it;
                        // mapping to patch 0 keeps the rule silent for
                        // legacy spec files instead of producing a
                        // false positive.
                        applied.insert(0);
                    }
                    DashP::Macro => {
                        return LineEffect::ShortCircuitAll;
                    }
                }
            } else if let Ok(n) = rest.parse::<u32>() {
                applied.insert(n);
            }
        }
    }
    LineEffect::Continue
}

enum DashP {
    Number(u32),
    Missing,
    Macro,
}

/// Look for `-P N` (two tokens) or `-PN` (one token) in the literal
/// text of `trailing`. A macro segment in `trailing` returns
/// `DashP::Macro` — we can't resolve macro-substituted patch numbers
/// statically.
fn parse_dash_p(trailing: &[TextSegment]) -> DashP {
    let mut text = String::new();
    for seg in trailing {
        match seg {
            TextSegment::Literal(s) => text.push_str(s),
            TextSegment::Macro(_) => return DashP::Macro,
            // `TextSegment` is `#[non_exhaustive]`; treat any future
            // variant the same as a macro — we can't resolve it
            // statically, so the caller treats the whole `%patch` call
            // as a global application.
            _ => return DashP::Macro,
        }
    }
    let mut tokens = text.split_ascii_whitespace();
    while let Some(t) = tokens.next() {
        if t == "-P" {
            if let Some(next) = tokens.next()
                && let Ok(n) = next.parse::<u32>()
            {
                return DashP::Number(n);
            }
        } else if let Some(rest) = t.strip_prefix("-P")
            && let Ok(n) = rest.parse::<u32>()
        {
            return DashP::Number(n);
        }
    }
    DashP::Missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = PatchDefinedNotApplied::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_unapplied_patch() {
        let src = "Name: x\nPatch1: foo.patch\n%prep\n%setup -q\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM064");
        assert!(diags[0].message.contains("Patch1"));
    }

    #[test]
    fn silent_when_patch_applied_via_dash_p() {
        let src = "Name: x\nPatch1: foo.patch\n%prep\n%setup -q\n%patch -P 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_patch_applied_via_dash_p_joined() {
        // `-P1` without a space — also valid.
        let src = "Name: x\nPatch1: foo.patch\n%prep\n%setup -q\n%patch -P1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_legacy_patch_form() {
        // `%patch1` — legacy numbered form.
        let src = "Name: x\nPatch1: foo.patch\n%prep\n%setup -q\n%patch1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_autopatch_present() {
        let src = "Name: x\nPatch1: a.patch\nPatch2: b.patch\n%prep\n%setup -q\n%autopatch\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_autosetup_present() {
        let src = "Name: x\nPatch1: a.patch\n%prep\n%autosetup\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_only_unapplied_when_some_applied() {
        let src = "Name: x\nPatch1: a.patch\nPatch2: b.patch\nPatch3: c.patch\n\
                   %prep\n%setup -q\n%patch1\n%patch -P 3\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("Patch2"));
    }

    #[test]
    fn silent_when_no_prep_section() {
        // No %prep => let RPM016 missing-prep-section handle it.
        let src = "Name: x\nPatch1: foo.patch\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_patches_declared() {
        let src = "Name: x\n%prep\n%setup -q\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_dash_p_argument_is_macro() {
        // `-P %{somenum}` — conservative: assume all applied.
        let src = "Name: x\nPatch1: a.patch\nPatch2: b.patch\n\
                   %prep\n%setup -q\n%patch -P %{somenum}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_unapplied_unnumbered_patch() {
        // Bare `Patch:` maps to Patch0. Without any application it's
        // unapplied and should be flagged.
        let src = "Name: x\nPatch: foo.patch\n%prep\n%setup -q\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("Patch0"), "got {}", diags[0].message);
    }

    #[test]
    fn silent_when_unnumbered_patch_applied_as_zero() {
        // Bare `%patch` (no `-P`) is treated as patch-0 application —
        // pairs with the unnumbered `Patch:` declaration.
        let src = "Name: x\nPatch: foo.patch\n%prep\n%setup -q\n%patch\n";
        assert!(run(src).is_empty());
    }
}
