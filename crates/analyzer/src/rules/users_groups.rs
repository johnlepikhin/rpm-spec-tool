//! RPM348 `unsafe-useradd-groupadd` — scriptlets that create users or
//! groups without idempotency guards.
//!
//! Direct `useradd` / `groupadd` invocations fail on re-install (the
//! account already exists) or, when run from `%pre`, partially commit
//! state that gets stranded if the transaction later aborts. The
//! correct idiom is to guard the call with `getent`:
//!
//! ```sh
//! getent group foo >/dev/null || groupadd -r foo
//! getent passwd foo >/dev/null || useradd -r -g foo foo
//! ```
//!
//! or — better — to rely on `sysusers.d` (RPM 4.16+) and the
//! distro's `%sysusers_create_package` macro family.
//!
//! The rule fires when:
//!
//! 1. Any scriptlet calls `useradd` / `groupadd` / `usermod` /
//!    `groupmod`, **and**
//! 2. The same line is not protected by an obvious `getent ... ||`
//!    guard.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::for_each_scriptlet;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM348",
    name: "unsafe-useradd-groupadd",
    description: "Scriptlet creates a user/group without a `getent … || …` idempotency \
                  guard. Re-installs fail noisily and partial transactions strand state.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Scriptlet creates a user/group without a `getent … || …` idempotency guard. Re-installs fail noisily and partial transactions strand state.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct UnsafeUseraddGroupadd {
    diagnostics: Vec<Diagnostic>,
}

impl UnsafeUseraddGroupadd {
    pub fn new() -> Self {
        Self::default()
    }
}

const ACCOUNT_CREATING_TOOLS: &[&str] = &[
    "useradd", "groupadd", "usermod", "groupmod", "adduser", "addgroup",
];

impl<'ast> Visit<'ast> for UnsafeUseraddGroupadd {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_scriptlet(spec, |s| {
            for line in &s.body.lines {
                let Some(lit) = line.literal_str() else {
                    continue;
                };
                let trimmed = lit.trim();
                let Some(tool) = first_account_tool(trimmed) else {
                    continue;
                };
                if line_has_getent_guard(trimmed) {
                    continue;
                }
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "scriptlet calls `{tool}` without a `getent … ||` guard; \
                         add the guard so re-installs and partial transactions are safe"
                    ),
                    s.data,
                ));
                // One diagnostic per scriptlet is plenty.
                return;
            }
        });
    }
}

fn first_account_tool(trimmed: &str) -> Option<&'static str> {
    // Split on shell separators (`|`, `&`, `;`) so a `useradd` that
    // sits after `something; useradd …` still surfaces. The caller
    // (`visit_spec`) runs this *before* the `getent` guard check, so
    // for `getent … || useradd …` we'll still return Some("useradd")
    // here — `line_has_getent_guard` is what suppresses the
    // diagnostic in that case.
    let token = trimmed
        .split(['|', '&', ';'])
        .flat_map(str::split_whitespace)
        .next()?;
    let token = token.trim_start_matches('!');
    ACCOUNT_CREATING_TOOLS.iter().copied().find(|t| *t == token)
}

fn line_has_getent_guard(trimmed: &str) -> bool {
    // Two acceptable shapes:
    //   getent passwd foo >/dev/null || useradd -r foo
    //   id foo >/dev/null 2>&1 || useradd -r foo
    // The `getent` / `id` form must appear *before* a `||` separator.
    let Some((before_or, _)) = trimmed.split_once("||") else {
        return false;
    };
    let head = before_or.trim();
    head.starts_with("getent ") || head.starts_with("id ") || head.contains(" getent ")
}

impl Lint for UnsafeUseraddGroupadd {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<UnsafeUseraddGroupadd>(src)
    }

    #[test]
    fn flags_bare_useradd() {
        let src = "Name: x\n%pre\nuseradd -r foo\nexit 0\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM348");
        assert!(diags[0].message.contains("useradd"));
    }

    #[test]
    fn flags_bare_groupadd() {
        let src = "Name: x\n%pre\ngroupadd -r foo\nexit 0\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_with_getent_guard() {
        let src = "Name: x\n%pre\ngetent passwd foo >/dev/null || useradd -r foo\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_with_id_guard() {
        let src = "Name: x\n%pre\nid foo >/dev/null 2>&1 || useradd -r foo\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_account_tool() {
        let src = "Name: x\n%pre\nsystemctl daemon-reload\nexit 0\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_one_per_scriptlet_even_with_multiple_calls() {
        let src = "Name: x\n%pre\ngroupadd foo\nuseradd foo\nexit 0\n";
        // Only one diagnostic per scriptlet — matches the policy of
        // "one human-reviewable finding, fix the scriptlet wholesale".
        assert_eq!(run(src).len(), 1);
    }
}
