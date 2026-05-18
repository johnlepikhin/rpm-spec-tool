//! `matrix portability` — cross-profile macro defined/missing report.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{PortabilityReport, session::parse};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Tabular human report grouped by status.
    Human,
    /// Structured JSON: one entry per macro plus per-spec totals.
    Json,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive — same
/// contract as `matrix check`.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct PortabilityOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    /// Exit with code 1 when any macro lands in the given status or
    /// worse. `partial` covers both `missing` and `partial`;
    /// `missing` covers only `missing`. `none` (default) never fails
    /// — the command is informational.
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
    /// Exit 1 if any macro is `missing` (no profile defines it).
    Missing,
    /// Exit 1 if any macro is `missing` or `partial`.
    Partial,
}

pub(super) fn run(opts: PortabilityOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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
    let mut any_partial = false;
    let mut any_missing = false;
    let mut reports: Vec<(io::Source, PortabilityReport)> = Vec::with_capacity(sources.len());

    for source in sources {
        let parsed = parse(&source.contents);
        let report = PortabilityReport::compute(&parsed.spec, &resolved);
        // Single counts() call drives both the per-spec log and the
        // batch-level fail-on accounting. tracing::debug! (not info)
        // because portability is often run on hundreds of specs and
        // info-level per-spec output would spam CI consoles.
        let counts = report.counts();
        tracing::debug!(
            spec = %source.display_name(),
            used = report.total_used(),
            missing = counts.missing,
            partial = counts.partial,
            portable = counts.portable,
            "portability report computed"
        );
        if counts.missing > 0 {
            any_missing = true;
        }
        if counts.partial > 0 {
            any_partial = true;
        }
        reports.push((source, report));
    }

    match opts.format {
        OutputFormat::Human => render_human(&reports, &resolved)?,
        OutputFormat::Json => render_json(&reports, &resolved)?,
    }

    let fail = match opts.fail_on {
        FailOn::None => false,
        FailOn::Missing => any_missing,
        FailOn::Partial => any_missing || any_partial,
    };
    Ok(if fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn render_human(
    reports: &[(io::Source, PortabilityReport)],
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix portability: target set `{}` ({} profiles)",
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
        let counts = report.counts();
        writeln!(
            out,
            "  {} macros referenced — {} missing, {} partial, {} portable",
            report.total_used(), counts.missing, counts.partial, counts.portable
        )?;
        if report.entries.is_empty() {
            writeln!(out, "  (no macro references)")?;
            writeln!(out)?;
            continue;
        }
        writeln!(out)?;
        writeln!(
            out,
            "  {:<10} {:<28} {:>6}/{:<6} MISSING ON",
            "STATUS", "MACRO", "DEF", "TOTAL"
        )?;
        let total = target_set.targets.len();
        for e in &report.entries {
            let missing = if e.missing_in.is_empty() {
                String::from("-")
            } else {
                e.missing_in.join(", ")
            };
            writeln!(
                out,
                "  {:<10} {:<28} {:>6}/{:<6} {}",
                e.status.as_label(),
                e.name,
                e.defined_in.len(),
                total,
                missing
            )?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn render_json(
    reports: &[(io::Source, PortabilityReport)],
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    let payload = PortabilityJsonReport {
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        files: reports
            .iter()
            .map(|(s, r)| {
                let counts = r.counts();
                PortabilityJsonFile {
                    path: s.display_name().to_string(),
                    total_used: r.total_used(),
                    missing: counts.missing,
                    partial: counts.partial,
                    portable: counts.portable,
                    entries: r
                        .entries
                        .iter()
                        .map(|e| PortabilityJsonEntry {
                            name: e.name.as_str(),
                            status: e.status.as_label(),
                            defined_in: &e.defined_in,
                            missing_in: &e.missing_in,
                        })
                        .collect(),
                }
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
struct PortabilityJsonReport<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    files: Vec<PortabilityJsonFile<'a>>,
}

#[derive(Debug, Serialize)]
struct PortabilityJsonFile<'a> {
    path: String,
    total_used: usize,
    missing: usize,
    partial: usize,
    portable: usize,
    entries: Vec<PortabilityJsonEntry<'a>>,
}

#[derive(Debug, Serialize)]
struct PortabilityJsonEntry<'a> {
    name: &'a str,
    status: &'a str,
    defined_in: &'a [String],
    missing_in: &'a [String],
}
