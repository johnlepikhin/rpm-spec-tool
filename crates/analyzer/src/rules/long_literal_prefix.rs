//! RPM498 `long-literal-prefix-macro-candidate` — flag specs where
//! several `Source*` / `Patch*` / `URL` values share a long literal
//! prefix.
//!
//! When the same upstream URL prefix is repeated across many tags,
//! extracting it into a `%global upstream_base …` definition saves
//! characters and centralises the address.

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_top_level_preamble, spec_span};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM498",
    name: "long-literal-prefix-macro-candidate",
    description: "Multiple `Source` / `Patch` / `URL` tags share a long literal prefix — extract \
                  it into a `%global` and reference the global instead.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Minimum number of items that must share the prefix before the
/// extraction payoff outweighs the indirection.
const MIN_COUNT: usize = 3;
/// Minimum length of the common prefix for the rewrite to pay off.
const MIN_PREFIX_LEN: usize = 25;

/// Multiple `Source` / `Patch` / `URL` tags share a long literal prefix — extract it into a `%global` and reference the global instead.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct LongLiteralPrefix {
    diagnostics: Vec<Diagnostic>,
}

impl LongLiteralPrefix {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for LongLiteralPrefix {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut values: Vec<String> = Vec::new();
        for item in collect_top_level_preamble(spec) {
            let is_url_like = matches!(item.tag, Tag::Source(_) | Tag::Patch(_) | Tag::URL);
            if !is_url_like {
                continue;
            }
            if let TagValue::Text(t) = &item.value
                && let Some(lit) = t.literal_str()
            {
                let trimmed = lit.trim();
                if !trimmed.is_empty() {
                    values.push(trimmed.to_owned());
                }
            }
        }
        if values.len() < MIN_COUNT {
            return;
        }
        let Some(prefix) = longest_common_prefix(&values) else {
            return;
        };
        if prefix.len() < MIN_PREFIX_LEN {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "{n} URL-like tags share a {len}-char common prefix (`{snippet}…`); extract \
                     it into a `%global` macro and reference it from every tag",
                    n = values.len(),
                    len = prefix.len(),
                    snippet = preview(&prefix, 40),
                ),
                spec_span(spec),
            )
            .with_suggestion(Suggestion::new(
                "define `%global upstream_base PREFIX` and rewrite each tag as \
                 `Source0: %{upstream_base}…`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

fn longest_common_prefix(values: &[String]) -> Option<String> {
    let mut iter = values.iter();
    let first = iter.next()?.as_str();
    // Track common-prefix length in bytes by walking byte slices in
    // tandem — avoids allocating a `Vec<u8>` and repeatedly truncating
    // it, which copied O(N × max_url_len) bytes per check.
    let mut common = first.len();
    for v in iter {
        let bytes = v.as_bytes();
        let first_bytes = first.as_bytes();
        let mut i = 0;
        let max = common.min(bytes.len());
        while i < max && first_bytes[i] == bytes[i] {
            i += 1;
        }
        common = i;
        if common == 0 {
            return None;
        }
    }
    // `common` is a byte index; back it off to the nearest UTF-8 char
    // boundary so the resulting `&str` slice is valid.
    while common > 0 && !first.is_char_boundary(common) {
        common -= 1;
    }
    if common == 0 {
        return None;
    }
    // The prefix must end at a "safe" boundary — split on the last `/`
    // (or `://`) so the suggestion doesn't slice a domain in half.
    Some(trim_to_path_boundary(&first[..common]))
}

fn trim_to_path_boundary(s: &str) -> String {
    // Cut at the last `/` so the prefix ends on a clean path component.
    if let Some(last_slash) = s.rfind('/') {
        s[..=last_slash].to_owned()
    } else {
        s.to_owned()
    }
}

fn preview(s: &str, n: usize) -> String {
    // `n` is a character count; truncate on UTF-8 char boundaries so a
    // multi-byte character (e.g. `пример.рф`) straddling the cutoff
    // does not panic the slice. We cap at the byte length of `s` for
    // short strings to avoid an extra allocation.
    if s.len() <= n {
        return s.to_owned();
    }
    s.chars().take(n).collect()
}

impl Lint for LongLiteralPrefix {
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
        run_lint::<LongLiteralPrefix>(src)
    }

    #[test]
    fn flags_three_sources_with_long_common_prefix() {
        let src = "Name: x\n\
URL: https://example.com/project/releases/\n\
Source0: https://example.com/project/releases/v1/foo-1.tar.gz\n\
Source1: https://example.com/project/releases/v1/foo-2.tar.gz\n\
Source2: https://example.com/project/releases/v1/foo-3.tar.gz\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM498");
    }

    #[test]
    fn silent_when_below_count_threshold() {
        let src = "Name: x\n\
Source0: https://example.com/project/releases/v1/foo-1.tar.gz\n\
Source1: https://example.com/project/releases/v1/foo-2.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_prefix_too_short() {
        let src = "Name: x\n\
Source0: http://a/b.tar\n\
Source1: http://a/c.tar\n\
Source2: http://a/d.tar\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_independent_sources() {
        let src = "Name: x\n\
Source0: https://example.com/a/foo.tar.gz\n\
Source1: https://other-host.net/b/bar.tar.gz\n\
Source2: https://third-host.org/c/baz.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn truncates_multibyte_utf8_at_char_boundary() {
        // The Cyrillic IDN `https://пример.рф/very-long-path/` shares
        // its full prefix across three sources; `preview(&prefix, 40)`
        // and the byte-boundary backoff inside `longest_common_prefix`
        // must not panic on the multi-byte characters straddling any
        // cutoff.
        let src = "Name: x\n\
Source0: https://пример.рф/very-long-path/foo-1.tar.gz\n\
Source1: https://пример.рф/very-long-path/foo-2.tar.gz\n\
Source2: https://пример.рф/very-long-path/foo-3.tar.gz\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM498");
    }

    #[test]
    fn preview_handles_multibyte_truncation_without_panic() {
        // Direct check of `preview`: 10 *chars* on a string with
        // multi-byte Cyrillic must produce a valid `String`.
        let s = "https://пример.рф/very-long-path";
        let p = preview(s, 10);
        assert_eq!(p.chars().count(), 10);
        assert!(p.starts_with("https://п"));
    }

    #[test]
    fn longest_common_prefix_handles_multibyte_at_diverge_point() {
        // Two strings whose common ASCII prefix runs right up to a
        // multi-byte character that differs in only its second byte.
        // The byte-boundary backoff must yield a valid `&str` and the
        // returned prefix must be the longest valid UTF-8 prefix.
        let a = "abcпример".to_owned();
        let b = "abcпример_other".to_owned();
        let prefix = longest_common_prefix(&[a, b]).expect("non-empty prefix");
        // `trim_to_path_boundary` keeps everything up to the last `/`;
        // there is no `/`, so the full common prefix survives.
        assert!(prefix.starts_with("abc"));
        // Common bytes include the full "пример" since both share it.
        assert!(prefix.contains("пример"));
    }
}
