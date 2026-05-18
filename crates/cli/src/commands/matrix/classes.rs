//! `matrix classes` — group target-set profiles by their effective
//! dependency footprint.
//!
//! Use case: a release team owns a matrix of 30 profiles spread over
//! 4 distros × 4 archs × 2 distro versions. The spec may produce the
//! same `BuildRequires`/`Requires` set on many of those profiles
//! once branch resolution is done. `matrix classes` collapses the
//! profile list into equivalence classes — one row per distinct
//! "what this spec produces" — and surfaces a recommended minimal
//! representative build set (one profile per class).
//!
//! Branch-aware via [`ProfileBranchSelection`] under
//! [`IndeterminatePolicy::Skip`] — matches `matrix diff` semantics
//! so two profiles equivalent here will also produce empty `only_a`
//! / `only_b` buckets when run through `matrix diff`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{ClassesReport, session::parse};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Per-class rank with member profile list + minimal build-set.
    Human,
    /// Structured JSON: per-class entries + recommended representatives.
    Json,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct ClassesOpts {
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

pub(super) fn run(opts: ClassesOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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
            "error: matrix classes operates on exactly one spec at a time \
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
        super::ParseDiagnosticContext::Classes {
            display_name: &display_name,
        },
        &parsed,
    );

    let report = ClassesReport::compute(&parsed.spec, &resolved, &opts.bcond.to_overrides());

    match opts.format {
        OutputFormat::Human => render_human(&source, &report, &resolved)?,
        OutputFormat::Json => render_json(&source, &report, &resolved)?,
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn render_human(
    source: &io::Source,
    report: &ClassesReport,
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix classes: target set `{}` ({} profiles → {} class(es))",
        target_set.id,
        target_set.targets.len(),
        report.class_count()
    )?;
    writeln!(out, "## {}", source.display_name())?;

    if report.classes.is_empty() {
        writeln!(out, "  (no profiles in target set)")?;
        return Ok(());
    }

    for (i, class) in report.classes.iter().enumerate() {
        writeln!(out)?;
        writeln!(
            out,
            "## Class {} ({} members, sig {})",
            i + 1,
            class.members.len(),
            class.signature
        )?;
        writeln!(out, "  representative: {}", class.representative)?;
        writeln!(out, "  members:        {}", class.members.join(", "))?;
        for bucket in &class.deps_by_tag {
            if bucket.deps.is_empty() {
                writeln!(out, "  {} (0): (none)", bucket.tag_label)?;
            } else {
                writeln!(
                    out,
                    "  {} ({}): {}",
                    bucket.tag_label,
                    bucket.deps.len(),
                    bucket.deps.join(", ")
                )?;
            }
        }
    }

    // Minimal build set — one representative per class, sorted by
    // member count descending (matches class order). Operators can
    // pipe this into CI.
    writeln!(out)?;
    writeln!(out, "## Minimal representative build set ({})", report.class_count())?;
    for rep in report.representatives() {
        writeln!(out, "  {rep}")?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ClassesJson<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    path: String,
    class_count: usize,
    classes: &'a [rpm_spec_analyzer::EquivalenceClass],
    representatives: Vec<&'a str>,
}

fn render_json(
    source: &io::Source,
    report: &ClassesReport,
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    let payload = ClassesJson {
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        path: source.display_name().to_string(),
        class_count: report.class_count(),
        classes: &report.classes,
        representatives: report.representatives().collect(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}
