//! Branch-aware AST walking.
//!
//! Generic infrastructure that, given a [`CoverageReport`] and a
//! profile identifier, decides for each `%if`/`%elif`/`%else` chain
//! which body should be walked when projecting the spec onto that
//! profile. Used by [`crate::contract`] (to gate `BuildRequires`
//! collection on conditional activity) and by `matrix diff` (to
//! compare effective preamble sets between two profiles).
//!
//! RPM conditional semantics are first-match-wins: at runtime only
//! one of `%if`/`%elif`/.../`%else` bodies runs. The walker mirrors
//! that — `ProfileBranchSelection::compute` resolves each conditional
//! to a single [`SelectedBody`] (or `None` if the whole conditional
//! is dead for the profile).
//!
//! ## Indeterminate policy
//!
//! When the evaluator can't decide a branch's condition (arithmetic
//! it can't model, undefined macros, shell substitution, …), the
//! caller picks one of two stances via [`IndeterminatePolicy`]:
//!
//! * `Skip` — treat indeterminate as if inactive. Conservative; right
//!   for "must this dep be declared?" gating where a false positive
//!   ("yes you have gcc") is worse than a false negative.
//! * `Include` — treat indeterminate as if active. Permissive; right
//!   for "could this reach the build?" exploration where a false
//!   negative ("no, this dep can't appear") is worse.
//!
//! ## What this is NOT
//!
//! Branch-aware walking only consumes a precomputed [`CoverageReport`];
//! it does NOT re-evaluate conditions. The cost of computing the report
//! is paid once per spec and shared across all profiles via the
//! `ResolvedTargetSet` API.

use std::collections::HashMap;

use rpm_spec::ast::{
    Conditional, FilesContent, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem,
};

use crate::branch_coverage::CoverageReport;

/// How to handle a branch the evaluator could not decide on the
/// given profile. See module docs for the trade-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndeterminatePolicy {
    /// Treat indeterminate branches as if inactive. Items inside them
    /// do NOT appear in the active set.
    Skip,
    /// Treat indeterminate branches as if active. Items inside them
    /// DO appear in the active set.
    Include,
}

/// Which body of a `%if/%elif/%else` chain should be walked for a
/// given profile. Returned by [`ProfileBranchSelection::selected`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectedBody {
    /// Walk `cond.branches[i].body` (i.e. the body of the i-th
    /// `%if`/`%elif`).
    Branch(usize),
    /// Walk `cond.otherwise` (the `%else` body).
    Otherwise,
}

/// Per-profile, per-conditional decision about which body to walk.
///
/// Keyed by the start line of the `%if` directive — that is the
/// shared join key between [`CoverageReport`] entries and AST
/// `Conditional` nodes (the parser's `data: Span` on a `Conditional`
/// has its `start_line` set to the `%if` line).
#[derive(Debug, Clone)]
pub struct ProfileBranchSelection {
    by_start_line: HashMap<u32, SelectedBody>,
}

impl ProfileBranchSelection {
    /// Compute the selection for `profile_id` from a precomputed
    /// [`CoverageReport`]. A conditional with no branch matching the
    /// inclusion policy AND no `%else` is fully dead for the profile
    /// and produces no entry in the map.
    #[must_use]
    pub fn compute(
        coverage: &CoverageReport,
        profile_id: &str,
        policy: IndeterminatePolicy,
    ) -> Self {
        let mut by_start_line = HashMap::with_capacity(coverage.conditionals.len());
        for cond in &coverage.conditionals {
            // Iterate branches in source order (`%if` first, then
            // `%elif`s). The first one that the policy accepts wins;
            // a true Indeterminate-under-Skip blocks the entire
            // conditional because we cannot know which body runs.
            let mut selected: Option<SelectedBody> = None;
            let mut blocked_by_indeterminate = false;
            for (i, b) in cond.branches.iter().enumerate() {
                let active = b.active_on.iter().any(|p| p == profile_id);
                let indet = b.indeterminate_on.iter().any(|p| p == profile_id);
                if active {
                    selected = Some(SelectedBody::Branch(i));
                    break;
                }
                if indet {
                    match policy {
                        IndeterminatePolicy::Include => {
                            selected = Some(SelectedBody::Branch(i));
                            break;
                        }
                        IndeterminatePolicy::Skip => {
                            // We can't know whether this branch runs
                            // at build time, so we also can't know
                            // whether a later branch's body runs —
                            // conservatively bail on the whole chain.
                            blocked_by_indeterminate = true;
                            break;
                        }
                    }
                }
                // Else: inactive — try the next branch.
            }
            // Otherwise is selected only when every `%if`/`%elif`
            // condition resolved to Inactive on this profile AND the
            // chain has a `%else` clause. An empty `branches` Vec
            // (degenerate AST where the parser recovered a malformed
            // `%else` with no `%if`) is NOT treated as "else always
            // runs" — that would synthesise active items from broken
            // input. Conservative: skip the whole conditional.
            if selected.is_none()
                && !blocked_by_indeterminate
                && cond.has_else
                && !cond.branches.is_empty()
            {
                selected = Some(SelectedBody::Otherwise);
            }
            if let Some(s) = selected {
                by_start_line.insert(cond.span.start_line, s);
            }
        }
        Self { by_start_line }
    }

    /// The selected body for the conditional starting at line `start_line`,
    /// or `None` if the conditional is fully dead for the profile.
    #[must_use]
    pub fn selected(&self, start_line: u32) -> Option<SelectedBody> {
        self.by_start_line.get(&start_line).copied()
    }

    /// Number of conditionals that have at least one body the
    /// profile will execute. Useful for tracing / diagnostics —
    /// pair it with [`CoverageReport::conditionals`] length at the
    /// call site if you need the "live / total" ratio.
    #[must_use]
    pub fn live_conditional_count(&self) -> usize {
        self.by_start_line.len()
    }
}

// ---------------------------------------------------------------------------
// Walkers
// ---------------------------------------------------------------------------

/// Visit every preamble item that survives branch projection onto
/// the profile encoded by `selection`. Visits the top-level
/// `Preamble` items AND the bodies of `%package` sub-package
/// preambles, both recursively through `%if`/`%elif`/`%else` chains.
///
/// Inactive branches contribute nothing. Items inside indeterminate
/// branches survive iff `selection` was computed with
/// [`IndeterminatePolicy::Include`].
pub fn walk_active_preamble<'ast, F>(
    spec: &'ast SpecFile<Span>,
    selection: &ProfileBranchSelection,
    mut on_item: F,
) where
    F: FnMut(&'ast PreambleItem<Span>),
{
    for item in &spec.items {
        walk_spec_item(item, selection, &mut on_item);
    }
}

fn walk_spec_item<'ast, F>(
    item: &'ast SpecItem<Span>,
    selection: &ProfileBranchSelection,
    on: &mut F,
) where
    F: FnMut(&'ast PreambleItem<Span>),
{
    match item {
        SpecItem::Preamble(p) => on(p),
        SpecItem::Conditional(c) => walk_conditional_spec_item(c, selection, on),
        SpecItem::Section(section) => {
            // %package sub-packages carry their own preamble; other
            // section kinds (description, build scripts, files,
            // changelog, …) hold no preamble items.
            if let Section::Package { content, .. } = section.as_ref() {
                for pc in content {
                    walk_preamble_content(pc, selection, on);
                }
            }
        }
        // MacroDef / BuildCondition / Include / Statement / Comment /
        // Blank — none are preamble items. The non_exhaustive AST
        // means we silently ignore unknown variants rather than
        // panic; they cannot contribute build deps either way.
        _ => {}
    }
}

fn walk_conditional_spec_item<'ast, F>(
    c: &'ast Conditional<Span, SpecItem<Span>>,
    selection: &ProfileBranchSelection,
    on: &mut F,
) where
    F: FnMut(&'ast PreambleItem<Span>),
{
    let Some(body) = pick_body_spec_item(c, selection) else {
        return;
    };
    for sub in body {
        walk_spec_item(sub, selection, on);
    }
}

fn pick_body_spec_item<'ast>(
    c: &'ast Conditional<Span, SpecItem<Span>>,
    selection: &ProfileBranchSelection,
) -> Option<&'ast [SpecItem<Span>]> {
    let s = selection.selected(c.data.start_line)?;
    match s {
        SelectedBody::Branch(i) => c.branches.get(i).map(|b| b.body.as_slice()),
        SelectedBody::Otherwise => c.otherwise.as_deref(),
    }
}

fn walk_preamble_content<'ast, F>(
    pc: &'ast PreambleContent<Span>,
    selection: &ProfileBranchSelection,
    on: &mut F,
) where
    F: FnMut(&'ast PreambleItem<Span>),
{
    match pc {
        PreambleContent::Item(p) => on(p),
        PreambleContent::Conditional(c) => {
            let Some(body) = pick_body_preamble_content(c, selection) else {
                return;
            };
            for sub in body {
                walk_preamble_content(sub, selection, on);
            }
        }
        // Comment / Blank — irrelevant for preamble-item collection.
        _ => {}
    }
}

fn pick_body_preamble_content<'ast>(
    c: &'ast Conditional<Span, PreambleContent<Span>>,
    selection: &ProfileBranchSelection,
) -> Option<&'ast [PreambleContent<Span>]> {
    let s = selection.selected(c.data.start_line)?;
    match s {
        SelectedBody::Branch(i) => c.branches.get(i).map(|b| b.body.as_slice()),
        SelectedBody::Otherwise => c.otherwise.as_deref(),
    }
}

/// Visit every files-section entry that survives branch projection.
/// Currently unused; provided for future symmetry (matrix diff is
/// likely to want `%files` content too, but Phase 9 starts with
/// preamble only).
#[doc(hidden)]
pub fn walk_active_files<'ast, F>(
    spec: &'ast SpecFile<Span>,
    selection: &ProfileBranchSelection,
    mut on_item: F,
) where
    F: FnMut(&'ast FilesContent<Span>),
{
    for item in &spec.items {
        if let SpecItem::Section(section) = item {
            if let Section::Files { content, .. } = section.as_ref() {
                for fc in content {
                    walk_files_content(fc, selection, &mut on_item);
                }
            }
        }
    }
}

fn walk_files_content<'ast, F>(
    fc: &'ast FilesContent<Span>,
    selection: &ProfileBranchSelection,
    on: &mut F,
) where
    F: FnMut(&'ast FilesContent<Span>),
{
    match fc {
        FilesContent::Conditional(c) => {
            let Some(body) = pick_body_files_content(c, selection) else {
                return;
            };
            for sub in body {
                walk_files_content(sub, selection, on);
            }
        }
        // For every non-conditional variant, deliver the node and
        // stop — files entries themselves don't carry nested bodies.
        other => on(other),
    }
}

fn pick_body_files_content<'ast>(
    c: &'ast Conditional<Span, FilesContent<Span>>,
    selection: &ProfileBranchSelection,
) -> Option<&'ast [FilesContent<Span>]> {
    let s = selection.selected(c.data.start_line)?;
    match s {
        SelectedBody::Branch(i) => c.branches.get(i).map(|b| b.body.as_slice()),
        SelectedBody::Otherwise => c.otherwise.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::ResolvedTargetSet;

    fn target_set_with(profiles: &[&str]) -> ResolvedTargetSet {
        use rpm_spec_profile::{ProfileSection, ResolveOptions, TargetEntry, resolve_target_set};
        let section = ProfileSection::new(None, std::collections::BTreeMap::new());
        let target = TargetEntry::from_profiles(profiles.iter().map(|s| s.to_string()).collect());
        resolve_target_set(
            &section,
            "test",
            &target,
            std::path::Path::new("/tmp"),
            ResolveOptions::default(),
        )
        .expect("resolve")
    }

    fn build_selection(
        spec_src: &str,
        profile_id: &str,
        policy: IndeterminatePolicy,
    ) -> (
        rpm_spec::ast::SpecFile<rpm_spec::ast::Span>,
        ProfileBranchSelection,
    ) {
        let parsed = parse(spec_src);
        let target_set = target_set_with(&[profile_id]);
        let cov = CoverageReport::compute(
            &parsed.spec,
            &target_set,
            &crate::bcond::BcondOverrides::default(),
        );
        let sel = ProfileBranchSelection::compute(&cov, profile_id, policy);
        (parsed.spec, sel)
    }

    /// Smoke: spec with `%if 0%{?rhel}` — branch active on rhel,
    /// inactive on alt; on alt the `%else` runs (here there is no
    /// %else so conditional is fully dead → no Preamble item from
    /// the if-block on alt).
    const RHEL_GATED: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: common-pkg
%if 0%{?rhel}
BuildRequires: rhel-only
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn rhel_only_dep_visible_on_rhel() {
        let (spec, sel) = build_selection(RHEL_GATED, "rhel-9-x86_64", IndeterminatePolicy::Skip);
        let mut names = Vec::new();
        walk_active_preamble(&spec, &sel, |p| {
            if let rpm_spec::ast::TagValue::Dep(_) = &p.value {
                if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                    names.push(format!("{:?}", p.tag));
                }
            }
        });
        // Two BuildRequires items survive on rhel: common-pkg and rhel-only.
        assert_eq!(names.len(), 2, "expected 2 BR on rhel, got {names:?}");
    }

    #[test]
    fn rhel_only_dep_hidden_on_alt() {
        let (spec, sel) =
            build_selection(RHEL_GATED, "altlinux-10-x86_64", IndeterminatePolicy::Skip);
        let mut br_count = 0;
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                br_count += 1;
            }
        });
        // Only common-pkg survives on alt (no %else, rhel branch dead).
        assert_eq!(br_count, 1, "expected 1 BR on alt, got {br_count}");
    }

    const ELSE_BRANCH: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 0%{?rhel}
BuildRequires: rhel-pkg
%else
BuildRequires: non-rhel-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn else_branch_runs_when_if_inactive() {
        let (spec, sel) =
            build_selection(ELSE_BRANCH, "altlinux-10-x86_64", IndeterminatePolicy::Skip);
        let mut br_count = 0;
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                br_count += 1;
            }
        });
        // The %else body runs on alt (rhel branch is Inactive,
        // has_else=true).
        assert_eq!(br_count, 1, "expected 1 BR (non-rhel-pkg) on alt");
    }

    #[test]
    fn else_branch_inactive_when_if_active() {
        let (spec, sel) = build_selection(ELSE_BRANCH, "rhel-9-x86_64", IndeterminatePolicy::Skip);
        let mut br_count = 0;
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                br_count += 1;
            }
        });
        // %if branch runs, %else does not.
        assert_eq!(br_count, 1, "expected 1 BR (rhel-pkg) on rhel");
    }

    #[test]
    fn skip_policy_drops_indeterminate_branches() {
        // `%if 0%{?rhel} >= 8` arithmetic Raw → Indeterminate. Under
        // Skip the conditional is conservatively dead → BR inside not
        // collected. Empty `%else` body in this test so only the BR
        // outside any conditional is left.
        const ARITH: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: outside-cond
%if 0%{?rhel} >= 8
BuildRequires: inside-cond
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let (spec, sel) = build_selection(ARITH, "rhel-9-x86_64", IndeterminatePolicy::Skip);
        let mut br_count = 0;
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                br_count += 1;
            }
        });
        assert_eq!(br_count, 1, "Skip policy must hide indeterminate BR");
    }

    #[test]
    fn include_policy_keeps_indeterminate_branches() {
        const ARITH: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: outside-cond
%if 0%{?rhel} >= 8
BuildRequires: inside-cond
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let (spec, sel) = build_selection(ARITH, "rhel-9-x86_64", IndeterminatePolicy::Include);
        let mut br_count = 0;
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                br_count += 1;
            }
        });
        assert_eq!(br_count, 2, "Include policy must keep indeterminate BR");
    }

    #[test]
    fn nested_if_inside_if_recurses_into_inner_body() {
        // Outer `%if 0%{?rhel}` and inner `%ifarch x86_64`: only the
        // intersection (rhel-9-x86_64) sees the inner BR. Pins the
        // walker's recursion into selected branch bodies.
        const NESTED: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 0%{?rhel}
%ifarch x86_64
BuildRequires: inner-pkg
%endif
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let (spec, sel) = build_selection(NESTED, "rhel-9-x86_64", IndeterminatePolicy::Skip);
        let mut names: Vec<String> = Vec::new();
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                if let rpm_spec::ast::TagValue::Dep(rpm_spec::ast::DepExpr::Atom(atom)) = &p.value {
                    if let Some(s) = atom.name.literal_str() {
                        names.push(s.to_string());
                    }
                }
            }
        });
        assert_eq!(
            names,
            vec!["inner-pkg"],
            "rhel-9-x86_64 must see inner-pkg via nested-active branches"
        );
    }

    #[test]
    fn elif_chain_picks_first_matching_branch() {
        // %if 0%{?fedora} → inactive on rhel
        // %elif 0%{?rhel}  → active on rhel
        // body of %elif must be selected, not the %if.
        const ELIF: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 0%{?fedora}
BuildRequires: fedora-pkg
%elif 0%{?rhel}
BuildRequires: rhel-pkg
%else
BuildRequires: other-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let (spec, sel) = build_selection(ELIF, "rhel-9-x86_64", IndeterminatePolicy::Skip);
        let mut names: Vec<String> = Vec::new();
        walk_active_preamble(&spec, &sel, |p| {
            if matches!(p.tag, rpm_spec::ast::Tag::BuildRequires) {
                if let rpm_spec::ast::TagValue::Dep(rpm_spec::ast::DepExpr::Atom(atom)) = &p.value {
                    if let Some(s) = atom.name.literal_str() {
                        names.push(s.to_string());
                    }
                }
            }
        });
        assert_eq!(
            names,
            vec!["rhel-pkg"],
            "elif body must be picked when %if is inactive; got {names:?}"
        );
    }

    #[test]
    fn live_conditional_count_matches_kept_entries() {
        let (_, sel) = build_selection(ELSE_BRANCH, "rhel-9-x86_64", IndeterminatePolicy::Skip);
        // ELSE_BRANCH has exactly one Conditional and it has a body
        // running on rhel → live_conditional_count == 1.
        assert_eq!(sel.live_conditional_count(), 1);
    }
}
