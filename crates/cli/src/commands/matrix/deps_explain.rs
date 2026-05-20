//! `matrix deps explain` — narrate the resolver's unsat core as a
//! tree of provenance chains.
//!
//! `matrix deps check` answers "is the buildroot resolvable?" with
//! one terse line per failed atom. `explain` answers the next
//! question operators always ask: "and *why* is this failing — what
//! pulled this dep in, what conflict shut the resolution down?".
//!
//! Output is a tree-shaped narrative per unmet dep / conflict:
//!
//! ```text
//! == /path/spec.spec / profile ==
//! UNMET (3):
//! ├─ libfoo-devel >= 2.0
//! │  └─ pulled by: someapp-1.0.el9
//! │     └─ pulled by: bigproj-2.0.el9 (from spec)
//! ├─ /usr/bin/some-tool
//! │  └─ pulled by: from spec
//! ...
//! CONFLICTS (1):
//! └─ pkgconf-pkg-config-1.7.3 ⟂ pkgconfig-1.6.3 (via /usr/bin/pkg-config)
//!    ├─ cause pulled by: from spec
//!    └─ victim pulled by: legacydep-3.0 (from spec)
//! ```
//!
//! Reuses the solver's existing [`rpm_spec_repo_resolver::UnsatCore`]
//! produced by `solve()` — `matrix deps check` already runs the
//! resolver per (spec × profile) but only renders the verdict; this
//! command renders the chain. No additional resolver work — same
//! solver pass, different render.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use rpm_spec_analyzer::profile::Profile;
use rpm_spec_repo_core::{NEVRA, RepoUniverse};
use rpm_spec_repo_metadata::cache::CacheDirs;
use rpm_spec_repo_resolver::Solution;
use serde::Serialize;

use super::coverage_style::Style;
use super::solver_glue::{self, ConflictEntry, SolveVerdict, UnmetEntry};
use super::universe::cache_universes;
use super::{MatrixPrepareError, prepare_matrix};
use crate::app::{ColorChoice, MacroDefinesArg};
use crate::commands::repo::RepoArgs;

#[derive(Debug, Args)]
pub struct ExplainOpts {
    /// Spec file to explain. Multiple paths are allowed; each is
    /// rendered against every profile in the target set.
    pub paths: Vec<PathBuf>,

    /// Release target set to evaluate. Mutually exclusive with
    /// `--profiles` / `--profile`.
    #[arg(long = "target-set", value_name = "ID", conflicts_with_all = ["profiles", "profile"])]
    pub target_set: Option<String>,

    /// Comma-separated list of profile names. Ad-hoc target set.
    #[arg(long, value_name = "NAMES", value_delimiter = ',', conflicts_with_all = ["target_set", "profile"])]
    pub profiles: Vec<String>,

    /// Single profile. Shorthand for `--profiles foo`.
    #[arg(long = "profile", value_name = "NAME", conflicts_with_all = ["target_set", "profiles"])]
    pub profile: Option<String>,

    /// Show every (spec × profile) pair, including OK ones. By
    /// default `explain` skips passing rows since they have no
    /// chains to render.
    #[arg(long = "show-ok")]
    pub show_ok: bool,

    /// Output format.
    #[arg(long, default_value = "human", value_enum)]
    pub format: OutputFormat,

    /// Exit-code policy. `unsat` (default) returns 1 when any (spec
    /// × profile) yields an unsat core; `never` always returns 0.
    #[arg(long, default_value = "unsat", value_enum)]
    pub fail_on: FailOn,

    #[command(flatten)]
    pub defines: MacroDefinesArg,

    #[command(flatten)]
    pub repo_args: RepoArgs,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum FailOn {
    Never,
    Unsat,
}

pub fn run(opts: ExplainOpts, config_path: Option<&Path>, color: ColorChoice) -> Result<ExitCode> {
    let style = Style::new(color);

    let mut profile_list = opts.profiles.clone();
    if let Some(p) = &opts.profile {
        profile_list.push(p.clone());
    }
    let ctx = match prepare_matrix(
        config_path,
        opts.target_set.as_deref(),
        &profile_list,
        &opts.defines,
    ) {
        Ok(c) => c,
        Err(MatrixPrepareError::UserInputReported) => return Ok(ExitCode::from(2)),
        Err(MatrixPrepareError::Internal(e)) => return Err(e),
    };
    if opts.paths.is_empty() {
        eprintln!("error: no spec file paths provided");
        return Ok(ExitCode::from(2));
    }

    let cache_root = opts.repo_args.resolve_cache_root()?;
    let dirs = CacheDirs::ensure(cache_root)
        .context("preparing the repo cache directory layout")?;
    let profiles: Vec<&_> = ctx.resolved.targets.iter().map(|t| &t.profile).collect();
    let universes = cache_universes(&profiles, &dirs)?;

    let mut report = ExplainReport::default();
    for spec_path in &opts.paths {
        let source = std::fs::read_to_string(spec_path)
            .with_context(|| format!("reading {}", spec_path.display()))?;
        for resolved in &ctx.resolved.targets {
            let profile = &resolved.profile;
            let universe = universes
                .get(&profile.identity.name)
                .cloned()
                .flatten();
            let row = explain_one(spec_path, &source, profile, universe.as_deref());
            if matches!(row.verdict, SolveVerdict::Ok) && !opts.show_ok {
                continue;
            }
            report.rows.push(row);
        }
    }

    render(&report, opts.format, &style)?;
    Ok(report.exit_code(opts.fail_on))
}

fn explain_one(
    spec_path: &Path,
    source: &str,
    profile: &Profile,
    universe: Option<&RepoUniverse>,
) -> ExplainRow {
    let header_spec = spec_path.display().to_string();
    let profile_name = profile.identity.name.clone();

    let Some(universe) = universe else {
        tracing::debug!(
            spec = ?spec_path,
            profile = %profile_name,
            "no cached universe; explain row will be `Skipped`",
        );
        return ExplainRow {
            spec_path: header_spec,
            profile: profile_name,
            verdict: SolveVerdict::Skipped,
            reason: None,
            unsatisfied: Vec::new(),
            conflicts: Vec::new(),
            rich_deps_skipped: 0,
        };
    };

    // Repo-side infra failures degrade to `Error` rather than
    // `?`-propagating, so one corrupt snapshot doesn't poison the
    // whole report. Mirrors the policy in `matrix upgrade-sim` /
    // `matrix buildroot diff`.
    let solution = match solver_glue::solve_for(source, profile, universe) {
        Ok(s) => s,
        Err(e) => {
            return ExplainRow {
                spec_path: header_spec,
                profile: profile_name,
                verdict: SolveVerdict::Error,
                reason: Some(format!("solver failed: {e:#}")),
                unsatisfied: Vec::new(),
                conflicts: Vec::new(),
                rich_deps_skipped: 0,
            };
        }
    };

    match solution {
        Solution::Ok(_) => ExplainRow {
            spec_path: header_spec,
            profile: profile_name,
            verdict: SolveVerdict::Ok,
            reason: None,
            unsatisfied: Vec::new(),
            conflicts: Vec::new(),
            rich_deps_skipped: 0,
        },
        Solution::Unsatisfiable(core) => {
            let deduped = solver_glue::dedup_unsat_core(&core);
            ExplainRow {
                spec_path: header_spec,
                profile: profile_name,
                verdict: SolveVerdict::Unsat,
                reason: None,
                unsatisfied: deduped.unsatisfied,
                conflicts: deduped.conflicts,
                rich_deps_skipped: core.rich_deps_skipped,
            }
        }
    }
}

#[derive(Debug, Default, Serialize)]
struct ExplainReport {
    rows: Vec<ExplainRow>,
}

impl ExplainReport {
    fn exit_code(&self, fail_on: FailOn) -> ExitCode {
        match fail_on {
            FailOn::Never => ExitCode::SUCCESS,
            FailOn::Unsat => {
                if self
                    .rows
                    .iter()
                    .any(|r| matches!(r.verdict, SolveVerdict::Unsat))
                {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct ExplainRow {
    spec_path: String,
    profile: String,
    verdict: SolveVerdict,
    /// Free-form context — populated for `Skipped` / `Error` rows so
    /// the operator sees the cause (cache miss vs corrupt snapshot).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    unsatisfied: Vec<UnmetEntry>,
    conflicts: Vec<ConflictEntry>,
    rich_deps_skipped: usize,
}

fn render(report: &ExplainReport, format: OutputFormat, style: &Style) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match format {
        OutputFormat::Human => {
            if report.rows.is_empty() {
                writeln!(out, "{}", style.dim("(no unsat rows; pass --show-ok to see passing ones)"))?;
                return Ok(());
            }
            for row in &report.rows {
                render_row(&mut out, row, style)?;
            }
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &report)
                .context("serialising matrix deps explain report as JSON")?;
            writeln!(out)?;
        }
    }
    Ok(())
}

fn render_row(out: &mut impl Write, row: &ExplainRow, style: &Style) -> std::io::Result<()> {
    let header = style.header(&format!("== {} / {} ==", row.spec_path, row.profile));
    writeln!(out, "{header}")?;

    match row.verdict {
        SolveVerdict::Ok => {
            writeln!(out, "  {}  (no unsat — nothing to explain)", style.always_tag("OK"))?;
            return Ok(());
        }
        SolveVerdict::Skipped => {
            let detail = row
                .reason
                .as_deref()
                .unwrap_or("no cached repo metadata for this profile");
            writeln!(out, "  {}  {detail}", style.dim("SKIPPED"))?;
            return Ok(());
        }
        SolveVerdict::Error => {
            let detail = row.reason.as_deref().unwrap_or("(no detail)");
            writeln!(out, "  {}  {detail}", style.dead_tag("ERROR"))?;
            return Ok(());
        }
        SolveVerdict::Unsat => {}
    }

    if !row.unsatisfied.is_empty() {
        writeln!(
            out,
            "  {} ({}):",
            style.dead_tag("UNMET"),
            row.unsatisfied.len(),
        )?;
        let last_idx = row.unsatisfied.len().saturating_sub(1);
        for (i, entry) in row.unsatisfied.iter().enumerate() {
            let prefix = if i == last_idx { "└─" } else { "├─" };
            writeln!(out, "  {prefix} {}", entry.dep)?;
            render_chain(out, &entry.required_by, i == last_idx, style)?;
        }
    }

    if !row.conflicts.is_empty() {
        writeln!(
            out,
            "  {} ({}):",
            style.dead_tag("CONFLICTS"),
            row.conflicts.len(),
        )?;
        let last_idx = row.conflicts.len().saturating_sub(1);
        for (i, entry) in row.conflicts.iter().enumerate() {
            let prefix = if i == last_idx { "└─" } else { "├─" };
            writeln!(
                out,
                "  {prefix} {} {} {} (via {})",
                entry.cause,
                style.dim("⟂"),
                entry.victim,
                entry.via,
            )?;
            let outer_cont = if i == last_idx { "   " } else { "│  " };
            render_conflict_side(out, "cause", &entry.cause_chain, outer_cont, false, style)?;
            render_conflict_side(out, "victim", &entry.victim_chain, outer_cont, true, style)?;
        }
    }

    if row.rich_deps_skipped > 0 {
        writeln!(
            out,
            "  {} {} rich dep expression(s) skipped — see RPM-REPO-INFO-RICH-DEP",
            style.conditional_tag("note:"),
            row.rich_deps_skipped,
        )?;
    }
    Ok(())
}

/// Render an unmet entry's provenance chain. `outer_last` controls
/// the carry-over indent character for the parent line — siblings
/// get `│  ` so the next sibling visually descends from the same
/// vertical, the last entry gets `   ` so the tree closes flush.
fn render_chain(
    out: &mut impl Write,
    chain: &[NEVRA],
    outer_last: bool,
    style: &Style,
) -> std::io::Result<()> {
    let outer_cont = if outer_last { "   " } else { "│  " };
    if chain.is_empty() {
        writeln!(out, "  {outer_cont} └─ {}", style.dim("(from spec)"))?;
        return Ok(());
    }
    // `chain[0]` is the *nearest* parent; render in that order then
    // append "(from spec)" as the implicit root.
    for (depth, parent) in chain.iter().enumerate() {
        let inner_indent = "   ".repeat(depth);
        writeln!(
            out,
            "  {outer_cont} {inner_indent}└─ pulled by: {parent}",
        )?;
    }
    let inner_indent = "   ".repeat(chain.len());
    writeln!(
        out,
        "  {outer_cont} {inner_indent}└─ {}",
        style.dim("(from spec)"),
    )?;
    Ok(())
}

/// Render one side of a conflict (cause or victim) with its
/// provenance. `outer_cont` is the carry-over indent from the
/// surrounding conflict node; `last_side` is `true` for the victim
/// (so the tree closes flush after it).
fn render_conflict_side(
    out: &mut impl Write,
    label: &str,
    chain: &[NEVRA],
    outer_cont: &str,
    last_side: bool,
    style: &Style,
) -> std::io::Result<()> {
    let side_prefix = if last_side { "└─" } else { "├─" };
    writeln!(out, "  {outer_cont}{side_prefix} {label} pulled by:")?;
    let side_cont = if last_side { "   " } else { "│  " };
    if chain.is_empty() {
        writeln!(
            out,
            "  {outer_cont}{side_cont}└─ {}",
            style.dim("(from spec)"),
        )?;
        return Ok(());
    }
    for (depth, parent) in chain.iter().enumerate() {
        let inner_indent = "   ".repeat(depth);
        writeln!(
            out,
            "  {outer_cont}{side_cont}{inner_indent}└─ {parent}",
        )?;
    }
    let inner_indent = "   ".repeat(chain.len());
    writeln!(
        out,
        "  {outer_cont}{side_cont}{inner_indent}└─ {}",
        style.dim("(from spec)"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn nevra(name: &str, version: &str) -> NEVRA {
        NEVRA {
            name: Arc::from(name),
            epoch: 0,
            version: Arc::from(version),
            release: Arc::from("1.el9"),
            arch: Arc::from("x86_64"),
        }
    }

    #[test]
    fn render_chain_empty_chain_says_from_spec() {
        let style = Style::new(ColorChoice::Never);
        let mut buf: Vec<u8> = Vec::new();
        render_chain(&mut buf, &[], true, &style).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("(from spec)"), "{text}");
    }

    #[test]
    fn render_chain_includes_each_parent() {
        let style = Style::new(ColorChoice::Never);
        let chain = vec![nevra("middle", "1.0"), nevra("root", "2.0")];
        let mut buf: Vec<u8> = Vec::new();
        render_chain(&mut buf, &chain, true, &style).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("middle-1.0"), "{text}");
        assert!(text.contains("root-2.0"), "{text}");
        assert!(text.contains("(from spec)"), "{text}");
    }

    #[test]
    fn exit_code_policy() {
        let mut report = ExplainReport::default();
        report.rows.push(ExplainRow {
            spec_path: "x".into(),
            profile: "p".into(),
            verdict: SolveVerdict::Unsat,
            reason: None,
            unsatisfied: Vec::new(),
            conflicts: Vec::new(),
            rich_deps_skipped: 0,
        });
        let unsat = format!("{:?}", report.exit_code(FailOn::Unsat));
        let never = format!("{:?}", report.exit_code(FailOn::Never));
        assert!(unsat.contains('1'), "{unsat}");
        assert!(never.contains('0'), "{never}");
    }
}
