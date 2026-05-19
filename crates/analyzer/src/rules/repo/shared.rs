//! Helpers shared between the RPM-REPO-* rules.
//!
//! Extracted so the three top-level rule files stay short and read
//! identically (each one is essentially "collect deps; for each dep,
//! call `lookup`; emit if `outcome == X`"). Macro expansion via
//! `MacroRegistry::expand_to_literal` lives here too — used by every
//! rule that needs to handle `BuildRequires: %{python3_pkgversion}-foo`.

use std::fmt::Write as _;
use std::sync::Arc;

use rpm_spec::ast::{
    BoolDep, Conditional, DepAtom, DepExpr, PreambleContent, PreambleItem, Section, Span, SpecFile,
    SpecItem, Tag, TagValue, VerOp,
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
const MACRO_EXPAND_DEPTH: u8 = 8;

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
pub(super) struct ProjectedDep {
    pub capability: Capability,
    pub display: String,
    pub span: Span,
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
pub(super) fn project_deps<F>(
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
    let all_preceding_false = true;
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) if all_preceding_false => {
                collect_active_spec_items(&branch.body, profile, bcond, out);
                return;
            }
            Ok(true) => {
                // A preceding branch was indeterminate; we can't
                // prove this branch fires (the preceding ones might
                // have been the active arm). Bail out — neither this
                // branch nor `%else` can be safely included.
                return;
            }
            Ok(false) => continue,
            Err(_) => {
                // Indeterminate condition: skip this branch AND
                // bail on the rest (we can't prove subsequent
                // guards' antecedents, and `%else` requires every
                // preceding to be decisively false).
                return;
            }
        }
    }
    if all_preceding_false
        && let Some(els) = &cond.otherwise
    {
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
    let all_preceding_false = true;
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) if all_preceding_false => {
                collect_active_preamble_contents(&branch.body, profile, bcond, out);
                return;
            }
            Ok(true) => return,
            Ok(false) => continue,
            Err(_) => return,
        }
    }
    if all_preceding_false
        && let Some(els) = &cond.otherwise
    {
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
fn format_capability_display(
    name: &str,
    arch: Option<&str>,
    op: CapFlags,
    evr: Option<&EVR>,
) -> String {
    let mut display = String::from(name);
    if let Some(arch) = arch {
        display.push('(');
        display.push_str(arch);
        display.push(')');
    }
    if let Some(evr) = evr {
        display.push(' ');
        display.push_str(match op {
            CapFlags::LT => "<",
            CapFlags::LE => "<=",
            CapFlags::EQ => "=",
            CapFlags::GE => ">=",
            CapFlags::GT => ">",
            _ => "?",
        });
        display.push(' ');
        if evr.epoch > 0 {
            // `write!` into the existing String avoids the
            // intermediate allocation `push_str(&format!(...))`
            // would create. Infallible on a String sink.
            let _ = write!(&mut display, "{}:", evr.epoch);
        }
        display.push_str(&evr.version);
        if !evr.release.is_empty() {
            display.push('-');
            display.push_str(&evr.release);
        }
    }
    display
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

