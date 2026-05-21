//! AST walkers for shell-bearing sections.
//!
//! Centralises the `SpecItem::Section` / `SpecItem::Conditional`
//! recursion so each Phase 19+ rule doesn't re-implement it. The
//! `MAX_DEPTH` guard mirrors the one in `files::walk` — defends
//! against adversarial `%if` nesting on untrusted input.

use rpm_spec::ast::{BuildScriptKind, Scriptlet, Section, ShellBody, Span, SpecFile, SpecItem};

const MAX_DEPTH: u32 = 128;

/// Run `f` on every `ShellBody` in `spec` — build-script sections
/// (`%prep`/`%build`/`%install`/`%check`/`%clean`/`%conf`/
/// `%generate_buildrequires`), scriptlets, triggers, file triggers,
/// `%verify`, and `%sepolicy`. The callback receives a "where did this
/// body come from?" tag so rules can filter.
pub fn for_each_shell_body<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(BodyLocation, &'ast ShellBody<Span>),
{
    walk_items(&spec.items, &mut f, 0);
}

/// Run `f` on every `Section::BuildScript` body. Convenience over
/// [`for_each_shell_body`] for the common case where only build
/// scripts matter.
pub fn for_each_buildscript<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(BuildScriptKind, &'ast ShellBody<Span>, Span),
{
    for_each_shell_body(spec, |loc, body| {
        if let BodyLocation::BuildScript { kind, span } = loc {
            f(kind, body, span);
        }
    });
}

/// Run `f` on every `Scriptlet`. Convenience over
/// [`for_each_shell_body`].
pub fn for_each_scriptlet<'ast, F>(spec: &'ast SpecFile<Span>, mut f: F)
where
    F: FnMut(&'ast Scriptlet<Span>),
{
    walk_scriptlets(&spec.items, &mut f, 0);
}

/// "Where this shell body lives in the spec." Carried alongside each
/// body so rules emit diagnostics with a useful anchor span.
#[derive(Debug, Clone, Copy)]
pub enum BodyLocation {
    /// `%prep` / `%build` / `%install` / `%check` / `%clean` / `%conf`
    /// / `%generate_buildrequires`.
    BuildScript { kind: BuildScriptKind, span: Span },
    /// `%pre` / `%post` / `%preun` / `%postun` / `%pretrans` /
    /// `%posttrans` / `%preuntrans` / `%postuntrans`.
    Scriptlet {
        kind: rpm_spec::ast::ScriptletKind,
        span: Span,
    },
    /// `%trigger*` body. `kind` is kept for future trigger-aware
    /// rules even though no current consumer reads it.
    Trigger {
        #[expect(
            dead_code,
            reason = "kept for future trigger-aware rules; no current consumer reads this field"
        )]
        kind: rpm_spec::ast::TriggerKind,
        span: Span,
    },
    /// `%filetrigger*` body. See note on [`Self::Trigger::kind`].
    FileTrigger {
        #[expect(
            dead_code,
            reason = "kept for future trigger-aware rules; no current consumer reads this field"
        )]
        kind: rpm_spec::ast::FileTriggerKind,
        span: Span,
    },
    /// `%verify` body.
    Verify { span: Span },
    /// `%sepolicy` body.
    Sepolicy { span: Span },
}

fn walk_items<'ast, F>(items: &'ast [SpecItem<Span>], f: &mut F, depth: u32)
where
    F: FnMut(BodyLocation, &'ast ShellBody<Span>),
{
    if depth >= MAX_DEPTH {
        return;
    }
    for item in items {
        match item {
            SpecItem::Section(boxed) => visit_section(boxed.as_ref(), f),
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_items(&branch.body, f, depth + 1);
                }
                if let Some(els) = &c.otherwise {
                    walk_items(els, f, depth + 1);
                }
            }
            _ => {}
        }
    }
}

fn visit_section<'ast, F>(section: &'ast Section<Span>, f: &mut F)
where
    F: FnMut(BodyLocation, &'ast ShellBody<Span>),
{
    match section {
        Section::BuildScript { kind, body, data } => {
            f(
                BodyLocation::BuildScript {
                    kind: *kind,
                    span: *data,
                },
                body,
            );
        }
        Section::Scriptlet(s) => {
            f(
                BodyLocation::Scriptlet {
                    kind: s.kind,
                    span: s.data,
                },
                &s.body,
            );
        }
        Section::Trigger(t) => {
            f(
                BodyLocation::Trigger {
                    kind: t.kind,
                    span: t.data,
                },
                &t.body,
            );
        }
        Section::FileTrigger(t) => {
            f(
                BodyLocation::FileTrigger {
                    kind: t.kind,
                    span: t.data,
                },
                &t.body,
            );
        }
        Section::Verify { body, data, .. } => {
            f(BodyLocation::Verify { span: *data }, body);
        }
        Section::Sepolicy { body, data, .. } => {
            f(BodyLocation::Sepolicy { span: *data }, body);
        }
        _ => {}
    }
}

fn walk_scriptlets<'ast, F>(items: &'ast [SpecItem<Span>], f: &mut F, depth: u32)
where
    F: FnMut(&'ast Scriptlet<Span>),
{
    if depth >= MAX_DEPTH {
        return;
    }
    for item in items {
        match item {
            #[allow(clippy::collapsible_match)]
            SpecItem::Section(boxed) => {
                if let Section::Scriptlet(s) = boxed.as_ref() {
                    f(s);
                }
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    walk_scriptlets(&branch.body, f, depth + 1);
                }
                if let Some(els) = &c.otherwise {
                    walk_scriptlets(els, f, depth + 1);
                }
            }
            _ => {}
        }
    }
}
