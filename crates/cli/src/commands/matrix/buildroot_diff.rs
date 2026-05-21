//! `matrix buildroot diff` — closure-vs-closure comparison.
//!
//! For one spec and exactly two profiles, run the full buildroot
//! solver against each profile's repository cache and diff the
//! resulting closures:
//!
//! * **common** — packages pinned by both profiles (same NEVRA).
//! * **only-A** — packages pinned by the first profile but not the
//!   second (or pinned with a different NEVRA on the other side).
//! * **only-B** — symmetric counterpart.
//!
//! Companion to `matrix buildroot solve`: where solve answers "what
//! would the chroot look like?", diff answers "how does that chroot
//! shape change across distros?" — i.e. the per-platform variance
//! operators usually grep for after a BR refactor lands.
//!
//! When either side fails to solve (no cached snapshot, unsatisfiable
//! BRs) the report surfaces the failure verdict per side rather than
//! attempting a meaningless diff against an empty closure. Exit code:
//!
//! * `--fail-on never` always returns 0.
//! * `--fail-on any-diff` (default) returns 1 if the closures
//!   actually differ (non-empty only-A or only-B).
//! * `--fail-on unsat` returns 1 if either side failed to solve.

use std::collections::{BTreeMap, BTreeSet};
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
use super::solver_glue::{self, SolveVerdict};
use super::universe::cache_universes;
use super::{MatrixPrepareError, prepare_matrix};
use crate::app::{ColorChoice, MacroDefinesArg};
use crate::commands::repo::RepoArgs;

#[derive(Debug, Args)]
pub struct DiffOpts {
    /// Spec file to evaluate.
    pub path: PathBuf,

    /// Exactly two profiles to compare. Comma-separated. Same shape
    /// as `matrix diff` — diff is binary by design.
    #[arg(
        long = "profiles",
        value_name = "A,B",
        value_delimiter = ',',
        required = true
    )]
    pub profiles: Vec<String>,

    /// Output format.
    #[arg(long, default_value = "human", value_enum)]
    pub format: OutputFormat,

    /// Exit-code policy.
    #[arg(long, default_value = "any-diff", value_enum)]
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
#[clap(rename_all = "kebab-case")]
pub enum FailOn {
    Never,
    /// Any non-empty only-A or only-B bucket.
    AnyDiff,
    /// Either side returned Unsatisfiable or Skipped.
    Unsat,
}

pub fn run(opts: DiffOpts, config_path: Option<&Path>, color: ColorChoice) -> Result<ExitCode> {
    let style = Style::new(color);

    if opts.profiles.len() != 2 {
        eprintln!(
            "error: matrix buildroot diff requires exactly two profiles (got {})",
            opts.profiles.len()
        );
        return Ok(ExitCode::from(2));
    }
    if opts.profiles[0] == opts.profiles[1] {
        eprintln!(
            "error: matrix buildroot diff requires two distinct profiles \
             (got `{}` twice)",
            opts.profiles[0]
        );
        return Ok(ExitCode::from(2));
    }

    let ctx = match prepare_matrix(config_path, None, &opts.profiles, &opts.defines) {
        Ok(c) => c,
        Err(MatrixPrepareError::UserInputReported) => return Ok(ExitCode::from(2)),
        Err(MatrixPrepareError::Internal(e)) => return Err(e),
    };
    // `prepare_matrix` preserves the order operators typed on the
    // CLI, so `targets[0]` is always the "A" side of the diff. If
    // resolution dropped one (typo, missing profile) the early-exit
    // gives a clear error instead of an out-of-bounds panic.
    if ctx.resolved.targets.len() != 2 {
        eprintln!(
            "error: profile resolution returned {} targets (expected 2); \
             check the names against `.rpmspec.toml`",
            ctx.resolved.targets.len()
        );
        return Ok(ExitCode::from(2));
    }

    let cache_root = opts.repo_args.resolve_cache_root()?;
    let dirs =
        CacheDirs::ensure(cache_root).context("preparing the repo cache directory layout")?;
    let profiles: Vec<&_> = ctx.resolved.targets.iter().map(|t| &t.profile).collect();
    let universes = cache_universes(&profiles, &dirs)?;

    // Fail-fast on missing cache for either side. The buildroot
    // solve is O(seconds) per side on a large spec, and a diff
    // against a "Skipped" side is just the OK side's full closure
    // — no diff to report. Surfacing the error up-front before any
    // solver work tells the operator exactly what to do (run repo
    // sync) and saves the wait. Other matrix commands accept a
    // partial Skipped row because they're aggregating many
    // (spec × profile) pairs; diff is binary, so one missing side
    // makes the whole call meaningless.
    let missing: Vec<&str> = ctx
        .resolved
        .targets
        .iter()
        .filter(|t| {
            // Missing key OR cached as `None` (cache-miss sentinel)
            // — both count as "no universe to diff against".
            universes
                .get(&t.profile.identity.name)
                .is_none_or(Option::is_none)
        })
        .map(|t| t.profile.identity.name.as_str())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "error: matrix buildroot diff requires cached metadata for both profiles; \
             no cache for: {}. Run `rpm-spec-tool repo sync --profile <NAME> --allow-fetch` \
             for each.",
            missing.join(", ")
        );
        return Ok(ExitCode::from(2));
    }

    let source = std::fs::read_to_string(&opts.path)
        .with_context(|| format!("reading {}", opts.path.display()))?;

    let mut sides: Vec<SideResult> = Vec::with_capacity(2);
    for resolved in &ctx.resolved.targets {
        let profile = &resolved.profile;
        let universe = universes.get(&profile.identity.name).cloned().flatten();
        sides.push(solve_side(&source, profile, universe.as_deref()));
    }
    debug_assert_eq!(sides.len(), 2, "early exit above guarantees two targets");

    let report = build_diff(&opts.path, &sides[0], &sides[1]);
    render(&report, opts.format, &style)?;
    Ok(report.exit_code(opts.fail_on))
}

fn solve_side(source: &str, profile: &Profile, universe: Option<&RepoUniverse>) -> SideResult {
    let profile_name = profile.identity.name.clone();
    let Some(universe) = universe else {
        tracing::debug!(
            profile = %profile_name,
            "no cached universe for profile; diff side will be `Skipped`",
        );
        return SideResult::Skipped {
            profile: profile_name,
        };
    };

    // Repo-side DB failures are infrastructure errors — degrade to
    // `Error`-side rather than `?`-propagating, so a corrupt
    // snapshot for one side doesn't poison the diff against a
    // healthy other side.
    let solution = match solver_glue::solve_for(source, profile, universe) {
        Ok(s) => s,
        Err(e) => {
            return SideResult::Error {
                profile: profile_name,
                message: format!("{e:#}"),
            };
        }
    };

    match solution {
        Solution::Ok(closure) => SideResult::Ok {
            profile: profile_name,
            // `BTreeMap` keyed by package NAME so diff buckets a
            // package update (foo-1.0 vs foo-2.0) as "same name,
            // different version" rather than two unrelated rows.
            closure: closure
                .packages
                .into_iter()
                .map(|n| (n.name.to_string(), n))
                .collect(),
        },
        Solution::Unsatisfiable(_) => SideResult::Unsat {
            profile: profile_name,
        },
    }
}

#[derive(Debug)]
enum SideResult {
    Ok {
        profile: String,
        closure: BTreeMap<String, NEVRA>,
    },
    Unsat {
        profile: String,
    },
    Skipped {
        profile: String,
    },
    /// Per-row infrastructure failure (corrupt snapshot, SQLite
    /// error). Surfaces in the report with the error chain in
    /// `message` so JSON consumers see the cause — distinct from
    /// `Skipped` (which is the absence of cache) so CI can alert
    /// on real breakage.
    Error {
        profile: String,
        message: String,
    },
}

impl SideResult {
    fn profile(&self) -> &str {
        match self {
            Self::Ok { profile, .. }
            | Self::Unsat { profile }
            | Self::Skipped { profile }
            | Self::Error { profile, .. } => profile,
        }
    }
}

fn build_diff(spec_path: &Path, a: &SideResult, b: &SideResult) -> DiffReport {
    let (closure_a, closure_b) = match (a, b) {
        (SideResult::Ok { closure: ca, .. }, SideResult::Ok { closure: cb, .. }) => (ca, cb),
        _ => {
            // At least one side failed; emit a degenerate report that
            // carries the per-side verdicts (and Error messages) so
            // the operator sees WHY and JSON consumers get a uniform
            // shape.
            return DiffReport {
                spec_path: spec_path.display().to_string(),
                profile_a: a.profile().to_string(),
                profile_b: b.profile().to_string(),
                verdict_a: verdict_for(a),
                verdict_b: verdict_for(b),
                reason_a: error_message(a),
                reason_b: error_message(b),
                common: Vec::new(),
                only_a: Vec::new(),
                only_b: Vec::new(),
                version_only: Vec::new(),
            };
        }
    };

    let names_a: BTreeSet<&str> = closure_a.keys().map(String::as_str).collect();
    let names_b: BTreeSet<&str> = closure_b.keys().map(String::as_str).collect();

    let mut common = Vec::new();
    let mut version_only = Vec::new();
    for name in names_a.intersection(&names_b) {
        // SAFETY-of-intent: both maps contain this name (set intersection).
        let nevra_a = closure_a.get(*name).expect("intersection invariant");
        let nevra_b = closure_b.get(*name).expect("intersection invariant");
        if nevra_a == nevra_b {
            common.push(nevra_a.clone());
        } else {
            version_only.push(VersionDiff {
                name: (*name).to_string(),
                nevra_a: nevra_a.clone(),
                nevra_b: nevra_b.clone(),
            });
        }
    }
    let only_a: Vec<NEVRA> = names_a
        .difference(&names_b)
        .map(|n| closure_a.get(*n).expect("difference invariant").clone())
        .collect();
    let only_b: Vec<NEVRA> = names_b
        .difference(&names_a)
        .map(|n| closure_b.get(*n).expect("difference invariant").clone())
        .collect();

    DiffReport {
        spec_path: spec_path.display().to_string(),
        profile_a: a.profile().to_string(),
        profile_b: b.profile().to_string(),
        verdict_a: SolveVerdict::Ok,
        verdict_b: SolveVerdict::Ok,
        reason_a: None,
        reason_b: None,
        common,
        only_a,
        only_b,
        version_only,
    }
}

fn verdict_for(side: &SideResult) -> SolveVerdict {
    match side {
        SideResult::Ok { .. } => SolveVerdict::Ok,
        SideResult::Unsat { .. } => SolveVerdict::Unsat,
        SideResult::Skipped { .. } => SolveVerdict::Skipped,
        SideResult::Error { .. } => SolveVerdict::Error,
    }
}

/// Per-side context: the Error variant's chain text (so JSON
/// consumers see *why* a side failed), else None.
fn error_message(side: &SideResult) -> Option<String> {
    match side {
        SideResult::Error { message, .. } => Some(message.clone()),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
struct DiffReport {
    spec_path: String,
    profile_a: String,
    profile_b: String,
    verdict_a: SolveVerdict,
    verdict_b: SolveVerdict,
    /// Per-side detail for non-Ok verdicts (typically the Error
    /// variant's chain text). `None` when the side resolved cleanly
    /// or is Unsat/Skipped without further context.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_b: Option<String>,
    /// Packages pinned by both sides at the same NEVRA.
    common: Vec<NEVRA>,
    /// Packages pinned only by side A.
    only_a: Vec<NEVRA>,
    /// Packages pinned only by side B.
    only_b: Vec<NEVRA>,
    /// Same-named packages with different NEVRAs (typical case:
    /// distro-version drift like `glibc-2.34` vs `glibc-2.38`).
    version_only: Vec<VersionDiff>,
}

impl DiffReport {
    fn exit_code(&self, fail_on: FailOn) -> ExitCode {
        match fail_on {
            FailOn::Never => ExitCode::SUCCESS,
            FailOn::AnyDiff => {
                if self.only_a.is_empty() && self.only_b.is_empty() && self.version_only.is_empty()
                {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            FailOn::Unsat => {
                if matches!(self.verdict_a, SolveVerdict::Ok)
                    && matches!(self.verdict_b, SolveVerdict::Ok)
                {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct VersionDiff {
    name: String,
    nevra_a: NEVRA,
    nevra_b: NEVRA,
}

fn render(report: &DiffReport, format: OutputFormat, style: &Style) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match format {
        OutputFormat::Human => {
            let header = style.header(&format!("== {} ==", report.spec_path));
            writeln!(out, "{header}")?;
            writeln!(
                out,
                "  {} (A) vs {} (B)",
                style.header(&report.profile_a),
                style.header(&report.profile_b),
            )?;
            for (label, verdict) in [
                (&report.profile_a, report.verdict_a),
                (&report.profile_b, report.verdict_b),
            ] {
                if verdict != SolveVerdict::Ok {
                    let tag = match verdict {
                        SolveVerdict::Unsat => style.dead_tag("UNSAT"),
                        SolveVerdict::Skipped => style.dim("SKIPPED"),
                        SolveVerdict::Error => style.dead_tag("ERROR"),
                        SolveVerdict::Ok => unreachable!("filtered above"),
                    };
                    writeln!(out, "  {tag}  {label} (no closure to diff against)",)?;
                }
            }
            if report.verdict_a != SolveVerdict::Ok || report.verdict_b != SolveVerdict::Ok {
                // Degraded case — nothing more to render.
                return Ok(());
            }
            writeln!(
                out,
                "  common: {} packages",
                style.always_tag(&report.common.len().to_string()),
            )?;
            render_bucket(&mut out, style, "only A", &report.only_a)?;
            render_bucket(&mut out, style, "only B", &report.only_b)?;
            if !report.version_only.is_empty() {
                writeln!(
                    out,
                    "  {}: {} packages",
                    style.conditional_tag("version drift"),
                    report.version_only.len(),
                )?;
                for entry in &report.version_only {
                    writeln!(
                        out,
                        "    {}: {} → {}",
                        entry.name, entry.nevra_a, entry.nevra_b,
                    )?;
                }
            }
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &report)
                .context("serialising matrix buildroot diff report as JSON")?;
            writeln!(out)?;
        }
    }
    Ok(())
}

fn render_bucket(
    out: &mut impl Write,
    style: &Style,
    label: &str,
    items: &[NEVRA],
) -> std::io::Result<()> {
    writeln!(
        out,
        "  {}: {} packages",
        style.conditional_tag(label),
        items.len(),
    )?;
    // Cap output at 20 entries to keep terminals readable; JSON
    // always carries the full list.
    const HUMAN_LIMIT: usize = 20;
    let take = items.len().min(HUMAN_LIMIT);
    for nevra in items.iter().take(take) {
        writeln!(out, "    {nevra}")?;
    }
    if items.len() > take {
        writeln!(
            out,
            "    {} (truncated — pass --format json for the full list)",
            style.dim(&format!("... +{} more", items.len() - take)),
        )?;
    }
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

    fn ok_side(profile: &str, pkgs: &[(&str, &str)]) -> SideResult {
        SideResult::Ok {
            profile: profile.to_string(),
            closure: pkgs
                .iter()
                .map(|(n, v)| (n.to_string(), nevra(n, v)))
                .collect(),
        }
    }

    #[test]
    fn diff_buckets_common_only_and_version_drift() {
        let a = ok_side("a", &[("bash", "5.1"), ("glibc", "2.34"), ("gcc", "11")]);
        let b = ok_side(
            "b",
            &[("bash", "5.1"), ("glibc", "2.38"), ("python", "3.12")],
        );
        let report = build_diff(Path::new("/x.spec"), &a, &b);
        assert_eq!(report.common.len(), 1, "{:?}", report.common);
        assert_eq!(report.common[0].name.as_ref(), "bash");
        assert_eq!(report.only_a.len(), 1);
        assert_eq!(report.only_a[0].name.as_ref(), "gcc");
        assert_eq!(report.only_b.len(), 1);
        assert_eq!(report.only_b[0].name.as_ref(), "python");
        assert_eq!(report.version_only.len(), 1);
        assert_eq!(report.version_only[0].name, "glibc");
    }

    #[test]
    fn exit_codes() {
        let a = ok_side("a", &[("bash", "5.1")]);
        let b = ok_side("b", &[("bash", "5.1")]);
        let identical = build_diff(Path::new("/x.spec"), &a, &b);
        // Identical closures → exit 0 even under any-diff.
        let dbg_id = format!("{:?}", identical.exit_code(FailOn::AnyDiff));
        assert!(dbg_id.contains('0'), "{dbg_id}");

        let differ = ok_side("b", &[("bash", "5.1"), ("extra", "1")]);
        let differing = build_diff(Path::new("/x.spec"), &a, &differ);
        let dbg_diff = format!("{:?}", differing.exit_code(FailOn::AnyDiff));
        assert!(dbg_diff.contains('1'), "{dbg_diff}");
        let dbg_never = format!("{:?}", differing.exit_code(FailOn::Never));
        assert!(dbg_never.contains('0'), "{dbg_never}");
    }

    #[test]
    fn unsat_side_yields_degraded_report() {
        let a = ok_side("a", &[("bash", "5.1")]);
        let b = SideResult::Unsat {
            profile: "b".to_string(),
        };
        let report = build_diff(Path::new("/x.spec"), &a, &b);
        assert_eq!(report.verdict_a, SolveVerdict::Ok);
        assert_eq!(report.verdict_b, SolveVerdict::Unsat);
        assert!(
            report.common.is_empty(),
            "no diff possible against unsat side"
        );
        let dbg = format!("{:?}", report.exit_code(FailOn::Unsat));
        assert!(dbg.contains('1'), "{dbg}");
    }

    #[test]
    fn version_drift_serialises() {
        let entry = VersionDiff {
            name: "glibc".into(),
            nevra_a: nevra("glibc", "2.34"),
            nevra_b: nevra("glibc", "2.38"),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"glibc\""), "{json}");
        assert!(json.contains("\"2.34\""), "{json}");
    }
}
