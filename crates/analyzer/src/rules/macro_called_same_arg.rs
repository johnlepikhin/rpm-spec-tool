//! RPM496 `macro-called-always-with-same-argument` — flag parametric
//! macro definitions whose every call site passes the same argument.
//!
//! If a parametric macro is always invoked with one specific value,
//! the parameter is dead weight — the body could just reference the
//! value directly. This is a soft "hint" lint (default `Allow`) because
//! the macro may be public API consumed by other specs.

use std::collections::{HashMap, HashSet};

use rpm_spec::ast::{MacroDefKind, MacroRef, Span, SpecFile, Text, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM496",
    name: "macro-called-always-with-same-argument",
    description: "Parametric macro is called at every site with the same argument — the \
                  parameter may be unnecessary; consider hardcoding the value.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Parametric macro is called at every site with the same argument — the parameter may be unnecessary; consider hardcoding the value.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MacroCalledSameArg {
    diagnostics: Vec<Diagnostic>,
    /// Parametric macros declared in this spec: `name → def span`.
    parametric: HashMap<String, Span>,
    /// Per-name set of canonical argument strings observed across the
    /// spec. If the set has exactly one member after the walk and the
    /// call count is >= 2, the rule fires.
    calls: HashMap<String, (HashSet<String>, usize)>,
}

impl MacroCalledSameArg {
    pub fn new() -> Self {
        Self::default()
    }

    fn record_call(&mut self, m: &MacroRef) {
        if !self.parametric.contains_key(&m.name) {
            return;
        }
        let arg_text = render_args(&m.args);
        let entry = self.calls.entry(m.name.clone()).or_default();
        entry.0.insert(arg_text);
        entry.1 += 1;
    }

    fn record_text(&mut self, t: &Text) {
        for seg in &t.segments {
            if let TextSegment::Macro(m) = seg {
                self.record_call(m);
                for a in &m.args {
                    self.record_text(a);
                }
                if let Some(v) = &m.with_value {
                    self.record_text(v);
                }
            }
        }
    }
}

fn render_args(args: &[Text]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        for seg in &a.segments {
            match seg {
                TextSegment::Literal(s) => out.push_str(s),
                TextSegment::Macro(m) => {
                    use rpm_spec::ast::MacroKind;
                    match m.kind {
                        MacroKind::Braced => out.push_str(&format!("%{{{}}}", m.name)),
                        MacroKind::Plain => out.push_str(&format!("%{}", m.name)),
                        _ => out.push_str(&format!("%{{{}}}", m.name)),
                    }
                }
                _ => {}
            }
        }
    }
    out.trim().to_string()
}

impl<'ast> Visit<'ast> for MacroCalledSameArg {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // First pass: collect parametric defs.
        for it in &spec.items {
            if let rpm_spec::ast::SpecItem::MacroDef(md) = it
                && !matches!(md.kind, MacroDefKind::Undefine)
                && md.opts.is_some()
            {
                self.parametric.insert(md.name.clone(), md.data);
            }
        }
        if self.parametric.is_empty() {
            return;
        }
        // Second pass: count calls + bucket args.
        visit::walk_spec(self, spec);
        for (name, (args, count)) in std::mem::take(&mut self.calls) {
            if count < 2 || args.len() != 1 {
                continue;
            }
            let arg = args.into_iter().next().unwrap();
            if arg.is_empty() {
                continue;
            }
            let Some(span) = self.parametric.get(&name) else {
                continue;
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "parametric macro `{name}` is called {count} times, every time with the \
                         same argument (`{arg}`) — consider hardcoding the value and dropping \
                         the parameter"
                    ),
                    *span,
                )
                .with_suggestion(Suggestion::new(
                    "drop the option list and inline the constant argument into the body",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }

    fn visit_text(&mut self, node: &'ast Text) {
        self.record_text(node);
        visit::walk_text(self, node);
    }
}

impl Lint for MacroCalledSameArg {
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
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<MacroCalledSameArg>(src)
    }

    #[test]
    fn flags_macro_with_constant_arg() {
        let src =
            "Name: x\n%define wrap(x:) echo %{-x}\nSummary: %{wrap foo}\nVersion: %{wrap foo}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM496");
    }

    #[test]
    fn silent_for_distinct_args() {
        let src =
            "Name: x\n%define wrap(x:) echo %{-x}\nSummary: %{wrap foo}\nVersion: %{wrap bar}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_single_call() {
        let src = "Name: x\n%define wrap(x:) echo %{-x}\nSummary: %{wrap foo}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_parametric_macro() {
        // No parametric def → nothing to analyse.
        let src = "Name: x\n%global wrap foo\nSummary: %{wrap}\nVersion: %{wrap}\n";
        assert!(run(src).is_empty());
    }
}
