//! `matrix deps check` — repository-aware lint pass.
//!
//! For each spec × profile in the target set:
//!   1. Build a `RepoUniverse` from cached `[profiles.X.repos.*]`
//!      snapshots (no network — cache-only).
//!   2. Run the analyzer's lint session against the spec, passing
//!      the universe so `RPM-REPO-001/002/003` are active.
//!   3. Emit only `RPM-REPO-*` diagnostics (other rules' findings
//!      are accessible via `matrix check` — this command is laser
//!      focused on repo resolvability).
//!
//! Profile rows where no repos are cached emit a single
//! one-time INFO note and skip silently — same UX `matrix check`
//! uses for any profile-aware rule.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use rpm_spec_analyzer::diagnostic::{Diagnostic, Severity};
use rpm_spec_analyzer::profile::Profile;
use rpm_spec_analyzer::{LintSession, parse};
use rpm_spec_repo_metadata::cache::CacheDirs;

use super::coverage_style::Style;
use super::universe::cache_universes;
use super::{MatrixPrepareError, prepare_matrix};
use crate::app::{ColorChoice, MacroDefinesArg};
use crate::commands::repo::RepoArgs;

#[derive(Debug, Args)]
pub struct DepsOpts {
    /// Spec file(s) to analyse. Use `-` to read one spec from stdin.
    pub paths: Vec<PathBuf>,

    /// Release target set to evaluate. Mutually exclusive with
    /// `--profiles` and `--profile`.
    #[arg(long = "target-set", value_name = "ID", conflicts_with_all = ["profiles", "profile"])]
    pub target_set: Option<String>,

    /// Comma-separated list of profile names. Ad-hoc target set.
    #[arg(long, value_name = "NAMES", value_delimiter = ',', conflicts_with_all = ["target_set", "profile"])]
    pub profiles: Vec<String>,

    /// Single profile to analyse. Equivalent to `--profiles foo`.
    #[arg(long = "profile", value_name = "NAME", conflicts_with_all = ["target_set", "profiles"])]
    pub profile: Option<String>,

    /// Output format.
    #[arg(long, default_value = "human", value_enum)]
    pub format: OutputFormat,

    /// Exit code policy. `none` → always 0; `findings` → 1 if any
    /// RPM-REPO-* finding emitted; `error` → 1 only when an
    /// RPM-REPO-* finding has severity `deny`.
    #[arg(long, default_value = "error", value_enum)]
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
    None,
    Findings,
    Error,
}

pub fn run(
    opts: DepsOpts,
    config_path: Option<&Path>,
    color: ColorChoice,
) -> Result<ExitCode> {
    let style = Style::new(color);

    // Single-`--profile` is just a one-element ad-hoc set; fold it
    // into `profiles` before calling `prepare_matrix` which only
    // recognises the two-flag CheckOpts shape.
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
        Ok(ctx) => ctx,
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

    // Build the per-profile universe ONCE up-front. The (spec × profile)
    // grid touches the same profile repeatedly; per-spec DB opens would
    // cost a fresh connection setup each time. `Arc<RepoUniverse>` makes
    // the per-iteration clone cheap (refcount bump).
    let profiles: Vec<&_> = ctx.resolved.targets.iter().map(|t| &t.profile).collect();
    let universe_cache = cache_universes(&profiles, &dirs)?;

    let mut report = DepsReport::default();
    for spec_path in &opts.paths {
        let source = std::fs::read_to_string(spec_path)
            .with_context(|| format!("reading {}", spec_path.display()))?;
        for resolved in &ctx.resolved.targets {
            let profile = resolved.profile.clone();
            let universe = universe_cache
                .get(&profile.identity.name)
                .cloned()
                .flatten();
            let universe_present = universe.is_some();
            let session = LintSession::from_config_with_profile_and_universe(
                &ctx.config,
                profile.clone(),
                universe,
            );
            run_one(
                spec_path,
                &source,
                &profile,
                session,
                universe_present,
                &mut report,
            );
        }
    }

    render(&report, opts.format, &style)?;
    Ok(report.exit_code(opts.fail_on))
}

fn run_one(
    spec_path: &Path,
    source: &str,
    profile: &Profile,
    mut session: LintSession,
    universe_present: bool,
    report: &mut DepsReport,
) {
    let outcome = parse(source);
    let mut diags = session.run(&outcome.spec, source);
    // Keep RPM-REPO-* diagnostics only — `matrix deps check` is a
    // focused command. Operators after the full lint surface should
    // call `matrix check`.
    diags.retain(|d| d.lint_id.starts_with("RPM-REPO-"));
    report.rows.push(DepsRow {
        spec_path: spec_path.display().to_string(),
        profile: profile.identity.name.clone(),
        universe_present,
        diagnostics: diags,
    });
}

#[derive(Debug, Default)]
struct DepsReport {
    rows: Vec<DepsRow>,
}

impl DepsReport {
    fn has_any_finding(&self) -> bool {
        self.rows.iter().any(|r| !r.diagnostics.is_empty())
    }

    fn has_deny_finding(&self) -> bool {
        self.rows.iter().any(|r| {
            r.diagnostics
                .iter()
                .any(|d| matches!(d.severity, Severity::Deny))
        })
    }

    fn exit_code(&self, fail_on: FailOn) -> ExitCode {
        match fail_on {
            FailOn::None => ExitCode::SUCCESS,
            FailOn::Findings => {
                if self.has_any_finding() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            FailOn::Error => {
                if self.has_deny_finding() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
        }
    }
}

#[derive(Debug)]
struct DepsRow {
    spec_path: String,
    profile: String,
    universe_present: bool,
    diagnostics: Vec<Diagnostic>,
}

fn render(report: &DepsReport, format: OutputFormat, style: &Style) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match format {
        OutputFormat::Human => {
            for row in &report.rows {
                // Header is always bold so spec/profile boundaries pop
                // out of the noise even when no diagnostics fire.
                let header = style.header(&format!("== {} / {} ==", row.spec_path, row.profile));
                if !row.universe_present {
                    let hint = style.dim(
                        "(no cached repo metadata — run `rpm-spec-tool repo sync` for this profile; RPM-REPO-* rules skipped)",
                    );
                    writeln!(out, "{header}  {hint}")?;
                    continue;
                }
                writeln!(out, "{header}")?;
                if row.diagnostics.is_empty() {
                    writeln!(out, "  {}", style.always_tag("OK"))?;
                    continue;
                }
                for d in &row.diagnostics {
                    // Severity colours match `matrix coverage`:
                    // bold red = deny, bold yellow = warn, bold blue
                    // = info (Allow is rendered as "info" because the
                    // diagnostic survived `Allow`-severity filtering
                    // only when a downstream re-promotion overrode
                    // the rule's default).
                    let sev_label = match d.severity {
                        Severity::Deny => style.dead_tag("error"),
                        Severity::Warn => style.conditional_tag("warn"),
                        Severity::Allow => style.indet_tag("info"),
                    };
                    let id = style.dim(d.lint_id);
                    let line = style.dim(&format!("L{}", d.primary_span.start_line));
                    writeln!(out, "  {sev_label} {id} {line} {msg}", msg = d.message)?;
                }
            }
        }
        OutputFormat::Json => {
            #[derive(serde::Serialize)]
            struct JsonRow<'a> {
                spec_path: &'a str,
                profile: &'a str,
                universe_present: bool,
                diagnostics: &'a [Diagnostic],
            }
            let rows: Vec<JsonRow<'_>> = report
                .rows
                .iter()
                .map(|r| JsonRow {
                    spec_path: &r.spec_path,
                    profile: &r.profile,
                    universe_present: r.universe_present,
                    diagnostics: &r.diagnostics,
                })
                .collect();
            serde_json::to_writer_pretty(&mut out, &rows)
                .context("serialising matrix deps check report as JSON")?;
            writeln!(out)?;
        }
    }
    Ok(())
}
