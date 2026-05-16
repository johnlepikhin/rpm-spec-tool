//! Phase 13 — `shellcheck` integration (RPM200 / RPM201).
//!
//! Runs the external `shellcheck` binary over every shell-bearing
//! section (`%prep`, `%build`, `%install`, `%check`, `%clean`,
//! `%generate_buildrequires`, scriptlets, triggers, `%verify`,
//! `%sepolicy`). Each finding is bridged into a single umbrella lint
//! `RPM200 / shellcheck`; the original SC code is preserved in the
//! message. A second metadata `RPM201 / shellcheck-unavailable`
//! surfaces tool-availability problems exactly once per session.
//!
//! Design notes:
//! - Macro references are masked **in-place** (replaced with `_` plus
//!   spaces of the matching length) before shellcheck sees the input,
//!   so byte columns stay roughly aligned and line numbers are
//!   preserved 1:1.
//! - Lines starting with `%if`/`%elif`/`%else`/`%endif`/`%ifarch`/
//!   `%ifos`/`%ifnarch`/`%ifnos` are blanked (they are stored as
//!   literal text lines inside `ShellBody` per `rpm-spec`'s design).
//! - The section header line (`%prep` etc.) is replaced with a
//!   `#!/bin/sh` shebang so shellcheck gets a syntactically valid
//!   shell script.
//! - One shellcheck process per section: simpler bookkeeping, no
//!   cross-section variable leaks, negligible perf cost (~50 ms each).

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rpm_spec::ast::{FileTrigger, Scriptlet, Section, Span, Trigger};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::config::Config;
use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

/// Default binary searched on `$PATH` when `[shellcheck].binary` is not set.
const DEFAULT_BINARY: &str = "shellcheck";

/// Default shell dialect. `bash` matches `/bin/sh` on RHEL/Fedora/Alt
/// /SUSE/openSUSE where rpm scriptlets actually run.
const DEFAULT_SHELL: &str = "bash";

/// Per-section timeout — a guard against a hung or malicious binary
/// (the binary path is user-controlled via TOML).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Polling cadence for `wait_with_timeout`.
const WAIT_POLL: Duration = Duration::from_millis(25);

/// Shell dialects accepted by `--shell=`. Validated at config-load
/// time so a typo (`shell = "fish"`) surfaces as a `warn!` rather
/// than mysterious shellcheck-side stderr.
const ALLOWED_SHELLS: &[&str] = &["sh", "bash", "dash", "ksh"];

// Named SC codes so the disable list reads as English next to the
// per-entry comment in `DEFAULT_DISABLED`.
const SC1090: u32 = 1090;
const SC1091: u32 = 1091;
const SC2050: u32 = 2050;
const SC2164: u32 = 2164;
const SC3044: u32 = 3044;

/// SC codes silenced by default because they are well-known noise in
/// the RPM build context. Users can override per-code via the
/// `[shellcheck].enable` config field.
///
/// - **SC2164**: `pushd … || exit`. rpm wraps `%prep`/`%build`/`%install`
///   bodies with `set -e -x`, so a failed `pushd` already aborts.
/// - **SC3044**: `pushd` is undefined in POSIX sh. We run with
///   `--shell=bash` so this never fires in practice, but the disable
///   stays as belt-and-suspenders for users who switch `shell = "sh"`.
/// - **SC1090**: `ShellCheck can't follow non-constant source`. rpm
///   scriptlets routinely `. $CONFIG` where the path is computed at
///   install time; static following would offer no value.
/// - **SC1091**: `Not following: …` for `source` / `.` of paths shellcheck
///   cannot resolve. rpm scriptlets routinely source `/etc/rpm/macros.d/*`
///   and other distro files; following them isn't useful here.
/// - **SC2050**: `This expression is constant`. After we mask macros to
///   underscore runs, comparisons like `[ "%{edition}" = "sdm" ]` look
///   like `[ "________" = "sdm" ]` — two literals — and shellcheck
///   flags every such comparison. Since this is purely an artefact of
///   masking, we silence it globally.
const DEFAULT_DISABLED: &[u32] = &[SC2164, SC3044, SC1090, SC1091, SC2050];

/// RPM200 — every shellcheck finding is funnelled through this metadata.
pub static SHELLCHECK_METADATA: LintMetadata = LintMetadata {
    id: "RPM200",
    name: "shellcheck",
    description: "Run shellcheck over %prep/%build/%install and scriptlet/trigger bodies; surface findings as diagnostics.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// RPM201 — emitted at most once per session when the shellcheck
/// binary is missing or its invocation fails (spawn error, timeout,
/// non-zero exit with empty stdout, unparseable JSON).
///
/// **Not independently configurable.** Diagnostics emitted under this
/// metadata are produced by the same [`ShellcheckLint`] instance as
/// RPM200 and inherit its resolved severity in
/// [`crate::session::LintSession::run`]. This means `shellcheck =
/// "allow"` silences RPM201 too (the lint is dropped before any
/// probing). The kebab-case `name` exists for parity with other
/// metadata, not for selective override.
pub static SHELLCHECK_UNAVAILABLE_METADATA: LintMetadata = LintMetadata {
    id: "RPM201",
    name: "shellcheck-unavailable",
    description: "shellcheck binary is unavailable or its invocation failed; the umbrella lint cannot run.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Tri-state cache for the shellcheck binary probe. Replaces the
/// earlier `Option<Result<PathBuf, String>>` anti-pattern so the three
/// states are visible in pattern matches.
#[derive(Debug)]
enum Availability {
    /// Probing has not been attempted yet.
    Unprobed,
    /// Binary is callable; this path is what we spawn.
    Available(PathBuf),
    /// Binary is unusable. Reason is preserved verbatim for diagnostics.
    Unavailable(String),
}

/// Typed errors from the subprocess + JSON path. Kept private — every
/// variant is folded into a single user-facing `RPM201` diagnostic.
#[derive(Debug, thiserror::Error)]
enum ShellcheckError {
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("subprocess pipe was unavailable (stdin/stdout/stderr)")]
    PipeMissing,
    #[error("wait failed: {0}")]
    Wait(#[source] std::io::Error),
    #[error("shellcheck exceeded the {:?} timeout and was killed", _0)]
    Timeout(Duration),
    #[error("exit {status} with empty stdout; stderr: {stderr}")]
    EmptyOutput { status: ExitStatus, stderr: String },
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

/// State for the RPM200 / RPM201 (`shellcheck`) lint.
///
/// One instance owns the binary probe result, the per-session config
/// fields, and the diagnostic buffer. Constructed via
/// [`crate::registry::builtin_lints`]; not intended for direct
/// construction by external crates.
///
/// **RPM201 severity** is *not* independently configurable. It is
/// emitted with `Severity::Warn` and then re-stamped to RPM200's
/// resolved severity by [`crate::session::LintSession::run`] — so
/// `shellcheck = "allow"` silences both, `shellcheck = "deny"` raises
/// both. This is intentional: the user controls the whole umbrella.
#[derive(Debug)]
pub struct ShellcheckLint {
    diagnostics: Vec<Diagnostic>,
    source: String,
    binary_override: Option<String>,
    shell: String,
    timeout: Duration,
    /// Effective set of suppressed SC codes (baseline ∪ user disable −
    /// user enable).
    disable: HashSet<u32>,
    /// Lazily probed binary; cached for the rest of the session.
    availability: Availability,
    /// True once an RPM201 diagnostic has been emitted; we never emit
    /// more than one even if probing is re-attempted later.
    unavailable_reported: bool,
    /// True once a runtime failure (process spawn, json parse) was
    /// reported; downstream sections are silently skipped in that case.
    fatal_reported: bool,
}

impl ShellcheckLint {
    /// Construct a fresh lint state. The shellcheck binary is **not**
    /// probed here — probing happens lazily on the first shell-bearing
    /// section visited, so a session with `shellcheck = "allow"`
    /// pays no subprocess cost.
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            source: String::new(),
            binary_override: None,
            shell: DEFAULT_SHELL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            disable: DEFAULT_DISABLED.iter().copied().collect(),
            availability: Availability::Unprobed,
            unavailable_reported: false,
            fatal_reported: false,
        }
    }

    /// Process a single shell body. `span` covers the entire section
    /// (header + body) in the original source; we slice the source
    /// directly rather than re-stringifying the AST so column/line
    /// arithmetic stays trivially correct.
    fn process_section(&mut self, span: Span) {
        if self.fatal_reported {
            return;
        }
        let binary = match self.ensure_available() {
            Some(path) => path.to_owned(),
            None => return,
        };
        let Some(section_text) = self.source.get(span.start_byte..span.end_byte) else {
            // Span misaligned with source bytes — programming error
            // somewhere in the parser/visitor pairing.
            debug!(
                target: "rpm_spec_analyzer::shellcheck",
                ?span,
                "section span out of range; skipping",
            );
            return;
        };
        if section_text.lines().count() < 2 {
            // Header only, no body to lint.
            return;
        }
        let (input, line_map) = build_shellcheck_input(section_text);
        let result = invoke_shellcheck(&binary, &self.shell, self.timeout, &input);
        match result {
            Ok(findings) => {
                debug!(
                    target: "rpm_spec_analyzer::shellcheck",
                    section_start = span.start_line,
                    finding_count = findings.len(),
                    "shellcheck section completed",
                );
                let mut diags = build_finding_diagnostics(
                    findings,
                    section_text,
                    &line_map,
                    span,
                    &self.disable,
                );
                self.diagnostics.append(&mut diags);
            }
            Err(err) => self.report_fatal(format!("{err}")),
        }
    }

    /// Probe the binary on first need. Result is cached; subsequent
    /// sections short-circuit on the cached error and emit RPM201 at
    /// most once.
    fn ensure_available(&mut self) -> Option<&Path> {
        if matches!(self.availability, Availability::Unprobed) {
            self.availability = match probe_binary(self.binary_override.as_deref()) {
                Ok(path) => {
                    debug!(
                        target: "rpm_spec_analyzer::shellcheck",
                        path = %path.display(),
                        "shellcheck binary probed successfully",
                    );
                    Availability::Available(path)
                }
                Err(reason) => Availability::Unavailable(reason),
            };
        }
        match &self.availability {
            Availability::Available(path) => Some(path.as_path()),
            Availability::Unavailable(reason) => {
                if !self.unavailable_reported {
                    self.unavailable_reported = true;
                    self.diagnostics.push(Diagnostic::new(
                        &SHELLCHECK_UNAVAILABLE_METADATA,
                        Severity::Warn,
                        format!(
                            "shellcheck binary is not available: {reason}; install shellcheck or set `shellcheck = \"allow\"` in .rpmspec.toml"
                        ),
                        Span::default(),
                    ));
                }
                None
            }
            Availability::Unprobed => None,
        }
    }

    fn report_fatal(&mut self, reason: String) {
        if !self.fatal_reported {
            self.fatal_reported = true;
            self.diagnostics.push(Diagnostic::new(
                &SHELLCHECK_UNAVAILABLE_METADATA,
                Severity::Warn,
                reason,
                Span::default(),
            ));
        }
    }
}

/// Translate `shellcheck` findings into RPM200 [`Diagnostic`]s.
///
/// Pulled out of `ShellcheckLint` to keep `process_section` free of
/// `&mut self` re-borrows after the slice is borrowed from
/// `self.source`. Callers append the result to their own diagnostic
/// buffer.
fn build_finding_diagnostics(
    findings: Vec<ShellcheckFinding>,
    section_text: &str,
    line_map: &[u32],
    span: Span,
    disable: &HashSet<u32>,
) -> Vec<Diagnostic> {
    // `build_shellcheck_input` always emits at least the shebang line,
    // so an empty `line_map` indicates a programming error upstream.
    debug_assert!(
        !line_map.is_empty(),
        "line_map must contain the shebang entry"
    );
    let line_offsets = compute_line_offsets(section_text);
    let mut out = Vec::with_capacity(findings.len());
    for f in findings {
        if disable.contains(&f.code) {
            continue;
        }
        // shellcheck reports 1-based line numbers in the *masked*
        // input. Translate to 1-based line in the source slice via
        // the line map; out-of-range falls back to the last mapped
        // line so we never anchor outside the section.
        let map_idx_start = f.line.saturating_sub(1) as usize;
        let map_idx_end = f.end_line.unwrap_or(f.line).saturating_sub(1) as usize;
        let src_line_in_slice = *line_map
            .get(map_idx_start)
            .or_else(|| line_map.last())
            .unwrap_or(&1);
        let src_end_in_slice = *line_map
            .get(map_idx_end)
            .or_else(|| line_map.last())
            .unwrap_or(&src_line_in_slice);
        // Convert 1-based slice line to 0-based index for byte lookup.
        let line_idx = src_line_in_slice.saturating_sub(1) as usize;
        let end_line_idx = src_end_in_slice.saturating_sub(1) as usize;
        let (start_byte_in_slice, line_end_in_slice) =
            line_range(&line_offsets, section_text, line_idx);
        let (_, end_byte_in_slice) = line_range(&line_offsets, section_text, end_line_idx);
        let abs_start = span.start_byte + start_byte_in_slice;
        let abs_end = span.start_byte + end_byte_in_slice.max(line_end_in_slice);
        let source_start_line = span.start_line.saturating_add(line_idx as u32);
        let source_end_line = span.start_line.saturating_add(end_line_idx as u32);
        let diag_span = Span::new(abs_start, abs_end, source_start_line, 0, source_end_line, 0);
        let message = format!("[SC{:04}:{}] {}", f.code, f.level, f.message);
        let mut diag = Diagnostic::new(
            &SHELLCHECK_METADATA,
            Severity::Warn, // overridden later by LintSession::run
            message,
            diag_span,
        );
        // shellcheck `fix` replacements describe byte-level edits
        // against the **masked** input we fed in, so a faithful
        // round-trip back to spec source would mis-align with the
        // user's macros. Surface their presence as a pointer to the
        // rule's wiki page; users get the actionable info without us
        // forging incorrect spans.
        if f.fix.is_some() {
            diag = diag.with_suggestion(Suggestion::new(
                format!(
                    "shellcheck has an auto-fix for SC{:04}; see https://www.shellcheck.net/wiki/SC{}",
                    f.code, f.code
                ),
                Vec::new(),
                Applicability::Manual,
            ));
        }
        out.push(diag);
    }
    out
}

impl Default for ShellcheckLint {
    fn default() -> Self {
        Self::new()
    }
}

impl<'ast> Visit<'ast> for ShellcheckLint {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        match node {
            Section::BuildScript { data, .. }
            | Section::Verify { data, .. }
            | Section::Sepolicy { data, .. } => {
                let span = *data;
                self.process_section(span);
            }
            _ => visit::walk_section(self, node),
        }
    }

    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        if node.from_file.is_some() {
            // Body lives in an external file we cannot inspect.
            return;
        }
        let span = node.data;
        self.process_section(span);
    }

    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        let span = node.data;
        self.process_section(span);
    }

    fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
        let span = node.data;
        self.process_section(span);
    }
}

impl Lint for ShellcheckLint {
    fn metadata(&self) -> &'static LintMetadata {
        &SHELLCHECK_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = source.to_owned();
    }
    fn set_config(&mut self, config: &Config) {
        self.binary_override = config.shellcheck.binary.clone();
        if let Some(s) = config.shellcheck.shell.as_deref() {
            if ALLOWED_SHELLS.contains(&s) {
                self.shell = s.to_owned();
            } else {
                warn!(
                    target: "rpm_spec_analyzer::shellcheck",
                    shell = %s,
                    allowed = ?ALLOWED_SHELLS,
                    "[shellcheck].shell value not recognised; keeping default `{}`",
                    DEFAULT_SHELL,
                );
            }
        }
        if let Some(secs) = config.shellcheck.timeout_secs {
            self.timeout = Duration::from_secs(secs);
        }
        // Effective disable = baseline ∪ user-disable − user-enable.
        // Unparseable entries (`"SC2O86"` with O instead of 0, etc.) are
        // skipped with a `warn!` so a silent typo never costs coverage.
        let mut disable: HashSet<u32> = DEFAULT_DISABLED.iter().copied().collect();
        for s in &config.shellcheck.disable {
            match parse_sc_code(s) {
                Some(code) => {
                    disable.insert(code);
                }
                None => warn!(
                    target: "rpm_spec_analyzer::shellcheck",
                    entry = %s,
                    "ignoring unparseable [shellcheck].disable entry; expected SC<n> or <n>",
                ),
            }
        }
        for s in &config.shellcheck.enable {
            match parse_sc_code(s) {
                Some(code) => {
                    disable.remove(&code);
                }
                None => warn!(
                    target: "rpm_spec_analyzer::shellcheck",
                    entry = %s,
                    "ignoring unparseable [shellcheck].enable entry; expected SC<n> or <n>",
                ),
            }
        }
        self.disable = disable;
    }
}

// ---------------------------------------------------------------------
// Input shaping
// ---------------------------------------------------------------------

/// Build the string fed to shellcheck **and** a 1-based source-line
/// map. `line_map[i] = source_line` where `i` is `shellcheck_line - 1`
/// (1-based source line within the section slice).
///
/// `%if/%elif/%else/%endif/%ifarch/%ifos/%ifnarch/%ifnos` lines are
/// dropped entirely rather than blanked: keeping them as empty lines
/// would split shell `\`-continuations spanning a conditional block
/// (e.g. `./configure --foo \ %if x \n --bar \ %endif \n --baz`) and
/// trip SC2215 on every conditionally-added flag.
fn build_shellcheck_input(slice: &str) -> (String, Vec<u32>) {
    let mut out = String::with_capacity(slice.len() + 16);
    let mut line_map: Vec<u32> = Vec::new();
    let mut src_line: u32 = 1;
    let mut first = true;
    for raw_line in slice.split_inclusive('\n') {
        let (line, terminator) = split_terminator(raw_line);
        if first {
            // Replace the section header with a shebang so shellcheck
            // treats the rest as a shell script.
            out.push_str("#!/bin/bash");
            out.push_str(terminator);
            line_map.push(src_line);
            first = false;
            src_line += 1;
            continue;
        }
        if is_rpm_conditional_line(line) {
            src_line += 1;
            continue;
        }
        out.push_str(&mask_macros(line));
        out.push_str(terminator);
        line_map.push(src_line);
        src_line += 1;
    }
    (out, line_map)
}

fn split_terminator(raw: &str) -> (&str, &str) {
    if let Some(stripped) = raw.strip_suffix("\r\n") {
        (stripped, "\r\n")
    } else if let Some(stripped) = raw.strip_suffix('\n') {
        (stripped, "\n")
    } else {
        (raw, "")
    }
}

/// True for `%if`, `%elif`, `%else`, `%endif`, `%ifarch`, `%ifos`,
/// `%ifnarch`, `%ifnos` (possibly preceded by whitespace). These appear
/// as literal text inside `ShellBody` and would otherwise confuse
/// shellcheck.
fn is_rpm_conditional_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    const KEYWORDS: &[&str] = &[
        "%ifarch", "%ifnarch", "%ifos", "%ifnos", "%elif", "%else", "%endif", "%if",
    ];
    for kw in KEYWORDS {
        if let Some(rest) = trimmed.strip_prefix(kw)
            && (rest.is_empty()
                || rest.starts_with(|c: char| c.is_whitespace())
                || rest.starts_with('#'))
        {
            return true;
        }
    }
    false
}

/// Replace every macro reference with a run of `_` characters of the
/// same byte length. Using `_` rather than spaces is critical inside
/// test brackets — `[ -f %{var}/path ]` masks to `[ -f _____/path ]`
/// which shellcheck parses as one path argument, whereas
/// `[ -f      /path ]` would parse as two arguments and trigger
/// SC1072/SC1073 syntax errors.
///
/// Returns `Cow::Borrowed(line)` when no `%` is present (the common
/// case for shell-body lines without macros), avoiding allocation.
///
/// **Correctness:** copies non-`%` regions as whole `&str` slices so
/// multibyte UTF-8 codepoints survive intact. Earlier byte-by-byte
/// reconstruction via `push(b as char)` silently doubled non-ASCII
/// bytes (a `0xC3` lead byte became U+00C3, re-encoded as 2 bytes).
fn mask_macros(line: &str) -> Cow<'_, str> {
    let bytes = line.as_bytes();
    if !bytes.contains(&b'%') {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut run_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        // Flush the verbatim run ending just before this `%`.
        if run_start < i {
            out.push_str(&line[run_start..i]);
        }
        // `%%` escapes a literal percent in spec syntax.
        if bytes.get(i + 1) == Some(&b'%') {
            out.push_str("%%");
            i += 2;
            run_start = i;
            continue;
        }
        if let Some(end) = scan_macro(&bytes[i..]) {
            for _ in 0..end {
                out.push('_');
            }
            i += end;
            run_start = i;
            continue;
        }
        // Stray `%` — keep as is; shellcheck will treat it as text.
        out.push('%');
        i += 1;
        run_start = i;
    }
    if run_start < bytes.len() {
        out.push_str(&line[run_start..]);
    }
    Cow::Owned(out)
}

/// Returns the byte length of the macro starting at `bytes[0] == b'%'`,
/// or `None` if no macro form is recognised. Handles balanced `{}`,
/// `()`, `[]` for delimited forms.
fn scan_macro(bytes: &[u8]) -> Option<usize> {
    debug_assert!(bytes.first() == Some(&b'%'));
    if bytes.len() < 2 {
        return None;
    }
    match bytes[1] {
        b'{' => scan_balanced(bytes, 1, b'{', b'}'),
        b'(' => scan_balanced(bytes, 1, b'(', b')'),
        b'[' => scan_balanced(bytes, 1, b'[', b']'),
        b'?' | b'!' => {
            // `%?foo` or `%!foo` — conditional plain form. Consume the
            // identifier that follows.
            let body_start = if bytes[1] == b'!' && bytes.get(2) == Some(&b'?') {
                3
            } else {
                2
            };
            scan_identifier(bytes, body_start)
        }
        b'a'..=b'z' | b'A'..=b'Z' | b'_' => scan_identifier(bytes, 1),
        b'0'..=b'9' | b'*' | b'#' => Some(2),
        _ => None,
    }
}

fn scan_balanced(bytes: &[u8], offset: usize, open: u8, close: u8) -> Option<usize> {
    debug_assert!(bytes.get(offset) == Some(&open));
    let mut depth: usize = 1;
    let mut i = offset + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    // Unterminated — consume the rest defensively so the masking still
    // makes forward progress.
    Some(bytes.len())
}

fn scan_identifier(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => i += 1,
            _ => break,
        }
    }
    if i == start { None } else { Some(i) }
}

// ---------------------------------------------------------------------
// Line offsets in the section slice
// ---------------------------------------------------------------------

/// Byte offsets (within the slice) of each line start.
fn compute_line_offsets(slice: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (idx, b) in slice.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(idx + 1);
        }
    }
    offsets
}

/// Returns `(line_start_byte, line_end_byte)` for the given line index
/// in the slice (clamped to the last line if out of range).
fn line_range(offsets: &[usize], slice: &str, line_idx: usize) -> (usize, usize) {
    let idx = line_idx.min(offsets.len().saturating_sub(1));
    let start = offsets[idx];
    let end = offsets.get(idx + 1).copied().unwrap_or(slice.len());
    let end = if end > start && slice.as_bytes().get(end - 1) == Some(&b'\n') {
        end - 1
    } else {
        end
    };
    (start, end)
}

// ---------------------------------------------------------------------
// Shellcheck JSON
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ShellcheckOutput {
    #[serde(rename = "comments")]
    findings: Vec<ShellcheckFinding>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellcheckFinding {
    line: u32,
    #[serde(default)]
    end_line: Option<u32>,
    level: String,
    code: u32,
    message: String,
    /// Presence of `fix` is enough — replacement bytes target the
    /// **masked** stream we fed shellcheck, not the user's spec, so
    /// faithful round-tripping isn't possible. We deserialize into a
    /// unit type and only key off `Option::is_some`.
    #[serde(default, deserialize_with = "deserialize_present")]
    fix: Option<()>,
}

/// Treat any non-null `fix` payload as `Some(())` without inspecting its
/// shape (which varies across shellcheck versions).
fn deserialize_present<'de, D>(deserializer: D) -> Result<Option<()>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::IgnoredAny;
    let opt = <Option<IgnoredAny>>::deserialize(deserializer)?;
    Ok(opt.map(|_| ()))
}

/// Probe shellcheck availability. Returns `Ok(path)` on a successful
/// `--version` invocation, `Err(reason)` otherwise.
///
/// Both stdout and stderr of `--version` are captured; stderr is
/// included in the error message when probing fails so operators
/// debugging CI see the actual loader error (`shellcheck: command not
/// found`, missing libraries, version mismatch) rather than a bare
/// exit status.
fn probe_binary(override_path: Option<&str>) -> Result<PathBuf, String> {
    let target: PathBuf = override_path.unwrap_or(DEFAULT_BINARY).into();
    let output = Command::new(&target)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot spawn `{}`: {e}", target.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();
        let stderr_part = if stderr_trimmed.is_empty() {
            String::new()
        } else {
            format!("; stderr: {stderr_trimmed}")
        };
        return Err(format!(
            "`{} --version` exited with status {}{}",
            target.display(),
            output.status,
            stderr_part,
        ));
    }
    Ok(target)
}

/// Run shellcheck on `input` and parse its JSON output.
///
/// Reliability notes:
/// - stdin is written on a side thread so this thread can drain
///   stdout/stderr in parallel — without that, large inputs (a
///   `%install` over the 64 KiB pipe buffer) used to deadlock waiting
///   for shellcheck to consume stdin while shellcheck was blocked on
///   a full stdout pipe.
/// - The child is killed if it does not exit within `timeout`. The
///   guard is also a defence-in-depth against an attacker-controlled
///   `[shellcheck].binary` pointing at a hung process.
/// - Non-zero exit is normal when findings are reported (shellcheck
///   exits 1); only process-level errors and unparseable JSON are
///   treated as failures.
fn invoke_shellcheck(
    binary: &Path,
    shell: &str,
    timeout: Duration,
    input: &str,
) -> Result<Vec<ShellcheckFinding>, ShellcheckError> {
    let mut child = Command::new(binary)
        .arg("--format=json1")
        .arg(format!("--shell={shell}"))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ShellcheckError::Spawn)?;

    let stdin = child.stdin.take().ok_or(ShellcheckError::PipeMissing)?;
    let stdout = child.stdout.take().ok_or(ShellcheckError::PipeMissing)?;
    let stderr = child.stderr.take().ok_or(ShellcheckError::PipeMissing)?;

    let input_bytes = input.as_bytes();
    let (stdout_buf, stderr_buf, status_result) = std::thread::scope(|s| {
        // Writer: best-effort. `BrokenPipe` is normal if shellcheck
        // bails early; we want to keep going to collect stdout/stderr.
        s.spawn(move || {
            let mut stdin = stdin;
            let _ = stdin.write_all(input_bytes);
            // `stdin` drops here, closing the pipe — shellcheck sees EOF.
        });
        let stdout_handle = s.spawn(move || drain_pipe(stdout));
        let stderr_handle = s.spawn(move || drain_pipe(stderr));
        let status_result = wait_with_timeout(&mut child, timeout);
        // Both reader threads unblock at EOF (process exit or kill).
        let stdout_buf = stdout_handle.join().unwrap_or_default();
        let stderr_buf = stderr_handle.join().unwrap_or_default();
        (stdout_buf, stderr_buf, status_result)
    });
    let status = status_result?;

    let stdout_str = String::from_utf8_lossy(&stdout_buf);
    if !stderr_buf.is_empty() {
        let stderr_str = String::from_utf8_lossy(&stderr_buf);
        let stderr_trimmed = stderr_str.trim();
        if !stderr_trimmed.is_empty() {
            debug!(
                target: "rpm_spec_analyzer::shellcheck",
                stderr = %stderr_trimmed,
                "shellcheck non-fatal stderr",
            );
        }
    }
    if stdout_str.trim().is_empty() {
        if status.success() {
            return Ok(Vec::new());
        }
        let stderr_str = String::from_utf8_lossy(&stderr_buf);
        return Err(ShellcheckError::EmptyOutput {
            status,
            stderr: stderr_str.trim().to_owned(),
        });
    }
    let parsed: ShellcheckOutput = serde_json::from_str(&stdout_str)?;
    Ok(parsed.findings)
}

/// Read a captured child pipe to EOF, swallowing IO errors. Used by
/// the parallel-reader threads in [`invoke_shellcheck`] — if the read
/// fails (process killed, pipe broken), the partial buffer is still
/// usable for diagnostics.
fn drain_pipe<R: Read>(mut pipe: R) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    buf
}

/// Poll `child.try_wait()` until the process exits or `timeout`
/// elapses. On timeout the child is killed and reaped before
/// returning [`ShellcheckError::Timeout`].
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<ExitStatus, ShellcheckError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(ShellcheckError::Wait)? {
            Some(status) => return Ok(status),
            None => {
                if Instant::now() >= deadline {
                    // Ignore kill errors (process may already be dead).
                    let _ = child.kill();
                    // Always reap to avoid leaving a zombie.
                    let _ = child.wait();
                    return Err(ShellcheckError::Timeout(timeout));
                }
                std::thread::sleep(WAIT_POLL);
            }
        }
    }
}

/// Accept `"SC2086"`, `"sc2086"`, or `"2086"` and yield `2086`.
fn parse_sc_code(s: &str) -> Option<u32> {
    let s = s.trim();
    let digits = s
        .strip_prefix("SC")
        .or_else(|| s.strip_prefix("sc"))
        .or_else(|| s.strip_prefix("Sc"))
        .unwrap_or(s);
    digits.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_macros_pads_to_length() {
        let input = "rm -rf %{buildroot}/usr";
        let out = mask_macros(input);
        // `%{buildroot}` is 12 bytes → 12 underscores.
        assert_eq!(out, "rm -rf ____________/usr");
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn mask_macros_keeps_test_expression_one_token() {
        // Regression: padding with spaces previously split a single
        // path-argument inside `[ ... ]` into two tokens and provoked
        // SC1072/SC1073/SC1076 parser errors.
        let out = mask_macros("if [ -f %{pgdatadir}/PG_VERSION ]; then");
        assert!(!out.contains("  "), "no double-space leaks into [: {out}");
        assert!(out.contains("_/PG_VERSION"));
    }

    #[test]
    fn mask_macros_handles_plain_form() {
        // %name (5 bytes) → "_____"; %{?foo} (7 bytes) → "_______".
        let out = mask_macros("echo %name and %{?foo}");
        assert_eq!(out, "echo _____ and _______");
    }

    #[test]
    fn mask_macros_keeps_double_percent() {
        let out = mask_macros("printf '%%d\\n' 1");
        assert_eq!(out, "printf '%%d\\n' 1");
    }

    #[test]
    fn mask_macros_handles_shell_macro() {
        let out = mask_macros("ver=%(date +%Y)");
        assert_eq!(out.len(), "ver=%(date +%Y)".len());
        assert!(out.starts_with("ver=_"));
    }

    #[test]
    fn mask_macros_handles_expr_macro() {
        let out = mask_macros("v=%[1 + 2]");
        assert_eq!(out.len(), "v=%[1 + 2]".len());
        assert!(out.starts_with("v=_"));
    }

    #[test]
    fn mask_macros_borrows_when_no_percent() {
        let line = "echo hello world";
        let out = mask_macros(line);
        assert!(matches!(out, Cow::Borrowed(_)), "expected zero-copy path");
        assert_eq!(out, line);
    }

    #[test]
    fn mask_macros_preserves_multibyte_utf8() {
        // Regression: the previous `b as char` byte loop turned every
        // non-ASCII byte (e.g. `0xC3` of `é`) into a double-byte U+00C3
        // codepoint, breaking byte-length invariants and line-number
        // alignment for shellcheck.
        let line = "echo привет %{name} мир";
        let out = mask_macros(line);
        assert_eq!(out.len(), line.len(), "byte length must be preserved");
        // Non-ASCII text is copied verbatim.
        assert!(out.contains("привет"));
        assert!(out.contains("мир"));
        // The macro span is masked.
        assert!(out.contains("_______"));
    }

    #[test]
    fn is_rpm_conditional_recognises_keywords() {
        assert!(is_rpm_conditional_line("%if 0%{?rhel}"));
        assert!(is_rpm_conditional_line("  %else"));
        assert!(is_rpm_conditional_line("%endif"));
        assert!(is_rpm_conditional_line("%ifarch x86_64"));
        assert!(!is_rpm_conditional_line("echo %if"));
        assert!(!is_rpm_conditional_line("%install"));
    }

    #[test]
    fn build_input_replaces_header_with_shebang() {
        let slice = "%install\nmkdir -p $RPM_BUILD_ROOT\n";
        let (out, map) = build_shellcheck_input(slice);
        assert!(out.starts_with("#!/bin/bash\n"));
        assert!(out.contains("mkdir -p $RPM_BUILD_ROOT"));
        // Two emitted lines: header → src line 1, body → src line 2.
        assert_eq!(map, vec![1, 2]);
    }

    #[test]
    fn build_input_strips_conditional_lines() {
        let slice = "%install\n%if 0%{?foo}\necho yes\n%endif\n";
        let (out, map) = build_shellcheck_input(slice);
        let lines: Vec<&str> = out.lines().collect();
        // Only the shebang and `echo yes` survive; %if/%endif disappear.
        assert_eq!(lines, vec!["#!/bin/bash", "echo yes"]);
        // shellcheck line 2 maps to source slice line 3 (echo yes).
        assert_eq!(map, vec![1, 3]);
    }

    #[test]
    fn build_input_preserves_continuation_across_if() {
        // Real-world regression: `./configure \ %if ... \ --flag \ %endif`
        // used to break continuation and trigger SC2215 on every
        // conditionally-added flag.
        let slice = "%build\n./configure \\\n  --foo \\\n%if 1\n  --bar \\\n%endif\n  --baz\n";
        let (out, _) = build_shellcheck_input(slice);
        // The reduced stream should be one unbroken continuation chain.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "#!/bin/bash");
        assert_eq!(lines[1], "./configure \\");
        assert_eq!(lines[2], "  --foo \\");
        assert_eq!(lines[3], "  --bar \\");
        assert_eq!(lines[4], "  --baz");
    }

    #[test]
    fn parse_sc_code_accepts_variants() {
        assert_eq!(parse_sc_code("SC2086"), Some(2086));
        assert_eq!(parse_sc_code("sc2086"), Some(2086));
        assert_eq!(parse_sc_code("2086"), Some(2086));
        assert_eq!(parse_sc_code("  SC2086  "), Some(2086));
        assert_eq!(parse_sc_code("nope"), None);
    }

    #[test]
    fn line_range_returns_slice_offsets() {
        let slice = "abc\ndef\nghi";
        let offsets = compute_line_offsets(slice);
        assert_eq!(offsets, vec![0, 4, 8]);
        assert_eq!(line_range(&offsets, slice, 0), (0, 3));
        assert_eq!(line_range(&offsets, slice, 1), (4, 7));
        assert_eq!(line_range(&offsets, slice, 2), (8, 11));
        // Out-of-range clamps to last line.
        assert_eq!(line_range(&offsets, slice, 99), (8, 11));
    }

    #[test]
    fn set_config_normalizes_disable() {
        let mut lint = ShellcheckLint::new();
        let mut cfg = Config::default();
        cfg.shellcheck.disable = vec!["SC2086".into(), "2155".into(), "bogus".into()];
        lint.set_config(&cfg);
        assert!(lint.disable.contains(&2086));
        assert!(lint.disable.contains(&2155));
        assert!(!lint.disable.contains(&0));
        // Baseline codes are still suppressed even when user only
        // populates `disable`.
        for code in DEFAULT_DISABLED {
            assert!(lint.disable.contains(code), "baseline SC{code} missing");
        }
    }

    #[test]
    fn enable_unsuppresses_baseline_codes() {
        let mut lint = ShellcheckLint::new();
        let mut cfg = Config::default();
        // User explicitly wants SC2164 back even though it is in
        // DEFAULT_DISABLED.
        cfg.shellcheck.enable = vec!["SC2164".into()];
        lint.set_config(&cfg);
        assert!(!lint.disable.contains(&2164));
        // Other baseline codes remain suppressed.
        assert!(lint.disable.contains(&3044));
    }

    #[test]
    fn shell_override_takes_effect() {
        let mut lint = ShellcheckLint::new();
        assert_eq!(lint.shell, "bash");
        let mut cfg = Config::default();
        cfg.shellcheck.shell = Some("sh".into());
        lint.set_config(&cfg);
        assert_eq!(lint.shell, "sh");
    }

    #[test]
    fn shell_override_rejects_unknown_dialect() {
        // `fish` is not in ALLOWED_SHELLS — the rule must keep the
        // default rather than silently passing junk to `--shell=`.
        let mut lint = ShellcheckLint::new();
        let mut cfg = Config::default();
        cfg.shellcheck.shell = Some("fish".into());
        lint.set_config(&cfg);
        assert_eq!(lint.shell, DEFAULT_SHELL);
    }

    #[test]
    fn timeout_config_overrides_default() {
        let mut lint = ShellcheckLint::new();
        let mut cfg = Config::default();
        cfg.shellcheck.timeout_secs = Some(5);
        lint.set_config(&cfg);
        assert_eq!(lint.timeout, Duration::from_secs(5));
    }

    #[test]
    fn unavailability_emits_single_diagnostic() {
        let src1 = "%install\nmkdir -p /tmp\n";
        let src2 = "%check\nmake test\n";
        let src = format!("{src1}{src2}");
        let mut lint = ShellcheckLint::new();
        let mut cfg = Config::default();
        cfg.shellcheck.binary = Some("/nonexistent/shellcheck-binary".into());
        lint.set_config(&cfg);
        lint.set_source(&src);
        // Spans accurately cover each section's byte range.
        lint.process_section(Span::new(0, src1.len(), 1, 1, 2, 14));
        lint.process_section(Span::new(src1.len(), src.len(), 3, 1, 4, 10));
        let diags = lint.take_diagnostics();
        assert_eq!(diags.len(), 1, "exactly one RPM201 expected");
        assert_eq!(diags[0].lint_id, "RPM201");
    }

    #[test]
    fn build_finding_diagnostics_handles_out_of_range_line() {
        // Regression: a malformed shellcheck stream reporting a line
        // number past line_map.len() must still produce a diagnostic
        // anchored inside the section (the last mapped source line),
        // not panic or anchor at line 0.
        use crate::diagnostic::Severity as Sev;
        let section_text = "%install\necho hello\n";
        let line_map = vec![1u32, 2u32];
        let span = Span::new(0, section_text.len(), 1, 1, 2, 12);
        let f = ShellcheckFinding {
            line: 999, // way past the mapped range
            end_line: None,
            level: "warning".to_owned(),
            code: 2086,
            message: "synthetic".to_owned(),
            fix: None,
        };
        let disable = HashSet::new();
        let diags = build_finding_diagnostics(vec![f], section_text, &line_map, span, &disable);
        assert_eq!(diags.len(), 1);
        // Falls back to the last mapped line (2 in slice → source line 2).
        assert_eq!(diags[0].primary_span.start_line, 2);
        assert_eq!(diags[0].severity, Sev::Warn);
    }

    /// Integration test against a real shellcheck binary.
    /// Auto-skipped when shellcheck is not in $PATH.
    #[test]
    fn real_shellcheck_flags_known_issue() {
        if probe_binary(None).is_err() {
            eprintln!("shellcheck not installed; skipping integration test");
            return;
        }
        let src = "%install\nFOO=$1\necho $FOO\n";
        let mut lint = ShellcheckLint::new();
        lint.set_config(&Config::default());
        lint.set_source(src);
        // The slice we feed to process_section covers the whole section
        // including header (the input shaper replaces the header).
        let span = Span::new(0, src.len(), 1, 1, 3, 11);
        lint.process_section(span);
        let diags = lint.take_diagnostics();
        assert!(
            diags.iter().any(|d| d.lint_id == "RPM200"),
            "expected at least one RPM200 finding, got {diags:?}"
        );
        for d in &diags {
            assert!(d.message.starts_with("[SC"), "message: {}", d.message);
        }
    }

    /// Integration test for the disable filter.
    #[test]
    fn real_shellcheck_respects_disable_list() {
        if probe_binary(None).is_err() {
            eprintln!("shellcheck not installed; skipping integration test");
            return;
        }
        let src = "%install\nFOO=$1\necho $FOO\n";
        let span = Span::new(0, src.len(), 1, 1, 3, 11);

        // Baseline: at least one finding.
        let mut baseline = ShellcheckLint::new();
        baseline.set_config(&Config::default());
        baseline.set_source(src);
        baseline.process_section(span);
        let baseline_diags = baseline.take_diagnostics();
        assert!(!baseline_diags.is_empty());

        // Disable every SC code we just saw → no diagnostics.
        let codes: Vec<String> = baseline_diags
            .iter()
            .filter_map(|d| {
                let msg = &d.message;
                let start = msg.find("[SC")? + 3;
                let end = msg[start..].find(':')?;
                Some(format!("SC{}", &msg[start..start + end]))
            })
            .collect();
        let mut cfg = Config::default();
        cfg.shellcheck.disable = codes;
        let mut filtered = ShellcheckLint::new();
        filtered.set_config(&cfg);
        filtered.set_source(src);
        filtered.process_section(span);
        let filtered_diags = filtered.take_diagnostics();
        assert!(
            filtered_diags.is_empty(),
            "expected disable filter to suppress all findings, got {filtered_diags:?}"
        );
    }
}
