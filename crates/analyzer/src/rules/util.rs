//! Helpers shared by lint rules.

use rpm_spec::ast::{PreambleItem, Span, SpecFile, SpecItem, Tag};

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
}
