//! `matrix upgrade-sim` — per-profile upgrade simulation against the
//! configured repository snapshots.
//!
//! For each (spec × profile) row, the command:
//! 1. Extracts the spec's main NEVR (Name + Epoch + Version + Release).
//! 2. Looks up the highest-EVR published binary built from the same
//!    source RPM, filtered to the profile's `build_arch` plus `noarch`.
//! 3. Renders one of five verdicts:
//!    - `UPGRADE` — new EVR strictly greater (the happy path)
//!    - `SAME` — new EVR equal to current
//!    - `REGRESS` — new EVR less than current (or epoch dropped /
//!      lowered, which rpm interprets as a regression)
//!    - `NEW` — no published binary for this name yet
//!    - `SKIPPED` — no cached repo metadata for the profile
//!
//! Exit-code policy:
//! * `--fail-on never` always returns 0.
//! * `--fail-on regress` (default) returns 1 if any row's verdict
//!   is `REGRESS`. `SAME` / `NEW` do not fail (intentional new
//!   packages and unchanged versions are common in CI dry-runs).
//!
//! Runs entirely against the on-disk SQLite cache populated by
//! `repo sync` — no network. The same source-name lookup powers
//! RPM-REPO-030 / RPM-REPO-031 lints, so verdicts agree by
//! construction.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;

use rpm_spec_analyzer::profile::Profile;
use rpm_spec_analyzer::session::parse;
use rpm_spec_analyzer::{ArchFilter, SpecMainNevr, enriched_macros_with_spec_locals};
use rpm_spec_repo_core::{EVR, NEVRA, RepoUniverse};
use rpm_spec_repo_metadata::cache::CacheDirs;
use serde::Serialize;

use super::coverage_style::Style;
use super::universe::cache_universes;
use super::{MatrixPrepareError, prepare_matrix};
use crate::app::{ColorChoice, MacroDefinesArg};
use crate::commands::repo::RepoArgs;

#[derive(Debug, Args)]
pub struct UpgradeSimOpts {
    /// Spec file(s) to evaluate. Each path runs against every member
    /// of the target set; N specs × M profiles → N×M rows.
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

    /// Output format.
    #[arg(long, default_value = "human", value_enum)]
    pub format: OutputFormat,

    /// Exit-code policy. `never` always returns 0; `regress` (default)
    /// returns 1 when any spec × profile yields a REGRESS verdict.
    #[arg(long, default_value = "regress", value_enum)]
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
    Regress,
}

pub fn run(
    opts: UpgradeSimOpts,
    config_path: Option<&Path>,
    color: ColorChoice,
) -> Result<ExitCode> {
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
    let profiles: Vec<&_> = ctx.resolved.targets.iter().map(|t| &t.profile).collect();
    let universes = cache_universes(&profiles, &dirs)?;

    let mut report = UpgradeSimReport::default();
    for spec_path in &opts.paths {
        let source = std::fs::read_to_string(spec_path)
            .with_context(|| format!("reading {}", spec_path.display()))?;
        for resolved in &ctx.resolved.targets {
            let profile = &resolved.profile;
            let universe = universes
                .get(&profile.identity.name)
                .cloned()
                .flatten();
            let row = simulate_one(spec_path, &source, profile, universe.as_deref())?;
            report.rows.push(row);
        }
    }

    render(&report, opts.format, &style)?;
    Ok(report.exit_code(opts.fail_on))
}

fn simulate_one(
    spec_path: &Path,
    source: &str,
    profile: &Profile,
    universe: Option<&RepoUniverse>,
) -> Result<UpgradeRow> {
    let header_spec = spec_path.display().to_string();
    let profile_name = profile.identity.name.clone();

    let outcome = parse(source);
    let macros = enriched_macros_with_spec_locals(&outcome.spec, profile);
    let Some(spec_nevr) = SpecMainNevr::extract(&outcome.spec, &macros) else {
        // Parser already complained — we can't say anything useful
        // about a spec with no Name/Version/Release.
        return Ok(UpgradeRow {
            spec_path: header_spec,
            profile: profile_name,
            verdict: Verdict::Skipped,
            spec_nevra: None,
            current_nevra: None,
            reason: Some("spec missing Name / Version / Release".to_string()),
        });
    };
    let spec_display = nevra_display(&spec_nevr);
    let proposed_evr = spec_nevr.to_evr();

    let Some(universe) = universe else {
        return Ok(UpgradeRow {
            spec_path: header_spec,
            profile: profile_name,
            verdict: Verdict::Skipped,
            spec_nevra: Some(spec_display),
            current_nevra: None,
            reason: Some("no cached repo metadata; run `repo sync --allow-fetch`".to_string()),
        });
    };

    // Repo-side DB failures are infrastructure errors, not user
    // errors — a corrupt snapshot for one profile shouldn't poison
    // the whole matrix run. Convert to `Verdict::Skipped` with the
    // error surfaced in `reason`, mirroring the cache-miss path
    // above, so CI keeps reporting on every other (spec × profile).
    let candidates = match universe.binaries_built_from(&spec_nevr.name) {
        Ok(c) => c,
        Err(e) => {
            return Ok(UpgradeRow {
                spec_path: header_spec,
                profile: profile_name,
                verdict: Verdict::Skipped,
                spec_nevra: Some(spec_display),
                current_nevra: None,
                reason: Some(format!(
                    // `RepoError` is thiserror with `#[error(... {0})]`
                    // / `#[source]` chains baked into the variant
                    // strings, so Display already includes the wrapped
                    // cause (e.g. `"SQLite error: …"`). The lint side
                    // also renders Display via `tracing::warn!(error = %e)`,
                    // so the CLI's `note:` line and the structured log
                    // entry agree by construction.
                    "repo lookup for `{}` failed: {e}",
                    spec_nevr.name
                )),
            });
        }
    };
    let arch_filter = ArchFilter::from_profile(profile);
    let best: Option<NEVRA> = candidates
        .into_iter()
        .filter(|(_pref, n)| {
            n.name.as_ref() == spec_nevr.name && arch_filter.matches(&n.arch)
        })
        .map(|(_pref, n)| n)
        .max_by(|a, b| a.evr().cmp(&b.evr()));

    let Some(best) = best else {
        return Ok(UpgradeRow {
            spec_path: header_spec,
            profile: profile_name,
            verdict: Verdict::New,
            spec_nevra: Some(spec_display),
            current_nevra: None,
            reason: None,
        });
    };

    let verdict = classify(
        &proposed_evr,
        &best.evr(),
        spec_nevr.epoch_for_ordering(),
        best.epoch,
    );
    Ok(UpgradeRow {
        spec_path: header_spec,
        profile: profile_name,
        verdict,
        spec_nevra: Some(spec_display),
        current_nevra: Some(best.to_string()),
        reason: None,
    })
}

/// Three-way verdict from comparing the proposed EVR (carrying the
/// spec's `Epoch:`) against the highest published binary EVR. We
/// inject the explicit epochs into temporary `EVR`s before comparing
/// so the rpm vercmp ordering reflects both halves uniformly — the
/// per-field `EVR` callers pass in here don't necessarily carry the
/// epoch (the lint's caller leaves it `None`), and rpm treats higher
/// epoch as monotonically greater regardless of version/release.
fn classify(proposed: &EVR, current: &EVR, proposed_epoch: u32, current_epoch: u32) -> Verdict {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let proposed = EVR::new(Some(proposed_epoch), proposed.version.as_str(), proposed.release.as_str());
    let current = EVR::new(Some(current_epoch), current.version.as_str(), current.release.as_str());
    match proposed.cmp(&current) {
        Greater => Verdict::Upgrade,
        Equal => Verdict::Same,
        Less => Verdict::Regress,
    }
}

fn nevra_display(nevr: &SpecMainNevr) -> String {
    // Render the user-authored form: explicit `0:` epoch (rather than
    // eliding it) when the spec wrote `Epoch: 0`, omitted entirely
    // when the spec has no `Epoch:` line. The 030/031 diagnostics
    // care about that distinction, so the CLI display should too.
    match nevr.epoch {
        Some(epoch) if epoch > 0 => {
            format!("{}-{}:{}-{}", nevr.name, epoch, nevr.version, nevr.release)
        }
        _ => format!("{}-{}-{}", nevr.name, nevr.version, nevr.release),
    }
}

#[derive(Debug, Default, Serialize)]
struct UpgradeSimReport {
    rows: Vec<UpgradeRow>,
}

impl UpgradeSimReport {
    fn exit_code(&self, fail_on: FailOn) -> ExitCode {
        match fail_on {
            FailOn::Never => ExitCode::SUCCESS,
            FailOn::Regress => {
                if self
                    .rows
                    .iter()
                    .any(|r| matches!(r.verdict, Verdict::Regress))
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
struct UpgradeRow {
    spec_path: String,
    profile: String,
    verdict: Verdict,
    /// What the spec proposes (NEVR without arch since the spec is
    /// arch-agnostic; the arch comes from `profile.build_arch`).
    /// `None` when the spec is missing required fields.
    spec_nevra: Option<String>,
    /// Highest-EVR binary currently published for this name+arch
    /// combination. `None` when nothing is published yet (`NEW`) or
    /// no cache (`SKIPPED`).
    current_nevra: Option<String>,
    /// Free-form context — populated for `SKIPPED` rows so the user
    /// understands whether it was a parse failure vs missing cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    /// New EVR strictly greater — happy upgrade path.
    Upgrade,
    /// New EVR equal to currently published. Common on rebuild-only
    /// commits; not a failure but worth surfacing.
    Same,
    /// New EVR less than published, OR epoch dropped / lowered.
    /// `--fail-on regress` returns 1 on any of these.
    Regress,
    /// No binary currently published for this name+arch in the repo.
    New,
    /// No cached repo metadata for the profile, or the spec is
    /// missing required preamble fields.
    Skipped,
}

fn render(report: &UpgradeSimReport, format: OutputFormat, style: &Style) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match format {
        OutputFormat::Human => {
            for row in &report.rows {
                let header = style.header(&format!("== {} / {} ==", row.spec_path, row.profile));
                writeln!(out, "{header}")?;
                let tag = match row.verdict {
                    // Reuse the matrix palette: green = healthy
                    // upgrade, yellow = same-as-published (worth
                    // checking but not a failure), red = regress,
                    // dim = new package / skipped.
                    Verdict::Upgrade => style.always_tag("UPGRADE"),
                    Verdict::Same => style.conditional_tag("SAME"),
                    Verdict::Regress => style.dead_tag("REGRESS"),
                    Verdict::New => style.dim("NEW"),
                    Verdict::Skipped => style.dim("SKIPPED"),
                };
                writeln!(out, "  verdict:  {tag}")?;
                if let Some(s) = &row.spec_nevra {
                    writeln!(out, "  proposed: {s}")?;
                }
                if let Some(c) = &row.current_nevra {
                    writeln!(out, "  current:  {c}")?;
                }
                if let Some(r) = &row.reason {
                    writeln!(out, "  note:     {}", style.dim(r))?;
                }
            }
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &report)
                .context("serialising matrix upgrade-sim report as JSON")?;
            writeln!(out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_paths() {
        let lower = EVR::new(None, "1.0", "1");
        let same = EVR::new(None, "1.0", "1");
        let higher = EVR::new(None, "2.0", "1");
        assert_eq!(classify(&higher, &lower, 0, 0), Verdict::Upgrade);
        assert_eq!(classify(&same, &lower, 0, 0), Verdict::Same);
        assert_eq!(classify(&lower, &higher, 0, 0), Verdict::Regress);

        // Epoch dropped (current > 0, proposed = 0): REGRESS even if
        // version+release otherwise advance.
        assert_eq!(classify(&higher, &lower, 0, 1), Verdict::Regress);

        // Epoch raised: UPGRADE even if version+release stay the same.
        assert_eq!(classify(&lower, &same, 2, 1), Verdict::Upgrade);
    }

    #[test]
    fn verdict_serialises_as_snake_case() {
        assert_eq!(serde_json::to_string(&Verdict::Upgrade).unwrap(), "\"upgrade\"");
        assert_eq!(serde_json::to_string(&Verdict::Same).unwrap(), "\"same\"");
        assert_eq!(serde_json::to_string(&Verdict::Regress).unwrap(), "\"regress\"");
        assert_eq!(serde_json::to_string(&Verdict::New).unwrap(), "\"new\"");
        assert_eq!(serde_json::to_string(&Verdict::Skipped).unwrap(), "\"skipped\"");
    }

    #[test]
    fn exit_code_policy() {
        let mut report = UpgradeSimReport::default();
        report.rows.push(UpgradeRow {
            spec_path: "x".into(),
            profile: "p".into(),
            verdict: Verdict::Regress,
            spec_nevra: None,
            current_nevra: None,
            reason: None,
        });
        // ExitCode lacks a stable u8-extraction API on stable Rust;
        // its `Debug` impl is platform-specific (`ExitCode(unix_exit_status(N))`
        // on Linux, where `N` is the literal status). Asserting on
        // the underlying byte via the debug form keeps the test
        // platform-portable without pulling in `std::os::unix`.
        let regress_dbg = format!("{:?}", report.exit_code(FailOn::Regress));
        let never_dbg = format!("{:?}", report.exit_code(FailOn::Never));
        assert!(regress_dbg.contains('1'), "expected non-zero exit, got {regress_dbg}");
        assert!(never_dbg.contains('0'), "expected zero exit, got {never_dbg}");

        // SAME / NEW / SKIPPED must NOT fail under `regress` policy.
        for v in [Verdict::Same, Verdict::New, Verdict::Skipped, Verdict::Upgrade] {
            let mut r = UpgradeSimReport::default();
            r.rows.push(UpgradeRow {
                spec_path: "x".into(),
                profile: "p".into(),
                verdict: v,
                spec_nevra: None,
                current_nevra: None,
                reason: None,
            });
            let dbg = format!("{:?}", r.exit_code(FailOn::Regress));
            assert!(
                dbg.contains('0'),
                "{v:?} must not fail `--fail-on regress`, got {dbg}"
            );
        }
    }
}
