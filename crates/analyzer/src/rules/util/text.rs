//! Text-rendering, SPDX tokenisation, path / shell-name byte
//! classifiers, fallback path table.
//!
//! These are scalar string utilities shared across rules. They have
//! no AST dependency beyond `Text` (for `render_text_with_macros` /
//! `literal_archs`).

use std::collections::BTreeSet;

use rpm_spec::ast::Text;

/// Default mapping from a literal hardcoded path prefix to the RPM macro
/// that should replace it. Used by [`crate::rules::hardcoded_paths`]
/// when no profile has been applied (or when the profile doesn't
/// override the macro's value). Distribution profiles can replace these
/// entries via `HardcodedPaths::set_profile`, which reads the actual
/// `_bindir` / `_libdir` / etc. from `profile.macros`.
///
/// **Order matters.** The table is scanned top-down; more-specific
/// prefixes (`/usr/lib64`, `/var/log`) must precede their less-specific
/// peers (`/usr/lib`, `/var/lib`) so that a literal like `/usr/lib64/foo`
/// matches the right replacement.
///
/// `pub(crate)` because this is an analyzer-internal default — downstream
/// consumers should read the actual path table off a resolved `Profile`,
/// not the fallback constants.
pub(crate) const FALLBACK_PATH_TABLE: &[(&str, &str)] = &[
    ("/usr/lib64", "%{_libdir}"),
    ("/usr/libexec", "%{_libexecdir}"),
    ("/usr/include", "%{_includedir}"),
    ("/usr/share", "%{_datadir}"),
    ("/usr/bin", "%{_bindir}"),
    ("/usr/sbin", "%{_sbindir}"),
    ("/usr/lib", "%{_libdir}"),
    ("/var/log", "%{_localstatedir}/log"),
    ("/var/lib", "%{_sharedstatedir}"),
    ("/etc", "%{_sysconfdir}"),
];

/// Split an SPDX license expression on the `OR` / `AND` / `WITH`
/// keywords (case-insensitive, word-boundary-respecting) and return
/// the trimmed atoms. Surrounding parentheses and commas are stripped.
///
/// Used by both RPM024 (`invalid-license`) and RPM127
/// (`legacy-license-syntax`). Kept here as the single source of truth
/// so a future fix (e.g. `WITH` exception identifier handling) applies
/// uniformly.
///
/// Implementation: tokenise on whitespace + paren/comma into raw
/// tokens, then drop the operator tokens. SPDX identifiers themselves
/// cannot contain whitespace, so a whitespace split never breaks an
/// atom.
pub(crate) fn split_spdx_atoms(expr: &str) -> Vec<&str> {
    expr.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && !is_spdx_operator(t))
        .collect()
}

/// True when `tok` is one of the SPDX expression operators
/// (`OR`/`AND`/`WITH`), case-insensitive.
pub(crate) fn is_spdx_operator(tok: &str) -> bool {
    tok.eq_ignore_ascii_case("OR")
        || tok.eq_ignore_ascii_case("AND")
        || tok.eq_ignore_ascii_case("WITH")
}

/// `true` when `b` can continue a shell-token / path-name. Used by
/// boundary checks in path-prefix matching ([`is_path_boundary`]) and
/// in command-word matching (e.g. RPM062 `egrep`/`fgrep` detection).
#[inline]
pub(crate) fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// `true` when `rest` (the bytes immediately after a candidate
/// path-prefix) marks the path's end. Name-continuation characters
/// (`a-zA-Z0-9._-`) keep the match going; anything else terminates.
/// Used by [`crate::rules::hardcoded_paths`] to validate matches
/// like `/usr/bin` vs `/usr/binfoo`.
#[inline]
pub(crate) fn is_path_boundary(rest: &str) -> bool {
    match rest.as_bytes().first() {
        None => true,
        Some(b'/') => true,
        Some(&b) => !is_name_byte(b),
    }
}

/// Extract the single target of a `mkdir -p DIR` shell command.
///
/// Recognises `mkdir`, `/bin/mkdir`, and `/usr/bin/mkdir`. Returns
/// `Some(DIR)` only when `-p` is the sole flag and exactly one
/// directory is given; `mkdir -p A B` and any flag-mix beyond `-p`
/// return `None`. Used by RPM530 (`mkdir-install-to-install-d`) and
/// RPM531 (`redundant-mkdir-before-install-d`) for adjacency
/// detection of `mkdir -p` followed by an `install` line.
pub(crate) fn extract_mkdir_p_target(line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let cmd = words.next()?;
    if cmd != "mkdir" && cmd != "/bin/mkdir" && cmd != "/usr/bin/mkdir" {
        return None;
    }
    if words.next()? != "-p" {
        return None;
    }
    let target = words.next()?;
    if words.next().is_some() {
        // mkdir -p A B — multiple targets; the adjacency check above
        // is designed for a single target.
        return None;
    }
    Some(target.to_owned())
}

/// Render a [`Text`] back to its source-text form, preserving macro
/// references as `%name` (`MacroKind::Plain`) or `%{name}` (any other
/// kind, including the catch-all). Literal segments pass through
/// unchanged.
///
/// Used as a stable bucket key / display rendering when the fully-
/// resolved literal value is unavailable because of macros in the
/// middle (e.g. `%{shortname}-foo` for subpackage refs). Two macro-
/// templated texts that share the same surface text collide on the
/// same key.
///
/// Output is NOT trimmed — callers that need a non-empty key should
/// trim and check `is_empty` themselves.
pub(crate) fn render_text_with_macros(text: &Text) -> String {
    use rpm_spec::ast::{MacroKind, TextSegment};
    let mut out = String::new();
    for seg in &text.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => match m.kind {
                MacroKind::Plain => {
                    out.push('%');
                    out.push_str(&m.name);
                }
                _ => {
                    out.push_str("%{");
                    out.push_str(&m.name);
                    out.push('}');
                }
            },
            // `TextSegment` is `#[non_exhaustive]` — unknown variants
            // contribute nothing rather than mis-render to a confusing
            // surface form.
            _ => {}
        }
    }
    out
}

/// Resolve a list of `%ifarch`-style arch [`Text`] entries to a
/// [`BTreeSet`] of literal arch names. Returns `None` when any entry
/// contains a macro (we can't statically pin the arch token) or is
/// empty after trimming.
///
/// Used by every `%ifarch`-reasoning rule (RPM440/RPM441/RPM442/
/// RPM453) — kept here as the single source of truth so the literal-
/// extraction policy is consistent.
pub(crate) fn literal_archs(list: &[Text]) -> Option<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for t in list {
        let s = t.literal_str()?.trim();
        if s.is_empty() {
            return None;
        }
        out.insert(s.to_owned());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_path_boundary_at_end_of_string() {
        assert!(is_path_boundary(""));
    }

    #[test]
    fn is_path_boundary_on_slash_continues_path() {
        assert!(is_path_boundary("/foo"));
    }

    #[test]
    fn is_path_boundary_rejects_name_continuation() {
        // `/usr/bin` followed by `foo` (alpha) is NOT a boundary —
        // the path keeps going as `/usr/binfoo`. Same for digits,
        // `_`, `-`, `.`.
        assert!(!is_path_boundary("foo"));
        assert!(!is_path_boundary("1"));
        assert!(!is_path_boundary("_x"));
        assert!(!is_path_boundary("-x"));
        assert!(!is_path_boundary(".x"));
    }

    #[test]
    fn is_path_boundary_terminator_chars() {
        // Anything that can't continue a path-name terminates: whitespace,
        // shell punctuation, end-of-string.
        assert!(is_path_boundary(" "));
        assert!(is_path_boundary("\t"));
        assert!(is_path_boundary("\""));
        assert!(is_path_boundary("'"));
        assert!(is_path_boundary(":"));
    }

    #[test]
    fn split_spdx_atoms_empty_input() {
        let v = split_spdx_atoms("");
        assert!(v.is_empty(), "empty input → no atoms; got {v:?}");
    }

    #[test]
    fn split_spdx_atoms_single_identifier() {
        assert_eq!(split_spdx_atoms("MIT"), vec!["MIT"]);
    }

    #[test]
    fn split_spdx_atoms_or_and_with() {
        assert_eq!(
            split_spdx_atoms("MIT OR GPL-2.0-or-later WITH Classpath-exception-2.0"),
            vec!["MIT", "GPL-2.0-or-later", "Classpath-exception-2.0"]
        );
    }

    #[test]
    fn split_spdx_atoms_handles_parens_and_commas() {
        // `(MIT OR Apache-2.0), GPL-3.0-or-later` — atoms regardless
        // of grouping syntax.
        let v = split_spdx_atoms("(MIT OR Apache-2.0), GPL-3.0-or-later");
        assert_eq!(v, vec!["MIT", "Apache-2.0", "GPL-3.0-or-later"]);
    }

    #[test]
    fn split_spdx_atoms_only_operators_yields_empty() {
        assert!(split_spdx_atoms("OR AND WITH").is_empty());
    }

    #[test]
    fn is_spdx_operator_case_insensitive() {
        for op in ["OR", "or", "Or", "AND", "and", "WITH", "with", "WitH"] {
            assert!(is_spdx_operator(op), "{op} should match");
        }
    }

    #[test]
    fn is_spdx_operator_rejects_non_operators() {
        for tok in ["MIT", "GPL", "ORIGINAL", "ANDX", "WITHOUT", "", "or-"] {
            assert!(!is_spdx_operator(tok), "{tok} should not match");
        }
    }

    // ---- extract_mkdir_p_target ----

    #[test]
    fn extract_mkdir_p_target_basic() {
        // Nested directory.
        assert_eq!(
            extract_mkdir_p_target("mkdir -p /a/b"),
            Some("/a/b".to_string())
        );
        // Single directory.
        assert_eq!(
            extract_mkdir_p_target("mkdir -p /a"),
            Some("/a".to_string())
        );
        // Absolute mkdir paths.
        assert_eq!(
            extract_mkdir_p_target("/bin/mkdir -p /a"),
            Some("/a".to_string())
        );
        assert_eq!(
            extract_mkdir_p_target("/usr/bin/mkdir -p /a"),
            Some("/a".to_string())
        );
        // Multiple targets — rejected.
        assert!(extract_mkdir_p_target("mkdir -p /a /b").is_none());
        // Missing -p.
        assert!(extract_mkdir_p_target("mkdir /a").is_none());
        // Different command.
        assert!(extract_mkdir_p_target("install -p /a").is_none());
    }

    // ---- render_text_with_macros ----

    #[test]
    fn render_text_with_macros_plain_and_braced() {
        use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef, TextSegment};
        // Compose: literal "foo-" + %name + literal "-" + %{ver}
        let text = Text {
            segments: vec![
                TextSegment::Literal("foo-".into()),
                TextSegment::Macro(Box::new(MacroRef {
                    kind: MacroKind::Plain,
                    name: "name".into(),
                    args: Vec::new(),
                    conditional: ConditionalMacro::None,
                    with_value: None,
                })),
                TextSegment::Literal("-".into()),
                TextSegment::Macro(Box::new(MacroRef {
                    kind: MacroKind::Braced,
                    name: "ver".into(),
                    args: Vec::new(),
                    conditional: ConditionalMacro::None,
                    with_value: None,
                })),
            ],
        };
        assert_eq!(render_text_with_macros(&text), "foo-%name-%{ver}");
    }

    // ---- literal_archs ----

    #[test]
    fn literal_archs_resolves_static_list() {
        let list: Vec<Text> = vec![Text::from("x86_64"), Text::from("aarch64")];
        let set = literal_archs(&list).expect("literal arch list should resolve");
        assert_eq!(set.len(), 2);
        assert!(set.contains("x86_64"));
        assert!(set.contains("aarch64"));
    }

    #[test]
    fn literal_archs_bails_on_macro() {
        use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef, TextSegment};
        // List with one macro-bearing entry → can't pin the arch.
        let macro_arch = Text {
            segments: vec![TextSegment::Macro(Box::new(MacroRef {
                kind: MacroKind::Braced,
                name: "some_arch".into(),
                args: Vec::new(),
                conditional: ConditionalMacro::None,
                with_value: None,
            }))],
        };
        let list = vec![Text::from("x86_64"), macro_arch];
        assert!(literal_archs(&list).is_none());
    }
}
