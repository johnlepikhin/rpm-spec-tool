//! RPM-REPO-020 `file-conflict-with-existing-package` — a path the
//! spec declares in any `%files` section is already owned by another
//! package in the configured repos.
//!
//! `dnf install` enforces "one file, one package" by default: if
//! `foobar-1.0` owns `/usr/bin/widget` and the new build also lists
//! `/usr/bin/widget` in `%files`, dnf rejects the transaction with
//! `file ... conflicts between attempted installs of ... and ...`.
//! The lint surfaces those collisions at lint time so they don't
//! land in the build pipeline.
//!
//! Heuristic, conservative:
//! 1. Only explicit literal paths from `%files` bodies are checked —
//!    no glob expansion (`/usr/lib/foo/*` is opaque), no `%file_list`
//!    indirection, no fully-macro paths (`%{python_sitelib}/...`).
//! 2. Paths owned by the SAME source name (i.e. another subpackage
//!    of the spec being built) are excluded — they're the
//!    "we're replacing our own files" case, not a conflict.
//! 3. `%ghost` entries are excluded — rpm treats those as "owned but
//!    not packaged"; multiple packages can claim the same `%ghost`
//!    legitimately.
//! 4. Paths under `/etc/alternatives/` are excluded — the alternatives
//!    system is built around shared ownership.
//!
//! Severity warn (per plan). A real conflict is a `dnf` error, but a
//! spec rename with `Obsoletes:` legitimately re-takes ownership and
//! the lint can't distinguish the two — warn lets operators triage.

use std::collections::BTreeSet;
use std::sync::Arc;

use rpm_spec::ast::{
    Conditional, FileDirective, FilePath, FilesContent, Section, Span, SpecFile, SpecItem,
};
use rpm_spec_profile::Profile;
use rpm_spec_repo_core::RepoUniverse;

use crate::bcond::{BcondMap, BcondOverrides};
use crate::branch_coverage::evaluate_branch;
use crate::diagnostic::{Diagnostic, LintCategory, RepoContext, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

use super::shared::RepoRule;
use crate::spec_nevr::{SpecMainNevr, enriched_macros_with_spec_locals};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM-REPO-020",
    name: "file-conflict-with-existing-package",
    description: "A path listed in `%files` is already owned by another package in the \
                  configured repos. `dnf install` rejects file collisions; either rename \
                  the file, add an `Obsoletes:` for the conflicting package, or mark it \
                  `%ghost`.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Path prefixes whose contents are intentionally shared between
/// packages (e.g. the alternatives system) and must NOT fire the
/// lint. Sorted longest-first so the prefix scan short-circuits on
/// the more specific entries.
const SHARED_PATH_PREFIXES: &[&str] = &["/etc/alternatives/"];

#[derive(Debug, Default)]
pub struct FileConflictWithExistingPackage {
    base: RepoRule,
}

impl FileConflictWithExistingPackage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for FileConflictWithExistingPackage {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(state) = self.base.state.as_ref() else {
            return;
        };
        let Some(profile) = self.base.profile.as_ref() else {
            return;
        };

        let bcond = BcondMap::from_spec(spec, &BcondOverrides::default());

        // Spec's own source-name — owners with this name are "our own
        // subpackages" and must NOT trigger the lint. Without the
        // source-name guard we'd flag every file the new build is
        // legitimately about to ship.
        //
        // Use the analyzer's full `SpecMainNevr` extractor (with
        // spec-local `%global`/`%define` expansion) so templated
        // names like `Name: %{prog_name}` resolve correctly — the
        // previous literal-only path silently skipped self-ownership
        // detection on every real-world spec.
        let macros = enriched_macros_with_spec_locals(spec, profile);
        let spec_source_name = SpecMainNevr::extract(spec, &macros).map(|n| n.name);
        if spec_source_name.is_none() {
            // Without the source name the self-ownership filter is
            // empty → every legitimate own-subpackage owner would
            // get flagged. Surface the degraded state so operators
            // tracing noisy reports can correlate it with their
            // unparseable preamble; the parser-side lints already
            // explain WHY `Name:` / `Version:` / `Release:` couldn't
            // be resolved (mixed macro, parser error).
            tracing::warn!(
                profile = %profile.identity.name,
                "RPM-REPO-020: spec main NEVR unresolvable; self-ownership filter inactive — \
                 reports may include false positives for own subpackages"
            );
        }
        // Hoist the "binaries built from this source" lookup OUT of
        // the per-file loop: the answer is the same for every file
        // we're about to check. Build a `HashSet<ProviderRef>` once,
        // turning N×SQL into 1×SQL + N×hash-lookup.
        let own_subpackage_refs: std::collections::HashSet<rpm_spec_repo_core::ProviderRef> =
            if let Some(name) = spec_source_name.as_deref() {
                match state.universe.binaries_built_from(name) {
                    Ok(c) => c.into_iter().map(|(p, _)| p).collect(),
                    Err(e) => {
                        tracing::warn!(
                            error = ?e,
                            name = ?name,
                            "binaries_built_from failed; self-ownership check degraded",
                        );
                        std::collections::HashSet::new()
                    }
                }
            } else {
                std::collections::HashSet::new()
            };

        // Per-spec dedup: a path listed in multiple `%files` sections
        // (or duplicated across subpackages) only emits one diagnostic.
        let mut reported: BTreeSet<String> = BTreeSet::new();

        let mut entries: Vec<&FilesEntryRef> = Vec::new();
        let mut owned_entries: Vec<FilesEntryRef> = Vec::new();
        collect_active_files_entries(&spec.items, profile, &bcond, &mut owned_entries);
        entries.extend(owned_entries.iter());

        for entry in entries {
            if entry.is_ghost {
                continue;
            }
            let Some(path) = entry.path.as_deref() else {
                continue;
            };
            if !looks_like_absolute_path(path) {
                // Relative paths get rpm-side prefixing (`%doc`,
                // `%{_docdir}/...`) we don't model; skip rather than
                // false-positive on `README.md`.
                continue;
            }
            if SHARED_PATH_PREFIXES.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            if !reported.insert(path.to_string()) {
                continue;
            }

            let owner_ref = match state.universe.file_owner(path) {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        path = ?path,
                        "file_owner lookup failed; skipping path",
                    );
                    continue;
                }
            };
            let owner_nevra = match state.universe.resolve_nevra(&owner_ref) {
                Ok(Some(n)) => n,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        path = ?path,
                        "resolve_nevra failed; skipping path",
                    );
                    continue;
                }
            };
            // Own-subpackage check: was the owning package built
            // from THIS spec's source RPM? If yes, "we're replacing
            // our own file" — legitimate, not a conflict. The set
            // was computed once above; per-file membership is just a
            // hash lookup.
            if own_subpackage_refs.contains(&owner_ref) {
                continue;
            }

            self.base.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    METADATA.default_severity,
                    format!(
                        "`{path}` is already owned by `{owner_nevra}` in the configured \
                         repos; `dnf install` will reject the file collision unless this \
                         spec declares `Obsoletes: {}` or marks the path `%ghost`",
                        owner_nevra.name,
                    ),
                    entry.span,
                )
                .with_repo_context(
                    RepoContext::for_profile(&state.universe.profile_name)
                        .with_nevra(owner_nevra.to_string()),
                ),
            );
        }
    }
}

impl Lint for FileConflictWithExistingPackage {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.base.take_diagnostics()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.base.set_profile(profile);
    }
    fn set_repo_universe(&mut self, universe: Option<Arc<RepoUniverse>>) {
        self.base.set_repo_universe(universe);
    }
}

/// Resolved view of one `%files` entry — the bits the lint actually
/// looks at, with directives flattened to a single `is_ghost` bool.
struct FilesEntryRef {
    path: Option<String>,
    is_ghost: bool,
    span: Span,
}

fn collect_active_files_entries(
    items: &[SpecItem<Span>],
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<FilesEntryRef>,
) {
    for item in items {
        match item {
            SpecItem::Section(boxed) => {
                if let Section::Files { content, data, .. } = boxed.as_ref() {
                    collect_active_files_content(content, profile, bcond, *data, out);
                }
            }
            SpecItem::Conditional(cond) => {
                walk_top_conditional(cond, profile, bcond, out);
            }
            _ => {}
        }
    }
}

fn walk_top_conditional(
    cond: &Conditional<Span, SpecItem<Span>>,
    profile: &Profile,
    bcond: &BcondMap,
    out: &mut Vec<FilesEntryRef>,
) {
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) => {
                collect_active_files_entries(&branch.body, profile, bcond, out);
                return;
            }
            Ok(false) => continue,
            Err(_) => return,
        }
    }
    if let Some(els) = &cond.otherwise {
        collect_active_files_entries(els, profile, bcond, out);
    }
}

fn collect_active_files_content(
    content: &[FilesContent<Span>],
    profile: &Profile,
    bcond: &BcondMap,
    section_span: Span,
    out: &mut Vec<FilesEntryRef>,
) {
    for c in content {
        match c {
            FilesContent::Entry(entry) => {
                out.push(FilesEntryRef {
                    path: entry.path.as_ref().and_then(file_path_literal),
                    is_ghost: entry
                        .directives
                        .iter()
                        .any(|d| matches!(d, FileDirective::Ghost)),
                    // Files section spans the whole `%files` body —
                    // re-use it for every entry so the diagnostic
                    // anchor is stable (per-entry sub-span isn't
                    // exposed on `FileEntry<Span>` directly).
                    span: entry.data,
                });
            }
            FilesContent::Conditional(cond) => {
                walk_files_conditional(cond, profile, bcond, section_span, out);
            }
            _ => {}
        }
    }
}

fn walk_files_conditional(
    cond: &Conditional<Span, FilesContent<Span>>,
    profile: &Profile,
    bcond: &BcondMap,
    section_span: Span,
    out: &mut Vec<FilesEntryRef>,
) {
    for branch in &cond.branches {
        match evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) => {
                collect_active_files_content(&branch.body, profile, bcond, section_span, out);
                return;
            }
            Ok(false) => continue,
            Err(_) => return,
        }
    }
    if let Some(els) = &cond.otherwise {
        collect_active_files_content(els, profile, bcond, section_span, out);
    }
}

/// Extract the literal text from a `FilePath` if the path is a pure
/// literal (no macros). Mixed-macro paths return `None` since the
/// lint can't compare them against repo file owners without
/// expansion — leave macro-resolved file conflicts for a future
/// iteration.
fn file_path_literal(fp: &FilePath) -> Option<String> {
    fp.path
        .literal_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A path is "absolute-looking" iff it starts with `/`. RPM file
/// paths are always rooted at `/`; relative entries are interpreted
/// via `%doc`/`%license` macros we don't expand here.
fn looks_like_absolute_path(path: &str) -> bool {
    path.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures::redos_profile;
    use crate::rules::test_support::run_repo_lint;
    use rpm_spec_repo_core::{
        CapFlags, Capability, NEVRA, Package, PkgChecksum, RepoIndex, RepoUniverse,
    };
    use time::OffsetDateTime;

    fn pkg(name: &str, files: Vec<&str>, source_rpm: Option<&str>) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from("1.0"),
                release: Arc::from("1.el9"),
                arch: Arc::from("x86_64"),
            },
            repo_id: Arc::from("baseos"),
            provides: vec![Capability {
                name: Arc::from(name),
                flags: CapFlags::None,
                evr: None,
            }],
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: source_rpm.map(Arc::from),
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Sha256(String::new()),
            location: Arc::from(""),
            files: files.into_iter().map(Arc::from).collect(),
        }
    }

    fn universe_with(packages: Vec<Package>) -> Arc<RepoUniverse> {
        let index = RepoIndex {
            repo_id: Arc::from("baseos"),
            revision: "rev0".to_string(),
            fetched_at: OffsetDateTime::now_utc(),
            packages,
            advisories: Vec::new(),
        };
        Arc::new(
            RepoUniverse::from_indexes_for_tests("redos-7.3-x86_64", vec![Arc::new(index)])
                .expect("build in-memory universe"),
        )
    }

    #[test]
    fn flags_path_owned_by_existing_package() {
        let uni = universe_with(vec![pkg(
            "otherpkg",
            vec!["/usr/bin/widget"],
            Some("otherpkg-1.0-1.src.rpm"),
        )]);
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\n/usr/bin/widget\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM-REPO-020");
        assert!(diags[0].message.contains("/usr/bin/widget"), "{}", diags[0].message);
        assert!(diags[0].message.contains("otherpkg"), "{}", diags[0].message);
    }

    #[test]
    fn silent_when_templated_name_resolves_to_own_source() {
        // Real-world `Name: %{prog_name}-server` shape: the spec
        // defines `%global prog_name acme` at the top, so the
        // resolved source-name is `acme-server`. The repo binary
        // `acme-server-libs` (built from `acme-server-1.0-1.src.rpm`)
        // must be recognised as a sibling subpackage and skipped —
        // a regression of the literal-only path would fail this.
        let uni = universe_with(vec![pkg(
            "acme-server-libs",
            vec!["/usr/lib/libacme.so.1"],
            Some("acme-server-1.0-1.src.rpm"),
        )]);
        let src = "%global prog_name acme\n\
                   Name: %{prog_name}-server\nVersion: 1\nRelease: 1\n\
                   Summary: s\nLicense: MIT\n\
                   %description\nx\n\
                   %files\n/usr/lib/libacme.so.1\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert!(
            diags.is_empty(),
            "templated `Name` must resolve through `enriched_macros_with_spec_locals`; got {diags:?}",
        );
    }

    #[test]
    fn silent_when_path_owned_by_own_subpackage() {
        // Existing repo binary is `mypkg-libs` built from `mypkg`
        // source. The new spec also names itself `mypkg` → owner is
        // a sibling subpackage, not a real conflict.
        let uni = universe_with(vec![pkg(
            "mypkg-libs",
            vec!["/usr/lib/libmypkg.so.1"],
            Some("mypkg-1.0-1.src.rpm"),
        )]);
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\n/usr/lib/libmypkg.so.1\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert!(diags.is_empty(), "self-ownership should not flag: {diags:?}");
    }

    #[test]
    fn silent_for_ghost_entries() {
        // %ghost is shared-ownership semantics — multiple packages may
        // legitimately claim the same path, e.g. /var/log/mypkg.log.
        let uni = universe_with(vec![pkg(
            "logrotate",
            vec!["/var/log/messages"],
            Some("logrotate-1.0-1.src.rpm"),
        )]);
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\n%ghost /var/log/messages\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert!(diags.is_empty(), "%ghost should not flag: {diags:?}");
    }

    #[test]
    fn silent_for_alternatives_paths() {
        // /etc/alternatives/ is shared-ownership by design.
        let uni = universe_with(vec![pkg(
            "java-21",
            vec!["/etc/alternatives/java"],
            Some("java-21-1.0-1.src.rpm"),
        )]);
        let src = "Name: java-17\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\n/etc/alternatives/java\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert!(diags.is_empty(), "alternatives should not flag: {diags:?}");
    }

    #[test]
    fn silent_for_unowned_paths() {
        // No package claims this path → nothing to conflict with.
        let uni = universe_with(vec![pkg(
            "other",
            vec!["/usr/bin/other"],
            Some("other-1.0-1.src.rpm"),
        )]);
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\n/usr/bin/mypkg\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_universe_missing() {
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\n/usr/bin/widget\n";
        let outcome = crate::session::parse(src);
        let mut lint = FileConflictWithExistingPackage::default();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(None);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn ignores_relative_paths_and_macro_paths() {
        // Relative `README.md` is %doc-ish — we don't model %doc
        // prefixing, so skip. `%{_bindir}/foo` is a macro path —
        // literal_str returns None → skipped.
        let uni = universe_with(vec![pkg(
            "decoy",
            vec!["/usr/bin/foo", "README.md"],
            Some("decoy-1.0-1.src.rpm"),
        )]);
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n%files\nREADME.md\n%{_bindir}/foo\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn dedup_across_subpackages() {
        // Same path in two %files sections (main + subpkg) — exactly
        // one diagnostic, not two.
        let uni = universe_with(vec![pkg(
            "otherpkg",
            vec!["/usr/share/mypkg/data"],
            Some("otherpkg-1.0-1.src.rpm"),
        )]);
        let src = "Name: mypkg\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
                   %description\nx\n\
                   %package extra\nSummary: extra\n%description extra\nextra\n\
                   %files\n/usr/share/mypkg/data\n\
                   %files extra\n/usr/share/mypkg/data\n";
        let diags = run_repo_lint::<FileConflictWithExistingPackage>(
            src,
            &redos_profile(),
            uni,
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
