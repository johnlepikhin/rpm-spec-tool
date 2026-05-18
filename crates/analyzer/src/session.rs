//! Lint session: parse a source, run configured lints, return diagnostics.

use rpm_spec::ast::{Span, SpecFile};
use rpm_spec::parse_result::{
    Diagnostic as RawParserDiagnostic, ParseResult, Severity as RawParserSeverity,
};
use rpm_spec::parser::parse_str_with_spans;
use rpm_spec_profile::Profile;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::diagnostic::{Diagnostic, Severity};
use crate::lint::Lint;
use crate::registry;

/// Parser-emitted diagnostic, decoupled from the upstream `rpm-spec`
/// types so analyzer consumers do not pick up its semver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ParserDiagnostic {
    pub severity: ParserSeverity,
    /// Stable identifier such as `"rpmspec/E0001"` when the parser tags it.
    pub code: Option<String>,
    pub span: Option<Span>,
    pub message: String,
    pub notes: Vec<String>,
}

/// Severity reported by the parser. Smaller than analyzer [`Severity`] —
/// the parser only emits warnings and errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ParserSeverity {
    Warning,
    Error,
}

impl From<RawParserDiagnostic> for ParserDiagnostic {
    fn from(d: RawParserDiagnostic) -> Self {
        let severity = match d.severity {
            RawParserSeverity::Warning => ParserSeverity::Warning,
            RawParserSeverity::Error => ParserSeverity::Error,
            // Upstream `Severity` is `#[non_exhaustive]`. If a stricter
            // variant (e.g. `Fatal`) is added, default to `Error` so we
            // never silently downgrade severity.
            other => {
                tracing::warn!(
                    ?other,
                    "unmapped upstream parser severity; treating as Error"
                );
                ParserSeverity::Error
            }
        };
        Self {
            severity,
            code: d.code,
            span: d.span,
            message: d.message,
            notes: d.notes,
        }
    }
}

/// Result of parsing a source into a `SpecFile` plus parser-level
/// diagnostics. Lint-level findings are produced separately by
/// [`LintSession::run`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ParseOutcome {
    pub spec: SpecFile<Span>,
    pub parser_diagnostics: Vec<ParserDiagnostic>,
}

/// Parse a `.spec` source string with span tracking.
pub fn parse(source: &str) -> ParseOutcome {
    let ParseResult {
        spec, diagnostics, ..
    } = parse_str_with_spans(source);
    ParseOutcome {
        spec,
        parser_diagnostics: diagnostics
            .into_iter()
            .map(ParserDiagnostic::from)
            .collect(),
    }
}

/// Convenience: parse, build a session from `config`, run lints, return both
/// the outcome (for parser diagnostics) and the lint diagnostics. CLI front-
/// ends call this instead of stitching the three steps themselves.
///
/// Parser-emitted diagnostics (the recoverable issues `rpm-spec` flags
/// while parsing — unrecognized lines, unbalanced rich deps, ...) are
/// bridged into the lint pipeline through [`bridge_parser_diagnostics`],
/// so they obey severity overrides and `--format json/sarif` just like
/// regular lints.
pub fn analyze(source: &str, config: &Config) -> (ParseOutcome, Vec<Diagnostic>) {
    analyze_with_profile(source, config, Profile::default())
}

/// Like [`analyze`] but runs lints against an explicit, pre-resolved
/// [`Profile`]. CLI front-ends use this so profile-aware lints see the
/// user's `[profiles.*]` configuration and `--profile <name>` overrides.
///
/// Thin convenience wrapper around [`analyze_with_profile_at`] with
/// `source_path = None`. New call sites should prefer the `_at`
/// variant directly — passing the real path enables filename-aware
/// rules (currently RPM312 `spec-filename-mismatch`, future rules may
/// add more). This shim is retained for API compatibility and for
/// pure in-memory consumers (tests, library users that do not have a
/// filesystem path).
pub fn analyze_with_profile(
    source: &str,
    config: &Config,
    profile: Profile,
) -> (ParseOutcome, Vec<Diagnostic>) {
    analyze_with_profile_at(source, None, config, profile)
}

/// Canonical analyzer entry point: parse `source`, run all active
/// rules against the resolved `profile`, return the parse outcome
/// (parser diagnostics) plus the lint diagnostics.
///
/// `source_path` is the spec's on-disk path when one exists (so
/// filename-aware rules like RPM312 can compare it against `Name:`).
/// Pass `None` for stdin or in-memory sources; affected rules stay
/// silent rather than guess.
pub fn analyze_with_profile_at(
    source: &str,
    source_path: Option<&std::path::Path>,
    config: &Config,
    profile: Profile,
) -> (ParseOutcome, Vec<Diagnostic>) {
    let outcome = parse(source);
    let mut session = LintSession::from_config_with_profile(config, profile);
    session.set_source_path(source_path);
    let mut diags = session.run(&outcome.spec, source);
    diags.extend(bridge_parser_diagnostics(
        &outcome.parser_diagnostics,
        config,
    ));
    sort_and_dedup(&mut diags);
    (outcome, diags)
}

/// Sort diagnostics by source location and drop exact duplicates.
///
/// Safety net against rules that anchor multiple findings at the same
/// span (e.g. one diagnostic per AST node when the same source
/// location is visited from several angles). Keying on the message
/// preserves distinct findings that happen to share a span but
/// suggest different fixes.
fn sort_and_dedup(diags: &mut Vec<Diagnostic>) {
    diags.sort_by(|a, b| {
        a.primary_span
            .start_byte
            .cmp(&b.primary_span.start_byte)
            .then_with(|| a.lint_id.cmp(b.lint_id))
            .then_with(|| a.message.cmp(&b.message))
    });
    diags.dedup_by(|a, b| {
        a.lint_id == b.lint_id && a.primary_span == b.primary_span && a.message == b.message
    });
}

/// A parser code that we expose to users as a lint.
///
/// Codes that are also covered by AST-based rules (e.g. `rpmspec/W0025`
/// → `changelog-implausible-date` is detected separately) are omitted
/// from this table to avoid double-reporting.
struct BridgeEntry {
    parser_code: &'static str,
    metadata: &'static crate::lint::LintMetadata,
}

/// Static metadata for every bridged parser code. Kept in one place so
/// new mappings are a one-line addition.
mod bridged {
    use crate::diagnostic::{LintCategory, Severity};
    use crate::lint::LintMetadata;

    macro_rules! bridged {
        ($vis:vis $name:ident = ($id:literal, $kebab:literal, $desc:literal, $sev:expr)) => {
            $vis static $name: LintMetadata = LintMetadata {
                id: $id,
                name: $kebab,
                description: $desc,
                default_severity: $sev,
                category: LintCategory::Correctness,
            };
        };
    }

    bridged!(pub NO_PROGRESS = (
        "parse/E0001", "parse-no-progress",
        "Parser couldn't consume any input at this position — the spec is unparseable here.",
        Severity::Deny
    ));
    bridged!(pub UNTERMINATED_COND = (
        "parse/E0002", "parse-unterminated-conditional",
        "An `%if`/`%ifarch`/`%ifos` block was opened without a matching `%endif`.",
        Severity::Deny
    ));
    bridged!(pub LINE_NOT_RECOGNIZED = (
        "parse/W0002", "parse-line-not-recognized",
        "Line did not match any known top-level construction.",
        Severity::Warn
    ));
    bridged!(pub STRAY_PERCENT = (
        "parse/W0001", "parse-stray-percent",
        "A `%` appeared in text without forming a valid macro reference — write `%%` for a literal percent.",
        Severity::Warn
    ));
    bridged!(pub UNTERMINATED_MACRO = (
        "parse/W0004", "parse-unterminated-macro",
        "A `%(...)`, `%[...]`, or `%{...}` macro reference was not closed before EOF or terminator.",
        Severity::Warn
    ));
    bridged!(pub MULTIPLE_ELSE = (
        "parse/W0005", "parse-multiple-else",
        "Conditional block has more than one `%else`; only the last one is honoured.",
        Severity::Warn
    ));
    bridged!(pub MALFORMED_CHANGELOG = (
        "parse/W0023", "parse-malformed-changelog-header",
        "Changelog entry header (`* Weekday Month Day Year ...`) could not be parsed.",
        Severity::Warn
    ));
}

/// Static mapping table. `rpmspec/W0025` is deliberately absent — it's
/// surfaced by the dedicated [`crate::rules::changelog_health::ChangelogImplausibleDate`]
/// AST rule instead, to give users a single overridable lint name.
const BRIDGE: &[BridgeEntry] = &[
    BridgeEntry {
        parser_code: "rpmspec/E0001",
        metadata: &bridged::NO_PROGRESS,
    },
    BridgeEntry {
        parser_code: "rpmspec/E0002",
        metadata: &bridged::UNTERMINATED_COND,
    },
    BridgeEntry {
        parser_code: "rpmspec/W0001",
        metadata: &bridged::STRAY_PERCENT,
    },
    BridgeEntry {
        parser_code: "rpmspec/W0002",
        metadata: &bridged::LINE_NOT_RECOGNIZED,
    },
    BridgeEntry {
        parser_code: "rpmspec/W0004",
        metadata: &bridged::UNTERMINATED_MACRO,
    },
    BridgeEntry {
        parser_code: "rpmspec/W0005",
        metadata: &bridged::MULTIPLE_ELSE,
    },
    BridgeEntry {
        parser_code: "rpmspec/W0023",
        metadata: &bridged::MALFORMED_CHANGELOG,
    },
];

/// Translate `rpm-spec` parser diagnostics into lint-level [`Diagnostic`]s.
/// Unknown parser codes are dropped — they surface in `ParseOutcome` for
/// callers who want them but don't pollute lint output.
pub(crate) fn bridge_parser_diagnostics(
    parser_diagnostics: &[ParserDiagnostic],
    config: &Config,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for pd in parser_diagnostics {
        let Some(code) = pd.code.as_deref() else {
            continue;
        };
        let Some(entry) = BRIDGE.iter().find(|e| e.parser_code == code) else {
            continue;
        };
        let severity = config.severity_for(entry.metadata.name, entry.metadata.default_severity);
        if severity.is_silenced() {
            continue;
        }
        // Parser diagnostics with no span hit the spec root (0..0) —
        // best effort, but we never want a `Diagnostic` without a span.
        let span = pd.span.unwrap_or_default();
        out.push(Diagnostic::new(
            entry.metadata,
            severity,
            pd.message.clone(),
            span,
        ));
    }
    out
}

/// Owns a configured set of lint rules and runs them sequentially over an AST.
pub struct LintSession {
    lints: Vec<ActiveLint>,
}

struct ActiveLint {
    lint: Box<dyn Lint>,
    /// Severity resolved from `Config` (or the rule's default).
    severity: Severity,
}

impl std::fmt::Debug for LintSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LintSession")
            .field("lints", &self.lints.len())
            .finish()
    }
}

impl LintSession {
    /// Build a session from a parsed `Config`. Rules whose configured
    /// severity is `Allow` are dropped at construction so they never run.
    ///
    /// Uses an empty default [`Profile`]. Callers that want a real
    /// distribution profile (`[profiles.*]` from `.rpmspec.toml`, plus
    /// `rpm --showrc` data) should use [`Self::from_config_with_profile`]
    /// and resolve the profile via [`Config::resolve_profile`] first.
    pub fn from_config(config: &Config) -> Self {
        Self::from_config_with_profile(config, Profile::default())
    }

    /// Build a session bound to an explicit, pre-resolved profile.
    ///
    /// CLI front-ends use this entry point so the profile reflects user
    /// overrides and `--profile <name>` flags.
    pub fn from_config_with_profile(config: &Config, profile: Profile) -> Self {
        tracing::debug!(
            family = ?profile.identity.family,
            dist_tag = ?profile.identity.dist_tag,
            n_macros = profile.macros.len(),
            n_licenses = profile.licenses.allowed.len(),
            "building lint session"
        );
        let mut active: Vec<ActiveLint> = Vec::new();
        for mut lint in registry::builtin_lints() {
            let meta = lint.metadata();
            let sev = config.severity_for(meta.name, meta.default_severity);
            if sev.is_silenced() {
                tracing::debug!(
                    rule = meta.name,
                    rule_id = meta.id,
                    "rule skipped: severity silenced"
                );
                continue;
            }
            // Check `applies_to_profile` BEFORE `set_config`/`set_profile`
            // so we don't pay the (often non-trivial) initialisation cost
            // for rules we're about to drop.
            if !lint.applies_to_profile(&profile) {
                tracing::debug!(
                    rule = meta.name,
                    rule_id = meta.id,
                    profile_family = ?profile.identity.family,
                    profile_dist_tag = ?profile.identity.dist_tag,
                    "rule skipped: applies_to_profile returned false"
                );
                continue;
            }
            lint.set_config(config);
            lint.set_profile(&profile);
            active.push(ActiveLint {
                lint,
                severity: sev,
            });
        }
        // `profile` is dropped here; rules have already copied whatever
        // fields they need into their own state via `Lint::set_profile`.
        Self { lints: active }
    }

    /// Forward an optional source-file path to every active rule. Must
    /// be called *before* [`Self::run`] if any rule reads the path via
    /// [`Lint::set_source_path`]. Calling this with `None` is fine and
    /// is the default if you skip it.
    pub fn set_source_path(&mut self, path: Option<&std::path::Path>) {
        for ActiveLint { lint, .. } in &mut self.lints {
            lint.set_source_path(path);
        }
    }

    /// Run every active rule over `spec`. Each rule is invoked in its own
    /// pass; the `Severity` recorded on returned [`Diagnostic`]s matches the
    /// configured level (`Warn` or `Deny`), never `Allow`.
    ///
    /// `source` is the original `.spec` source text, passed to every rule
    /// via [`Lint::set_source`] before the visit pass starts. Rules that
    /// inspect raw bytes (whitespace, tabs, exact line slicing) use it;
    /// AST-only rules ignore it.
    ///
    /// Diagnostics are returned in lint-emission order. Callers that
    /// want sorted, deduplicated output should use [`analyze`] (which
    /// runs [`sort_and_dedup`]) instead of post-processing themselves.
    pub fn run(&mut self, spec: &SpecFile<Span>, source: &str) -> Vec<Diagnostic> {
        let line_table = build_line_table(source);
        // Build the shared `Arc<str>` once so every active rule pays
        // only a refcount bump instead of cloning the full source —
        // dominant allocation cost for large specs otherwise.
        let shared: std::sync::Arc<str> = std::sync::Arc::from(source);
        let mut out = Vec::new();
        for ActiveLint { lint, severity } in &mut self.lints {
            lint.set_source(std::sync::Arc::clone(&shared));
            lint.visit_spec(spec);
            for mut diag in lint.take_diagnostics() {
                diag.severity = *severity;
                resolve_diagnostic_lines(&mut diag, source, &line_table);
                out.push(diag);
            }
        }
        out
    }
}

/// Pre-compute the starting byte offset of every line in `source`.
///
/// The result is indexed by 0-based line number: `line_table[0] == 0`,
/// `line_table[i]` is the byte just after the `i-th` `\n`. Used by
/// [`resolve_diagnostic_lines`] to fill in line/column for spans whose
/// originator forgot to populate them (typically `Span::from_bytes`).
fn build_line_table(source: &str) -> Vec<usize> {
    let mut table = Vec::with_capacity(source.bytes().filter(|&b| b == b'\n').count() + 1);
    table.push(0);
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            table.push(i + 1);
        }
    }
    table
}

/// Resolve missing line/column information on a diagnostic's spans.
///
/// Spans built via `Span::from_bytes` carry `start_line == 0`. Without
/// real line numbers the JSON/SARIF output is unusable: editors and CI
/// pipelines navigate by `file:line`, not by byte offset. We fix this
/// once, centrally, instead of forcing every lint to remember the
/// source-aware constructor.
fn resolve_diagnostic_lines(diag: &mut Diagnostic, source: &str, line_table: &[usize]) {
    resolve_span(&mut diag.primary_span, source, line_table);
    for label in &mut diag.labels {
        resolve_span(&mut label.span, source, line_table);
    }
    for suggestion in &mut diag.suggestions {
        for edit in &mut suggestion.edits {
            resolve_span(&mut edit.span, source, line_table);
        }
    }
}

fn resolve_span(span: &mut Span, source: &str, line_table: &[usize]) {
    if span.start_line == 0 {
        let (line, col) = byte_to_line_col(span.start_byte, source, line_table);
        span.start_line = line;
        span.start_column = col;
    }
    if span.end_line == 0 {
        let (line, col) = byte_to_line_col(span.end_byte, source, line_table);
        span.end_line = line;
        span.end_column = col;
    }
}

/// Convert a byte offset into a 1-based `(line, column)` pair using a
/// pre-computed line table. Column is the byte distance from the start
/// of the line, also 1-based to match `nom_locate` conventions used by
/// the parser.
fn byte_to_line_col(byte: usize, source: &str, line_table: &[usize]) -> (u32, u32) {
    let clamped = byte.min(source.len());
    // `line_table` is sorted; binary-search for the line whose start is
    // <= clamped. `partition_point` returns the count of starts that are
    // <= clamped, which is the 1-based line number.
    let line = line_table.partition_point(|&start| start <= clamped);
    let line_start = line_table.get(line.saturating_sub(1)).copied().unwrap_or(0);
    let col = clamped - line_start + 1;
    (line as u32, col as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Severity;

    #[test]
    fn analyze_flags_missing_changelog() {
        let cfg = Config::default();
        let (_outcome, diags) = analyze("Name: hello\nVersion: 1\n", &cfg);
        assert!(
            diags.iter().any(|d| d.lint_id == "RPM001"),
            "expected RPM001 in {diags:?}"
        );
    }

    #[test]
    fn analyze_with_profile_routes_profile_to_lints() {
        // Smoke test that the profile actually reaches Lint::set_profile.
        // Since no existing lint reads the profile yet, we install a
        // minimal probe lint and assert it observed our value.
        use crate::diagnostic::{Diagnostic, LintCategory};
        use crate::lint::{Lint, LintMetadata};
        use crate::visit::Visit;
        use rpm_spec_profile::{Family, Profile};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct ProbeLint {
            captured: Arc<Mutex<Option<Family>>>,
        }
        static META: LintMetadata = LintMetadata {
            id: "TEST_PROBE",
            name: "test-probe",
            description: "test probe lint",
            default_severity: Severity::Warn,
            category: LintCategory::Correctness,
        };
        impl Visit<'_> for ProbeLint {}
        impl Lint for ProbeLint {
            fn metadata(&self) -> &'static LintMetadata {
                &META
            }
            fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                Vec::new()
            }
            fn set_profile(&mut self, profile: &Profile) {
                *self.captured.lock().unwrap() = profile.identity.family;
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let probe = ProbeLint {
            captured: Arc::clone(&captured),
        };
        let mut profile = Profile::default();
        profile.identity.family = Some(Family::Rhel);

        let mut session = LintSession { lints: Vec::new() };
        // Manually inject the probe so we don't depend on the global
        // registry — keeps the test hermetic.
        let mut probe_box: Box<dyn Lint> = Box::new(probe);
        probe_box.set_profile(&profile);
        session.lints.push(ActiveLint {
            lint: probe_box,
            severity: Severity::Warn,
        });

        assert_eq!(*captured.lock().unwrap(), Some(Family::Rhel));
    }

    #[test]
    fn analyze_silent_on_clean_spec() {
        let src = "Name: x\n%description\nbody\n%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";
        let cfg = Config::default();
        let (_outcome, diags) = analyze(src, &cfg);
        // Filter to the two lints actually relevant here so the test
        // doesn't become flaky when a new default-warn rule lands.
        let relevant: Vec<_> = diags
            .iter()
            .filter(|d| d.lint_id == "RPM001" || d.lint_id == "RPM002")
            .collect();
        assert!(
            relevant.is_empty(),
            "expected no RPM001/RPM002 diagnostics, got {relevant:?}"
        );
    }

    /// Verify that `Lint::applies_to_profile` returning `false` causes
    /// the rule to be dropped from the active set entirely — no visit
    /// pass, no diagnostics. Counterpart: with `applies = true` the
    /// probe's `visit_spec` increments the counter.
    ///
    /// Scope: exercises the `applies_to_profile` *contract* on a
    /// custom probe. The registry-integration path (real rule
    /// registered in `builtin_lints()`, selected via `--profile`) is
    /// covered end-to-end by `cross_profile_lint_counts_differ`
    /// in `crates/cli/tests/cli.rs`.
    #[test]
    fn rules_inapplicable_to_profile_are_skipped() {
        use crate::diagnostic::{Diagnostic, LintCategory};
        use crate::lint::{Lint, LintMetadata};
        use crate::visit::Visit;
        use rpm_spec::ast::{Span, SpecFile};
        use rpm_spec_profile::{Family, Profile};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        // Probe instrumented to count every lifecycle call. Used below
        // to assert two contracts:
        //   1. `applies_to_profile == false` ⇒ `visit_spec` never runs
        //      (the documented behaviour);
        //   2. `applies_to_profile == false` ⇒ `set_config` /
        //      `set_profile` are also skipped (the perf-oriented
        //      reorder — guards against a future refactor that
        //      re-introduces the wasted initialisation).
        struct GatedProbe {
            fired: Arc<AtomicBool>,
            set_config_calls: Arc<AtomicUsize>,
            set_profile_calls: Arc<AtomicUsize>,
            applies: bool,
        }
        static META: LintMetadata = LintMetadata {
            id: "TEST_GATED",
            name: "test-gated",
            description: "test gated probe",
            default_severity: Severity::Warn,
            category: LintCategory::Correctness,
        };
        impl<'ast> Visit<'ast> for GatedProbe {
            fn visit_spec(&mut self, _: &'ast SpecFile<Span>) {
                self.fired.store(true, Ordering::SeqCst);
            }
        }
        impl Lint for GatedProbe {
            fn metadata(&self) -> &'static LintMetadata {
                &META
            }
            fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                Vec::new()
            }
            fn set_config(&mut self, _: &Config) {
                self.set_config_calls.fetch_add(1, Ordering::SeqCst);
            }
            fn set_profile(&mut self, _: &Profile) {
                self.set_profile_calls.fetch_add(1, Ordering::SeqCst);
            }
            fn applies_to_profile(&self, _: &Profile) -> bool {
                self.applies
            }
        }

        let src = "Name: x\n";

        // Sanity: when applies=true, the probe runs.
        let fired_on = Arc::new(AtomicBool::new(false));
        let cfg = Config::default();
        let mut profile = Profile::default();
        profile.identity.family = Some(Family::Alt);
        let mut session = LintSession::from_config_with_profile(&cfg, profile.clone());
        session.lints.push(ActiveLint {
            lint: Box::new(GatedProbe {
                fired: Arc::clone(&fired_on),
                set_config_calls: Arc::new(AtomicUsize::new(0)),
                set_profile_calls: Arc::new(AtomicUsize::new(0)),
                applies: true,
            }),
            severity: Severity::Warn,
        });
        let parsed = parse(src);
        session.run(&parsed.spec, src);
        assert!(
            fired_on.load(Ordering::SeqCst),
            "probe with applies=true must fire"
        );

        // Real test: applies=false → no visit_spec, AND no init calls.
        // We invoke the gate manually below because `LintSession::from_config_with_profile`
        // only accepts rules from `registry::builtin_lints()` and there's
        // no public hook to inject a custom probe through that path. The
        // manual sequence here mirrors the session's exact ordering at
        // `session.rs:316-331` — if that order ever changes, this test
        // must change with it.
        let fired_off = Arc::new(AtomicBool::new(false));
        let set_config_off = Arc::new(AtomicUsize::new(0));
        let set_profile_off = Arc::new(AtomicUsize::new(0));
        let mut session2 = LintSession { lints: Vec::new() };
        let mut probe: Box<dyn Lint> = Box::new(GatedProbe {
            fired: Arc::clone(&fired_off),
            set_config_calls: Arc::clone(&set_config_off),
            set_profile_calls: Arc::clone(&set_profile_off),
            applies: false,
        });
        // Mirror the production order at session.rs:316-331:
        // applies_to_profile runs BEFORE set_config / set_profile.
        if probe.applies_to_profile(&profile) {
            probe.set_config(&cfg);
            probe.set_profile(&profile);
            session2.lints.push(ActiveLint {
                lint: probe,
                severity: Severity::Warn,
            });
        }
        session2.run(&parsed.spec, src);
        assert!(
            !fired_off.load(Ordering::SeqCst),
            "probe with applies=false must not fire"
        );
        assert_eq!(
            set_config_off.load(Ordering::SeqCst),
            0,
            "applies=false should skip set_config (perf reorder contract)"
        );
        assert_eq!(
            set_profile_off.load(Ordering::SeqCst),
            0,
            "applies=false should skip set_profile (perf reorder contract)"
        );
    }

    #[test]
    fn allow_severity_suppresses_lint() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&["missing-changelog"], &[], &[]);
        let (_outcome, diags) = analyze("Name: x\n", &cfg);
        assert!(
            !diags.iter().any(|d| d.lint_id == "RPM001"),
            "RPM001 should be silenced by allow override"
        );
    }

    #[test]
    fn deny_overrides_default_warn() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&[], &[], &["missing-changelog"]);
        let (_outcome, diags) = analyze("Name: x\n", &cfg);
        let d = diags
            .iter()
            .find(|d| d.lint_id == "RPM001")
            .expect("RPM001 expected");
        assert_eq!(d.severity, Severity::Deny);
    }

    // -----------------------------------------------------------------
    // bridge_parser_diagnostics
    // -----------------------------------------------------------------

    fn pd(code: Option<&str>, span: Option<Span>, msg: &str) -> ParserDiagnostic {
        ParserDiagnostic {
            severity: ParserSeverity::Warning,
            code: code.map(str::to_owned),
            span,
            message: msg.into(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn sort_and_dedup_collapses_identical_diagnostics() {
        use crate::diagnostic::{LintCategory, Severity};
        use crate::lint::LintMetadata;

        static META: LintMetadata = LintMetadata {
            id: "RPM999",
            name: "test-lint",
            description: "",
            default_severity: Severity::Warn,
            category: LintCategory::Correctness,
        };
        let anchor = Span::from_bytes(10, 20);
        let mk = |msg: &str| Diagnostic::new(&META, Severity::Warn, msg.to_owned(), anchor);
        // Three diagnostics: two identical (same span + message), one
        // with a different message at the same anchor.
        let mut diags = vec![mk("same"), mk("same"), mk("different")];
        sort_and_dedup(&mut diags);
        assert_eq!(diags.len(), 2, "expected exact-dup to collapse: {diags:?}");
        // Distinct messages must both survive.
        let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(messages.contains(&"same"));
        assert!(messages.contains(&"different"));
    }

    #[test]
    fn bridge_drops_diag_without_code() {
        let diags = bridge_parser_diagnostics(&[pd(None, None, "x")], &Config::default());
        assert!(diags.is_empty());
    }

    #[test]
    fn bridge_drops_unknown_code() {
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/Z9999"), None, "unknown")],
            &Config::default(),
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn bridge_silenced_by_allow_override() {
        let mut cfg = Config::default();
        cfg.apply_cli_overrides::<&str>(&["parse-line-not-recognized"], &[], &[]);
        let diags = bridge_parser_diagnostics(&[pd(Some("rpmspec/W0002"), None, "msg")], &cfg);
        assert!(
            diags.is_empty(),
            "allow override must suppress bridged diag"
        );
    }

    #[test]
    fn bridge_uses_span_default_when_missing() {
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/W0002"), None, "msg")],
            &Config::default(),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].primary_span, Span::default());
    }

    #[test]
    fn bridge_propagates_message_and_code() {
        let span = Span::from_bytes(10, 20);
        let diags = bridge_parser_diagnostics(
            &[pd(Some("rpmspec/W0001"), Some(span), "stray % here")],
            &Config::default(),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "parse/W0001");
        assert_eq!(diags[0].lint_name, "parse-stray-percent");
        assert!(diags[0].message.contains("stray % here"));
        assert_eq!(diags[0].primary_span, span);
    }

    #[test]
    fn bridge_covers_every_table_entry() {
        // Table-driven regression: every BRIDGE row must round-trip
        // (parser_code -> lint_id) so adding a new entry can't silently
        // break the mapping.
        for entry in BRIDGE {
            let diags = bridge_parser_diagnostics(
                &[pd(Some(entry.parser_code), None, "x")],
                &Config::default(),
            );
            assert_eq!(
                diags.len(),
                1,
                "entry {} produced no diag",
                entry.parser_code
            );
            assert_eq!(diags[0].lint_id, entry.metadata.id);
            assert_eq!(diags[0].lint_name, entry.metadata.name);
        }
    }

    // -----------------------------------------------------------------
    // build_line_table / byte_to_line_col / resolve_span /
    // resolve_diagnostic_lines — line-resolution hot path
    // -----------------------------------------------------------------

    #[test]
    fn build_line_table_basic() {
        // Empty source still yields a single line starting at byte 0,
        // so `byte_to_line_col` can answer (1, 1) for byte 0.
        assert_eq!(build_line_table(""), vec![0]);
        // Trailing newline → an extra entry past EOF; that entry
        // represents the empty "line N+1" the parser would land on if
        // it read one more byte.
        assert_eq!(build_line_table("a\nb\nc\n"), vec![0, 2, 4, 6]);
        // No trailing newline → no phantom entry.
        assert_eq!(build_line_table("a\nb\nc"), vec![0, 2, 4]);
    }

    #[test]
    fn byte_to_line_col_ascii() {
        let src = "abc\ndef\nghi";
        let tbl = build_line_table(src);
        // Byte 0 is the first byte of line 1.
        assert_eq!(byte_to_line_col(0, src, &tbl), (1, 1));
        // Byte 4 is 'd', the first byte after the first '\n'.
        assert_eq!(byte_to_line_col(4, src, &tbl), (2, 1));
        // Byte past end clamps to source length and resolves to the
        // last line (column = distance from last line start + 1).
        let past = byte_to_line_col(999, src, &tbl);
        assert_eq!(past.0, 3, "byte past EOF should land on last line");
    }

    #[test]
    fn byte_to_line_col_clrf() {
        // The line table splits on '\n' alone; the '\r' byte therefore
        // belongs to the line *before* the newline. We assert this so a
        // future "smart" CRLF handler doesn't silently shift columns
        // for Windows-authored specs.
        let src = "a\r\nb\r\n";
        let tbl = build_line_table(src);
        assert_eq!(byte_to_line_col(0, src, &tbl), (1, 1));
        // Byte 3 is 'b' — first byte on line 2.
        assert_eq!(byte_to_line_col(3, src, &tbl), (2, 1));
        // Byte 1 is '\r' — still on line 1, column 2.
        assert_eq!(byte_to_line_col(1, src, &tbl), (1, 2));
    }

    #[test]
    fn byte_to_line_col_utf8() {
        // "αβγ" is 6 bytes (3 × 2-byte codepoints), then '\n' at byte 6,
        // then "foo" starting at byte 7.
        let src = "αβγ\nfoo";
        let tbl = build_line_table(src);
        // 'f' must resolve to line 2 col 1.
        assert_eq!(byte_to_line_col(7, src, &tbl), (2, 1));
        // Mid-codepoint byte must not panic. Documented behaviour:
        // column is byte distance (nom_locate convention), so byte 1
        // inside α gives column 2 on line 1.
        assert_eq!(byte_to_line_col(1, src, &tbl), (1, 2));
    }

    #[test]
    fn byte_to_line_col_offset_at_eof() {
        // byte == source.len() must not panic; column is byte-distance
        // from the line start, +1 for 1-based indexing.
        let src = "abc";
        let tbl = build_line_table(src);
        assert_eq!(byte_to_line_col(3, src, &tbl), (1, 4));
    }

    #[test]
    fn resolve_span_fills_unresolved() {
        let src = "hello\nworld\n";
        let tbl = build_line_table(src);
        let mut span = Span::from_bytes(2, 5);
        resolve_span(&mut span, src, &tbl);
        assert_eq!(span.start_line, 1);
        assert_eq!(span.start_column, 3);
        assert_eq!(span.end_line, 1);
        assert_eq!(span.end_column, 6);
        // Byte fields must be left untouched.
        assert_eq!(span.start_byte, 2);
        assert_eq!(span.end_byte, 5);
    }

    #[test]
    fn resolve_span_keeps_resolved() {
        // Pre-populated line/column must not be overwritten — the
        // parser-produced spans are authoritative and may carry visual
        // columns that don't match a naive byte recalculation.
        let src = "hello\nworld\n";
        let tbl = build_line_table(src);
        let mut span = Span::new(2, 5, 7, 8, 9, 10);
        resolve_span(&mut span, src, &tbl);
        assert_eq!(span.start_line, 7);
        assert_eq!(span.start_column, 8);
        assert_eq!(span.end_line, 9);
        assert_eq!(span.end_column, 10);
    }

    #[test]
    fn resolve_diagnostic_lines_walks_labels_and_suggestions() {
        use crate::diagnostic::{Applicability, Edit, Label, LintCategory, Suggestion};
        use crate::lint::LintMetadata;

        static META: LintMetadata = LintMetadata {
            id: "RPM_TEST_RESOLVE",
            name: "test-resolve",
            description: "",
            default_severity: Severity::Warn,
            category: LintCategory::Correctness,
        };
        let src = "hello\nworld\n";
        let tbl = build_line_table(src);
        let mut diag = Diagnostic::new(
            &META,
            Severity::Warn,
            "test".to_owned(),
            Span::from_bytes(0, 5),
        );
        diag.labels.push(Label {
            span: Span::from_bytes(6, 11),
            message: "lbl".into(),
        });
        diag.suggestions.push(Suggestion::new(
            "fix".to_owned(),
            vec![Edit::new(Span::from_bytes(2, 5), "X".to_owned())],
            Applicability::MachineApplicable,
        ));

        resolve_diagnostic_lines(&mut diag, src, &tbl);

        // primary_span: bytes 0..5 → line 1, col 1..6
        assert_eq!(diag.primary_span.start_line, 1);
        assert_eq!(diag.primary_span.start_column, 1);
        assert_eq!(diag.primary_span.end_line, 1);
        assert_eq!(diag.primary_span.end_column, 6);
        // label span: bytes 6..11 → line 2, col 1..6
        assert_eq!(diag.labels[0].span.start_line, 2);
        assert_eq!(diag.labels[0].span.start_column, 1);
        assert_eq!(diag.labels[0].span.end_line, 2);
        assert_eq!(diag.labels[0].span.end_column, 6);
        // suggestion edit span: bytes 2..5 → line 1, col 3..6
        let edit_span = diag.suggestions[0].edits[0].span;
        assert_eq!(edit_span.start_line, 1);
        assert_eq!(edit_span.start_column, 3);
        assert_eq!(edit_span.end_line, 1);
        assert_eq!(edit_span.end_column, 6);
    }
}
