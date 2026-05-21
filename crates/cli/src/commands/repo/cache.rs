//! `repo cache` — inspect and prune the on-disk metadata cache.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand};

use rpm_spec_analyzer::config::Config;
use rpm_spec_repo_metadata::cache::CacheDirs;

use super::RepoArgs;
use crate::commands::profile::style::Style;

#[derive(Debug, Args)]
pub struct CacheOpts {
    #[command(subcommand)]
    pub action: CacheAction,
}

#[derive(Debug, Subcommand)]
pub enum CacheAction {
    /// List every cached repo + its snapshot count.
    #[command(visible_alias = "ls")]
    List,
    /// Delete snapshot directories that are not the `current` link
    /// and not pinned by any known lockfile. Defaults to keeping
    /// only the current snapshot per repo.
    Gc {
        /// Keep this many most-recent snapshots per repo (default 1).
        #[arg(long, default_value = "1")]
        keep: usize,
        /// Skip the interactive confirmation. Required for non-TTY
        /// invocations (CI/scripts).
        #[arg(long, short = 'y')]
        yes: bool,
        /// Enumerate what *would* be deleted (full paths) without
        /// touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
    /// Wipe all cached snapshots for one or every repo.
    Prune {
        /// SHA prefix of a specific repo (`repos/<sha>/`). Wipes
        /// everything when unset.
        #[arg(long)]
        repo: Option<String>,
        /// Skip the interactive confirmation. Required for non-TTY
        /// invocations (CI/scripts).
        #[arg(long, short = 'y')]
        yes: bool,
        /// Enumerate what *would* be deleted (full paths) without
        /// touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run(
    opts: CacheOpts,
    repo_args: RepoArgs,
    _config: &Config,
    _base_dir: &Path,
    _style: &Style,
) -> Result<ExitCode> {
    let cache_root = repo_args.resolve_cache_root()?;
    let dirs = CacheDirs::ensure(cache_root)?;
    let mut stdout = std::io::stdout().lock();

    match opts.action {
        CacheAction::List => ls(&dirs, &mut stdout),
        CacheAction::Gc { keep, yes, dry_run } => gc(&dirs, keep, yes, dry_run, &mut stdout),
        CacheAction::Prune { repo, yes, dry_run } => prune(&dirs, repo, yes, dry_run, &mut stdout),
    }
}

fn ls(dirs: &CacheDirs, stdout: &mut impl Write) -> Result<ExitCode> {
    if !dirs.repos.exists() {
        writeln!(stdout, "cache not initialized: {}", dirs.repos.display())?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dirs.repos)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let snapshots_dir = path.join("snapshots");
        let snap_count = if snapshots_dir.exists() {
            std::fs::read_dir(&snapshots_dir)?.count()
        } else {
            0
        };
        let current = path.join("current");
        let current_target = std::fs::read_link(&current)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "(none)".into());
        writeln!(
            stdout,
            "{}  snapshots={snap_count}  current={current_target}",
            entry.file_name().to_string_lossy()
        )?;
        count += 1;
    }
    if count == 0 {
        writeln!(
            stdout,
            "cache empty (all repos pruned): {}",
            dirs.repos.display()
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Prompt on stderr and read a line from stdin. Returns `true` iff the
/// user typed an affirmative reply (`y`, `Y`, `yes`).
fn confirm() -> Result<bool> {
    eprint!("Proceed? [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    handle.read_line(&mut line)?;
    let answer = line.trim();
    Ok(matches!(answer, "y" | "Y" | "yes"))
}

/// Gate a destructive action behind `--yes`/TTY confirmation. Returns
/// `Ok(Some(code))` to short-circuit (refuse or abort) and `Ok(None)`
/// to proceed.
fn gate_destructive(yes: bool) -> Result<Option<ExitCode>> {
    if yes {
        return Ok(None);
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("error: refusing to delete without confirmation");
        eprintln!("hint: pass --yes for non-interactive use or --dry-run to preview");
        return Ok(Some(ExitCode::from(2)));
    }
    if confirm()? {
        Ok(None)
    } else {
        eprintln!("aborted");
        Ok(Some(ExitCode::from(130)))
    }
}

/// Compute the list of snapshot directories that `gc` would remove,
/// honouring `keep` and the `current` symlink target. Does not touch
/// the filesystem beyond reading.
fn gc_candidates(dirs: &CacheDirs, keep: usize) -> Result<Vec<std::path::PathBuf>> {
    let mut candidates = Vec::new();
    if !dirs.repos.exists() {
        return Ok(candidates);
    }
    for entry in std::fs::read_dir(&dirs.repos)? {
        let entry = entry?;
        let repo_dir = entry.path();
        let snapshots = repo_dir.join("snapshots");
        if !snapshots.exists() {
            continue;
        }
        let mut by_mtime: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        for s in std::fs::read_dir(&snapshots)? {
            let s = s?;
            let m = s.metadata()?;
            by_mtime.push((s.path(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
        }
        by_mtime.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let current_target = std::fs::read_link(repo_dir.join("current")).ok();
        for (path, _) in by_mtime.into_iter().skip(keep) {
            if current_target.as_ref() == Some(&path) {
                continue;
            }
            candidates.push(path);
        }
    }
    Ok(candidates)
}

fn gc(
    dirs: &CacheDirs,
    keep: usize,
    yes: bool,
    dry_run: bool,
    stdout: &mut impl Write,
) -> Result<ExitCode> {
    if !dirs.repos.exists() {
        writeln!(stdout, "gc: removed 0 snapshots")?;
        return Ok(ExitCode::SUCCESS);
    }

    if dry_run {
        let candidates = gc_candidates(dirs, keep)?;
        for path in &candidates {
            writeln!(stdout, "would remove: {}", path.display())?;
        }
        if candidates.is_empty() {
            writeln!(stdout, "(no snapshots to remove)")?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !yes {
        let candidates = gc_candidates(dirs, keep)?;
        if candidates.is_empty() {
            writeln!(stdout, "gc: removed 0 snapshots")?;
            return Ok(ExitCode::SUCCESS);
        }
        let n = candidates.len();
        let noun = if n == 1 { "snapshot" } else { "snapshots" };
        eprintln!("gc would remove {n} {noun}:");
        for path in &candidates {
            eprintln!("  {}", path.display());
        }
        if let Some(code) = gate_destructive(yes)? {
            return Ok(code);
        }
    }

    let mut removed = 0usize;
    for entry in std::fs::read_dir(&dirs.repos)? {
        let entry = entry?;
        let repo_dir = entry.path();
        let snapshots = repo_dir.join("snapshots");
        if !snapshots.exists() {
            continue;
        }
        // Hold an exclusive per-repo lock across snapshot pruning so
        // we don't race a concurrent `repo sync` writing into the
        // same directory. Snapshots are subdirs (not the dir holding
        // `.lock`), so it's safe to remove them while the lock is
        // held.
        let _lock = rpm_spec_repo_metadata::locks::acquire(&repo_dir)?;
        let mut by_mtime: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        for s in std::fs::read_dir(&snapshots)? {
            let s = s?;
            let m = s.metadata()?;
            by_mtime.push((s.path(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
        }
        // Newest first.
        by_mtime.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        for (path, _) in by_mtime.into_iter().skip(keep) {
            // Skip if it's still the `current` target.
            let current = repo_dir.join("current");
            if let Ok(target) = std::fs::read_link(&current)
                && target == path
            {
                continue;
            }
            tracing::info!(snapshot = %path.display(), "gc'ing snapshot");
            std::fs::remove_dir_all(&path)?;
            removed += 1;
        }
    }
    let noun = if removed == 1 {
        "snapshot"
    } else {
        "snapshots"
    };
    writeln!(stdout, "gc: removed {removed} {noun}")?;
    Ok(ExitCode::SUCCESS)
}

/// Move `dir` aside under a unique sibling name while holding the
/// per-repo lock, then release the lock and remove the renamed
/// directory.
///
/// Plain "acquire lock, drop lock, then `remove_dir_all`" leaves a
/// race window where a concurrent `repo sync` can re-acquire the
/// just-released lock and re-populate the directory before the
/// removal lands. By renaming under the lock the original path
/// disappears atomically; a concurrent sync recreating the same
/// path is benign because it operates on a fresh directory whose
/// lock file is also fresh.
fn rename_then_remove(dir: &std::path::Path) -> Result<()> {
    let unique = format!(
        ".removing-{pid}-{nanos}",
        pid = std::process::id(),
        nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let renamed = dir.with_extension(unique);
    {
        let _lock = rpm_spec_repo_metadata::locks::acquire(dir)?;
        std::fs::rename(dir, &renamed)?;
    }
    std::fs::remove_dir_all(&renamed)?;
    Ok(())
}

/// Compute the list of top-level repo directories that `prune` would
/// remove for the given filter.
fn prune_candidates(dirs: &CacheDirs, repo: Option<&str>) -> Result<Vec<std::path::PathBuf>> {
    let mut candidates = Vec::new();
    if !dirs.repos.exists() {
        return Ok(candidates);
    }
    for entry in std::fs::read_dir(&dirs.repos)? {
        let entry = entry?;
        match repo {
            Some(prefix) if entry.file_name().to_string_lossy().starts_with(prefix) => {
                candidates.push(entry.path());
            }
            Some(_) => {}
            None => candidates.push(entry.path()),
        }
    }
    Ok(candidates)
}

fn prune(
    dirs: &CacheDirs,
    repo: Option<String>,
    yes: bool,
    dry_run: bool,
    stdout: &mut impl Write,
) -> Result<ExitCode> {
    // The empty-prefix guard is independent of `--yes`/`--dry-run`:
    // an empty (or whitespace-only) string would otherwise match every
    // repo and silently turn `--repo ""` / `--repo "  "` into a "wipe
    // everything" footgun.
    if let Some(prefix) = repo.as_deref()
        && prefix.trim().is_empty()
    {
        eprintln!(
            "error: `--repo` prefix is empty; refusing to prune everything (omit the flag to wipe all)"
        );
        return Ok(ExitCode::from(2));
    }

    if !dirs.repos.exists() {
        match repo.as_deref() {
            Some(prefix) => writeln!(stdout, "prune: pruned 0 repos matching '{prefix}'")?,
            None => writeln!(stdout, "prune: pruned all cached repos")?,
        }
        return Ok(ExitCode::SUCCESS);
    }

    if dry_run {
        let candidates = prune_candidates(dirs, repo.as_deref())?;
        for path in &candidates {
            writeln!(stdout, "would remove: {}", path.display())?;
        }
        if candidates.is_empty() {
            writeln!(stdout, "(no repos to prune)")?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !yes {
        let candidates = prune_candidates(dirs, repo.as_deref())?;
        if candidates.is_empty() {
            match repo.as_deref() {
                Some(prefix) => {
                    writeln!(stdout, "prune: pruned 0 repos matching '{prefix}'")?;
                }
                None => writeln!(stdout, "prune: pruned all cached repos")?,
            }
            return Ok(ExitCode::SUCCESS);
        }
        let n = candidates.len();
        let noun = if n == 1 { "repo" } else { "repos" };
        eprintln!("prune would remove {n} {noun}:");
        for path in &candidates {
            eprintln!("  {}", path.display());
        }
        if let Some(code) = gate_destructive(yes)? {
            return Ok(code);
        }
    }

    match repo {
        Some(prefix) => {
            let mut removed = 0usize;
            for entry in std::fs::read_dir(&dirs.repos)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let dir = entry.path();
                    rename_then_remove(&dir)?;
                    removed += 1;
                }
            }
            let noun = if removed == 1 { "repo" } else { "repos" };
            writeln!(stdout, "prune: pruned {removed} {noun} matching '{prefix}'")?;
        }
        None => {
            for entry in std::fs::read_dir(&dirs.repos)? {
                let entry = entry?;
                let dir = entry.path();
                rename_then_remove(&dir)?;
            }
            writeln!(stdout, "prune: pruned all cached repos")?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
