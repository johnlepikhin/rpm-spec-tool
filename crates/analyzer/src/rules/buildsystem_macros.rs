//! RPM390 `buildsystem-macro-modernization` — `%build` invokes a
//! build system directly (`cmake .`, `meson setup …`, `cargo build`,
//! `python3 setup.py build`, `go build`, …) instead of using the
//! distro's wrapper macro (`%cmake`, `%meson`, `%cargo_build`,
//! `%py3_build` / `%pyproject_*`, `%gobuild`).
//!
//! Wrapper macros do three things the bare invocation doesn't: they
//! plumb `%{optflags}` / `%{build_ldflags}` through automatically, they
//! pass the distro's per-architecture options (e.g. CMake `-DCMAKE_*`
//! defaults), and they keep the call site stable across distro
//! version bumps. Bare invocations get out of sync with the macro
//! conventions and drop hardening, similar to RPM385.
//!
//! Family-gated by macro presence: the rule fires only when the
//! active profile actually defines the suggested replacement macro.
//! On a profile without `%cmake`, the suggestion would be wrong, so
//! the rule stays silent.

use std::collections::BTreeSet;

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{CommandUseIndex, SectionRef, ShellToken, first_non_flag_arg};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM390",
    name: "buildsystem-macro-modernization",
    description: "Build script invokes a build system directly (`cmake`, `meson`, `cargo`, …) \
                  instead of the distro's wrapper macro (`%cmake`, `%meson`, `%cargo_build`, …). \
                  The wrappers plumb `%{optflags}` and per-arch defaults; bare calls drop them.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint state for RPM390 `buildsystem-macro-modernization`.
///
/// `available` is populated from the active profile's macro table and
/// is used as a membership-test set when deciding whether to suggest a
/// wrapper macro replacement for a bare build-system invocation.
#[derive(Debug, Default)]
pub struct BuildsystemMacroModernization {
    diagnostics: Vec<Diagnostic>,
    available: BTreeSet<&'static str>,
}

impl BuildsystemMacroModernization {
    /// Create a fresh lint instance with no profile bound.
    ///
    /// Call [`Lint::set_profile`] to populate the `available` macro set
    /// before visiting a spec; otherwise the rule stays silent.
    pub fn new() -> Self {
        Self::default()
    }
}

/// All wrapper macros this rule knows about. The presence of an entry
/// in `Profile::macros` gates the corresponding suggestion.
const KNOWN_MACROS: &[&str] = &[
    "cmake",
    "cmake_build",
    "cmake_install",
    "meson",
    "meson_build",
    "meson_install",
    "cargo_build",
    "cargo_install",
    "py3_build",
    "py3_install",
    "pyproject_wheel",
    "pyproject_install",
    "gobuild",
    "goinstall",
];

impl<'ast> Visit<'ast> for BuildsystemMacroModernization {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.available.is_empty() {
            return;
        }
        let idx = CommandUseIndex::from_spec(spec);
        // Dedup by (section, suggestion) so a build with three `cmake`
        // lines emits once.
        let mut seen: BTreeSet<(usize, &'static str)> = BTreeSet::new();
        for call in idx.all() {
            let SectionRef::BuildScript { kind, .. } = call.location else {
                continue;
            };
            // Only %build/%install — %prep/%clean/%check rarely invoke
            // these tools, and %check usually wants `meson test` etc.
            // which is the right shape.
            if !matches!(
                kind,
                rpm_spec::ast::BuildScriptKind::Build | rpm_spec::ast::BuildScriptKind::Install
            ) {
                continue;
            }
            let Some(cmd) = call.name.as_deref() else {
                continue;
            };
            let Some(suggestion) = suggest_macro(cmd, &call.tokens, &self.available) else {
                continue;
            };
            let key = (call.location.section_span().start_byte, suggestion);
            if !seen.insert(key) {
                continue;
            }
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "build script invokes `{cmd}` directly; the active profile defines \
                     `%{suggestion}` — prefer the macro so distro hardening flags and \
                     per-arch defaults are applied (replace with `%{suggestion}`)"
                ),
                call.location.section_span(),
            ));
        }
    }
}

/// Map a `(command, args)` pair to the wrapper macro that should
/// replace it, when the macro is defined on the active profile.
fn suggest_macro(
    cmd: &str,
    tokens: &[ShellToken],
    available: &BTreeSet<&'static str>,
) -> Option<&'static str> {
    let sub = first_non_flag_arg(tokens);
    let candidate: &'static str = match cmd {
        "cmake" => match sub.as_deref() {
            // `cmake --build …` / `cmake --install …` use the long
            // flag, so first_non_flag_arg skips them; detect by
            // scanning the whole token list.
            _ if has_token(tokens, "--build") => "cmake_build",
            _ if has_token(tokens, "--install") => "cmake_install",
            // Anything else (`cmake .`, `cmake ..`, `cmake -S …`) is
            // the configure step.
            _ => "cmake",
        },
        "meson" => match sub.as_deref() {
            Some("setup") | None => "meson",
            Some("compile") => "meson_build",
            Some("install") => "meson_install",
            _ => return None,
        },
        "cargo" => match sub.as_deref() {
            Some("build") => "cargo_build",
            Some("install") => "cargo_install",
            _ => return None,
        },
        "python" | "python3" => {
            // `python3 setup.py build` → `%py3_build`;
            // `python3 setup.py install` → `%py3_install`.
            // `python3 -m build` → `%pyproject_wheel`.
            if has_token(tokens, "setup.py") {
                if has_token(tokens, "build") {
                    "py3_build"
                } else if has_token(tokens, "install") {
                    "py3_install"
                } else {
                    return None;
                }
            } else if has_subseq(tokens, &["-m", "build"]) {
                "pyproject_wheel"
            } else {
                return None;
            }
        }
        "go" => match sub.as_deref() {
            Some("build") => "gobuild",
            Some("install") => "goinstall",
            _ => return None,
        },
        _ => return None,
    };
    if available.contains(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

/// `true` if any token matches `needle` literally.
fn has_token(tokens: &[ShellToken], needle: &str) -> bool {
    tokens
        .iter()
        .any(|t| t.literal_str().as_deref() == Some(needle))
}

/// `true` if `tokens` contains `needles` as a consecutive subsequence.
fn has_subseq(tokens: &[ShellToken], needles: &[&str]) -> bool {
    if needles.is_empty() {
        return true;
    }
    tokens.windows(needles.len()).any(|win| {
        win.iter()
            .zip(needles)
            .all(|(tok, n)| tok.literal_str().as_deref() == Some(*n))
    })
}

impl Lint for BuildsystemMacroModernization {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        KNOWN_MACROS.iter().any(|m| profile.macros.get(m).is_some())
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.available = KNOWN_MACROS
            .iter()
            .copied()
            .filter(|m| profile.macros.get(m).is_some())
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};

    fn profile_with(macros: &[&str]) -> Profile {
        let mut p = Profile::default();
        for m in macros {
            p.macros
                .insert(*m, MacroEntry::literal("body", Provenance::Override));
        }
        p
    }

    fn run_with(src: &str, profile: &Profile) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = BuildsystemMacroModernization::new();
        lint.set_profile(profile);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_bare_cmake_when_macro_available() {
        let src = "Name: x\n%build\ncmake .\n";
        let diags = run_with(src, &profile_with(&["cmake"]));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM390");
        assert!(diags[0].message.contains("%cmake"));
    }

    #[test]
    fn flags_cmake_build() {
        let src = "Name: x\n%build\ncmake --build build\n";
        let diags = run_with(src, &profile_with(&["cmake_build"]));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%cmake_build"));
    }

    #[test]
    fn flags_cmake_install() {
        let src = "Name: x\n%install\ncmake --install build\n";
        let diags = run_with(src, &profile_with(&["cmake_install"]));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%cmake_install"));
    }

    #[test]
    fn flags_meson_setup() {
        let src = "Name: x\n%build\nmeson setup builddir\n";
        let diags = run_with(src, &profile_with(&["meson"]));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%meson"));
    }

    #[test]
    fn flags_cargo_build() {
        let src = "Name: x\n%build\ncargo build --release\n";
        let diags = run_with(src, &profile_with(&["cargo_build"]));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_python_setup_py_build() {
        let src = "Name: x\n%build\npython3 setup.py build\n";
        let diags = run_with(src, &profile_with(&["py3_build"]));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("%py3_build"));
    }

    #[test]
    fn flags_python_m_build() {
        let src = "Name: x\n%build\npython3 -m build --wheel\n";
        let diags = run_with(src, &profile_with(&["pyproject_wheel"]));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_go_build() {
        let src = "Name: x\n%build\ngo build ./cmd/foo\n";
        let diags = run_with(src, &profile_with(&["gobuild"]));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_when_macro_unavailable() {
        // Profile has no `cmake` macro → no suggestion.
        let src = "Name: x\n%build\ncmake .\n";
        assert!(run_with(src, &Profile::default()).is_empty());
    }

    #[test]
    fn silent_for_unrelated_command() {
        let src = "Name: x\n%build\nmake\n";
        let diags = run_with(src, &profile_with(&["cmake", "meson"]));
        assert!(diags.is_empty());
    }

    #[test]
    fn silent_in_check_section() {
        // Rule scoped to %build/%install. `meson test` is correct in %check.
        let src = "Name: x\n%check\nmeson test\n";
        assert!(run_with(src, &profile_with(&["meson"])).is_empty());
    }

    #[test]
    fn deduplicates_repeated_calls() {
        let src = "Name: x\n%build\ncmake .\ncmake .\n";
        let diags = run_with(src, &profile_with(&["cmake"]));
        assert_eq!(diags.len(), 1);
    }
}
