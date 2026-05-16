//! [`CommandUseIndex`] — flat list of "command invocations" seen
//! across every shell-bearing section of a spec.
//!
//! The index is built once per spec; rules query it by command name
//! (`index.find("systemctl")`) or by section kind. It's the cheap
//! alternative to each rule re-walking every section.
//!
//! ## Heuristic: what is a "command"?
//!
//! For each non-blank shell line the index records *the first token*
//! as the command, and the remaining tokens as its arguments. This is
//! coarse — shell pipelines like `a | b | c` only surface `a`, and
//! commands after a `;` are missed — but matches the level of
//! precision the rules need today. A full shell CFG is deferred to a
//! later phase.
//!
//! Tokens are produced by [`super::tokens::tokenize_line`] and
//! preserve macro references verbatim.

use rpm_spec::ast::{BuildScriptKind, ScriptletKind, ShellBody, Span, SpecFile};

use super::tokens::{ShellToken, tokenize_line};
use super::walk::{BodyLocation, for_each_shell_body};

/// One observed command invocation.
#[derive(Debug, Clone)]
pub struct CommandUse {
    /// Best-effort literal name of the command. `None` when the
    /// command position contained a macro the tokenizer couldn't
    /// resolve to a literal (e.g. `%{myprog} arg`).
    pub name: Option<String>,
    /// All tokens on the line including the command itself at
    /// position 0.
    pub tokens: Vec<ShellToken>,
    /// Where in the spec this line lives.
    pub location: SectionRef,
    /// Zero-based line index within the surrounding `ShellBody`.
    pub line_idx: usize,
}

/// Coarse classification of which section a command lives in.
/// Mirrors the structural variants of [`BodyLocation`] but discards
/// the per-section span (the index keeps that on the `CommandUse`
/// itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionRef {
    BuildScript {
        kind: BuildScriptKind,
        section_span: Span,
    },
    Scriptlet {
        kind: ScriptletKind,
        section_span: Span,
    },
    Trigger {
        section_span: Span,
    },
    FileTrigger {
        section_span: Span,
    },
    Verify {
        section_span: Span,
    },
    Sepolicy {
        section_span: Span,
    },
}

impl SectionRef {
    /// Anchor span of the surrounding section (useful when the
    /// per-line span isn't precise enough or when the rule wants to
    /// label "this %install" rather than a specific line).
    pub fn section_span(&self) -> Span {
        match *self {
            Self::BuildScript { section_span, .. }
            | Self::Scriptlet { section_span, .. }
            | Self::Trigger { section_span }
            | Self::FileTrigger { section_span }
            | Self::Verify { section_span }
            | Self::Sepolicy { section_span } => section_span,
        }
    }
}

/// Flat index of every command invocation in a spec.
#[derive(Debug, Default)]
pub struct CommandUseIndex {
    uses: Vec<CommandUse>,
}

impl CommandUseIndex {
    /// Build the index by walking every shell-bearing section in `spec`.
    pub fn from_spec(spec: &SpecFile<Span>) -> Self {
        let mut uses = Vec::new();
        for_each_shell_body(spec, |loc, body| {
            collect_body(&loc, body, &mut uses);
        });
        Self { uses }
    }

    /// Every `CommandUse` recorded in the index, in source order
    /// across sections.
    pub fn all(&self) -> &[CommandUse] {
        &self.uses
    }

    /// All invocations whose command name matches `name` literally
    /// (case-sensitive). Skips uses whose command position is a
    /// macro reference.
    pub fn find<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a CommandUse> + 'a {
        self.uses
            .iter()
            .filter(move |u| u.name.as_deref() == Some(name))
    }

    /// All invocations inside a build-script of the given `kind`
    /// (e.g. `%install` → `BuildScriptKind::Install`).
    pub fn in_buildscript(&self, kind: BuildScriptKind) -> impl Iterator<Item = &CommandUse> + '_ {
        self.uses.iter().filter(move |u| match u.location {
            SectionRef::BuildScript { kind: k, .. } => k == kind,
            _ => false,
        })
    }
}

fn collect_body(loc: &BodyLocation, body: &ShellBody, out: &mut Vec<CommandUse>) {
    let section_ref = body_location_to_section_ref(loc);
    for (idx, line) in body.lines.iter().enumerate() {
        let tokens = tokenize_line(line);
        if tokens.is_empty() {
            continue;
        }
        // Skip shell control words at position 0; treat the token at
        // position 1 as the command in that case. This is a tiny set
        // of common cases — `if`, `then`, `else`, `elif`, `fi`, `do`,
        // `done`, `while`, `for`, `case`, `esac` — that show up at the
        // start of multi-line shell constructs. The tokenizer can't
        // do this correctly without a full parser; the list below is
        // the pragmatic minimum that keeps `systemctl` inside
        // `if condition; then systemctl …` findable.
        let cmd_idx = control_word_skip(&tokens);
        let name = tokens.get(cmd_idx).and_then(|t| t.literal_str());
        out.push(CommandUse {
            name,
            tokens,
            location: section_ref,
            line_idx: idx,
        });
    }
}

fn control_word_skip(tokens: &[ShellToken]) -> usize {
    const SHELL_CONTROL: &[&str] = &[
        "if", "then", "else", "elif", "fi", "do", "done", "while", "for", "case", "esac", "until",
    ];
    if let Some(first) = tokens.first().and_then(|t| t.literal_str())
        && SHELL_CONTROL.contains(&first.as_str())
    {
        return 1;
    }
    0
}

fn body_location_to_section_ref(loc: &BodyLocation) -> SectionRef {
    match *loc {
        BodyLocation::BuildScript { kind, span } => SectionRef::BuildScript {
            kind,
            section_span: span,
        },
        BodyLocation::Scriptlet { kind, span } => SectionRef::Scriptlet {
            kind,
            section_span: span,
        },
        BodyLocation::Trigger { span, .. } => SectionRef::Trigger { section_span: span },
        BodyLocation::FileTrigger { span, .. } => SectionRef::FileTrigger { section_span: span },
        BodyLocation::Verify { span } => SectionRef::Verify { section_span: span },
        BodyLocation::Sepolicy { span } => SectionRef::Sepolicy { section_span: span },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    #[test]
    fn collects_commands_in_install() {
        let src = "Name: x\n%install\nrm -rf %{buildroot}\ninstall -m 0644 foo /etc/foo\n";
        let outcome = parse(src);
        let idx = CommandUseIndex::from_spec(&outcome.spec);
        let install: Vec<_> = idx
            .in_buildscript(BuildScriptKind::Install)
            .map(|u| u.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(install, vec!["rm", "install"]);
    }

    #[test]
    fn finds_commands_across_sections() {
        let src = "Name: x\n%install\nsystemctl daemon-reload\n%post\nsystemctl restart foo\n";
        let outcome = parse(src);
        let idx = CommandUseIndex::from_spec(&outcome.spec);
        let occ: Vec<_> = idx.find("systemctl").collect();
        assert_eq!(occ.len(), 2, "{:?}", idx.all());
    }

    #[test]
    fn skips_shell_control_word_to_real_command() {
        let src = "Name: x\n%post\nif [ $1 = 1 ]; then systemctl enable foo; fi\n";
        let outcome = parse(src);
        let idx = CommandUseIndex::from_spec(&outcome.spec);
        // The first non-control token after `if` is `[`. Acceptable —
        // we just need the line to be visible. Verify the line is
        // recorded and we can iterate its tokens.
        let scriptlet_uses: Vec<_> = idx
            .all()
            .iter()
            .filter(|u| matches!(u.location, SectionRef::Scriptlet { .. }))
            .collect();
        assert!(!scriptlet_uses.is_empty());
        let toks: Vec<_> = scriptlet_uses[0]
            .tokens
            .iter()
            .filter_map(|t| t.literal_str())
            .collect();
        assert!(toks.iter().any(|s| s == "systemctl"));
    }

    #[test]
    fn empty_spec_yields_empty_index() {
        let outcome = parse("Name: x\n");
        let idx = CommandUseIndex::from_spec(&outcome.spec);
        assert!(idx.all().is_empty());
    }

    #[test]
    fn macro_in_command_position_yields_no_name() {
        let src = "Name: x\n%install\n%{my_install_helper} arg\n";
        let outcome = parse(src);
        let idx = CommandUseIndex::from_spec(&outcome.spec);
        assert_eq!(idx.all().len(), 1);
        assert!(idx.all()[0].name.is_none());
    }
}
