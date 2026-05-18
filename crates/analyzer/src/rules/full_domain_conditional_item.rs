//! RPM453 `full-domain-conditional-item` — flag dependency atoms whose
//! `%ifarch`-guarded copies together cover every arch in the profile's
//! target universe.
//!
//! ```text
//! %ifarch x86_64
//! BuildRequires: foo
//! %endif
//! %ifarch aarch64
//! BuildRequires: foo
//! %endif
//! ```
//! If the profile only ever targets `{x86_64, aarch64}`, the two
//! conditional copies together fire on every build — i.e. `foo` is
//! effectively unconditional. The pair can be replaced by a single
//! top-level `BuildRequires: foo`.
//!
//! Companion to RPM452 (`complementary-guards-same-item`), which uses
//! boolean DNF tautology checks. RPM453 specialises to arch-only
//! guards where the profile's [`rpm_spec_profile::Profile::arch_universe`]
//! is the cover budget — boolean DNF can't prove arch-cover without it.

use std::collections::{BTreeMap, BTreeSet};

use rpm_spec::ast::{
    CondExpr, CondKind, DepExpr, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem,
    TagValue,
};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{DepTagKey, dep_atom_text, literal_archs};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM453",
    name: "full-domain-conditional-item",
    description: "Multiple `%ifarch`-guarded copies of the same dependency atom together cover \
                  every arch in the profile's target universe — make the entry unconditional.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug)]
struct Occurrence {
    tag: DepTagKey,
    atom_text: String,
    arch_guard: BTreeSet<String>,
    span: Span,
}

/// Multiple `%ifarch`-guarded copies of the same dependency atom together cover every arch in the profile's target universe — make the entry unconditional.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct FullDomainConditionalItem {
    diagnostics: Vec<Diagnostic>,
    universe: BTreeSet<String>,
}

impl FullDomainConditionalItem {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for FullDomainConditionalItem {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.universe.is_empty() {
            return;
        }

        // Main package. The arch stack is owned mutably here and grows
        // only when a real `%ifarch` adds a frame — see `walk_top_items`
        // for the push/pop discipline. Avoids the per-branch
        // `arch_stack.to_vec()` allocation the previous implementation
        // paid on every `%if` entry.
        let mut occs = Vec::new();
        let mut stack: Vec<BTreeSet<String>> = Vec::new();
        walk_top_items(&spec.items, &mut stack, &mut occs);
        emit(&occs, &self.universe, &mut self.diagnostics);

        // Subpackages.
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            let mut sub_occs = Vec::new();
            let mut sub_stack: Vec<BTreeSet<String>> = Vec::new();
            walk_preamble_content(content, &mut sub_stack, &mut sub_occs);
            emit(&sub_occs, &self.universe, &mut self.diagnostics);
        }
    }
}

fn record_preamble(
    item: &PreambleItem<Span>,
    stack: &[BTreeSet<String>],
    occs: &mut Vec<Occurrence>,
) {
    let Some(tag) = DepTagKey::from_tag(&item.tag) else {
        return;
    };
    let TagValue::Dep(expr) = &item.value else {
        return;
    };
    let DepExpr::Atom(atom) = expr else {
        return;
    };
    let Some(atom_text) = dep_atom_text(atom) else {
        return;
    };
    // The guard is the INTERSECTION of every enclosing arch list (each
    // nested `%ifarch` further narrows the active arch set). Empty
    // stack = unconditional; emit-time skips those.
    //
    // Hot path: in-place `retain` instead of rebuilding the set with
    // `.intersection().cloned().collect()` on every level — the old
    // form cloned every retained `String` once per stack frame. Early
    // exit on empty intersection keeps the worst case (contradictory
    // nesting) cheap too.
    if stack.is_empty() {
        return;
    }
    let mut guard = stack[0].clone();
    for s in &stack[1..] {
        guard.retain(|x| s.contains(x));
        if guard.is_empty() {
            break;
        }
    }
    if guard.is_empty() {
        return; // contradictory nesting — RPM113 covers it
    }
    occs.push(Occurrence {
        tag,
        atom_text,
        arch_guard: guard,
        span: item.data,
    });
}

/// Try to push an arch frame for this branch. Returns `true` when a
/// frame was pushed (caller is responsible for the matching `pop`).
///
/// Only `%ifarch` / `%elifarch` branches whose arch-list is fully
/// literal (no macros) contribute a frame; anything else leaves the
/// stack alone — which is the same as the previous semantics.
///
/// Generic over `Body` because the same logic applies to both
/// `SpecItem<Span>` and `PreambleContent<Span>` branches.
fn try_push_arch_frame<Body>(
    stack: &mut Vec<BTreeSet<String>>,
    branch: &rpm_spec::ast::CondBranch<Span, Body>,
) -> bool {
    if matches!(branch.kind, CondKind::IfArch | CondKind::ElifArch)
        && let CondExpr::ArchList(list) = &branch.expr
        && let Some(archs) = literal_archs(list)
    {
        stack.push(archs);
        return true;
    }
    false
}

fn walk_top_items(
    items: &[SpecItem<Span>],
    arch_stack: &mut Vec<BTreeSet<String>>,
    occs: &mut Vec<Occurrence>,
) {
    for it in items {
        match it {
            SpecItem::Preamble(p) => record_preamble(p, arch_stack, occs),
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    let pushed = try_push_arch_frame(arch_stack, branch);
                    walk_top_items(&branch.body, arch_stack, occs);
                    if pushed {
                        arch_stack.pop();
                    }
                }
                if let Some(els) = &c.otherwise {
                    // `%else` inherits the surrounding stack — no frame
                    // because RPM's `%ifarch` `%else` arm fires on
                    // *every other* arch, not on a single literal list.
                    walk_top_items(els, arch_stack, occs);
                }
            }
            _ => {}
        }
    }
}

fn walk_preamble_content(
    items: &[PreambleContent<Span>],
    arch_stack: &mut Vec<BTreeSet<String>>,
    occs: &mut Vec<Occurrence>,
) {
    for it in items {
        match it {
            PreambleContent::Item(p) => record_preamble(p, arch_stack, occs),
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    let pushed = try_push_arch_frame(arch_stack, branch);
                    walk_preamble_content(&branch.body, arch_stack, occs);
                    if pushed {
                        arch_stack.pop();
                    }
                }
                if let Some(els) = &c.otherwise {
                    walk_preamble_content(els, arch_stack, occs);
                }
            }
            _ => {}
        }
    }
}

fn emit(occs: &[Occurrence], universe: &BTreeSet<String>, diagnostics: &mut Vec<Diagnostic>) {
    let mut buckets: BTreeMap<(DepTagKey, &str), Vec<&Occurrence>> = BTreeMap::new();
    for o in occs {
        buckets
            .entry((o.tag, o.atom_text.as_str()))
            .or_default()
            .push(o);
    }
    for (_key, group) in buckets {
        if group.len() < 2 {
            continue;
        }
        let union: BTreeSet<&String> = group.iter().flat_map(|o| o.arch_guard.iter()).collect();
        let universe_refs: BTreeSet<&String> = universe.iter().collect();
        if !universe_refs.is_subset(&union) {
            continue;
        }
        for o in &group {
            diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`{label}: {atom}` is guarded by `%ifarch` blocks that together cover \
                         every arch in the profile's universe — make it unconditional",
                        label = o.tag.label(),
                        atom = o.atom_text,
                    ),
                    o.span,
                )
                .with_suggestion(Suggestion::new(
                    "drop the `%ifarch` wrappers and list this entry once at the top level",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl Lint for FullDomainConditionalItem {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.universe = profile.arch_universe().cloned().unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn build_profile_with_universe(archs: &[&str]) -> Profile {
        use rpm_spec_profile::merge::{ArchPatch, ProfilePatch};
        let mut p = Profile::default();
        let universe: BTreeSet<String> = archs.iter().map(|a| (*a).to_string()).collect();
        p.apply(ProfilePatch {
            arch: ArchPatch {
                target_arch_universe: Some(universe),
                ..Default::default()
            },
            ..Default::default()
        });
        p
    }

    fn run(src: &str, archs: &[&str]) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let profile = build_profile_with_universe(archs);
        let mut lint = FullDomainConditionalItem::new();
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_two_arch_pair_covering_universe() {
        let src = "Name: x\n\
%ifarch x86_64\n\
BuildRequires: foo\n\
%endif\n\
%ifarch aarch64\n\
BuildRequires: foo\n\
%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM453"));
    }

    #[test]
    fn silent_when_pair_does_not_cover_universe() {
        let src = "Name: x\n\
%ifarch x86_64\nBuildRequires: foo\n%endif\n\
%ifarch aarch64\nBuildRequires: foo\n%endif\n";
        // Universe has three arches — pair only covers two.
        let diags = run(src, &["x86_64", "aarch64", "ppc64le"]);
        assert!(diags.is_empty());
    }

    #[test]
    fn silent_when_profile_has_no_universe() {
        let src = "Name: x\n\
%ifarch x86_64\nBuildRequires: foo\n%endif\n\
%ifarch aarch64\nBuildRequires: foo\n%endif\n";
        let outcome = parse(src);
        let mut lint = FullDomainConditionalItem::new();
        lint.set_profile(&Profile::default());
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn silent_for_single_arch_block() {
        let src = "Name: x\n%ifarch x86_64\nBuildRequires: foo\n%endif\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert!(diags.is_empty());
    }

    #[test]
    fn flags_inside_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%ifarch x86_64\n\
Requires: foo\n\
%endif\n\
%ifarch aarch64\n\
Requires: foo\n\
%endif\n\
%description devel\nbody\n";
        let diags = run(src, &["x86_64", "aarch64"]);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }
}
