//! Collect every macro name referenced from a parsed spec.
//!
//! The result drives `matrix portability` — for every used name we
//! look it up in each member profile's [`MacroRegistry`] and report
//! which profiles can resolve it.
//!
//! What we collect:
//!
//! * `%foo`, `%{foo}`, `%{?foo}`, `%{!?foo}`, `%{?foo:default}` — all
//!   forms that refer by name. Recursed into via the existing
//!   [`crate::visit::Visit`] machinery, so nested args and `with_value`
//!   bodies are covered automatically.
//!
//! What we skip:
//!
//! * Positional placeholders (`%1`–`%9`, `%*`, `%**`, `%#`).
//! * Flag refs (`%{-f}`, `%{-f*}`).
//! * Builtin function macros (`%{shrink:...}`, `%{lua:...}`, `%(sh)`,
//!   `%[expr]`). These are language constructs, not user-defined
//!   macros — they can't be missing from a profile.
//!
//! Definitions (`%define`, `%global`, `%bcond`) and macro-def bodies
//! are walked too — the names they USE inside the body still count
//! as usage, since a profile that doesn't define those names will
//! fail to evaluate the body.

use std::collections::BTreeSet;

use rpm_spec::ast::{
    CondExpr, Conditional, ExprAst, FilesContent, MacroKind, MacroRef, PreambleContent, Span,
    SpecFile, SpecItem,
};

use crate::visit::{
    Visit, walk_files_conditional, walk_macro_ref, walk_preamble_conditional,
    walk_top_conditional,
};

/// Visitor that records every user-meaningful macro name referenced
/// in a spec. Names are stored in a [`BTreeSet`] so the output is
/// deterministic and matches `matrix portability` table order.
#[derive(Debug, Default)]
pub struct MacroUsageCollector {
    names: BTreeSet<String>,
}

impl MacroUsageCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Walk `spec` and return the set of macro names it references.
    /// One-shot convenience for callers that don't need to inspect
    /// the visitor state.
    pub fn collect(spec: &SpecFile<Span>) -> BTreeSet<String> {
        let mut me = Self::new();
        me.visit_spec(spec);
        me.into_names()
    }

    /// Consume the visitor and return the accumulated names.
    pub fn into_names(self) -> BTreeSet<String> {
        self.names
    }

    /// Record every macro reference inside a `%if EXPR`-style branch
    /// header. The default `walk_*_conditional` only recurses into
    /// branch bodies, not the conditions themselves — so without
    /// this hook a spec like `%if %{distro_version} >= 9` would
    /// silently hide its dependency on `distro_version` from the
    /// portability report.
    fn record_cond_expr(&mut self, expr: &CondExpr<Span>) {
        match expr {
            CondExpr::Raw(text) => self.visit_text(text),
            CondExpr::Parsed(boxed) => self.record_expr_ast(boxed),
            CondExpr::ArchList(texts) => {
                for t in texts {
                    self.visit_text(t);
                }
            }
            // CondExpr is #[non_exhaustive]; any future kind that
            // can hold macros should add an explicit arm — the
            // wildcard is the safe default for new variants that
            // can't.
            _ => {}
        }
    }

    /// Walk a parsed condition expression, harvesting macro names.
    /// `ExprAst::Macro` stores the verbatim source token (e.g.
    /// `"%{?systemd}"`); we trim it back to the bare name so the
    /// portability lookup can compare against the registry.
    fn record_expr_ast(&mut self, expr: &ExprAst<Span>) {
        match expr {
            ExprAst::Macro { text, .. } => {
                if let Some(name) = macro_name_from_verbatim(text) {
                    self.names.insert(name);
                }
            }
            ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => {
                self.record_expr_ast(inner);
            }
            ExprAst::Binary { lhs, rhs, .. } => {
                self.record_expr_ast(lhs);
                self.record_expr_ast(rhs);
            }
            ExprAst::Integer { .. } | ExprAst::String { .. } | ExprAst::Identifier { .. } => {}
            // ExprAst is #[non_exhaustive] in the rpm-spec crate;
            // any future leaf variant that doesn't contain macros
            // can stay silent here, the worst case is a missed
            // reference (visible as a partial portability report).
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for MacroUsageCollector {
    fn visit_macro_ref(&mut self, node: &'ast MacroRef) {
        if is_user_macro(node) && !self.names.contains(node.name.as_str()) {
            // contains() + clone() avoids the allocation for the
            // common case where the same macro is referenced
            // multiple times (e.g. `%{_libdir}` in dozens of
            // `%files` lines).
            self.names.insert(node.name.clone());
        }
        // Always recurse — args and with_value bodies may contain
        // further references regardless of whether the outer ref
        // was user-meaningful.
        walk_macro_ref(self, node);
    }

    fn visit_statement(&mut self, node: &'ast MacroRef) {
        // The default visit_statement only walks children; it
        // doesn't call visit_macro_ref on the statement itself,
        // so top-level statement macros (e.g. `%dump`, `%trace`)
        // would slip through. Delegate explicitly.
        self.visit_macro_ref(node);
    }

    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for branch in &node.branches {
            self.record_cond_expr(&branch.expr);
        }
        walk_top_conditional(self, node);
    }

    fn visit_preamble_conditional(
        &mut self,
        node: &'ast Conditional<Span, PreambleContent<Span>>,
    ) {
        for branch in &node.branches {
            self.record_cond_expr(&branch.expr);
        }
        walk_preamble_conditional(self, node);
    }

    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        for branch in &node.branches {
            self.record_cond_expr(&branch.expr);
        }
        walk_files_conditional(self, node);
    }
}

/// Pull the bare name out of a verbatim macro reference token like
/// `%foo`, `%{foo}`, `%{?foo}`, `%{!?foo}`, `%{?foo:default}`. The
/// parser stores `ExprAst::Macro.text` as the raw source; the
/// expression sub-tree doesn't model the breakdown.
fn macro_name_from_verbatim(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix('%')?;
    let body = match inner.strip_prefix('{') {
        Some(rest) => rest.strip_suffix('}')?,
        None => inner,
    };
    // Strip conditional `!`/`?` prefixes (in either order) before
    // the name.
    let mut s = body;
    if let Some(rest) = s.strip_prefix('!') {
        s = rest;
    }
    if let Some(rest) = s.strip_prefix('?') {
        s = rest;
    }
    // Name ends at the first non-ident char (`:` for default-value
    // form, whitespace for parametric forms, etc.).
    let name_end = s
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(s.len());
    let name = &s[..name_end];
    (!name.is_empty()).then(|| name.to_string())
}

/// True for macro references whose resolvability depends on the
/// profile's macro registry. False for positional/flag/builtin —
/// those are language constructs, not registered macros.
fn is_user_macro(m: &MacroRef) -> bool {
    if matches!(
        m.kind,
        MacroKind::Builtin(_) | MacroKind::Shell | MacroKind::Expr | MacroKind::Lua
    ) {
        return false;
    }
    if m.positional_index().is_some()
        || m.is_all_positional()
        || m.is_all_args()
        || m.is_arg_count()
        || m.flag_ref().is_some()
    {
        return false;
    }
    !m.name.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn names_of(src: &str) -> BTreeSet<String> {
        let outcome = crate::session::parse(src);
        MacroUsageCollector::collect(&outcome.spec)
    }

    #[test]
    fn collects_braced_and_plain_refs() {
        let names = names_of(
            "\
Name:    foo
Version: %{ver}
Release: 1
Summary: %name
License: MIT

%description
%{?desc}

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(names.contains("ver"));
        assert!(names.contains("name"));
        assert!(names.contains("desc"));
    }

    #[test]
    fn skips_positional_and_flag_refs() {
        let names = names_of(
            "\
%define wrap() %{*}
%define greet() %1
%define check() %#
%define long() %{**}
%define flag() %{-f*}

Name:    foo
Version: 1
Release: 1
Summary: S
License: MIT

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        // The placeholders inside the define bodies are positional
        // markers, not user macros — they must NOT contaminate the
        // portability report.
        assert!(!names.contains("*"));
        assert!(!names.contains("**"));
        assert!(!names.contains("1"));
        assert!(!names.contains("#"));
        assert!(!names.contains("-f*"));
    }

    #[test]
    fn skips_builtin_kinds() {
        // Builtins like %{shrink:...}, %(sh), %[expr], %{lua:...}
        // are language constructs; they cannot be missing from a
        // profile. Whatever appears INSIDE the construct (`foo` in
        // `%{shrink:%foo}`) is still recorded though.
        let names = names_of(
            "\
Name:    foo
Version: 1
Release: 1
Summary: %{shrink:%foo}
License: MIT

%description
%(echo hi)

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(names.contains("foo"));
        // The builtin name itself ("shrink") is the kind, not the
        // ref name — it should not appear.
        assert!(!names.contains("shrink"));
    }

    #[test]
    fn records_conditional_default_value_body() {
        // `%{?foo:%bar}` — both foo and bar are user macros.
        let names = names_of(
            "\
Name:    foo
Version: %{?foo:%bar}
Release: 1
Summary: S
License: MIT

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(names.contains("foo"));
        assert!(names.contains("bar"));
    }

    #[test]
    fn macro_name_from_verbatim_handles_common_forms() {
        // ExprAst::Macro.text is the raw source token; our parser
        // peels the surface forms back to the registry-lookup name.
        assert_eq!(macro_name_from_verbatim("%foo"), Some("foo".to_string()));
        assert_eq!(macro_name_from_verbatim("%{foo}"), Some("foo".to_string()));
        assert_eq!(macro_name_from_verbatim("%{?foo}"), Some("foo".to_string()));
        assert_eq!(macro_name_from_verbatim("%{!?foo}"), Some("foo".to_string()));
        assert_eq!(
            macro_name_from_verbatim("%{?foo:default}"),
            Some("foo".to_string())
        );
        assert_eq!(macro_name_from_verbatim(""), None);
        assert_eq!(macro_name_from_verbatim("not-a-macro"), None);
    }

    #[test]
    fn empty_spec_yields_empty_set() {
        let names = names_of("");
        assert!(names.is_empty());
    }

    #[test]
    fn records_macros_in_top_level_if_condition() {
        // Without explicit conditional-expr walking, names used in
        // `%if EXPR` slip through the default visit_*_conditional
        // (it only recurses into bodies). This test pins the
        // contract that distro_version IS collected.
        let names = names_of(
            "\
Name:    foo
Version: 1
Release: 1
Summary: S
License: MIT

%description
B

%if %{distro_version} >= 9
Requires: foo
%endif

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(
            names.contains("distro_version"),
            "expected `distro_version` in {names:?}"
        );
    }

    #[test]
    fn records_macros_in_preamble_if_condition() {
        // `%if` inside the preamble walks through visit_preamble_conditional —
        // separate code path from the top-level case.
        let names = names_of(
            "\
Name:    foo
Version: 1
Release: 1
Summary: S
License: MIT
%if %{?with_systemd}
Requires: systemd
%endif

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(
            names.contains("with_systemd"),
            "expected `with_systemd` in {names:?}"
        );
    }

    #[test]
    fn records_macros_in_ifarch_list() {
        // `%ifarch %{ix86} x86_64` — names inside the arch list
        // (Text segments) must be harvested too.
        let names = names_of(
            "\
Name:    foo
Version: 1
Release: 1
Summary: S
License: MIT

%description
B

%ifarch %{ix86} x86_64
%global flag 1
%endif

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(names.contains("ix86"), "expected `ix86` in {names:?}");
    }

    #[test]
    fn dedups_repeated_refs() {
        let names = names_of(
            "\
Name:    foo
Version: %{_libdir}
Release: 1%{dist}
Summary: S
License: MIT

%description
%{_libdir} again

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert_eq!(names.iter().filter(|n| *n == "_libdir").count(), 1);
        assert!(names.contains("_libdir"));
        assert!(names.contains("dist"));
    }
}
