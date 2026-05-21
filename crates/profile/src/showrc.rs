//! Parser for `rpm --showrc` output.
//!
//! Line-based: header sections at the top (`ARCHITECTURE AND OS:`,
//! `BACKEND:`, `RPMRC VALUES:`, `Features supported by rpmlib:`), then a
//! `========================` divider, then macro records of the form
//!
//! ```text
//! -LEVEL: name(opts)?\tbody
//! -LEVEL= name(opts)?\tbody    (rpm uses `=` for locally-overridden entries)
//! ```
//!
//! Both `:` and `=` separators appear in real `rpm --showrc` output — `=`
//! marks an entry that was rebound in a local macro file. Treating them
//! the same here is safe: callers care about the resulting value, not
//! the rebinding mechanism.
//!
//! Multi-line macro bodies have continuation lines that do **not** start
//! with `^-\d+[:=]` — they are absorbed into the current entry until the
//! next record or the closing `======================== active N empty M`
//! summary line is reached.
//!
//! Header sections and the macro list may both be absent; the parser
//! fills in only what it sees.

use std::path::{Path, PathBuf};

use crate::merge::{ArchPatch, IdentityPatch, ProfilePatch};
use crate::types::{LayerInfo, MacroEntry, MacroValue, Provenance};

/// Parse a `rpm --showrc` dump into a [`ProfilePatch`].
///
/// Parsing is permissive: unrecognised lines are silently skipped rather
/// than treated as errors. `source_path` is recorded in macro provenance
/// so `profile show` can point at the dump file. Pass `None` for
/// in-memory fixtures.
pub fn parse(input: &str, source_path: Option<&Path>) -> ProfilePatch {
    let mut p = Parser::new(input, source_path);
    p.run();
    p.into_patch()
}

struct Parser<'a> {
    lines: std::str::Lines<'a>,
    source_path: Option<PathBuf>,

    // Output accumulators (mirrors ProfilePatch shape but mutable).
    arch: ArchPatch,
    rpmlib: Vec<(String, String)>,
    macros: Vec<(String, MacroEntry)>,

    // Pending macro record being built (entry + name + accumulated body).
    pending: Option<PendingMacro>,
}

struct PendingMacro {
    name: String,
    opts: Option<String>,
    level: i16,
    body: String,
    is_builtin: bool,
    /// `true` once at least one continuation line has been appended.
    multiline: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, source_path: Option<&Path>) -> Self {
        Self {
            lines: input.lines(),
            source_path: source_path.map(PathBuf::from),
            arch: ArchPatch::default(),
            rpmlib: Vec::new(),
            macros: Vec::new(),
            pending: None,
        }
    }

    fn run(&mut self) {
        let mut in_macro_section = false;

        // Walk lines without materialising them — `Lines` is a cheap
        // forward iterator and we never look back.
        while let Some(line) = self.lines.next() {
            if !in_macro_section {
                if line.starts_with(SECTION_DIVIDER) {
                    in_macro_section = true;
                    continue;
                }
                self.parse_header_line(line);
                continue;
            }

            // In macro section. Recognise the closing summary line
            // (`======================== active N empty M`) and stop.
            if line.starts_with(SECTION_DIVIDER) {
                break;
            }

            if let Some((level, name, opts, body_first_line)) = parse_record_header(line) {
                self.flush_pending();
                self.pending = Some(PendingMacro {
                    name: name.to_string(),
                    opts: opts.map(str::to_string),
                    level,
                    body: body_first_line.to_string(),
                    is_builtin: body_first_line == BUILTIN_MARKER,
                    multiline: false,
                });
            } else if let Some(p) = self.pending.as_mut() {
                // Continuation of previous macro body.
                p.body.push('\n');
                p.body.push_str(line);
                p.multiline = true;
            }
            // else: stray line before any macro record — silently dropped.
        }
        self.flush_pending();
    }

    fn parse_header_line(&mut self, line: &str) {
        let trimmed = line.trim_start();

        // Architecture / OS — split on first `:`.
        if let Some(rest) = trimmed.strip_prefix("build arch")
            && let Some(v) = colon_value(rest)
        {
            self.arch.build_arch = Some(v.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("build os")
            && let Some(v) = colon_value(rest)
        {
            self.arch.build_os = Some(v.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("compatible build archs")
            && let Some(v) = colon_value(rest)
        {
            self.arch.compatible_archs = Some(split_ws(v));
        } else if let Some(rest) = trimmed.strip_prefix("optflags")
            && let Some(v) = colon_value(rest)
        {
            self.arch.optflags_template = Some(v.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("rpmlib(") {
            // "rpmlib(Foo) = 1.2.3-1"
            if let Some(end) = rest.find(')') {
                let feature = &rest[..end];
                let after = &rest[end + 1..];
                if let Some(eq) = after.find('=') {
                    let version = after[eq + 1..].trim();
                    self.rpmlib
                        .push((format!("rpmlib({feature})"), version.to_string()));
                }
            }
        }
        // Everything else (BACKEND, default backend, Macro path, blank
        // lines, section headers themselves) is currently ignored.
    }

    fn flush_pending(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let provenance = Provenance::Showrc {
            level: p.level,
            path: self.source_path.clone(),
        };
        let body = p.body.trim_end_matches('\n').to_string();

        let value = if p.is_builtin {
            MacroValue::Builtin
        } else if !p.multiline && !body.contains('%') {
            MacroValue::Literal(body)
        } else {
            MacroValue::Raw {
                body,
                multiline: p.multiline,
            }
        };

        self.macros.push((
            p.name,
            MacroEntry {
                value,
                opts: p.opts,
                provenance,
            },
        ));
    }

    fn into_patch(self) -> ProfilePatch {
        let path_for_layer = self.source_path.clone();
        let macros_count = self.macros.len();
        let layer = path_for_layer.map(|path| LayerInfo::Showrc {
            path,
            macros: macros_count,
        });

        ProfilePatch {
            identity: IdentityPatch::default(), // identity comes from autodetect in a later pass
            macros: self.macros,
            rpmlib: self.rpmlib,
            arch: self.arch,
            licenses: None,
            groups: None,
            layer,
        }
    }
}

/// Try to parse `-LVL: name(opts)?\tbody` from a single line. Returns
/// `None` on lines that aren't macro-record headers (header sections,
/// continuation lines, blank lines).
fn parse_record_header(line: &str) -> Option<(i16, &str, Option<&str>, &str)> {
    // A macro record always starts with a signed level like `-13:` or
    // `-20:`. Keep the sign in the parsed value so provenance carries
    // the correct (negative) level. Rpm also emits `=` (instead of `:`)
    // for entries rebound by a local macro file — treat both the same.
    if !line.starts_with('-') {
        return None;
    }
    let sep = line.find([':', '='])?;
    let level: i16 = line[..sep].parse().ok()?;
    let after_colon = &line[sep + 1..];
    let after_colon = after_colon.strip_prefix(' ').unwrap_or(after_colon);

    // Split name(opts) from body on the first tab. If there is no tab,
    // the body is empty (rare but legal).
    let (name_part, body) = match after_colon.find('\t') {
        Some(t) => (&after_colon[..t], &after_colon[t + 1..]),
        None => (after_colon, ""),
    };

    // Macro names are `[A-Za-z_][A-Za-z0-9_]*` followed by an optional
    // `(...)`. If parens are present, anything inside is opts (rpm uses
    // forms like `(qp:m:)`, `(v:)`, `(n:)`).
    let (name, opts) = if let Some(open) = name_part.find('(') {
        let close = name_part[open..].find(')').map(|c| open + c)?;
        let opts = &name_part[open + 1..close];
        let name = &name_part[..open];
        (name, Some(opts))
    } else {
        (name_part, None)
    };

    if name.is_empty() || !is_valid_macro_name(name) {
        return None;
    }
    Some((level, name, opts, body))
}

fn is_valid_macro_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn colon_value(s: &str) -> Option<&str> {
    s.split_once(':').map(|(_, v)| v.trim())
}

fn split_ws(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// Section divider emitted by `rpm --showrc` before the macro list and
/// after it (`======================== active N empty M`).
const SECTION_DIVIDER: &str = "========";

/// First-line body marker for compiled-in (C-implemented) macros at
/// level `-20`.
const BUILTIN_MARKER: &str = "<builtin>";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_single_literal() {
        let input = "\
========================
-13: dist\t.el9
";
        let patch = parse(input, None);
        assert_eq!(patch.macros.len(), 1);
        let (name, entry) = &patch.macros[0];
        assert_eq!(name, "dist");
        assert!(matches!(&entry.value, MacroValue::Literal(s) if s == ".el9"));
        assert!(matches!(
            entry.provenance,
            Provenance::Showrc {
                level: -13,
                path: None
            }
        ));
    }

    #[test]
    fn parse_builtin() {
        let input = "\
========================
-20: P\t<builtin>
";
        let patch = parse(input, None);
        let (name, entry) = &patch.macros[0];
        assert_eq!(name, "P");
        assert!(matches!(entry.value, MacroValue::Builtin));
    }

    #[test]
    fn parse_parameterized_opts() {
        let input = "\
========================
-13: wordwrap(v:)\t%{lua:print('x')}
";
        let patch = parse(input, None);
        let (_, entry) = &patch.macros[0];
        assert_eq!(entry.opts.as_deref(), Some("v:"));
        assert!(matches!(entry.value, MacroValue::Raw { .. }));
    }

    #[test]
    fn parse_multiline_body() {
        let input = "\
========================
-13: ___build_pre\t
  %{___build_pre_env}
  umask 022
  cd \"%{_builddir}\"
-13: dist\t.el9
";
        let patch = parse(input, None);
        assert_eq!(patch.macros.len(), 2);
        let (n0, e0) = &patch.macros[0];
        assert_eq!(n0, "___build_pre");
        match &e0.value {
            MacroValue::Raw { multiline, body } => {
                assert!(multiline);
                assert!(body.contains("umask 022"));
                assert!(body.contains("cd \"%{_builddir}\""));
            }
            other => panic!("expected Raw, got {other:?}"),
        }
        let (n1, _) = &patch.macros[1];
        assert_eq!(n1, "dist");
    }

    #[test]
    fn parse_arch_and_rpmlib() {
        let input = "\
ARCHITECTURE AND OS:
build arch            : x86_64
compatible build archs: x86_64 noarch
build os              : Linux

Features supported by rpmlib:
    rpmlib(RichDependencies) = 4.12.0-1
    rpmlib(FileDigests) = 4.6.0-1

========================
";
        let patch = parse(input, None);
        assert_eq!(patch.arch.build_arch.as_deref(), Some("x86_64"));
        assert_eq!(patch.arch.build_os.as_deref(), Some("Linux"));
        assert_eq!(
            patch.arch.compatible_archs.as_deref(),
            Some(["x86_64".to_string(), "noarch".to_string()].as_slice())
        );
        assert_eq!(patch.rpmlib.len(), 2);
        let (k, v) = &patch.rpmlib[0];
        assert_eq!(k, "rpmlib(RichDependencies)");
        assert_eq!(v, "4.12.0-1");
    }

    #[test]
    fn parse_closes_at_active_line() {
        let input = "\
========================
-13: dist\t.el9
======================== active 1 empty 0
-13: post_terminator\t.ignored
";
        let patch = parse(input, None);
        // Only `dist` is captured — the `======...` summary closes the
        // macro section, and the following record is dropped.
        assert_eq!(patch.macros.len(), 1);
        assert_eq!(patch.macros[0].0, "dist");
    }

    #[test]
    fn parse_records_source_path_in_provenance() {
        let input = "========================\n-13: dist\t.el9\n";
        let patch = parse(input, Some(&PathBuf::from("vendor/foo.txt")));
        match &patch.macros[0].1.provenance {
            Provenance::Showrc { path, .. } => {
                assert_eq!(path.as_ref().unwrap(), &PathBuf::from("vendor/foo.txt"));
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[test]
    fn parse_equals_separator_accepted() {
        // `rpm --showrc` uses `=` instead of `:` when the entry was
        // rebound by a local macro file. Treat both as record headers.
        let input = "\
========================
-13= _db_backend\tsqlite
-11= _target_cpu\tx86_64
";
        let patch = parse(input, None);
        assert_eq!(patch.macros.len(), 2);
        assert_eq!(patch.macros[0].0, "_db_backend");
        assert_eq!(patch.macros[1].0, "_target_cpu");
        assert!(matches!(&patch.macros[0].1.value, MacroValue::Literal(s) if s == "sqlite"));
    }

    #[test]
    fn macro_with_percent_ref_is_raw_not_literal() {
        let input = "\
========================
-13: _target_alias\t%{_host_alias}
";
        let patch = parse(input, None);
        // Single-line, but contains a macro reference → must be Raw, not
        // Literal (Literal is reserved for already-resolved values that
        // downstream code can use as-is).
        assert!(matches!(
            patch.macros[0].1.value,
            MacroValue::Raw {
                multiline: false,
                ..
            }
        ));
    }
}
