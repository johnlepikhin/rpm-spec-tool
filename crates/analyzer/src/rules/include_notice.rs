//! RPM406 `include-not-expanded` — `%include path` directive present
//! in the spec.
//!
//! The parser keeps `%include` lines verbatim and does **not** load
//! the referenced file. Consequently, all other rules that reason
//! about declared `BuildRequires`, declared bconds, available
//! sections, or anything else "global" run with an incomplete view —
//! whatever the include contributes is invisible. Rules will
//! optimistically not fire (false negatives) for content present in
//! the included file.
//!
//! This rule emits a single per-include notice so the user knows the
//! lint output may be incomplete. It is **opt-in informational**:
//! [`Severity::Allow`] is the default, meaning the lint is *skipped
//! entirely* by [`crate::session::LintSession::run`] (the session
//! treats `Allow` as silenced and never calls `take_diagnostics`).
//! Users who want the notice enable it via config or
//! `--warn=include-not-expanded` / `--warn=RPM406`. A future
//! repository-level analyzer (Phase 25.9) will resolve the include
//! and lift the limitation. The diagnostic is anchored at the include
//! line so editor-jump lands on it.

use rpm_spec::ast::{
    ConditionalMacro, IncludeDirective, Span, SpecFile, SpecItem, Text, TextSegment,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM406",
    name: "include-not-expanded",
    description: "`%include path` directive — the analyzer does not follow includes, so other \
                  rules see only the visible spec. Findings may be incomplete for symbols \
                  defined inside the included file.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Lint state for RPM406.
#[derive(Debug, Default)]
pub struct IncludeNotExpanded {
    diagnostics: Vec<Diagnostic>,
}

impl IncludeNotExpanded {
    /// Construct an empty lint instance with no diagnostics buffered.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for IncludeNotExpanded {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        walk_items(&spec.items, &mut self.diagnostics);
    }
}

fn walk_items(items: &[SpecItem<Span>], out: &mut Vec<Diagnostic>) {
    for item in items {
        match item {
            SpecItem::Include(inc) => emit(inc, out),
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_items(&branch.body, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_items(els, out);
                }
            }
            _ => {}
        }
    }
}

fn emit(inc: &IncludeDirective<Span>, out: &mut Vec<Diagnostic>) {
    let path_label = render_path_verbatim(&inc.path);
    out.push(Diagnostic::new(
        &METADATA,
        Severity::Allow,
        format!(
            "`%include {path_label}` is not followed by the analyzer; rules checking declared \
             symbols, sections, or bconds may miss anything defined in the included file"
        ),
        inc.data,
    ));
}

/// Best-effort verbatim render of a `Text` path: literal segments are
/// emitted as-is, macro segments as `%{name}` (or `%{?name}` /
/// `%{!?name}` for conditional references). Mirrors
/// [`crate::shell::tokens::ShellToken::render_verbatim`] so diagnostic
/// messages never silently drop macros from the include path. Macro
/// arguments and `:default` bodies are not reconstructed — for an
/// `%include` path the bare `%{name}` form is unambiguous enough and
/// avoids re-implementing the full `Text` pretty-printer here.
fn render_path_verbatim(path: &Text) -> String {
    let mut out = String::new();
    for seg in &path.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => {
                let prefix = match m.conditional {
                    ConditionalMacro::None => "",
                    ConditionalMacro::IfDefined => "?",
                    ConditionalMacro::IfNotDefined => "!?",
                    // `ConditionalMacro` is `#[non_exhaustive]`. Any
                    // variant added upstream falls back to the bare
                    // `%{name}` form — better than failing to compile
                    // and acceptable for a diagnostic-only message.
                    _ => "",
                };
                out.push_str("%{");
                out.push_str(prefix);
                out.push_str(&m.name);
                out.push('}');
            }
            _ => {}
        }
    }
    if out.is_empty() {
        // Defensive: the parser should not produce an empty include
        // path, but if it does, fall back to a recognisable marker so
        // the diagnostic message is still readable.
        out.push_str("<empty path>");
    }
    out
}

impl Lint for IncludeNotExpanded {
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
        let mut lint = IncludeNotExpanded::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_each_include() {
        let src = "Name: x\n%include common.spec\n%include macros.inc\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM406");
    }

    #[test]
    fn silent_on_clean_spec() {
        let src = "Name: x\nVersion: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_include_inside_conditional() {
        let src = "Name: x\n%if 0%{?fedora}\n%include fedora.inc\n%endif\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn diagnostic_carries_path() {
        let src = "Name: x\n%include path/to/foo.spec\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("path/to/foo.spec"));
    }

    #[test]
    fn flags_include_with_macro_path() {
        let src = "Name: x\n%include %{_sourcedir}/common.inc\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        // The diagnostic should mention the include's path verbatim
        // (including the unresolved macro), not the generic placeholder.
        assert!(
            diags[0].message.contains("_sourcedir") || diags[0].message.contains("common.inc"),
            "message should surface the include path: {}",
            diags[0].message
        );
    }
}
