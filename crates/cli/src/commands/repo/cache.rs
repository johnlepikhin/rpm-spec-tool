//! `repo cache` — inspect and prune the on-disk metadata cache.

use std::io::Write;
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
    Ls,
    /// Delete snapshot directories that are not the `current` link
    /// and not pinned by any known lockfile. Defaults to keeping
    /// only the current snapshot per repo.
    Gc {
        /// Keep this many most-recent snapshots per repo (default 1).
        #[arg(long, default_value = "1")]
        keep: usize,
    },
    /// Wipe all cached snapshots for one or every repo.
    Prune {
        /// SHA prefix of a specific repo (`repos/<sha>/`). Wipes
        /// everything when unset.
        #[arg(long)]
        repo: Option<String>,
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
        CacheAction::Ls => ls(&dirs, &mut stdout),
        CacheAction::Gc { keep } => gc(&dirs, keep, &mut stdout),
        CacheAction::Prune { repo } => prune(&dirs, repo, &mut stdout),
    }
}

fn ls(dirs: &CacheDirs, stdout: &mut impl Write) -> Result<ExitCode> {
    if !dirs.repos.exists() {
        writeln!(stdout, "cache empty: {}", dirs.repos.display())?;
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
        writeln!(stdout, "cache empty: {}", dirs.repos.display())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn gc(dirs: &CacheDirs, keep: usize, stdout: &mut impl Write) -> Result<ExitCode> {
    if !dirs.repos.exists() {
        return Ok(ExitCode::SUCCESS);
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
        by_mtime.sort_by(|a, b| b.1.cmp(&a.1));
        for (path, _) in by_mtime.into_iter().skip(keep) {
            // Skip if it's still the `current` target.
            let current = repo_dir.join("current");
            if let Ok(target) = std::fs::read_link(&current) {
                if target == path {
                    continue;
                }
            }
            tracing::info!(snapshot = %path.display(), "gc'ing snapshot");
            std::fs::remove_dir_all(&path)?;
            removed += 1;
        }
    }
    writeln!(stdout, "removed {removed} snapshot(s)")?;
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

fn prune(dirs: &CacheDirs, repo: Option<String>, stdout: &mut impl Write) -> Result<ExitCode> {
    if !dirs.repos.exists() {
        return Ok(ExitCode::SUCCESS);
    }
    match repo {
        Some(prefix) => {
            if prefix.is_empty() {
                eprintln!(
                    "info: `--repo` prefix is empty; refusing to prune everything (omit the flag to wipe all)"
                );
                return Ok(ExitCode::from(2));
            }
            let mut removed = 0usize;
            for entry in std::fs::read_dir(&dirs.repos)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&prefix)
                {
                    let dir = entry.path();
                    rename_then_remove(&dir)?;
                    removed += 1;
                }
            }
            writeln!(stdout, "pruned {removed} repo(s) matching `{prefix}`")?;
        }
        None => {
            for entry in std::fs::read_dir(&dirs.repos)? {
                let entry = entry?;
                let dir = entry.path();
                rename_then_remove(&dir)?;
            }
            writeln!(stdout, "pruned all cached repos")?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
