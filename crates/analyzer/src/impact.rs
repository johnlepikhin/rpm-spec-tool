//! Per-profile impact of a spec change between two revisions.
//!
//! Phase 13 closes the diff/expand/classes/impact quartet. Where
//! `matrix diff` compares effective spec views between two
//! **profiles** at one revision, `matrix impact` compares effective
//! spec views of one spec at two **revisions**, per profile. The PR
//! review use case: "this commit touches `foo.spec` — which profiles
//! are materially affected and which deps moved?".
//!
//! Mechanics:
//!
//! 1. Caller fetches `from_spec` and `to_spec` bytes (typically via
//!    `git show REV:path`; the CLI helper lives in
//!    `crates/cli/src/commands/matrix/impact.rs::git_show`).
//! 2. [`ImpactReport::compute`] parses both, computes per-profile
//!    [`ProfileSignature`](crate::ProfileSignature)s on each side,
//!    and reports the symmetric set diff per (profile, tag) pair.
//! 3. Output lists `added` / `removed` / `unchanged` dep names for
//!    every profile and every compared tag.
//!
//! Branch-aware via [`IndeterminatePolicy::Skip`] for symmetry with
//! `matrix diff`. Bcond overrides flow through unchanged.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rpm_spec::ast::{
    BuildScriptKind, ChangelogEntry, FileEntry, ScriptletKind, Section, ShellBody, Span, SpecFile,
    SpecItem, SubpkgRef, Tag, TagValue, TextBody,
};
use rpm_spec_profile::ResolvedTargetSet;
use serde::Serialize;

use crate::bcond::BcondOverrides;
use crate::branch_aware::{IndeterminatePolicy, ProfileBranchSelection, walk_active_preamble};
use crate::branch_coverage::CoverageReport;
use crate::dep_walk::{for_each_dep_atom, render_text_with_macros};

/// Tags the impact report folds over — same set as `matrix diff`'s
/// `COMPARED_TAGS` and `matrix classes`, keeping the semantic
/// invariant that two profiles equivalent under `matrix classes` see
/// no `matrix diff` deltas AND no `matrix impact` deltas at the same
/// revision pair. The list mirrors all 11 dep-bearing preamble tags
/// in [`rpm_spec::ast::Tag`] (`Requires`, `BuildRequires`, `Provides`,
/// `Conflicts`, `Obsoletes`, `Recommends`, `Suggests`, `Supplements`,
/// `Enhances`, `BuildConflicts`, `OrderWithRequires`).
///
/// Public so external consumers can correlate a [`TagImpact::tag_label`]
/// string back to its [`Tag`] enum value (the JSON wire shape uses the
/// label only). Order matches `ProfileImpact::tags` positionally and
/// `matrix diff`'s tag order — extending here must be mirrored in
/// `matrix diff`'s `COMPARED_TAGS` to preserve the invariant above.
pub const COMPARED_TAGS: &[(Tag, &str)] = &[
    (Tag::BuildRequires, "BuildRequires"),
    (Tag::Requires, "Requires"),
    (Tag::Provides, "Provides"),
    (Tag::Conflicts, "Conflicts"),
    (Tag::Obsoletes, "Obsoletes"),
    (Tag::Recommends, "Recommends"),
    (Tag::Suggests, "Suggests"),
    (Tag::Supplements, "Supplements"),
    (Tag::Enhances, "Enhances"),
    (Tag::BuildConflicts, "BuildConflicts"),
    (Tag::OrderWithRequires, "OrderWithRequires"),
];

/// Set-diff between two dep buckets for one (profile, tag) pair.
///
/// Buckets are sorted alphabetically for stable output: a renamed
/// dep surfaces as one `removed` + one `added`, not as a churn.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ChangeSet {
    /// Deps present at `to` but not at `from`.
    pub added: Vec<String>,
    /// Deps present at `from` but not at `to`.
    pub removed: Vec<String>,
    /// Deps present on both sides — surfaced so operators can see
    /// "this profile has 12 BR, 2 new and 1 dropped, 9 unchanged"
    /// without re-computing the spec.
    pub unchanged: Vec<String>,
}

impl ChangeSet {
    /// `true` iff this changeset records no movement on either side.
    ///
    /// Intentionally ignores [`Self::unchanged`] — a profile with 12
    /// unchanged deps and 0 added/removed has had no impact even
    /// though `unchanged` is non-empty. The doc-comment is the
    /// contract; the underlying behaviour is pinned by
    /// `has_no_movement_ignores_unchanged` in the unit tests.
    #[must_use]
    pub fn has_no_movement(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Per-(profile, tag) entry in the impact report.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TagImpact {
    /// `"BuildRequires"`, `"Requires"`, …
    pub tag_label: &'static str,
    pub changes: ChangeSet,
}

/// Per-profile entry in the impact report.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ProfileImpact {
    pub profile_id: String,
    /// One entry per tag in [`COMPARED_TAGS`], in the same order.
    pub tags: Vec<TagImpact>,
    /// Script-bearing sections (`%prep`, `%build`, `%install`,
    /// `%check`, scriptlets, triggers, `%verify`, `%sepolicy`,
    /// `%description`, `%changelog`, `%files`, `%sourcelist`,
    /// `%patchlist`) whose body moved between `from` and `to`
    /// **as observed on this profile**. Shell-body changes inside an
    /// inactive `%if` branch on the profile don't appear here — the
    /// same edit may show up on a different profile where the branch
    /// is active. Text-bodied sections (changelog, files, …) have no
    /// conditional gating, so identical entries appear under every
    /// profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_sections: Vec<ScriptSectionChange>,
}

impl ProfileImpact {
    /// `true` iff every tag's changeset **and** every script-section's
    /// per-profile delta is empty for this profile. CLI summaries
    /// highlight profiles where this is `false` — those are the
    /// platforms a PR actually moved.
    #[must_use]
    pub fn is_no_change(&self) -> bool {
        self.tags.iter().all(|t| t.changes.has_no_movement()) && self.script_sections.is_empty()
    }
}

/// One script-bearing section (`%prep`, `%build`, `%install`,
/// `%check`, `%clean`, `%conf`, `%generate_buildrequires`, the
/// scriptlets, triggers, `%verify`, `%sepolicy`) whose body shifted
/// between `from` and `to`. Reported at the report level rather than
/// per-profile because script-section content doesn't gate on
/// per-profile macros the way preamble deps do (the same body runs on
/// every profile that builds the package). Per-profile precision is a
/// future refinement; the current `added` / `removed` counts already
/// answer the primary PR-review question — "did this PR touch build
/// behaviour, and where?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ScriptSectionChange {
    /// Section label as written in source: `"%install"`, `"%post"`,
    /// `"%post libfoo"`, `"%post -n libfoo"`, `"%triggerin"`,
    /// `"%filetriggerin"`, `"%verify"`, `"%sepolicy"`. Subpackage
    /// modifiers are included so multi-scriptlet specs distinguish
    /// the main package's `%post` from a subpackage's.
    pub label: String,
    /// Lines present in `to` but not in `from`, **filtered to lines
    /// active on the enclosing profile**. A change inside an `%if`
    /// branch that's inactive on the profile contributes zero.
    pub added: usize,
    /// Lines present in `from` but not in `to`, profile-filtered.
    pub removed: usize,
    /// Lines on both sides (intersection of the profile-filtered
    /// multisets). Surfaced for ratio context — a `+1 -1 (12
    /// unchanged)` line reads differently than `+1 -1 (0 unchanged)`.
    pub unchanged: usize,
}

impl ScriptSectionChange {
    /// `true` iff this entry records no actual movement.
    /// Invariant: only entries with movement land in
    /// [`ImpactReport::script_sections`] — this method is provided
    /// for downstream filtering / sanity checks.
    #[must_use]
    pub fn has_no_movement(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

/// Output of [`ImpactReport::compute`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ImpactReport {
    /// One row per profile in target-set declaration order, so
    /// renderers preserve column alignment. Each row carries the
    /// dep-tag movement *and* the script-section movement filtered
    /// to lines active on that profile — see
    /// [`ProfileImpact::script_sections`].
    pub per_profile: Vec<ProfileImpact>,
}

impl ImpactReport {
    /// Compute the impact of changing `from_spec` to `to_spec` for
    /// every profile in `target_set`.
    ///
    /// `bcond_overrides` flows through to both `CoverageReport`
    /// computations so the same `--with`/`--without` policy applies
    /// on both sides — otherwise the diff would mix "spec changed"
    /// with "CLI flag changed", which is exactly the noise the
    /// command is meant to eliminate.
    #[must_use]
    pub fn compute(
        from_spec: &SpecFile<Span>,
        to_spec: &SpecFile<Span>,
        target_set: &ResolvedTargetSet,
        bcond_overrides: &BcondOverrides,
    ) -> Self {
        // One CoverageReport per side, shared across profiles. The
        // same selection-policy choice as `matrix diff`: Skip
        // indeterminate so a branch we can't statically resolve
        // contributes nothing to either side — under uncertainty,
        // err toward "no impact reported" rather than spurious
        // adds/removes.
        let from_cov = CoverageReport::compute(from_spec, target_set, bcond_overrides);
        let to_cov = CoverageReport::compute(to_spec, target_set, bcond_overrides);

        // Pre-collect the global section labels once so the per-
        // profile loop doesn't re-walk both specs for every target.
        // The label set is small (typically <20 entries), so a
        // BTreeSet over &String keeps both keys and iteration order
        // stable.
        let from_sections = collect_script_sections(from_spec);
        let to_sections = collect_script_sections(to_spec);
        let mut section_labels: BTreeSet<&String> = BTreeSet::new();
        section_labels.extend(from_sections.keys());
        section_labels.extend(to_sections.keys());

        let per_profile = target_set
            .targets
            .iter()
            .map(|rt| {
                let from_sel = ProfileBranchSelection::compute(
                    &from_cov,
                    &rt.profile_id,
                    IndeterminatePolicy::Skip,
                );
                let to_sel = ProfileBranchSelection::compute(
                    &to_cov,
                    &rt.profile_id,
                    IndeterminatePolicy::Skip,
                );
                let from_buckets = collect_dep_names(from_spec, &from_sel);
                let to_buckets = collect_dep_names(to_spec, &to_sel);
                let tags = COMPARED_TAGS
                    .iter()
                    .enumerate()
                    .map(|(i, (_, label))| {
                        let f = &from_buckets[i];
                        let t = &to_buckets[i];
                        let added: Vec<String> = t.difference(f).cloned().collect();
                        let removed: Vec<String> = f.difference(t).cloned().collect();
                        let unchanged: Vec<String> = f.intersection(t).cloned().collect();
                        TagImpact {
                            tag_label: label,
                            changes: ChangeSet {
                                added,
                                removed,
                                unchanged,
                            },
                        }
                    })
                    .collect();
                // Per-profile script-section diff: active-line filter
                // each side then multiset_diff. Only sections with
                // movement on THIS profile land in the result —
                // changes gated by inactive conditionals on this
                // profile don't pollute the output.
                let mut script_sections = Vec::new();
                for label in &section_labels {
                    let from_active =
                        collect_active_section_lines(from_spec, &from_cov, &rt.profile_id, label);
                    let to_active =
                        collect_active_section_lines(to_spec, &to_cov, &rt.profile_id, label);
                    let change = multiset_diff((*label).clone(), &from_active, &to_active);
                    if !change.has_no_movement() {
                        script_sections.push(change);
                    }
                }
                ProfileImpact {
                    profile_id: rt.profile_id.clone(),
                    tags,
                    script_sections,
                }
            })
            .collect();

        Self { per_profile }
    }

    /// Number of profiles where the impact is non-empty. CLI
    /// summaries lead with this number — "PR affects 3 of 12
    /// profiles" is the most useful one-line headline.
    #[must_use]
    pub fn affected_profile_count(&self) -> usize {
        self.per_profile
            .iter()
            .filter(|p| !p.is_no_change())
            .count()
    }

    /// `true` iff every profile's changeset is empty across every
    /// tag **and** every script section. The PR materially changed
    /// nothing the analyzer can see (under the Skip indeterminate
    /// policy).
    #[must_use]
    pub fn is_no_change(&self) -> bool {
        self.per_profile.iter().all(ProfileImpact::is_no_change)
    }
}

/// Walk `spec` once collecting branch-projected dep sets per tag in
/// `COMPARED_TAGS`. Shared with `matrix diff` in spirit; isolated
/// here to avoid a dependency from the `diff` CLI module into the
/// `impact` module.
fn collect_dep_names(
    spec: &SpecFile<Span>,
    selection: &ProfileBranchSelection,
) -> Vec<BTreeSet<String>> {
    let mut buckets: Vec<BTreeSet<String>> =
        COMPARED_TAGS.iter().map(|_| BTreeSet::new()).collect();
    walk_active_preamble(spec, selection, |item| {
        if let Some(idx) = COMPARED_TAGS.iter().position(|(t, _)| t == &item.tag)
            && let TagValue::Dep(dep) = &item.value
        {
            for_each_dep_atom(dep, |name| {
                let rendered = render_text_with_macros(name);
                let trimmed = rendered.trim();
                if !trimmed.is_empty() {
                    buckets[idx].insert(trimmed.to_string());
                }
            });
        }
    });
    buckets
}

// =====================================================================
// Script-section body diff
// =====================================================================

/// Render a [`ShellBody`]'s lines as a `Vec<String>` for multiset diff.
/// Macro references inside lines are kept verbatim — the operator's
/// PR is about source-level changes, not about post-expansion content.
fn render_shell_body_lines(body: &ShellBody<Span>) -> Vec<String> {
    body.lines.iter().map(render_text_with_macros).collect()
}

/// Build the canonical label for a scriptlet/trigger/verify/sepolicy
/// section, including its subpackage modifier if present. Multi-
/// scriptlet specs (e.g. `%post mainpkg` + `%post libfoo`) must
/// distinguish bodies so a change in one doesn't bleed into the other.
fn label_with_subpkg(kind: &str, subpkg: Option<&SubpkgRef>) -> String {
    match subpkg {
        None => kind.to_string(),
        Some(SubpkgRef::Relative(t)) => {
            // `%post foo` — bare suffix.
            let name = render_text_with_macros(t);
            format!("{kind} {name}")
        }
        Some(SubpkgRef::Absolute(t)) => {
            // `%post -n libfoo` — absolute name.
            let name = render_text_with_macros(t);
            format!("{kind} -n {name}")
        }
        // `SubpkgRef` is `#[non_exhaustive]` upstream; fall back to
        // a kind-only label so a new variant doesn't break the build.
        Some(_) => kind.to_string(),
    }
}

fn buildscript_kind_label(k: BuildScriptKind) -> &'static str {
    match k {
        BuildScriptKind::Prep => "%prep",
        BuildScriptKind::Conf => "%conf",
        BuildScriptKind::Build => "%build",
        BuildScriptKind::Install => "%install",
        BuildScriptKind::Check => "%check",
        BuildScriptKind::Clean => "%clean",
        BuildScriptKind::GenerateBuildRequires => "%generate_buildrequires",
        // `BuildScriptKind` is `#[non_exhaustive]` upstream.
        _ => "%buildscript",
    }
}

fn scriptlet_kind_label(k: ScriptletKind) -> &'static str {
    match k {
        ScriptletKind::Pre => "%pre",
        ScriptletKind::Post => "%post",
        ScriptletKind::Preun => "%preun",
        ScriptletKind::Postun => "%postun",
        ScriptletKind::Pretrans => "%pretrans",
        ScriptletKind::Posttrans => "%posttrans",
        ScriptletKind::Preuntrans => "%preuntrans",
        ScriptletKind::Postuntrans => "%postuntrans",
        // `ScriptletKind` is `#[non_exhaustive]` upstream; fall back to
        // a generic label so a new variant doesn't break the analyzer.
        _ => "%scriptlet",
    }
}

/// Walk every script-bearing section in `spec` and return a
/// `label → rendered lines` map. Recurses into top-level `%if`
/// branches so a section guarded by `%if cond` is still inspected —
/// otherwise a PR that moves a `%post` into / out of a conditional
/// would surface as no-change. Exposed (`pub`) so `matrix diff` can
/// enumerate the same section label universe `matrix impact` uses,
/// and call [`collect_active_section_lines`] per label per profile.
pub fn collect_script_sections(spec: &SpecFile<Span>) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_from_items(&spec.items, &mut out, 0);
    out
}

fn collect_from_items(
    items: &[SpecItem<Span>],
    out: &mut BTreeMap<String, Vec<String>>,
    depth: u32,
) {
    // Guard against pathological nesting; mirrors `shell::walk::MAX_DEPTH`.
    if depth > 128 {
        return;
    }
    for item in items {
        match item {
            SpecItem::Section(boxed) => collect_from_section(boxed.as_ref(), out),
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    collect_from_items(&branch.body, out, depth + 1);
                }
                if let Some(els) = &c.otherwise {
                    collect_from_items(els, out, depth + 1);
                }
            }
            _ => {}
        }
    }
}

fn collect_from_section(section: &Section<Span>, out: &mut BTreeMap<String, Vec<String>>) {
    match section {
        Section::BuildScript { kind, body, .. } => {
            out.entry(buildscript_kind_label(*kind).to_string())
                .or_default()
                .extend(render_shell_body_lines(body));
        }
        Section::Scriptlet(s) => {
            let label = label_with_subpkg(scriptlet_kind_label(s.kind), s.subpkg.as_ref());
            out.entry(label)
                .or_default()
                .extend(render_shell_body_lines(&s.body));
        }
        Section::Trigger(t) => {
            // Triggers have a kind enum + a body; for the label drop
            // the conditions list (which is structured deps, not text)
            // and just use the kind name. Multiple triggers of the
            // same kind merge — uncommon, acceptable for v1.
            out.entry(format!("%trigger{:?}", t.kind).to_lowercase())
                .or_default()
                .extend(render_shell_body_lines(&t.body));
        }
        Section::FileTrigger(t) => {
            out.entry(format!("%filetrigger{:?}", t.kind).to_lowercase())
                .or_default()
                .extend(render_shell_body_lines(&t.body));
        }
        Section::Verify { subpkg, body, .. } => {
            let label = label_with_subpkg("%verify", subpkg.as_ref());
            out.entry(label)
                .or_default()
                .extend(render_shell_body_lines(body));
        }
        Section::Sepolicy { subpkg, body, .. } => {
            let label = label_with_subpkg("%sepolicy", subpkg.as_ref());
            out.entry(label)
                .or_default()
                .extend(render_shell_body_lines(body));
        }
        Section::Description { subpkg, body, .. } => {
            // Description bodies are text — every line counts for
            // the operator (wording changes affect package metadata
            // surfaced to end users by `rpm -qi`).
            let label = label_with_subpkg("%description", subpkg.as_ref());
            out.entry(label)
                .or_default()
                .extend(render_text_body_lines(body));
        }
        Section::Changelog { entries, .. } => {
            // Changelog: most PRs add a new entry, so this section
            // will almost always show movement. That's accurate
            // signal — a PR without a changelog entry IS unusual.
            out.entry("%changelog".to_string())
                .or_default()
                .extend(render_changelog_entries(entries));
        }
        Section::Files {
            subpkg,
            file_lists,
            content,
            ..
        } => {
            // `%files` is part of the packaging boundary — added or
            // removed paths are first-class PR signal. Render each
            // file entry's path text (entries with no path, e.g.
            // bare `%defattr(...)`, contribute nothing).
            let label = label_with_subpkg("%files", subpkg.as_ref());
            let mut lines: Vec<String> = file_lists.iter().map(render_text_with_macros).collect();
            for c in content {
                if let rpm_spec::ast::FilesContent::Entry(e) = c
                    && let Some(line) = render_file_entry_path(e)
                {
                    lines.push(line);
                }
            }
            out.entry(label).or_default().extend(lines);
        }
        Section::SourceList { entries, .. } => {
            // `%sourcelist` and `%patchlist` are flat text — diff the
            // entries to surface added/removed sources at a glance.
            out.entry("%sourcelist".to_string())
                .or_default()
                .extend(entries.iter().map(render_text_with_macros));
        }
        Section::PatchList { entries, .. } => {
            out.entry("%patchlist".to_string())
                .or_default()
                .extend(entries.iter().map(render_text_with_macros));
        }
        _ => {}
    }
}

/// Render a [`TextBody`]'s lines as `Vec<String>` for multiset diff.
fn render_text_body_lines(body: &TextBody) -> Vec<String> {
    body.lines.iter().map(render_text_with_macros).collect()
}

/// Flatten a `%changelog` entry into a list of `Vec<String>` lines:
/// `* Mon Jan 01 2026 author <email> - version` header plus each
/// body line. Header is rendered as one line; body lines as-is.
/// Multiset diff on this representation surfaces "new entry added"
/// (entry's lines all in `added`) and "version bumped" (header line
/// added + old header removed).
fn render_changelog_entries<T>(entries: &[ChangelogEntry<T>]) -> Vec<String> {
    let mut out = Vec::with_capacity(entries.len() * 4);
    for e in entries {
        let mut header = String::new();
        header.push_str("* ");
        header.push_str(&format!(
            "{:?} {:?} {:02} {}",
            e.date.weekday, e.date.month, e.date.day, e.date.year
        ));
        header.push(' ');
        header.push_str(&render_text_with_macros(&e.author));
        if let Some(email) = &e.email {
            header.push_str(" <");
            header.push_str(&render_text_with_macros(email));
            header.push('>');
        }
        if let Some(version) = &e.version {
            header.push_str(" - ");
            header.push_str(&render_text_with_macros(version));
        }
        out.push(header);
        out.extend(e.body.iter().map(render_text_with_macros));
    }
    out
}

/// Render a `%files` entry's path text. Returns `None` for entries
/// that have no path (e.g. bare `%defattr(...)`) so they don't
/// contribute noise to the diff. Directives without paths still
/// matter (a `%defattr` change is a real semantic change), but their
/// shape is structured and warrants a dedicated diff later — for now
/// the path-based view catches the high-signal majority.
fn render_file_entry_path<T>(e: &FileEntry<T>) -> Option<String> {
    let path = e.path.as_ref()?;
    let text = render_text_with_macros(&path.path);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// For one script section identified by `label`, return the rendered
/// lines **active on `profile_id`** — i.e. with lines inside inactive
/// [`rpm_spec::ast::ShellConditional`] branches filtered out. For
/// non-shell sections the active filter degenerates to the identity
/// function — those bodies have no conditional gating. Used by
/// `matrix diff` to compare two profiles' active views of the same
/// section, and internally by `matrix impact` for the per-profile
/// `script_sections` diff.
pub fn collect_active_section_lines(
    spec: &SpecFile<Span>,
    coverage: &CoverageReport,
    profile_id: &str,
    label: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_active_from_items(&spec.items, coverage, profile_id, label, &mut out, 0);
    out
}

fn collect_active_from_items(
    items: &[SpecItem<Span>],
    coverage: &CoverageReport,
    profile_id: &str,
    label: &str,
    out: &mut Vec<String>,
    depth: u32,
) {
    if depth > 128 {
        return;
    }
    for item in items {
        match item {
            SpecItem::Section(boxed) => {
                collect_active_from_section(boxed.as_ref(), coverage, profile_id, label, out);
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    collect_active_from_items(
                        &branch.body,
                        coverage,
                        profile_id,
                        label,
                        out,
                        depth + 1,
                    );
                }
                if let Some(els) = &c.otherwise {
                    collect_active_from_items(els, coverage, profile_id, label, out, depth + 1);
                }
            }
            _ => {}
        }
    }
}

fn collect_active_from_section(
    section: &Section<Span>,
    coverage: &CoverageReport,
    profile_id: &str,
    label: &str,
    out: &mut Vec<String>,
) {
    match section {
        Section::BuildScript { kind, body, data } if buildscript_kind_label(*kind) == label => {
            out.extend(active_shell_lines(body, *data, coverage, profile_id));
        }
        Section::Scriptlet(s) => {
            let l = label_with_subpkg(scriptlet_kind_label(s.kind), s.subpkg.as_ref());
            if l == label {
                out.extend(active_shell_lines(&s.body, s.data, coverage, profile_id));
            }
        }
        Section::Trigger(t) => {
            let l = format!("%trigger{:?}", t.kind).to_lowercase();
            if l == label {
                out.extend(active_shell_lines(&t.body, t.data, coverage, profile_id));
            }
        }
        Section::FileTrigger(t) => {
            let l = format!("%filetrigger{:?}", t.kind).to_lowercase();
            if l == label {
                out.extend(active_shell_lines(&t.body, t.data, coverage, profile_id));
            }
        }
        Section::Verify { subpkg, body, data } => {
            let l = label_with_subpkg("%verify", subpkg.as_ref());
            if l == label {
                out.extend(active_shell_lines(body, *data, coverage, profile_id));
            }
        }
        Section::Sepolicy { subpkg, body, data } => {
            let l = label_with_subpkg("%sepolicy", subpkg.as_ref());
            if l == label {
                out.extend(active_shell_lines(body, *data, coverage, profile_id));
            }
        }
        // Text-bodied sections (no conditional gating). Reuse the
        // unfiltered renderers from the global walk.
        Section::Description { subpkg, body, .. } => {
            let l = label_with_subpkg("%description", subpkg.as_ref());
            if l == label {
                out.extend(render_text_body_lines(body));
            }
        }
        Section::Changelog { entries, .. } if label == "%changelog" => {
            out.extend(render_changelog_entries(entries));
        }
        Section::Files {
            subpkg,
            file_lists,
            content,
            ..
        } => {
            let l = label_with_subpkg("%files", subpkg.as_ref());
            if l == label {
                out.extend(file_lists.iter().map(render_text_with_macros));
                for c in content {
                    if let rpm_spec::ast::FilesContent::Entry(e) = c
                        && let Some(line) = render_file_entry_path(e)
                    {
                        out.push(line);
                    }
                }
            }
        }
        Section::SourceList { entries, .. } if label == "%sourcelist" => {
            out.extend(entries.iter().map(render_text_with_macros));
        }
        Section::PatchList { entries, .. } if label == "%patchlist" => {
            out.extend(entries.iter().map(render_text_with_macros));
        }
        _ => {}
    }
}

/// Filter a [`ShellBody`]'s lines to those *active* on `profile_id`:
/// drop lines that fall inside any [`rpm_spec::ast::ShellConditional`]
/// branch (or `%else`) the coverage report classifies as `Inactive`
/// on the profile. Source-line numbers are derived from `section_span`
/// (`start_line + 1 + line_index`) — the parser pushes one physical
/// line per `body.lines` entry, so this mapping is exact.
fn active_shell_lines(
    body: &rpm_spec::ast::ShellBody<Span>,
    section_span: Span,
    coverage: &CoverageReport,
    profile_id: &str,
) -> Vec<String> {
    if body.conditionals.is_empty() {
        return render_shell_body_lines(body);
    }
    // Source-line ranges (inclusive) that are inactive on this profile.
    let mut inactive_ranges: Vec<(u32, u32)> = Vec::new();
    for cond in &body.conditionals {
        // Per-branch coverage lookup.
        let statuses: Vec<BranchVerdict> = cond
            .branches
            .iter()
            .map(|b| lookup_branch_verdict(coverage, b.data, profile_id))
            .collect();
        for (i, branch) in cond.branches.iter().enumerate() {
            if matches!(statuses[i], BranchVerdict::Inactive) {
                inactive_ranges.push((branch.data.start_line, branch.data.end_line));
            }
        }
        // `%else` activity is the complement of its siblings:
        // any sibling Active → else Inactive.
        if let Some(els) = &cond.otherwise {
            let else_inactive = statuses.iter().any(|s| matches!(s, BranchVerdict::Active));
            if else_inactive {
                inactive_ranges.push((els.data.start_line, els.data.end_line));
            }
        }
    }

    let section_start = section_span.start_line;
    body.lines
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            // body.lines[0] is on source-line (section_start + 1).
            let source_line = section_start.saturating_add(1).saturating_add(*i as u32);
            !inactive_ranges
                .iter()
                .any(|(start, end)| source_line >= *start && source_line <= *end)
        })
        .map(|(_, line)| render_text_with_macros(line))
        .collect()
}

/// Per-profile verdict on one conditional branch — collapsed view of
/// the [`crate::BranchCoverage`] active/inactive/indeterminate sets.
enum BranchVerdict {
    Active,
    Inactive,
    Indeterminate,
}

/// Look up a [`rpm_spec::ast::ShellCondBranch`]'s coverage verdict on
/// `profile_id`. ShellConditional branches were folded into
/// [`CoverageReport`] by `ConditionalCollector::record_shell`, so
/// lookup by start_byte matches both top-level and shell-body
/// conditionals uniformly.
fn lookup_branch_verdict(
    coverage: &CoverageReport,
    branch_span: Span,
    profile_id: &str,
) -> BranchVerdict {
    for entry in &coverage.conditionals {
        for bc in &entry.branches {
            if bc.branch.span.start_byte == branch_span.start_byte {
                if bc.active_on.iter().any(|p| p == profile_id) {
                    return BranchVerdict::Active;
                }
                if bc.inactive_on.iter().any(|p| p == profile_id) {
                    return BranchVerdict::Inactive;
                }
                return BranchVerdict::Indeterminate;
            }
        }
    }
    BranchVerdict::Indeterminate
}

fn multiset_diff(label: String, from: &[String], to: &[String]) -> ScriptSectionChange {
    let mut from_counts: HashMap<&str, i64> = HashMap::new();
    for line in from {
        *from_counts.entry(line.as_str()).or_insert(0) += 1;
    }
    let mut to_counts: HashMap<&str, i64> = HashMap::new();
    for line in to {
        *to_counts.entry(line.as_str()).or_insert(0) += 1;
    }
    let mut added: usize = 0;
    let mut removed: usize = 0;
    let mut unchanged: usize = 0;
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    keys.extend(from_counts.keys().copied());
    keys.extend(to_counts.keys().copied());
    for k in keys {
        let f = from_counts.get(k).copied().unwrap_or(0);
        let t = to_counts.get(k).copied().unwrap_or(0);
        let delta = t - f;
        if delta > 0 {
            added += delta as usize;
        } else if delta < 0 {
            removed += (-delta) as usize;
        }
        unchanged += f.min(t) as usize;
    }
    ScriptSectionChange {
        label,
        added,
        removed,
        unchanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

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

    fn report(from_src: &str, to_src: &str, profiles: &[&str]) -> ImpactReport {
        let from_parsed = parse(from_src);
        let to_parsed = parse(to_src);
        let ts = target_set_with(profiles);
        ImpactReport::compute(
            &from_parsed.spec,
            &to_parsed.spec,
            &ts,
            &BcondOverrides::default(),
        )
    }

    const BASE_SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn no_change_when_specs_identical() {
        let r = report(BASE_SPEC, BASE_SPEC, &["rhel-9-x86_64"]);
        assert!(r.is_no_change());
        assert_eq!(r.affected_profile_count(), 0);
        // All deps in "unchanged" bucket.
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.tag_label, "BuildRequires");
        assert!(br.changes.added.is_empty());
        assert!(br.changes.removed.is_empty());
        assert_eq!(br.changes.unchanged, vec!["gcc", "make"]);
    }

    #[test]
    fn has_no_movement_ignores_unchanged() {
        // Pins the doc contract: a ChangeSet with N unchanged deps but
        // 0 added/removed reports `has_no_movement == true`. Without
        // this test a well-meaning "is the changeset empty?" rename
        // would silently flip CI semantics (every PR that touches a
        // spec with stable deps would suddenly look "changed").
        let cs = ChangeSet {
            added: Vec::new(),
            removed: Vec::new(),
            unchanged: vec!["gcc".to_string(), "make".to_string()],
        };
        assert!(
            cs.has_no_movement(),
            "unchanged-only set must not register as movement"
        );
    }

    #[test]
    fn added_dep_surfaces_per_profile() {
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make
BuildRequires: cmake

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(BASE_SPEC, TO, &["rhel-9-x86_64"]);
        assert!(!r.is_no_change());
        assert_eq!(r.affected_profile_count(), 1);
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.changes.added, vec!["cmake"]);
        assert!(br.changes.removed.is_empty());
    }

    #[test]
    fn removed_dep_surfaces_per_profile() {
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(BASE_SPEC, TO, &["rhel-9-x86_64"]);
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.changes.removed, vec!["make"]);
        assert!(br.changes.added.is_empty());
    }

    #[test]
    fn rhel_only_branch_split_affects_only_rhel() {
        // Add a RHEL-gated BR in the new revision. Affects only
        // rhel-* profiles; altlinux sees no change.
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make

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
        let r = report(BASE_SPEC, TO, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        // rhel-9-x86_64: rhel-only added.
        let rhel = r
            .per_profile
            .iter()
            .find(|p| p.profile_id == "rhel-9-x86_64")
            .expect("rhel profile present");
        assert!(!rhel.is_no_change());
        assert!(
            rhel.tags[0]
                .changes
                .added
                .contains(&"rhel-only".to_string())
        );
        // altlinux-10-x86_64: unaffected (the new BR is gated away).
        let alt = r
            .per_profile
            .iter()
            .find(|p| p.profile_id == "altlinux-10-x86_64")
            .expect("alt profile present");
        assert!(alt.is_no_change(), "alt must be unaffected; got {alt:?}");
        // Reported count is 1 of 2 profiles affected.
        assert_eq!(r.affected_profile_count(), 1);
    }

    #[test]
    fn empty_from_spec_reports_all_as_added() {
        // Edge: from-side is a spec that declares no deps (e.g.
        // the file was just added). Everything in to-spec is
        // "added" relative to a clean slate.
        const EMPTY: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(EMPTY, BASE_SPEC, &["rhel-9-x86_64"]);
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.changes.added, vec!["gcc", "make"]);
        assert!(br.changes.removed.is_empty());
        assert!(br.changes.unchanged.is_empty());
    }

    #[test]
    fn affected_profile_count_excludes_unchanged_profiles() {
        // 3 profiles, only 1 has any change.
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make
%if 0%{?suse_version}
BuildRequires: suse-only
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(
            BASE_SPEC,
            TO,
            &["rhel-9-x86_64", "altlinux-10-x86_64", "sles-15-x86_64"],
        );
        // Only sles-15-x86_64 should see suse-only added.
        assert_eq!(r.affected_profile_count(), 1);
    }

    #[test]
    fn collect_script_sections_emits_known_labels() {
        // Verify the public `collect_script_sections` walker returns
        // labels for every script-bearing section it encounters. The
        // exact key set is the section-label universe `matrix diff`
        // and `matrix impact` operate on.
        const SRC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT

%description
B

%install
install -m 0755 foo %{buildroot}/usr/bin/foo

%post
echo posted

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let parsed = parse(SRC);
        let sections = collect_script_sections(&parsed.spec);
        assert!(
            sections.contains_key("%install"),
            "expected %install label; got keys {:?}",
            sections.keys().collect::<Vec<_>>()
        );
        assert!(
            sections.contains_key("%post"),
            "expected %post label; got keys {:?}",
            sections.keys().collect::<Vec<_>>()
        );
        assert!(
            sections.contains_key("%files"),
            "expected %files label; got keys {:?}",
            sections.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn collect_active_section_lines_filters_inactive_branch() {
        // A `%build` section gated by `%if 0` keeps the else-branch
        // active on every profile. `collect_active_section_lines`
        // must return only the active branch's lines.
        const SRC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT

%description
B

%build
%if 0
echo inactive_line
%else
echo active_line
%endif

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let parsed = parse(SRC);
        let ts = target_set_with(&["rhel-9-x86_64"]);
        let cov = CoverageReport::compute(&parsed.spec, &ts, &BcondOverrides::default());
        let lines = collect_active_section_lines(&parsed.spec, &cov, "rhel-9-x86_64", "%build");
        let joined = lines.join("\n");
        assert!(
            joined.contains("active_line"),
            "expected active branch present; got: {joined:?}"
        );
        assert!(
            !joined.contains("inactive_line"),
            "expected inactive branch filtered out; got: {joined:?}"
        );
    }

    #[test]
    fn multiset_diff_counts_repeated_lines() {
        // Two identical "echo a" lines on the `from` side, one
        // identical line shared with `to`, plus one only-on-`a` line:
        // `removed == 2` (one extra echo a + one only_a), `added == 1`
        // (new only_b), `unchanged == 1` (the shared echo a).
        let from = vec![
            "echo a".to_string(),
            "echo a".to_string(),
            "only_a".to_string(),
        ];
        let to = vec!["echo a".to_string(), "only_b".to_string()];
        let change = multiset_diff("%build".to_string(), &from, &to);
        assert_eq!(change.label, "%build");
        assert_eq!(change.added, 1, "added: {change:?}");
        assert_eq!(change.removed, 2, "removed: {change:?}");
        assert_eq!(change.unchanged, 1, "unchanged: {change:?}");
        assert!(!change.has_no_movement());
    }

    #[test]
    fn impact_report_compute_includes_provides_diff() {
        // The extended COMPARED_TAGS list (Phase 25) now covers
        // Provides. Two specs differing only in their `Provides:` tag
        // must surface a non-empty ChangeSet under the Provides
        // TagImpact for the targeted profile.
        const FROM: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
Provides: foo-old

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
Provides: foo-new

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(FROM, TO, &["rhel-9-x86_64"]);
        let provides = r.per_profile[0]
            .tags
            .iter()
            .find(|t| t.tag_label == "Provides")
            .expect("Provides tag present in COMPARED_TAGS");
        assert_eq!(
            provides.changes.added,
            vec!["foo-new".to_string()],
            "added: {:?}",
            provides.changes
        );
        assert_eq!(
            provides.changes.removed,
            vec!["foo-old".to_string()],
            "removed: {:?}",
            provides.changes
        );
        assert!(!r.is_no_change());
    }
}
