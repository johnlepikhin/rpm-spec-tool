//! RPM490 `macro-composition-to-specific-macro` — flag macro-composed
//! paths that have a more canonical specific macro.
//!
//! `%{_prefix}/bin` resolves to `/usr/bin` on every standard profile,
//! but `%{_bindir}` resolves to the same thing in one step. Using the
//! specific macro keeps the spec readable when distros override the
//! underlying paths (e.g. multilib `_libdir` = `/usr/lib64`).
//!
//! Distinct from RPM050 (`hardcoded-paths`), which catches *literal*
//! paths like `/usr/bin`. RPM490 catches macro-based paths that simply
//! compose to the same thing.

use rpm_spec::ast::{FileEntry, PreambleItem, Scriptlet, Section, Span, Tag, Trigger};
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::DepTagKey;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM490",
    name: "macro-composition-to-specific-macro",
    description: "A macro-composed path (e.g. `%{_prefix}/bin`) has a more specific canonical \
                  macro (`%{_bindir}`); switch to the specific form.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Candidate (general macro, suffix, specific macro). Order matters:
/// more-specific suffixes are tried first so `%{_prefix}/lib64` (when
/// it equals `%{_libdir}`) wins over `%{_prefix}/lib`.
const CANDIDATES: &[(&str, &str, &str)] = &[
    ("_prefix", "/lib64", "_libdir"),
    ("_prefix", "/libexec", "_libexecdir"),
    ("_prefix", "/include", "_includedir"),
    ("_prefix", "/share", "_datadir"),
    ("_prefix", "/sbin", "_sbindir"),
    ("_prefix", "/bin", "_bindir"),
    ("_prefix", "/lib", "_libdir"),
    ("_datadir", "/man", "_mandir"),
    ("_datadir", "/info", "_infodir"),
    ("_datadir", "/doc", "_docdir"),
];

const EXPAND_DEPTH: u8 = 8;

/// A macro-composed path (e.g. `%{_prefix}/bin`) has a more specific canonical macro (`%{_bindir}`); switch to the specific form.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MacroCompositionToSpecific {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
    /// Active rewrites validated against the profile, with the
    /// `%{NAME}` and `%NAME` lookup strings precomputed once. Empty
    /// when no profile is loaded.
    active: Vec<ActiveCandidate>,
}

/// Precomputed candidate so the hot `match_here` loop only does string
/// comparisons — no `format!` allocations per `%` token.
#[derive(Debug)]
struct ActiveCandidate {
    general: &'static str,
    suffix: &'static str,
    specific: &'static str,
    /// `"%{general}"` — pre-allocated once.
    braced: String,
    /// `"%general"` — pre-allocated once.
    plain: String,
}

impl MacroCompositionToSpecific {
    pub fn new() -> Self {
        Self::default()
    }

    fn scan_anchor(&mut self, anchor: Span) {
        let Some(source) = &self.source else { return };
        if self.active.is_empty() {
            return;
        }
        let end = anchor.end_byte.min(source.len());
        let start = anchor.start_byte.min(end);
        let Some(slice) = source.get(start..end) else {
            return;
        };

        let mut idx = 0;
        while let Some(pct) = slice[idx..].find('%') {
            let pos = idx + pct;
            idx = pos + 1;
            if let Some(found) = self.match_here(&slice[pos..]) {
                let abs_start = start + pos;
                let abs_end = abs_start + found.matched_len;
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`%{{{general}}}{suffix}` is the same path as `%{{{specific}}}` — \
                             use the specific macro",
                            general = found.general,
                            suffix = found.suffix,
                            specific = found.specific,
                        ),
                        Span::from_bytes(abs_start, abs_end),
                    )
                    .with_suggestion(Suggestion::new(
                        format!("replace with `%{{{}}}`", found.specific),
                        Vec::new(),
                        Applicability::MachineApplicable,
                    )),
                );
                idx = abs_end - start;
            }
        }
    }

    /// Try to match a candidate at the start of `text` (which begins
    /// with `%`). Returns the first hit (CANDIDATES is ordered specific-
    /// first so longest match wins).
    fn match_here(&self, text: &str) -> Option<Match> {
        for cand in &self.active {
            // Accept both `%{NAME}` and `%NAME` (boundary-terminated).
            let prefix_len = if text.starts_with(cand.braced.as_str()) {
                cand.braced.len()
            } else if let Some(rest) = text.strip_prefix(cand.plain.as_str()) {
                // `%NAME` must terminate at a non-name char (so
                // `%_prefix/bin` matches but `%_prefixed` doesn't).
                match rest.as_bytes().first() {
                    Some(&b) if b.is_ascii_alphanumeric() || b == b'_' => continue,
                    _ => cand.plain.len(),
                }
            } else {
                continue;
            };
            let rest = &text[prefix_len..];
            if !rest.starts_with(cand.suffix) {
                continue;
            }
            // Suffix must end at a path boundary so `/lib` doesn't
            // match `%{_prefix}/library`.
            let after_suffix = &rest[cand.suffix.len()..];
            match after_suffix.as_bytes().first() {
                None => {}
                Some(&b) if !b.is_ascii_alphanumeric() && b != b'_' && b != b'.' => {}
                _ => continue,
            }
            return Some(Match {
                general: cand.general,
                suffix: cand.suffix,
                specific: cand.specific,
                matched_len: prefix_len + cand.suffix.len(),
            });
        }
        None
    }
}

#[derive(Debug)]
struct Match {
    general: &'static str,
    suffix: &'static str,
    specific: &'static str,
    matched_len: usize,
}

fn validate_candidates(profile: &Profile) -> Vec<ActiveCandidate> {
    let mut out = Vec::new();
    for &(general, suffix, specific) in CANDIDATES {
        let Some(g) = profile.macros.expand_to_literal(general, EXPAND_DEPTH) else {
            continue;
        };
        let Some(s) = profile.macros.expand_to_literal(specific, EXPAND_DEPTH) else {
            continue;
        };
        let composed = format!("{g}{suffix}");
        if composed == s {
            out.push(ActiveCandidate {
                general,
                suffix,
                specific,
                braced: format!("%{{{general}}}"),
                plain: format!("%{general}"),
            });
        }
    }
    out
}

impl<'ast> Visit<'ast> for MacroCompositionToSpecific {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        if !is_safe_tag(&node.tag) {
            self.scan_anchor(node.data);
        }
        visit::walk_preamble(self, node);
    }

    fn visit_file_entry(&mut self, node: &'ast FileEntry<Span>) {
        self.scan_anchor(node.data);
        visit::walk_file_entry(self, node);
    }

    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Section::BuildScript { data, .. }
        | Section::Verify { data, .. }
        | Section::Sepolicy { data, .. } = node
        {
            self.scan_anchor(*data);
        }
        visit::walk_section(self, node);
    }

    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        self.scan_anchor(node.data);
        visit::walk_scriptlet(self, node);
    }

    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        self.scan_anchor(node.data);
        visit::walk_trigger(self, node);
    }
}

/// Tags where literal-style paths are usually legitimate (URLs, free
/// text) — kept in lockstep with RPM050.
fn is_safe_tag(tag: &Tag) -> bool {
    if matches!(
        tag,
        Tag::Source(_) | Tag::Patch(_) | Tag::URL | Tag::Summary | Tag::License | Tag::Group
    ) {
        return true;
    }
    // Dep tags — see `hardcoded_paths::is_safe_tag` for the policy
    // rationale (file-based deps must stay literal).
    DepTagKey::from_tag(tag).is_some()
}

impl Lint for MacroCompositionToSpecific {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = Some(source);
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.active = validate_candidates(profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::util::make_test_profile;
    use crate::session::parse;

    fn run_with_profile(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = MacroCompositionToSpecific::new();
        lint.set_source(std::sync::Arc::from(src));
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn fedora_like() -> Profile {
        make_test_profile(
            None,
            None,
            &[
                ("_prefix", "/usr"),
                ("_bindir", "/usr/bin"),
                ("_sbindir", "/usr/sbin"),
                ("_libdir", "/usr/lib64"),
                ("_libexecdir", "/usr/libexec"),
                ("_includedir", "/usr/include"),
                ("_datadir", "/usr/share"),
                ("_mandir", "/usr/share/man"),
                ("_infodir", "/usr/share/info"),
                ("_docdir", "/usr/share/doc"),
            ],
            &[],
        )
    }

    #[test]
    fn flags_prefix_bin_to_bindir() {
        let src = "Name: x\n%install\ninstall -m755 foo %{buildroot}%{_prefix}/bin/foo\n";
        let diags = run_with_profile(src, &fedora_like());
        assert!(diags.iter().any(|d| d.lint_id == "RPM490"), "{diags:?}");
        let msg = diags.iter().find(|d| d.lint_id == "RPM490").unwrap();
        assert!(msg.message.contains("_bindir"));
    }

    #[test]
    fn flags_datadir_man_to_mandir() {
        let src = "Name: x\n%files\n%{_datadir}/man/man1/foo.1\n";
        let diags = run_with_profile(src, &fedora_like());
        assert!(
            diags.iter().any(|d| d.message.contains("_mandir")),
            "{diags:?}"
        );
    }

    #[test]
    fn silent_when_specific_already_used() {
        let src = "Name: x\n%files\n%{_bindir}/foo\n";
        let diags = run_with_profile(src, &fedora_like());
        assert!(diags.iter().all(|d| d.lint_id != "RPM490"));
    }

    #[test]
    fn silent_at_boundary_after_suffix() {
        // `%{_prefix}/binary` is NOT a hit for `_bindir` — boundary
        // rule rejects the extra `ary`.
        let src = "Name: x\n%files\n%{_prefix}/binary/whatever\n";
        let diags = run_with_profile(src, &fedora_like());
        assert!(diags.iter().all(|d| d.lint_id != "RPM490"));
    }

    #[test]
    fn silent_when_profile_missing_specific_macro() {
        // Profile without `_bindir` — no candidate validates.
        let profile = make_test_profile(None, None, &[("_prefix", "/usr")], &[]);
        let src = "Name: x\n%files\n%{_prefix}/bin/foo\n";
        let diags = run_with_profile(src, &profile);
        assert!(diags.iter().all(|d| d.lint_id != "RPM490"));
    }

    #[test]
    fn flags_inside_buildroot_prefix() {
        let src = "Name: x\n%install\nmkdir -p %{buildroot}%{_prefix}/share/foo\n";
        let diags = run_with_profile(src, &fedora_like());
        assert!(
            diags.iter().any(|d| d.message.contains("_datadir")),
            "{diags:?}"
        );
    }

    #[test]
    fn silent_in_dep_tags() {
        let src = "Name: x\nRequires: %{_prefix}/bin/somefile\n";
        let diags = run_with_profile(src, &fedora_like());
        // is_safe_tag excludes Requires.
        assert!(diags.iter().all(|d| d.lint_id != "RPM490"));
    }

    #[test]
    fn precomputes_macro_forms_outside_hot_path() {
        // Smoke check: after the refactor that hoists `format!` calls
        // out of `match_here`, both `%{NAME}` and `%NAME` lookup forms
        // still match. The braced form fires on `%{_prefix}/bin`; the
        // plain form fires on `%_prefix/bin`.
        let profile = fedora_like();
        let candidates = validate_candidates(&profile);
        assert!(!candidates.is_empty(), "expected active candidates");
        for cand in &candidates {
            assert_eq!(cand.braced, format!("%{{{}}}", cand.general));
            assert_eq!(cand.plain, format!("%{}", cand.general));
        }
        // Plain `%_prefix/bin` (no braces) should still trigger.
        let src = "Name: x\n%install\ncp foo %_prefix/bin/foo\n";
        let diags = run_with_profile(src, &profile);
        assert!(
            diags.iter().any(|d| d.lint_id == "RPM490"),
            "plain `%NAME` form should still match: {diags:?}"
        );
    }
}
