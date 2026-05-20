//! RPM-REPO-011 `missing-buildrequires-for-file` — a build-script
//! section (`%prep` / `%build` / `%install` / `%check` / `%conf` /
//! `%clean` / `%generate_buildrequires`) invokes a tool by an
//! absolute path (e.g. `/usr/bin/xsltproc`, `/sbin/ldconfig`) and the
//! file is owned by a package that is NOT declared in
//! `BuildRequires:`.
//!
//! Operators routinely forget the implicit tool dependencies a script
//! line introduces: a `/usr/bin/xsltproc input.xml` call only works
//! on the clean-chroot builder if `BuildRequires: libxslt` was
//! remembered. This lint correlates path tokens found in the
//! build-script body against the repo universe's `file_owner` index
//! and flags every (path, owner) pair the spec hasn't declared.
//!
//! Scriptlet sections (`%post`, `%postun`, ...) are deliberately
//! NOT scanned — runtime tool deps are RPM-REPO-002's territory.
//!
//! Silent when the universe is unavailable, the path resolves to an
//! "implicit" base tool (`/bin/sh`, `/sbin/ldconfig`, ...), or the
//! containing conditional branch is provably inactive on the active
//! profile (matches the conditional-gating policy of `project_deps`).

use std::collections::BTreeSet;
use std::sync::Arc;

use rpm_spec::ast::{BuildScriptKind, ShellBody, Span, SpecFile, Tag};
use rpm_spec_profile::{MacroRegistry, Profile};
use rpm_spec_repo_core::RepoUniverse;

use crate::bcond::{BcondMap, BcondOverrides};
use crate::diagnostic::{Diagnostic, LintCategory, RepoContext, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

use super::shared::{self, MACRO_EXPAND_DEPTH, RepoRule};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM-REPO-011",
    name: "missing-buildrequires-for-file",
    description: "A build-script section invokes a tool by absolute path \
                  (e.g. `/usr/bin/xsltproc`) whose owning package is not \
                  declared in `BuildRequires:`. Clean-chroot builds will fail \
                  with the tool missing.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Paths universally available in the build chroot. Profiles may carry
/// their own `implicit_buildrequires` in a future milestone; until
/// then this in-code allow-list suppresses the noise.
const IMPLICIT_BUILD_TOOLS: &[&str] = &[
    "/bin/sh",
    "/bin/bash",
    "/bin/cat",
    "/bin/cp",
    "/bin/mv",
    "/bin/rm",
    "/bin/ls",
    "/bin/mkdir",
    "/bin/install",
    "/usr/bin/sh",
    "/usr/bin/bash",
    "/usr/bin/cat",
    "/usr/bin/cp",
    "/usr/bin/mv",
    "/usr/bin/rm",
    "/usr/bin/ls",
    "/usr/bin/mkdir",
    "/usr/bin/install",
    "/sbin/ldconfig",
    "/usr/sbin/ldconfig",
];

#[derive(Debug, Default)]
pub struct MissingBuildRequiresForFile {
    base: RepoRule,
}

impl MissingBuildRequiresForFile {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MissingBuildRequiresForFile {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Short-circuit silently when the universe / profile haven't
        // been wired up. Matches the policy of every other RPM-REPO-*
        // rule: "no repos configured" is not a per-lint warning.
        let Some(state) = self.base.state.as_ref() else {
            return;
        };
        let Some(profile) = self.base.profile.as_ref() else {
            return;
        };

        let bcond = BcondMap::from_spec(spec, &BcondOverrides::default());

        // Names declared in any top-level `BuildRequires:`, after the
        // same active-conditional gating policy. Reuse the resolver-
        // shaped projection from `shared.rs` to inherit macro
        // expansion + rich-dep flattening — we only need the names.
        let declared_br: BTreeSet<String> = super::shared::project_deps(
            spec,
            profile,
            &bcond,
            |t| matches!(t, Tag::BuildRequires),
        )
        .into_iter()
        .map(|d| d.capability.name.to_string())
        .collect();

        // Walk every active build-script section.
        let sections: Vec<(BuildScriptKind, &ShellBody<Span>, Span)> =
            shared::collect_active_build_scripts(&spec.items, profile, &bcond);

        // Per-spec dedup: don't emit the same (path, owner) pair
        // twice if it appears in `%build` AND `%install`.
        let mut reported: BTreeSet<(String, String)> = BTreeSet::new();

        for (_kind, body, section_span) in sections {
            // Shell-body `%if`/`%endif` blocks are stored on the body's
            // `conditionals` sidecar with per-branch source-line spans.
            // Collect inactive ranges so we don't scan lines that fall
            // inside a branch the active profile won't execute (same
            // policy as `project_deps`: indeterminate → skip the
            // branch but don't take `%else` either).
            let inactive_ranges = shared::inactive_line_ranges(body, profile, &bcond);
            let section_start = section_span.start_line;
            for (idx, line) in body.lines.iter().enumerate() {
                // Parser pushes one physical line per `body.lines`
                // entry; index 0 corresponds to `section_start + 1`
                // (the line after `%install`). Mirrors `impact::active_shell_lines`.
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
                for raw_token in scan_tool_paths(text) {
                    let Some(path) = expand_path_macros(raw_token, &profile.macros) else {
                        continue;
                    };
                    if !looks_like_bin_path(&path) {
                        continue;
                    }
                    if IMPLICIT_BUILD_TOOLS.contains(&path.as_str()) {
                        continue;
                    }
                    let owner_ref = match state.universe.file_owner(&path) {
                        Ok(Some(r)) => r,
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                path = ?path,
                                "file_owner lookup failed; skipping token",
                            );
                            continue;
                        }
                    };
                    let owner_name = match state.universe.resolve_nevra(&owner_ref) {
                        Ok(Some(n)) => n.name.to_string(),
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                path = ?path,
                                "resolve_nevra failed; skipping token",
                            );
                            continue;
                        }
                    };
                    if declared_br.contains(&owner_name) {
                        continue;
                    }
                    if !reported.insert((path.clone(), owner_name.clone())) {
                        continue;
                    }
                    self.base.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            METADATA.default_severity,
                            format!(
                                "build script uses `{path}` (owned by `{owner_name}`), \
                                 but `BuildRequires: {owner_name}` is not declared; \
                                 clean-chroot builds will fail with the tool missing",
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

impl Lint for MissingBuildRequiresForFile {
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

/// Yield every whitespace-separated token in `line` that starts with
/// `/` or with a `%{macro}` reference whose first character isn't a
/// shell metacharacter — candidates for further validation.
///
/// Deliberately permissive: the real filtering happens in
/// [`looks_like_bin_path`] after macro expansion. Returning a few
/// extra tokens is cheaper than carrying a tokenizer state machine.
fn scan_tool_paths(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for raw in line.split_whitespace() {
        // Strip common shell punctuation that hugs the path on the
        // left (backticks, parens, redirections) — keeps the heuristic
        // useful inside `$(...)` and `;cmd` constructs.
        let trimmed = raw.trim_start_matches(|c: char| {
            matches!(c, '(' | ')' | '`' | ';' | '&' | '|' | '<' | '>' | '"' | '\'')
        });
        let trimmed = trimmed.trim_end_matches(|c: char| {
            matches!(c, '(' | ')' | '`' | ';' | '&' | '|' | '<' | '>' | '"' | '\'')
        });
        if trimmed.starts_with('/') || trimmed.starts_with('%') {
            out.push(trimmed);
        }
    }
    out
}

/// Expand every `%{name}` reference inside `token` via
/// [`MacroRegistry::expand_to_literal`]. Returns `None` if any
/// reference fails to resolve or the input contains shell-only
/// expressions (`$(...)`, `${...}`, `*`, etc.) that we don't model.
///
/// The grammar handled is intentionally minimal: a token is a
/// concatenation of literal chunks and `%{name}` references. Anything
/// else (`%name` bareword without braces, `%[expr]`, defaulted refs)
/// is conservatively rejected. The remaining filename character set
/// (`[A-Za-z0-9._/-]`) catches typical tool paths without false
/// positives on quoted strings or shell variables.
fn expand_path_macros(token: &str, macros: &MacroRegistry) -> Option<String> {
    if !token.contains('%') {
        // Fast path: literal token. Still validate the character set
        // so quoted strings ("hello") don't escape into the next step.
        if !token
            .chars()
            .all(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '/' | '-'))
        {
            return None;
        }
        return Some(token.to_string());
    }
    let mut out = String::with_capacity(token.len());
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'%' {
            // Only `%{name}` is supported.
            if i + 1 >= bytes.len() || bytes[i + 1] != b'{' {
                return None;
            }
            let start = i + 2;
            let rel = bytes[start..].iter().position(|&b| b == b'}')?;
            let name_bytes = &bytes[start..start + rel];
            // Macro names must be plain identifier characters; reject
            // qualifiers like `%{?foo}` to stay conservative.
            if name_bytes.is_empty()
                || !name_bytes.iter().all(|&b| {
                    b.is_ascii_alphanumeric() || b == b'_'
                })
            {
                return None;
            }
            let name = std::str::from_utf8(name_bytes).ok()?;
            let expanded = macros.expand_to_literal(name, MACRO_EXPAND_DEPTH)?;
            out.push_str(&expanded);
            i = start + rel + 1;
        } else {
            // Permitted character set for non-macro chunks.
            let ch = c as char;
            if !matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '/' | '-') {
                return None;
            }
            out.push(ch);
            i += 1;
        }
    }
    Some(out)
}

/// Whether `path` looks like a tool path we should query the universe
/// about. Skips library / config / data paths that file_owner would
/// happily resolve but aren't "tools" — keeps the noise floor low.
fn looks_like_bin_path(path: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/bin/",
        "/sbin/",
        "/usr/bin/",
        "/usr/sbin/",
        "/usr/local/bin/",
        "/usr/local/sbin/",
    ];
    if PREFIXES.iter().any(|p| path.starts_with(p)) {
        return is_bin_basename(path);
    }
    // `/opt/<vendor>/bin/foo` or `/opt/<vendor>/sbin/foo`.
    if let Some(rest) = path.strip_prefix("/opt/")
        && let Some(slash) = rest.find('/')
    {
        let tail = &rest[slash + 1..];
        if (tail.starts_with("bin/") || tail.starts_with("sbin/")) && is_bin_basename(path) {
            return true;
        }
    }
    false
}

/// Final-segment validation: a "tool" basename must have at least one
/// non-empty char beyond the leading `/`, no nested slashes after the
/// prefix, no extensions associated with libraries (`.so`), and no
/// pkgconfig data files.
fn is_bin_basename(path: &str) -> bool {
    let Some(base) = path.rsplit('/').next() else {
        return false;
    };
    if base.is_empty() {
        return false;
    }
    // Library / data / dev files — file_owner resolves them but they
    // aren't tools.
    if base.contains(".so") || base.ends_with(".pc") || base.ends_with(".h") {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures::redos_profile;
    use crate::rules::test_support::run_repo_lint;
    use rpm_spec_repo_core::{
        Capability, NEVRA, Package, PkgChecksum, RepoIndex, RepoUniverse,
    };
    use time::OffsetDateTime;

    fn pkg(name: &str, version: &str, release: &str, files: Vec<&str>) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from(version),
                release: Arc::from(release),
                arch: Arc::from("x86_64"),
            },
            repo_id: Arc::from("baseos"),
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

    /// Universe specialised for this lint: includes `libxslt` (owns
    /// `/usr/bin/xsltproc`) on top of the base set so the missing-BR
    /// path is exercisable without depending on the shared
    /// [`super::test_fixtures::tiny_universe`] additions.
    fn universe_with_xsltproc() -> Arc<RepoUniverse> {
        let packages = vec![
            pkg("bash", "5.1.8", "9.el9", vec!["/bin/bash"]),
            pkg("glibc", "2.34", "1.el9", vec![]),
            pkg("libxslt", "1.1.34", "1.el9", vec!["/usr/bin/xsltproc"]),
        ];
        let index = RepoIndex {
            repo_id: Arc::from("baseos"),
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
    fn flags_missing_xsltproc_in_install_section() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %install\n/usr/bin/xsltproc input.xml\n";
        let diags = run_repo_lint::<MissingBuildRequiresForFile>(
            src,
            &redos_profile(),
            universe_with_xsltproc(),
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM-REPO-011");
        assert!(
            diags[0].message.contains("/usr/bin/xsltproc"),
            "{}",
            diags[0].message
        );
        assert!(diags[0].message.contains("libxslt"), "{}", diags[0].message);
    }

    #[test]
    fn silent_when_br_declares_owner() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\nBuildRequires: libxslt\n%description\nx\n\
                   %install\n/usr/bin/xsltproc input.xml\n";
        let diags = run_repo_lint::<MissingBuildRequiresForFile>(
            src,
            &redos_profile(),
            universe_with_xsltproc(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_path_in_implicit_list() {
        // `/sbin/ldconfig` is universally available — must not fire
        // even when the universe has no `glibc` package owning it.
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %install\n/sbin/ldconfig -n /tmp\n";
        let diags = run_repo_lint::<MissingBuildRequiresForFile>(
            src,
            &redos_profile(),
            universe_with_xsltproc(),
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_universe_missing() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %install\n/usr/bin/xsltproc input.xml\n";
        let outcome = crate::session::parse(src);
        let mut lint = MissingBuildRequiresForFile::default();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(None);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn silent_when_inactive_conditional() {
        // Shell-body `%if`/`%endif` block wrapping the only call site.
        // `%_vendor` in the active profile is overridden to "redhat",
        // so the ROSA-guarded body must NOT fire — no diagnostic.
        // (Section-level `%if` around `%install` won't work for this
        // test: section headers break the surrounding conditional in
        // the parser, leaving the section flat at top level. Wrapping
        // the tool call INSIDE the section body is the realistic
        // scenario the rule must handle.)
        let src = "Name: x\nVersion: 1\nRelease: 1\nSummary: s\n\
                   License: MIT\n%description\nx\n\
                   %install\n\
                   %if \"%_vendor\" == \"rosa\"\n\
                   /usr/bin/xsltproc input.xml\n\
                   %endif\n";
        let mut profile = redos_profile();
        profile.macros.insert(
            "_vendor".to_string(),
            rpm_spec_profile::MacroEntry::literal(
                "redhat",
                rpm_spec_profile::Provenance::Override,
            ),
        );
        let diags = run_repo_lint::<MissingBuildRequiresForFile>(
            src,
            &profile,
            universe_with_xsltproc(),
        );
        assert!(
            diags.is_empty(),
            "inactive vendor branch should be skipped; got {diags:?}",
        );
    }
}
