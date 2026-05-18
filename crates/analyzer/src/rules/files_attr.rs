//! RPM370 `suspicious-attr-permissions` — flag `%attr(...)` values
//! that grant excessive permissions or set group/other write without
//! a sticky bit.
//!
//! Cases:
//!
//! - Any `%attr(0777, ...)` or `%attr(0666, ...)`. World-writable
//!   files are almost always a packaging bug.
//! - World-writable without sticky bit (`o+w` set, `sticky` not set):
//!   raises severity to `Deny`. Sticky-set forms like `01777` (for
//!   `/tmp`-like dirs) are accepted.
//! - Setuid (mode & 04000) or setgid (mode & 02000) emit a warn-level
//!   "review needed" diagnostic; they're not always wrong but should
//!   never slip past review unnoticed.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::files::{FilesClassifier, for_each_files_entry};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;
use rpm_spec_profile::Profile;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM370",
    name: "suspicious-attr-permissions",
    description: "`%attr(...)` grants suspicious permissions: world-writable, setuid/setgid, or \
                  777 on a regular file.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// `%attr(...)` grants suspicious permissions: world-writable, setuid/setgid, or 777 on a regular file.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SuspiciousAttrPermissions {
    diagnostics: Vec<Diagnostic>,
    profile: Profile,
}

impl SuspiciousAttrPermissions {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SuspiciousAttrPermissions {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let classifier = FilesClassifier::new(&self.profile);
        for_each_files_entry(spec, |entry| {
            let cls = classifier.classify(entry);
            let Some(attr) = cls.directives.attr else {
                return;
            };
            let Some(mode) = attr.mode else {
                return;
            };
            let path = cls.resolved_path.as_deref().unwrap_or("");
            if let Some((severity, reason)) = classify_mode(mode) {
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    severity,
                    format!("`%attr({mode:04o}, …)` on `{path}` — {reason}"),
                    cls.span(),
                ));
            }
        });
    }
}

// POSIX mode bits, named like libc's `<sys/stat.h>` constants so a
// reader cross-referencing the kernel headers finds them instantly.
/// Set-user-ID on execution.
const S_ISUID: u32 = 0o4000;
/// Set-group-ID on execution.
const S_ISGID: u32 = 0o2000;
/// Sticky bit (restricted deletion flag).
const S_ISVTX: u32 = 0o1000;
/// Other has write permission.
const S_IWOTH: u32 = 0o002;
/// Group has write permission.
const S_IWGRP: u32 = 0o020;
/// Group has read + write + execute (used to detect "group-writable
/// *and* group-readable" together).
const S_IRWXG: u32 = 0o060;
/// Mask covering rwx-for-owner/group/other (no setuid/setgid/sticky).
const PERM_MASK: u32 = 0o777;

/// Decide whether a numeric mode warrants a diagnostic. Returns
/// `(severity, human_reason)` for the *worst* offence seen — checks
/// are priority-ordered (highest severity first), so combined cases
/// like setuid+setgid surface only the higher-severity bit.
/// Conservative on overlap: a single message keeps the diagnostic
/// stream readable.
fn classify_mode(mode: u32) -> Option<(Severity, &'static str)> {
    let setuid = mode & S_ISUID != 0;
    let setgid = mode & S_ISGID != 0;
    let sticky = mode & S_ISVTX != 0;
    let other_write = mode & S_IWOTH != 0;
    let group_write = mode & S_IWGRP != 0;
    let all_perm = mode & PERM_MASK;

    // Sticky bit on world-writable dirs (e.g. `/tmp` → `01777`) is
    // the canonical "shared spool" idiom; treat it as the explicit
    // opt-in to o+w and let the rest of the checks ignore that case.
    if all_perm == 0o777 && !sticky {
        return Some((
            Severity::Deny,
            "777 grants read/write/execute to everyone — review and tighten",
        ));
    }
    if all_perm == 0o666 && !sticky {
        return Some((
            Severity::Deny,
            "666 grants read/write to everyone — review and tighten",
        ));
    }
    if other_write && !sticky {
        return Some((
            Severity::Deny,
            "world-writable without sticky bit — use 1xxx if a shared spool, otherwise drop o+w",
        ));
    }
    if setuid {
        return Some((Severity::Warn, "setuid bit set — security review required"));
    }
    if setgid {
        return Some((Severity::Warn, "setgid bit set — security review required"));
    }
    if group_write && all_perm & S_IRWXG == S_IRWXG && !sticky {
        // Group-writable + group-readable; not always wrong but worth
        // flagging at the lowest noticeable severity. Sticky-mode dirs
        // (e.g. `1777` for `/tmp`-like spools) are explicitly opting
        // in to broad write access and are silenced above — skip the
        // group-write check too.
        return Some((
            Severity::Warn,
            "group-writable — confirm the group ownership is intended",
        ));
    }
    None
}

impl Lint for SuspiciousAttrPermissions {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
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
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<SuspiciousAttrPermissions>(src)
    }

    #[test]
    fn flags_777_as_deny() {
        let src = "Name: x\n%files\n%attr(0777,root,root) /usr/bin/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM370");
        assert_eq!(diags[0].severity, Severity::Deny);
    }

    #[test]
    fn flags_world_writable_without_sticky() {
        let src = "Name: x\n%files\n%attr(0666,root,root) /tmp/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Deny);
    }

    #[test]
    fn silent_for_world_writable_with_sticky() {
        let src = "Name: x\n%files\n%attr(1777,root,root) /var/spool/foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_setuid_as_warn() {
        let src = "Name: x\n%files\n%attr(4755,root,root) /usr/bin/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warn);
    }

    #[test]
    fn flags_setgid_as_warn() {
        let src = "Name: x\n%files\n%attr(2755,root,root) /usr/bin/foo\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_for_normal_modes() {
        let src = "Name: x\n%files\n%attr(0755,root,root) /usr/bin/foo\n%attr(0644,root,root) /etc/foo.conf\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn classify_mode_setuid_wins_over_setgid_combo() {
        // `06755` = setuid + setgid + 755. The priority-ordered chain
        // surfaces setuid first; this test pins that behaviour so any
        // future re-ordering is a conscious decision.
        let (sev, reason) = classify_mode(0o6755).expect("combined mode must flag");
        assert_eq!(sev, Severity::Warn);
        assert!(reason.contains("setuid"), "reason: {reason}");
    }

    #[test]
    fn classify_mode_world_writable_not_777_or_666() {
        // `0o646` — other-writable but not the high-profile 777/666.
        // Should still be Deny via the "world-writable without sticky"
        // branch.
        let (sev, _reason) = classify_mode(0o646).expect("o+w must flag");
        assert_eq!(sev, Severity::Deny);
    }

    #[test]
    fn classify_mode_group_writable_alone() {
        // `0o064` — group rw + read, no other-write, no setuid/setgid.
        // Falls into the lowest-severity group-writable branch.
        let (sev, reason) = classify_mode(0o064).expect("group rw must flag");
        assert_eq!(sev, Severity::Warn);
        assert!(reason.contains("group"), "reason: {reason}");
    }

    #[test]
    fn classify_mode_silent_for_plain_owner_only_modes() {
        assert!(classify_mode(0o700).is_none());
        assert!(classify_mode(0o644).is_none());
        assert!(classify_mode(0o755).is_none());
    }
}
