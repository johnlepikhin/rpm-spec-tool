//! `matrix check` — analyse one or more spec files against every
//! profile in a release target set.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::{MatrixResult, MatrixSignature, run_matrix};

use crate::app::ColorChoice;
use crate::io;

/// Sentinel target-set id reported when the matrix is built from
/// `--profiles a,b,c` rather than a config-defined `[targets.*]`.
/// Public-ish constant because it appears in JSON/SARIF output and in
/// integration tests.
pub const AD_HOC_TARGET_SET_ID: &str = "<ad-hoc>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Per-source table + grouped diagnostics.
    Human,
    /// Structured JSON for tooling consumption.
    Json,
    /// SARIF 2.1.0 with matrix-aware properties on each Result.
    Sarif,
}

/// Policy for the matrix-check exit code when a baseline is supplied.
///
/// * `All` — any deny-severity finding fails (Phase 1 default; matches
///   `lint` semantics).
/// * `New` — only findings whose [`MatrixSignature`] is NOT recorded
///   in the baseline contribute to a non-zero exit. Designed for CI
///   on a legacy spec where the existing warning corpus is acceptable
///   and only regressions should gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[clap(rename_all = "lower")]
pub enum FailOn {
    #[default]
    All,
    New,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive: matrix
/// resolution either uses a config-defined set or an ad-hoc list.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct CheckOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    /// Output format for matrix diagnostics.
    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    /// Name of a target set defined in `[targets.<name>]`.
    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    /// Ad-hoc profile list — bypasses `[targets.*]` and runs against
    /// the given profiles directly. Comma-separated. Equivalent to a
    /// minimal `[targets.<ad-hoc>]` with no shared defines.
    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    /// Override lint severity to `deny` for the named rule. Repeatable.
    /// The `warnings` meta-name (clippy convention) promotes every
    /// `warn` to `deny`.
    #[arg(long = "deny", value_name = "LINT")]
    pub deny: Vec<String>,

    /// Override lint severity to `warn` for the named rule. Repeatable.
    #[arg(long = "warn", value_name = "LINT")]
    pub warn: Vec<String>,

    /// Override lint severity to `allow` (silence) for the named rule.
    /// Repeatable. The `warnings` meta-name clears any earlier
    /// `--deny warnings` promotion.
    #[arg(long = "allow", value_name = "LINT")]
    pub allow: Vec<String>,

    /// Path to a baseline JSON document produced by
    /// `matrix baseline create`. When set, matching aggregated
    /// findings are flagged as "known" in the output and `--fail-on
    /// new` ignores them for exit-code purposes.
    #[arg(long = "baseline", value_name = "PATH")]
    pub baseline: Option<std::path::PathBuf>,

    /// Exit-code policy. Default `all` matches single-profile lint
    /// semantics — any deny finding fails. `new` (requires
    /// `--baseline`) only fails on findings whose signature is not
    /// in the baseline.
    #[arg(long = "fail-on", default_value_t = FailOn::All, value_enum)]
    pub fail_on: FailOn,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

/// Per-source matrix outcome bundled with the source itself for the
/// renderers. Internal DTO between command layer and `output::matrix`
/// — never crosses the crate boundary.
#[derive(Debug)]
pub(crate) struct MatrixCheckResult {
    pub(crate) source: io::Source,
    pub(crate) result: MatrixResult,
}

pub(super) fn run(
    opts: CheckOpts,
    config_override: Option<&Path>,
    color: ColorChoice,
) -> Result<ExitCode> {
    let ctx = match super::prepare_matrix(
        config_override,
        opts.target_set.as_deref(),
        &opts.profiles,
        &opts.defines,
    ) {
        Ok(c) => c,
        Err(e) => return e.into_exit(),
    };
    // Severity overrides are check-specific (only `matrix check` and
    // `matrix baseline create` honour `--deny`/`--warn`/`--allow`).
    // Apply them on top of the shared config the helper returned.
    let config = config_with_severity_overrides(&ctx.config, &opts);
    let resolved = ctx.resolved;

    // Pre-flight: --fail-on=new without --baseline would silently
    // treat every finding as "new", defeating CI gating. Reject up
    // front rather than after I/O.
    if matches!(opts.fail_on, FailOn::New) && opts.baseline.is_none() {
        eprintln!("error: --fail-on new requires --baseline FILE");
        return Ok(ExitCode::from(2));
    }
    let known_signatures = load_known_signatures(opts.baseline.as_deref())?;

    let sources = io::read_sources(&opts.input.paths)?;
    let mut per_source: Vec<MatrixCheckResult> = Vec::with_capacity(sources.len());
    let mut any_deny = false;
    let mut any_new_deny = false;

    for source in sources {
        let source_path = if source.is_stdin {
            None
        } else {
            Some(source.path.as_path())
        };
        let result = run_matrix(&source.contents, source_path, &config, &resolved);
        let (had_deny, had_new_deny) = count_deny_findings(&result, &known_signatures);
        any_deny |= had_deny;
        any_new_deny |= had_new_deny;
        per_source.push(MatrixCheckResult { source, result });
    }

    let _ = color; // colour wiring deferred — see render_human doc.
    match opts.format {
        OutputFormat::Human => {
            crate::output::matrix::render_human(&per_source, &resolved, &known_signatures)?
        }
        OutputFormat::Json => crate::output::matrix::render_json(&per_source, &resolved)?,
        OutputFormat::Sarif => crate::output::matrix::render_sarif(&per_source, &resolved)?,
    }

    let fail = match opts.fail_on {
        FailOn::All => any_deny,
        FailOn::New => any_new_deny,
    };
    Ok(if fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Load and validate the `--baseline` file, returning a typed
/// signature index for O(1) lookup. Returns an empty set when no
/// baseline was supplied.
fn load_known_signatures(baseline_path: Option<&Path>) -> Result<HashSet<MatrixSignature>> {
    let Some(path) = baseline_path else {
        return Ok(HashSet::new());
    };
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening baseline {}", path.display()))?;
    let baseline = rpm_spec_analyzer::Baseline::read(file)
        .with_context(|| format!("reading baseline {}", path.display()))?;
    tracing::info!(path = %path.display(), entries = baseline.len(), "baseline loaded");
    Ok(baseline.signature_set())
}

/// Count Deny-severity findings in one source's matrix result and
/// split them into "any" / "new" (relative to the known signatures
/// from a loaded baseline). Returns `(any_deny, any_new_deny)`.
///
/// Operates on `result.aggregated` rather than per-profile diagnostics:
/// one entry per unique `(lint_id, span, message)` finding, with
/// `AggregatedDiagnostic::signature` already cached by the aggregator.
/// O(unique findings) instead of O(profiles × findings), and aligned
/// with what [`Baseline::from_aggregated`](rpm_spec_analyzer::Baseline::from_aggregated)
/// records — same signature source on both sides, so a recorded
/// baseline and a fresh run see identical signatures for the same
/// root-cause finding.
fn count_deny_findings(result: &MatrixResult, known: &HashSet<MatrixSignature>) -> (bool, bool) {
    let mut any_deny = false;
    let mut any_new_deny = false;
    for ad in &result.aggregated {
        if ad.diagnostic.severity != rpm_spec_analyzer::Severity::Deny {
            continue;
        }
        any_deny = true;
        if !known.contains(&ad.signature) {
            any_new_deny = true;
        }
    }
    (any_deny, any_new_deny)
}

/// Apply severity overrides from `--deny/--warn/--allow` to a fresh
/// owned [`Config`] if any are set; otherwise clone the cached one.
/// Shared with the `matrix baseline create` path so both honour the
/// same severity policy.
pub(super) fn config_with_severity_overrides(cached: &Config, opts: &CheckOpts) -> Config {
    let mut c: Config = cached.clone();
    if !opts.deny.is_empty() || !opts.warn.is_empty() || !opts.allow.is_empty() {
        c.apply_cli_overrides(&opts.allow, &opts.warn, &opts.deny);
    }
    c
}
