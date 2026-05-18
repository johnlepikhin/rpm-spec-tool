//! `matrix coverage` — show which `%if`/`%ifarch` branches activate
//! on which profiles.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{
    BranchCoverage, CoverageReport, EvalError, EvalErrorCategory, session::parse,
};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Per-spec table grouped by branch.
    Human,
    /// Structured JSON for tooling consumption.
    Json,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive — same
/// contract as the other `matrix` subcommands.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct CoverageOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    /// Exit with code 1 when at least one branch matches the given
    /// status. `dead` covers branches no profile activates;
    /// `indeterminate` covers branches the evaluator couldn't
    /// resolve on any profile. `none` (default) — informational only.
    #[arg(long = "fail-on", default_value_t = FailOn::None, value_enum)]
    pub fail_on: FailOn,

    /// Restrict the per-branch listing to one verdict class. The
    /// summary header still reports the full count for every class;
    /// only the detailed branch lines are filtered. `noisy` is the
    /// most common triage view — everything except universally-active
    /// branches, since those add no signal.
    #[arg(long = "only", value_enum)]
    pub only: Option<OnlyFilter>,

    /// Print only the summary header (counts + reason rollup) and
    /// skip the per-branch listing entirely. Use for a quick "is
    /// this spec healthy?" check or for CI logs that don't need the
    /// full body. Combine with `--format json` to get a stable
    /// machine-readable summary.
    #[arg(long = "summary", default_value_t = false)]
    pub summary: bool,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,

    #[command(flatten)]
    pub bcond: crate::app::BcondOverridesArg,
}

/// Per-branch filter selector for `--only`. The four direct
/// classifications mirror the renderer's tag set; `noisy` is the
/// shorthand for "everything except ALWAYS" — what an operator
/// scanning a real-world spec actually wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OnlyFilter {
    /// Branches no profile activates AND no variant rescues.
    Dead,
    /// Branches reachable under at least one declared `[macros.*]`
    /// variant but inactive under the current build.
    Conditional,
    /// Branches the evaluator couldn't resolve on at least one profile.
    Indeterminate,
    /// Branches every profile activates — usually noise on a healthy
    /// spec, but explicit `--only always` is occasionally useful to
    /// confirm a build flag did flip.
    Always,
    /// Anything except universally-active. The default triage view
    /// (combines dead + conditional + indeterminate + mixed verdicts).
    Noisy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[clap(rename_all = "lower")]
pub enum FailOn {
    /// Always exit 0; the command is purely informational.
    #[default]
    None,
    /// Exit 1 if any branch is dead across the whole target set.
    Dead,
    /// Exit 1 if any branch is dead OR indeterminate on at least
    /// one profile. Strict mode — subsumes `dead`.
    Indeterminate,
}

pub(super) fn run(opts: CoverageOpts, config_override: Option<&Path>) -> Result<ExitCode> {
    let ctx = match super::prepare_matrix(
        config_override,
        opts.target_set.as_deref(),
        &opts.profiles,
        &opts.defines,
    ) {
        Ok(c) => c,
        Err(e) => return e.into_exit(),
    };
    let resolved = ctx.resolved;
    // `matrix coverage` is the only `matrix` subcommand that surfaces
    // the variant-conditional vs. dead distinction. Pull the declared
    // variants out of the config once and feed them to every spec's
    // CoverageReport computation. Other matrix commands keep the
    // variant-blind `compute()` shape — variants only refine the
    // dead-vs-conditional classification, which is coverage-specific.
    let macro_variants = ctx.config.macros.clone();

    let sources = io::read_sources(&opts.input.paths)?;
    let mut any_dead = false;
    let mut any_indeterminate = false;
    let mut reports: Vec<(io::Source, CoverageReport)> = Vec::with_capacity(sources.len());

    for source in sources {
        let parsed = parse(&source.contents);
        let report = CoverageReport::compute_with_variants(
            &parsed.spec,
            &resolved,
            &opts.bcond.to_overrides(),
            &macro_variants,
        );
        if report.dead_branches() > 0 {
            any_dead = true;
        }
        if report.indeterminate_branches() > 0 {
            any_indeterminate = true;
        }
        tracing::debug!(
            spec = %source.display_name(),
            total = report.total_branches(),
            dead = report.dead_branches(),
            indeterminate = report.indeterminate_branches(),
            "coverage report computed"
        );
        reports.push((source, report));
    }

    match opts.format {
        OutputFormat::Human => render_human(&reports, &resolved, opts.only, opts.summary)?,
        OutputFormat::Json => render_json(&reports, &resolved)?,
    }

    let fail = match opts.fail_on {
        FailOn::None => false,
        FailOn::Dead => any_dead,
        FailOn::Indeterminate => any_dead || any_indeterminate,
    };
    Ok(if fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Classification of a branch into one of the rendered tags. Drives
/// both the per-branch tag and the `--only` filter so the renderer
/// can't disagree with the filter about what counts as e.g. DEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchClass {
    Always,
    Dead,
    Conditional,
    Indeterminate,
    /// Verdicts split across profiles (some active, some not) but
    /// none of the strict tags apply. Surfaces as no bracketed
    /// tag in the rendered output.
    Mixed,
}

fn classify(b: &BranchCoverage) -> BranchClass {
    if b.is_dead() {
        BranchClass::Dead
    } else if b.is_universally_active() {
        BranchClass::Always
    } else if b.is_conditional() {
        BranchClass::Conditional
    } else if b.active_on.is_empty() && !b.indeterminate_on.is_empty() {
        BranchClass::Indeterminate
    } else {
        BranchClass::Mixed
    }
}

fn class_passes_filter(cls: BranchClass, filter: Option<OnlyFilter>) -> bool {
    match (cls, filter) {
        (_, None) => true,
        (BranchClass::Always, Some(OnlyFilter::Always)) => true,
        (BranchClass::Dead, Some(OnlyFilter::Dead)) => true,
        (BranchClass::Conditional, Some(OnlyFilter::Conditional)) => true,
        (BranchClass::Indeterminate, Some(OnlyFilter::Indeterminate)) => true,
        // `noisy` = everything except universally-active.
        (_, Some(OnlyFilter::Noisy)) => cls != BranchClass::Always,
        _ => false,
    }
}

/// Aggregate verdict + reason-rollup counters for one spec's report.
/// Single pass over the report so the summary header and the
/// indeterminate-reason rollup share data instead of recomputing.
#[derive(Debug, Default)]
struct ReportSummary {
    total: usize,
    always: usize,
    dead: usize,
    conditional: usize,
    indeterminate: usize,
    mixed: usize,
    /// `(category, code) -> count` over indeterminate branches.
    /// Branches with multiple distinct reasons across profiles are
    /// counted under each reason once; this matches how an operator
    /// would think about "branches touching macro X" rather than
    /// "(branch, profile) pairs".
    reason_counts: BTreeMap<(EvalErrorCategory, &'static str), usize>,
    /// Sample human reason per `(category, code)` for the rollup
    /// line. The renderer keeps the first reason seen so the
    /// rollup carries a representative message (e.g. the actual
    /// macro name for `undefined-macro`).
    reason_sample: BTreeMap<(EvalErrorCategory, &'static str), String>,
}

impl ReportSummary {
    fn from_report(report: &CoverageReport) -> Self {
        let mut s = Self::default();
        for c in &report.conditionals {
            for b in &c.branches {
                s.total += 1;
                match classify(b) {
                    BranchClass::Always => s.always += 1,
                    BranchClass::Dead => s.dead += 1,
                    BranchClass::Conditional => s.conditional += 1,
                    BranchClass::Indeterminate => s.indeterminate += 1,
                    BranchClass::Mixed => s.mixed += 1,
                }
                // Reason rollup includes Mixed branches too — a
                // branch can be active on some profiles and
                // indeterminate on others, and the operator still
                // wants to know which macro to declare.
                let mut seen: std::collections::HashSet<(EvalErrorCategory, &'static str)> =
                    std::collections::HashSet::new();
                for reason in b.indeterminate_reasons.values() {
                    let key = (reason.category(), reason.code());
                    if seen.insert(key) {
                        *s.reason_counts.entry(key).or_insert(0) += 1;
                        s.reason_sample
                            .entry(key)
                            .or_insert_with(|| reason.to_string());
                    }
                }
            }
        }
        s
    }
}

fn render_human(
    reports: &[(io::Source, CoverageReport)],
    target_set: &ResolvedTargetSet,
    only: Option<OnlyFilter>,
    summary_only: bool,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "Matrix coverage: target set `{}` ({} profiles)",
        target_set.id,
        target_set.targets.len()
    )?;
    writeln!(out)?;

    if reports.is_empty() {
        writeln!(out, "(no input specs)")?;
        return Ok(());
    }

    let total_profiles = target_set.targets.len();
    for (source, report) in reports {
        let summary = ReportSummary::from_report(report);
        writeln!(out, "==> {}", source.display_name())?;
        writeln!(
            out,
            "    {} branches: {} always · {} conditional · {} dead · {} indeterminate · {} mixed",
            summary.total,
            summary.always,
            summary.conditional,
            summary.dead,
            summary.indeterminate,
            summary.mixed,
        )?;
        write_reason_rollup(&mut out, &summary)?;
        if report.conditionals.is_empty() {
            writeln!(out, "    (no conditionals — spec has no %if / %ifarch blocks)")?;
            writeln!(out)?;
            continue;
        }
        if summary_only {
            writeln!(out)?;
            continue;
        }
        // Filter-aware printing: emit a header per non-empty section
        // when --only narrows the listing OR when the rendering crosses
        // class boundaries. Operators scan tags; an explicit section
        // banner reinforces "what am I looking at".
        writeln!(out)?;
        let mut printed = 0usize;
        for c in &report.conditionals {
            for b in &c.branches {
                let cls = classify(b);
                if !class_passes_filter(cls, only) {
                    continue;
                }
                write_branch(&mut out, b, cls, total_profiles)?;
                printed += 1;
            }
        }
        if printed == 0 {
            let banner = only
                .map(only_filter_label)
                .unwrap_or("matching");
            writeln!(out, "    (no {banner} branches)")?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Human label for the `--only X` filter, used in the empty-result
/// message when a filter narrows everything away. Lets the operator
/// confirm the filter was actually applied rather than wondering
/// whether the spec genuinely has zero branches.
fn only_filter_label(f: OnlyFilter) -> &'static str {
    match f {
        OnlyFilter::Dead => "dead",
        OnlyFilter::Conditional => "conditional",
        OnlyFilter::Indeterminate => "indeterminate",
        OnlyFilter::Always => "universally-active",
        OnlyFilter::Noisy => "noisy",
    }
}

/// Print the per-spec indeterminate-reason rollup. Operators triaging
/// 100+ indeterminate branches can scan this once to know which
/// macros to declare (`[config]` rows) and which evaluator gaps need
/// a tool fix (`[tool]` rows) before reading any branch detail.
fn write_reason_rollup<W: std::io::Write>(out: &mut W, summary: &ReportSummary) -> Result<()> {
    if summary.reason_counts.is_empty() {
        return Ok(());
    }
    for ((category, code), count) in &summary.reason_counts {
        let prefix = category_short(*category);
        let sample = summary
            .reason_sample
            .get(&(*category, code))
            .map(String::as_str)
            .unwrap_or(*code);
        writeln!(
            out,
            "    {prefix} {code:<22} ({count} branches)  {sample}"
        )?;
    }
    Ok(())
}

fn write_branch<W: std::io::Write>(
    out: &mut W,
    b: &BranchCoverage,
    cls: BranchClass,
    total_profiles: usize,
) -> Result<()> {
    let tag = match cls {
        BranchClass::Always => " [ALWAYS]".to_string(),
        BranchClass::Dead => " [DEAD]".to_string(),
        BranchClass::Conditional => format!(
            " [CONDITIONAL: {}]",
            format_reachable_under(&b.reachable_under)
        ),
        BranchClass::Indeterminate => " [INDET]".to_string(),
        BranchClass::Mixed => String::new(),
    };
    writeln!(
        out,
        "  line {}: {}{tag}",
        b.branch.span.start_line, b.branch.display
    )?;
    // Compact form for monochrome verdicts (one bucket non-empty):
    // ALWAYS, DEAD, and pure-INDET branches don't need the
    // active/inactive/indeterminate skeleton — the tag already says
    // it. This trims ~3 lines per branch on the dominant cases.
    if matches!(cls, BranchClass::Always | BranchClass::Dead) {
        return Ok(());
    }
    // CONDITIONAL with the variant assignment matching every
    // profile in the target set: tag already carries the same info
    // as a `reachable when` line. Skip both the verdict skeleton
    // and the `reachable when` duplicate.
    let conditional_covers_everyone = matches!(cls, BranchClass::Conditional)
        && b.conditional_on.len() == total_profiles
        && b.active_on.is_empty();
    if conditional_covers_everyone && b.indeterminate_on.is_empty() {
        return Ok(());
    }
    // Pure-indeterminate with one reason for every profile: emit a
    // single follow-up line instead of three (active/inactive/indet).
    if matches!(cls, BranchClass::Indeterminate)
        && b.active_on.is_empty()
        && b.inactive_on.is_empty()
        && !b.indeterminate_on.is_empty()
    {
        write_indeterminate_reasons(out, b, total_profiles)?;
        return Ok(());
    }

    // Verbose form: at least two verdict buckets carry information
    // (e.g. `%ifarch` split between arches, or a branch active on
    // some profiles and indeterminate on others).
    writeln!(
        out,
        "    active:   {}",
        format_profile_list(&b.active_on, total_profiles)
    )?;
    writeln!(
        out,
        "    inactive: {}",
        format_profile_list(&b.inactive_on, total_profiles)
    )?;
    if !b.conditional_on.is_empty() && !conditional_covers_everyone {
        writeln!(
            out,
            "    reachable when: {}",
            format_profile_list(&b.conditional_on, total_profiles)
        )?;
    }
    if !b.indeterminate_on.is_empty() {
        write_indeterminate_reasons(out, b, total_profiles)?;
    }
    Ok(())
}

/// Render indeterminate reasons grouped by `(category, code)` with
/// human reason text. Used by both the verbose and the compact
/// branch renderers — shared so the wording stays consistent.
fn write_indeterminate_reasons<W: std::io::Write>(
    out: &mut W,
    b: &BranchCoverage,
    total_profiles: usize,
) -> Result<()> {
    // Group by reason Display text (already normalised at the
    // `EvalError` level — same text means same root cause) so each
    // reason gets one line with the profile list.
    let mut by_reason: BTreeMap<(EvalErrorCategory, &str, String), Vec<&str>> = BTreeMap::new();
    let mut no_reason: Vec<&str> = Vec::new();
    for pid in &b.indeterminate_on {
        match b.indeterminate_reasons.get(pid) {
            Some(reason) => {
                let key = (reason.category(), reason.code(), reason.to_string());
                by_reason.entry(key).or_default().push(pid.as_str());
            }
            None => no_reason.push(pid.as_str()),
        }
    }
    if no_reason.is_empty() && by_reason.len() == 1 {
        let ((cat, code, reason), profiles) = by_reason.iter().next().expect("len==1");
        writeln!(
            out,
            "    indeterminate: {} [{tag}] {reason} → {profiles}",
            category_short(*cat),
            tag = code,
            reason = reason,
            profiles = format_profile_list(profiles, total_profiles)
        )?;
        return Ok(());
    }
    writeln!(out, "    indeterminate:")?;
    for ((cat, code, reason), profiles) in &by_reason {
        writeln!(
            out,
            "      {cat} [{code}] {reason} → {profiles}",
            cat = category_short(*cat),
            code = code,
            reason = reason,
            profiles = format_profile_list(profiles, total_profiles)
        )?;
    }
    if !no_reason.is_empty() {
        writeln!(
            out,
            "      [tool]   [missing-reason] internal: reason not recorded — please file a bug → {}",
            format_profile_list(&no_reason, total_profiles)
        )?;
    }
    Ok(())
}

fn category_short(cat: EvalErrorCategory) -> &'static str {
    match cat {
        EvalErrorCategory::Config => "[config]",
        EvalErrorCategory::Tool => "[tool]  ",
        // `#[non_exhaustive]` on the upstream enum: fall through
        // as the more conservative "tool" label if the analyser
        // ever adds a new variant before the renderer is updated.
        _ => "[tool]  ",
    }
}

/// Lists with fewer profiles than this are rendered verbose
/// (`a, b, c`) even when they cover the whole target set. The
/// collapsed form `(all N profiles)` only pays off when the
/// verbose form would dominate the line; below this threshold
/// the operator wants to see the names directly, and existing
/// snapshot tests rely on the verbose form for small fixtures.
const COLLAPSE_THRESHOLD: usize = 4;

/// Render a profile-id list either as `(none)`, `(all N profiles)`
/// when it covers the whole (sufficiently large) target set, or the
/// comma-joined IDs. Generic over `AsRef<str>` so callers passing
/// `&[String]` (the per-branch lists) and `&[&str]` (the
/// regrouped-by-reason view) hit the same body — eliminates the
/// risk that the two callers' formatting drifts apart.
fn format_profile_list<S: AsRef<str>>(ids: &[S], total_profiles: usize) -> String {
    if ids.is_empty() {
        return "(none)".to_string();
    }
    if total_profiles >= COLLAPSE_THRESHOLD && ids.len() == total_profiles {
        return format!("(all {total_profiles})");
    }
    ids.iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render `macro → {values}` as `macro={v1|v2}, macro2={v3}`.
/// Stable across runs because the underlying map is a `BTreeMap`
/// (sorted by macro name) and the inner set is a `BTreeSet` (sorted
/// by value). The brace-pipe form for multi-value sets disambiguates
/// from the macro-separator comma: previously `;` was used between
/// macros, which scanned as a statement terminator.
fn format_reachable_under(
    reachable: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> String {
    reachable
        .iter()
        .map(|(name, values)| {
            let joined: Vec<&str> = values.iter().map(String::as_str).collect();
            if joined.len() == 1 {
                format!("{name}={}", joined[0])
            } else {
                format!("{name}={{{}}}", joined.join("|"))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_json(
    reports: &[(io::Source, CoverageReport)],
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    let payload = CoverageJsonReport {
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        files: reports
            .iter()
            .map(|(s, r)| CoverageJsonFile {
                path: s.display_name().to_string(),
                total_branches: r.total_branches(),
                dead_branches: r.dead_branches(),
                indeterminate_branches: r.indeterminate_branches(),
                conditionals: r
                    .conditionals
                    .iter()
                    .map(|c| CoverageJsonConditional {
                        start_line: c.span.start_line,
                        has_else: c.has_else,
                        branches: c
                            .branches
                            .iter()
                            .map(|b| CoverageJsonBranch {
                                line: b.branch.span.start_line,
                                display: b.branch.display.as_str(),
                                is_dead: b.is_dead(),
                                is_universally_active: b.is_universally_active(),
                                is_conditional: b.is_conditional(),
                                active_on: &b.active_on,
                                inactive_on: &b.inactive_on,
                                indeterminate_on: &b.indeterminate_on,
                                indeterminate_reasons: &b.indeterminate_reasons,
                                indeterminate_groups: build_indeterminate_groups(b),
                                conditional_on: &b.conditional_on,
                                reachable_under: &b.reachable_under,
                            })
                            .collect(),
                    })
                    .collect(),
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

#[derive(Debug, Serialize)]
struct CoverageJsonReport<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    files: Vec<CoverageJsonFile<'a>>,
}

#[derive(Debug, Serialize)]
struct CoverageJsonFile<'a> {
    path: String,
    total_branches: usize,
    dead_branches: usize,
    indeterminate_branches: usize,
    conditionals: Vec<CoverageJsonConditional<'a>>,
}

#[derive(Debug, Serialize)]
struct CoverageJsonConditional<'a> {
    start_line: u32,
    has_else: bool,
    branches: Vec<CoverageJsonBranch<'a>>,
}

#[derive(Debug, Serialize)]
struct CoverageJsonBranch<'a> {
    line: u32,
    display: &'a str,
    is_dead: bool,
    is_universally_active: bool,
    /// True iff the branch is inactive under the current build but
    /// reachable under at least one declared macro-variant value. See
    /// the parallel `conditional_on` / `reachable_under` fields for
    /// detail. Mutually exclusive with `is_dead`.
    is_conditional: bool,
    active_on: &'a [String],
    inactive_on: &'a [String],
    indeterminate_on: &'a [String],
    /// Map of `profile_id → human-readable reason` for entries in
    /// `indeterminate_on`. `EvalError` serializes as its Display
    /// string (see `impl Serialize for EvalError`), so the JSON
    /// shape is `{profile_id: "reason string"}` — unchanged from the
    /// previous `BTreeMap<String, String>` representation. Empty
    /// (`{}`) when no profile produced an indeterminate verdict.
    indeterminate_reasons: &'a std::collections::BTreeMap<String, EvalError>,
    /// Derived view of `indeterminate_reasons` pivoted to
    /// `[{reason, profiles}]`. Lets tooling skip the
    /// re-grouping step the human renderer also does, and keeps
    /// the per-reason profile-set explicit when many profiles
    /// share one reason. Omitted when no profile is indeterminate.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indeterminate_groups: Vec<IndeterminateGroup>,
    /// Sorted profile IDs where the branch is build-conditional —
    /// inactive under the current `Profile.macros` but reachable
    /// under at least one declared variant value. Omitted when no
    /// `[macros.*]` variants are loaded or none apply.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    conditional_on: &'a [String],
    /// `macro → values` recording which variant values contribute to
    /// reachability on at least one profile. Empty when
    /// `conditional_on` is empty. Sorted by macro name (BTreeMap) and
    /// by value (BTreeSet) for snapshot stability.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    reachable_under: &'a std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

#[derive(Debug, Serialize)]
struct IndeterminateGroup {
    reason: String,
    profiles: Vec<String>,
}

/// Sentinel reason string for orphan profiles (indeterminate but
/// missing a recorded `EvalError`). Mirrors the human renderer's
/// `(no reason recorded)` bucket so JSON and human views stay
/// symmetric — without this, a downstream consumer parsing
/// `indeterminate_groups` would under-count indeterminate profiles
/// vs. `indeterminate_on`. Today every indeterminate profile gets a
/// reason inserted at the source (branch_coverage.rs `evaluate_branch`
/// path), but a future evaluator path that forgets the insertion
/// would surface here rather than silently disappear.
const NO_REASON_RECORDED: &str = "(no reason recorded)";

fn build_indeterminate_groups(b: &BranchCoverage) -> Vec<IndeterminateGroup> {
    if b.indeterminate_on.is_empty() {
        return Vec::new();
    }
    let mut by_reason: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pid in &b.indeterminate_on {
        let key = b
            .indeterminate_reasons
            .get(pid)
            .map(ToString::to_string)
            .unwrap_or_else(|| NO_REASON_RECORDED.to_string());
        by_reason.entry(key).or_default().push(pid.clone());
    }
    by_reason
        .into_iter()
        .map(|(reason, profiles)| IndeterminateGroup { reason, profiles })
        .collect()
}
