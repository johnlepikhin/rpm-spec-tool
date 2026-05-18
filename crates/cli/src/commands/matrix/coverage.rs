//! `matrix coverage` — show which `%if`/`%ifarch` branches activate
//! on which profiles.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{BranchCoverage, CoverageReport, EvalError, session::parse};
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

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
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
    if let Err(e) = opts.defines.validate() {
        eprintln!("error: {e}");
        return Ok(ExitCode::from(2));
    }
    let (config, base_dir) =
        crate::commands::config_loader::load_config(config_override)?;
    let resolved = match super::resolve_matrix_source(
        &config,
        &base_dir,
        opts.target_set.as_deref(),
        &opts.profiles,
        &opts.defines.raw,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(ExitCode::from(2));
        }
    };

    let sources = io::read_sources(&opts.input.paths)?;
    let mut any_dead = false;
    let mut any_indeterminate = false;
    let mut reports: Vec<(io::Source, CoverageReport)> = Vec::with_capacity(sources.len());

    for source in sources {
        let parsed = parse(&source.contents);
        let report = CoverageReport::compute(&parsed.spec, &resolved);
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
        OutputFormat::Human => render_human(&reports, &resolved)?,
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

fn render_human(
    reports: &[(io::Source, CoverageReport)],
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix coverage: target set `{}` ({} profiles)",
        target_set.id,
        target_set.targets.len()
    )?;
    writeln!(out)?;

    if reports.is_empty() {
        writeln!(out, "(no input specs)")?;
        return Ok(());
    }

    for (source, report) in reports {
        writeln!(out, "## {}", source.display_name())?;
        writeln!(
            out,
            "  {} branches — {} dead, {} indeterminate",
            report.total_branches(),
            report.dead_branches(),
            report.indeterminate_branches()
        )?;
        if report.conditionals.is_empty() {
            writeln!(out, "  (no conditionals)")?;
            writeln!(out)?;
            continue;
        }
        writeln!(out)?;
        for c in &report.conditionals {
            for b in &c.branches {
                write_branch(&mut out, b)?;
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

fn write_branch<W: std::io::Write>(out: &mut W, b: &BranchCoverage) -> Result<()> {
    let tag = if b.is_dead() {
        " [DEAD]"
    } else if b.is_universally_active() {
        " [ALWAYS]"
    } else {
        ""
    };
    writeln!(
        out,
        "  line {}: {}{tag}",
        b.branch.span.start_line, b.branch.display
    )?;
    writeln!(
        out,
        "    active: {}",
        format_or_none(&b.active_on)
    )?;
    writeln!(
        out,
        "    inactive: {}",
        format_or_none(&b.inactive_on)
    )?;
    if !b.indeterminate_on.is_empty() {
        // Render each indeterminate profile with its reason inline
        // when available. Format: `pid (reason), pid (reason), ...`.
        let mut parts = Vec::with_capacity(b.indeterminate_on.len());
        for pid in &b.indeterminate_on {
            match b.indeterminate_reasons.get(pid) {
                Some(reason) => parts.push(format!("{pid} ({reason})")),
                None => parts.push(pid.clone()),
            }
        }
        writeln!(out, "    indeterminate: {}", parts.join(", "))?;
    }
    Ok(())
}

fn format_or_none(ids: &[String]) -> String {
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
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
                                active_on: &b.active_on,
                                inactive_on: &b.inactive_on,
                                indeterminate_on: &b.indeterminate_on,
                                indeterminate_reasons: &b.indeterminate_reasons,
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
}
