//! `matrix verify-contract` — CI-grade assertion that the spec
//! produces the BuildRequires set declared in a separate contract
//! TOML document.
//!
//! Phase 7 supports `must_have_buildrequires` /
//! `must_not_have_buildrequires` per profile only. The collector is
//! conditional-unaware (see `analyzer::contract` module doc) — that
//! is the intentional MVP trade-off and is the right semantic for
//! "did anyone forget to declare a critical build dep" gating.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{
    Contract, ContractProfileStatus, ContractReport, ContractViolation,
    session::parse,
};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Per-spec block: each profile's status + violations.
    Human,
    /// Structured JSON for tooling consumption.
    Json,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive; `--contract`
/// is always required so the gate has explicit expectations and
/// `verify-contract` never silently passes with no rules.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct VerifyContractOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    /// Path to the contract TOML document. Required — without it the
    /// command would silently pass since there are no expectations to
    /// check.
    #[arg(long = "contract", value_name = "PATH")]
    pub contract: PathBuf,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

pub(super) fn run(opts: VerifyContractOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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

    let contract = match load_contract(&opts.contract) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(ExitCode::from(2));
        }
    };

    let profile_ids: Vec<String> = resolved
        .targets
        .iter()
        .map(|t| t.profile_id.clone())
        .collect();

    let sources = io::read_sources(&opts.input.paths)?;
    let mut any_violations = false;
    let mut reports: Vec<(io::Source, ContractReport)> = Vec::with_capacity(sources.len());

    for source in sources {
        let parsed = parse(&source.contents);
        // Surface parser-level issues up-front: a contract verdict
        // on a partial AST is meaningless ("required dep missing"
        // could just mean "parser dropped that line during recovery"
        // — and worse, a "must_not_have" violation might silently
        // slip past). Mirror explain's banner so the operator sees
        // the degraded state before reading the report.
        if !parsed.parser_diagnostics.is_empty() {
            let total = parsed.parser_diagnostics.len();
            let errors = parsed
                .parser_diagnostics
                .iter()
                .filter(|d| {
                    matches!(d.severity, rpm_spec_analyzer::ParserSeverity::Error)
                })
                .count();
            eprintln!(
                "warning: {} produced {total} parser diagnostic(s) ({errors} error-level) — \
                 contract verdict below is computed against the recovered AST and may be unreliable",
                source.display_name()
            );
        }
        let report = ContractReport::compute(&parsed.spec, &contract, &profile_ids);
        if report.has_violations() {
            any_violations = true;
        }
        tracing::debug!(
            spec = %source.display_name(),
            violations = report.has_violations(),
            "contract report computed"
        );
        reports.push((source, report));
    }

    match opts.format {
        OutputFormat::Human => render_human(&reports, &resolved)?,
        OutputFormat::Json => render_json(&reports, &resolved)?,
    }

    Ok(if any_violations {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn load_contract(path: &Path) -> Result<Contract> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("opening contract {}", path.display()))?;
    let contract = Contract::from_toml_str(&raw)
        .with_context(|| format!("parsing contract {}", path.display()))?;
    tracing::info!(
        path = %path.display(),
        profiles = contract.profiles.len(),
        "contract loaded"
    );
    Ok(contract)
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn render_human(
    reports: &[(io::Source, ContractReport)],
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix verify-contract: target set `{}` ({} profiles)",
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
        for entry in &report.per_profile {
            match &entry.status {
                ContractProfileStatus::NoContract => {
                    writeln!(out, "  {}: (no contract — skipping)", entry.profile_id)?;
                }
                ContractProfileStatus::Pass => {
                    writeln!(out, "  {}: PASS", entry.profile_id)?;
                }
                ContractProfileStatus::Violations { violations } => {
                    writeln!(
                        out,
                        "  {}: FAIL ({} violation(s))",
                        entry.profile_id,
                        violations.len()
                    )?;
                    for v in violations {
                        render_violation(&mut out, v)?;
                    }
                }
                // `ContractProfileStatus` is `#[non_exhaustive]` in
                // the analyzer crate, so cross-crate exhaustive
                // matching needs this arm. Renders a stable label
                // (not `{:?}`) so a future variant added in the
                // analyzer surfaces visibly in the CLI output.
                _ => writeln!(
                    out,
                    "  {}: (unknown status — please update rpm-spec-tool)",
                    entry.profile_id
                )?,
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Render one violation in the human format. The wildcard arm
/// exists because `ContractViolation` is `#[non_exhaustive]` in the
/// analyzer crate and we are matching across crate boundaries —
/// rustc requires a fallback. The fallback emits a stable label
/// rather than a Debug-formatted blob so a future variant produces
/// a visible UX-grade hint, not opaque struct dump.
fn render_violation(
    out: &mut impl std::io::Write,
    v: &ContractViolation,
) -> std::io::Result<()> {
    match v {
        ContractViolation::MissingRequired { package } => {
            writeln!(out, "    [missing] {package}")
        }
        ContractViolation::ForbiddenPresent { package, found_as } => {
            if package == found_as {
                writeln!(out, "    [forbidden] {package}")
            } else {
                writeln!(out, "    [forbidden] {package} (found as `{found_as}`)")
            }
        }
        _ => writeln!(out, "    [unknown violation kind — please update rpm-spec-tool]"),
    }
}

#[derive(Debug, Serialize)]
struct ContractJson<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    files: Vec<ContractJsonFile<'a>>,
}

#[derive(Debug, Serialize)]
struct ContractJsonFile<'a> {
    path: String,
    /// Per-profile report — borrows the analyzer's `ContractReport`
    /// shape verbatim (the analyzer types already derive `Serialize`),
    /// keeping the JSON wire format aligned with the in-memory model.
    per_profile: &'a [rpm_spec_analyzer::ProfileContractReport],
}

fn render_json(
    reports: &[(io::Source, ContractReport)],
    target_set: &ResolvedTargetSet,
) -> Result<()> {
    let payload = ContractJson {
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        files: reports
            .iter()
            .map(|(s, r)| ContractJsonFile {
                path: s.display_name().to_string(),
                per_profile: &r.per_profile,
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
