//! Parser for ad-hoc macro overrides supplied as raw `NAME VALUE`
//! strings. Modelled on `rpmbuild --define 'NAME VALUE'` so packagers
//! coming from `rpmbuild` get the same syntax: a single argument, name
//! and value separated by ASCII whitespace, value preserved verbatim
//! (including embedded whitespace and trailing whitespace) up to the
//! end of the argument.
//!
//! Used by [`crate::resolve::ResolveOptions::cli_defines`]; consumers
//! pass a slice of raw argv strings, the resolver parses each via
//! [`parse_define`] and applies them as the final
//! [`crate::LayerInfo::CliDefine`] layer.
//!
//! Strictness differences from rpmbuild — fail-fast where rpmbuild
//! silently accepts garbage that can never be referenced:
//!
//! * Empty argument → [`DefineParseError::EmptyArgument`].
//! * Whitespace-only argument → also [`DefineParseError::EmptyArgument`].
//! * No whitespace separator → [`DefineParseError::MissingValue`]
//!   (rpmbuild silently defines the macro with empty body; that's
//!   indistinguishable from a typo at lint time).
//! * Empty value after the separator → same `MissingValue`.
//! * Name containing `%`, `{`, `}`, or whitespace →
//!   [`DefineParseError::InvalidName`] (rpmbuild stores these but they
//!   can never be referenced from a spec, so they're always typos).

use thiserror::Error;

use crate::types::{MacroEntry, MacroValue, Provenance};

/// One parsed CLI override — name plus the constructed [`MacroEntry`]
/// ready to layer onto a [`crate::Profile`].
#[derive(Debug, Clone)]
pub struct CliDefine {
    pub name: String,
    pub entry: MacroEntry,
}

/// Parse failures for a single `--define` argument. Each variant
/// carries enough context for the CLI to print an actionable error
/// (the offending name when it's known, the original raw string when
/// it isn't).
///
/// `#[non_exhaustive]` because we expect to add variants when
/// `--undefine` / `--force-distro` land — keeps adding a new failure
/// mode from being a breaking change for downstream `match`es.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DefineParseError {
    /// The argument was empty or contained only whitespace —
    /// almost certainly a shell-quoting bug (`--define ""`).
    #[error("--define argument is empty or whitespace-only")]
    EmptyArgument,

    /// Name was parsed but no value followed (no whitespace separator,
    /// or value was empty). rpmbuild accepts this and defines the
    /// macro with an empty body; we reject because a CI typo like
    /// `-D 'with_python'` (missing value) is hard to debug otherwise.
    #[error("--define `{name}`: missing value (expected `NAME VALUE` with whitespace separator)")]
    MissingValue { name: String },

    /// Name contains a character that would make the macro
    /// unreferenceable: `%`, `{`, `}`, or whitespace.
    #[error("--define `{name}`: invalid macro name ({reason})")]
    InvalidName { name: String, reason: &'static str },
}

/// Parse a single `--define` argument into a [`CliDefine`].
///
/// Tokenisation mirrors rpmbuild's: split on the *first* run of ASCII
/// whitespace. The value is the rest of the argument verbatim — no
/// trimming of trailing whitespace, no quote stripping (the shell
/// already did that).
///
/// The constructed entry carries [`Provenance::Override`]; the
/// distinction between TOML overrides and CLI overrides is captured
/// at layer granularity via [`crate::LayerInfo::CliDefine`] rather
/// than per-entry, because a sub-field on `Provenance::Override`
/// would force every existing consumer to match on it.
pub fn parse_define(raw: &str) -> Result<CliDefine, DefineParseError> {
    if raw.trim().is_empty() {
        return Err(DefineParseError::EmptyArgument);
    }

    // Strip leading whitespace, then find the first whitespace run to
    // split name from value. `trim_start` because rpmbuild silently
    // ignores leading spaces; doing the same avoids spurious errors
    // from `--define ' name value'` (which a careless shell could
    // produce).
    let raw = raw.trim_start();
    let split_at = raw.find(|c: char| c.is_ascii_whitespace()).ok_or_else(|| {
        DefineParseError::MissingValue {
            name: raw.to_string(),
        }
    })?;
    let (name, rest) = raw.split_at(split_at);
    // Skip exactly one separator run; the value starts at the first
    // non-whitespace character. Anything after that (including trailing
    // whitespace) is preserved verbatim — `%{dist}` values like
    // ".el9 " with a trailing space are legal even if uncommon.
    let value_start = rest
        .char_indices()
        .find(|(_, c)| !c.is_ascii_whitespace())
        .map(|(i, _)| i);
    let Some(value_start) = value_start else {
        return Err(DefineParseError::MissingValue {
            name: name.to_string(),
        });
    };
    let value = &rest[value_start..];

    validate_name(name)?;

    Ok(CliDefine {
        name: name.to_string(),
        entry: MacroEntry {
            value: MacroValue::from_user_body(value),
            opts: None,
            provenance: Provenance::Override,
        },
    })
}

/// Validate a parsed macro name against rpm's lexer rule:
/// `[A-Za-z_][A-Za-z0-9_]*`. Anything outside this whitelist produces
/// a macro that can never be referenced from a spec (rpm's scanner
/// won't match it), so accepting it would silently no-op at lint
/// time — exactly the CI typo we want to catch.
///
/// The previous denylist (only `%`, `{`, `}`, whitespace) let through:
/// digit-leading names (`9foo` — not a valid identifier), parens and
/// colons (`foo(x)`, `foo:default` — reserved by parameterised and
/// defaulting macro syntax), and non-ASCII (`café` — rpm scanner is
/// ASCII-only). All now rejected with a precise reason.
fn validate_name(name: &str) -> Result<(), DefineParseError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(DefineParseError::InvalidName {
            name: String::new(),
            reason: "empty",
        });
    };
    if !is_name_start(first) {
        return Err(DefineParseError::InvalidName {
            name: name.to_string(),
            reason: "macro names must start with an ASCII letter or `_`",
        });
    }
    for c in chars {
        if !is_name_cont(c) {
            return Err(DefineParseError::InvalidName {
                name: name.to_string(),
                reason: "macro names may only contain ASCII letters, digits, or `_`",
            });
        }
    }
    Ok(())
}

/// First character of an rpm macro name: ASCII letter or underscore.
/// Mirrors the production rule in rpm's lexer.
#[inline]
fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Continuation character of an rpm macro name: ASCII alphanumeric or
/// underscore.
#[inline]
fn is_name_cont(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Parse every raw argument in `raws`, collecting the parsed defines
/// in CLI order. Returns the first error encountered — partial
/// application is never visible.
///
/// Generic over the element so call sites with `&[String]`,
/// `&[&str]`, or `&[Cow<'_, str>]` don't need to allocate.
/// `pub(crate)` because the only external user is the resolver; CLI
/// callers should funnel through [`crate::resolve::ResolveOptions`]
/// instead of touching the parser directly.
///
/// # Errors
///
/// Returns the first [`DefineParseError`] encountered (the iterator
/// short-circuits on `Err`).
pub(crate) fn parse_all<S: AsRef<str>>(raws: &[S]) -> Result<Vec<CliDefine>, DefineParseError> {
    raws.iter().map(|s| parse_define(s.as_ref())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_value_pair() {
        let d = parse_define("with_python 1").unwrap();
        assert_eq!(d.name, "with_python");
        assert_eq!(d.entry.value, MacroValue::Literal("1".into()));
        assert!(matches!(d.entry.provenance, Provenance::Override));
    }

    #[test]
    fn value_with_macro_ref_becomes_raw() {
        let d = parse_define("_libdir %{_prefix}/lib64").unwrap();
        assert_eq!(d.name, "_libdir");
        match &d.entry.value {
            MacroValue::Raw { body, multiline } => {
                assert_eq!(body, "%{_prefix}/lib64");
                assert!(!multiline);
            }
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn value_preserves_embedded_whitespace() {
        // SPDX expressions, command lines, etc. — interior whitespace
        // is significant.
        let d = parse_define("license MIT OR Apache-2.0").unwrap();
        assert_eq!(
            d.entry.value,
            MacroValue::Literal("MIT OR Apache-2.0".into())
        );
    }

    #[test]
    fn value_preserves_trailing_whitespace() {
        // rpmbuild keeps trailing whitespace verbatim — `dist .el9 ` is
        // technically different from `dist .el9`. We match that.
        let d = parse_define("dist .el9 ").unwrap();
        assert_eq!(d.entry.value, MacroValue::Literal(".el9 ".into()));
    }

    #[test]
    fn tab_separator_works() {
        // Tabs are ASCII whitespace; rpmbuild handles them. Real users
        // are unlikely to type tabs by hand, but scripts that build
        // argv programmatically might.
        let d = parse_define("name\tvalue").unwrap();
        assert_eq!(d.name, "name");
        assert_eq!(d.entry.value, MacroValue::Literal("value".into()));
    }

    #[test]
    fn leading_whitespace_is_stripped() {
        let d = parse_define("   name value").unwrap();
        assert_eq!(d.name, "name");
    }

    #[test]
    fn multiple_separator_chars_collapsed() {
        let d = parse_define("name   value").unwrap();
        // The value should not include the separator whitespace.
        assert_eq!(d.entry.value, MacroValue::Literal("value".into()));
    }

    #[test]
    fn empty_argument_rejected() {
        assert!(matches!(
            parse_define(""),
            Err(DefineParseError::EmptyArgument)
        ));
        assert!(matches!(
            parse_define("   "),
            Err(DefineParseError::EmptyArgument)
        ));
    }

    #[test]
    fn missing_value_rejected() {
        // No whitespace at all → no separator → MissingValue.
        match parse_define("with_python") {
            Err(DefineParseError::MissingValue { name }) => assert_eq!(name, "with_python"),
            other => panic!("expected MissingValue, got {other:?}"),
        }
        // Trailing whitespace but no value → also MissingValue. We
        // reject this even though rpmbuild would accept it because a
        // CI typo like `-D 'with_python '` shouldn't silently no-op.
        match parse_define("with_python   ") {
            Err(DefineParseError::MissingValue { name }) => assert_eq!(name, "with_python"),
            other => panic!("expected MissingValue, got {other:?}"),
        }
    }

    #[test]
    fn name_containing_percent_rejected() {
        match parse_define("%foo bar") {
            Err(DefineParseError::InvalidName { name, .. }) => assert_eq!(name, "%foo"),
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    #[test]
    fn name_containing_braces_rejected() {
        assert!(matches!(
            parse_define("foo{bar} baz"),
            Err(DefineParseError::InvalidName { .. })
        ));
        assert!(matches!(
            parse_define("{foo bar"),
            Err(DefineParseError::InvalidName { .. })
        ));
    }

    /// Names starting with a digit are syntactically valid as
    /// HashMap keys but unreferenceable as rpm macros (rpm's lexer
    /// requires `[A-Za-z_]` start). The strict whitelist must reject
    /// them so a CI typo like `-D '9foo bar'` fails loudly.
    #[test]
    fn name_starting_with_digit_rejected() {
        match parse_define("9foo bar") {
            Err(DefineParseError::InvalidName { name, reason }) => {
                assert_eq!(name, "9foo");
                assert!(
                    reason.contains("start"),
                    "reason should mention start-character rule: {reason}"
                );
            }
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    /// Parameterised-macro syntax (`%define foo(opts) body`) reserves
    /// `(` / `)` in names. CLI defines don't support the parameterised
    /// form, so a name containing parens is a typo.
    #[test]
    fn name_containing_parens_rejected() {
        assert!(matches!(
            parse_define("foo(s) value"),
            Err(DefineParseError::InvalidName { .. })
        ));
    }

    /// `:` is reserved for default-value syntax (`%{name:default}`)
    /// inside macro references. A `:` in the *name* is always wrong.
    #[test]
    fn name_containing_colon_rejected() {
        assert!(matches!(
            parse_define("foo:bar baz"),
            Err(DefineParseError::InvalidName { .. })
        ));
    }

    /// rpm's macro scanner is ASCII-only. A non-ASCII name lands in
    /// the registry but can never be matched by `%{name}` lookup.
    #[test]
    fn non_ascii_name_rejected() {
        assert!(matches!(
            parse_define("café value"),
            Err(DefineParseError::InvalidName { .. })
        ));
    }

    /// Single underscore is a valid identifier (rpm convention is
    /// `_prefix`, `_libdir`, etc.) — must NOT be rejected.
    #[test]
    fn underscore_leading_name_accepted() {
        let d = parse_define("_libdir /opt/lib").unwrap();
        assert_eq!(d.name, "_libdir");
    }

    #[test]
    fn parse_all_collects_in_order() {
        let raws: Vec<&str> = vec!["a 1", "b 2", "c 3"];
        let parsed = parse_all(&raws).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].name, "a");
        assert_eq!(parsed[2].name, "c");
    }

    #[test]
    fn parse_all_short_circuits_on_first_error() {
        // Second element is malformed; the third is fine but parse_all
        // must surface the second's error without continuing.
        let raws: Vec<&str> = vec!["a 1", "%bad value", "c 3"];
        let err = parse_all(&raws).unwrap_err();
        assert!(matches!(err, DefineParseError::InvalidName { .. }));
    }

    /// `parse_all` is generic over `AsRef<str>` — make sure both
    /// `&[String]` (the resolver's call shape) and `&[&str]`
    /// (convenient for ad-hoc callers) compile and behave identically.
    #[test]
    fn parse_all_works_with_string_slice() {
        let raws: Vec<String> = vec!["x 1".to_string(), "y 2".to_string()];
        let parsed = parse_all(&raws).unwrap();
        assert_eq!(parsed.len(), 2);
    }
}
