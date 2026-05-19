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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{ImpactReport, session::parse};
use serde::Serialize;

use super::coverage_style::Style;
use crate::app::ColorChoice;
use crate::io;

/// Upper bound on bytes read from a single `git show` invocation.
/// A typical spec is well under 100 KiB; 16 MiB tolerates checked-in
/// vendored sources or accidental binary blobs without OOM-ing on a
/// crafted commit. Hitting this cap surfaces as a hard error rather
/// than silently truncating.
const GIT_SHOW_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Build a `Command` for the configured git binary with locale forced
/// to `C` (and `cwd` set via `-C`). Locking the locale is load-bearing:
/// the file-missing fallback in [`git_show`] parses substrings from
/// stderr, and git translates those messages when `LANG` is set to a
/// non-English locale. Without this normalisation a Russian-locale
/// developer would see "новый файл" instead of "does not exist", the
/// fallback would silently not fire, and a brand-new spec on a PR
/// would surface as a hard error instead of the documented
/// "deps added from scratch" output.
fn git_command(git_cmd: &str, repo: &Path) -> Command {
    let mut cmd = Command::new(git_cmd);
    cmd.arg("-C").arg(repo);
    cmd.env("LC_ALL", "C");
    cmd.env("LANG", "C");
    cmd.stdin(Stdio::null());
    cmd
}

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

    /// Git revision for the "after" side. **Default: working tree** —
    /// the spec file on disk, *including uncommitted changes*. Pass a
    /// concrete REV (commit SHA, branch, tag, `HEAD~3`, or literal
    /// `HEAD`) for a frozen committed-vs-committed comparison.
    ///
    /// The worktree default reflects the primary PR-review workflow:
    /// "I edited the spec, what's the impact before I commit?". Older
    /// callers that relied on the previous `HEAD` default should
    /// switch to `--to HEAD` explicitly.
    #[arg(long = "to", value_name = "REV")]
    pub to: Option<String>,

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

pub(super) fn run(
    opts: ImpactOpts,
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
            eprintln!(
                "error: cannot canonicalise spec path {}: {e}",
                source.path.display()
            );
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

    // Resolve `--from` (always git) up-front. `--to` only needs the
    // pre-check when it's an explicit revision — worktree-mode reads
    // straight off disk and can't have a typo'd SHA. A typo'd `--from`
    // SHA must surface as a hard error here rather than later, because
    // git's "path does not exist in REV" message is emitted identically
    // for "rev unknown" and "rev valid but file missing" — without
    // this pre-check we'd silently treat a typo as "spec added in this
    // PR" and produce misleading impact output.
    if let Err(e) = git_resolve_rev(&opts.git_cmd, &repo_root, &opts.from) {
        eprintln!("error: revision `{}`: {e:#}", opts.from);
        return Ok(ExitCode::from(2));
    }
    if let Some(to_rev) = opts.to.as_deref() {
        if let Err(e) = git_resolve_rev(&opts.git_cmd, &repo_root, to_rev) {
            eprintln!("error: revision `{to_rev}`: {e:#}");
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
    // `--to`: explicit REV → git show; omitted → working tree (the
    // bytes already read from disk via `io::read_sources` above).
    // The display label kept short ("worktree") so the human/JSON
    // headers stay readable when the user hasn't supplied one.
    let (to_bytes, to_label): (String, String) = match opts.to.as_deref() {
        Some(rev) => match git_show(&opts.git_cmd, &repo_root, rev, &spec_rel) {
            Ok(b) => (b, rev.to_owned()),
            Err(e) => {
                eprintln!("error: reading spec at {rev}: {e:#}");
                return Ok(ExitCode::from(2));
            }
        },
        None => (source.contents.clone(), "worktree".to_owned()),
    };

    let from_parsed = parse(&from_bytes);
    let to_parsed = parse(&to_bytes);
    super::surface_parser_diagnostics(
        super::ParseDiagnosticContext::ImpactSide {
            label: "from",
            rev: &opts.from,
        },
        &from_parsed,
    );
    super::surface_parser_diagnostics(
        super::ParseDiagnosticContext::ImpactSide {
            label: "to",
            rev: &to_label,
        },
        &to_parsed,
    );

    let report = ImpactReport::compute(
        &from_parsed.spec,
        &to_parsed.spec,
        &resolved,
        &opts.bcond.to_overrides(),
    );

    match opts.format {
        OutputFormat::Human => {
            let style = Style::new(color);
            render_human(&source, &report, &resolved, &opts.from, &to_label, &style)?;
        }
        OutputFormat::Json => render_json(&source, &report, &resolved, &opts.from, &to_label)?,
    }
    Ok(ExitCode::SUCCESS)
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
    let out = git_command(git_cmd, repo)
        .args(["rev-parse", "--verify", "--quiet", &arg])
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
///
/// Canonicalises the result so a downstream `strip_prefix(repo_root)`
/// against the (already canonicalised) spec path lines up cleanly even
/// on systems where `/home` or similar is symlinked — git's
/// `--show-toplevel` can return the unresolved form there.
fn git_repo_root(git_cmd: &str, spec_abs: &Path) -> Result<PathBuf> {
    let spec_dir = spec_abs
        .parent()
        .with_context(|| format!("spec path has no parent: {}", spec_abs.display()))?;
    let out = git_command(git_cmd, spec_dir)
        .args(["rev-parse", "--show-toplevel"])
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
    let raw = PathBuf::from(root);
    // canonicalize() may fail on a path that no longer exists (race
    // with `git worktree remove`); fall back to the raw form rather
    // than aborting — strip_prefix gives a clean diagnostic later.
    Ok(raw.canonicalize().unwrap_or(raw))
}

/// Run `git -C repo show REV:rel_path` and return the spec content
/// as a UTF-8 string. Maps git's "file does not exist at REV" exit
/// status to an empty string so callers see "deps added from
/// scratch" cleanly rather than a hard error.
///
/// Reads stdout through a [`GIT_SHOW_MAX_BYTES`] cap so a 1 GiB blob
/// accidentally committed to the repo surfaces as a typed error
/// rather than an OOM kill. The parser tolerates non-UTF-8 in
/// changelog entries (common in older specs with Latin-1 author
/// names), so the bytes are decoded via [`String::from_utf8_lossy`]
/// instead of strict validation.
fn git_show(git_cmd: &str, repo: &Path, rev: &str, rel_path: &Path) -> Result<String> {
    // Build `REV:relpath`. Use forward slashes — git uses
    // POSIX-style internal paths regardless of host OS.
    let rel_posix = rel_path
        .to_str()
        .with_context(|| format!("non-UTF-8 path: {}", rel_path.display()))?
        .replace('\\', "/");
    let pathspec = format!("{rev}:{rel_posix}");
    let mut child = git_command(git_cmd, repo)
        .args(["show", &pathspec])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running `{git_cmd} show {pathspec}`"))?;

    // Read at most GIT_SHOW_MAX_BYTES + 1 to detect overrun without
    // buffering an unbounded blob into RAM.
    let mut stdout_buf = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let limit = GIT_SHOW_MAX_BYTES.saturating_add(1);
        stdout
            .by_ref()
            .take(limit)
            .read_to_end(&mut stdout_buf)
            .with_context(|| format!("reading `{git_cmd} show {pathspec}` stdout"))?;
    }
    let out = child
        .wait_with_output()
        .with_context(|| format!("waiting on `{git_cmd} show {pathspec}`"))?;

    if stdout_buf.len() as u64 > GIT_SHOW_MAX_BYTES {
        anyhow::bail!(
            "git show `{pathspec}` exceeded {GIT_SHOW_MAX_BYTES}-byte cap; refusing to load"
        );
    }

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Distinguish "rev exists but file missing" (treat as empty
        // spec — added at `to` side) from "rev itself is unknown"
        // (hard error — operator typo). git's wording varies across
        // versions: "does not exist" (older + newer-when-absent),
        // "exists on disk, but not in" (newer-when-present-in-worktree).
        // The git_command helper forces `LC_ALL=C` so these English
        // substrings are stable regardless of operator locale.
        let file_missing_at_rev =
            stderr.contains("does not exist") || stderr.contains("exists on disk, but not in");
        if file_missing_at_rev {
            tracing::info!(
                target: "matrix::impact",
                rev = %rev,
                path = %rel_posix,
                "spec file does not exist at revision; treating as empty (deps will surface as added/removed)"
            );
            return Ok(String::new());
        }
        anyhow::bail!("git show failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&stdout_buf).into_owned())
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
    style: &Style,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        style.header(&format!(
            "# Matrix impact: {from} → {to}, target set `{}` ({}/{} profile(s) affected)",
            target_set.id,
            report.affected_profile_count(),
            target_set.targets.len(),
        ))
    )?;
    writeln!(out, "{}", style.header(&format!("## {}", source.display_name())))?;

    if report.is_no_change() {
        writeln!(out)?;
        writeln!(out, "  {}", style.dim("(no change on any profile)"))?;
        return Ok(());
    }

    for prof in &report.per_profile {
        writeln!(out)?;
        if prof.is_no_change() {
            writeln!(
                out,
                "  {}: {}",
                prof.profile_id,
                style.dim("(no change)")
            )?;
            continue;
        }
        // Headline: total added/removed across compared tags +
        // script-section lines moved on this profile (the latter
        // already filtered by inactive `%if` branches, so the count
        // reflects what THIS profile actually sees). `+N` painted
        // green (`always_tag`-family) and `-N` red (`dead_tag`) so
        // operators read the per-profile churn at a glance.
        let (dep_added, dep_removed): (usize, usize) = prof.tags.iter().fold((0, 0), |acc, t| {
            (
                acc.0 + t.changes.added.len(),
                acc.1 + t.changes.removed.len(),
            )
        });
        let (script_added, script_removed): (usize, usize) =
            prof.script_sections.iter().fold((0, 0), |acc, s| {
                (acc.0 + s.added, acc.1 + s.removed)
            });
        let added_n = dep_added + script_added;
        let removed_n = dep_removed + script_removed;
        writeln!(
            out,
            "  {}: {} {}",
            style.header(&prof.profile_id),
            style.always_tag(&format!("+{added_n}")),
            style.dead_tag(&format!("-{removed_n}")),
        )?;
        for tag in &prof.tags {
            if tag.changes.has_no_movement() {
                continue;
            }
            writeln!(out, "    {}", style.header(tag.tag_label))?;
            if !tag.changes.added.is_empty() {
                writeln!(
                    out,
                    "      {} ({}): {}",
                    style.always_tag("added"),
                    tag.changes.added.len(),
                    tag.changes.added.join(", ")
                )?;
            }
            if !tag.changes.removed.is_empty() {
                writeln!(
                    out,
                    "      {} ({}): {}",
                    style.dead_tag("removed"),
                    tag.changes.removed.len(),
                    tag.changes.removed.join(", ")
                )?;
            }
        }
        // Per-profile script-section deltas — filtered to lines
        // active on this profile. A `%build` change gated by `%if
        // 0%{?el6}` only shows here for el6 profiles; on rhel-9 the
        // edit lives in an inactive branch and contributes nothing
        // to the profile's count.
        for sec in &prof.script_sections {
            writeln!(
                out,
                "    {}: {} {} {}",
                style.header(&sec.label),
                style.always_tag(&format!("+{}", sec.added)),
                style.dead_tag(&format!("-{}", sec.removed)),
                style.dim(&format!("({} unchanged here)", sec.unchanged)),
            )?;
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
    /// Per-profile rows include `tags` (preamble dep diffs) plus
    /// `script_sections` (per-profile-filtered shell-body /
    /// text-body diffs). See `ProfileImpact` upstream for the
    /// exhaustive shape.
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
