//! `matrix expand` — per-profile annotated view of the spec source.
//!
//! For every member profile of a target set, prints the spec lines
//! verbatim with each branch directive line (`%if` / `%elif` /
//! `%else` / `%ifarch` / `%ifnarch` / `%ifos` / `%ifnos`) tagged
//! `[ACTIVE]` / `[INACTIVE]` / `[INDETERMINATE]` according to how
//! the branch evaluator resolves the condition on that profile. The output is a static analogue of `rpmspec -P`:
//! macros are NOT expanded and inactive branch bodies stay in place
//! (just visibly marked so the reader can scan past them).
//!
//! Single-spec command — `matrix expand` on a 200-line spec across
//! 10 profiles already produces 2k lines, batching would drown the
//! signal.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{CoverageReport, EvalError, session::parse};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Annotated original source per profile.
    Human,
    /// Structured JSON: per-profile list of branch directive lines
    /// with their status. The full source is NOT serialised — JSON
    /// consumers can re-read the input themselves.
    Json,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive, matching
/// the rest of the `matrix` family.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct ExpandOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,

    #[command(flatten)]
    pub bcond: crate::app::BcondOverridesArg,
}

pub(super) fn run(opts: ExpandOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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

    let sources = io::read_sources(&opts.input.paths)?;
    if sources.len() > 1 {
        eprintln!(
            "error: matrix expand operates on exactly one spec at a time \
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
    // Surface parser-level issues: the per-profile annotation is
    // computed against the recovered AST, so a partial parse can
    // make a branch invisible (no tag) which would otherwise look
    // like a renderer bug.
    let display_name = source.display_name();
    super::surface_parser_diagnostics(
        super::ParseDiagnosticContext::Expand {
            display_name: &display_name,
        },
        &parsed,
    );
    let coverage = CoverageReport::compute(
        &parsed.spec,
        &resolved,
        &opts.bcond.to_overrides(),
    );

    match opts.format {
        OutputFormat::Human => render_human(&source, &coverage, &resolved)?,
        OutputFormat::Json => render_json(&source, &coverage, &resolved)?,
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Per-profile branch status lookup
// ---------------------------------------------------------------------------

/// One branch's per-profile activity, indexed by source line number.
/// The key is the branch directive line (`%if`/`%elif`/`%else`/etc.);
/// the value carries the original directive text and the activity
/// verdict on the profile under consideration.
struct BranchInfo<'a> {
    directive: &'a str,
    status: BranchStatus<'a>,
}

#[derive(Debug)]
enum BranchStatus<'a> {
    Active,
    Inactive,
    /// Carries the human-readable reason from the evaluator.
    Indeterminate(&'a EvalError),
}

impl BranchStatus<'_> {
    /// Human-renderer tag. Static strings are returned by reference
    /// so the common Active/Inactive paths don't allocate; only the
    /// rare `Indeterminate` arm pays for `format!`.
    fn tag(&self) -> Cow<'static, str> {
        match self {
            Self::Active => Cow::Borrowed("[ACTIVE]"),
            Self::Inactive => Cow::Borrowed("[INACTIVE]"),
            Self::Indeterminate(reason) => {
                Cow::Owned(format!("[INDETERMINATE: {reason}]"))
            }
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Indeterminate(_) => "indeterminate",
        }
    }
}

/// Build `line → BranchInfo` for one profile. Returns an empty map
/// if the coverage report has no conditionals (spec has no `%if`).
fn build_profile_index<'a>(
    coverage: &'a CoverageReport,
    profile_id: &str,
) -> HashMap<u32, BranchInfo<'a>> {
    let mut out = HashMap::new();
    for cond in &coverage.conditionals {
        for b in &cond.branches {
            let status = if b.active_on.iter().any(|p| p == profile_id) {
                BranchStatus::Active
            } else if b.inactive_on.iter().any(|p| p == profile_id) {
                BranchStatus::Inactive
            } else if b.indeterminate_on.iter().any(|p| p == profile_id) {
                let reason = b
                    .indeterminate_reasons
                    .get(profile_id)
                    .expect("indeterminate_on guarantees a matching reasons entry");
                BranchStatus::Indeterminate(reason)
            } else {
                // Defensive: a branch might exist in the coverage
                // report without the profile appearing in any list
                // (e.g. a future evaluator state). Skip rather than
                // render a misleading "ACTIVE" by default.
                continue;
            };
            out.insert(
                b.branch.span.start_line,
                BranchInfo {
                    directive: b.branch.display.as_str(),
                    status,
                },
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn render_human(
    source: &io::Source,
    coverage: &CoverageReport,
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix expand: target set `{}` ({} profiles)",
        target_set.id,
        target_set.targets.len()
    )?;
    writeln!(out, "## {}", source.display_name())?;

    for rt in &target_set.targets {
        let index = build_profile_index(coverage, &rt.profile_id);
        writeln!(out)?;
        writeln!(out, "== Profile {} ==", rt.profile_id)?;
        // Source lines are 1-based; `Span::start_line` mirrors that.
        for (line_no, line) in source.contents.lines().enumerate() {
            let line_no = (line_no + 1) as u32;
            match index.get(&line_no) {
                Some(info) => writeln!(out, "{line}  {}", info.status.tag())?,
                None => writeln!(out, "{line}")?,
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ExpandJson<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    path: String,
    per_profile: Vec<ProfileJson<'a>>,
}

#[derive(Debug, Serialize)]
struct ProfileJson<'a> {
    profile_id: &'a str,
    branches: Vec<BranchJson<'a>>,
}

#[derive(Debug, Serialize)]
struct BranchJson<'a> {
    line: u32,
    directive: &'a str,
    /// `"active"` / `"inactive"` / `"indeterminate"` — `snake_case`
    /// matching the rest of the matrix JSON wire format.
    status: &'static str,
    /// Non-null only when `status == "indeterminate"`; carries the
    /// evaluator's Display string for the [`EvalError`].
    indeterminate_reason: Option<String>,
}

fn render_json(
    source: &io::Source,
    coverage: &CoverageReport,
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    let mut per_profile = Vec::with_capacity(target_set.targets.len());
    for rt in &target_set.targets {
        let index = build_profile_index(coverage, &rt.profile_id);
        // Walk the index in line order so the JSON output is
        // deterministic and tools can diff two runs.
        let mut entries: Vec<(u32, &BranchInfo<'_>)> = index.iter().map(|(k, v)| (*k, v)).collect();
        entries.sort_by_key(|(line, _)| *line);
        let branches: Vec<BranchJson<'_>> = entries
            .iter()
            .map(|(line, info)| BranchJson {
                line: *line,
                directive: info.directive,
                status: info.status.kind_label(),
                indeterminate_reason: match info.status {
                    BranchStatus::Indeterminate(reason) => Some(reason.to_string()),
                    _ => None,
                },
            })
            .collect();
        per_profile.push(ProfileJson {
            profile_id: rt.profile_id.as_str(),
            branches,
        });
    }
    let payload = ExpandJson {
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        path: source.display_name().to_string(),
        per_profile,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}
