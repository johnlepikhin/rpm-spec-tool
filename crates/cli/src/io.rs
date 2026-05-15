//! Reading sources from disk or stdin and writing them back atomically.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Logical name of a source — either an on-disk path or `"<stdin>"`.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub contents: String,
    pub is_stdin: bool,
}

impl Source {
    /// Human-readable name of the source, with control characters in real
    /// paths escaped so a malicious filename can't smuggle newlines or
    /// escape sequences into stderr / structured logs.
    pub fn display_name(&self) -> String {
        if self.is_stdin {
            "<stdin>".to_owned()
        } else {
            sanitize_path_for_display(&self.path)
        }
    }
}

fn sanitize_path_for_display(path: &Path) -> String {
    let raw = path.display().to_string();
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Read every spec source named on the command line. An empty `paths` list
/// (or a single `-`) reads stdin once.
pub fn read_sources(paths: &[PathBuf]) -> Result<Vec<Source>> {
    if paths.is_empty() || (paths.len() == 1 && paths[0] == Path::new("-")) {
        return Ok(vec![read_stdin()?]);
    }

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if p == Path::new("-") {
            out.push(read_stdin()?);
            continue;
        }
        let contents = fs::read_to_string(p)
            .with_context(|| format!("failed to read {}", p.display()))?;
        out.push(Source { path: p.clone(), contents, is_stdin: false });
    }
    Ok(out)
}

fn read_stdin() -> Result<Source> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).context("failed to read stdin")?;
    Ok(Source { path: PathBuf::from("-"), contents: buf, is_stdin: true })
}

/// Write `contents` to `path` atomically.
///
/// Algorithm: stage the content in a `NamedTempFile` in the same directory,
/// fsync it, copy the target's existing permissions onto the temp inode,
/// then `rename(2)` it into place and fsync the containing directory.
/// `rename(2)` is atomic within one filesystem, so a crash leaves either
/// the old or the new content — never a half-written file.
///
/// The symlink check at the start is a UX guard (so a `--fix` of `foo.spec`
/// doesn't silently rewrite whatever `foo.spec` points to), not a security
/// boundary — `rename(2)` itself does not follow symlinks at the
/// destination.
///
/// # Permissions
///
/// When `path` already exists, its mode is copied onto the temp file
/// (masked by `0o777`, so setuid/setgid/sticky aren't smuggled across a
/// rewrite). When `path` does **not** exist, the result inherits
/// `NamedTempFile`'s default mode (`0o600` on Unix), **not** the usual
/// `0o666 & !umask` of `fs::write`. Today this only matters for the
/// `--fix` flow on a fresh path, which doesn't happen — but callers
/// targeting new files should set permissions explicitly afterwards.
///
/// # Errors
///
/// - `path` is an existing symlink (refused on purpose).
/// - The parent directory is not writable.
/// - The cross-step IO (write / fsync / rename) fails.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        bail!(
            "refusing to overwrite symlink: {} (resolve manually if intentional)",
            path.display()
        );
    }

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)
        .with_context(|| format!("failed to create temp file in {}", dir.display()))?;
    tmp.as_file_mut()
        .write_all(contents.as_bytes())
        .with_context(|| format!("failed to write temp file for {}", path.display()))?;
    tmp.as_file_mut()
        .sync_all()
        .with_context(|| format!("failed to fsync temp file for {}", path.display()))?;

    // Preserve permissions of the destination if it already exists. Without
    // this step the freshly persisted file inherits the temp file's 0600,
    // silently downgrading a 0644 spec. Mask off setuid/setgid/sticky bits
    // so a chmod-rogue source can't carry them onto the rewritten file.
    if let Ok(orig_meta) = fs::metadata(path) {
        let perms = mask_perms(orig_meta.permissions());
        if let Err(e) = tmp.as_file_mut().set_permissions(perms) {
            tracing::warn!(
                path = %path.display(),
                err = %e,
                "failed to preserve mode of destination; file will inherit temp file's mode"
            );
        }
    }

    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("failed to persist {}: {}", path.display(), e.error))?;

    // fsync the directory entry so the rename survives a crash.
    // Some filesystems (tmpfs, overlayfs) reject `sync_all` on directories;
    // we surface that at debug level rather than fail the write.
    match fs::File::open(&dir) {
        Ok(dir_file) => {
            if let Err(e) = dir_file.sync_all() {
                tracing::debug!(
                    dir = %dir.display(),
                    err = %e,
                    "directory fsync failed; durability is best-effort on this filesystem"
                );
            }
        }
        Err(e) => {
            tracing::debug!(
                dir = %dir.display(),
                err = %e,
                "failed to open directory for fsync; durability is best-effort"
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mask_perms(p: fs::Permissions) -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(p.mode() & 0o777)
}

#[cfg(not(unix))]
fn mask_perms(p: fs::Permissions) -> fs::Permissions {
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn write_atomic_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        write_atomic(&target, "hello").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
    }

    #[test]
    fn write_atomic_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        fs::write(&target, "old").unwrap();
        write_atomic(&target, "new").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        fs::write(&target, "old").unwrap();
        let mut perms = fs::metadata(&target).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&target, perms).unwrap();

        write_atomic(&target, "new").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "expected mode 0644 preserved, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_refuses_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("link.txt");
        let real = dir.path().join("real.txt");
        fs::write(&real, "untouched").unwrap();
        symlink(&real, &target).unwrap();

        let err = write_atomic(&target, "tainted").expect_err("should refuse symlink");
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink message, got: {err}"
        );
        // Real file must remain unchanged.
        assert_eq!(fs::read_to_string(&real).unwrap(), "untouched");
    }

    #[test]
    fn display_name_escapes_control_chars() {
        let s = Source {
            path: PathBuf::from("evil\nspec.spec"),
            contents: String::new(),
            is_stdin: false,
        };
        assert_eq!(s.display_name(), "evil\\nspec.spec");
    }
}
