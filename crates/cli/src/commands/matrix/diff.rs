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
    CoverageReport, IndeterminatePolicy, ProfileBranchSelection, branch_aware::walk_active_preamble,
    session::parse,
};
use serde::Serialize;

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
    #[arg(long = "profiles", value_name = "A,B", value_delimiter = ',', required = true)]
    pub profiles: Vec<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,

    #[command(flatten)]
    pub bcond: crate::app::BcondOverridesArg,
}

pub(super) fn run(opts: DiffOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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
    let ctx = match super::prepare_matrix(
        config_override,
        None,
        &opts.profiles,
        &opts.defines,
    ) {
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

    let coverage = CoverageReport::compute(
        &parsed.spec,
        &resolved,
        &opts.bcond.to_overrides(),
    );
    let report = DiffReport::compute(&parsed.spec, &coverage, &resolved);

    match opts.format {
        OutputFormat::Human => render_human(&source, &report, &resolved)?,
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

/// Tags compared by `matrix diff`. Restricted to deps that materially
/// affect "what is installed on the build host" (`BuildRequires`) or
/// "what is required at install time" (`Requires`). Other dep-bearing
/// tags (`Provides`, `Conflicts`, `Obsoletes`, …) are interesting too
/// but deferred — adding them later is additive on the JSON shape.
const COMPARED_TAGS: &[(Tag, &str)] = &[
    (Tag::BuildRequires, "BuildRequires"),
    (Tag::Requires, "Requires"),
];

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
        let sel_a = ProfileBranchSelection::compute(coverage, &a.profile_id, IndeterminatePolicy::Skip);
        let sel_b = ProfileBranchSelection::compute(coverage, &b.profile_id, IndeterminatePolicy::Skip);

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
        Self {
            profile_a: a.profile_id.clone(),
            profile_b: b.profile_id.clone(),
            groups,
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
        if let Some(idx) = tags.iter().position(|(t, _)| t == &item.tag) {
            if let TagValue::Dep(dep) = &item.value {
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
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix diff: {} vs {}",
        report.profile_a, report.profile_b
    )?;
    writeln!(out, "## {}", source.display_name())?;

    for g in &report.groups {
        writeln!(out)?;
        writeln!(out, "  {}", g.tag_label)?;
        write_bucket(&mut out, "    common", &g.common)?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_a),
            &g.only_a,
        )?;
        write_bucket(
            &mut out,
            &format!("    only {}", report.profile_b),
            &g.only_b,
        )?;
    }
    Ok(())
}

fn write_bucket<W: std::io::Write>(out: &mut W, label: &str, items: &[String]) -> std::io::Result<()> {
    if items.is_empty() {
        writeln!(out, "{label} (0): (none)")?;
    } else {
        writeln!(out, "{label} ({}): {}", items.len(), items.join(", "))?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct DiffJson<'a> {
    profile_a: &'a str,
    profile_b: &'a str,
    path: String,
    groups: Vec<TagDiffJson<'a>>,
}

#[derive(Debug, Serialize)]
struct TagDiffJson<'a> {
    tag: &'static str,
    common: &'a [String],
    only_a: &'a [String],
    only_b: &'a [String],
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
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}
