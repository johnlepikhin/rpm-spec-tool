//! `textDocument/completion` — context-aware suggestions.
//!
//! Two completion contexts are supported:
//!
//! * **Directive context** — the user typed `%` at the start of a
//!   line (optionally followed by some letters). Suggest every entry
//!   in [`crate::hover::DIRECTIVES`]: `%prep`, `%build`, `%install`,
//!   scriptlets, conditionals, macros.
//!
//! * **Tag context** — the cursor sits at the start of a line outside
//!   any directive, with no `:` yet. Suggest every entry in
//!   [`crate::hover::TAGS`]: `Name:`, `Version:`, `BuildRequires:`, ...
//!
//! Anything else (inside a section body, after a `:`, mid-value) is
//! left unfilled. The active profile (when known) seeds the directive
//! context with every macro it carries, so users see `%_libdir`,
//! `%dist`, distribution-specific macros, etc. alongside the
//! hard-coded RPM directives.

use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, InsertTextFormat, MarkupContent, MarkupKind,
    Position,
};
use rpm_spec_profile::Profile;

use crate::hover::{DIRECTIVES, TAGS};

/// Compute the completion list for `source` at `pos`. Returns an empty
/// vector when the cursor isn't in a recognised context.
///
/// `profile` carries the active distribution profile (when one was
/// resolved); its macros are mixed into the directive completion list.
/// Pass `None` before the first analysis pass — completion still works,
/// just without profile-specific macros.
pub fn complete(source: &str, pos: Position, profile: Option<&Profile>) -> Vec<CompletionItem> {
    let Some(line) = source.lines().nth(pos.line as usize) else {
        return Vec::new();
    };
    let col = (pos.character as usize).min(line.len());
    let prefix = &line[..col];

    match classify(prefix) {
        Context::Directive { typed } => {
            let mut out = directive_items(typed);
            if let Some(p) = profile {
                out.extend(profile_macro_items(typed, p));
            }
            out
        }
        Context::Tag { typed } => tag_items(typed),
        Context::None => Vec::new(),
    }
}

/// Build completion entries for every macro in the profile whose name
/// starts with `typed`. Each item carries the macro's expansion as
/// `detail` (so the editor's right column reveals the value at a
/// glance) and provenance as `documentation`.
fn profile_macro_items(typed: &str, profile: &Profile) -> Vec<CompletionItem> {
    profile
        .macros
        .entries
        .iter()
        .filter(|(name, _)| name.starts_with(typed))
        // Skip names already present in the hard-coded DIRECTIVES list
        // — those are keywords with their own dedicated hover docs and
        // we don't want duplicates in the completion popup.
        .filter(|(name, _)| !DIRECTIVES.iter().any(|(d, _)| d == name))
        .map(|(name, entry)| {
            let expansion = profile.macros.expand_to_literal(name, 4);
            let detail = expansion.as_deref().map(|s| truncate(s, 60));
            CompletionItem {
                label: format!("%{name}"),
                kind: Some(CompletionItemKind::CONSTANT),
                insert_text: Some(name.clone()),
                insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
                detail,
                documentation: Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!(
                        "**`%{name}`** — from profile (provenance: `{:?}`).",
                        entry.provenance
                    ),
                })),
                ..Default::default()
            }
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[derive(Debug)]
enum Context<'a> {
    /// User typed `%` and (maybe) some characters at the start of a
    /// logical line. `typed` is the substring after the `%` (lowercase
    /// alpha), used to filter the list.
    Directive {
        typed: &'a str,
    },
    /// Cursor at the start of a line, no `:` yet, no `%`. `typed` is
    /// the partial tag name typed so far.
    Tag {
        typed: &'a str,
    },
    None,
}

fn classify(prefix: &str) -> Context<'_> {
    // We only fire completion at the very *start* of a logical line:
    // either the whole prefix is whitespace + `%foo`, or whitespace + alphanum.
    // Anything containing `:`, `(`, quote chars, etc. is treated as
    // mid-value — not our concern.
    let trimmed = prefix.trim_start();
    let leading_ws = prefix.len() - trimmed.len();
    if trimmed.contains([':', '"', '\'', '(', ')']) {
        return Context::None;
    }

    if let Some(rest) = trimmed.strip_prefix('%') {
        if rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Context::Directive { typed: rest };
        }
        return Context::None;
    }

    // Tag context: must be an alpha/digit run at the top of a line.
    // Reject if there's any non-ident character in the trimmed prefix.
    if trimmed.is_empty() {
        // Empty line — surface tag completions so first-time users
        // discover `Name:`/`Version:`/... by hitting Ctrl-Space.
        return Context::Tag { typed: "" };
    }
    if trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Context::Tag { typed: trimmed };
    }
    // Belt-and-braces: a line that begins with whitespace and a
    // non-ident character (e.g. `  - bullet`) isn't a tag context.
    let _ = leading_ws;
    Context::None
}

fn directive_items(typed: &str) -> Vec<CompletionItem> {
    let typed_lc = typed.to_ascii_lowercase();
    DIRECTIVES
        .iter()
        .filter(|(name, _)| name.starts_with(&typed_lc))
        .map(|(name, doc)| CompletionItem {
            label: format!("%{name}"),
            kind: Some(CompletionItemKind::KEYWORD),
            // Editor already shows `%` from the user's keystroke, so
            // the inserted text omits it.
            insert_text: Some((*name).to_string()),
            insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
            detail: Some(short_detail(doc)),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: doc.to_string(),
            })),
            ..Default::default()
        })
        .collect()
}

fn tag_items(typed: &str) -> Vec<CompletionItem> {
    let typed_lc = typed.to_ascii_lowercase();
    TAGS.iter()
        .filter(|(name, _)| name.to_ascii_lowercase().starts_with(&typed_lc))
        .map(|(name, doc)| CompletionItem {
            label: format!("{name}:"),
            kind: Some(CompletionItemKind::FIELD),
            insert_text: Some(format!("{name}: ")),
            insert_text_format: Some(InsertTextFormat::PLAIN_TEXT),
            detail: Some(short_detail(doc)),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: doc.to_string(),
            })),
            ..Default::default()
        })
        .collect()
}

/// First sentence (or first 80 chars) of a doc string, plain-text-ish.
/// Used as the `detail` field — what the client shows on the right of
/// the completion entry. Markdown markers are kept for now; editors
/// usually display them as-is.
fn short_detail(doc: &str) -> String {
    let stripped = doc.trim_start_matches('*').trim_start();
    // Cut at the first em-dash or period that ends a sentence; fall
    // back to a 80-char window.
    if let Some(idx) = stripped.find('—') {
        stripped[idx + '—'.len_utf8()..].trim().to_string()
    } else {
        stripped.chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, ch: u32) -> Position {
        Position::new(line, ch)
    }

    #[test]
    fn directive_completion_after_percent() {
        let src = "%pre\nset -x\n";
        let items = complete(src, pos(0, 4), None);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"%prep"), "got {labels:?}");
        // Should also keep `%pre`/`%pretrans` because filtering is by
        // prefix and the user typed `pre`.
        assert!(labels.contains(&"%pre"), "got {labels:?}");
        assert!(labels.contains(&"%pretrans"), "got {labels:?}");
        // Unrelated tags must not leak in.
        assert!(
            !labels.iter().any(|l| !l.starts_with('%')),
            "got {labels:?}"
        );
    }

    #[test]
    fn tag_completion_at_start_of_line() {
        let src = "Buil\n";
        let items = complete(src, pos(0, 4), None);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"BuildRequires:"), "got {labels:?}");
        assert!(labels.contains(&"BuildArch:"), "got {labels:?}");
        // Nothing starting with `%`.
        assert!(items.iter().all(|i| !i.label.starts_with('%')));
    }

    #[test]
    fn empty_line_offers_full_tag_list() {
        let src = "Name: hello\n\n";
        let items = complete(src, pos(1, 0), None);
        // Whole tag table should appear (29 entries at the moment of
        // writing — assert lower bound to stay robust as we add tags).
        assert!(items.len() >= 20, "got {} items", items.len());
        assert!(items.iter().any(|i| i.label == "Name:"));
    }

    #[test]
    fn mid_value_returns_nothing() {
        let src = "License: GPLv2\n";
        let items = complete(src, pos(0, 12), None);
        assert!(items.is_empty(), "expected no completions inside a value");
    }

    #[test]
    fn line_starting_with_dash_is_not_a_tag_context() {
        // Hits inside a `%description` body or a `%changelog` entry —
        // those typically start with `-` or `*` and should NOT trigger
        // tag completion.
        let src = "- this is a bullet\n";
        let items = complete(src, pos(0, 2), None);
        assert!(
            items.is_empty(),
            "got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn directive_filter_typing_is_prefix_based() {
        // After `%inst` we should see `%install` only.
        let src = "%inst\n";
        let items = complete(src, pos(0, 5), None);
        let labels: Vec<_> = items.iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, vec!["%install"], "got {labels:?}");
    }
}
