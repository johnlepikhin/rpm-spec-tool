//! RPM493 `macro-alias-of-builtin` — flag `%global foo %{_bindir}`
//! patterns where the body is just a reference to a well-known RPM
//! macro.
//!
//! The alias hides the canonical name (`%{_bindir}`, `%{_datadir}`, …)
//! that everyone who reads spec files recognises, and creates an
//! extra hop for tooling that resolves paths. Reference the well-known
//! macro directly.

use rpm_spec::ast::{MacroDef, MacroDefKind, MacroKind, Span, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM493",
    name: "macro-alias-of-builtin",
    description: "`%global NAME %{builtin}` aliases a well-known RPM macro — reference the \
                  builtin directly instead of hiding it behind a local alias.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Well-known RPM macros that aliasing buys nothing for. Limited to
/// canonical path macros plus the identity tags every spec exposes.
const WELL_KNOWN: &[&str] = &[
    "_bindir",
    "_sbindir",
    "_libdir",
    "_libexecdir",
    "_includedir",
    "_datadir",
    "_mandir",
    "_infodir",
    "_localstatedir",
    "_sharedstatedir",
    "_sysconfdir",
    "_unitdir",
    "_userunitdir",
    "_tmpfilesdir",
    "_sysusersdir",
    "_docdir",
    "_defaultlicensedir",
    "_prefix",
    "_exec_prefix",
    "_localedir",
    "buildroot",
    "name",
    "version",
    "release",
    "epoch",
];

/// `%global NAME %{builtin}` aliases a well-known RPM macro — reference the builtin directly instead of hiding it behind a local alias.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MacroAliasOfBuiltin {
    diagnostics: Vec<Diagnostic>,
}

impl MacroAliasOfBuiltin {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MacroAliasOfBuiltin {
    fn visit_macro_def(&mut self, node: &'ast MacroDef<Span>) {
        if matches!(node.kind, MacroDefKind::Undefine) {
            return;
        }
        // Names beginning with `__` are reserved for RPM-internal
        // configuration knobs (e.g. `__requires_exclude_from`,
        // `__find_requires`). Assigning a builtin path to them is a
        // legitimate config override, not a redundant alias.
        if node.name.starts_with("__") {
            return;
        }
        let Some(target) = body_is_single_macro(node) else {
            return;
        };
        if !WELL_KNOWN.contains(&target.as_str()) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`{kind} {name} %{{{target}}}` aliases a well-known macro — use `%{{{target}}}` \
                     directly",
                    kind = match node.kind {
                        MacroDefKind::Global => "%global",
                        MacroDefKind::Define => "%define",
                        _ => "%global",
                    },
                    name = node.name,
                ),
                node.data,
            )
            .with_suggestion(Suggestion::new(
                format!("drop the `{name}` alias and reference `%{{{target}}}` at every call site",
                    name = node.name),
                Vec::new(),
                Applicability::Manual,
            )),
        );
        visit::walk_macro_def(self, node);
    }
}

/// Returns `Some(NAME)` when the macro body is exactly `%{NAME}` (with
/// optional surrounding whitespace) and `NAME` is a plain/braced macro
/// with no args or conditional flags. Returns `None` otherwise.
fn body_is_single_macro(node: &MacroDef<Span>) -> Option<String> {
    use rpm_spec::ast::ConditionalMacro;
    // Filter out whitespace-only Literal segments around a single
    // Macro segment.
    let mut macros = Vec::new();
    for seg in &node.body.segments {
        match seg {
            TextSegment::Literal(s) if !s.trim().is_empty() => {
                return None;
            }
            TextSegment::Literal(_) => {}
            TextSegment::Macro(m) => macros.push(m),
            _ => return None,
        }
    }
    if macros.len() != 1 {
        return None;
    }
    let m = macros[0];
    if !matches!(m.kind, MacroKind::Plain | MacroKind::Braced) {
        return None;
    }
    if !matches!(m.conditional, ConditionalMacro::None) {
        return None;
    }
    if !m.args.is_empty() || m.with_value.is_some() {
        return None;
    }
    Some(m.name.clone())
}

impl Lint for MacroAliasOfBuiltin {
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
        run_lint::<MacroAliasOfBuiltin>(src)
    }

    #[test]
    fn flags_alias_of_bindir() {
        let src = "Name: x\n%global bindir %{_bindir}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM493");
    }

    #[test]
    fn flags_define_alias_of_name() {
        let src = "Name: x\n%define pkgname %{name}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_double_underscore_prefix() {
        // `__requires_exclude_from` is an RPM-internal config knob, not
        // an alias of `_docdir`.
        let src = "Name: x\n%global __requires_exclude_from %{_docdir}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_alias_body() {
        let src = "Name: x\n%global pkgname my-thing\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_alias_of_unknown_macro() {
        let src = "Name: x\n%global pkgname %{some_local_macro}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_composite_body() {
        // Body has more than just the macro — not a pure alias.
        let src = "Name: x\n%global bindir2 %{_bindir}/extra\n";
        assert!(run(src).is_empty());
    }
}
