//! RPM309 `buildarch-reparse-hazard` — flag `%define` / `%global`
//! macros with shell or Lua side-effects that appear *before* a
//! `BuildArch:` tag.
//!
//! When RPM encounters `BuildArch:` it re-parses the entire spec from
//! the top with the new arch in effect. Any `%global` / `%define` line
//! whose body relies on `%(...)` shell expansion or `%{lua:...}` is
//! evaluated **twice**. If the side-effect is non-idempotent — a
//! `date`, a counter increment, a `git rev-parse` — the second
//! evaluation produces a different value than the first, breaking
//! reproducibility or simply giving the wrong answer.
//!
//! The fix is to move the side-effecting macro definition **below**
//! the `BuildArch:` line (the re-parse stops at `BuildArch:`), or
//! restructure to avoid the side effect.
//!
//! Heuristic: any `%global` / `%define` whose body contains a
//! [`MacroKind::Shell`] or [`MacroKind::Lua`] segment, located in
//! source-order before the first `BuildArch:` tag, is flagged. Pure
//! constant macros (`%global foo 1`) are silent.

use rpm_spec::ast::{Conditional, MacroDef, MacroKind, Span, SpecFile, SpecItem, Tag, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM309",
    name: "buildarch-reparse-hazard",
    description: "A `%global` / `%define` with `%(...)` shell or `%{lua:...}` side-effects \
                  appears before `BuildArch:`. RPM re-parses the spec at `BuildArch:`, so the \
                  side effect runs twice and may yield different values. Move the definition \
                  below `BuildArch:` or remove the side effect.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct BuildarchReparseHazard {
    diagnostics: Vec<Diagnostic>,
}

impl BuildarchReparseHazard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for BuildarchReparseHazard {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(buildarch_span) = find_buildarch_span(&spec.items) else {
            return;
        };
        let buildarch_byte = buildarch_span.start_byte;

        let mut suspects: Vec<&MacroDef<Span>> = Vec::new();
        collect_side_effect_macros(&spec.items, &mut suspects);

        for m in suspects {
            if m.data.start_byte < buildarch_byte {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`%{kind} {name}` uses shell or Lua expansion and appears before \
                             `BuildArch:`; the body will be re-evaluated when RPM reparses on \
                             BuildArch, which is unsafe for non-pure side effects",
                            kind = macro_kind_label(m),
                            name = m.name,
                        ),
                        m.data,
                    )
                    .with_label(buildarch_span, "`BuildArch:` declared here"),
                );
            }
        }
    }
}

fn macro_kind_label(m: &MacroDef<Span>) -> &'static str {
    match m.kind {
        rpm_spec::ast::MacroDefKind::Define => "define",
        rpm_spec::ast::MacroDefKind::Global => "global",
        rpm_spec::ast::MacroDefKind::Undefine => "undefine",
        _ => "define",
    }
}

fn find_buildarch_span(items: &[SpecItem<Span>]) -> Option<Span> {
    for item in items {
        match item {
            SpecItem::Preamble(p) if matches!(p.tag, Tag::BuildArch) => return Some(p.data),
            SpecItem::Conditional(c) => {
                if let Some(s) = find_buildarch_in_conditional(c) {
                    return Some(s);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_buildarch_in_conditional(cond: &Conditional<Span, SpecItem<Span>>) -> Option<Span> {
    for branch in &cond.branches {
        if let Some(s) = find_buildarch_span(&branch.body) {
            return Some(s);
        }
    }
    if let Some(els) = &cond.otherwise
        && let Some(s) = find_buildarch_span(els)
    {
        return Some(s);
    }
    None
}

fn collect_side_effect_macros<'a>(items: &'a [SpecItem<Span>], out: &mut Vec<&'a MacroDef<Span>>) {
    for item in items {
        match item {
            SpecItem::MacroDef(m) if macro_body_has_side_effects(m) => {
                out.push(m);
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    collect_side_effect_macros(&branch.body, out);
                }
                if let Some(els) = &c.otherwise {
                    collect_side_effect_macros(els, out);
                }
            }
            _ => {}
        }
    }
}

fn macro_body_has_side_effects(m: &MacroDef<Span>) -> bool {
    m.body.segments.iter().any(|seg| {
        let TextSegment::Macro(inner) = seg else {
            return false;
        };
        matches!(inner.kind, MacroKind::Shell | MacroKind::Lua)
    })
}

impl Lint for BuildarchReparseHazard {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = BuildarchReparseHazard::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_shell_macro_before_buildarch() {
        let src = "Name: x\n\
%global builddate %(date +%%Y%%m%%d)\n\
Version: 1\n\
BuildArch: noarch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM309");
        assert!(diags[0].message.contains("builddate"));
    }

    #[test]
    fn flags_lua_macro_before_buildarch() {
        let src = "Name: x\n\
%global counter %{lua:print(os.time())}\n\
BuildArch: noarch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("counter"));
    }

    #[test]
    fn silent_when_macro_appears_after_buildarch() {
        let src = "Name: x\n\
BuildArch: noarch\n\
%global builddate %(date +%%Y%%m%%d)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_pure_macro_before_buildarch() {
        let src = "Name: x\n\
%global counter 42\n\
BuildArch: noarch\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_buildarch_absent() {
        // No BuildArch ⇒ no reparse hazard.
        let src = "Name: x\n%global builddate %(date)\nVersion: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_define_with_shell_expansion() {
        let src = "Name: x\n\
%define ts %(date +%%s)\n\
BuildArch: noarch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }
}
