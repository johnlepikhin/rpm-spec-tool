//! Helpers shared by lint rules.

use rpm_spec::ast::{
    BoolDep, CondExpr, DepAtom, DepExpr, FilesContent, PackageName, PreambleContent, PreambleItem,
    Section, Span, SpecFile, SpecItem, Tag, TagValue, Text,
};

use crate::diagnostic::Edit;

/// Collect top-level preamble items into a `Vec`.
///
/// "Top-level" means items that belong to the main package: those that
/// appear directly in `SpecFile.items` (optionally nested inside a
/// `Conditional`). Preamble items inside a `Section::Package` belong to
/// a subpackage and are deliberately skipped — `missing-name-tag` etc.
/// fire only when the main package itself lacks the tag.
///
/// Returns an owned `Vec` because the walk has to recurse through
/// `Conditional` arms before yielding anything; pretending to be a lazy
/// iterator would mislead callers about `.find()` / `.any()` short
/// circuiting.
pub fn collect_top_level_preamble(spec: &SpecFile<Span>) -> Vec<&PreambleItem<Span>> {
    fn walk<'a>(items: &'a [SpecItem<Span>], out: &mut Vec<&'a PreambleItem<Span>>) {
        for item in items {
            match item {
                SpecItem::Preamble(p) => out.push(p),
                SpecItem::Conditional(c) => {
                    for branch in &c.branches {
                        walk(&branch.body, out);
                    }
                    if let Some(els) = &c.otherwise {
                        walk(els, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&spec.items, &mut out);
    out
}

/// `true` if any top-level preamble item matches `predicate`.
pub fn has_top_level_tag<F>(spec: &SpecFile<Span>, mut predicate: F) -> bool
where
    F: FnMut(&Tag) -> bool,
{
    collect_top_level_preamble(spec)
        .iter()
        .any(|p| predicate(&p.tag))
}

/// Span covering the whole `SpecFile` (the parser stores it on
/// `SpecFile.data`). Useful as a fallback `primary_span` for "missing X"
/// diagnostics that don't have a concrete offending node to point at.
pub fn spec_span(spec: &SpecFile<Span>) -> Span {
    spec.data
}

/// Build an [`Edit`] that deletes the byte range covered by `span`.
///
/// Common building block for auto-fixes that drop an entire preamble
/// line or section — the parser already arranges spans to include the
/// trailing newline, so the replacement is just the empty string.
pub fn drop_span(span: Span) -> Edit {
    Edit::new(span, "")
}

/// Default mapping from a literal hardcoded path prefix to the RPM macro
/// that should replace it. Used by [`crate::rules::hardcoded_paths`]
/// when no profile has been applied (or when the profile doesn't
/// override the macro's value). Distribution profiles can replace these
/// entries via `HardcodedPaths::set_profile`, which reads the actual
/// `_bindir` / `_libdir` / etc. from `profile.macros`.
///
/// **Order matters.** The table is scanned top-down; more-specific
/// prefixes (`/usr/lib64`, `/var/log`) must precede their less-specific
/// peers (`/usr/lib`, `/var/lib`) so that a literal like `/usr/lib64/foo`
/// matches the right replacement.
///
/// `pub(crate)` because this is an analyzer-internal default — downstream
/// consumers should read the actual path table off a resolved `Profile`,
/// not the fallback constants.
pub(crate) const FALLBACK_PATH_TABLE: &[(&str, &str)] = &[
    ("/usr/lib64", "%{_libdir}"),
    ("/usr/libexec", "%{_libexecdir}"),
    ("/usr/include", "%{_includedir}"),
    ("/usr/share", "%{_datadir}"),
    ("/usr/bin", "%{_bindir}"),
    ("/usr/sbin", "%{_sbindir}"),
    ("/usr/lib", "%{_libdir}"),
    ("/var/log", "%{_localstatedir}/log"),
    ("/var/lib", "%{_sharedstatedir}"),
    ("/etc", "%{_sysconfdir}"),
];

/// Split an SPDX license expression on the `OR` / `AND` / `WITH`
/// keywords (case-insensitive, word-boundary-respecting) and return
/// the trimmed atoms. Surrounding parentheses and commas are stripped.
///
/// Used by both RPM024 (`invalid-license`) and RPM127
/// (`legacy-license-syntax`). Kept here as the single source of truth
/// so a future fix (e.g. `WITH` exception identifier handling) applies
/// uniformly.
///
/// Implementation: tokenise on whitespace + paren/comma into raw
/// tokens, then drop the operator tokens. SPDX identifiers themselves
/// cannot contain whitespace, so a whitespace split never breaks an
/// atom.
pub(crate) fn split_spdx_atoms(expr: &str) -> Vec<&str> {
    expr.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && !is_spdx_operator(t))
        .collect()
}

/// True when `tok` is one of the SPDX expression operators
/// (`OR`/`AND`/`WITH`), case-insensitive.
pub(crate) fn is_spdx_operator(tok: &str) -> bool {
    tok.eq_ignore_ascii_case("OR")
        || tok.eq_ignore_ascii_case("AND")
        || tok.eq_ignore_ascii_case("WITH")
}

/// `true` when `b` can continue a shell-token / path-name. Used by
/// boundary checks in path-prefix matching ([`is_path_boundary`]) and
/// in command-word matching (e.g. RPM062 `egrep`/`fgrep` detection).
#[inline]
pub(crate) fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// `true` when `rest` (the bytes immediately after a candidate
/// path-prefix) marks the path's end. Name-continuation characters
/// (`a-zA-Z0-9._-`) keep the match going; anything else terminates.
/// Used by [`crate::rules::hardcoded_paths`] to validate matches
/// like `/usr/bin` vs `/usr/binfoo`.
#[inline]
pub(crate) fn is_path_boundary(rest: &str) -> bool {
    match rest.as_bytes().first() {
        None => true,
        Some(b'/') => true,
        Some(&b) => !is_name_byte(b),
    }
}

/// One declared patch tag.
///
/// RPM treats the unnumbered `Patch:` form as `Patch0:` for application
/// purposes, so we normalise both into `number` — but keep `explicit`
/// around in case a future rule needs to distinguish them (e.g. to
/// warn about the legacy shortcut).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PatchDecl {
    pub number: u32,
    pub span: Span,
    /// `true` if the source wrote a number (`Patch5:`); `false` for
    /// the bare `Patch:` shortcut, which RPM treats as `Patch0:`.
    #[allow(dead_code)]
    pub explicit: bool,
}

/// Collect every declared `Patch:` / `PatchN:` tag at the top level
/// of `spec`, in source order. Used by [`RPM064 patch-defined-not-applied`]
/// to pair declarations against applications inside `%prep`.
pub(crate) fn collect_declared_patches(spec: &SpecFile<Span>) -> Vec<PatchDecl> {
    let mut out = Vec::new();
    for item in collect_top_level_preamble(spec) {
        if let Tag::Patch(n) = item.tag {
            out.push(PatchDecl {
                number: n.unwrap_or(0),
                span: item.data,
                explicit: n.is_some(),
            });
        }
    }
    out
}

/// Names of well-known macros that lint rules inspect. Kept in one
/// place so a rename in a future RPM release is a single-line change.
pub(crate) const MACRO_SETUP: &str = "setup";
pub(crate) const MACRO_AUTOSETUP: &str = "autosetup";
pub(crate) const MACRO_AUTOPATCH: &str = "autopatch";
pub(crate) const MACRO_PATCH_PREFIX: &str = "patch";

// =====================================================================
// Conditional-block helpers (used by Phase 6 rules RPM070–RPM077).
// =====================================================================

/// `true` when `expr` is a literal `1` / `true` — evaluates to true
/// unconditionally. Returns `false` for any expression containing
/// macros (we can't statically resolve those).
///
/// Handles both [`CondExpr::Raw`] (legacy raw-text path) and
/// [`CondExpr::Parsed`] (structured AST path). Both paths agree on
/// what counts as a constant truth.
pub(crate) fn is_constant_true_condition<T>(expr: &CondExpr<T>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => match ast.as_ref().peel_parens() {
            rpm_spec::ast::ExprAst::Integer { value, .. } => *value != 0,
            rpm_spec::ast::ExprAst::String { value, .. } => !value.is_empty(),
            rpm_spec::ast::ExprAst::Identifier { name, .. } => name == "true",
            _ => false,
        },
        CondExpr::Raw(text) => {
            let Some(lit) = text.literal_str() else {
                return false;
            };
            matches!(lit.trim(), "1" | "true")
        }
        _ => false,
    }
}

/// `true` when `expr` is a literal `0` / `false` / empty — always
/// evaluates to false. RPM's expression language treats the empty
/// string as false, so `%if` with an empty body counts.
pub(crate) fn is_constant_false_condition<T>(expr: &CondExpr<T>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => match ast.as_ref().peel_parens() {
            rpm_spec::ast::ExprAst::Integer { value, .. } => *value == 0,
            rpm_spec::ast::ExprAst::String { value, .. } => value.is_empty(),
            rpm_spec::ast::ExprAst::Identifier { name, .. } => name == "false",
            _ => false,
        },
        CondExpr::Raw(text) => {
            let Some(lit) = text.literal_str() else {
                return false;
            };
            matches!(lit.trim(), "0" | "false" | "")
        }
        _ => false,
    }
}

/// Conservative structural equality for two `CondExpr` values.
///
/// Returns `true` only when both expressions are statically
/// resolvable to the same text. Any expression that contains a `%`
/// (macro reference) causes a `false` return — the parser stores
/// the RPM expression language as raw text, so we can't tell what a
/// macro will expand to at build time, which is outside the lint's
/// scope.
///
/// `ArchList` arms must contain the same set of literal architecture
/// tokens (order-insensitive).
pub(crate) fn cond_expr_resolvably_eq<T, U>(a: &CondExpr<T>, b: &CondExpr<U>) -> bool {
    match (a, b) {
        (CondExpr::Raw(t1), CondExpr::Raw(t2)) => match (t1.literal_str(), t2.literal_str()) {
            (Some(s1), Some(s2)) => {
                let trimmed1 = s1.trim();
                let trimmed2 = s2.trim();
                // Conservative bail-out: any `%` likely starts a macro
                // reference (or `%%` literal). The parser doesn't
                // tokenise the expression grammar, so we can't tell
                // them apart — refuse to claim equivalence.
                if trimmed1.contains('%') || trimmed2.contains('%') {
                    return false;
                }
                trimmed1 == trimmed2
            }
            _ => false,
        },
        (CondExpr::ArchList(a1), CondExpr::ArchList(a2)) => {
            if a1.len() != a2.len() {
                return false;
            }
            let lit_set = |items: &[Text]| -> Option<Vec<String>> {
                let mut out = Vec::with_capacity(items.len());
                for t in items {
                    let s = t.literal_str()?.trim();
                    if s.contains('%') {
                        return None;
                    }
                    out.push(s.to_owned());
                }
                Some(out)
            };
            let (Some(mut s1), Some(mut s2)) = (lit_set(a1), lit_set(a2)) else {
                return false;
            };
            s1.sort();
            s2.sort();
            s1 == s2
        }
        // Parsed-vs-Parsed: structural equality of the AST (after
        // peeling parens) gives us a precise comparison without
        // re-stringifying. Any macro reference inside (whether
        // structurally identical or not) triggers a conservative
        // bail-out — we can't resolve `%{name}` at lint time.
        (CondExpr::Parsed(a1), CondExpr::Parsed(b1)) => {
            !contains_macro_ast(a1) && !contains_macro_ast(b1) && exprs_equiv(a1, b1)
        }
        // `Raw` vs `ArchList` (and any future variant) — different
        // shape; can't be equal. `CondExpr` is `#[non_exhaustive]`,
        // so the wildcard is required.
        _ => false,
    }
}

/// Structural equality for two [`ExprAst`] trees, ignoring `data`
/// (spans). Paren wrappers are peeled before comparison, so
/// `(X) && (Y)` compares equal to `X && Y`.
///
/// Used by [`cond_expr_resolvably_eq`] and Phase 7-series rules that
/// need to ask whether two sub-expressions denote the same value
/// (RPM086 idempotent, RPM088 self-comparison, RPM102 inequality
/// grouping, RPM104 string-set, RPM101 absorption).
pub(crate) fn exprs_equiv<T, U>(
    a: &rpm_spec::ast::ExprAst<T>,
    b: &rpm_spec::ast::ExprAst<U>,
) -> bool {
    use rpm_spec::ast::ExprAst;
    match (a.peel_parens(), b.peel_parens()) {
        (ExprAst::Integer { value: v1, .. }, ExprAst::Integer { value: v2, .. }) => v1 == v2,
        (ExprAst::String { value: v1, .. }, ExprAst::String { value: v2, .. }) => v1 == v2,
        (ExprAst::Identifier { name: n1, .. }, ExprAst::Identifier { name: n2, .. }) => n1 == n2,
        (ExprAst::Macro { text: m1, .. }, ExprAst::Macro { text: m2, .. }) => m1 == m2,
        (ExprAst::Not { inner: i1, .. }, ExprAst::Not { inner: i2, .. }) => exprs_equiv(i1, i2),
        (
            ExprAst::Binary {
                kind: k1,
                lhs: l1,
                rhs: r1,
                ..
            },
            ExprAst::Binary {
                kind: k2,
                lhs: l2,
                rhs: r2,
                ..
            },
        ) => k1 == k2 && exprs_equiv(l1, l2) && exprs_equiv(r1, r2),
        _ => false,
    }
}

/// `true` when the AST contains any macro reference — used by
/// [`cond_expr_resolvably_eq`] to bail out conservatively.
pub(crate) fn contains_macro_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Integer { .. } | ExprAst::String { .. } | ExprAst::Identifier { .. } => false,
        ExprAst::Macro { .. } => true,
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => contains_macro_ast(inner),
        ExprAst::Binary { lhs, rhs, .. } => contains_macro_ast(lhs) || contains_macro_ast(rhs),
        // `ExprAst` is `#[non_exhaustive]` from the upstream crate; a
        // future variant is treated conservatively as "contains
        // macros".
        _ => true,
    }
}

/// `true` when every item in the conditional body is "filler"
/// (blank line or comment) — i.e. the branch has no real content.
pub(crate) fn is_empty_top_body(body: &[SpecItem<Span>]) -> bool {
    body.iter()
        .all(|i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)))
}

pub(crate) fn is_empty_preamble_body(body: &[PreambleContent<Span>]) -> bool {
    body.iter()
        .all(|i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)))
}

pub(crate) fn is_empty_files_body(body: &[FilesContent<Span>]) -> bool {
    body.iter()
        .all(|i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)))
}

/// Return the resolved literal name of the main package, or `None`
/// when the `Name:` tag is missing or contains macros (can't be
/// safely compared by string).
pub fn package_name(spec: &SpecFile<Span>) -> Option<&str> {
    for item in collect_top_level_preamble(spec) {
        if let Tag::Name = item.tag
            && let TagValue::Text(t) = &item.value
        {
            return t.literal_str();
        }
    }
    None
}

/// View of one package within a spec — either the main package or one
/// of its `%package` subpackages.
///
/// Fields are crate-visible (not `pub`) so callers stay within the
/// analyzer crate. Public access goes through the inherent methods,
/// which lets us evolve the representation (e.g. switch `name` to
/// `Cow<'a, str>` or split into an enum) without a breaking change.
#[derive(Debug)]
pub struct PackageView<'a> {
    pub(crate) name: Option<String>,
    pub(crate) items: Vec<&'a PreambleItem<Span>>,
    pub(crate) header_span: Span,
}

impl<'a> PackageView<'a> {
    /// Resolved package name as a literal string, or `None` when the
    /// name (main `Name:` for the main package, or `%package` argument
    /// for a subpackage) contains macros and can't be safely compared.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Preamble items belonging to this package, unwrapped from any
    /// surrounding `Conditional` arms.
    pub fn items(&self) -> &[&'a PreambleItem<Span>] {
        &self.items
    }

    /// Span pointing at the package's header — `SpecFile.data` for the
    /// main package, `Section::Package.data` for a subpackage.
    pub fn header_span(&self) -> Span {
        self.header_span
    }
}

/// Iterate the main package + every top-level `%package` section in
/// source order.
///
/// `%package` blocks nested inside a `Conditional` (`%if`/`%endif`) are
/// intentionally skipped: cross-distro patterns rarely declare entire
/// subpackages conditionally, and self-* checks on partial views would
/// produce false positives.
pub fn iter_packages(spec: &SpecFile<Span>) -> Vec<PackageView<'_>> {
    let main_name = package_name(spec).map(str::to_owned);

    let mut views = Vec::new();
    views.push(PackageView {
        name: main_name.clone(),
        items: collect_top_level_preamble(spec),
        header_span: spec.data,
    });

    for item in &spec.items {
        if let SpecItem::Section(boxed) = item
            && let Section::Package {
                name_arg,
                content,
                data,
            } = boxed.as_ref()
        {
            views.push(PackageView {
                name: resolve_subpackage_name(main_name.as_deref(), name_arg),
                items: collect_preamble_items_in_content(content),
                header_span: *data,
            });
        }
    }
    views
}

fn resolve_subpackage_name(main: Option<&str>, arg: &PackageName) -> Option<String> {
    match arg {
        PackageName::Relative(t) => match (main, t.literal_str()) {
            (Some(m), Some(suffix)) => Some(format!("{m}-{suffix}")),
            _ => None,
        },
        PackageName::Absolute(t) => t.literal_str().map(str::to_owned),
        _ => None,
    }
}

fn collect_preamble_items_in_content(
    content: &[PreambleContent<Span>],
) -> Vec<&PreambleItem<Span>> {
    fn walk<'a>(items: &'a [PreambleContent<Span>], out: &mut Vec<&'a PreambleItem<Span>>) {
        for c in items {
            match c {
                PreambleContent::Item(p) => out.push(p),
                PreambleContent::Conditional(cond) => {
                    for branch in &cond.branches {
                        walk(&branch.body, out);
                    }
                    if let Some(els) = &cond.otherwise {
                        walk(els, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(content, &mut out);
    out
}

/// Collect every [`DepAtom`] reachable from preamble items whose tag
/// satisfies `tag_matcher`. Walks into `DepExpr::Rich(BoolDep)` so
/// atoms inside boolean dependencies (`(foo and bar)`) are not missed.
pub fn collect_dep_atoms_in_items<'a, F>(
    items: &[&'a PreambleItem<Span>],
    tag_matcher: F,
) -> Vec<&'a DepAtom>
where
    F: Fn(&Tag) -> bool,
{
    let mut out = Vec::new();
    for item in items {
        if !tag_matcher(&item.tag) {
            continue;
        }
        if let TagValue::Dep(expr) = &item.value {
            collect_atoms(expr, &mut out);
        }
    }
    out
}

fn collect_atoms<'a>(expr: &'a DepExpr, out: &mut Vec<&'a DepAtom>) {
    match expr {
        DepExpr::Atom(a) => out.push(a),
        DepExpr::Rich(b) => collect_atoms_bool(b, out),
        _ => {}
    }
}

fn collect_atoms_bool<'a>(b: &'a BoolDep, out: &mut Vec<&'a DepAtom>) {
    match b {
        BoolDep::And(xs) | BoolDep::Or(xs) | BoolDep::With(xs) => {
            for x in xs {
                collect_atoms(x, out);
            }
        }
        BoolDep::If {
            cond,
            then,
            otherwise,
        }
        | BoolDep::Unless {
            cond,
            then,
            otherwise,
        } => {
            collect_atoms(cond, out);
            collect_atoms(then, out);
            if let Some(o) = otherwise {
                collect_atoms(o, out);
            }
        }
        BoolDep::Without { left, right } => {
            collect_atoms(left, out);
            collect_atoms(right, out);
        }
        _ => {}
    }
}

/// Generate a "missing required tag" lint.
///
/// Phase 1 introduced six near-identical lints (`missing-name-tag`,
/// `missing-version-tag`, ...). They differ only in metadata, the
/// matched [`Tag`] variant, and the message; the visit body, `Lint`
/// impl, and tests are byte-for-byte the same. This macro keeps them
/// declarative and prevents the templated boilerplate from drifting
/// over time.
///
/// Each invocation expands to a `pub mod` with `pub static METADATA`,
/// a `Default + new()` struct, `impl Visit + Lint`, and two unit tests.
#[macro_export]
macro_rules! declare_missing_tag_lint {
    (
        mod $mod_id:ident,
        struct $struct:ident,
        id: $id:literal,
        name: $name:literal,
        description: $desc:literal,
        severity: $sev:expr,
        tag: $tag:pat,
        message: $msg:literal,
        good_fixture: $good:literal,
        bad_fixture: $bad:literal $(,)?
    ) => {
        pub mod $mod_id {
            use rpm_spec::ast::{Span, SpecFile, Tag};

            use $crate::diagnostic::{Diagnostic, LintCategory, Severity};
            use $crate::lint::{Lint, LintMetadata};
            use $crate::rules::util::{has_top_level_tag, spec_span};
            use $crate::visit::Visit;

            pub static METADATA: LintMetadata = LintMetadata {
                id: $id,
                name: $name,
                description: $desc,
                default_severity: $sev,
                category: LintCategory::Packaging,
            };

            #[derive(Debug, Default)]
            pub struct $struct {
                diagnostics: Vec<Diagnostic>,
            }

            impl $struct {
                pub fn new() -> Self {
                    Self::default()
                }
            }

            impl<'ast> Visit<'ast> for $struct {
                fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
                    if !has_top_level_tag(spec, |t: &Tag| matches!(t, $tag)) {
                        self.diagnostics.push(Diagnostic::new(
                            &METADATA,
                            $sev,
                            $msg,
                            spec_span(spec),
                        ));
                    }
                }
            }

            impl Lint for $struct {
                fn metadata(&self) -> &'static LintMetadata {
                    &METADATA
                }
                fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                    ::std::mem::take(&mut self.diagnostics)
                }
            }

            #[cfg(test)]
            mod tests {
                use super::*;
                use $crate::session::parse;

                fn run(src: &str) -> Vec<Diagnostic> {
                    let outcome = parse(src);
                    let mut lint = $struct::new();
                    lint.visit_spec(&outcome.spec);
                    lint.take_diagnostics()
                }

                #[test]
                fn flags_when_tag_missing() {
                    let diags = run($bad);
                    assert_eq!(diags.len(), 1);
                    assert_eq!(diags[0].lint_id, $id);
                }

                #[test]
                fn silent_when_tag_present() {
                    assert!(run($good).is_empty());
                }
            }
        }
    };
}

/// Generate a "missing required %section" lint.
///
/// Symmetric to [`declare_missing_tag_lint!`] but matches against
/// `Section::BuildScript { kind, .. }`. Each invocation expands to a
/// `pub mod` with metadata, a `Lint` impl, and two unit tests.
#[macro_export]
macro_rules! declare_missing_section_lint {
    (
        mod $mod_id:ident,
        struct $struct:ident,
        id: $id:literal,
        name: $name:literal,
        description: $desc:literal,
        severity: $sev:expr,
        kind: $kind:expr,
        message: $msg:literal,
        good_fixture: $good:literal,
        bad_fixture: $bad:literal $(,)?
    ) => {
        pub mod $mod_id {
            use rpm_spec::ast::{BuildScriptKind, Section, Span, SpecFile, SpecItem};

            use $crate::diagnostic::{Diagnostic, LintCategory, Severity};
            use $crate::lint::{Lint, LintMetadata};
            use $crate::rules::util::spec_span;
            use $crate::visit::Visit;

            pub static METADATA: LintMetadata = LintMetadata {
                id: $id,
                name: $name,
                description: $desc,
                default_severity: $sev,
                category: LintCategory::Packaging,
            };

            #[derive(Debug, Default)]
            pub struct $struct {
                diagnostics: Vec<Diagnostic>,
            }

            impl $struct {
                pub fn new() -> Self {
                    Self::default()
                }
            }

            impl<'ast> Visit<'ast> for $struct {
                fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
                    let target_kind: BuildScriptKind = $kind;
                    let found = spec.items.iter().any(|item| {
                        matches!(
                            item,
                            SpecItem::Section(s)
                                if matches!(
                                    s.as_ref(),
                                    Section::BuildScript { kind, .. } if *kind == target_kind
                                )
                        )
                    });
                    if !found {
                        self.diagnostics.push(Diagnostic::new(
                            &METADATA,
                            $sev,
                            $msg,
                            spec_span(spec),
                        ));
                    }
                }
            }

            impl Lint for $struct {
                fn metadata(&self) -> &'static LintMetadata {
                    &METADATA
                }
                fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                    ::std::mem::take(&mut self.diagnostics)
                }
            }

            #[cfg(test)]
            mod tests {
                use super::*;
                use $crate::session::parse;

                fn run(src: &str) -> Vec<Diagnostic> {
                    let outcome = parse(src);
                    let mut lint = $struct::new();
                    lint.visit_spec(&outcome.spec);
                    lint.take_diagnostics()
                }

                #[test]
                fn flags_when_section_missing() {
                    let diags = run($bad);
                    assert_eq!(diags.len(), 1);
                    assert_eq!(diags[0].lint_id, $id);
                }

                #[test]
                fn silent_when_section_present() {
                    assert!(run($good).is_empty());
                }
            }
        }
    };
}

/// Build a synthetic `Profile` for unit-testing profile-aware rules.
/// Convenience over `Profile::default()` mutation for the common
/// "family + a few macros + a few rpmlib features" case.
///
/// `dist_tag` is optional because most rules gate on family alone;
/// composite gates (RPM127's "Fedora ≥ 40") pass `Some(".fc40")` etc.
#[cfg(test)]
#[allow(dead_code)] // construction helper used selectively across rule modules
pub fn make_test_profile(
    family: Option<rpm_spec_profile::Family>,
    dist_tag: Option<&str>,
    macros: &[(&str, &str)],
    rpmlib: &[(&str, &str)],
) -> rpm_spec_profile::Profile {
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};
    let mut p = Profile::default();
    p.identity.family = family;
    p.identity.dist_tag = dist_tag.map(str::to_owned);
    for (name, body) in macros {
        p.macros
            .insert(*name, MacroEntry::literal(*body, Provenance::Override));
    }
    for (name, ver) in rpmlib {
        p.rpmlib.features.insert((*name).into(), (*ver).into());
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn p(src: &str) -> rpm_spec::ast::SpecFile<Span> {
        parse(src).spec
    }

    #[test]
    fn collects_simple_top_level() {
        let spec = p("Name: hello\nVersion: 1\n");
        let items = collect_top_level_preamble(&spec);
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0].tag, Tag::Name));
        assert!(matches!(items[1].tag, Tag::Version));
    }

    #[test]
    fn skips_subpackage_preamble() {
        // Tags inside `%package -n foo` should not count as top-level.
        let spec = p("Name: main\n\
%package -n foo\n\
Summary: subpackage\n\
%description -n foo\n\
sub body\n");
        // Only `Name: main` is top-level. `Summary` lives inside the
        // %package section and belongs to the subpackage.
        let items = collect_top_level_preamble(&spec);
        assert_eq!(items.len(), 1, "got {items:?}");
        assert!(matches!(items[0].tag, Tag::Name));
    }

    #[test]
    fn dives_into_conditional_branches() {
        // Tags inside `%if ... %endif` are still top-level — both arms
        // contribute.
        let spec = p("%if 0%{?rhel}\n\
Name: rhel-pkg\n\
%else\n\
Name: fedora-pkg\n\
%endif\n");
        let items = collect_top_level_preamble(&spec);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|p| matches!(p.tag, Tag::Name)));
    }

    #[test]
    fn empty_spec_returns_empty_vec() {
        let spec = p("");
        assert!(collect_top_level_preamble(&spec).is_empty());
    }

    #[test]
    fn has_top_level_tag_finds_match() {
        let spec = p("Name: x\nLicense: MIT\n");
        assert!(has_top_level_tag(&spec, |t| matches!(t, Tag::License)));
        assert!(!has_top_level_tag(&spec, |t| matches!(t, Tag::URL)));
    }

    #[test]
    fn drop_span_emits_empty_replacement() {
        let span = Span::from_bytes(5, 12);
        let edit = drop_span(span);
        assert_eq!(edit.span, span);
        assert!(edit.replacement.is_empty());
    }

    #[test]
    fn package_name_extracts_main_literal() {
        let spec = p("Name: hello\nVersion: 1\n");
        assert_eq!(package_name(&spec), Some("hello"));
    }

    #[test]
    fn package_name_none_when_macro() {
        // Name with embedded macro can't be resolved literally.
        let spec = p("Name: %{base_name}\n");
        assert_eq!(package_name(&spec), None);
    }

    #[test]
    fn iter_packages_main_only() {
        let spec = p("Name: hello\nVersion: 1\n");
        let pkgs = iter_packages(&spec);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name.as_deref(), Some("hello"));
        // Two top-level preamble items.
        assert_eq!(pkgs[0].items.len(), 2);
    }

    #[test]
    fn iter_packages_relative_subpackage() {
        // `%package devel` resolves to `<main>-devel`.
        let spec = p("Name: hello\n\
%package devel\n\
Summary: dev files\n\
%description devel\nbody\n");
        let pkgs = iter_packages(&spec);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[1].name.as_deref(), Some("hello-devel"));
        assert!(pkgs[1].items.iter().any(|p| matches!(p.tag, Tag::Summary)));
    }

    #[test]
    fn iter_packages_absolute_subpackage() {
        let spec = p("Name: hello\n\
%package -n bar\n\
Summary: standalone\n\
%description -n bar\nbody\n");
        let pkgs = iter_packages(&spec);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[1].name.as_deref(), Some("bar"));
    }

    #[test]
    fn collect_dep_atoms_finds_plain_and_rich() {
        // `Requires: a, (b and c)` — three atoms total.
        let spec = p("Name: x\nRequires: a, (b and c)\n");
        let pkgs = iter_packages(&spec);
        let atoms = collect_dep_atoms_in_items(&pkgs[0].items, |t| matches!(t, Tag::Requires));
        let names: Vec<&str> = atoms.iter().filter_map(|a| a.name.literal_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }

    // -- shared helpers extracted by Phase-15 fixall --

    #[test]
    fn is_path_boundary_at_end_of_string() {
        assert!(is_path_boundary(""));
    }

    #[test]
    fn is_path_boundary_on_slash_continues_path() {
        assert!(is_path_boundary("/foo"));
    }

    #[test]
    fn is_path_boundary_rejects_name_continuation() {
        // `/usr/bin` followed by `foo` (alpha) is NOT a boundary —
        // the path keeps going as `/usr/binfoo`. Same for digits,
        // `_`, `-`, `.`.
        assert!(!is_path_boundary("foo"));
        assert!(!is_path_boundary("1"));
        assert!(!is_path_boundary("_x"));
        assert!(!is_path_boundary("-x"));
        assert!(!is_path_boundary(".x"));
    }

    #[test]
    fn is_path_boundary_terminator_chars() {
        // Anything that can't continue a path-name terminates: whitespace,
        // shell punctuation, end-of-string.
        assert!(is_path_boundary(" "));
        assert!(is_path_boundary("\t"));
        assert!(is_path_boundary("\""));
        assert!(is_path_boundary("'"));
        assert!(is_path_boundary(":"));
    }

    #[test]
    fn split_spdx_atoms_empty_input() {
        let v = split_spdx_atoms("");
        assert!(v.is_empty(), "empty input → no atoms; got {v:?}");
    }

    #[test]
    fn split_spdx_atoms_single_identifier() {
        assert_eq!(split_spdx_atoms("MIT"), vec!["MIT"]);
    }

    #[test]
    fn split_spdx_atoms_or_and_with() {
        assert_eq!(
            split_spdx_atoms("MIT OR GPL-2.0-or-later WITH Classpath-exception-2.0"),
            vec!["MIT", "GPL-2.0-or-later", "Classpath-exception-2.0"]
        );
    }

    #[test]
    fn split_spdx_atoms_handles_parens_and_commas() {
        // `(MIT OR Apache-2.0), GPL-3.0-or-later` — atoms regardless
        // of grouping syntax.
        let v = split_spdx_atoms("(MIT OR Apache-2.0), GPL-3.0-or-later");
        assert_eq!(v, vec!["MIT", "Apache-2.0", "GPL-3.0-or-later"]);
    }

    #[test]
    fn split_spdx_atoms_only_operators_yields_empty() {
        assert!(split_spdx_atoms("OR AND WITH").is_empty());
    }

    #[test]
    fn is_spdx_operator_case_insensitive() {
        for op in ["OR", "or", "Or", "AND", "and", "WITH", "with", "WitH"] {
            assert!(is_spdx_operator(op), "{op} should match");
        }
    }

    #[test]
    fn is_spdx_operator_rejects_non_operators() {
        for tok in ["MIT", "GPL", "ORIGINAL", "ANDX", "WITHOUT", "", "or-"] {
            assert!(!is_spdx_operator(tok), "{tok} should not match");
        }
    }
}
