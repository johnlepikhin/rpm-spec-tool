//! `matrix explain` — answer "why is this line / macro active on
//! profile X but not Y?" for a single spec.
//!
//! Two query modes (mutually exclusive):
//!
//! * `--line N` — for every `%if`/`%ifarch`/`%ifos` branch whose
//!   directive span contains line `N` (or whose conditional body
//!   covers it), report per-profile activity. Uses the same evaluator
//!   as `matrix coverage` for consistency — if a branch is flagged
//!   `[DEAD]` there, it will surface here too.
//! * `--macro NAME` — for every member profile, report whether the
//!   macro is defined and its expanded literal value. Mirrors the
//!   single-profile `profile macro NAME` introspection but spread
//!   across a target set.
//!
//! Output formats: `human` (grouped sections) and `json` (structured
//! per-profile payload). The JSON shape is documented in
//! `doc/matrix.md` under "Explain mode".

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Args, ValueEnum};
use rpm_spec_analyzer::profile::ResolvedTargetSet;
use rpm_spec_analyzer::{CoverageReport, EvalError, session::parse};
use serde::Serialize;

use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Grouped per-profile text.
    Human,
    /// Structured JSON for tooling consumption.
    Json,
}

/// `--target-set NAME` and `--profiles a,b,c` are exclusive; so are
/// `--line N` and `--macro NAME`. The two `ArgGroup`s independently
/// require exactly one option each.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix_source")
        .required(true)
        .args(["target_set", "profiles"]),
))]
#[command(group(
    ArgGroup::new("explain_query")
        .required(true)
        .args(["line", "macro_name"]),
))]
pub struct ExplainOpts {
    #[command(flatten)]
    pub input: crate::app::CommonInput,

    #[arg(long, default_value_t = OutputFormat::Human, value_enum)]
    pub format: OutputFormat,

    #[arg(long = "target-set", value_name = "NAME")]
    pub target_set: Option<String>,

    #[arg(long = "profiles", value_name = "P1,P2,...", value_delimiter = ',')]
    pub profiles: Vec<String>,

    /// Spec line number (1-based) to explain. Reports activity of every
    /// `%if`-class branch whose body covers that line.
    #[arg(long = "line", value_name = "N")]
    pub line: Option<u32>,

    /// Macro name (without `%` / `%{}`) to explain. Reports the
    /// expanded literal value on every member profile.
    #[arg(long = "macro", value_name = "NAME")]
    pub macro_name: Option<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

pub(super) fn run(opts: ExplainOpts, config_override: Option<&Path>) -> Result<ExitCode> {
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
            "error: matrix explain operates on exactly one spec at a time \
             (got {} sources)",
            sources.len()
        );
        return Ok(ExitCode::from(2));
    }
    // `io::read_sources` returns at least one source (it injects a
    // stdin sentinel when the path list is empty), so an empty Vec is
    // structurally impossible. `expect` documents that invariant
    // instead of pretending this is a user-facing anyhow error path.
    let source = sources
        .into_iter()
        .next()
        .expect("io::read_sources guarantees >= 1 source");

    match (opts.line, opts.macro_name.as_deref()) {
        (Some(line), None) => {
            let report = explain_line(&source, line, &resolved);
            emit(opts.format, &source, &resolved, ExplainPayload::Line(report))
        }
        (None, Some(name)) => {
            // Macro explanation is profile-scoped: it consults the
            // resolved macro registry of each member profile and never
            // looks at the spec contents. We still require a path arg
            // for symmetry with `--line` and so config discovery
            // (`.rpmspec.toml` walk-up) starts from somewhere
            // meaningful.
            let report = explain_macro(name, &resolved);
            emit(
                opts.format,
                &source,
                &resolved,
                ExplainPayload::Macro(report),
            )
        }
        // `ArgGroup::required(true)` on `explain_query` + the
        // `.args([…])` exclusivity together guarantee exactly one of
        // `line`/`macro_name` is `Some`. Reaching this arm means clap
        // shipped a regression — panic loudly so it's caught in CI
        // rather than masked as a silent ExitCode::from(2).
        (Some(_), Some(_)) | (None, None) => {
            unreachable!(
                "clap ArgGroup `explain_query` was bypassed (line={:?}, macro={:?})",
                opts.line, opts.macro_name
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Line explanation
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct LineReport {
    line: u32,
    matched: Vec<MatchedBranch>,
}

#[derive(Debug)]
struct MatchedBranch {
    branch_line: u32,
    display: String,
    is_dead: bool,
    is_universally_active: bool,
    active_on: Vec<String>,
    inactive_on: Vec<String>,
    indeterminate_on: Vec<String>,
    indeterminate_reasons: std::collections::BTreeMap<String, EvalError>,
}

fn explain_line(source: &io::Source, line: u32, target_set: &ResolvedTargetSet) -> LineReport {
    let parsed = parse(&source.contents);
    // Surface parser-level issues up-front. Without this banner an
    // empty `matched` list is indistinguishable from "spec is broken
    // and the conditional was dropped during error recovery" — the
    // user gets the "(no enclosing branch covers this line)" message
    // and has no clue the spec failed to parse. We don't abort:
    // explain is itself a diagnostic tool, and a partial AST often
    // still answers the question.
    if !parsed.parser_diagnostics.is_empty() {
        let total = parsed.parser_diagnostics.len();
        let errors = parsed
            .parser_diagnostics
            .iter()
            .filter(|d| {
                matches!(d.severity, rpm_spec_analyzer::ParserSeverity::Error)
            })
            .count();
        eprintln!(
            "warning: spec produced {total} parser diagnostic(s) ({errors} error-level) — \
             the report below is computed against the recovered AST and may be incomplete"
        );
    }
    let coverage = CoverageReport::compute(&parsed.spec, target_set);
    // A branch "covers" the requested line if the line falls inside
    // the conditional's span. We don't try to identify the exact
    // branch body — for nested chains the user reads the report and
    // sees every enclosing decision in turn (matches what
    // `matrix coverage` already shows).
    let mut matched = Vec::new();
    for cond in &coverage.conditionals {
        if line < cond.span.start_line || line > cond.span.end_line {
            continue;
        }
        for b in &cond.branches {
            matched.push(MatchedBranch {
                branch_line: b.branch.span.start_line,
                display: b.branch.display.clone(),
                is_dead: b.is_dead(),
                is_universally_active: b.is_universally_active(),
                active_on: b.active_on.clone(),
                inactive_on: b.inactive_on.clone(),
                indeterminate_on: b.indeterminate_on.clone(),
                indeterminate_reasons: b.indeterminate_reasons.clone(),
            });
        }
    }
    LineReport { line, matched }
}

// ---------------------------------------------------------------------------
// Macro explanation
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct MacroReport {
    name: String,
    profiles: Vec<MacroPerProfile>,
}

#[derive(Debug)]
struct MacroPerProfile {
    profile_id: String,
    state: MacroState,
}

/// Per-profile macro lookup outcome. Three-valued because "defined
/// but body not literal-expandable" is operationally distinct from
/// "undefined" (the former hints "use `rpm --eval` for the runtime
/// value", the latter "add a guard or define it").
#[derive(Debug)]
enum MacroState {
    /// Macro is not registered for this profile.
    Undefined,
    /// Macro is registered and its body reduced to a literal.
    Literal(String),
    /// Macro is registered but expansion can't produce a literal —
    /// e.g. `MacroValue::Builtin`, parameterised body, conditional
    /// refs like `%{?name}`, or shell substitution. The static
    /// string carries the short human-readable reason.
    Unexpandable(&'static str),
}

/// Depth budget for `MacroRegistry::expand_to_literal`. Matches the
/// default used elsewhere in the analyzer — see the type doc on the
/// registry method for the rationale (path macros resolve in 2 levels,
/// 8 leaves headroom for nested helpers).
const EXPAND_DEPTH: u8 = 8;

fn explain_macro(name: &str, target_set: &ResolvedTargetSet) -> MacroReport {
    let mut profiles = Vec::with_capacity(target_set.targets.len());
    for rt in &target_set.targets {
        let state = match rt.profile.macros.get(name) {
            None => MacroState::Undefined,
            Some(_) => match rt.profile.macros.expand_to_literal(name, EXPAND_DEPTH) {
                Some(literal) => MacroState::Literal(literal),
                None => MacroState::Unexpandable("body not literal-expandable"),
            },
        };
        profiles.push(MacroPerProfile {
            profile_id: rt.profile_id.clone(),
            state,
        });
    }
    MacroReport {
        name: name.to_string(),
        profiles,
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum ExplainPayload {
    Line(LineReport),
    Macro(MacroReport),
}

fn emit(
    format: OutputFormat,
    source: &io::Source,
    target_set: &ResolvedTargetSet,
    payload: ExplainPayload,
) -> Result<ExitCode> {
    match format {
        OutputFormat::Human => render_human(source, target_set, &payload)?,
        OutputFormat::Json => render_json(source, target_set, &payload)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn render_human(
    source: &io::Source,
    target_set: &ResolvedTargetSet,
    payload: &ExplainPayload,
) -> Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "# Matrix explain: target set `{}` ({} profiles)",
        target_set.id,
        target_set.targets.len()
    )?;
    writeln!(out, "## {}", source.display_name())?;

    match payload {
        ExplainPayload::Line(line) => {
            writeln!(out, "  line {}", line.line)?;
            if line.matched.is_empty() {
                writeln!(
                    out,
                    "    (no enclosing %if/%ifarch/%ifos branch covers this line)"
                )?;
                return Ok(());
            }
            for b in &line.matched {
                let tag = if b.is_dead {
                    " [DEAD]"
                } else if b.is_universally_active {
                    " [ALWAYS]"
                } else {
                    ""
                };
                writeln!(out, "    branch {}: {}{tag}", b.branch_line, b.display)?;
                writeln!(out, "      active:   {}", or_none(&b.active_on))?;
                writeln!(out, "      inactive: {}", or_none(&b.inactive_on))?;
                if !b.indeterminate_on.is_empty() {
                    let mut parts = Vec::with_capacity(b.indeterminate_on.len());
                    for pid in &b.indeterminate_on {
                        match b.indeterminate_reasons.get(pid) {
                            Some(reason) => parts.push(format!("{pid} ({reason})")),
                            None => parts.push(pid.clone()),
                        }
                    }
                    writeln!(out, "      indeterminate: {}", parts.join(", "))?;
                }
            }
        }
        ExplainPayload::Macro(m) => {
            writeln!(out, "  macro %{{{}}}", m.name)?;
            for p in &m.profiles {
                let rendered = match &p.state {
                    MacroState::Undefined => "(undefined)".to_string(),
                    MacroState::Literal(v) => v.clone(),
                    MacroState::Unexpandable(reason) => format!("(defined but {reason})"),
                };
                writeln!(out, "    {} = {rendered}", p.profile_id)?;
            }
        }
    }
    Ok(())
}

fn or_none(ids: &[String]) -> String {
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}

fn render_json(
    source: &io::Source,
    target_set: &ResolvedTargetSet,
    payload: &ExplainPayload,
) -> Result<()> {
    let json = match payload {
        ExplainPayload::Line(line) => ExplainJson::Line(LineJson {
            target_set: target_set.id.as_str(),
            profiles: target_set
                .targets
                .iter()
                .map(|t| t.profile_id.as_str())
                .collect(),
            path: source.display_name().to_string(),
            line: line.line,
            branches: line
                .matched
                .iter()
                .map(|b| BranchJson {
                    branch_line: b.branch_line,
                    display: b.display.as_str(),
                    is_dead: b.is_dead,
                    is_universally_active: b.is_universally_active,
                    active_on: &b.active_on,
                    inactive_on: &b.inactive_on,
                    indeterminate_on: &b.indeterminate_on,
                    indeterminate_reasons: &b.indeterminate_reasons,
                })
                .collect(),
        }),
        ExplainPayload::Macro(m) => ExplainJson::Macro(MacroJson {
            target_set: target_set.id.as_str(),
            profiles: target_set
                .targets
                .iter()
                .map(|t| t.profile_id.as_str())
                .collect(),
            path: source.display_name().to_string(),
            name: m.name.as_str(),
            entries: m
                .profiles
                .iter()
                .map(|p| {
                    let (defined, value, unexpandable_reason) = match &p.state {
                        MacroState::Undefined => (false, None, None),
                        MacroState::Literal(v) => (true, Some(v.as_str()), None),
                        MacroState::Unexpandable(reason) => (true, None, Some(*reason)),
                    };
                    MacroEntryJson {
                        profile_id: p.profile_id.as_str(),
                        defined,
                        value,
                        unexpandable_reason,
                    }
                })
                .collect(),
        }),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &json)?;
    use std::io::Write;
    writeln!(out)?;
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(tag = "query", rename_all = "snake_case")]
enum ExplainJson<'a> {
    Line(LineJson<'a>),
    Macro(MacroJson<'a>),
}

#[derive(Debug, Serialize)]
struct LineJson<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    path: String,
    line: u32,
    branches: Vec<BranchJson<'a>>,
}

#[derive(Debug, Serialize)]
struct BranchJson<'a> {
    branch_line: u32,
    display: &'a str,
    is_dead: bool,
    is_universally_active: bool,
    active_on: &'a [String],
    inactive_on: &'a [String],
    indeterminate_on: &'a [String],
    indeterminate_reasons: &'a std::collections::BTreeMap<String, EvalError>,
}

#[derive(Debug, Serialize)]
struct MacroJson<'a> {
    target_set: &'a str,
    profiles: Vec<&'a str>,
    path: String,
    name: &'a str,
    entries: Vec<MacroEntryJson<'a>>,
}

#[derive(Debug, Serialize)]
struct MacroEntryJson<'a> {
    profile_id: &'a str,
    defined: bool,
    /// Expanded literal value when the macro resolves to one. `None`
    /// when the macro is undefined OR when its body cannot be reduced
    /// to a literal at lint time (see `unexpandable_reason`).
    value: Option<&'a str>,
    /// Set only when `defined == true` and `value == None`. Carries
    /// the short reason from the analyzer.
    unexpandable_reason: Option<&'static str>,
}
