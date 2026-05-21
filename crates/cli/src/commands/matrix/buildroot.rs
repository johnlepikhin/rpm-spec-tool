//! `matrix buildroot solve` — full buildroot closure for a spec.
//!
//! For each (spec × profile) in the target set, ask the resolver to
//! pin every package that would land in the build chroot:
//! `profile.buildroot.base_packages` + the platform's
//! `implicit_buildrequires` + the spec's declared `BuildRequires:`.
//!
//! On success: list of pinned NEVRAs plus total installed size. On
//! failure: the unsat core (which dep had no provider, which conflict
//! chain blocked it).
//!
//! Output formats:
//! * `--format human` (default) — coloured per-(spec × profile)
//!   block with the verdict tag, the closure (capped), and unsat
//!   details when applicable.
//! * `--format json` — structured for CI consumption.
//!
//! Exit code:
//! * `--fail-on never` — always 0.
//! * `--fail-on unsat` (default) — 1 if any (spec × profile) yields
//!   `Unsatisfiable`.

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

/// Maximum number of pinned packages to render in the human output.
/// JSON always carries the full list.
const HUMAN_CLOSURE_LIMIT: usize = 20;

#[derive(Debug, Args)]
pub struct SolveOpts {
    /// Spec file(s) to solve. Each path is resolved separately
    /// against every member of the target set, so passing N specs
    /// × M profiles produces N×M result rows.
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

    /// Exit-code policy. `never` always returns 0; `unsat` (default)
    /// returns 1 when any spec × profile yields `Unsatisfiable`.
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

pub fn run(opts: SolveOpts, config_path: Option<&Path>, color: ColorChoice) -> Result<ExitCode> {
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
    let dirs =
        CacheDirs::ensure(cache_root).context("preparing the repo cache directory layout")?;
    let profiles: Vec<&_> = ctx.resolved.targets.iter().map(|t| &t.profile).collect();
    let universes = cache_universes(&profiles, &dirs)?;

    let mut report = SolveReport::default();
    for spec_path in &opts.paths {
        let source = std::fs::read_to_string(spec_path)
            .with_context(|| format!("reading {}", spec_path.display()))?;
        for resolved in &ctx.resolved.targets {
            let profile = &resolved.profile;
            let universe = universes.get(&profile.identity.name).cloned().flatten();
            let row = solve_one(spec_path, &source, profile, universe.as_deref())?;
            report.rows.push(row);
        }
    }

    render(&report, opts.format, &style)?;
    Ok(report.exit_code(opts.fail_on))
}

fn solve_one(
    spec_path: &Path,
    source: &str,
    profile: &Profile,
    universe: Option<&RepoUniverse>,
) -> Result<SolveRow> {
    let header_spec = spec_path.display().to_string();
    let profile_name = profile.identity.name.clone();
    let Some(universe) = universe else {
        return Ok(SolveRow {
            spec_path: header_spec,
            profile: profile_name,
            verdict: SolveVerdict::Skipped,
            closure_size: 0,
            install_size_total: 0,
            closure: Vec::new(),
            unsatisfied: Vec::new(),
            conflicts: Vec::new(),
        });
    };

    let solution = solver_glue::solve_for(source, profile, universe)
        .with_context(|| format!("solver for {}", spec_path.display()))?;

    Ok(match solution {
        Solution::Ok(closure) => {
            let install_size_total = closure.install_size_total;
            let pinned: Vec<String> = closure.packages.iter().map(NEVRA::to_string).collect();
            SolveRow {
                spec_path: header_spec,
                profile: profile_name,
                verdict: SolveVerdict::Ok,
                closure_size: pinned.len(),
                install_size_total,
                closure: pinned,
                unsatisfied: Vec::new(),
                conflicts: Vec::new(),
            }
        }
        Solution::Unsatisfiable(core) => {
            let deduped = solver_glue::dedup_unsat_core(&core);
            SolveRow {
                spec_path: header_spec,
                profile: profile_name,
                verdict: SolveVerdict::Unsat,
                closure_size: 0,
                install_size_total: 0,
                closure: Vec::new(),
                unsatisfied: deduped.unsatisfied,
                conflicts: deduped.conflicts,
            }
        }
    })
}

/// Render the `UNSAT` block with categorisation, prose
/// explanation, and concrete remediation hints. Splits the raw
/// `unmet` list into four buckets by surface form so an operator
/// reading the report can see at a glance whether the failure is
/// "wrong distro family" (lots of `lib64*`, `openldap2-devel`),
/// "tool missing" (file paths), "shared library missing" (`.so.N`
/// sonames), or "version constraint" — and direct their next
/// step accordingly.
fn render_unsat<W: Write>(out: &mut W, row: &SolveRow, style: &Style) -> std::io::Result<()> {
    let unmet_total = row.unsatisfied.len();
    let conflict_total = row.conflicts.len();
    writeln!(
        out,
        "  {} buildroot is unsatisfiable ({} unmet, {} conflict{})",
        style.dead_tag("UNSAT"),
        unmet_total,
        conflict_total,
        if conflict_total == 1 { "" } else { "s" },
    )?;

    let buckets = bucket_unmet(&row.unsatisfied);
    writeln!(out)?;
    writeln!(
        out,
        "  {}",
        style.header(
            "Reason: every dep listed below has no provider in the configured repos for this profile."
        ),
    )?;
    writeln!(
        out,
        "  {}",
        style.dim("Common causes: spec was written for a different distro family (Mandriva-style"),
    )?;
    writeln!(
        out,
        "  {}",
        style.dim(
            "`lib64*-devel`/`openldap2-devel`, ROSA `llvm-toolset-*`); newer compilers not in"
        ),
    )?;
    writeln!(
        out,
        "  {}",
        style.dim(
            "the repos (`clang5/7/15`); missing file owners; or unsatisfied version constraints."
        ),
    )?;
    writeln!(out)?;

    render_bucket(
        out,
        style,
        "file-path requirements",
        "usually need the package owning the file declared as `BuildRequires:`",
        &buckets.file_paths,
    )?;
    render_bucket(
        out,
        style,
        "shared-library sonames",
        "declare the shared-library-providing package as `BuildRequires:`",
        &buckets.sonames,
    )?;
    render_bucket(
        out,
        style,
        "versioned constraints",
        "repo has no release matching the version constraint",
        &buckets.versioned,
    )?;
    render_bucket(
        out,
        style,
        "missing package names",
        "no package by this name; spec may target a different distro",
        &buckets.plain,
    )?;

    if !row.conflicts.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "  {} ({}) — two packages both declare `Conflicts:` against each other:",
            style.header("conflict chains"),
            row.conflicts.len(),
        )?;
        for c in &row.conflicts {
            writeln!(
                out,
                "    {} ↔ {}  {}",
                c.cause,
                c.victim,
                style.dim(&format!("via {}", c.via)),
            )?;
            if !c.cause_chain.is_empty() {
                let chain: Vec<String> = c.cause_chain.iter().map(NEVRA::to_string).collect();
                writeln!(
                    out,
                    "      {} {}",
                    style.dim(&format!("{} pulled in by:", c.cause)),
                    style.dim(&chain.join(" ← ")),
                )?;
            }
            if !c.victim_chain.is_empty() {
                let chain: Vec<String> = c.victim_chain.iter().map(NEVRA::to_string).collect();
                writeln!(
                    out,
                    "      {} {}",
                    style.dim(&format!("{} pulled in by:", c.victim)),
                    style.dim(&chain.join(" ← ")),
                )?;
            }
        }
        writeln!(
            out,
            "  {}",
            style.dim(
                "Resolution: pin one provider explicitly (e.g. add the chosen one to `base_packages`),"
            ),
        )?;
        writeln!(
            out,
            "  {}",
            style.dim("or drop the spec dep that pulls the conflicting alternative."),
        )?;
    }

    Ok(())
}

/// Emit one labelled unmet bucket with provenance under each entry.
/// Skips when the bucket is empty to avoid a section header with no
/// content. `required_by` is rendered as `nearest ← root` so the
/// operator's eye lands on the package whose `Requires:` line
/// actually fails first.
fn render_bucket<W: Write>(
    out: &mut W,
    style: &Style,
    label: &str,
    hint: &str,
    items: &[&UnmetEntry],
) -> std::io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "  {} ({}) — {}:",
        style.header(label),
        items.len(),
        hint,
    )?;
    for item in items {
        writeln!(out, "    {}", item.dep)?;
        if item.required_by.is_empty() {
            writeln!(
                out,
                "      {}",
                style.dim("required by: the spec / buildroot baseline")
            )?;
        } else {
            let chain: Vec<String> = item.required_by.iter().map(NEVRA::to_string).collect();
            writeln!(
                out,
                "      {} {}",
                style.dim("required by:"),
                style.dim(&chain.join(" ← ")),
            )?;
        }
    }
    Ok(())
}

/// Categorise an unmet capability by its surface form, so the
/// renderer can group "shared-library missing" together separately
/// from "wrong distro package name". Heuristic — meant to direct
/// the operator's eye, not be exhaustive.
#[derive(Default)]
struct UnmetBuckets<'a> {
    file_paths: Vec<&'a UnmetEntry>,
    sonames: Vec<&'a UnmetEntry>,
    versioned: Vec<&'a UnmetEntry>,
    plain: Vec<&'a UnmetEntry>,
}

fn bucket_unmet(items: &[UnmetEntry]) -> UnmetBuckets<'_> {
    let mut b = UnmetBuckets::default();
    for entry in items {
        let raw = entry.dep.as_str();
        // Heuristic order matters: `.so.N` paths are file paths
        // AND sonames; classify as soname (clearer intent for the
        // operator). Versioned forms contain ` op `, e.g. `cmake
        // >= 3.28` or `libomp = 10.0.0`.
        if raw.contains(".so.") || raw.contains(".so ") || raw.ends_with(".so") {
            b.sonames.push(entry);
        } else if raw.starts_with('/') {
            b.file_paths.push(entry);
        } else if raw.contains(" = ")
            || raw.contains(" >= ")
            || raw.contains(" <= ")
            || raw.contains(" > ")
            || raw.contains(" < ")
        {
            b.versioned.push(entry);
        } else {
            b.plain.push(entry);
        }
    }
    b
}

/// Render a byte count in the largest unit ≤ value, with one
/// decimal place above KB. JSON keeps the raw u64 — this is the
/// human view only.
fn format_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

#[derive(Debug, Default, Serialize)]
struct SolveReport {
    rows: Vec<SolveRow>,
}

impl SolveReport {
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
struct SolveRow {
    spec_path: String,
    profile: String,
    verdict: SolveVerdict,
    closure_size: usize,
    install_size_total: u64,
    closure: Vec<String>,
    unsatisfied: Vec<UnmetEntry>,
    conflicts: Vec<ConflictEntry>,
}

fn render(report: &SolveReport, format: OutputFormat, style: &Style) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match format {
        OutputFormat::Human => {
            for row in &report.rows {
                let header = style.header(&format!("== {} / {} ==", row.spec_path, row.profile));
                writeln!(out, "{header}")?;
                match row.verdict {
                    SolveVerdict::Skipped => {
                        let hint = style.dim(
                            "(no cached repo metadata for this profile — run `rpm-spec-tool repo sync`; solver skipped)",
                        );
                        writeln!(out, "  {hint}")?;
                    }
                    SolveVerdict::Ok => {
                        writeln!(
                            out,
                            "  {} closure: {} packages, {} installed",
                            style.always_tag("OK"),
                            row.closure_size,
                            format_bytes(row.install_size_total),
                        )?;
                        let take = row.closure_size.min(HUMAN_CLOSURE_LIMIT);
                        for nevra in row.closure.iter().take(take) {
                            writeln!(out, "    {nevra}")?;
                        }
                        if row.closure_size > take {
                            writeln!(
                                out,
                                "    {} (truncated — pass --format json for the full list)",
                                style.dim(&format!("... +{} more", row.closure_size - take)),
                            )?;
                        }
                    }
                    SolveVerdict::Unsat => {
                        render_unsat(&mut out, row, style)?;
                    }
                    SolveVerdict::Error => {
                        // `matrix buildroot solve` currently
                        // `?`-propagates infra errors at the `run`
                        // level rather than surfacing per-row, so
                        // this arm is unreachable today — but the
                        // exhaustive match guards against the day a
                        // future refactor adopts the per-row Error
                        // pattern used by buildroot diff / deps
                        // explain.
                        let hint =
                            style.dead_tag("ERROR  solver infrastructure failure (see logs)");
                        writeln!(out, "  {hint}")?;
                    }
                }
            }
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &report)
                .context("serialising matrix buildroot solve report as JSON")?;
            writeln!(out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Inline coverage for the pure helpers — `bucket_unmet` classifies
    //! unmet deps into four user-meaningful groups, and `Verdict`
    //! serialises into stable JSON tokens. Both are easy to break in
    //! a refactor and tedious to catch via integration tests.
    use super::*;

    fn unmet(dep: &str) -> UnmetEntry {
        UnmetEntry {
            dep: dep.to_string(),
            required_by: Vec::new(),
        }
    }

    #[test]
    fn bucket_unmet_routes_each_dep_shape() {
        let items = vec![
            unmet("libfoo.so.6"),
            unmet("libbar.so.6(GLIBC_2.34)"),
            unmet("/usr/bin/qux"),
            unmet("cmake >= 3.26"),
            unmet("python3-foo = 1.2-3"),
            unmet("bash"),
        ];
        let b = bucket_unmet(&items);
        assert_eq!(
            b.sonames.len(),
            2,
            "soname bucket should win over file_paths"
        );
        assert_eq!(b.file_paths.len(), 1);
        assert_eq!(b.versioned.len(), 2);
        assert_eq!(b.plain.len(), 1);
    }

    #[test]
    fn format_bytes_uses_largest_fitting_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
    }
}
