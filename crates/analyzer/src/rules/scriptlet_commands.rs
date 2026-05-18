//! Scriptlet-command hygiene: RPM342, RPM349.
//!
//! - **RPM342 `direct-systemctl-in-scriptlet`** — calls to `systemctl`
//!   (enable/start/restart/stop/disable/daemon-reload) belong in
//!   distro-supplied helpers (`%systemd_post` on Fedora,
//!   `%service_add_post` on openSUSE). Calling `systemctl` directly
//!   bypasses the distro's "are we in a chroot? a container? a
//!   non-systemd init?" sanity checks and breaks builds for images
//!   without systemd.
//! - **RPM349 `scriptlet-state-outside-rpm-state`** — scriptlets that
//!   stash state under `/tmp` or `/var/tmp` race with parallel
//!   transactions and orphan the file when the transaction aborts.
//!   Use `$RPM_STATE_DIR` (or `/var/lib/rpm-state/<pkg>`) so RPM
//!   owns the lifecycle.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{CommandUseIndex, SectionRef};
use crate::visit::Visit;

// =====================================================================
// RPM342 direct-systemctl-in-scriptlet
// =====================================================================

pub static DIRECT_SYSTEMCTL_METADATA: LintMetadata = LintMetadata {
    id: "RPM342",
    name: "direct-systemctl-in-scriptlet",
    description: "A scriptlet invokes `systemctl` directly. Use the distro-provided helpers \
                  (`%systemd_post` / `%service_add_post` etc.) so the unit lifecycle is \
                  managed by macros that handle non-systemd targets, chroots, and image \
                  builds.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// A scriptlet invokes `systemctl` directly. Use the distro-provided helpers (`%systemd_post` / `%service_add_post` etc.) so the unit lifecycle is managed by macros that handle non-systemd targets, chroots, and image builds.
///
/// See [`DIRECT_SYSTEMCTL_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DirectSystemctlInScriptlet {
    diagnostics: Vec<Diagnostic>,
}

impl DirectSystemctlInScriptlet {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `systemctl` sub-commands that carry mutating semantics. `status`,
/// `is-active`, etc. are silenced — we flag side effects, not queries.
const MUTATING_SUBCOMMANDS: &[&str] = &[
    "enable",
    "disable",
    "start",
    "stop",
    "restart",
    "reload",
    "try-restart",
    "reload-or-restart",
    "preset",
    "preset-all",
    "mask",
    "unmask",
    "daemon-reload",
    "daemon-reexec",
];

impl<'ast> Visit<'ast> for DirectSystemctlInScriptlet {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let idx = CommandUseIndex::from_spec(spec);
        for use_ in idx.find("systemctl") {
            if !matches!(use_.location, SectionRef::Scriptlet { .. }) {
                continue;
            }
            // Look at the first non-flag argument for the subcommand
            // name. Skip flags (`-q`, `--quiet`, `--now`, `--system`,
            // `--user`).
            let Some(subcmd) = first_subcommand(&use_.tokens) else {
                continue;
            };
            if !MUTATING_SUBCOMMANDS.contains(&subcmd.as_str()) {
                continue;
            }
            self.diagnostics.push(Diagnostic::new(
                &DIRECT_SYSTEMCTL_METADATA,
                Severity::Warn,
                format!(
                    "scriptlet calls `systemctl {subcmd}` directly; switch to the distro's \
                     `%systemd_*` / `%service_*` macro family"
                ),
                use_.location.section_span(),
            ));
        }
    }
}

fn first_subcommand(tokens: &[crate::shell::ShellToken]) -> Option<String> {
    for tok in tokens.iter().skip(1) {
        let lit = tok.literal_str()?;
        if lit.starts_with('-') {
            continue;
        }
        return Some(lit);
    }
    None
}

impl Lint for DirectSystemctlInScriptlet {
    fn metadata(&self) -> &'static LintMetadata {
        &DIRECT_SYSTEMCTL_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM349 scriptlet-state-outside-rpm-state
// =====================================================================

pub static STATE_OUTSIDE_METADATA: LintMetadata = LintMetadata {
    id: "RPM349",
    name: "scriptlet-state-outside-rpm-state",
    description: "Scriptlet writes scratch state under `/tmp` or `/var/tmp`. Those races with \
                  parallel transactions and leak on abort; use `$RPM_STATE_DIR` or \
                  `/var/lib/rpm-state/<pkg>` instead.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Scriptlet writes scratch state under `/tmp` or `/var/tmp`. Those races with parallel transactions and leak on abort; use `$RPM_STATE_DIR` or `/var/lib/rpm-state/<pkg>` instead.
///
/// See [`STATE_OUTSIDE_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ScriptletStateOutsideRpmState {
    diagnostics: Vec<Diagnostic>,
}

impl ScriptletStateOutsideRpmState {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ScriptletStateOutsideRpmState {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let idx = CommandUseIndex::from_spec(spec);
        for use_ in idx.all() {
            if !matches!(use_.location, SectionRef::Scriptlet { .. }) {
                continue;
            }
            // Only flag write-flavoured commands; `cat /tmp/foo` (a
            // read) is not a packaging problem.
            let Some(name) = use_.name.as_deref() else {
                continue;
            };
            if !WRITE_LIKE_COMMANDS.contains(&name) {
                continue;
            }
            let Some(bad) = first_arg_under_tmp(&use_.tokens) else {
                continue;
            };
            self.diagnostics.push(Diagnostic::new(
                &STATE_OUTSIDE_METADATA,
                Severity::Warn,
                format!(
                    "scriptlet writes scratch state at `{bad}`; use `$RPM_STATE_DIR` or \
                     `/var/lib/rpm-state/<pkg>` for state shared between scriptlet phases"
                ),
                use_.location.section_span(),
            ));
        }
    }
}

const WRITE_LIKE_COMMANDS: &[&str] = &[
    "touch", "cp", "mv", "install", "ln", "mkdir", "tee", "dd", "echo",
];

fn first_arg_under_tmp(tokens: &[crate::shell::ShellToken]) -> Option<String> {
    for tok in tokens.iter().skip(1) {
        let lit = tok.render_verbatim();
        if lit.starts_with("/tmp/") || lit == "/tmp" {
            return Some(lit);
        }
        if lit.starts_with("/var/tmp/") || lit == "/var/tmp" {
            return Some(lit);
        }
    }
    None
}

impl Lint for ScriptletStateOutsideRpmState {
    fn metadata(&self) -> &'static LintMetadata {
        &STATE_OUTSIDE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_342(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = DirectSystemctlInScriptlet::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn run_349(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ScriptletStateOutsideRpmState::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ----- RPM342 -----

    #[test]
    fn rpm342_flags_systemctl_enable() {
        let src = "Name: x\n%post\nsystemctl enable foo.service\nexit 0\n";
        let diags = run_342(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM342");
        assert!(diags[0].message.contains("enable"));
    }

    #[test]
    fn rpm342_flags_systemctl_restart() {
        let src = "Name: x\n%post\nsystemctl restart foo\nexit 0\n";
        assert_eq!(run_342(src).len(), 1);
    }

    #[test]
    fn rpm342_silent_for_systemctl_status() {
        // `status` is a query — no side effect.
        let src = "Name: x\n%post\nsystemctl status foo\nexit 0\n";
        assert!(run_342(src).is_empty());
    }

    #[test]
    fn rpm342_silent_in_install_section() {
        // Same call in `%install` is RPM342-irrelevant (different
        // execution phase). Other rules handle that surface.
        let src = "Name: x\n%install\nsystemctl daemon-reload\n";
        assert!(run_342(src).is_empty());
    }

    #[test]
    fn rpm342_flags_with_flag_argument() {
        let src = "Name: x\n%post\nsystemctl --now enable foo.service\nexit 0\n";
        assert_eq!(run_342(src).len(), 1);
    }

    // ----- RPM349 -----

    #[test]
    fn rpm349_flags_touch_in_tmp() {
        let src = "Name: x\n%post\ntouch /tmp/foo.flag\nexit 0\n";
        let diags = run_349(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM349");
    }

    #[test]
    fn rpm349_flags_mkdir_in_var_tmp() {
        let src = "Name: x\n%pre\nmkdir /var/tmp/scratch\nexit 0\n";
        assert_eq!(run_349(src).len(), 1);
    }

    #[test]
    fn rpm349_silent_for_read_in_tmp() {
        // `cat` isn't a write; no diagnostic.
        let src = "Name: x\n%post\ncat /tmp/seed\nexit 0\n";
        assert!(run_349(src).is_empty());
    }

    #[test]
    fn rpm349_silent_in_install_section() {
        let src = "Name: x\n%install\ntouch /tmp/foo\n";
        assert!(run_349(src).is_empty());
    }

    #[test]
    fn rpm349_silent_for_rpm_state_dir() {
        let src = "Name: x\n%post\ntouch $RPM_STATE_DIR/foo\nexit 0\n";
        assert!(run_349(src).is_empty());
    }
}
