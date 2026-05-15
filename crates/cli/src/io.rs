//! Reading sources from disk or stdin.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
