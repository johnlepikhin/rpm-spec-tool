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
    pub fn display_name(&self) -> String {
        if self.is_stdin {
            "<stdin>".to_owned()
        } else {
            self.path.display().to_string()
        }
    }
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

/// Atomically write `contents` to `path`. The file is staged in the same
/// directory and `persist`ed in place so a crash between truncate and write
/// cannot leave the user with a half-written file. Refuses to follow a
/// pre-existing symlink — the link target would be rewritten silently
/// otherwise.
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
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("failed to persist {}: {}", path.display(), e.error))?;
    Ok(())
}
