//! RPM-REPO-010 `missing-buildrequires-for-command` — a build-script
//! section (`%prep` / `%build` / `%install` / `%check`) invokes a bare
//! command (e.g. `cmake .`, `meson setup`, `cargo build`) and the
//! command's owning package in the configured repo is NOT declared in
//! `BuildRequires:`.
//!
//! Sibling of RPM-REPO-011 (absolute paths). Together the pair covers
//! the common "spec runs a tool the chroot doesn't have" failure mode.
//! The split keeps the rules' diagnostic wording specific: -010 says
//! "use `BuildRequires: cmake`", -011 says "`/usr/bin/xsltproc` is
//! owned by `libxslt`".
//!
//! Heuristic, not exhaustive:
//! 1. Aggressive whitelist of shell built-ins and base coreutils
//!    that every build chroot has (we don't bother querying the
//!    universe for them).
//! 2. Skip macro-only lines (`%cmake_build`, `%make_install`) — the
//!    macro registry typically can't resolve them to a single command
//!    deterministically. The lint sees the literal `%cmake_build`
//!    token, doesn't match it against `/usr/bin/CMD`, and skips. The
//!    sibling lints (RPM-REPO-001 / 003) catch macro-package BRs
//!    through the spec's declared `BuildRequires:` list, which the
//!    operator usually already pairs with the macro.
//! 3. The first whitespace-separated token of each command position
//!    (line start, after `;`/`&&`/`||`/`|`/`` ` ``/`$(`) is treated as
//!    a candidate command. `cmd args` → check `cmd`; `make foo && bar`
//!    → check `make` and `bar`.
//! 4. For each candidate, query `file_owner(/usr/bin/CMD)` and
//!    `file_owner(/usr/sbin/CMD)` in that order. If the owning binary
//!    package isn't declared as `BuildRequires:`, warn.
//!
//! Silent on:
//! * Universe unavailable (cache miss).
//! * Command starts with `/` → that path is RPM-REPO-011's territory.
//! * Command starts with `%` → macro reference; rejected by the
//!   whitelist policy (see point 2 above).
//! * Conditional branch is provably inactive on the active profile
//!   (same gating policy as RPM-REPO-011).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use rpm_spec::ast::{BuildScriptKind, ShellBody, Span, SpecFile, Tag};
use rpm_spec_profile::Profile;
use rpm_spec_repo_core::RepoUniverse;

use crate::bcond::{BcondMap, BcondOverrides};
use crate::diagnostic::{Diagnostic, LintCategory, RepoContext, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

use super::shared::{self, RepoRule};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM-REPO-010",
    name: "missing-buildrequires-for-command",
    description: "A build-script section invokes a bare command (e.g. `cmake`, \
                  `meson`, `cargo`) whose owning package in the configured repo \
                  is not declared in `BuildRequires:`. Clean-chroot builds will \
                  fail with `command not found`.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Commands that every build chroot ships unconditionally — shell
/// built-ins + GNU coreutils + the base build set every distro
/// pre-installs (sh, bash, make, env, install, …). Filtering these
/// out keeps the noise floor low on real specs.
///
/// Membership is checked via linear scan (`.contains(&cmd)`) since
/// the list is partitioned by category (shell builtins / coreutils
/// / build stack) rather than globally sorted — readability of the
/// grouped form wins over a `binary_search` micro-opt at ~110
/// entries (linear scan is ~ns).
const IMPLICIT_BUILD_COMMANDS: &[&str] = &[
    // Shell built-ins (POSIX + common bash). Most are already filtered
    // by the parser, but a few (cd, set, eval, source) appear as bare
    // tokens in scripts.
    ":",
    "[",
    "[[",
    "alias",
    "break",
    "builtin",
    "case",
    "cd",
    "command",
    "continue",
    "declare",
    "do",
    "done",
    "echo",
    "elif",
    "else",
    "esac",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fi",
    "for",
    "function",
    "getopts",
    "hash",
    "if",
    "in",
    "let",
    "local",
    "printf",
    "pushd",
    "popd",
    "pwd",
    "read",
    "readonly",
    "return",
    "set",
    "shift",
    "source",
    "test",
    "then",
    "time",
    "trap",
    "true",
    "type",
    "ulimit",
    "umask",
    "unalias",
    "unset",
    "until",
    "wait",
    "while",
    // Coreutils — universally present in any build chroot.
    "awk",
    "basename",
    "cat",
    "chgrp",
    "chmod",
    "chown",
    "cmp",
    "cp",
    "cut",
    "date",
    "diff",
    "dirname",
    "du",
    "env",
    "expr",
    "find",
    "grep",
    "head",
    "iconv",
    "id",
    "install",
    "ln",
    "ls",
    "mkdir",
    "mktemp",
    "mv",
    "od",
    "readlink",
    "realpath",
    "rm",
    "rmdir",
    "sed",
    "seq",
    "sleep",
    "sort",
    "stat",
    "tail",
    "tee",
    // `test` / `true` already listed under shell built-ins; this
    // section deliberately omits them to keep `.contains()` from
    // walking duplicates.
    "touch",
    "tr",
    "uname",
    "uniq",
    "wc",
    "which",
    "xargs",
    // Standard build / unpack stack (every distro pulls these into
    // the buildroot baseline via `base_packages` or rpm-build itself).
    "bash",
    "bunzip2",
    "bzip2",
    "gunzip",
    "gzip",
    "make",
    "patch",
    "sh",
    "tar",
    "unxz",
    "unzip",
    "xz",
    "zstd",
];

#[derive(Debug, Default)]
pub struct MissingBuildRequiresForCommand {
    base: RepoRule,
}

impl MissingBuildRequiresForCommand {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MissingBuildRequiresForCommand {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(state) = self.base.state.as_ref() else {
            return;
        };
        let Some(profile) = self.base.profile.as_ref() else {
            return;
        };
        let bcond = BcondMap::from_spec(spec, &BcondOverrides::default());

        let declared_br: BTreeSet<String> = super::shared::project_deps(
            spec,
            profile,
            &bcond,
            |t| matches!(t, Tag::BuildRequires),
        )
        .into_iter()
        .map(|d| d.capability.name.to_string())
        .collect();

        let sections: Vec<(BuildScriptKind, &ShellBody<Span>, Span)> =
            shared::collect_active_build_scripts(&spec.items, profile, &bcond);

        // Per-spec dedup so `cmake` invoked from %build AND %check
        // produces only one diagnostic.
        let mut reported: BTreeSet<(String, String)> = BTreeSet::new();
        // Per-spec memoisation of `resolve_command_owner` results —
        // the same shell command repeated across sections (or even on
        // multiple lines of one section) would otherwise re-run up to
        // eight SQLite probes per repeat (four `/usr/bin/`-style
        // prefixes × two queries each: file_owner + resolve_nevra).
        // `None` is a valid cached value ("we asked, nobody owns it").
        // `HashMap` over `BTreeMap` because ordering is never observed
        // and the typical hit set is < 50 short commands.
        let mut command_owner_cache: HashMap<String, Option<String>> = HashMap::new();

        for (_kind, body, section_span) in sections {
            let inactive_ranges = shared::inactive_line_ranges(body, profile, &bcond);
            let section_start = section_span.start_line;
            for (idx, line) in body.lines.iter().enumerate() {
                let source_line = section_start
                    .saturating_add(1)
                    .saturating_add(idx as u32);
                if inactive_ranges
                    .iter()
                    .any(|(s, e)| source_line >= *s && source_line <= *e)
                {
                    continue;
                }
                let Some(text) = line.literal_str() else {
                    continue;
                };
                for cmd in scan_bare_commands(text) {
                    if IMPLICIT_BUILD_COMMANDS.contains(&cmd) {
                        continue;
                    }
                    // Memoised /usr/bin/CMD → owner-name lookup. The
                    // cache avoids re-running up to 4 SQLite path
                    // probes for every repeat of `cmake` / `meson` /
                    // `make` across a multi-section build script.
                    let owner_lookup = if let Some(cached) = command_owner_cache.get(cmd) {
                        cached.clone()
                    } else {
                        let resolved = match resolve_command_owner(&state.universe, cmd) {
                            Ok(name) => name,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    command = cmd,
                                    "file_owner lookup failed; skipping command",
                                );
                                // Don't cache the failure — a transient
                                // SQLite error shouldn't suppress a
                                // later legitimate hit for the same
                                // command.
                                continue;
                            }
                        };
                        command_owner_cache.insert(cmd.to_string(), resolved.clone());
                        resolved
                    };
                    let Some(owner_name) = owner_lookup else {
                        continue;
                    };
                    if declared_br.contains(&owner_name) {
                        continue;
                    }
                    if !reported.insert((cmd.to_string(), owner_name.clone())) {
                        continue;
                    }
                    self.base.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            METADATA.default_severity,
                            format!(
                                "build script invokes `{cmd}` (owned by `{owner_name}`), \
                                 but `BuildRequires: {owner_name}` is not declared; \
                                 clean-chroot builds will fail with `{cmd}: command not found`",
                            ),
                            section_span,
                        )
                        .with_repo_context(RepoContext::for_profile(
                            &state.universe.profile_name,
                        )),
                    );
                }
            }
        }
    }
}

impl Lint for MissingBuildRequiresForCommand {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.base.take_diagnostics()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.base.set_profile(profile);
    }
    fn set_repo_universe(&mut self, universe: Option<Arc<RepoUniverse>>) {
        self.base.set_repo_universe(universe);
    }
}

/// Extract every bare command from `line` — the first token of every
/// command position. Heuristic shell splitter; doesn't handle quoted
/// strings or parameter expansions but catches the common cases.
///
/// Command positions:
/// * line start (post-indent)
/// * after `;`, `&&`, `||`, `|`, `&`
/// * inside `$(...)` and backticks (first token after the opener)
///
/// Rejected tokens:
/// * empty
/// * starts with `/` (file-path territory → RPM-REPO-011)
/// * starts with `%` (macro reference → can't statically resolve)
/// * starts with `$` or `(` (shell variable / subshell artefacts that
///   slipped past trimming)
/// * contains `=` (variable assignment, not a command)
fn scan_bare_commands(line: &str) -> Vec<&str> {
    // Strip a leading single-line `#` comment so commands inside the
    // commented suffix are ignored. (Comments inside heredocs would
    // require a state machine — out of scope for the heuristic; the
    // false-positive rate from heredoc bodies is low because operators
    // rarely write `cmake` inside `<<EOF`.)
    let line = match line.find('#') {
        Some(0) => return Vec::new(),
        Some(i) => &line[..i],
        None => line,
    };

    // Skip shell `case` pattern lines: `i386 | x86_64 | ppc)` and
    // friends. Without this the `|`-as-separator heuristic treats
    // each alternative as a command at a new command position and
    // emits one diagnostic per token. Detection: the trimmed line
    // ends with `)` and contains no matching `(` (a real command
    // like `cmd $(...)` has both). Misses one-line case branches
    // like `i386) cmd ;;` — see test for the rationale.
    if is_case_pattern_line(line) {
        return Vec::new();
    }

    let mut out = Vec::new();
    // Track which byte indices begin a command position. Always start
    // at zero (line start). After each separator, the next non-space
    // byte begins a new command.
    let mut at_command = true;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if at_command && !c.is_ascii_whitespace() {
            // Collect the first whitespace-delimited token.
            let start = i;
            let mut end = i;
            while end < bytes.len()
                && !bytes[end].is_ascii_whitespace()
                && !is_separator(bytes[end])
            {
                end += 1;
            }
            let raw = &line[start..end];
            let token = trim_punct(raw);
            if is_candidate_command(token) {
                out.push(token);
            }
            at_command = false;
            i = end;
            continue;
        }
        // Track separators to know where the next command begins.
        let consumed = separator_len(&bytes[i..]);
        if consumed > 0 {
            at_command = true;
            i += consumed;
            continue;
        }
        // Subshell openers: `$(...)` or backticks → next token is a
        // command. We don't track close because a close-only signal
        // doesn't change the command-position state.
        if c == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            at_command = true;
            i += 2;
            continue;
        }
        if c == b'`' {
            at_command = true;
            i += 1;
            continue;
        }
        i += 1;
    }
    out
}

/// Heuristic: is this a shell `case`-pattern line (`*)`, `i386 |
/// x86_64)`, …) that we should skip entirely?
///
/// Returns true when the trimmed line ends with `)` AND contains no
/// matching `(`. A real command line that legitimately ends in `)`
/// (`./configure $(rpm --eval '%_prefix')`) has both, so the
/// no-`(` check rules those out.
///
/// Known false-negative: one-liner case branches like
/// `i386) cmd ;;` slip through (the `)` is *inside* the line, not at
/// the end). They produce spurious diagnostics for the pattern token
/// (e.g. `i386`); fixing requires proper state tracking across
/// `case ... esac`, deferred. Real-world specs almost always put the
/// pattern on its own line.
fn is_case_pattern_line(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.ends_with(')') {
        return false;
    }
    // Subshell / command-substitution `$(...)` or grouping `(...)`
    // always pair a `(` somewhere on the line. Real case-pattern
    // lines never do (the `case ... in` keyword sits on its own line).
    !trimmed.contains('(')
}

fn is_separator(b: u8) -> bool {
    matches!(b, b';' | b'|' | b'&')
}

fn separator_len(rest: &[u8]) -> usize {
    if rest.is_empty() {
        return 0;
    }
    match rest[0] {
        b';' => 1,
        b'|' => {
            if rest.len() >= 2 && rest[1] == b'|' {
                2
            } else {
                1
            }
        }
        b'&' => {
            if rest.len() >= 2 && rest[1] == b'&' {
                2
            } else {
                1
            }
        }
        _ => 0,
    }
}

fn trim_punct(s: &str) -> &str {
    let s = s.trim_start_matches(['(', '`', '"', '\'']);
    s.trim_end_matches([')', '`', '"', '\''])
}

fn is_candidate_command(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let first = token.as_bytes()[0];
    if matches!(first, b'/' | b'%' | b'$' | b'(' | b'-') {
        return false;
    }
    // Variable assignment (FOO=bar) — not a command.
    if token.contains('=') {
        return false;
    }
    // Plausible command-name character set: alphanumeric + `_-.+`.
    // Rejects shell glob results, quoted strings, command flags.
    token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
}

/// Try `/usr/bin/CMD` then `/usr/sbin/CMD` in the universe's file
/// index and resolve the owning package's NAME (not full NEVRA, since
/// the diagnostic compares against the spec's declared `BuildRequires:`
/// which is also name-only).
fn resolve_command_owner(
    universe: &RepoUniverse,
    command: &str,
) -> Result<Option<String>, rpm_spec_repo_core::RepoError> {
    for prefix in ["/usr/bin/", "/usr/sbin/", "/bin/", "/sbin/"] {
        let path = format!("{prefix}{command}");
        if let Some(owner_ref) = universe.file_owner(&path)?
            && let Some(nevra) = universe.resolve_nevra(&owner_ref)?
        {
            return Ok(Some(nevra.name.to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures::redos_profile;
    use crate::rules::test_support::run_repo_lint;
    use rpm_spec_repo_core::{
        Capability, NEVRA, Package, PkgChecksum, RepoId, RepoIndex, RepoUniverse,
    };
    use time::OffsetDateTime;

    fn pkg(name: &str, files: Vec<&str>) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from("1.0"),
                release: Arc::from("1"),
                arch: Arc::from("x86_64"),
            },
            repo_id: RepoId::from("baseos"),
            provides: vec![Capability::unversioned(Arc::from(name))],
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: None,
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Sha256(String::new()),
            location: Arc::from(""),
            files: files.into_iter().map(Arc::from).collect(),
        }
    }

    fn universe_with_cmake() -> Arc<RepoUniverse> {
        let packages = vec![
            pkg("cmake", vec!["/usr/bin/cmake"]),
            pkg("meson", vec!["/usr/bin/meson"]),
            pkg("bash", vec!["/usr/bin/bash"]),
        ];
        let index = RepoIndex {
            repo_id: RepoId::from("baseos"),
            revision: "deadbeef".to_string(),
            fetched_at: OffsetDateTime::now_utc(),
            packages,
            advisories: Vec::new(),
        };
        Arc::new(
            RepoUniverse::from_indexes_for_tests("redos-7.3-x86_64", vec![Arc::new(index)])
                .expect("build in-memory universe"),
        )
    }

    #[test]
    fn flags_missing_cmake_in_build_section() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\ncmake -B build\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM-REPO-010");
        assert!(diags[0].message.contains("cmake"), "{}", diags[0].message);
    }

    #[test]
    fn silent_when_br_declares_owner() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: cmake\n%description\nx\n\
                   %build\ncmake -B build\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn ignores_whitelisted_commands() {
        // `cp`, `make`, `sh` are all in IMPLICIT_BUILD_COMMANDS — must
        // not fire even though no BR is declared and they're not in the
        // tiny universe at all.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %install\ncp foo bar\nmake install\nsh -c 'echo hi'\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn ignores_absolute_paths() {
        // `/usr/bin/cmake` is RPM-REPO-011's territory; this rule
        // must skip it without complaint.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\n/usr/bin/cmake -B build\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn ignores_macro_invocations() {
        // `%cmake_build` is a macro; the lint can't statically resolve
        // what command it expands to. Skip silently rather than
        // false-positive on `%cmake_build` itself.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\n%cmake_build\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn handles_commands_after_and() {
        // `make && meson` — both commands must be checked. `make` is
        // whitelisted, `meson` is in the universe but not BR'd → 1 diag.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\nmake && meson setup build\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("meson"), "{}", diags[0].message);
    }

    #[test]
    fn skips_variable_assignments() {
        // `FOO=bar` is not a command — must not warn even if `bar`
        // resolves in the universe.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\nCMAKE=cmake\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn dedup_across_sections() {
        // `cmake` invoked in both %build and %install — exactly one
        // diagnostic, not two.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\ncmake -B build\n\
                   %install\ncmake --install build\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_when_universe_missing() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\ncmake -B build\n";
        let outcome = crate::session::parse(src);
        let mut lint = MissingBuildRequiresForCommand::default();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(None);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn unknown_command_silent() {
        // `furiousfrog` is not in the universe and not whitelisted.
        // The lint must NOT fabricate a guess — silent skip.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %build\nfuriousfrog --hop\n";
        let diags = run_repo_lint::<MissingBuildRequiresForCommand>(
            src,
            &redos_profile(),
            universe_with_cmake(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn scan_bare_commands_basic() {
        assert_eq!(scan_bare_commands("cmake -B build"), vec!["cmake"]);
        assert_eq!(scan_bare_commands("make && meson setup"), vec!["make", "meson"]);
        assert_eq!(scan_bare_commands("a ; b | c || d"), vec!["a", "b", "c", "d"]);
        // Heuristic catches the first token at each command position;
        // `do` is the syntactic first token after `;` in `for...do`.
        // The whitelist filters it out downstream so it doesn't
        // produce a false-positive.
        assert_eq!(
            scan_bare_commands("for f in *; do echo $f; done"),
            vec!["for", "do", "done"]
        );
    }

    #[test]
    fn scan_bare_commands_skips_case_pattern_lines() {
        // Real-world `case` arm — the `|` here is shell pattern
        // alternation, NOT a command pipe. Must produce zero tokens.
        assert!(scan_bare_commands("   i386 | x86_64 | ppc | ppc64 | s390 | s390x)").is_empty());
        assert!(scan_bare_commands("*)").is_empty());
        assert!(scan_bare_commands("    el7 | el8)").is_empty());
        // Real commands ending in `)` because of subshell expansion
        // have a matching `(` — the heuristic doesn't skip those, so
        // the inner `rpm` is still picked up as a candidate.
        assert_eq!(
            scan_bare_commands("cmake $(rpm --eval '%_prefix')"),
            vec!["cmake", "rpm"],
        );
    }

    #[test]
    fn scan_bare_commands_rejects_non_commands() {
        assert!(scan_bare_commands("# pure comment").is_empty());
        assert!(scan_bare_commands("").is_empty());
        assert_eq!(scan_bare_commands("/usr/bin/foo bar"), Vec::<&str>::new());
        assert_eq!(scan_bare_commands("%cmake_build"), Vec::<&str>::new());
        assert_eq!(scan_bare_commands("FOO=bar"), Vec::<&str>::new());
    }
}
