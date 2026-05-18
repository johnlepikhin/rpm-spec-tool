//! Shared shell-body walker for prep/build/install/scriptlet/trigger
//! rules.
//!
//! Each Phase-25 shell rule walks the same set of sections and renders
//! lines to plain text the same way. Centralising the boilerplate here
//! keeps the rule modules focused on their pattern matching.

use rpm_spec::ast::{
    FileTrigger, Scriptlet, Section, ShellBody, Span, SpecFile, SpecItem, Text, TextSegment,
    Trigger,
};

/// Run `f` on every shell body in the spec, passing the body and a
/// span suitable for diagnostic anchoring.
pub(crate) fn for_each_shell_body<'a, F>(spec: &'a SpecFile<Span>, mut f: F)
where
    F: FnMut(&'a ShellBody, Span),
{
    for it in &spec.items {
        let SpecItem::Section(boxed) = it else {
            continue;
        };
        match boxed.as_ref() {
            Section::BuildScript { body, data, .. } => f(body, *data),
            Section::Verify { body, data, .. } => f(body, *data),
            Section::Scriptlet(Scriptlet { body, data, .. }) => f(body, *data),
            Section::Trigger(Trigger { body, data, .. }) => f(body, *data),
            Section::FileTrigger(FileTrigger { body, data, .. }) => f(body, *data),
            Section::Sepolicy { body, data, .. } => f(body, *data),
            _ => {}
        }
    }
}

/// Render a parsed shell line back to a plain-text approximation:
/// literals as-is, macro refs as `%name` / `%{name}`. Unknown segment
/// kinds are silently skipped (forward-compat with `TextSegment`'s
/// non-exhaustive tail). See [`render_shell_line`]'s wildcard arm for
/// the `MacroKind` rendering contract.
pub(crate) fn render_shell_line(line: &Text) -> String {
    use rpm_spec::ast::MacroKind;
    let mut out = String::new();
    for seg in &line.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => match m.kind {
                MacroKind::Braced => out.push_str(&format!("%{{{}}}", m.name)),
                MacroKind::Plain => out.push_str(&format!("%{}", m.name)),
                // `MacroKind` is `#[non_exhaustive]`. Render unknown
                // future variants in braced form `%{name}` — braces
                // delimit the macro unambiguously in all contexts and
                // are strictly safer for downstream string matching
                // than the plain `%name` form (which can swallow
                // following identifier chars).
                _ => out.push_str(&format!("%{{{}}}", m.name)),
            },
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn count_bodies(src: &str) -> usize {
        let outcome = parse(src);
        let mut n = 0usize;
        for_each_shell_body(&outcome.spec, |_body, _anchor| {
            n += 1;
        });
        n
    }

    #[test]
    fn for_each_shell_body_visits_sepolicy_section() {
        // %sepolicy has a shell body — must be enumerated alongside
        // %prep/%build/%post/etc. so rules that walk shell sections
        // don't silently skip SELinux module bodies.
        let src = "Name: x\n%sepolicy\necho selinux\n";
        assert!(count_bodies(src) >= 1, "%sepolicy body must be visited");
    }

    #[test]
    fn for_each_shell_body_visits_file_trigger_section() {
        // %filetriggerin (and siblings) carry shell bodies executed by
        // RPM transactions. The walker must include them so triggers
        // are subject to the same shell-rule audit as scriptlets.
        let src = "Name: x\n%filetriggerin -- /usr/lib\necho hi\n";
        assert!(
            count_bodies(src) >= 1,
            "%filetriggerin body must be visited"
        );
    }
}
