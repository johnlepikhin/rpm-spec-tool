//! AST walkers for `%files` sections. Shared by all Phase 18 rule
//! modules so each rule doesn't re-implement the same recursion into
//! conditionals.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};

/// Run `f` on every `FileEntry` reachable from `%files` sections in
/// `spec`, including those nested inside `%if` blocks at the spec
/// level or inside the `%files` section itself.
pub fn for_each_files_entry<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(&'ast FileEntry<Span>),
{
    for_each_files_section(spec, |_subpkg, content| {
        walk_entries(content, &mut f);
    });
}

/// Like [`for_each_files_entry`], but also exposes the subpackage
/// reference (`-n sub` or bare suffix) of the surrounding `%files`
/// section. `None` for the main package's `%files`.
pub fn for_each_files_entry_with_subpkg<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(Option<&'ast rpm_spec::ast::SubpkgRef>, &'ast FileEntry<Span>),
{
    for_each_files_section(spec, |subpkg, content| {
        walk_entries(content, &mut |e| f(subpkg, e));
    });
}

/// Run `f` on the content of every `%files` section in `spec`.
/// Callback receives both the `subpkg` reference (for subpackage-aware
/// rules) and the section body.
pub fn for_each_files_section<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(Option<&'ast rpm_spec::ast::SubpkgRef>, &'ast [FilesContent<Span>]),
{
    walk_top_items(&spec.items, &mut f);
}

fn walk_top_items<'ast, F>(items: &'ast [SpecItem<Span>], f: &mut F)
where
    F: FnMut(Option<&'ast rpm_spec::ast::SubpkgRef>, &'ast [FilesContent<Span>]),
{
    for item in items {
        match item {
            SpecItem::Section(boxed) => {
                if let Section::Files {
                    subpkg, content, ..
                } = boxed.as_ref()
                {
                    f(subpkg.as_ref(), content);
                }
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_top_items(&branch.body, f);
                }
                if let Some(els) = &c.otherwise {
                    walk_top_items(els, f);
                }
            }
            _ => {}
        }
    }
}

fn walk_entries<'ast, F>(items: &'ast [FilesContent<Span>], f: &mut F)
where
    F: FnMut(&'ast FileEntry<Span>),
{
    for item in items {
        match item {
            FilesContent::Entry(e) => f(e),
            FilesContent::Conditional(c) => {
                for branch in &c.branches {
                    walk_entries(&branch.body, f);
                }
                if let Some(els) = &c.otherwise {
                    walk_entries(els, f);
                }
            }
            _ => {}
        }
    }
}

/// `true` when the `FilesContent` item immediately before or after
/// position `i` is a comment. Used by rules that require a nearby
/// justification comment (RPM362).
pub fn neighbour_is_comment(items: &[FilesContent<Span>], i: usize) -> bool {
    let before = i.checked_sub(1).map(|j| &items[j]);
    let after = items.get(i + 1);
    before.is_some_and(|x| matches!(x, FilesContent::Comment(_)))
        || after.is_some_and(|x| matches!(x, FilesContent::Comment(_)))
}

/// Resolve the canonical name of the subpackage referenced by a
/// `%files -n …` / `%files sub` header. Returns `None` for the main
/// package or when the reference contains unresolved macros.
pub fn resolve_subpkg_name(
    main_name: Option<&str>,
    subpkg: Option<&rpm_spec::ast::SubpkgRef>,
) -> Option<String> {
    use rpm_spec::ast::SubpkgRef;
    let r = subpkg?;
    match r {
        SubpkgRef::Relative(t) => {
            let suffix = t.literal_str()?.trim();
            let main = main_name?;
            if suffix.is_empty() {
                return None;
            }
            Some(format!("{main}-{suffix}"))
        }
        SubpkgRef::Absolute(t) => {
            let n = t.literal_str()?.trim();
            if n.is_empty() {
                None
            } else {
                Some(n.to_owned())
            }
        }
        _ => None,
    }
}
