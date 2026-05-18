//! Preamble walkers and span helpers.
//!
//! "Top-level" here means items that belong to the main package: those
//! appearing directly in `SpecFile.items` (optionally nested inside a
//! `Conditional`). Items inside a `Section::Package` belong to a
//! subpackage and are handled separately by [`super::packages`].

use rpm_spec::ast::{FilesContent, PreambleContent, PreambleItem, Span, SpecFile, SpecItem, Tag};

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
pub(crate) fn collect_top_level_preamble(spec: &SpecFile<Span>) -> Vec<&PreambleItem<Span>> {
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
pub(crate) fn has_top_level_tag<F>(spec: &SpecFile<Span>, mut predicate: F) -> bool
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
pub(crate) fn spec_span(spec: &SpecFile<Span>) -> Span {
    spec.data
}

/// Build an [`Edit`] that deletes the byte range covered by `span`.
///
/// Common building block for auto-fixes that drop an entire preamble
/// line or section — the parser already arranges spans to include the
/// trailing newline, so the replacement is just the empty string.
pub(crate) fn drop_span(span: Span) -> Edit {
    Edit::new(span, "")
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
    #[expect(
        dead_code,
        reason = "future rule may distinguish bare `Patch:` from explicit `PatchN:`"
    )]
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

/// `true` when every item in the conditional body is "filler"
/// (blank line or comment) — i.e. the branch has no real content.
///
/// A *completely empty* `body` returns `false`: the AST being empty
/// usually means the parser couldn't decode some macro inside the
/// branch (e.g. a project-private `%gendep_*` macro) rather than the
/// source really being empty between `%if` and `%endif`. RPM073 would
/// otherwise fire on every such block. Genuine `%if … %endif` with
/// nothing in source still gets `Blank` items emitted by the parser,
/// so this stays accurate for the actual lint target.
pub(crate) fn is_empty_top_body(body: &[SpecItem<Span>]) -> bool {
    !body.is_empty()
        && body
            .iter()
            .all(|i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)))
}

pub(crate) fn is_empty_preamble_body(body: &[PreambleContent<Span>]) -> bool {
    !body.is_empty()
        && body
            .iter()
            .all(|i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)))
}

pub(crate) fn is_empty_files_body(body: &[FilesContent<Span>]) -> bool {
    !body.is_empty()
        && body
            .iter()
            .all(|i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)))
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

    // ---- empty-body helpers ----

    /// Zero-item slices must report `false` — an empty AST body usually
    /// signals "the parser couldn't decode something" rather than a
    /// genuine empty source between `%if` and `%endif`. RPM073 (and its
    /// preamble/files cousins) would otherwise fire on every such block.
    #[test]
    fn is_empty_top_body_returns_false_on_empty_slice() {
        assert!(!is_empty_top_body(&[]));
    }

    #[test]
    fn is_empty_preamble_body_returns_false_on_empty_slice() {
        assert!(!is_empty_preamble_body(&[]));
    }

    #[test]
    fn is_empty_files_body_returns_false_on_empty_slice() {
        assert!(!is_empty_files_body(&[]));
    }

    /// A single Blank item is the "genuine empty" shape the parser
    /// emits for `%if\n%endif`. All three helpers must return `true`.
    #[test]
    fn is_empty_top_body_true_on_single_blank() {
        let body: Vec<SpecItem<Span>> = vec![SpecItem::Blank];
        assert!(is_empty_top_body(&body));
    }

    #[test]
    fn is_empty_preamble_body_true_on_single_blank() {
        let body: Vec<PreambleContent<Span>> = vec![PreambleContent::Blank];
        assert!(is_empty_preamble_body(&body));
    }

    #[test]
    fn is_empty_files_body_true_on_single_blank() {
        let body: Vec<FilesContent<Span>> = vec![FilesContent::Blank];
        assert!(is_empty_files_body(&body));
    }
}
