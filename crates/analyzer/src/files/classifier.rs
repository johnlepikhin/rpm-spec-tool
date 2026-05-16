//! [`FilesClassifier`] — profile-aware view of one `FileEntry`.
//!
//! Single source of truth for the four questions every `%files`-flavoured
//! lint rule asks:
//!
//! 1. *What path does this entry refer to once macros are expanded?*
//!    — [`EntryClassification::resolved_path`].
//! 2. *Which directives apply (config, doc, license, ghost, attr…)?*
//!    — [`DirectiveSummary`].
//! 3. *What kind of file is it (devel header, locale, systemd unit…)?*
//!    — [`KindHints`].
//! 4. *Is it under a sensitive directory tree (`/etc`, `/var/run`,
//!    `/usr/lib/debug`, a standard-owned dir, …)?* — [`KindHints`].
//!
//! Paths are expanded against `Profile::macros` so distro-specific
//! layouts (`%{_libdir}` on x86_64 vs e2k, `%{_sysconfdir}` overrides)
//! are honoured. Macros that don't resolve leave the entry with
//! `resolved_path = None`; downstream rules treat that as "can't tell"
//! and stay silent — the conservative bail-out matches the rest of the
//! analyzer.

use rpm_spec::ast::{
    AttrField, AttrFields, ConfigFlag, FileDirective, FileEntry, FilePath, Span, Text, TextSegment,
};
use rpm_spec_profile::Profile;

/// Default depth limit for macro expansion in path resolution. Two-step
/// chains like `_libdir → %{_prefix}/lib64 → /usr/lib64` are common; 8
/// covers them with plenty of headroom while still bounding adversarial
/// inputs.
const EXPAND_DEPTH: u8 = 8;

/// Profile-bound view of `%files` semantics. Cheap to construct
/// (borrows the profile, expands a small set of well-known macros up
/// front), cheap to query.
#[derive(Debug)]
pub struct FilesClassifier<'a> {
    profile: &'a Profile,
    /// Cache of well-known directory macro expansions, in the
    /// "longest-prefix first" order required by [`KindHints`] checks.
    /// Stored as `(literal_prefix, &'static label)` so kind detection
    /// can use the label without re-expanding the macro per call.
    dir_table: Vec<(String, &'static str)>,
}

impl<'a> FilesClassifier<'a> {
    /// Build a classifier bound to `profile`. Expands a fixed set of
    /// directory macros once; everything else is lazy.
    pub fn new(profile: &'a Profile) -> Self {
        let dir_table = build_dir_table(profile);
        Self { profile, dir_table }
    }

    /// Classify one `FileEntry`. Always returns a value: when the path
    /// can't be resolved literally `resolved_path` is `None` and
    /// [`KindHints`] is built from whatever directive-only signal
    /// exists.
    pub fn classify<'e>(&self, entry: &'e FileEntry<Span>) -> EntryClassification<'e> {
        let resolved_path = entry.path.as_ref().and_then(|p| self.expand_path(&p.path));
        let directives = summarize_directives(&entry.directives);
        let kind_hints = match resolved_path.as_deref() {
            Some(path) => self.detect_kinds(path),
            None => KindHints::default(),
        };
        EntryClassification {
            entry,
            resolved_path,
            directives,
            kind_hints,
        }
    }

    /// Try to fully expand `text` to a plain path string. Returns
    /// `None` when any segment is a macro the profile can't resolve to
    /// a literal — callers conservatively skip such entries.
    pub fn expand_path(&self, text: &Text) -> Option<String> {
        let mut out = String::new();
        for seg in &text.segments {
            match seg {
                TextSegment::Literal(s) => out.push_str(s),
                TextSegment::Macro(m) => {
                    // `%{?dist}`-style conditional refs without a body
                    // resolve to empty when undefined, but only when
                    // the conditional says so; we conservatively bail
                    // out for any macro reference that isn't a plain
                    // `%{name}` we can answer.
                    use rpm_spec::ast::{ConditionalMacro, MacroKind};
                    if !matches!(m.kind, MacroKind::Plain | MacroKind::Braced) {
                        return None;
                    }
                    if !matches!(m.conditional, ConditionalMacro::None) {
                        // `%{?foo}` etc. — we don't know if `foo` is
                        // defined for the active profile; bail.
                        return None;
                    }
                    if !m.args.is_empty() || m.with_value.is_some() {
                        return None;
                    }
                    let expanded = self
                        .profile
                        .macros
                        .expand_to_literal(&m.name, EXPAND_DEPTH)?;
                    out.push_str(&expanded);
                }
                _ => return None,
            }
        }
        Some(out)
    }

    fn detect_kinds(&self, path: &str) -> KindHints {
        let mut hints = KindHints::default();
        let trimmed = path.trim();

        if trimmed.starts_with("/etc/") || trimmed == "/etc" {
            hints.under_etc = true;
        }
        if trimmed.starts_with("/usr/") || trimmed == "/usr" {
            hints.under_usr = true;
        }
        if trimmed.starts_with("/var/run/") || trimmed.starts_with("/run/") {
            hints.under_var_run = true;
        }
        if trimmed.starts_with("/var/lock/") {
            hints.under_var_lock = true;
        }
        if trimmed.starts_with("/usr/lib/debug")
            || trimmed.contains("/.build-id/")
            || trimmed.ends_with(".debug")
        {
            hints.under_debug = true;
        }
        if trimmed.starts_with("/usr/share/locale/") || trimmed.starts_with("/usr/lib/locale/") {
            hints.under_locale_dir = true;
        }
        if trimmed.ends_with(".mo") && hints.under_locale_dir {
            hints.is_locale_mo = true;
        }
        if trimmed.ends_with(".h") || trimmed.ends_with(".hpp") || trimmed.ends_with(".hxx") {
            hints.is_devel_header = true;
        }
        if trimmed.ends_with(".pc") && trimmed.contains("/pkgconfig/") {
            hints.is_pkgconfig = true;
        }
        if trimmed.ends_with("Config.cmake")
            || trimmed.ends_with("-config.cmake")
            || trimmed.ends_with("ConfigVersion.cmake")
        {
            hints.is_cmake_config = true;
        }
        // Unversioned `.so` link — `libfoo.so` (development), as
        // opposed to `libfoo.so.1` / `libfoo.so.1.2.3` (runtime).
        if let Some(stem) = strip_so_extension(trimmed) {
            hints.is_unversioned_so = !stem.contains(".so.");
        }
        if let Some(ext) = systemd_unit_ext(trimmed) {
            hints.systemd_unit_ext = Some(ext);
        }
        if (trimmed.starts_with("/usr/lib/tmpfiles.d/")
            || trimmed.starts_with("/etc/tmpfiles.d/")
            || trimmed.contains("/tmpfiles.d/"))
            && trimmed.ends_with(".conf")
        {
            hints.is_tmpfiles_conf = true;
        }
        if (trimmed.starts_with("/usr/lib/sysusers.d/") || trimmed.contains("/sysusers.d/"))
            && trimmed.ends_with(".conf")
        {
            hints.is_sysusers_conf = true;
        }
        // Standard-dir match: path exactly equals one of the cached
        // dir-table entries.
        if let Some((_prefix, label)) = self
            .dir_table
            .iter()
            .find(|(prefix, _)| trimmed == prefix.trim_end_matches('/'))
        {
            hints.standard_dir_macro = Some(*label);
        }
        // Broad glob: literal "<macro_dir>/*" with no further path
        // component. We match against the resolved prefix and then a
        // literal `/*`.
        if let Some(rest) = trimmed.strip_suffix("/*") {
            if let Some((_prefix, label)) = self
                .dir_table
                .iter()
                .find(|(prefix, _)| rest == prefix.trim_end_matches('/'))
            {
                hints.broad_glob_for = Some(*label);
            }
        }

        hints
    }
}

/// Result of classifying one `FileEntry`. Borrowed from the AST node
/// (the entry itself) plus owned derived data.
#[derive(Debug)]
pub struct EntryClassification<'a> {
    pub entry: &'a FileEntry<Span>,
    pub resolved_path: Option<String>,
    pub directives: DirectiveSummary,
    pub kind_hints: KindHints,
}

impl EntryClassification<'_> {
    /// Span pointing at the entry (used as the diagnostic anchor).
    pub fn span(&self) -> Span {
        self.entry.data
    }

    /// Convenience accessor for the raw `FilePath` AST node if present.
    pub fn file_path(&self) -> Option<&FilePath> {
        self.entry.path.as_ref()
    }
}

/// Flat view of an entry's directives. Multiple directives on one line
/// are merged; e.g. `%attr(0755, root, root) %config(noreplace) /etc/foo`
/// yields `config = Some(NoReplace)` and `attr = Some(...)`.
#[derive(Debug, Default, Clone)]
pub struct DirectiveSummary {
    pub config: Option<ConfigKind>,
    pub is_doc: bool,
    pub is_license: bool,
    pub is_ghost: bool,
    pub is_dir: bool,
    pub is_artifact: bool,
    pub is_missing_ok: bool,
    pub has_lang: bool,
    pub attr: Option<AttrSummary>,
}

/// `%config` flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// `%config` — overwrite on upgrade (unsafe but allowed).
    Plain,
    /// `%config(noreplace)` — preserve user changes on upgrade.
    NoReplace,
}

/// Numeric/literal summary of an `%attr(mode, user, group)` directive.
/// Each field is `Some` only when a literal value was supplied; `-`
/// placeholders and macro-valued slots stay `None`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AttrSummary {
    pub mode: Option<u32>,
    /// `true` when the user slot was a literal name (currently we don't
    /// expose the name itself — RPM370 only needs the mode + a tristate
    /// `is_root_owner` hint, which we can grow into).
    pub user_literal: bool,
    pub group_literal: bool,
}

/// Path-shape hints derived from the resolved path string.
#[derive(Debug, Default, Clone)]
pub struct KindHints {
    pub under_etc: bool,
    pub under_usr: bool,
    pub under_var_run: bool,
    pub under_var_lock: bool,
    pub under_debug: bool,
    pub under_locale_dir: bool,
    pub is_locale_mo: bool,
    pub is_devel_header: bool,
    pub is_pkgconfig: bool,
    pub is_cmake_config: bool,
    /// `true` for `libfoo.so` (unversioned development symlink), `false`
    /// for versioned `libfoo.so.1` runtime libraries, `false` for
    /// non-`.so` paths.
    pub is_unversioned_so: bool,
    /// `.service`, `.socket`, `.timer`, `.path`, `.mount`, `.target` —
    /// `None` for non-systemd paths.
    pub systemd_unit_ext: Option<&'static str>,
    pub is_tmpfiles_conf: bool,
    pub is_sysusers_conf: bool,
    /// `Some("_bindir")` when the entry's path is exactly the standard
    /// directory `_bindir` expands to — i.e. the package owns the bare
    /// directory.
    pub standard_dir_macro: Option<&'static str>,
    /// `Some("_datadir")` for `"%{_datadir}/*"`-style glob entries —
    /// the package owns everything under a standard directory.
    pub broad_glob_for: Option<&'static str>,
}

/// Build the directory table from `profile.macros`. Each entry is
/// `(expanded_literal_path, well_known_macro_label)`. Order matters:
/// longer prefixes come first so `/usr/lib64` wins over `/usr/lib`.
fn build_dir_table(profile: &Profile) -> Vec<(String, &'static str)> {
    /// Well-known directory macros that lints inspect. Listed in a
    /// hand-curated order so the longest typical literal expansion
    /// wins over shorter ones once sorted by `len` descending below.
    const KNOWN: &[&str] = &[
        "_bindir",
        "_sbindir",
        "_libdir",
        "_libexecdir",
        "_includedir",
        "_datadir",
        "_mandir",
        "_infodir",
        "_localstatedir",
        "_sharedstatedir",
        "_sysconfdir",
        "_unitdir",
        "_userunitdir",
        "_tmpfilesdir",
        "_sysusersdir",
        "_prefix",
        "_exec_prefix",
        "_docdir",
        "_defaultlicensedir",
    ];
    let mut out: Vec<(String, &'static str)> = KNOWN
        .iter()
        .filter_map(|name| {
            profile
                .macros
                .expand_to_literal(name, EXPAND_DEPTH)
                .map(|literal| (literal, *name))
        })
        .collect();
    // Longest first so longest-prefix matching is deterministic.
    out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    out
}

fn summarize_directives(dirs: &[FileDirective]) -> DirectiveSummary {
    let mut summary = DirectiveSummary::default();
    for d in dirs {
        match d {
            FileDirective::Config(flags) => {
                summary.config = Some(
                    if flags.iter().any(|f| matches!(f, ConfigFlag::NoReplace)) {
                        ConfigKind::NoReplace
                    } else {
                        ConfigKind::Plain
                    },
                );
            }
            FileDirective::Doc => summary.is_doc = true,
            FileDirective::License => summary.is_license = true,
            FileDirective::Ghost => summary.is_ghost = true,
            FileDirective::Dir => summary.is_dir = true,
            FileDirective::Artifact => summary.is_artifact = true,
            FileDirective::MissingOk => summary.is_missing_ok = true,
            FileDirective::Lang(_) => summary.has_lang = true,
            FileDirective::Attr(a) => summary.attr = Some(attr_summary(a)),
            // Defattr is per-section, not per-entry; handle separately
            // when needed. Verify/Caps don't affect Phase 18 rules.
            _ => {}
        }
    }
    summary
}

fn attr_summary(a: &AttrFields) -> AttrSummary {
    AttrSummary {
        mode: match &a.mode {
            AttrField::Numeric(n) => Some(*n),
            _ => None,
        },
        user_literal: matches!(a.user, AttrField::Name(_)),
        group_literal: matches!(a.group, AttrField::Name(_)),
    }
}

fn strip_so_extension(path: &str) -> Option<&str> {
    let last = path.rsplit('/').next()?;
    if !last.contains(".so") {
        return None;
    }
    Some(last)
}

fn systemd_unit_ext(path: &str) -> Option<&'static str> {
    const EXTS: &[&str] = &[
        ".service",
        ".socket",
        ".timer",
        ".path",
        ".mount",
        ".target",
        ".automount",
        ".slice",
    ];
    EXTS.iter().copied().find(|ext| path.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec::ast::{FilePath, Text};
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};

    fn make_profile(macros: &[(&str, &str)]) -> Profile {
        let mut p = Profile::default();
        for (name, body) in macros {
            p.macros
                .insert(*name, MacroEntry::literal(*body, Provenance::Override));
        }
        p
    }

    fn entry_with_path(path: &str) -> FileEntry<Span> {
        FileEntry {
            directives: Vec::new(),
            path: Some(FilePath {
                path: Text::from(path),
            }),
            data: Span::default(),
        }
    }

    fn fedora_like() -> Profile {
        make_profile(&[
            ("_prefix", "/usr"),
            ("_bindir", "/usr/bin"),
            ("_sbindir", "/usr/sbin"),
            ("_libdir", "/usr/lib64"),
            ("_datadir", "/usr/share"),
            ("_includedir", "/usr/include"),
            ("_sysconfdir", "/etc"),
            ("_unitdir", "/usr/lib/systemd/system"),
            ("_tmpfilesdir", "/usr/lib/tmpfiles.d"),
        ])
    }

    #[test]
    fn resolves_literal_path_unchanged() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/etc/foo.conf");
        let cls = c.classify(&e);
        assert_eq!(cls.resolved_path.as_deref(), Some("/etc/foo.conf"));
        assert!(cls.kind_hints.under_etc);
    }

    #[test]
    fn expands_braced_macro_in_path() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let mut e = entry_with_path("");
        e.path = Some(FilePath {
            path: text_macro_and_literal("_bindir", "/foo"),
        });
        let cls = c.classify(&e);
        assert_eq!(cls.resolved_path.as_deref(), Some("/usr/bin/foo"));
        assert!(cls.kind_hints.under_usr);
    }

    #[test]
    fn unresolvable_macro_leaves_path_none() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let mut e = entry_with_path("");
        e.path = Some(FilePath {
            path: text_macro_and_literal("totally_undefined", "/x"),
        });
        let cls = c.classify(&e);
        assert!(cls.resolved_path.is_none());
        assert!(!cls.kind_hints.under_usr);
    }

    #[test]
    fn standard_dir_macro_detected_for_bare_bindir() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let mut e = entry_with_path("");
        e.path = Some(FilePath {
            path: text_macro("_bindir"),
        });
        let cls = c.classify(&e);
        assert_eq!(cls.kind_hints.standard_dir_macro, Some("_bindir"));
    }

    #[test]
    fn broad_glob_detected_for_datadir_star() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let mut e = entry_with_path("");
        e.path = Some(FilePath {
            path: text_macro_and_literal("_datadir", "/*"),
        });
        let cls = c.classify(&e);
        assert_eq!(cls.kind_hints.broad_glob_for, Some("_datadir"));
    }

    #[test]
    fn locale_mo_detected() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/share/locale/ru/LC_MESSAGES/foo.mo");
        let cls = c.classify(&e);
        assert!(cls.kind_hints.is_locale_mo);
        assert!(cls.kind_hints.under_locale_dir);
    }

    #[test]
    fn devel_header_detected() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/include/foo.h");
        let cls = c.classify(&e);
        assert!(cls.kind_hints.is_devel_header);
    }

    #[test]
    fn pkgconfig_detected() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/lib64/pkgconfig/foo.pc");
        let cls = c.classify(&e);
        assert!(cls.kind_hints.is_pkgconfig);
    }

    #[test]
    fn systemd_unit_ext_detected() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/lib/systemd/system/foo.service");
        let cls = c.classify(&e);
        assert_eq!(cls.kind_hints.systemd_unit_ext, Some(".service"));
    }

    #[test]
    fn debug_path_detected() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/lib/debug/usr/bin/foo.debug");
        let cls = c.classify(&e);
        assert!(cls.kind_hints.under_debug);
    }

    #[test]
    fn unversioned_so_detected() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/lib64/libfoo.so");
        let cls = c.classify(&e);
        assert!(cls.kind_hints.is_unversioned_so);
    }

    #[test]
    fn versioned_so_not_flagged_as_unversioned() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let e = entry_with_path("/usr/lib64/libfoo.so.1");
        let cls = c.classify(&e);
        assert!(!cls.kind_hints.is_unversioned_so);
    }

    #[test]
    fn directive_summary_collects_config_doc_license() {
        let p = fedora_like();
        let c = FilesClassifier::new(&p);
        let mut e = entry_with_path("/etc/foo.conf");
        e.directives = vec![FileDirective::Config(vec![ConfigFlag::NoReplace])];
        let cls = c.classify(&e);
        assert_eq!(cls.directives.config, Some(ConfigKind::NoReplace));

        let mut e2 = entry_with_path("/usr/share/doc/foo/LICENSE");
        e2.directives = vec![FileDirective::License];
        let cls2 = c.classify(&e2);
        assert!(cls2.directives.is_license);
    }

    fn text_macro(name: &str) -> Text {
        use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef};
        Text {
            segments: vec![TextSegment::macro_ref(MacroRef {
                kind: MacroKind::Braced,
                name: name.into(),
                args: Vec::new(),
                conditional: ConditionalMacro::None,
                with_value: None,
            })],
        }
    }

    fn text_macro_and_literal(name: &str, suffix: &str) -> Text {
        use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef};
        Text {
            segments: vec![
                TextSegment::macro_ref(MacroRef {
                    kind: MacroKind::Braced,
                    name: name.into(),
                    args: Vec::new(),
                    conditional: ConditionalMacro::None,
                    with_value: None,
                }),
                TextSegment::Literal(suffix.into()),
            ],
        }
    }
}
