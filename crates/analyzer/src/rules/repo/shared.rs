//! Helpers shared between the RPM-REPO-* rules.
//!
//! Extracted so the three top-level rule files stay short and read
//! identically (each one is essentially "collect deps; for each dep,
//! call `lookup`; emit if `outcome == X`"). Macro expansion via
//! `MacroRegistry::expand_to_literal` lives here too — used by every
//! rule that needs to handle `BuildRequires: %{python3_pkgversion}-foo`.

use std::sync::Arc;

use rpm_spec::ast::{
    BoolDep, BuildScriptKind, Conditional, DepAtom, DepExpr, PreambleContent, PreambleItem,
    Section, ShellBody, Span, SpecFile, SpecItem, Tag, TagValue, VerOp,
};
use rpm_spec_profile::{MacroRegistry, Profile};
use rpm_spec_repo_core::{CapFlags, Capability, EVR, RepoUniverse};

use crate::bcond::{BcondMap, BcondOverrides};
use crate::branch_coverage::evaluate_branch;
use crate::diagnostic::Diagnostic;

/// Macro expansion depth limit for BR/Requires atoms. Matches the
/// convention used by `branch_coverage`, `files::classifier` and other
/// analyzer modules — chains like `%{python3_pkgversion} →
/// %{__python3_pkgversion} → "3.11"` resolve well within 8 hops.
///
/// Public so RPM-REPO-* siblings (e.g. `upgrade_check.rs`) share one
/// budget rather than each carrying a private copy that can drift.
pub const MACRO_EXPAND_DEPTH: u8 = 8;

/// Bundle of state every RPM-REPO-* rule needs. Previously also
/// cached a priority map; with the SQLite-backed universe, priority
/// lookup is one O(1) call on the universe itself, so the bundle is
/// a thin wrapper now (kept as a struct rather than a bare
/// `Arc<RepoUniverse>` so adding session-level caches later — e.g.
/// per-spec capability cache — stays a local change).
#[derive(Debug)]
pub(super) struct RepoContextState {
    pub universe: Arc<RepoUniverse>,
}

impl RepoContextState {
    pub fn build(universe: Arc<RepoUniverse>) -> Self {
        Self { universe }
    }
}

/// State + plumbing shared by every RPM-REPO-* lint that walks a single
/// tag (`BuildRequires` or `Requires`) and emits at most one diagnostic
/// per declared dep.
///
/// Each rule struct wraps a `RepoRule`, forwards the `Lint` trait
/// boilerplate (`set_profile`, `set_repo_universe`, `take_diagnostics`)
/// and supplies a per-dep closure via [`RepoRule::walk_deps`] that
/// decides whether the dep deserves a diagnostic.
#[derive(Debug, Default)]
pub(super) struct RepoRule {
    pub diagnostics: Vec<Diagnostic>,
    pub profile: Option<Profile>,
    pub state: Option<RepoContextState>,
}

impl RepoRule {
    pub fn set_profile(&mut self, profile: &Profile) {
        self.profile = Some(profile.clone());
    }

    pub fn set_repo_universe(&mut self, universe: Option<Arc<RepoUniverse>>) {
        // The universe is keyed by profile, so we only build the
        // cached priority map when both halves are present. Calling
        // `set_repo_universe(None)` (no repos configured / cache miss)
        // leaves `state` as `None` and every rule short-circuits.
        if let (Some(u), Some(_)) = (universe, &self.profile) {
            self.state = Some(RepoContextState::build(u));
        }
    }

    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Walk every dep declared under a tag matching `tag_matcher`
    /// and let `emit` push diagnostics for the ones that deserve one.
    ///
    /// Short-circuits silently when the universe or profile haven't
    /// been wired up — see the module docs and `repo/mod.rs` for the
    /// rationale (a missing universe is "no repos configured", not a
    /// per-lint warning).
    pub fn walk_deps<TM, F>(&mut self, spec: &SpecFile<Span>, tag_matcher: TM, mut emit: F)
    where
        TM: Fn(&Tag) -> bool,
        F: FnMut(&RepoContextState, ProjectedDep, &mut Vec<Diagnostic>),
    {
        let Some(state) = &self.state else {
            return;
        };
        let Some(profile) = &self.profile else {
            return;
        };
        // Build the BcondMap once per spec, before walking. Empty
        // overrides — RPM-REPO doesn't expose `--with`/`--without`
        // flags directly; the user controls bcond defaults via
        // their profile / config. `BcondMap` is cheap (one pass over
        // the spec) and feeds `evaluate_branch` for the conditional
        // gating below.
        let bcond = BcondMap::from_spec(spec, &BcondOverrides::default());
        for dep in project_deps(spec, profile, &bcond, tag_matcher) {
            emit(state, dep, &mut self.diagnostics);
        }
    }
}

/// One declared dependency, projected from the spec AST into the
/// shape `rpm-spec-repo-resolver::lookup` consumes.
///
/// `span` carries the source location so the lint diagnostic can
/// anchor on the original `BuildRequires:` / `Requires:` line. The
/// `display` form is the human-readable original (e.g.
/// `"cmake >= 3.26"`) — kept for diagnostic messages so the user
/// sees what they wrote, not the resolver's normalised form.
#[derive(Debug)]
pub struct ProjectedDep {
    pub capability: Capability,
    pub display: String,
    pub span: Span,
}

/// `project_deps` shortcut for callers that just want the active
/// `BuildRequires:` set, already projected to resolver shape with
/// `%if` / `%else` branches gated against `profile` + bcond.
/// Returns `Vec<Capability>` because that's the only field
/// downstream consumers (`matrix buildroot solve`, future
/// `matrix upgrade-sim`) actually look at.
pub fn active_buildrequires(spec: &SpecFile<Span>, profile: &Profile) -> Vec<Capability> {
    let bcond = BcondMap::from_spec(spec, &BcondOverrides::default());
    project_deps(spec, profile, &bcond, |t| matches!(t, Tag::BuildRequires))
        .into_iter()
        .map(|d| d.capability)
        .collect()
}

/// Walk every top-level preamble item whose tag matches
/// `tag_matcher` and project its dep atoms into resolver shape.
///
/// Atoms whose name expands to nothing (unrecognised macro, or a
/// macro that doesn't resolve to a literal) are silently skipped —
/// the lint can't say anything useful about a name it doesn't
/// know. Boolean / rich deps are flattened via the same walker the
/// resolver uses (`for_each_dep_atom`).
///
/// `%if` / `%elif` / `%else` blocks are evaluated against `profile`
/// plus `bcond` so cross-distro conditional BRs only count when the
/// branch would actually fire — e.g. `%if "%_vendor" == "rosa"` in
/// a redos profile (where `%_vendor` resolves to `redhat`) is
/// definitively skipped instead of emitting a false-positive
/// "missing provider" diagnostic for the rosa-only atom.
///
/// Branches whose condition is indeterminate (unknown macro,
/// unmodelled expression) are skipped along with their `%else`
/// counterpart — being conservative on false positives is the
/// design goal of the RPM-REPO-* family.
pub fn project_deps<F>(
    spec: &SpecFile<Span>,
    profile: &Profile,
    bcond: &BcondMap,
    tag_matcher: F,
) -> Vec<ProjectedDep>
where
    F: Fn(&Tag) -> bool,
{
    let mut out = Vec::new();
    let mut items: Vec<&PreambleItem<Span>> = Vec::new();
    collect_active_spec_items(&spec.items, profile, bcond, &mut items);
    for sub in iter_subpackage_contents(spec) {
        collect_active_preamble_contents(sub, profile, bcond, &mut items);
    }
    for item in items {
        if !tag_matcher(&item.tag) {
            continue;
        }
        let TagValue::Dep(expr) = &item.value else {
            continue;
        };
        let span = item.data;
        collect_into(expr, &mut out, &profile.macros, span);
    }
    out
}

/// Collect preamble items from `SpecItem` slices, following only
/// the active branch of each `Conditional` per the policy in the
/// module docs.
fn collect_active_spec_items<'a>(
    items: &'a [SpecItem<Span>],
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<&'a PreambleItem<Span>>,
) {
    for item in items {
        match item {
            SpecItem::Preamble(p) => out.push(p),
            SpecItem::Conditional(cond) => {
                walk_spec_conditional(cond, profile, bcond, out);
            }
            // Other variants (Section, MacroDef, BuildCondition,
            // etc.) don't contribute preamble items at this level —
            // subpackage `Section::Package` content is walked via
            // `iter_subpackage_contents` so we can apply the same
            // conditional filter to nested `PreambleContent`.
            _ => {}
        }
    }
}

fn walk_spec_conditional<'a>(
    cond: &'a Conditional<Span, SpecItem<Span>>,
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<&'a PreambleItem<Span>>,
) {
    // First `Ok(true)` arm is the active one — return immediately.
    // `Err(_)` indeterminate aborts the whole conditional (we can't
    // prove subsequent branches' antecedents are decisively false,
    // and `%else` requires that proof too). `Ok(false)` advances.
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) => {
                collect_active_spec_items(&branch.body, profile, bcond, out);
                return;
            }
            Ok(false) => continue,
            Err(_) => return,
        }
    }
    if let Some(els) = &cond.otherwise {
        collect_active_spec_items(els, profile, bcond, out);
    }
}

fn collect_active_preamble_contents<'a>(
    contents: &'a [PreambleContent<Span>],
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<&'a PreambleItem<Span>>,
) {
    for c in contents {
        match c {
            PreambleContent::Item(p) => out.push(p),
            PreambleContent::Conditional(cond) => {
                walk_preamble_conditional(cond, profile, bcond, out);
            }
            _ => {}
        }
    }
}

fn walk_preamble_conditional<'a>(
    cond: &'a Conditional<Span, PreambleContent<Span>>,
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<&'a PreambleItem<Span>>,
) {
    // Same policy as `walk_spec_conditional`: first decisive `true`
    // wins, indeterminate aborts the whole conditional.
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) => {
                collect_active_preamble_contents(&branch.body, profile, bcond, out);
                return;
            }
            Ok(false) => continue,
            Err(_) => return,
        }
    }
    if let Some(els) = &cond.otherwise {
        collect_active_preamble_contents(els, profile, bcond, out);
    }
}

/// Yield each subpackage's `PreambleContent` slice. `%package` blocks
/// nested inside a top-level `Conditional` are skipped (matches the
/// existing `iter_packages` convention — wholesale conditional
/// subpackages are rare and the partial-view risk outweighs the catch
/// rate).
fn iter_subpackage_contents(spec: &SpecFile<Span>) -> Vec<&[PreambleContent<Span>]> {
    let mut out: Vec<&[PreambleContent<Span>]> = Vec::new();
    for item in &spec.items {
        if let SpecItem::Section(boxed) = item
            && let Section::Package { content, .. } = boxed.as_ref()
        {
            out.push(content.as_slice());
        }
    }
    out
}

fn collect_into(
    expr: &DepExpr,
    out: &mut Vec<ProjectedDep>,
    macros: &MacroRegistry,
    span: Span,
) {
    match expr {
        DepExpr::Atom(atom) => {
            if let Some(projected) = project_atom(atom, macros, span) {
                out.push(projected);
            }
        }
        DepExpr::Rich(boxed) => collect_into_bool(boxed, out, macros, span),
        _ => {}
    }
}

fn collect_into_bool(
    b: &BoolDep,
    out: &mut Vec<ProjectedDep>,
    macros: &MacroRegistry,
    span: Span,
) {
    match b {
        BoolDep::And(xs) | BoolDep::Or(xs) | BoolDep::With(xs) => {
            for x in xs {
                collect_into(x, out, macros, span);
            }
        }
        BoolDep::Without { left, .. } => collect_into(left, out, macros, span),
        // `If` / `Unless` arms gate `then` (and optionally `otherwise`)
        // on a runtime condition. For RPM-REPO-* resolvability checks
        // the gated atoms are still real deps the resolver must be
        // able to satisfy when the condition fires — skipping them
        // produces false negatives (`BuildRequires: (cmake if linux)`
        // really does require `cmake` to be available). Conservatively
        // walk both branches; we don't model the condition itself.
        BoolDep::If { then, otherwise, .. } | BoolDep::Unless { then, otherwise, .. } => {
            collect_into(then, out, macros, span);
            if let Some(other) = otherwise.as_deref() {
                collect_into(other, out, macros, span);
            }
        }
        _ => {}
    }
}

fn project_atom(atom: &DepAtom, macros: &MacroRegistry, span: Span) -> Option<ProjectedDep> {
    let name = resolve_text(atom.name.literal_str()?.trim(), macros)?;
    if name.is_empty() {
        return None;
    }
    let arch = atom
        .arch
        .as_ref()
        .and_then(|t| t.literal_str())
        .map(str::trim);

    let (flags, evr) = if let Some(c) = atom.constraint.as_ref() {
        let op = match c.op {
            VerOp::Lt => CapFlags::LT,
            VerOp::Le => CapFlags::LE,
            VerOp::Eq => CapFlags::EQ,
            VerOp::Ge => CapFlags::GE,
            VerOp::Gt => CapFlags::GT,
            // VerOp is non_exhaustive; unknown operators short-circuit.
            _ => return None,
        };
        let version = resolve_text(c.evr.version.literal_str()?.trim(), macros)?;
        let release = if let Some(rel) = c.evr.release.as_ref() {
            resolve_text(rel.literal_str()?.trim(), macros)?
        } else {
            // The resolver treats an empty release as "no release
            // constraint" via EVR ordering rules.
            String::new()
        };
        let evr = EVR::new(c.evr.epoch, version, release);
        (op, Some(evr))
    } else {
        (CapFlags::None, None)
    };

    let display = format_capability_display(&name, arch, flags, evr.as_ref());

    Some(ProjectedDep {
        capability: Capability {
            name: Arc::from(name),
            flags,
            evr,
        },
        display,
        span,
    })
}

/// Render a capability as the user would have typed it: `name(arch) op
/// E:V-R` with optional parts elided. Arch suffix (`foo(x86-64)`) is
/// not part of the resolver's capability name — the rpm-md primary.xml
/// stores it via the package's `arch` field, not as part of the
/// capability string. Kept here only for diagnostics.
///
/// Delegates the name + EVR rendering to [`Capability::display`] (the
/// shared canonical form used by `matrix buildroot solve` too), then
/// inserts the optional `(arch)` suffix between name and EVR — a
/// detail unique to spec-side display text.
fn format_capability_display(
    name: &str,
    arch: Option<&str>,
    op: CapFlags,
    evr: Option<&EVR>,
) -> String {
    let cap = Capability {
        name: Arc::from(name),
        flags: op,
        evr: evr.cloned(),
    };
    let base = cap.display();
    let Some(arch) = arch else {
        return base;
    };
    // Splice `(arch)` after the name, before any ` op E:V-R` suffix.
    // The name has no spaces (rpm capability names forbid them), so
    // the first space — if present — marks the start of the operator.
    let mut out = String::with_capacity(base.len() + arch.len() + 2);
    if let Some(idx) = base.find(' ') {
        out.push_str(&base[..idx]);
        out.push('(');
        out.push_str(arch);
        out.push(')');
        out.push_str(&base[idx..]);
    } else {
        out.push_str(&base);
        out.push('(');
        out.push_str(arch);
        out.push(')');
    }
    out
}

/// Expand any `%{name}` / `%name` references via the profile's
/// macro registry. A literal in → that literal out. References that
/// can't be resolved → `None` so the caller drops the dep.
fn resolve_text(text: &str, macros: &MacroRegistry) -> Option<String> {
    if !text.contains('%') {
        return Some(text.to_string());
    }
    // `expand_to_literal` only handles a pure macro name. For mixed
    // text like `%{python3_pkgversion}-numpy` we'd need the body-level
    // `expand_body` helper which isn't public — conservative: return
    // `None` and let the lint skip the atom.
    macros.expand_to_literal(
        text.trim_start_matches('%').trim_start_matches('{'),
        MACRO_EXPAND_DEPTH,
    )
}

/// Collect every active build-script section's `(kind, body, span)`
/// triple under `Conditional` gating. Shared between RPM-REPO-010
/// and RPM-REPO-011 so the section-list and conditional policy stay
/// in sync. Indeterminate or non-first-active branches are skipped
/// — same false-negative-over-false-positive trade-off the other
/// repo lints use.
pub fn collect_active_build_scripts<'a>(
    items: &'a [SpecItem<Span>],
    profile: &Profile,
    bcond: &BcondMap,
) -> Vec<(BuildScriptKind, &'a ShellBody<Span>, Span)> {
    let mut out = Vec::new();
    collect_active_build_scripts_inner(items, profile, bcond, &mut out);
    out
}

fn collect_active_build_scripts_inner<'a>(
    items: &'a [SpecItem<Span>],
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<(BuildScriptKind, &'a ShellBody<Span>, Span)>,
) {
    for item in items {
        match item {
            SpecItem::Section(boxed) => {
                if let Section::BuildScript { kind, body, data } = boxed.as_ref() {
                    out.push((*kind, body, *data));
                }
            }
            SpecItem::Conditional(cond) => {
                walk_build_script_conditional(cond, profile, bcond, out);
            }
            _ => {}
        }
    }
}

fn walk_build_script_conditional<'a>(
    cond: &'a Conditional<Span, SpecItem<Span>>,
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<(BuildScriptKind, &'a ShellBody<Span>, Span)>,
) {
    // Walk branches in order; the first `Ok(true)` arm is the active
    // one and stops the walk. `Ok(false)` lets us continue (preceding
    // arms decisively didn't fire); `Err(_)` is indeterminate, and
    // policy is to bail conservatively (we can't prove `%else`'s
    // antecedents are decisively false either).
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) => {
                collect_active_build_scripts_inner(&branch.body, profile, bcond, out);
                return;
            }
            Ok(false) => continue,
            Err(_) => return,
        }
    }
    // `%else` fires only when every `%if`/`%elif` was decisively
    // false (the `Ok(false) => continue` arm above is what carries
    // us here from a fully-falsified chain).
    if let Some(els) = &cond.otherwise {
        collect_active_build_scripts_inner(els, profile, bcond, out);
    }
}

/// Compute the inclusive source-line ranges inside `body` that the
/// active profile will NOT execute. Used by RPM-REPO-010/011 to skip
/// tool-path / command scans inside inactive `%if` arms within a
/// build-script section. Indeterminate branches are conservatively
/// inactive; `%else` is inactive whenever any sibling is active OR
/// indeterminate.
pub fn inactive_line_ranges(
    body: &ShellBody<Span>,
    profile: &Profile,
    bcond: &BcondMap,
) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for cond in &body.conditionals {
        let mut active_branch_idx: Option<usize> = None;
        let mut indeterminate_seen = false;
        for (i, branch) in cond.branches.iter().enumerate() {
            match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
                Ok(true) if !indeterminate_seen && active_branch_idx.is_none() => {
                    active_branch_idx = Some(i);
                }
                Ok(true) | Ok(false) => {}
                Err(_) => indeterminate_seen = true,
            }
        }
        for (i, branch) in cond.branches.iter().enumerate() {
            if Some(i) != active_branch_idx {
                let span = branch.data;
                out.push((span.start_line, span.end_line));
            }
        }
        if let Some(els) = &cond.otherwise
            && (active_branch_idx.is_some() || indeterminate_seen)
        {
            out.push((els.data.start_line, els.data.end_line));
        }
    }
    out
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures;

    #[test]
    fn active_buildrequires_skips_inactive_vendor_branch() {
        // Motivating bug: spec uses
        //   %if "%_vendor" == "rosa"
        //   BuildRequires: lib64foo
        //   %else
        //   BuildRequires: foo
        //   %endif
        // On a profile with `%_vendor = "redhat"`, only `foo` should
        // be returned by `active_buildrequires` — the rosa-guarded
        // `lib64foo` arm is dead.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n\
                   %if \"%_vendor\" == \"rosa\"\n\
                   BuildRequires: lib64foo\n\
                   %else\n\
                   BuildRequires: foo\n\
                   %endif\n\
                   %description\nx\n";
        let mut profile = test_fixtures::redos_profile();
        profile.macros.insert(
            "_vendor".to_string(),
            rpm_spec_profile::MacroEntry::literal(
                "redhat",
                rpm_spec_profile::Provenance::Override,
            ),
        );
        let outcome = crate::session::parse(src);
        let brs = active_buildrequires(&outcome.spec, &profile);
        let names: Vec<&str> = brs.iter().map(|c| c.name.as_ref()).collect();
        assert!(names.contains(&"foo"), "got {names:?}");
        assert!(
            !names.contains(&"lib64foo"),
            "rosa branch should be inactive, got {names:?}",
        );
    }
}
