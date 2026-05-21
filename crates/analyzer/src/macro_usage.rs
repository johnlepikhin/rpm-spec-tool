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
    CondExpr, Conditional, ConditionalMacro, ExprAst, FilesContent, MacroKind, MacroRef,
    PreambleContent, Span, SpecFile, SpecItem,
};

use crate::visit::{
    Visit, walk_files_conditional, walk_macro_ref, walk_preamble_conditional, walk_top_conditional,
};

/// RPM control-flow and section-marker keywords that the parser may
/// surface as `MacroRef` nodes in some grammar contexts (`%if`,
/// `%else`, `%endif`, `%files`, …). They are not user-defined macros
/// — no profile registry can resolve them — so a `matrix portability`
/// report flagging them as `missing on all profiles` is noise, not
/// signal. Filter at the collector boundary.
///
/// The proper fix is parser-side (the rpm-spec crate should not
/// emit `MacroRef` for these tokens), but a name-based filter here
/// is contained to one crate and unblocks operator-facing output
/// today.
const RESERVED_KEYWORDS: &[&str] = &[
    // Conditional directives
    "if",
    "elif",
    "else",
    "endif",
    "ifarch",
    "ifnarch",
    "ifos",
    "ifnos",
    // Section markers
    "package",
    "description",
    "prep",
    "setup",
    "patch",
    "autopatch",
    "build",
    "install",
    "check",
    "files",
    "changelog",
    "clean",
    // Scriptlet sections
    "post",
    "postun",
    "pre",
    "preun",
    "pretrans",
    "posttrans",
    "preuntrans",
    "postuntrans",
    "verifyscript",
    // Trigger / filetrigger family
    "triggerin",
    "triggerun",
    "triggerpostun",
    "triggerprein",
    "trigger",
    "filetriggerin",
    "filetriggerun",
    "filetriggerpostun",
    "filetriggerprein",
    "transfiletriggerin",
    "transfiletriggerun",
    "transfiletriggerpostun",
    "transfiletriggerprein",
    // Definition keywords (the directive itself, not the name being defined)
    "define",
    "global",
    "undefine",
    "bcond",
    "bcond_with",
    "bcond_without",
    "include",
    "load",
];

/// RPM auto-defines from the preamble: the build process injects
/// these without any showrc declaration, so no profile registry has
/// them either. Reporting `%{name}` / `%{version}` / `%{SOURCE0}` as
/// `missing on all 23 profiles` is doubly wrong — they're defined,
/// just not in a place this analyzer can see.
///
/// `SOURCE\d+`, `PATCH\d+`, `NOSOURCE\d+`, `NOPATCH\d+` are matched
/// by prefix-and-tail-digits in [`is_rpm_auto_macro`] so the list
/// stays manageable.
const RPM_AUTO_MACROS: &[&str] = &[
    "name",
    "version",
    "release",
    "epoch",
    "arch",
    "os",
    "vendor",
    "packager",
    "license",
    "summary",
    "description",
    "buildroot",
    "buildsubdir",
];

/// Visitor that records every user-meaningful macro name referenced
/// in a spec. Names are stored in a [`BTreeSet`] so the output is
/// deterministic and matches `matrix portability` table order.
#[derive(Debug, Default)]
pub struct MacroUsageCollector {
    names: BTreeSet<String>,
    /// Names defined locally in the spec via `%global` / `%define` /
    /// `%bcond_*`. Tracked separately because portability classifies
    /// them very differently from external references: the spec
    /// itself provides the macro, so no profile-level "missing"
    /// finding makes sense.
    local_defs: BTreeSet<String>,
}

impl MacroUsageCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Walk `spec` and return the set of macro names it references.
    /// References to spec-local `%global`/`%define` names are
    /// excluded — they're defined by the spec itself and would
    /// pollute portability reports as false-positive "missing"
    /// entries. Callers that need the unfiltered set, or the
    /// local-defs set in parallel, should use
    /// [`Self::collect_with_local_defs`] instead.
    pub fn collect(spec: &SpecFile<Span>) -> BTreeSet<String> {
        let (refs, _) = Self::collect_with_local_defs(spec);
        refs
    }

    /// Walk `spec` and return `(references, local_defs)`. References
    /// is the cleaned, portability-relevant set: it excludes
    /// reserved keywords ([`RESERVED_KEYWORDS`]), RPM auto-defines
    /// ([`RPM_AUTO_MACROS`] plus `SOURCE\d+` / `PATCH\d+`), and
    /// names that the spec itself defines locally. `local_defs` is
    /// the raw set of names introduced by `%global` / `%define` /
    /// `%bcond_*` — exposed so renderers can show a "spec-local
    /// macros: N" rollup if useful.
    pub fn collect_with_local_defs(spec: &SpecFile<Span>) -> (BTreeSet<String>, BTreeSet<String>) {
        let mut me = Self::new();
        me.collect_local_defs(spec);
        me.visit_spec(spec);
        let mut refs = me.names;
        // Subtract spec-local definitions: the spec is its own
        // authority for those names.
        for n in &me.local_defs {
            refs.remove(n);
        }
        (refs, me.local_defs)
    }

    /// Pre-scan top-level items for `%global` / `%define` / `%bcond_*`
    /// definitions. Two-pass design keeps the existing `Visit`
    /// traversal unchanged and avoids reordering pitfalls (a `%global
    /// foo` after `%{?foo}` still cancels the reference).
    fn collect_local_defs(&mut self, spec: &SpecFile<Span>) {
        for item in &spec.items {
            self.scan_item_for_local_defs(item);
        }
    }

    fn scan_item_for_local_defs(&mut self, item: &SpecItem<Span>) {
        match item {
            SpecItem::MacroDef(def) => {
                if !def.name.is_empty() {
                    self.local_defs.insert(def.name.clone());
                }
            }
            SpecItem::BuildCondition(bc) => {
                // `%bcond_with foo` registers `%{with foo}` /
                // `%{without foo}` (handled by builtin path) AND
                // the bare name `foo` becomes a referenceable
                // macro in some setups. Be generous — record it.
                if !bc.name.is_empty() {
                    self.local_defs.insert(bc.name.clone());
                }
            }
            SpecItem::Statement(mr) => {
                // `%{!?name:%global name 1}` — conditional-define
                // idiom. The outer ref reads as
                // `name` (IfNotDefined) and the `with_value` body
                // contains a `%global NAME` invocation. Either way,
                // after this top-level Statement runs, NAME is
                // registered. Record both `name` (the outer ref's
                // own name) and any names registered by nested
                // MacroDef-shaped text inside `with_value`.
                self.scan_macro_ref_for_self_defines(mr);
            }
            SpecItem::Conditional(c) => {
                // Definitions inside a top-level `%if`/`%ifarch`
                // count — operators often guard `%global foo` on
                // a profile-specific branch. Even if the branch
                // is inactive under the current build, the spec
                // author's intent is "this is my name".
                for branch in &c.branches {
                    for nested in &branch.body {
                        self.scan_item_for_local_defs(nested);
                    }
                }
            }
            _ => {}
        }
    }

    /// Recognise the `%{!?name:%global name VALUE}` /
    /// `%{?name:%global name VALUE}` conditional-define idiom: when
    /// the outer ref has a non-empty `with_value`, the spec author
    /// has accepted that the name may need a fallback definition.
    /// Register the outer name as a local definition so the
    /// portability report stops claiming it's "missing on all 23
    /// profiles" — the spec is self-sufficient for it.
    fn scan_macro_ref_for_self_defines(&mut self, mr: &MacroRef) {
        if matches!(
            mr.conditional,
            ConditionalMacro::IfDefined | ConditionalMacro::IfNotDefined
        ) && mr.with_value.is_some()
            && !mr.name.is_empty()
        {
            self.local_defs.insert(mr.name.clone());
        }
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
                if let Some(name) = macro_name_from_verbatim(text)
                    && !is_reserved_keyword(&name)
                    && !is_rpm_auto_macro(&name)
                {
                    // Apply the same reserved-keyword and
                    // RPM-auto-macro filters as `is_user_macro` so
                    // condition-side references aren't a backdoor
                    // for noise.
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

    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
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
/// expression sub-tree doesn't model the breakdown. Visible to the
/// rest of the analyzer crate (e.g. `branch_coverage`) so the
/// parsing logic stays in one place.
pub(crate) fn macro_name_from_verbatim(text: &str) -> Option<String> {
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
/// those are language constructs, not registered macros — plus
/// reserved control-flow / section keywords and RPM auto-defines
/// from the preamble.
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
    if m.name.is_empty() {
        return false;
    }
    if is_reserved_keyword(&m.name) {
        return false;
    }
    if is_rpm_auto_macro(&m.name) {
        return false;
    }
    // `%{?name}` / `%{!?name}` (with or without a `:default`) is
    // the idiom where the operator explicitly handles the macro
    // being undefined: expand-when-defined / expand-when-undefined,
    // with a fallback body for the `:default` form. From a
    // portability standpoint the operator has already accepted
    // that the name may not exist, so flagging it as "missing on
    // N profiles" is noise — the `?` / `!?` prefix is the
    // operator's signed acknowledgement.
    //
    // Cross-profile detection of "some define, some don't" still
    // works via [`record_expr_ast`], which collects names from
    // `%if` condition bodies regardless of `?` prefix: that's
    // exactly where operators express deliberate portability
    // dependencies (`%if 0%{?suse_version}` etc.).
    if matches!(
        m.conditional,
        ConditionalMacro::IfDefined | ConditionalMacro::IfNotDefined
    ) {
        return false;
    }
    true
}

/// True when `name` matches a hardcoded RPM control-flow directive
/// or section marker. The parser surfaces some of these as
/// `MacroRef` in specific grammar contexts (notably the body of
/// `Raw` `CondExpr` and certain text fall-throughs); filter them
/// here so the portability report doesn't claim `%if` is "missing
/// on all 23 profiles".
fn is_reserved_keyword(name: &str) -> bool {
    RESERVED_KEYWORDS.contains(&name)
}

/// True when `name` is an RPM auto-defined macro: either listed
/// verbatim in [`RPM_AUTO_MACROS`] or matching `SOURCE\d+` /
/// `PATCH\d+` / `NOSOURCE\d+` / `NOPATCH\d+`. These are injected by
/// the RPM build process from the preamble, so no profile registry
/// declares them — but they're not portability concerns, they're
/// always present at build time.
fn is_rpm_auto_macro(name: &str) -> bool {
    if RPM_AUTO_MACROS.contains(&name) {
        return true;
    }
    for prefix in ["SOURCE", "PATCH", "NOSOURCE", "NOPATCH"] {
        if let Some(tail) = name.strip_prefix(prefix)
            && !tail.is_empty()
            && tail.chars().all(|c| c.is_ascii_digit())
        {
            return true;
        }
    }
    false
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
Summary: %my_summary
License: MIT

%description
%{?desc}

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(names.contains("ver"));
        assert!(names.contains("my_summary"));
        // `%{?desc}` is a conditional reference — operator
        // explicitly handles the macro being undefined, so it's
        // not flagged. See `excludes_conditional_question_refs`.
        assert!(!names.contains("desc"));
        // `%name` would have been collected here pre-filter; it's
        // now classified as an RPM auto-define (preamble injects
        // it) so the collector excludes it.
        assert!(!names.contains("name"));
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
        // `%{?foo:%bar}` — outer `%{?foo}` is operator-handled
        // (suppressed), but the default body `%bar` is a hard
        // reference and must still be collected.
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
        // Suppressed by the conditional-reference filter.
        assert!(!names.contains("foo"));
        // Inside the `:default` body — hard reference, kept.
        assert!(names.contains("bar"));
    }

    #[test]
    fn excludes_conditional_question_refs() {
        // `%{?name}` / `%{!?name}` are operator-handled forms:
        // the `?` / `!?` prefix is an explicit acknowledgement
        // that the macro may not be defined. Portability noise
        // would otherwise dominate the `missing` bucket on every
        // realistic spec.
        let names = names_of(
            "\
Name:    foo
Version: 1%{?distver}
Release: 1
Summary: %{!?my_summary:default text}
License: MIT

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
",
        );
        assert!(!names.contains("distver"));
        assert!(!names.contains("my_summary"));
    }

    #[test]
    fn macro_name_from_verbatim_handles_common_forms() {
        // ExprAst::Macro.text is the raw source token; our parser
        // peels the surface forms back to the registry-lookup name.
        assert_eq!(macro_name_from_verbatim("%foo"), Some("foo".to_string()));
        assert_eq!(macro_name_from_verbatim("%{foo}"), Some("foo".to_string()));
        assert_eq!(macro_name_from_verbatim("%{?foo}"), Some("foo".to_string()));
        assert_eq!(
            macro_name_from_verbatim("%{!?foo}"),
            Some("foo".to_string())
        );
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
