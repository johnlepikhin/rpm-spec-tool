//! Main package + subpackage iteration helpers.
//!
//! Lints that want to reason per-package (e.g. "does *each* package
//! declare `Group`?") use [`iter_packages`] to walk the main package
//! plus every top-level `%package` section uniformly.

use rpm_spec::ast::{
    PackageName, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, Tag, TagValue,
};

use super::preamble::collect_top_level_preamble;

/// Return the resolved literal name of the main package, or `None`
/// when the `Name:` tag is missing or contains macros (can't be
/// safely compared by string).
pub(crate) fn package_name(spec: &SpecFile<Span>) -> Option<&str> {
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
pub(crate) struct PackageView<'a> {
    pub(crate) name: Option<String>,
    pub(crate) items: Vec<&'a PreambleItem<Span>>,
    pub(crate) header_span: Span,
}

impl<'a> PackageView<'a> {
    /// Resolved package name as a literal string, or `None` when the
    /// name (main `Name:` for the main package, or `%package` argument
    /// for a subpackage) contains macros and can't be safely compared.
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Preamble items belonging to this package, unwrapped from any
    /// surrounding `Conditional` arms.
    pub(crate) fn items(&self) -> &[&'a PreambleItem<Span>] {
        &self.items
    }

    /// Span pointing at the package's header — `SpecFile.data` for the
    /// main package, `Section::Package.data` for a subpackage.
    pub(crate) fn header_span(&self) -> Span {
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
pub(crate) fn iter_packages(spec: &SpecFile<Span>) -> Vec<PackageView<'_>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn p(src: &str) -> rpm_spec::ast::SpecFile<Span> {
        parse(src).spec
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
}
