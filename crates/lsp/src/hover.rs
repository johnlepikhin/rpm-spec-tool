//! Static documentation lookup for hover requests.
//!
//! The LSP server doesn't know about macros defined by `rpm --showrc`
//! (that's a profile-level concern handled elsewhere). This module
//! covers the *common, stable* parts of the spec language:
//!
//! * Preamble tags (`Name`, `Version`, `License`, `BuildRequires`, …)
//! * Section directives (`%prep`, `%build`, `%install`, `%files`, …)
//! * Scriptlets (`%pre`, `%post`, `%postun`, …)
//!
//! Everything is hard-coded — fast, no I/O, no allocations on the hot
//! path. The token under the cursor is matched against the tables by
//! exact (case-insensitive for tags, case-sensitive for `%`-directives).

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};
use rpm_spec_profile::Profile;

/// Look up the token at `pos` in `source` and return hover content if
/// it matches a known tag or directive. When the token is a macro not
/// covered by the hard-coded tables, fall back to `profile.macros` —
/// editors get the expansion + provenance for every macro the active
/// distribution profile knows about.
///
/// Returns `None` when there is nothing useful to say — the client
/// treats that as "no hover".
pub fn lookup(source: &str, pos: Position, profile: Option<&Profile>) -> Option<Hover> {
    let line = line_for_position(source, pos)?;
    let col = pos.character as usize;
    let token = token_at(line, col)?;

    if let Some(doc) = docs_for(token) {
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: doc.to_string(),
            }),
            range: None,
        });
    }

    // Fallback: maybe the token is a `%name` macro defined in the
    // resolved profile. Strip the `%` prefix and look it up.
    let profile = profile?;
    let macro_name = token.strip_prefix('%')?;
    let entry = profile.macros.get(macro_name)?;
    let expansion = profile
        .macros
        .expand_to_literal(macro_name, 4)
        .unwrap_or_else(|| "<unable to expand>".to_string());
    let value = format!(
        "**`%{macro_name}`** — defined in profile.\n\n\
         **Value:** `{expansion}`\n\n\
         **Provenance:** `{:?}`",
        entry.provenance
    );
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

/// Extract the source line at `pos.line` (0-based). Returns `None` if
/// the line index is past EOF.
fn line_for_position(source: &str, pos: Position) -> Option<&str> {
    source.lines().nth(pos.line as usize)
}

/// Identify the identifier-like token covering `byte_col` within
/// `line`. We look both ways from the cursor as far as the character
/// class of an identifier allows.
///
/// Token classes recognised:
///   * `%xxx` — directive / scriptlet name (with the leading `%`)
///   * `Xxx` — preamble tag (alphanumeric + `_`)
///
/// Hovers in the middle of values, free-form text, or whitespace
/// return `None`.
fn token_at(line: &str, byte_col: usize) -> Option<&str> {
    // Treat `byte_col` as a UTF-16 code-unit offset in the typical
    // case where the line is ASCII (every preamble tag we care about
    // is ASCII anyway). For non-ASCII lines hover precision degrades
    // gracefully — the cursor lands on bytes, the lookup misses, and
    // we return None rather than risk slicing inside a codepoint.
    let bytes = line.as_bytes();
    if byte_col > bytes.len() {
        return None;
    }
    let col = byte_col.min(bytes.len());

    // Case: cursor sits *on* the `%` byte. Treat the following
    // identifier as part of a directive token starting at this `%`.
    if col < bytes.len() && bytes[col] == b'%' {
        let mut end = col + 1;
        while end < bytes.len() && is_ident_byte(bytes[end]) {
            end += 1;
        }
        if end > col + 1 {
            return Some(&line[col..end]);
        }
        return None;
    }

    // Walk back over identifier bytes to find the token start.
    let mut start = col;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    // Pull in a leading `%` if it directly precedes the identifier run.
    if start > 0 && bytes[start - 1] == b'%' {
        start -= 1;
    }
    let mut end = col;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if end == start {
        return None;
    }
    let token = &line[start..end];
    if token.is_empty() || token == "%" {
        None
    } else {
        Some(token)
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Lookup table: token → markdown doc snippet. Case-sensitive for
/// `%`-prefixed directives; case-insensitive for preamble tags.
fn docs_for(token: &str) -> Option<&'static str> {
    if let Some(directive) = token.strip_prefix('%') {
        for (name, doc) in DIRECTIVES {
            if *name == directive {
                return Some(doc);
            }
        }
        return None;
    }
    for (name, doc) in TAGS {
        if name.eq_ignore_ascii_case(token) {
            return Some(doc);
        }
    }
    None
}

/// Section directives, scriptlets, triggers. The bare name is stored
/// without the leading `%` — `token_at` strips it before lookup.
///
/// `pub(crate)` so the completion module can reuse the same labels +
/// docs without duplicating the table.
pub(crate) const DIRECTIVES: &[(&str, &str)] = &[
    // Build-script sections
    (
        "prep",
        "**`%prep`** — preparation phase. Unpack sources, apply patches. Runs in `$RPM_BUILD_DIR`.",
    ),
    (
        "conf",
        "**`%conf`** (rpm ≥ 4.18) — configure phase. Runs `./configure`-style commands separately from `%build`.",
    ),
    (
        "build",
        "**`%build`** — compile phase. Runs in the unpacked source directory.",
    ),
    (
        "install",
        "**`%install`** — installation phase. Stage files into `%{buildroot}`. Do not modify the live system.",
    ),
    (
        "check",
        "**`%check`** — test phase. Runs after `%install`. Failures abort the build unless `--nocheck` is passed.",
    ),
    (
        "clean",
        "**`%clean`** — buildroot cleanup. Deprecated since rpm 4.6 — rpm cleans `%{buildroot}` itself.",
    ),
    (
        "generate_buildrequires",
        "**`%generate_buildrequires`** (rpm ≥ 4.15) — emits additional `BuildRequires:` lines at build time (e.g. from `cargo metadata`).",
    ),
    // Subpackage / metadata sections
    (
        "package",
        "**`%package [-n] NAME`** — declares a subpackage with its own preamble. `-n` makes the name absolute; without `-n` it becomes `<main>-NAME`.",
    ),
    (
        "description",
        "**`%description [-n] [SUB]`** — free-form description text for the main package or a subpackage.",
    ),
    (
        "files",
        "**`%files [-n SUB] [-f FILELIST]`** — list of installed paths included in the (sub)package, with `%attr`, `%doc`, `%config`, `%dir` directives.",
    ),
    (
        "changelog",
        "**`%changelog`** — package change history. Entries start with `* Wkday Mon DD YYYY Name <email> - version`.",
    ),
    (
        "sourcelist",
        "**`%sourcelist`** — alternative to numbered `SourceN:` tags. One source URL per line.",
    ),
    (
        "patchlist",
        "**`%patchlist`** — alternative to numbered `PatchN:` tags. One patch path per line.",
    ),
    (
        "sepolicy",
        "**`%sepolicy [-n SUB]`** — SELinux module body (RHEL / Fedora). Built into a sepolicy subpackage.",
    ),
    (
        "verify",
        "**`%verify [-n SUB]`** — shell body executed by `rpm --verify` after installation.",
    ),
    // Scriptlets
    (
        "pre",
        "**`%pre`** — scriptlet run *before* package files are installed.",
    ),
    (
        "post",
        "**`%post`** — scriptlet run *after* package files are installed.",
    ),
    (
        "preun",
        "**`%preun`** — scriptlet run *before* the package is removed.",
    ),
    (
        "postun",
        "**`%postun`** — scriptlet run *after* the package is removed.",
    ),
    (
        "pretrans",
        "**`%pretrans`** — scriptlet run once before the whole transaction (install + upgrade + erase) starts.",
    ),
    (
        "posttrans",
        "**`%posttrans`** — scriptlet run once after the whole transaction finishes.",
    ),
    (
        "preuntrans",
        "**`%preuntrans`** (rpm ≥ 4.19) — pre-transaction scriptlet for erase.",
    ),
    (
        "postuntrans",
        "**`%postuntrans`** (rpm ≥ 4.19) — post-transaction scriptlet for erase.",
    ),
    // Triggers
    (
        "triggerprein",
        "**`%triggerprein`** — trigger that fires *before* another package is installed/upgraded.",
    ),
    (
        "triggerin",
        "**`%triggerin`** — trigger that fires *after* another package is installed/upgraded.",
    ),
    (
        "triggerun",
        "**`%triggerun`** — trigger that fires *before* another package is removed.",
    ),
    (
        "triggerpostun",
        "**`%triggerpostun`** — trigger that fires *after* another package is removed.",
    ),
    (
        "filetriggerin",
        "**`%filetriggerin`** (rpm ≥ 4.13) — fires after a transaction that touched any file matching the prefix.",
    ),
    (
        "filetriggerun",
        "**`%filetriggerun`** — fires before such a transaction.",
    ),
    (
        "filetriggerpostun",
        "**`%filetriggerpostun`** — fires after the transaction has finished erasing.",
    ),
    (
        "transfiletriggerin",
        "**`%transfiletriggerin`** — like `%filetriggerin` but runs once per transaction.",
    ),
    (
        "transfiletriggerun",
        "**`%transfiletriggerun`** — like `%filetriggerun` but runs once per transaction.",
    ),
    (
        "transfiletriggerpostun",
        "**`%transfiletriggerpostun`** — like `%filetriggerpostun` but runs once per transaction.",
    ),
    // Conditionals (commonly hovered)
    (
        "if",
        "**`%if EXPR`** — open a conditional block. Expression is evaluated at parse time. Close with `%endif`.",
    ),
    (
        "ifarch",
        "**`%ifarch ARCH...`** — branch on build target architecture (e.g. `%ifarch x86_64`).",
    ),
    ("ifnarch", "**`%ifnarch ARCH...`** — inverse of `%ifarch`."),
    ("ifos", "**`%ifos OS`** — branch on build target OS."),
    (
        "elif",
        "**`%elif EXPR`** — elif branch of a `%if` conditional.",
    ),
    ("else", "**`%else`** — else branch of a `%if` conditional."),
    ("endif", "**`%endif`** — close a `%if` conditional block."),
    // Macros
    (
        "define",
        "**`%define NAME VALUE`** — define a macro local to the current spec.",
    ),
    (
        "global",
        "**`%global NAME VALUE`** — define a macro visible to all later contexts (including subshells).",
    ),
    (
        "undefine",
        "**`%undefine NAME`** — undefine a previously-defined macro.",
    ),
    (
        "include",
        "**`%include FILE`** — splice the contents of another file at this point.",
    ),
    (
        "bcond",
        "**`%bcond NAME [default]`** (rpm ≥ 4.17.1) — define a build conditional toggled by `--with NAME` / `--without NAME`.",
    ),
    (
        "bcond_with",
        "**`%bcond_with NAME`** — legacy form; defaults to *without*.",
    ),
    (
        "bcond_without",
        "**`%bcond_without NAME`** — legacy form; defaults to *with*.",
    ),
];

/// Preamble tags. Looked up case-insensitively.
pub(crate) const TAGS: &[(&str, &str)] = &[
    (
        "Name",
        "**`Name:`** — package name. Required. Must match the spec filename without `.spec`.",
    ),
    (
        "Version",
        "**`Version:`** — upstream version. No dashes allowed (use `~`/`^` for pre/post-release markers).",
    ),
    (
        "Release",
        "**`Release:`** — packaging revision. Usually `<num>%{?dist}` so the distribution tag is appended.",
    ),
    (
        "Summary",
        "**`Summary:`** — one-line description shown by `rpm -qi`.",
    ),
    (
        "License",
        "**`License:`** — SPDX-style license identifier(s). Use ` AND ` / ` OR ` for compound expressions.",
    ),
    (
        "Group",
        "**`Group:`** — package group. Deprecated since rpm 4.6 but still parsed.",
    ),
    ("URL", "**`URL:`** — homepage of the upstream project."),
    (
        "Source",
        "**`SourceN:`** — source tarball or file. `Source0:` is conventional for the main archive.",
    ),
    ("Source0", "**`Source0:`** — primary upstream tarball."),
    (
        "Patch",
        "**`PatchN:`** — patch to apply during `%prep` via `%patchN` or `%autopatch`.",
    ),
    (
        "BuildArch",
        "**`BuildArch:`** — restrict the package architecture (commonly `noarch`).",
    ),
    (
        "ExclusiveArch",
        "**`ExclusiveArch:`** — build only on the listed architectures.",
    ),
    (
        "ExcludeArch",
        "**`ExcludeArch:`** — skip the listed architectures.",
    ),
    (
        "BuildRequires",
        "**`BuildRequires:`** — build-time dependency. Installed by `dnf builddep` before `%build`.",
    ),
    (
        "Requires",
        "**`Requires:`** — runtime dependency. Use `Requires(pre)`/`Requires(post)`/… for scriptlet-time deps.",
    ),
    (
        "Recommends",
        "**`Recommends:`** — soft dependency installed by default but removable.",
    ),
    (
        "Suggests",
        "**`Suggests:`** — soft dependency *not* installed by default.",
    ),
    (
        "Supplements",
        "**`Supplements:`** — reverse `Recommends` (installs this package if the named one is present).",
    ),
    ("Enhances", "**`Enhances:`** — reverse `Suggests`."),
    (
        "Provides",
        "**`Provides:`** — virtual capability exposed by this package.",
    ),
    (
        "Conflicts",
        "**`Conflicts:`** — packages that cannot be installed alongside this one.",
    ),
    (
        "Obsoletes",
        "**`Obsoletes:`** — packages this one replaces on upgrade.",
    ),
    (
        "Prefixes",
        "**`Prefixes:`** — installation prefix list for relocatable packages.",
    ),
    (
        "Prefix",
        "**`Prefix:`** — single installation prefix for relocatable packages.",
    ),
    (
        "BuildRoot",
        "**`BuildRoot:`** — historical buildroot override. Modern rpm ignores this; do not set.",
    ),
    (
        "Epoch",
        "**`Epoch:`** — version-comparison override. Avoid unless absolutely necessary.",
    ),
    (
        "Vendor",
        "**`Vendor:`** — distribution / packager identity. Often set via macros.",
    ),
    (
        "Packager",
        "**`Packager:`** — name + email of the maintainer.",
    ),
    ("Distribution", "**`Distribution:`** — distribution name."),
    (
        "AutoReq",
        "**`AutoReq:`** — `0` disables automatic dependency extraction.",
    ),
    (
        "AutoProv",
        "**`AutoProv:`** — `0` disables automatic provide extraction.",
    ),
    (
        "AutoReqProv",
        "**`AutoReqProv:`** — `0` disables both `AutoReq` and `AutoProv`.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_at_finds_tag() {
        let line = "BuildRequires: foo, bar";
        assert_eq!(token_at(line, 0), Some("BuildRequires"));
        assert_eq!(token_at(line, 5), Some("BuildRequires"));
        assert_eq!(token_at(line, 13), Some("BuildRequires"));
    }

    #[test]
    fn token_at_finds_directive_with_percent() {
        let line = "%prep";
        assert_eq!(token_at(line, 0), Some("%prep"));
        assert_eq!(token_at(line, 3), Some("%prep"));
    }

    #[test]
    fn token_at_returns_none_in_whitespace() {
        let line = "Name:   hello";
        assert_eq!(token_at(line, 6), None); // middle of spaces
    }

    #[test]
    fn lookup_known_tag_returns_markdown() {
        let src = "License: MIT\n";
        let h = lookup(src, Position::new(0, 2), None).expect("hover for License");
        match h.contents {
            HoverContents::Markup(m) => {
                assert_eq!(m.kind, MarkupKind::Markdown);
                assert!(m.value.contains("License"));
            }
            _ => panic!("expected markup"),
        }
    }

    #[test]
    fn lookup_unknown_returns_none() {
        let src = "FooBarBaz: nope\n";
        assert!(lookup(src, Position::new(0, 2), None).is_none());
    }

    #[test]
    fn lookup_directive_at_percent_position() {
        let src = "%install\nmake install\n";
        let h = lookup(src, Position::new(0, 0), None).expect("hover for %install");
        match h.contents {
            HoverContents::Markup(m) => assert!(m.value.contains("installation")),
            _ => panic!("expected markup"),
        }
    }

    #[test]
    fn tag_lookup_is_case_insensitive() {
        let src = "license: GPL\n";
        assert!(lookup(src, Position::new(0, 1), None).is_some());
    }
}
