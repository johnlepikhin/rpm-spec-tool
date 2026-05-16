//! `%config` directive policy: RPM360, RPM361, RPM362.
//!
//! Three rules that share `FilesClassifier`-driven path/directive
//! inspection but anchor at different problem shapes:
//!
//! - **RPM360 `etc-file-not-config`** — files under `/etc` (or
//!   `%{_sysconfdir}`) without a `%config` directive. Without
//!   `%config` RPM blindly overwrites local edits on every upgrade.
//! - **RPM361 `config-under-usr`** — `%config` on a path that lives
//!   under `/usr`. `/usr` is read-only by Filesystem Hierarchy
//!   Standard / openSUSE rules; configuration belongs in `/etc`.
//! - **RPM362 `plain-config-without-comment`** — `%config` *without*
//!   `noreplace` and *without* an explanatory comment nearby. Plain
//!   `%config` overwrites user changes on upgrade in some scenarios;
//!   requiring a comment forces the maintainer to spell out why
//!   that's safe.

use rpm_spec::ast::{FilesContent, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{
    ConfigKind, FilesClassifier, for_each_files_entry, for_each_files_section, neighbour_is_comment,
};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

// =====================================================================
// RPM360 etc-file-not-config
// =====================================================================

pub static ETC_NOT_CONFIG_METADATA: LintMetadata = LintMetadata {
    id: "RPM360",
    name: "etc-file-not-config",
    description: "A file under `/etc` (or `%{_sysconfdir}`) is listed without `%config`. RPM \
                  will overwrite local edits on every upgrade — mark it as `%config(noreplace)`.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct EtcFileNotConfig {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl EtcFileNotConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for EtcFileNotConfig {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            if !cls.kind_hints.under_etc {
                return;
            }
            if cls.directives.is_dir || cls.directives.is_ghost || cls.directives.config.is_some() {
                return;
            }
            let Some(ref path) = cls.resolved_path else {
                return;
            };
            self.diagnostics.push(Diagnostic::new(
                &ETC_NOT_CONFIG_METADATA,
                Severity::Warn,
                format!(
                    "`{path}` lives under `/etc` but is not marked `%config`; upgrades will \
                     overwrite local edits — prefer `%config(noreplace) {path}`"
                ),
                cls.span(),
            ));
        });
    }
}

impl Lint for EtcFileNotConfig {
    fn metadata(&self) -> &'static LintMetadata {
        &ETC_NOT_CONFIG_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

// =====================================================================
// RPM361 config-under-usr
// =====================================================================

pub static CONFIG_UNDER_USR_METADATA: LintMetadata = LintMetadata {
    id: "RPM361",
    name: "config-under-usr",
    description: "`%config` is applied to a path under `/usr`. The FHS treats `/usr` as \
                  read-only — configuration belongs in `/etc`.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct ConfigUnderUsr {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl ConfigUnderUsr {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ConfigUnderUsr {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            if cls.directives.config.is_none() {
                return;
            }
            if !cls.kind_hints.under_usr {
                return;
            }
            let Some(ref path) = cls.resolved_path else {
                return;
            };
            self.diagnostics.push(Diagnostic::new(
                &CONFIG_UNDER_USR_METADATA,
                Severity::Warn,
                format!(
                    "`%config` set on `{path}` which lives under `/usr`; move runtime \
                     configuration to `/etc` per FHS"
                ),
                cls.span(),
            ));
        });
    }
}

impl Lint for ConfigUnderUsr {
    fn metadata(&self) -> &'static LintMetadata {
        &CONFIG_UNDER_USR_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

// =====================================================================
// RPM362 plain-config-without-comment
// =====================================================================

pub static PLAIN_CONFIG_METADATA: LintMetadata = LintMetadata {
    id: "RPM362",
    name: "plain-config-without-comment",
    description: "`%config` without `noreplace` is risky — on upgrade rpm may overwrite local \
                  edits with the package default. Either switch to `%config(noreplace)` or \
                  leave a comment explaining why plain `%config` is intended.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

#[derive(Debug, Default)]
pub struct PlainConfigWithoutComment {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl PlainConfigWithoutComment {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for PlainConfigWithoutComment {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_section(spec, |sec| {
            for (i, item) in sec.content.iter().enumerate() {
                let FilesContent::Entry(entry) = item else {
                    continue;
                };
                let cls = classifier.classify(entry);
                if cls.directives.config != Some(ConfigKind::Plain) {
                    continue;
                }
                if neighbour_is_comment(sec.content, i) {
                    continue;
                }
                let path = cls.resolved_path.as_deref().unwrap_or("");
                self.diagnostics.push(Diagnostic::new(
                    &PLAIN_CONFIG_METADATA,
                    Severity::Warn,
                    format!(
                        "plain `%config` on `{path}` without `noreplace` and no neighbouring \
                         comment — explain or switch to `%config(noreplace)`"
                    ),
                    cls.span(),
                ));
            }
        });
    }
}

impl Lint for PlainConfigWithoutComment {
    fn metadata(&self) -> &'static LintMetadata {
        &PLAIN_CONFIG_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = profile.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};

    fn fedora_profile() -> Profile {
        let mut p = Profile::default();
        for (name, body) in [
            ("_prefix", "/usr"),
            ("_bindir", "/usr/bin"),
            ("_libdir", "/usr/lib64"),
            ("_datadir", "/usr/share"),
            ("_sysconfdir", "/etc"),
        ] {
            p.macros
                .insert(name, MacroEntry::literal(body, Provenance::Override));
        }
        p
    }

    fn run_360(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = EtcFileNotConfig::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_361(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ConfigUnderUsr::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_362(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = PlainConfigWithoutComment::new();
        lint.set_profile(&fedora_profile());
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM360 -----

    #[test]
    fn rpm360_flags_etc_file_not_config() {
        let src = "Name: x\n%files\n/etc/foo.conf\n";
        let diags = run_360(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM360");
    }

    #[test]
    fn rpm360_silent_when_config_present() {
        let src = "Name: x\n%files\n%config(noreplace) /etc/foo.conf\n";
        assert!(run_360(src).is_empty());
    }

    #[test]
    fn rpm360_silent_for_ghost_etc_file() {
        let src = "Name: x\n%files\n%ghost /etc/foo.cache\n";
        assert!(run_360(src).is_empty());
    }

    #[test]
    fn rpm360_silent_for_dir_under_etc() {
        let src = "Name: x\n%files\n%dir /etc/foo\n";
        assert!(run_360(src).is_empty());
    }

    // ----- RPM361 -----

    #[test]
    fn rpm361_flags_config_under_usr() {
        let src = "Name: x\n%files\n%config /usr/share/foo/settings.conf\n";
        let diags = run_361(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM361");
    }

    #[test]
    fn rpm361_silent_for_config_under_etc() {
        let src = "Name: x\n%files\n%config(noreplace) /etc/foo.conf\n";
        assert!(run_361(src).is_empty());
    }

    #[test]
    fn rpm361_silent_when_no_config_directive() {
        let src = "Name: x\n%files\n/usr/share/foo/data.txt\n";
        assert!(run_361(src).is_empty());
    }

    // ----- RPM362 -----

    #[test]
    fn rpm362_flags_plain_config_without_comment() {
        let src = "Name: x\n%files\n%config /etc/foo.conf\n";
        let diags = run_362(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM362");
    }

    #[test]
    fn rpm362_silent_for_config_noreplace() {
        let src = "Name: x\n%files\n%config(noreplace) /etc/foo.conf\n";
        assert!(run_362(src).is_empty());
    }

    #[test]
    fn rpm362_silent_with_preceding_comment() {
        let src = "Name: x\n%files\n# bundled default; intentionally overwriteable\n%config /etc/foo.conf\n";
        assert!(run_362(src).is_empty());
    }

    #[test]
    fn rpm362_silent_with_following_comment() {
        let src = "Name: x\n%files\n%config /etc/foo.conf\n# justification here\n";
        assert!(run_362(src).is_empty());
    }
}
