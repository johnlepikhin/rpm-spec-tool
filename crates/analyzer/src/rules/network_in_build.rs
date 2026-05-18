//! RPM388 `network-access-in-build` — `%build`, `%install`, or
//! `%check` invokes a tool that wants the network (`curl`, `wget`,
//! `git clone`, `pip install`, `cargo install`, `npm install`,
//! `go get`, …).
//!
//! Distribution builders (Mock, Koji, OBS, ALT Linux's `hasher`) run
//! the build inside an offline chroot — the network call will fail
//! with "Could not resolve host" or "Connection refused". The pattern
//! happens most often when a spec is adapted from a `Dockerfile` or
//! upstream CI script that assumes a live internet.
//!
//! The rule is family-gated to the distributions where the offline
//! convention is universal (Fedora, RHEL, openSUSE, Mageia, ALT). On
//! Generic / unknown profiles it stays silent — a custom build
//! environment might legitimately have network access.
//!
//! Allow-list: some commands have offline-only sub-flags (`git
//! checkout`, `git apply`, `pip install --no-index`). The rule
//! sub-classifies known commands by their first non-flag argument
//! and silences the offline variants.

use std::collections::BTreeSet;

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{CommandUseIndex, SectionRef, ShellToken, first_non_flag_arg};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM388",
    name: "network-access-in-build",
    description: "Build script invokes a network-fetching command (`curl`, `wget`, `git clone`, \
                  `pip install`, …). Mock/Koji/OBS run the build in an offline chroot — the \
                  fetch will fail.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// RPM388 lint: detects build-script commands that require the network.
///
/// Gated to families whose canonical build environment is an offline
/// chroot (see [`rpm_spec_profile::Family::has_offline_build_chroot`]).
#[derive(Debug, Default)]
pub struct NetworkAccessInBuild {
    diagnostics: Vec<Diagnostic>,
    enabled: bool,
}

impl NetworkAccessInBuild {
    /// Construct an instance with no diagnostics and gating disabled
    /// (the profile is wired in later via [`Lint::set_profile`]).
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for NetworkAccessInBuild {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if !self.enabled {
            return;
        }
        let idx = CommandUseIndex::from_spec(spec);
        // Emit at most one diag per (section_span, command) so a build
        // that downloads three tarballs flags once per command.
        let mut seen: BTreeSet<(usize, &'static str)> = BTreeSet::new();
        for call in idx.all() {
            if !matches!(call.location, SectionRef::BuildScript { .. }) {
                continue;
            }
            let Some(cmd) = call.name.as_deref() else {
                continue;
            };
            let Some(reason) = classify_call(cmd, &call.tokens) else {
                continue;
            };
            let key = (call.location.section_span().start_byte, reason.dedup_key);
            if !seen.insert(key) {
                continue;
            }
            // NOTE: `CommandUse` exposes only section-level spans
            // (`location.section_span()`) plus a `line_idx`. There is
            // no per-line `Span` on the struct, so the diagnostic is
            // anchored at the section. If a per-line span is added
            // later, swap this in.
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "build script invokes `{cmd}` which fetches over the network ({explanation}); \
                     clean-chroot builds (Mock/Koji/OBS) run offline and this will fail; \
                     remediation: {hint}",
                    explanation = reason.detail,
                    hint = reason.fix_hint,
                ),
                call.location.section_span(),
            ));
        }
    }
}

/// Result of analysing a single command call.
///
/// * `dedup_key` — stable, command-class identifier used only to
///   suppress duplicate diagnostics within the same section. Not a
///   normalized form of the command (the original name is preserved
///   in the diagnostic message).
/// * `detail` — human-facing description of what the command does.
/// * `fix_hint` — short remediation suggestion appended to the
///   diagnostic message.
struct NetworkCall {
    dedup_key: &'static str,
    detail: &'static str,
    fix_hint: &'static str,
}

/// Classify a build-script command. Returns `Some` when the command
/// is known to require the network (and isn't using an offline-only
/// sub-flag), `None` otherwise.
fn classify_call(cmd: &str, tokens: &[ShellToken]) -> Option<NetworkCall> {
    match cmd {
        "curl" | "wget" | "aria2c" | "fetch" => Some(NetworkCall {
            dedup_key: "curl/wget",
            detail: "downloads a URL",
            fix_hint: "include the download as `Source:` or vendor it",
        }),
        "git" => {
            let sub = first_non_flag_arg(tokens);
            match sub.as_deref() {
                Some("clone" | "fetch" | "pull" | "push" | "remote-update") => Some(NetworkCall {
                    dedup_key: "git-network",
                    detail: "talks to a remote repository",
                    fix_hint: "vendor the upstream repo as a tarball via `Source:` and \
                               `git apply` patches in `%prep`",
                }),
                _ => None,
            }
        }
        "pip" | "pip3" => {
            let sub = first_non_flag_arg(tokens);
            if matches!(sub.as_deref(), Some("install" | "download" | "wheel")) {
                if tokens
                    .iter()
                    .any(|t| t.literal_str().as_deref() == Some("--no-index"))
                {
                    return None;
                }
                return Some(NetworkCall {
                    dedup_key: "pip-install",
                    detail: "fetches Python packages from PyPI",
                    fix_hint: "pass `--no-index` and vendor the wheels",
                });
            }
            None
        }
        "cargo" => {
            let sub = first_non_flag_arg(tokens);
            match sub.as_deref() {
                Some("install" | "update" | "fetch" | "search" | "publish") => Some(NetworkCall {
                    dedup_key: "cargo-network",
                    detail: "fetches Rust crates from crates.io",
                    fix_hint: "use `cargo install --offline` and vendor `vendor/`",
                }),
                _ => None,
            }
        }
        "npm" | "yarn" | "pnpm" => {
            let sub = first_non_flag_arg(tokens);
            if matches!(sub.as_deref(), Some("install" | "ci" | "update" | "i")) {
                if tokens
                    .iter()
                    .any(|t| t.literal_str().as_deref() == Some("--offline"))
                {
                    return None;
                }
                return Some(NetworkCall {
                    dedup_key: "npm-install",
                    detail: "fetches Node packages from the registry",
                    fix_hint: "pass `--offline` and vendor `node_modules` or use \
                               `npm ci --cache <dir> --offline`",
                });
            }
            None
        }
        "go" => {
            let sub = first_non_flag_arg(tokens);
            match sub.as_deref() {
                Some("get" | "mod") => Some(NetworkCall {
                    dedup_key: "go-network",
                    detail: "fetches Go modules from upstream",
                    fix_hint: "vendor with `go mod vendor` and use `go build -mod=vendor`",
                }),
                _ => None,
            }
        }
        "gem" => {
            let sub = first_non_flag_arg(tokens);
            if matches!(sub.as_deref(), Some("install" | "fetch" | "update")) {
                return Some(NetworkCall {
                    dedup_key: "gem-install",
                    detail: "fetches Ruby gems",
                    fix_hint: "vendor the gem and install from path",
                });
            }
            None
        }
        _ => None,
    }
}

impl Lint for NetworkAccessInBuild {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn applies_to_profile(&self, profile: &Profile) -> bool {
        profile
            .identity
            .family
            .is_some_and(rpm_spec_profile::Family::has_offline_build_chroot)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.enabled = profile
            .identity
            .family
            .is_some_and(rpm_spec_profile::Family::has_offline_build_chroot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint_with_profile;
    use crate::session::parse;
    use rpm_spec_profile::Family;

    fn fedora() -> Profile {
        let mut p = Profile::default();
        p.identity.family = Some(Family::Fedora);
        p
    }

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint_with_profile::<NetworkAccessInBuild>(src, &fedora())
    }

    #[test]
    fn flags_curl_in_build() {
        let src = "Name: x\n%build\ncurl -O https://example.com/data.tar\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM388");
    }

    #[test]
    fn flags_wget_in_install() {
        let src = "Name: x\n%install\nwget https://example.com/x\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_aria2c() {
        let src = "Name: x\n%build\naria2c http://example.com/x\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("aria2c"));
    }

    #[test]
    fn flags_git_clone() {
        let src = "Name: x\n%build\ngit clone https://example.com/repo.git\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("git"));
    }

    #[test]
    fn silent_for_git_checkout() {
        // `git checkout` (offline) shouldn't fire.
        let src = "Name: x\n%build\ngit checkout main\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_pip_install() {
        let src = "Name: x\n%build\npip install requests\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_pip_install_no_index() {
        // `pip install --no-index` reads only the local cache.
        let src = "Name: x\n%build\npip install --no-index --find-links=. mypkg\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_cargo_install() {
        let src = "Name: x\n%build\ncargo install --path .\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_cargo_build_not_in_table() {
        // `cargo build` is not in the network-affecting sub-command
        // table (the rule only knows install/update/fetch/search/publish).
        let src = "Name: x\n%build\ncargo build\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_npm_install() {
        let src = "Name: x\n%build\nnpm install\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_npm_install_offline() {
        let src = "Name: x\n%build\nnpm install --offline\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_yarn_install() {
        let src = "Name: x\n%build\nyarn install\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("yarn"));
    }

    #[test]
    fn flags_pnpm_install() {
        let src = "Name: x\n%build\npnpm install\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("pnpm"));
    }

    #[test]
    fn flags_go_get() {
        let src = "Name: x\n%build\ngo get example.com/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("go"));
    }

    #[test]
    fn flags_go_mod() {
        let src = "Name: x\n%build\ngo mod download\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("go"));
    }

    #[test]
    fn flags_gem_install() {
        let src = "Name: x\n%build\ngem install foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("gem"));
    }

    #[test]
    fn silent_in_scriptlet() {
        // %post running curl is a different problem (RPM349/...) — out of scope here.
        let src = "Name: x\n%post\ncurl http://example.com\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_on_generic_profile() {
        let outcome = parse("Name: x\n%build\ncurl -O https://example.com/x\n");
        let mut lint = NetworkAccessInBuild::new();
        lint.set_profile(&Profile::default());
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn deduplicates_repeated_calls() {
        // Three curl calls in %build → one diag.
        let src = "Name: x\n%build\ncurl a\ncurl b\ncurl c\n";
        assert_eq!(run(src).len(), 1);
    }
}
