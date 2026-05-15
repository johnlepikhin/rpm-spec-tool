//! Helpers shared by lint rules.

use rpm_spec::ast::{
    BoolDep, DepAtom, DepExpr, PackageName, PreambleContent, PreambleItem, Section, Span,
    SpecFile, SpecItem, Tag, TagValue,
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
            && let Section::Package { name_arg, content, data } = boxed.as_ref()
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
        BoolDep::If { cond, then, otherwise } | BoolDep::Unless { cond, then, otherwise } => {
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
        let spec = p(
            "Name: main\n\
%package -n foo\n\
Summary: subpackage\n\
%description -n foo\n\
sub body\n",
        );
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
        let spec = p(
            "%if 0%{?rhel}\n\
Name: rhel-pkg\n\
%else\n\
Name: fedora-pkg\n\
%endif\n",
        );
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
        let spec = p(
            "Name: hello\n\
%package devel\n\
Summary: dev files\n\
%description devel\nbody\n",
        );
        let pkgs = iter_packages(&spec);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[1].name.as_deref(), Some("hello-devel"));
        assert!(pkgs[1].items.iter().any(|p| matches!(p.tag, Tag::Summary)));
    }

    #[test]
    fn iter_packages_absolute_subpackage() {
        let spec = p(
            "Name: hello\n\
%package -n bar\n\
Summary: standalone\n\
%description -n bar\nbody\n",
        );
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
        let names: Vec<&str> = atoms
            .iter()
            .filter_map(|a| a.name.literal_str())
            .collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }
}
