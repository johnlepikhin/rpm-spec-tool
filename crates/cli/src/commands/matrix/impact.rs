//! `matrix impact` — per-profile dep-set delta between two git
//! revisions of a single spec.
//!
//! PR-review workflow: "this commit touches `foo.spec` — which
//! platforms are materially affected and which deps moved?". Reads
//! both revisions of the spec via `git show REV:relpath`, runs the
//! same dep-set extraction as `matrix diff` on each side, and
//! reports per-(profile, tag) `added` / `removed` / `unchanged`
//! buckets.
//!
//! No new heavy dependency: the git invocation is a plain
//! `std::process::Command` shell-out. The CLI fails with exit code
//! 2 when git is missing, the revisions are unresolvable, or the
//! repository can't be located.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{ImpactReport, session::parse};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Per-profile table with added / removed / unchanged counts.
    Human,
    /// Structured JSON for tooling consumption.
    Json,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
pub struct ImpactOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    /// Git revision the spec content is read from for the "before"
    /// side. Accepted in any form `git show REV:path` accepts —
    /// commit SHAs, branches, tags, `HEAD~3`, etc.
    #[arg(long = "from", value_name = "REV")]
    pub from: String,

    /// Git revision for the "after" side. Pass `HEAD` (or
    /// `--to` omitted-with-default) for "what's about to ship?"
    /// or a concrete SHA for a frozen comparison.
    #[arg(long = "to", value_name = "REV", default_value = "HEAD")]
    pub to: String,

    /// Path to `git` binary. Useful when the CLI runs in a
    /// container that ships git under a non-standard name. Default
    /// `git` honours `PATH` lookup.
    #[arg(long = "git-cmd", value_name = "PATH", default_value = "git")]
    pub git_cmd: String,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,

    #[command(flatten)]
    pub bcond: crate::app::BcondOverridesArg,
}

pub(super) fn run(opts: ImpactOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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
            "error: matrix impact operates on exactly one spec at a time \
             (got {} sources)",
            sources.len()
        );
        return Ok(ExitCode::from(2));
    }
    let source = sources
        .into_iter()
        .next()
        .expect("io::read_sources guarantees >= 1 source");
    if source.is_stdin {
        eprintln!(
            "error: matrix impact reads spec content from git revisions; \
             cannot operate on stdin (pass a spec file path)"
        );
        return Ok(ExitCode::from(2));
    }

    // Locate the spec's git repo + the spec's path relative to the
    // git root. Both git invocations below operate against that
    // root so relative-path semantics line up regardless of the
    // CLI's cwd.
    let spec_abs = match source.path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot canonicalise spec path {}: {e}", source.path.display());
            return Ok(ExitCode::from(2));
        }
    };
    let repo_root = match git_repo_root(&opts.git_cmd, &spec_abs) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(ExitCode::from(2));
        }
    };
    let spec_rel = match spec_abs.strip_prefix(&repo_root) {
        Ok(p) => p.to_path_buf(),
        Err(_) => {
            eprintln!(
                "error: spec {} is outside git repository {}",
                spec_abs.display(),
                repo_root.display()
            );
            return Ok(ExitCode::from(2));
        }
    };

    // Resolve both revs up-front. A typo'd SHA must surface as a hard
    // error here rather than later, because git's "path does not
    // exist in REV" message is emitted identically for "rev unknown"
    // and "rev valid but file missing" — without this pre-check we'd
    // silently treat a typo as "spec added in this PR" and produce
    // misleading impact output.
    for rev in [&opts.from, &opts.to] {
        if let Err(e) = git_resolve_rev(&opts.git_cmd, &repo_root, rev) {
            eprintln!("error: revision `{rev}`: {e:#}");
            return Ok(ExitCode::from(2));
        }
    }

    let from_bytes = match git_show(&opts.git_cmd, &repo_root, &opts.from, &spec_rel) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: reading spec at {}: {e:#}", opts.from);
            return Ok(ExitCode::from(2));
        }
    };
    let to_bytes = match git_show(&opts.git_cmd, &repo_root, &opts.to, &spec_rel) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: reading spec at {}: {e:#}", opts.to);
            return Ok(ExitCode::from(2));
        }
    };

    let from_parsed = parse(&from_bytes);
    let to_parsed = parse(&to_bytes);
    surface_parser_diagnostics("from", &opts.from, &from_parsed);
    surface_parser_diagnostics("to", &opts.to, &to_parsed);

    let report = ImpactReport::compute(
        &from_parsed.spec,
        &to_parsed.spec,
        &resolved,
        &opts.bcond.to_overrides(),
    );

    match opts.format {
        OutputFormat::Human => render_human(&source, &report, &resolved, &opts.from, &opts.to)?,
        OutputFormat::Json => render_json(&source, &report, &resolved, &opts.from, &opts.to)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn surface_parser_diagnostics(
    side: &str,
    rev: &str,
    parsed: &rpm_spec_analyzer::ParseOutcome,
) {
    if parsed.parser_diagnostics.is_empty() {
        return;
    }
    let total = parsed.parser_diagnostics.len();
    let errors = parsed
        .parser_diagnostics
        .iter()
        .filter(|d| matches!(d.severity, rpm_spec_analyzer::ParserSeverity::Error))
        .count();
    eprintln!(
        "warning: {side}-side spec ({rev}) produced {total} parser \
         diagnostic(s) ({errors} error-level) — the impact report is \
         computed against the recovered AST and may be incomplete"
    );
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Verify that `rev` resolves to a commit in `repo`. Without this
/// pre-check, a typo'd SHA looks identical to "the spec did not exist
/// at that revision" because `git show REV:path` emits the same
/// "path … does not exist in REV" message in both cases — so we'd
/// silently treat a typo as "added in this PR" and produce misleading
/// impact output.
///
/// `^{commit}` peels through tags/annotated refs so symbolic names
/// like `v1.2.3` resolve cleanly. Plain hashes pass through unchanged.
fn git_resolve_rev(git_cmd: &str, repo: &Path, rev: &str) -> Result<()> {
    let arg = format!("{rev}^{{commit}}");
    let out = Command::new(git_cmd)
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &arg])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running `{git_cmd} rev-parse {arg}`"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let trimmed = stderr.trim();
        if trimmed.is_empty() {
            // `--quiet` suppresses stderr for the common "no such rev"
            // case; substitute a useful message so the operator can
            // distinguish a typo from a wider git failure.
            anyhow::bail!("unknown revision (no commit named `{rev}`)");
        }
        anyhow::bail!("{trimmed}");
    }
    Ok(())
}

/// Resolve the git repository root containing `spec_abs`.
fn git_repo_root(git_cmd: &str, spec_abs: &Path) -> Result<PathBuf> {
    let spec_dir = spec_abs
        .parent()
        .with_context(|| format!("spec path has no parent: {}", spec_abs.display()))?;
    let out = Command::new(git_cmd)
        .arg("-C")
        .arg(spec_dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running `{git_cmd} rev-parse --show-toplevel`"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "not a git repository: {} ({})",
            spec_dir.display(),
            stderr.trim()
        );
    }
    let root = String::from_utf8(out.stdout)
        .with_context(|| "git rev-parse produced non-UTF-8 path")?
        .trim()
        .to_string();
    Ok(PathBuf::from(root))
}

/// Run `git -C repo show REV:rel_path` and return the spec content
/// as a UTF-8 string. Maps git's "file does not exist at REV" exit
/// status to an empty string so callers see "deps added from
/// scratch" cleanly rather than a hard error.
fn git_show(git_cmd: &str, repo: &Path, rev: &str, rel_path: &Path) -> Result<String> {
    // Build `REV:relpath`. Use forward slashes — git uses
    // POSIX-style internal paths regardless of host OS.
    let rel_posix = rel_path
        .to_str()
        .with_context(|| format!("non-UTF-8 path: {}", rel_path.display()))?
        .replace('\\', "/");
    let spec_arg = format!("{rev}:{rel_posix}");
    let out = Command::new(git_cmd)
        .arg("-C")
        .arg(repo)
        .args(["show", &spec_arg])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running `{git_cmd} show {spec_arg}`"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Distinguish "rev exists but file missing" (treat as empty
        // spec — added at `to` side) from "rev itself is unknown"
        // (hard error — operator typo). git's wording varies across
        // versions: "does not exist" (older), "exists on disk, but
        // not in" (newer, when path is present in worktree), or "path
        // ... does not exist in" (newer, when path is absent). Match
        // all three to avoid a hard error on a perfectly normal
        // "added in this PR" workflow.
        let file_missing_at_rev = stderr.contains("does not exist")
            || stderr.contains("exists on disk, but not in");
        if file_missing_at_rev {
            tracing::debug!(
                target: "matrix::impact",
                rev = %rev,
                path = %rel_posix,
                "spec file does not exist at revision; treating as empty"
            );
            return Ok(String::new());
        }
        anyhow::bail!("git show failed: {}", stderr.trim());
    }
    String::from_utf8(out.stdout).with_context(|| "git show produced non-UTF-8 content")
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn render_human(
    source: &io::Source,
    report: &ImpactReport,
    target_set: &ResolvedTargetSet,
    from: &str,
    to: &str,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix impact: {from} → {to}, target set `{}` ({}/{} profile(s) affected)",
        target_set.id,
        report.affected_profile_count(),
        target_set.targets.len()
    )?;
    writeln!(out, "## {}", source.display_name())?;

    if report.is_no_change() {
        writeln!(out)?;
        writeln!(out, "  (no change on any profile)")?;
        return Ok(());
    }

    for prof in &report.per_profile {
        writeln!(out)?;
        if prof.is_no_change() {
            writeln!(out, "  {}: (no change)", prof.profile_id)?;
            continue;
        }
        // Headline: total added/removed across compared tags.
        let (added_n, removed_n): (usize, usize) = prof.tags.iter().fold((0, 0), |acc, t| {
            (acc.0 + t.changes.added.len(), acc.1 + t.changes.removed.len())
        });
        writeln!(out, "  {}: +{added_n} -{removed_n}", prof.profile_id)?;
        for tag in &prof.tags {
            if tag.changes.is_empty_diff() {
                continue;
            }
            writeln!(out, "    {}", tag.tag_label)?;
            if !tag.changes.added.is_empty() {
                writeln!(out, "      added ({}): {}", tag.changes.added.len(), tag.changes.added.join(", "))?;
            }
            if !tag.changes.removed.is_empty() {
                writeln!(
                    out,
                    "      removed ({}): {}",
                    tag.changes.removed.len(),
                    tag.changes.removed.join(", ")
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ImpactJson<'a> {
    from: &'a str,
    to: &'a str,
    target_set: &'a str,
    profiles: Vec<&'a str>,
    path: String,
    affected_profile_count: usize,
    per_profile: &'a [rpm_spec_analyzer::ProfileImpact],
}

fn render_json(
    source: &io::Source,
    report: &ImpactReport,
    target_set: &ResolvedTargetSet,
    from: &str,
    to: &str,
) -> Result<()> {
    let payload = ImpactJson {
        from,
        to,
        target_set: target_set.id.as_str(),
        profiles: target_set
            .targets
            .iter()
            .map(|t| t.profile_id.as_str())
            .collect(),
        path: source.display_name().to_string(),
        affected_profile_count: report.affected_profile_count(),
        per_profile: &report.per_profile,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &payload)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}
