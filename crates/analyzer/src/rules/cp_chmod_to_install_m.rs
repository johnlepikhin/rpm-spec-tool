//! RPM533 `cp-chmod-to-install-m` — flag `cp src dst` immediately
//! followed by `chmod MODE dst` (or `dst/file`) — fold into a single
//! `install -m MODE src dst`.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::{for_each_shell_body, render_shell_line};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM533",
    name: "cp-chmod-to-install-m",
    description: "`cp src dst` + `chmod MODE dst` pair — fold into `install -m MODE src dst`.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `cp src dst` + `chmod MODE dst` pair — fold into `install -m MODE src dst`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct CpChmodToInstallM {
    diagnostics: Vec<Diagnostic>,
}

impl CpChmodToInstallM {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for CpChmodToInstallM {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut hits: Vec<Span> = Vec::new();
        for_each_shell_body(spec, |body, anchor| {
            let mut prev_cp_dst: Option<String> = None;
            for line in &body.lines {
                let raw = render_shell_line(line);
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some(prev_dst) = &prev_cp_dst
                    && let Some(chmod_target) = extract_chmod_target(trimmed)
                    && chmod_targets_cp_result(prev_dst, &chmod_target)
                {
                    hits.push(anchor);
                }
                prev_cp_dst = extract_cp_dst(trimmed);
            }
        });
        for anchor in hits {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "`cp` + `chmod` pair — collapse into a single `install -m MODE src dst`",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "replace the `cp src dst` / `chmod MODE dst` lines with one \
                     `install -m MODE src dst`",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// `cp [-p] SRC DST` — returns DST. Skips when SRC has glob chars.
fn extract_cp_dst(line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let cmd = words.next()?;
    if cmd != "cp" && cmd != "/bin/cp" && cmd != "/usr/bin/cp" {
        return None;
    }
    let tokens: Vec<&str> = words.collect();
    if tokens.len() < 2 {
        return None;
    }
    let first_non_flag = tokens.iter().position(|t| !t.starts_with('-'))?;
    let payload = &tokens[first_non_flag..];
    if payload.len() != 2 {
        // Multi-source cp doesn't trivially translate to install -m.
        return None;
    }
    if payload[0].contains('*') {
        return None;
    }
    Some(payload[1].to_string())
}

/// `chmod MODE TARGET` — returns TARGET. Skips chmod with multiple
/// targets.
fn extract_chmod_target(line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let cmd = words.next()?;
    if cmd != "chmod" && cmd != "/bin/chmod" && cmd != "/usr/bin/chmod" {
        return None;
    }
    let _mode = words.next()?;
    let target = words.next()?;
    if words.next().is_some() {
        return None;
    }
    Some(target.to_owned())
}

fn chmod_targets_cp_result(cp_dst: &str, chmod_target: &str) -> bool {
    if cp_dst == chmod_target {
        return true;
    }
    // `cp src DST` may copy into a directory; chmod might target a
    // file inside it. Accept that pattern when chmod_target starts
    // with cp_dst + "/".
    chmod_target.starts_with(&format!("{cp_dst}/"))
}

impl Lint for CpChmodToInstallM {
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
        run_lint::<CpChmodToInstallM>(src)
    }

    #[test]
    fn flags_cp_chmod_pair() {
        let src = "Name: x\n%install\ncp src dst\nchmod 0755 dst\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM533");
    }

    #[test]
    fn flags_cp_dir_chmod_file() {
        let src = "Name: x\n%install\ncp script bindir\nchmod 0755 bindir/script\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_when_chmod_target_unrelated() {
        let src = "Name: x\n%install\ncp src dst\nchmod 0755 other\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_lone_cp() {
        let src = "Name: x\n%install\ncp src dst\n";
        assert!(run(src).is_empty());
    }
}
