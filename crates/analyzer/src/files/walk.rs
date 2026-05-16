//! AST walkers for `%files` sections. Shared by all Phase 18 rule
//! modules so each rule doesn't re-implement the same recursion into
//! conditionals.
//!
//! ## Recursion bound
//!
//! Both `walk_top_items` and `walk_entries` recurse through
//! `%if`/`%else` branches. Real-world specs nest 2–3 levels deep;
//! pathological input (a fuzz-crafted spec with 10 000+ nested `%if`)
//! could blow the thread stack. [`MAX_DEPTH`] caps the recursion at a
//! depth that's well above any human-authored spec but stays
//! comfortably within Rust's default 8 MiB main-thread stack.

use rpm_spec::ast::{FileEntry, FilesContent, Section, Span, SpecFile, SpecItem};

/// Hard cap on `%if` nesting traversed by the `%files` walkers. Real
/// specs use 2–3 levels; 128 is a defensive bound that never reached
/// in practice but defends against fuzz / adversarial input.
const MAX_DEPTH: u32 = 128;

/// Run `f` on every `FileEntry` reachable from `%files` sections in
/// `spec`, including those nested inside `%if` blocks at the spec
/// level or inside the `%files` section itself.
pub fn for_each_files_entry<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(&'ast FileEntry<Span>),
{
    for_each_files_section(spec, |sec| {
        walk_entries(sec.content, &mut f);
    });
}

/// Like [`for_each_files_entry`], but also exposes the subpackage
/// reference (`-n sub` or bare suffix) of the surrounding `%files`
/// section. `None` for the main package's `%files`.
pub fn for_each_files_entry_with_subpkg<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(Option<&'ast rpm_spec::ast::SubpkgRef>, &'ast FileEntry<Span>),
{
    for_each_files_section(spec, |sec| {
        let subpkg = sec.subpkg;
        walk_entries(sec.content, &mut |e| f(subpkg, e));
    });
}

/// View of one `%files` section exposed to walkers. Bundles the
/// section's subpackage reference, its `-f` filelist references, and
/// its body in a single struct so callers don't have to pattern-match
/// on `Section::Files` themselves and so future fields can be added
/// without changing every callback's signature.
#[derive(Debug)]
pub struct FilesSectionView<'a> {
    pub subpkg: Option<&'a rpm_spec::ast::SubpkgRef>,
    /// `-f <name>.lang` arguments on the `%files` header. Empty for
    /// the bare `%files` form.
    pub file_lists: &'a [rpm_spec::ast::Text],
    pub content: &'a [FilesContent<Span>],
}

/// Run `f` on every `%files` section in `spec`. The view bundles the
/// subpackage reference, the `-f` filelist arguments, and the section
/// body.
pub fn for_each_files_section<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(FilesSectionView<'ast>),
{
    walk_top_items(&spec.items, &mut f);
}

fn walk_top_items<'ast, F>(items: &'ast [SpecItem<Span>], f: &mut F)
where
    F: FnMut(FilesSectionView<'ast>),
{
    walk_top_items_inner(items, f, 0);
}

fn walk_top_items_inner<'ast, F>(items: &'ast [SpecItem<Span>], f: &mut F, depth: u32)
where
    F: FnMut(FilesSectionView<'ast>),
{
    if depth >= MAX_DEPTH {
        return;
    }
    for item in items {
        match item {
            SpecItem::Section(boxed) => {
                if let Section::Files {
                    subpkg,
                    file_lists,
                    content,
                    ..
                } = boxed.as_ref()
                {
                    f(FilesSectionView {
                        subpkg: subpkg.as_ref(),
                        file_lists,
                        content,
                    });
                }
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_top_items_inner(&branch.body, f, depth + 1);
                }
                if let Some(els) = &c.otherwise {
                    walk_top_items_inner(els, f, depth + 1);
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
    walk_entries_inner(items, f, 0);
}

fn walk_entries_inner<'ast, F>(items: &'ast [FilesContent<Span>], f: &mut F, depth: u32)
where
    F: FnMut(&'ast FileEntry<Span>),
{
    if depth >= MAX_DEPTH {
        return;
    }
    for item in items {
        match item {
            FilesContent::Entry(e) => f(e),
            FilesContent::Conditional(c) => {
                for branch in &c.branches {
                    walk_entries_inner(&branch.body, f, depth + 1);
                }
                if let Some(els) = &c.otherwise {
                    walk_entries_inner(els, f, depth + 1);
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

/// Resolve the canonical name of the package whose `%files` section
/// is being inspected. Combines [`resolve_subpkg_name`] with the
/// "fall back to the main package name" branch that three Phase 18
/// rules share (RPM364, RPM366, RPM371). Returns an empty string when
/// neither the subpackage nor the main package name can be resolved
/// — callers can use the result safely in `format!` messages without
/// another `unwrap_or`.
pub fn pkg_name_for(main_name: Option<&str>, subpkg: Option<&rpm_spec::ast::SubpkgRef>) -> String {
    resolve_subpkg_name(main_name, subpkg)
        .or_else(|| main_name.map(str::to_owned))
        .unwrap_or_default()
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
