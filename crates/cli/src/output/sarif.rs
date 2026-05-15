//! Minimal SARIF 2.1.0 output (single run, single tool).
//!
//! Sufficient for GitHub Code Scanning ingest; not a full SARIF emitter.

use anyhow::Result;
use rpm_spec_analyzer::{Diagnostic, Severity};
use serde_json::json;

use crate::io::Source;

pub fn render(items: &[(Source, Vec<Diagnostic>)]) -> Result<()> {
    let mut results = Vec::new();
    for (source, diags) in items {
        for d in diags {
            results.push(json!({
                "ruleId": d.lint_id,
                "level": match d.severity {
                    Severity::Deny => "error",
                    Severity::Warn => "warning",
                    Severity::Allow => "note",
                },
                "message": { "text": d.message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": source.display_name() },
                        "region": {
                            "startLine": d.primary_span.start_line,
                            "startColumn": d.primary_span.start_column,
                            "endLine": d.primary_span.end_line,
                            "endColumn": d.primary_span.end_column,
                            "byteOffset": d.primary_span.start_byte,
                            "byteLength": d.primary_span.end_byte.saturating_sub(d.primary_span.start_byte),
                        }
                    }
                }]
            }));
        }
    }

    let report = json!({
        "version": "2.1.0",
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/Schemata/sarif-schema-2.1.0.json",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "rpm-spec-tool",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/johnlepikhin/rpm-spec-tool"
                }
            },
            "results": results
        }]
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
