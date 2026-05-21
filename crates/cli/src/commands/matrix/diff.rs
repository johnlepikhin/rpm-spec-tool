//! `matrix diff` — structural diff of effective spec view between
//! two profiles.
//!
//! Answers "what actually changes between profile A and B?" by
//! walking the spec branch-aware per profile and comparing the
//! resulting `BuildRequires` and `Requires` sets. Items live in one
//! of three buckets: present on both, only on A, only on B.
//!
//! Conditional resolution uses the *Skip* indeterminate policy —
//! conservative: items inside indeterminate branches don't appear on
//! either side. This means a diff under-reports changes when the
//! evaluator can't decide a condition; the alternative (Include)
//! would over-report, which we judged worse for PR-review use cases.
//!
//! Exactly two profiles are required (the diff is binary by design).
//! For N-profile equivalence-class grouping see the future
//! `matrix classes` command in `doc/matrix.md`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, ValueEnum};
use rpm_spec::ast::{Span, SpecFile, Tag, TagValue};
use rpm_spec_analyzer::dep_walk::{for_each_dep_atom, render_text_with_macros};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{
    CoverageReport, IndeterminatePolicy, ProfileBranchSelection,
    branch_aware::walk_active_preamble, session::parse,
};
use serde::Serialize;

use super::coverage_style::Style;
use crate::app::ColorChoice;
use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Per-tag grouped report with common / only-A / only-B sections.
    Human,
    /// Structured JSON for tooling consumption.
    Json,
}

#[derive(Debug, Args)]
pub struct DiffOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    /// Exactly two profiles to compare. Comma-separated. The diff
    /// model is binary by design — for N-profile classification use
    /// `matrix classes` (when available).
    #[arg(
        long = "profiles",
        value_name = "A,B",
        value_delimiter = ',',
        required = true
    )]
    pub profiles: Vec<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,

    #[command(flatten)]
    pub bcond: crate::app::BcondOverridesArg,
}

pub(super) fn run(
    opts: DiffOpts,
    config_override: Option<&Path>,
    color: ColorChoice,
) -> Result<ExitCode> {
    if opts.profiles.len() != 2 {
        eprintln!(
            "error: matrix diff requires exactly two profiles (got {})",
            opts.profiles.len()
        );
        return Ok(ExitCode::from(2));
    }
    // Self-diff is degenerate (empty bucket on every dimension) and
    // worse, it collapses the resolver's `targets` vec from 2 to 1
    // member because profile resolution dedups by ID. Reject so the
    // caller sees a clear "use two different profiles" message
    // rather than an internal index-out-of-bounds panic later.
    if opts.profiles[0] == opts.profiles[1] {
        eprintln!(
            "error: matrix diff requires two distinct profiles (got `{}` twice)",
            opts.profiles[0]
        );
        return Ok(ExitCode::from(2));
    }
    let ctx = match super::prepare_matrix(config_override, None, &opts.profiles, &opts.defines) {
        Ok(c) => c,
        Err(e) => return e.into_exit(),
    };
    let resolved = ctx.resolved;

    let sources = io::read_sources(&opts.input.paths)?;
    if sources.len() > 1 {
        eprintln!(
            "error: matrix diff operates on exactly one spec at a time \
             (got {} sources)",
            sources.len()
        );
        return Ok(ExitCode::from(2));
    }
    let source = sources
        .into_iter()
        .next()
        .expect("io::read_sources guarantees >= 1 source");

    let parsed = parse(&source.contents);
    let display_name = source.display_name();
    super::surface_parser_diagnostics(
        super::ParseDiagnosticContext::Diff {
            display_name: &display_name,
        },
        &parsed,
    );

    let coverage = CoverageReport::compute(&parsed.spec, &resolved, &opts.bcond.to_overrides());
    let report = DiffReport::compute(&parsed.spec, &coverage, &resolved);

    match opts.format {
        OutputFormat::Human => {
            let style = Style::new(color);
            render_human(&source, &report, &resolved, &style)?;
        }
        OutputFormat::Json => render_json(&source, &report, &resolved)?,
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// DiffReport
// ---------------------------------------------------------------------------

/// Per-tag diff between profile A and profile B.
#[derive(Debug)]
struct DiffReport {
    profile_a: String,
    profile_b: String,
    groups: Vec<TagDiff>,
    /// Script-bearing section (`%prep`, `%build`, `%install`,
    /// scriptlets, triggers, `%verify`, `%sepolicy`, `%description`,
    /// `%changelog`, `%files`, `%sourcelist`, `%patchlist`) bodies
    /// diffed at the **active-line** level — same per-profile
    /// active-line filter as `matrix impact`. Sections without
    /// divergence don't land here; the renderer can then skip the
    /// block entirely.
    script_sections: Vec<ScriptSectionDiff>,
    /// Subpackage names produced on each profile. Diverges when
    /// `%package -n foo` is wrapped in `%if`-branches that resolve
    /// differently per profile.
    subpackages: SubpackagesDiff,
    /// File-path entries in `%files` sections, per profile.
    /// Diverges when `%files` entries are gated by `%if` (rare in
    /// preamble form but common via `%if`-wrapped path lists).
    files: FilesDiff,
    /// `Source0:` / `PatchN:` counts and entries per profile.
    /// Diverges when sources/patches are conditionally added.
    sources_patches: SourcesPatchesDiff,
    /// Scalar preamble values (`Summary`, `License`, `URL`, `Group`).
    /// Per-tag side-by-side diff: typically identical, sometimes
    /// localised or conditional.
    scalars: Vec<ScalarDiff>,
}

#[derive(Debug)]
struct TagDiff {
    /// Display label — `"BuildRequires"`, `"Requires"`, …
    tag_label: &'static str,
    /// Names present on both profiles (sorted).
    common: Vec<String>,
    /// Names only on profile A (sorted).
    only_a: Vec<String>,
    /// Names only on profile B (sorted).
    only_b: Vec<String>,
}

/// Per-script-section line diff between the two profiles' **active**
/// views of the same section body. Uses the same multiset model as
/// `matrix impact`'s `script_sections`: identical line on both sides
/// counts as `common`, missing-on-A or missing-on-B as `only_*`.
#[derive(Debug)]
struct ScriptSectionDiff {
    /// `"%install"`, `"%post libfoo"`, `"%changelog"`, ...
    label: String,
    common: Vec<String>,
    only_a: Vec<String>,
    only_b: Vec<String>,
}

/// Diff of the subpackage-name sets produced on each profile after
/// branch-aware resolution. Profile-independent specs see an empty
/// diff here (`only_a` / `only_b` empty).
#[derive(Debug, Default)]
struct SubpackagesDiff {
    common: Vec<String>,
    only_a: Vec<String>,
    only_b: Vec<String>,
}

impl SubpackagesDiff {
    fn has_no_movement(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty()
    }
}

/// Diff of file-path entries inside `%files` sections. Set-based:
/// paths repeated in source still count once. Multiple `%files`
/// sub-sections (main + subpackages) are flattened — the set view
/// answers "which paths can THIS profile end up packaging?".
#[derive(Debug, Default)]
struct FilesDiff {
    common: Vec<String>,
    only_a: Vec<String>,
    only_b: Vec<String>,
}

impl FilesDiff {
    fn has_no_movement(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty()
    }
}

/// Diff of source/patch lists. Source numbers (`Source0..N`) and
/// patch numbers (`Patch0..N`) are kept verbatim with their value
/// for stable comparison: `"Source0=foo-1.0.tar.gz"`. Per-profile
/// add/drop of a `Patch:` line surfaces here.
#[derive(Debug, Default)]
struct SourcesPatchesDiff {
    common: Vec<String>,
    only_a: Vec<String>,
    only_b: Vec<String>,
}

impl SourcesPatchesDiff {
    fn has_no_movement(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty()
    }
}

/// Per-tag scalar value diff. Only emitted when the values diverge
/// between A and B (identical values omit the entry entirely — no
/// `common` bucket since scalars are single-valued).
#[derive(Debug)]
struct ScalarDiff {
    tag_label: &'static str,
    value_a: String,
    value_b: String,
}

/// Every dep-bearing preamble tag — re-exported from `analyzer` so
/// `matrix impact` and `matrix diff` stay in lockstep. Order matches
/// `matrix impact`'s `tags` array positionally. See
/// [`rpm_spec_analyzer::COMPARED_TAGS`] for the canonical list and
/// rationale.
use rpm_spec_analyzer::COMPARED_TAGS;

impl DiffReport {
    fn compute(
        spec: &SpecFile<Span>,
        coverage: &CoverageReport,
        target_set: &ResolvedTargetSet,
    ) -> Self {
        // Precondition: `run()` rejects opts.profiles.len() != 2 and
        // identical pairs, so the resolver returns exactly two
        // distinct profiles. Slice pattern surfaces a regression in
        // that contract as a hard error rather than a misleading
        // `targets[1]` panic.
        let [a, b] = target_set.targets.as_slice() else {
            unreachable!(
                "matrix diff invariant: target_set has exactly 2 targets, got {}",
                target_set.targets.len()
            );
        };
        // Conservative diff: Skip indeterminate. False positive on
        // "this is only on A" when the dep is gated by an
        // undecidable %if would be misleading in PR review; better
        // to under-report under uncertainty.
        let sel_a =
            ProfileBranchSelection::compute(coverage, &a.profile_id, IndeterminatePolicy::Skip);
        let sel_b =
            ProfileBranchSelection::compute(coverage, &b.profile_id, IndeterminatePolicy::Skip);

        // Single walk per profile collecting every comparable tag in
        // one pass. Each profile pays O(AST), not O(AST × tags).
        let buckets_a = collect_dep_names_by_tag(spec, &sel_a, COMPARED_TAGS);
        let buckets_b = collect_dep_names_by_tag(spec, &sel_b, COMPARED_TAGS);

        let groups = COMPARED_TAGS
            .iter()
            .enumerate()
            .map(|(i, (_, label))| {
                let set_a = &buckets_a[i];
                let set_b = &buckets_b[i];
                TagDiff {
                    tag_label: label,
                    common: set_a.intersection(set_b).cloned().collect(),
                    only_a: set_a.difference(set_b).cloned().collect(),
                    only_b: set_b.difference(set_a).cloned().collect(),
                }
            })
            .collect();

        // Script-section diff: enumerate every section label that
        // appears in the spec (union of A and B's active views — but
        // since labels don't gate on profile, one pass suffices), then
        // per-label fetch active-line vectors for each profile and
        // multiset-diff them. Sections without divergence are omitted.
        let section_label_map = rpm_spec_analyzer::collect_script_sections(spec);
        let mut script_sections = Vec::new();
        for label in section_label_map.keys() {
            // Strip conditional directives (`%if`/`%elif`/`%else`/
            // `%endif`/`%ifarch`/…) before comparing. The directives
            // exist *physically* on both sides — they're scaffolding
            // that GATES the body, not a build-step. Leaving them in
            // would surface "profile A sees `%if`/`%endif`, profile B
            // doesn't" as divergence — technically true (B's branch
            // is inactive so the directives' range is filtered out)
            // but UX-noise: the operator wants to know WHAT executes
            // differently, not which control flow is reached.
            // `matrix impact` keeps the directives — there an added
            // `%if` block IS the visible change between revisions.
            let lines_a: Vec<String> = rpm_spec_analyzer::collect_active_section_lines(
                spec,
                coverage,
                &a.profile_id,
                label,
            )
            .into_iter()
            .filter(|l| !is_conditional_directive(l))
            .collect();
            let lines_b: Vec<String> = rpm_spec_analyzer::collect_active_section_lines(
                spec,
                coverage,
                &b.profile_id,
                label,
            )
            .into_iter()
            .filter(|l| !is_conditional_directive(l))
            .collect();
            let diff = section_multiset_diff(label.clone(), &lines_a, &lines_b);
            if !diff.only_a.is_empty() || !diff.only_b.is_empty() {
                script_sections.push(diff);
            }
        }

        // Subpackages: branch-aware walk produces the canonical
        // package-name set per profile. Diff via set ops mirrors the
        // dep-tag path. `main_name` resolved from the spec preamble
        // — needed to canonicalise `PackageName::Relative(suffix)`
        // into `<main>-<suffix>`.
        let main_name = main_package_name(spec);
        let subs_a = collect_subpackages(spec, &sel_a, &main_name);
        let subs_b = collect_subpackages(spec, &sel_b, &main_name);
        let subpackages = SubpackagesDiff {
            common: subs_a.intersection(&subs_b).cloned().collect(),
            only_a: subs_a.difference(&subs_b).cloned().collect(),
            only_b: subs_b.difference(&subs_a).cloned().collect(),
        };

        // %files paths — branch-aware. Path set per profile, then
        // standard set-diff. Same `walk_active_sections_in_items`
        // walker as subpackages: surfaces `Section::Files` bodies
        // that survive the profile's branch projection.
        let files_a = collect_files_paths(spec, &sel_a);
        let files_b = collect_files_paths(spec, &sel_b);
        let files = FilesDiff {
            common: files_a.intersection(&files_b).cloned().collect(),
            only_a: files_a.difference(&files_b).cloned().collect(),
            only_b: files_b.difference(&files_a).cloned().collect(),
        };

        // Source/Patch lists: collected from active preamble (a
        // `Source3:` inside an `%if` only counts on profiles where
        // the branch fires). Entries formatted as
        // `"Source3=foo-1.0.tar.gz"` so a renamed source surfaces
        // as one removed + one added rather than silent shift.
        let sp_a = collect_sources_patches(spec, &sel_a);
        let sp_b = collect_sources_patches(spec, &sel_b);
        let sources_patches = SourcesPatchesDiff {
            common: sp_a.intersection(&sp_b).cloned().collect(),
            only_a: sp_a.difference(&sp_b).cloned().collect(),
            only_b: sp_b.difference(&sp_a).cloned().collect(),
        };

        // Scalar preamble: per-tag side-by-side. Only entries where
        // value_a != value_b land in the vec — identical scalars
        // contribute nothing, keeping output focused on divergence.
        let scalars = collect_scalar_diffs(spec, &sel_a, &sel_b);

        Self {
            profile_a: a.profile_id.clone(),
            profile_b: b.profile_id.clone(),
            groups,
            script_sections,
            subpackages,
            files,
            sources_patches,
            scalars,
        }
    }
}

/// Collect every `%files`-entry path that survives the profile's
/// branch projection. Returns paths sorted via `BTreeSet`.
/// Implementation: walk active sections; for each `Section::Files`,
/// extract `FileEntry.path` text from each `FilesContent::Entry`.
fn collect_files_paths(
    spec: &SpecFile<Span>,
    selection: &ProfileBranchSelection,
) -> BTreeSet<String> {
    use rpm_spec::ast::{Section, SpecItem};
    let mut out: BTreeSet<String> = BTreeSet::new();
    walk_active_sections_in_items(&spec.items, selection, &mut |item| {
        if let SpecItem::Section(boxed) = item
            && let Section::Files { content, .. } = boxed.as_ref()
        {
            collect_files_from_content(content, selection, &mut out);
        }
    });
    out
}

fn collect_files_from_content(
    content: &[rpm_spec::ast::FilesContent<Span>],
    selection: &ProfileBranchSelection,
    out: &mut BTreeSet<String>,
) {
    use rpm_spec::ast::FilesContent;
    use rpm_spec_analyzer::SelectedBody;
    for fc in content {
        match fc {
            FilesContent::Entry(e) => {
                if let Some(path) = &e.path {
                    let s = rpm_spec_analyzer::render_text_with_macros(&path.path);
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        out.insert(trimmed.to_string());
                    }
                }
            }
            FilesContent::Conditional(c) => {
                // Recurse only into the branch the profile activates,
                // matching the dep walker's policy. The `selected()`
                // map keys on the conditional's start line.
                let Some(picked) = selection.selected(c.data.start_line) else {
                    continue;
                };
                let body: Option<&[FilesContent<Span>]> = match picked {
                    SelectedBody::Branch(i) => c.branches.get(i).map(|b| b.body.as_slice()),
                    SelectedBody::Otherwise => c.otherwise.as_deref(),
                    _ => None,
                };
                if let Some(body) = body {
                    collect_files_from_content(body, selection, out);
                }
            }
            _ => {}
        }
    }
}

/// Collect `Source<N>:` and `Patch<N>:` entries that survive the
/// profile's branch projection. Each entry rendered as
/// `"Source3=foo-1.0.tar.gz"` so a rename surfaces as one removed +
/// one added across the profile pair.
fn collect_sources_patches(
    spec: &SpecFile<Span>,
    selection: &ProfileBranchSelection,
) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    rpm_spec_analyzer::walk_active_preamble(spec, selection, |item| {
        let kind = match item.tag {
            Tag::Source(idx) => format!("Source{}", idx.unwrap_or(0)),
            Tag::Patch(idx) => format!("Patch{}", idx.unwrap_or(0)),
            _ => return,
        };
        if let TagValue::Text(t) = &item.value {
            let rendered = rpm_spec_analyzer::render_text_with_macros(t);
            let trimmed = rendered.trim();
            if !trimmed.is_empty() {
                out.insert(format!("{kind}={trimmed}"));
            }
        }
    });
    out
}

/// Side-by-side diff of single-value preamble tags. Identical
/// (`value_a == value_b`) entries are omitted entirely; a tag absent
/// on one side appears with the literal placeholder `"(absent)"`
/// so renderers can distinguish "no tag" from "empty value".
fn collect_scalar_diffs(
    spec: &SpecFile<Span>,
    sel_a: &ProfileBranchSelection,
    sel_b: &ProfileBranchSelection,
) -> Vec<ScalarDiff> {
    /// Scalar tags interesting for cross-profile review — single-
    /// valued, not dep-bearing. `Group`/`License`/`URL` are most
    /// likely to be the same across profiles but rarely differ via
    /// `%if`; `Summary` can be localised (e.g. `Summary(ru):`).
    const SCALAR_TAGS: &[(Tag, &str)] = &[
        (Tag::Summary, "Summary"),
        (Tag::License, "License"),
        (Tag::URL, "URL"),
        (Tag::Group, "Group"),
    ];

    let mut values_a: std::collections::HashMap<&'static str, String> =
        std::collections::HashMap::new();
    let mut values_b: std::collections::HashMap<&'static str, String> =
        std::collections::HashMap::new();

    let collect_into =
        |sel: &ProfileBranchSelection,
         out: &mut std::collections::HashMap<&'static str, String>| {
            rpm_spec_analyzer::walk_active_preamble(spec, sel, |item| {
                for (tag, label) in SCALAR_TAGS {
                    if &item.tag == tag
                        && let TagValue::Text(t) = &item.value
                    {
                        let rendered = rpm_spec_analyzer::render_text_with_macros(t);
                        // First-write-wins: subpackage preamble can
                        // declare the same tag again with a different
                        // value, but the top-level scalar is what we
                        // want for cross-profile comparison.
                        out.entry(label).or_insert(rendered);
                    }
                }
            });
        };
    collect_into(sel_a, &mut values_a);
    collect_into(sel_b, &mut values_b);

    let mut out: Vec<ScalarDiff> = Vec::new();
    for (_, label) in SCALAR_TAGS {
        let va = values_a.get(label).cloned();
        let vb = values_b.get(label).cloned();
        if va == vb {
            continue;
        }
        out.push(ScalarDiff {
            tag_label: label,
            value_a: va.unwrap_or_else(|| "(absent)".to_string()),
            value_b: vb.unwrap_or_else(|| "(absent)".to_string()),
        });
    }
    out
}

/// `true` when the line is a conditional directive (`%if`,
/// `%elif`, `%else`, `%endif`, `%ifarch`, `%ifnarch`, `%ifos`,
/// `%ifnos`, `%elifarch`, `%elifos`) rather than a body statement.
/// Used by `matrix diff` to strip control-flow scaffolding from
/// per-profile script-section comparisons — directives gate the body
/// they wrap, they don't represent build steps that "run differently"
/// across profiles.
fn is_conditional_directive(line: &str) -> bool {
    let trimmed = line.trim_start();
    // Match longest prefix first so `%elifarch` doesn't false-match
    // `%elif`, etc. A trailing alphanumeric character means this is
    // a longer keyword we don't want to swallow (e.g. `%ifoobar`).
    for kw in [
        "%elifarch",
        "%elifos",
        "%ifarch",
        "%ifnarch",
        "%ifnos",
        "%ifos",
        "%endif",
        "%elif",
        "%else",
        "%if",
    ] {
        if let Some(rest) = trimmed.strip_prefix(kw) {
            let next = rest.chars().next();
            if !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == '_') {
                return true;
            }
        }
    }
    false
}

/// Multiset diff for script-section lines. Mirrors the algorithm in
/// `matrix impact`'s `multiset_diff`, but the output shape matches
/// the diff's `TagDiff` triple (common / only_a / only_b) rather than
/// impact's counts.
fn section_multiset_diff(label: String, a: &[String], b: &[String]) -> ScriptSectionDiff {
    use std::collections::HashMap;
    let mut counts_a: HashMap<&str, i64> = HashMap::new();
    for line in a {
        *counts_a.entry(line.as_str()).or_insert(0) += 1;
    }
    let mut counts_b: HashMap<&str, i64> = HashMap::new();
    for line in b {
        *counts_b.entry(line.as_str()).or_insert(0) += 1;
    }
    let mut common: Vec<String> = Vec::new();
    let mut only_a: Vec<String> = Vec::new();
    let mut only_b: Vec<String> = Vec::new();
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    keys.extend(counts_a.keys().copied());
    keys.extend(counts_b.keys().copied());
    for k in keys {
        let f = counts_a.get(k).copied().unwrap_or(0);
        let t = counts_b.get(k).copied().unwrap_or(0);
        let shared = f.min(t);
        for _ in 0..shared {
            common.push(k.to_string());
        }
        let delta = t - f;
        if delta > 0 {
            for _ in 0..delta {
                only_b.push(k.to_string());
            }
        } else if delta < 0 {
            for _ in 0..(-delta) {
                only_a.push(k.to_string());
            }
        }
    }
    ScriptSectionDiff {
        label,
        common,
        only_a,
        only_b,
    }
}

/// Resolve the spec's main package name from the preamble. Falls
/// back to `"(unknown)"` if no `Name:` tag is present — the
/// subpackage canonicalisation gracefully degrades to `(unknown)-foo`
/// which is still useful for relative comparison between profiles.
fn main_package_name(spec: &SpecFile<Span>) -> String {
    for item in &spec.items {
        if let rpm_spec::ast::SpecItem::Preamble(p) = item
            && matches!(p.tag, Tag::Name)
            && let TagValue::Text(t) = &p.value
        {
            let rendered = rpm_spec_analyzer::render_text_with_macros(t);
            let trimmed = rendered.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "(unknown)".to_string()
}

/// Walk active sections of `spec` on the profile and collect every
/// `%package` declaration's canonical name. `Relative(suffix)` →
/// `<main>-<suffix>`; `Absolute(name)` → `name` as-is.
fn collect_subpackages(
    spec: &SpecFile<Span>,
    selection: &ProfileBranchSelection,
    main_name: &str,
) -> BTreeSet<String> {
    use rpm_spec::ast::{PackageName, Section, SpecItem};
    let mut out: BTreeSet<String> = BTreeSet::new();
    walk_active_sections_in_items(&spec.items, selection, &mut |item| {
        if let SpecItem::Section(boxed) = item
            && let Section::Package { name_arg, .. } = boxed.as_ref()
        {
            let name = match name_arg {
                PackageName::Relative(t) => {
                    let suffix = rpm_spec_analyzer::render_text_with_macros(t);
                    format!("{}-{}", main_name, suffix.trim())
                }
                PackageName::Absolute(t) => rpm_spec_analyzer::render_text_with_macros(t)
                    .trim()
                    .to_string(),
                _ => return,
            };
            if !name.is_empty() {
                out.insert(name);
            }
        }
    });
    out
}

/// Walk top-level items, recursing through active conditional
/// branches per `selection`. Invokes `f` on every active SpecItem.
/// Mirrors the active-walker pattern in `analyzer::branch_aware`
/// (`pick_body_spec_item`) but inlined here since the matrix-diff
/// caller doesn't need preamble or files specialisation.
fn walk_active_sections_in_items<'ast, F>(
    items: &'ast [rpm_spec::ast::SpecItem<Span>],
    selection: &ProfileBranchSelection,
    f: &mut F,
) where
    F: FnMut(&'ast rpm_spec::ast::SpecItem<Span>),
{
    use rpm_spec::ast::SpecItem;
    use rpm_spec_analyzer::SelectedBody;
    for item in items {
        match item {
            SpecItem::Conditional(c) => {
                // Use the per-line selection map to pick which body
                // (if any) the profile activates. Inactive ⇒ no
                // entry, no walk. `c.data.start_line` joins onto the
                // CoverageReport's `start_line` keys.
                let Some(picked) = selection.selected(c.data.start_line) else {
                    continue;
                };
                let body: Option<&[SpecItem<Span>]> = match picked {
                    SelectedBody::Branch(i) => c.branches.get(i).map(|b| b.body.as_slice()),
                    SelectedBody::Otherwise => c.otherwise.as_deref(),
                    _ => None,
                };
                if let Some(body) = body {
                    walk_active_sections_in_items(body, selection, f);
                }
            }
            other => f(other),
        }
    }
}

/// Collect dep-name buckets for every tag in `COMPARED_TAGS` in a
/// single AST walk. The previous (one-walk-per-tag) approach paid
/// O(tags × ast) per profile; this is O(ast) and remains tags-agnostic
/// — adding a `Tag::Provides` entry to `COMPARED_TAGS` requires no
/// changes here.
fn collect_dep_names_by_tag(
    spec: &SpecFile<Span>,
    selection: &ProfileBranchSelection,
    tags: &[(Tag, &'static str)],
) -> Vec<BTreeSet<String>> {
    let mut buckets: Vec<BTreeSet<String>> = (0..tags.len()).map(|_| BTreeSet::new()).collect();
    walk_active_preamble(spec, selection, |item| {
        // `Tag` derives `PartialEq` and `==` works regardless of
        // future `#[non_exhaustive]` additions: an unknown tag simply
        // matches no entry in COMPARED_TAGS and the item is skipped.
        if let Some(idx) = tags.iter().position(|(t, _)| t == &item.tag)
            && let TagValue::Dep(dep) = &item.value
        {
            // Shared walker keeps the And/Or flatten + If/Unless/Not
            // skip policy aligned with contract verification.
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

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn render_human(
    source: &io::Source,
    report: &DiffReport,
    _target_set: &ResolvedTargetSet,
    style: &Style,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        style.header(&format!(
            "# Matrix diff: {} vs {}",
            report.profile_a, report.profile_b,
        ))
    )?;
    writeln!(
        out,
        "{}",
        style.header(&format!("## {}", source.display_name()))
    )?;

    // Default policy: print ONLY groups with actual divergence
    // between profiles — i.e. at least one of `only_a` / `only_b`
    // non-empty. Common-only groups are silenced because the
    // operator running `diff` cares "what's different?", not
    // "what's the same?". When no group has any divergence we still
    // emit a confirming "(no differences …)" footer below so the
    // operator can tell "empty diff" apart from "renderer crashed".
    let mut any_printed = false;
    for g in &report.groups {
        if g.only_a.is_empty() && g.only_b.is_empty() {
            continue;
        }
        any_printed = true;
        writeln!(out)?;
        writeln!(out, "  {}", style.header(g.tag_label))?;
        // `common` painted dim (no movement); `only A`/`only B` get
        // the green/red accent so the eye lands on diverged buckets.
        write_bucket(&mut out, "    common", &g.common, |s| style.dim(s), style)?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_a),
            &g.only_a,
            |s| style.always_tag(s),
            style,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_b),
            &g.only_b,
            |s| style.dead_tag(s),
            style,
        )?;
    }
    // Script-section diff: per-section line-level multiset diff,
    // active-line filtered per profile. Only sections with movement
    // land in `report.script_sections`, so the "movement-only"
    // policy is applied implicitly at compute-time (no further
    // filtering here).
    if !report.script_sections.is_empty() {
        any_printed = true;
        writeln!(out)?;
        writeln!(out, "  {}", style.header("Script sections"))?;
        for sec in &report.script_sections {
            writeln!(out, "    {}", style.header(&sec.label))?;
            write_bucket(
                &mut out,
                "      common",
                &sec.common,
                |s| style.dim(s),
                style,
            )?;
            write_bucket(
                &mut out,
                &format!("      only {}", report.profile_a),
                &sec.only_a,
                |s| style.always_tag(s),
                style,
            )?;
            write_bucket(
                &mut out,
                &format!("      only {}", report.profile_b),
                &sec.only_b,
                |s| style.dead_tag(s),
                style,
            )?;
        }
    }

    // Subpackages: print only when there's actual divergence between
    // profiles. Common-only suppressed (matches the dep-tag policy
    // above — operator running `diff` cares about deltas).
    let subs = &report.subpackages;
    if !subs.has_no_movement() {
        any_printed = true;
        writeln!(out)?;
        writeln!(out, "  {}", style.header("Subpackages"))?;
        write_bucket(
            &mut out,
            "    common",
            &subs.common,
            |s| style.dim(s),
            style,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_a),
            &subs.only_a,
            |s| style.always_tag(s),
            style,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_b),
            &subs.only_b,
            |s| style.dead_tag(s),
            style,
        )?;
    }

    // Files: only diverging path entries surface; common-only is
    // silenced (same policy as dep tags above).
    let files = &report.files;
    if !files.has_no_movement() {
        any_printed = true;
        writeln!(out)?;
        writeln!(out, "  {}", style.header("Files"))?;
        write_bucket(
            &mut out,
            "    common",
            &files.common,
            |s| style.dim(s),
            style,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_a),
            &files.only_a,
            |s| style.always_tag(s),
            style,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_b),
            &files.only_b,
            |s| style.dead_tag(s),
            style,
        )?;
    }

    // Sources/Patches: same policy — print only on divergence.
    let sp = &report.sources_patches;
    if !sp.has_no_movement() {
        any_printed = true;
        writeln!(out)?;
        writeln!(out, "  {}", style.header("Sources/Patches"))?;
        write_bucket(&mut out, "    common", &sp.common, |s| style.dim(s), style)?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_a),
            &sp.only_a,
            |s| style.always_tag(s),
            style,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_b),
            &sp.only_b,
            |s| style.dead_tag(s),
            style,
        )?;
    }

    // Scalar tags: only entries already filtered to "value_a !=
    // value_b" land here; absent ⇔ no movement. By construction the
    // vec is already movement-only.
    if !report.scalars.is_empty() {
        any_printed = true;
        writeln!(out)?;
        writeln!(out, "  {}", style.header("Scalar tags"))?;
        for s in &report.scalars {
            writeln!(out, "    {}", style.header(s.tag_label))?;
            writeln!(
                out,
                "      {}: {}",
                style.always_tag(&format!("on {}", report.profile_a)),
                s.value_a
            )?;
            writeln!(
                out,
                "      {}: {}",
                style.dead_tag(&format!("on {}", report.profile_b)),
                s.value_b
            )?;
        }
    }

    if !any_printed {
        writeln!(out)?;
        writeln!(
            out,
            "  {}",
            style.dim("(no differences across the compared tags)")
        )?;
    }
    Ok(())
}

/// Paint a single bucket header + entries. `label_paint` styles the
/// `"only A"` / `"only B"` / `"common"` accent — different colours
/// per bucket help the eye triangulate which side a name lives on.
/// Entries themselves stay in default colour to keep readability of
/// long dep lists.
///
/// **Empty buckets are silently skipped.** Showing `only X (0): (none)`
/// for every bucket added 3× line noise per group; the group-level
/// "completely empty" filter handles the all-zero case, while non-zero
/// buckets benefit from the cleaner shape.
fn write_bucket<W, F>(
    out: &mut W,
    label: &str,
    items: &[String],
    label_paint: F,
    _style: &Style,
) -> std::io::Result<()>
where
    W: std::io::Write,
    F: Fn(&str) -> String,
{
    if items.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "{} ({}): {}",
        label_paint(label),
        items.len(),
        items.join(", ")
    )?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct DiffJson<'a> {
    profile_a: &'a str,
    profile_b: &'a str,
    path: String,
    groups: Vec<TagDiffJson<'a>>,
    /// Script-bearing sections whose active-line view differs between
    /// the two profiles. Empty when all sections agree on this profile
    /// pair; omitted from the wire format (`skip_serializing_if`) to
    /// keep older consumers unchanged when no script divergence exists.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    script_sections: Vec<ScriptSectionDiffJson<'a>>,
    /// Subpackage names per profile after branch resolution. Always
    /// present (even with empty buckets) so the JSON schema is stable
    /// across runs; consumers can `len()`-check the buckets if they
    /// only care about divergence.
    subpackages: SubpackagesDiffJson<'a>,
    /// File-path entries per profile. Always present.
    files: FilesDiffJson<'a>,
    /// `Source<N>:` / `Patch<N>:` entries per profile. Always present.
    sources_patches: SourcesPatchesDiffJson<'a>,
    /// Scalar tag values that diverge between profiles. Empty when
    /// identical; omitted from the wire format on no-divergence.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    scalars: Vec<ScalarDiffJson<'a>>,
}

#[derive(Debug, Serialize)]
struct TagDiffJson<'a> {
    tag: &'static str,
    common: &'a [String],
    only_a: &'a [String],
    only_b: &'a [String],
}

#[derive(Debug, Serialize)]
struct ScriptSectionDiffJson<'a> {
    /// `"%install"`, `"%post libfoo"`, etc.
    label: &'a str,
    common: &'a [String],
    only_a: &'a [String],
    only_b: &'a [String],
}

#[derive(Debug, Serialize)]
struct SubpackagesDiffJson<'a> {
    common: &'a [String],
    only_a: &'a [String],
    only_b: &'a [String],
}

#[derive(Debug, Serialize)]
struct FilesDiffJson<'a> {
    common: &'a [String],
    only_a: &'a [String],
    only_b: &'a [String],
}

#[derive(Debug, Serialize)]
struct SourcesPatchesDiffJson<'a> {
    common: &'a [String],
    only_a: &'a [String],
    only_b: &'a [String],
}

#[derive(Debug, Serialize)]
struct ScalarDiffJson<'a> {
    tag: &'static str,
    value_a: &'a str,
    value_b: &'a str,
}

fn render_json(
    source: &io::Source,
    report: &DiffReport,
    _target_set: &ResolvedTargetSet,
) -> Result<()> {
    let payload = DiffJson {
        profile_a: report.profile_a.as_str(),
        profile_b: report.profile_b.as_str(),
        path: source.display_name().to_string(),
        groups: report
            .groups
            .iter()
            .map(|g| TagDiffJson {
                tag: g.tag_label,
                common: &g.common,
                only_a: &g.only_a,
                only_b: &g.only_b,
            })
            .collect(),
        script_sections: report
            .script_sections
            .iter()
            .map(|s| ScriptSectionDiffJson {
                label: s.label.as_str(),
                common: &s.common,
                only_a: &s.only_a,
                only_b: &s.only_b,
            })
            .collect(),
        subpackages: SubpackagesDiffJson {
            common: &report.subpackages.common,
            only_a: &report.subpackages.only_a,
            only_b: &report.subpackages.only_b,
        },
        files: FilesDiffJson {
            common: &report.files.common,
            only_a: &report.files.only_a,
            only_b: &report.files.only_b,
        },
        sources_patches: SourcesPatchesDiffJson {
            common: &report.sources_patches.common,
            only_a: &report.sources_patches.only_a,
            only_b: &report.sources_patches.only_b,
        },
        scalars: report
            .scalars
            .iter()
            .map(|s| ScalarDiffJson {
                tag: s.tag_label,
                value_a: s.value_a.as_str(),
                value_b: s.value_b.as_str(),
            })
            .collect(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests for tricky private helpers.
//
// These cover string-level logic that's hard to exercise end-to-end
// through the CLI integration tests (`is_conditional_directive`,
// `section_multiset_diff`) plus the two small AST-walking collectors
// for which mistakes silently degrade the human/JSON output rather
// than fail loudly.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_conditional_directive ------------------------------------------

    #[test]
    fn cond_directive_recognises_canonical_keywords() {
        for kw in [
            "%if",
            "%elif",
            "%else",
            "%endif",
            "%ifarch",
            "%ifnarch",
            "%ifos",
            "%ifnos",
            "%elifarch",
            "%elifos",
        ] {
            assert!(
                is_conditional_directive(kw),
                "bare keyword `{kw}` must classify as conditional directive"
            );
        }
    }

    #[test]
    fn cond_directive_recognises_keyword_with_args() {
        // Real-world spec form: `%if 0%{?rhel}`, `%ifarch x86_64`, etc.
        // Whitespace after the keyword must not block the match.
        assert!(is_conditional_directive("%if 0%{?rhel}"));
        assert!(is_conditional_directive("%elif 0%{?fedora}"));
        assert!(is_conditional_directive("%ifarch x86_64 aarch64"));
        assert!(is_conditional_directive("%ifnarch ppc64le"));
        assert!(is_conditional_directive("%elifarch s390x"));
    }

    #[test]
    fn cond_directive_strips_leading_whitespace() {
        assert!(is_conditional_directive("  %if 1"));
        assert!(is_conditional_directive("\t%endif"));
        assert!(is_conditional_directive("    %else"));
    }

    #[test]
    fn cond_directive_rejects_alnum_suffix() {
        // `%ifoobar` / `%ifoo` / `%elseif` are NOT real conditional
        // directives — the longest-prefix match must reject them so
        // we don't strip unrelated macro invocations.
        assert!(!is_conditional_directive("%ifoobar"));
        assert!(!is_conditional_directive("%ifoo"));
        assert!(!is_conditional_directive("%endifx"));
        assert!(!is_conditional_directive("%elifx"));
        assert!(!is_conditional_directive("%elsewhere"));
        // Underscore suffix is treated the same as alphanumeric — a
        // macro starting with `%if_` is still a separate keyword.
        assert!(!is_conditional_directive("%if_with_args"));
    }

    #[test]
    fn cond_directive_rejects_dep_lines_and_other_macros() {
        // Plain preamble / body lines — must never be misclassified
        // as a control-flow directive.
        assert!(!is_conditional_directive(
            "Source0: %{name}-%{version}.tar.gz"
        ));
        assert!(!is_conditional_directive("BuildRequires: gcc"));
        assert!(!is_conditional_directive("%global foo 1"));
        assert!(!is_conditional_directive("%define bar 2"));
        assert!(!is_conditional_directive("make %{?_smp_mflags}"));
        // Comment line that happens to mention `%if` in its text —
        // anchor must remain at the line start (after trim_start).
        assert!(!is_conditional_directive("# %if comment"));
    }

    #[test]
    fn cond_directive_handles_empty_and_blank_lines() {
        assert!(!is_conditional_directive(""));
        assert!(!is_conditional_directive("    "));
        assert!(!is_conditional_directive("\t"));
    }

    // --- section_multiset_diff ---------------------------------------------

    #[test]
    fn multiset_diff_identical_bodies_collapse_to_common() {
        let a = vec!["make".to_string(), "make install".to_string()];
        let b = vec!["make".to_string(), "make install".to_string()];
        let d = section_multiset_diff("%build".to_string(), &a, &b);
        assert_eq!(d.label, "%build");
        assert!(d.only_a.is_empty());
        assert!(d.only_b.is_empty());
        // Order of `common` is BTreeSet-key-sorted under the hood.
        let mut got = d.common.clone();
        got.sort();
        let mut want = vec!["make".to_string(), "make install".to_string()];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn multiset_diff_extra_repeated_line_goes_to_only_side() {
        // A has `make` twice; B has `make` once → the extra one lands
        // in `only_a` (since A has the higher count), the shared one
        // in `common`. `make install` is unique to B.
        let a = vec!["make".to_string(), "make".to_string()];
        let b = vec!["make".to_string(), "make install".to_string()];
        let d = section_multiset_diff("%build".to_string(), &a, &b);
        assert_eq!(d.common, vec!["make".to_string()]);
        assert_eq!(d.only_a, vec!["make".to_string()]);
        assert_eq!(d.only_b, vec!["make install".to_string()]);
    }

    #[test]
    fn multiset_diff_disjoint_sides_have_empty_common() {
        let a = vec!["./configure".to_string()];
        let b = vec!["cmake .".to_string()];
        let d = section_multiset_diff("%build".to_string(), &a, &b);
        assert!(d.common.is_empty());
        assert_eq!(d.only_a, vec!["./configure".to_string()]);
        assert_eq!(d.only_b, vec!["cmake .".to_string()]);
    }

    #[test]
    fn multiset_diff_both_empty() {
        let d = section_multiset_diff("%build".to_string(), &[], &[]);
        assert!(d.common.is_empty());
        assert!(d.only_a.is_empty());
        assert!(d.only_b.is_empty());
    }

    // --- collect_subpackages / collect_sources_patches ---------------------

    /// Build a `ProfileBranchSelection` for a spec with NO conditional
    /// blocks — every active item survives the trivial projection.
    /// Lets us unit-test the AST walkers in isolation from the
    /// profile-resolution stack.
    fn unconditional_selection(
        spec: &SpecFile<Span>,
    ) -> (ResolvedTargetSet, ProfileBranchSelection) {
        use rpm_spec_analyzer::profile::{
            ProfileSection, ResolveOptions, TargetEntry, resolve_target_set,
        };
        let section = ProfileSection::new(None, std::collections::BTreeMap::new());
        let target = TargetEntry::from_profiles(vec!["generic".to_string()]);
        let target_set = resolve_target_set(
            &section,
            "test",
            &target,
            std::path::Path::new("/tmp"),
            ResolveOptions::default(),
        )
        .expect("resolve generic profile");
        let coverage = CoverageReport::compute(
            spec,
            &target_set,
            &rpm_spec_analyzer::BcondOverrides::default(),
        );
        let sel = ProfileBranchSelection::compute(&coverage, "generic", IndeterminatePolicy::Skip);
        (target_set, sel)
    }

    #[test]
    fn collect_subpackages_relative_and_absolute() {
        // `%package devel`        → Relative("devel")  ⇒ "foo-devel"
        // `%package -n libfoo`    → Absolute("libfoo") ⇒ "libfoo"
        // `%package -n %{name}-x` → Absolute, macro renders to "foo-x"
        const SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT

%description
m

%package devel
Summary: d
%description devel
d

%package -n libfoo
Summary: lf
%description -n libfoo
lf

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let parsed = parse(SPEC);
        let (_ts, sel) = unconditional_selection(&parsed.spec);
        let main = main_package_name(&parsed.spec);
        assert_eq!(main, "foo");
        let names = collect_subpackages(&parsed.spec, &sel, &main);
        assert!(
            names.contains("foo-devel"),
            "relative subpackage must canonicalise to `<main>-<suffix>`; got {names:?}"
        );
        assert!(
            names.contains("libfoo"),
            "absolute subpackage must keep its given name; got {names:?}"
        );
        // The main package itself is NOT a `%package` declaration —
        // must not appear in the subpackages set.
        assert!(
            !names.contains("foo"),
            "main package must not appear in the subpackages set; got {names:?}"
        );
    }

    #[test]
    fn collect_sources_patches_numbered_entries() {
        const SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT
Source0: foo-1.0.tar.gz
Source1: foo-extra.tar.gz
Patch2: backport.patch

%description
m

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let parsed = parse(SPEC);
        let (_ts, sel) = unconditional_selection(&parsed.spec);
        let set = collect_sources_patches(&parsed.spec, &sel);
        assert!(
            set.contains("Source0=foo-1.0.tar.gz"),
            "Source0 must surface with index + value; got {set:?}"
        );
        assert!(
            set.contains("Source1=foo-extra.tar.gz"),
            "Source1 must surface with its own index; got {set:?}"
        );
        assert!(
            set.contains("Patch2=backport.patch"),
            "Patch2 must surface with its given index; got {set:?}"
        );
    }
}
