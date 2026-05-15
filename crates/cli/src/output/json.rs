//! JSON diagnostic output. One JSON object per `--format json` invocation
//! with the shape `{ "files": [ { "path": ..., "diagnostics": [...] } ] }`.

use anyhow::Result;
use rpm_spec_analyzer::Diagnostic;
use serde::Serialize;

use crate::io::Source;

#[derive(Serialize)]
struct FileEntry<'a> {
    path: String,
    diagnostics: &'a [Diagnostic],
}

#[derive(Serialize)]
struct Report<'a> {
    files: Vec<FileEntry<'a>>,
}

pub fn render(items: &[(Source, Vec<Diagnostic>)]) -> Result<()> {
    let files = items
        .iter()
        .map(|(s, d)| FileEntry { path: s.display_name(), diagnostics: d })
        .collect();
    let report = Report { files };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
