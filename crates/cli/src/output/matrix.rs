//! Output renderers for `matrix check`.
//!
//! Three formats:
//!
//! * `render_human` — per-source summary table (one row per profile)
//!   followed by a list of aggregated diagnostics grouped by signature
//!   with the `affected_profiles` set.
//! * `render_json` — `MatrixJsonReport`. The shape is distinct from
//!   the single-profile `lint` JSON output so tools that consume one
//!   never accidentally mis-parse the other.
//! * `render_sarif` — SARIF 2.1.0. Each Result carries `properties`
//!   with `profile`, `matrix_signature`, `affected_profiles`,
//!   `target_set`. SARIF allows arbitrary properties, so this
//!   stays consumer-compatible.

use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{AggregatedDiagnostic, MatrixSignature, ProfileResult, Severity};
use serde::Serialize;

use crate::commands::matrix::check::MatrixCheckResult;

/// Tool name written into SARIF `tool.driver.name`. Sourced from the
/// CLI crate's `Cargo.toml` so a rename happens in one place.
const TOOL_NAME: &str = env!("CARGO_PKG_NAME");

/// Static link to the project, written into SARIF `tool.driver.informationUri`.
const TOOL_INFORMATION_URI: &str = "https://github.com/johnlepikhin/rpm-spec-tool";

/// Suffix appended in human output to aggregated entries that match
/// the loaded baseline. Documented in `doc/matrix.md`; promoted to a
/// constant so the doc, code, and any snapshot tests stay in sync.
const KNOWN_FROM_BASELINE_TAG: &str = " [baseline]";

// ---------------------------------------------------------------------------
// Human
// ---------------------------------------------------------------------------

/// Matrix human output is intentionally monochrome in this phase —
/// codespan-reporting's coloured span renderer doesn't compose well
/// with the aggregated `(N/M profiles)` view. When colour lands, take
/// a `ColorChoice` parameter and route through [`crate::output::resolve_color`].
///
/// `known_signatures` is the set loaded from `--baseline` (empty
/// when no baseline was supplied). Matched aggregated entries are
/// tagged with [`KNOWN_FROM_BASELINE_TAG`].
pub fn render_human(
    items: &[MatrixCheckResult],
    target_set: &ResolvedTargetSet,
    known_signatures: &std::collections::HashSet<MatrixSignature>,
) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    writeln!(out, "Matrix run: target set \"{}\"", target_set.id)?;
    writeln!(out, "  profiles: {}", target_set.targets.len())?;
    writeln!(out)?;

    if items.is_empty() {
        writeln!(out, "(no input specs)")?;
        return Ok(());
    }

    for item in items {
        writeln!(out, "=== {} ===", item.source.display_name())?;
        render_table(&mut out, &item.result.per_profile)?;
        writeln!(out)?;
        if item.result.aggregated.is_empty() {
            writeln!(out, "  (no diagnostics)")?;
        } else {
            render_aggregated(
                &mut out,
                &item.result.aggregated,
                target_set,
                known_signatures,
            )?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn render_table(out: &mut impl Write, per_profile: &[ProfileResult]) -> Result<()> {
    writeln!(
        out,
        "  {:<28} {:>6} {:>6} {:>6}",
        "PROFILE", "DENY", "WARN", "PARSE"
    )?;
    for pr in per_profile {
        let mut deny = 0usize;
        let mut warn = 0usize;
        for d in &pr.diagnostics {
            match d.severity {
                Severity::Deny => deny += 1,
                Severity::Warn => warn += 1,
                Severity::Allow => {}
            }
        }
        let parse = if pr.parse.parser_diagnostics.is_empty() {
            "OK"
        } else {
            "ISSUES"
        };
        writeln!(
            out,
            "  {:<28} {:>6} {:>6} {:>6}",
            pr.profile_id, deny, warn, parse
        )?;
    }
    Ok(())
}

fn render_aggregated(
    out: &mut impl Write,
    aggregated: &[AggregatedDiagnostic],
    target_set: &ResolvedTargetSet,
    known_signatures: &std::collections::HashSet<MatrixSignature>,
) -> Result<()> {
    let total = target_set.targets.len();
    // Stable order: by severity (deny first), then lint_id, then span.
    let mut sorted: Vec<&AggregatedDiagnostic> = aggregated.iter().collect();
    sorted.sort_by(|a, b| {
        severity_rank(a.diagnostic.severity)
            .cmp(&severity_rank(b.diagnostic.severity))
            .then_with(|| a.diagnostic.lint_id.cmp(b.diagnostic.lint_id))
            .then_with(|| {
                a.diagnostic
                    .primary_span
                    .start_byte
                    .cmp(&b.diagnostic.primary_span.start_byte)
            })
    });
    for ad in sorted {
        let known_tag = if known_signatures.contains(&ad.signature) {
            KNOWN_FROM_BASELINE_TAG
        } else {
            ""
        };
        writeln!(
            out,
            "  [{sev}] {id} {lname} ({hit}/{total}){known_tag}",
            sev = severity_label(ad.diagnostic.severity),
            id = ad.diagnostic.lint_id,
            lname = ad.diagnostic.lint_name,
            hit = ad.affected_profiles.len(),
        )?;
        writeln!(out, "    {}", ad.diagnostic.message)?;
        writeln!(
            out,
            "    line {}, column {}",
            ad.diagnostic.primary_span.start_line, ad.diagnostic.primary_span.start_column,
        )?;
        writeln!(out, "    affected: {}", ad.affected_profiles.join(", "))?;
    }
    Ok(())
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Deny => 0,
        Severity::Warn => 1,
        Severity::Allow => 2,
    }
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Deny => "deny",
        Severity::Warn => "warn",
        Severity::Allow => "allow",
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Top-level JSON shape emitted by `matrix check --format json`.
///
/// Distinct from the `lint` JSON (which is a flat per-file array of
/// diagnostics): this carries the matrix dimension (per-profile
/// breakdown + aggregated view) so downstream tooling can render
/// either pivot.
#[derive(Debug, Serialize)]
struct MatrixJsonReport<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    files: Vec<MatrixJsonFile<'a>>,
}

#[derive(Debug, Serialize)]
struct MatrixJsonFile<'a> {
    path: String,
    per_profile: Vec<MatrixJsonProfileResult<'a>>,
    aggregated: Vec<MatrixJsonAggregated<'a>>,
}

#[derive(Debug, Serialize)]
struct MatrixJsonProfileResult<'a> {
    profile: &'a str,
    parse_ok: bool,
    diagnostics: Vec<JsonDiagnostic<'a>>,
}

#[derive(Debug, Serialize)]
struct MatrixJsonAggregated<'a> {
    matrix_signature: String,
    lint_id: &'a str,
    lint_name: &'a str,
    severity: &'a str,
    message: &'a str,
    primary_span: JsonSpan,
    affected_profiles: &'a [String],
}

#[derive(Debug, Serialize)]
struct JsonDiagnostic<'a> {
    lint_id: &'a str,
    lint_name: &'a str,
    severity: &'a str,
    message: &'a str,
    primary_span: JsonSpan,
}

#[derive(Debug, Serialize)]
struct JsonSpan {
    start_byte: usize,
    end_byte: usize,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
}

impl JsonSpan {
    fn from(s: &rpm_spec_analyzer::Span) -> Self {
        Self {
            start_byte: s.start_byte,
            end_byte: s.end_byte,
            start_line: s.start_line,
            start_column: s.start_column,
            end_line: s.end_line,
            end_column: s.end_column,
        }
    }
}

pub fn render_json(items: &[MatrixCheckResult], target_set: &ResolvedTargetSet) -> Result<()> {
    let profile_names: Vec<&str> = target_set
        .targets
        .iter()
        .map(|t| t.profile_id.as_str())
        .collect();
    let files: Vec<MatrixJsonFile> = items
        .iter()
        .map(|item| MatrixJsonFile {
            path: item.source.display_name().to_string(),
            per_profile: item
                .result
                .per_profile
                .iter()
                .map(|pr| MatrixJsonProfileResult {
                    profile: pr.profile_id.as_str(),
                    parse_ok: pr.parse.parser_diagnostics.is_empty(),
                    diagnostics: pr
                        .diagnostics
                        .iter()
                        .map(|d| JsonDiagnostic {
                            lint_id: d.lint_id,
                            lint_name: d.lint_name,
                            severity: severity_label(d.severity),
                            message: d.message.as_str(),
                            primary_span: JsonSpan::from(&d.primary_span),
                        })
                        .collect(),
                })
                .collect(),
            aggregated: item
                .result
                .aggregated
                .iter()
                .map(|ad| MatrixJsonAggregated {
                    matrix_signature: ad.signature.to_string(),
                    lint_id: ad.diagnostic.lint_id,
                    lint_name: ad.diagnostic.lint_name,
                    severity: severity_label(ad.diagnostic.severity),
                    message: ad.diagnostic.message.as_str(),
                    primary_span: JsonSpan::from(&ad.diagnostic.primary_span),
                    affected_profiles: &ad.affected_profiles,
                })
                .collect(),
        })
        .collect();

    let report = MatrixJsonReport {
        target_set: target_set.id.as_str(),
        profiles: profile_names,
        files,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &report)?;
    writeln!(out)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SARIF
// ---------------------------------------------------------------------------

pub fn render_sarif(items: &[MatrixCheckResult], target_set: &ResolvedTargetSet) -> Result<()> {
    // SARIF 2.1.0 minimal envelope. Each Diagnostic becomes one
    // Result, with matrix metadata in `properties`. We emit
    // per-profile findings (not the aggregated view) so consumers
    // that already understand SARIF get a flat result list; the
    // matrix dimension surfaces through properties.
    let mut results: Vec<serde_json::Value> = Vec::new();
    for item in items {
        // Build a signature → affected_profiles index once per source
        // so per-diagnostic emission stays O(1) instead of O(aggregated).
        let affected_by_sig: HashMap<MatrixSignature, &[String]> = item
            .result
            .aggregated
            .iter()
            .map(|ad| (ad.signature, ad.affected_profiles.as_slice()))
            .collect();
        for pr in &item.result.per_profile {
            for d in &pr.diagnostics {
                let sig = MatrixSignature::for_diagnostic(d);
                let affected: &[String] = affected_by_sig.get(&sig).copied().unwrap_or(&[]);
                results.push(sarif_result(
                    &item.source.display_name(),
                    &pr.profile_id,
                    &target_set.id,
                    sig,
                    affected,
                    d,
                ));
            }
        }
    }
    let envelope = serde_json::json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": TOOL_NAME,
                    "informationUri": TOOL_INFORMATION_URI,
                }
            },
            "results": results,
        }],
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &envelope)?;
    writeln!(out)?;
    Ok(())
}

fn sarif_result(
    artifact_uri: &str,
    profile_id: &str,
    target_set_id: &str,
    signature: MatrixSignature,
    affected: &[String],
    d: &rpm_spec_analyzer::Diagnostic,
) -> serde_json::Value {
    // saturating_sub guards against a malformed Diagnostic where
    // end_byte < start_byte (e.g. constructed by tests or by a future
    // rule with a bug). Single-profile SARIF has the same defence.
    let byte_length = d
        .primary_span
        .end_byte
        .saturating_sub(d.primary_span.start_byte);
    serde_json::json!({
        "ruleId": d.lint_id,
        "level": match d.severity {
            Severity::Deny => "error",
            Severity::Warn => "warning",
            Severity::Allow => "none",
        },
        "message": { "text": d.message },
        "locations": [{
            "physicalLocation": {
                "artifactLocation": { "uri": artifact_uri.to_string() },
                "region": {
                    "startLine": d.primary_span.start_line,
                    "startColumn": d.primary_span.start_column,
                    "endLine": d.primary_span.end_line,
                    "endColumn": d.primary_span.end_column,
                    "byteOffset": d.primary_span.start_byte,
                    "byteLength": byte_length,
                }
            }
        }],
        "properties": {
            "profile": profile_id,
            "target_set": target_set_id,
            "matrix_signature": signature.to_string(),
            "affected_profiles": affected,
        }
    })
}
