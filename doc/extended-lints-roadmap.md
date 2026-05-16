# Extended Lints — Implementation Roadmap

> Source research: `doc/extendend-lints.md` (Section 4: ~100 proposed rules; Section 5: deep analyses).
> Status: approved plan, phased across PRs 17–25. Track progress here as phases ship.

## Context

`doc/extendend-lints.md` proposes ~100 new lint rules in 6 categories (RPM300–RPM406) plus several deeper analyses (Section 5). The analyzer currently ships ~85 rules (RPM001–RPM129 + RPM200–RPM201, registry in `crates/analyzer/src/registry.rs`). This document phases the work so that:

1. Each PR ships a working set of rules with tests and stays reviewable in size.
2. Reusable infrastructure (`FilesClassifier`, `CommandUseIndex`, `ProfilePolicyRegistry`) lands **together** with the first rule that uses it — no pure-plumbing PRs, but at most one major component per phase.
3. P0 rules (implementable on existing infrastructure) come first, then P1 (need policy maps), then P2 (deep analyses as a separate roadmap inside Phase 25).

ID numbering follows the research doc (RPM3xx–RPM4xx). Intentional gaps are preserved; the range does not collide with existing RPM0xx–RPM2xx.

## Phasing strategy

Each phase = one PR with a feat commit (`feat(analyzer): Phase N — <theme>`) matching the existing style:
- `Phase 14-15 — profile-aware lints + family-gated rules`
- `Phase 16 — rpmbuild-style --define`

Current head is Phase 16. New phases are **17–25**.

## Progress tracker

| Phase | Theme | Status |
|---|---|---|
| 17 | Metadata / cross-tag consistency (P0) | shipped @ 88e3308 |
| 18 | `FilesClassifier` + first `%files` rules (P0) | shipped @ f681549 |
| 19 | `CommandUseIndex` + scriptlet/install rules (P0) | shipped @ c926936 |
| 20 | `ProfilePolicyRegistry` + systemd/tmpfiles/users (P1) | shipped @ f544e78 |
| 21 | Dependency semantics (P0/P1) | shipped @ 15dffaf |
| 22 | build-tool ↔ BuildRequires + pkgconfig (P1) | shipped @ 9d79989 |
| 23 | Build/install policy (P1) | not started |
| 24 | Conditional builds / macros (P1) | not started |
| 25 | Deep analyses (P2, roadmap) | not started |

Update the status column (`not started` → `in progress` → `shipped @ <commit>`) as each phase lands.

---

## Phase 17 — Metadata / cross-tag consistency (P0, no new infrastructure)

**Goal.** Quick P0 wins on existing primitives: `collect_top_level_preamble` (`rules/util.rs:22`), `DepAtom`/`EVR` walker, changelog AST, MacroDef/BuildCondition nodes.

**Rules.**
- `RPM300 duplicate-singleton-tag` — repeated `Name`/`Version`/`Release`/`License`/`URL`/`Summary`/`Epoch`/`BuildArch`/`AutoReqProv` in one preamble scope. Walk `PreambleItem` per package, group by `Tag`.
- `RPM301 subpackage-name-collision` — `%package devel` vs `%package -n %{name}-devel`. Existing subpackage view in `subpackage_hygiene.rs` — extend.
- `RPM302 invalid-name-version-release-epoch-format` — literal validation of Name/Version/Release/Epoch (whitespace, illegal chars, `Epoch: 0`).
- `RPM304 source-version-mismatch` — hardcoded version in `Source0` does not match `Version`.
- `RPM305 source-patch-list-mixing` — `SourceN:` + `%sourcelist`, `PatchN:` + `%patchlist`.
- `RPM306 patch-applied-more-than-once` — extends existing `patch_tracking.rs` (RPM064).
- `RPM308 autoreqprov-disabled-without-comment` — `AutoReqProv: no` without a neighboring comment. Raw source available via Span.
- `RPM309 buildarch-reparse-hazard` — `%global`/`%define` with `%(...)`/`%{lua:}`/date before `BuildArch`. Walk top-level items.
- `RPM310 arch-policy-contradiction` — `BuildArch: noarch` + `ExclusiveArch`/`ExcludeArch`; intersections.
- `RPM311 changelog-order-weekday-evr` — extends `changelog_health.rs` (RPM037–039). `Weekday`/`ChangelogDate`/EVR normalizer already exist.
- `RPM312 spec-filename-mismatch` — filename ≠ `%{name}.spec`. Needs file path in lint session (already available in CLI `Cmd::run`).

**Files touched.**
- New: `crates/analyzer/src/rules/{duplicate_singleton_tag,subpackage_name_collision,nvre_format,source_version_consistency,source_patch_list,autoreqprov_comment,buildarch_reparse,arch_policy,spec_filename}.rs`.
- Extended: `patch_tracking.rs`, `changelog_health.rs`, `subpackage_hygiene.rs`.
- Registration: `crates/analyzer/src/registry.rs`.
- Filename plumbing: check `analyzer::Session`/`analyze_with_profile`; likely adds `source_path: Option<PathBuf>` field.

**Tests.** Inline `#[cfg(test)]` per rule using the existing pattern: `run(src) -> Vec<Diagnostic>` via `session::parse`.

---

## Phase 18 — `FilesClassifier` + first batch of `%files` rules (P0)

**Goal.** Land the reusable `FilesClassifier` component. Covers the most painful currently-unanalyzed area.

**New component.** `crates/analyzer/src/files/classifier.rs`:
```rust
pub struct FilesClassifier<'a> {
    profile: &'a Profile,
}
pub enum FileKind {
    Config, ConfigNoreplace, License, Doc, Ghost, Lang,
    DevelHeader, DevelPkgConfig, DevelCMakeConfig, UnversionedSoLink,
    Library, Plugin, Binary,
    SystemdUnit { sub: SystemdUnitKind },
    TmpfilesConf, SysusersConf,
    LocaleMo, DebuginfoPath, StandardDir, VolatilePath,
    Other,
}
impl FilesClassifier<'_> {
    pub fn classify(&self, entry: &FileEntry<Span>) -> Classification { ... }
}
```
Uses `Profile::macros.expand_to_literal` to resolve `%{_bindir}`/`%{_libdir}` etc., then matches by path table + suffix glob.

**`%files` directive parsing.** AST already carries `FileEntry::directives: Vec<FileDirective>` (see `Files` section in `rpm-spec/src/ast/section.rs`). No new parsing — just a wrapper over AST.

**Rules (all of 4.4 except those parked for Phase 22).**
- `RPM360 etc-file-not-config`
- `RPM361 config-under-usr`
- `RPM362 plain-config-without-comment`
- `RPM363 license-file-marked-doc`
- `RPM364 devel-file-in-non-devel-package`
- `RPM365 locale-file-not-lang`
- `RPM366 duplicate-files-in-files-sections`
- `RPM367 standard-dir-owned`
- `RPM368 broad-files-glob`
- `RPM369 var-run-var-lock-not-ghost`
- `RPM370 suspicious-attr-permissions`
- `RPM371 debuginfo-path-in-main-files`

**Files.**
- New: `crates/analyzer/src/files/{mod.rs,classifier.rs}`, `crates/analyzer/src/rules/files_*.rs` (per rule or grouped: `files_config.rs`, `files_devel.rs`, `files_systemd_paths.rs`, `files_attr.rs`).
- Registration in `registry.rs`.

**Tests.** Classifier — dedicated unit tests across profiles (Fedora/openSUSE). Rules — inline using the existing pattern.

---

## Phase 19 — `CommandUseIndex` + scriptlet/install baseline rules (P0)

**Goal.** Land `CommandUseIndex` for extracting commands from `ShellBody` (scriptlet/buildscript/trigger). Close P0 rules on scriptlet exit semantics and install boundaries.

**New component.** `crates/analyzer/src/shell/cmd_index.rs`:
```rust
pub struct CommandUse {
    pub name: String,                // "systemctl", "useradd", "rm", "install"
    pub args: Vec<ShellArg>,         // tokens with macro refs preserved
    pub span: Span,
    pub line_idx: usize,
    pub source_section: SectionRef,  // BuildScript(kind) / Scriptlet(kind) / Trigger
}
pub struct CommandUseIndex {
    uses: Vec<CommandUse>,
}
impl CommandUseIndex {
    pub fn from_spec(spec: &SpecFile<Span>) -> Self { ... }
    pub fn find(&self, name: &str) -> impl Iterator<Item = &CommandUse> { ... }
    pub fn in_section(&self, sec: SectionRef) -> impl Iterator<Item = &CommandUse> { ... }
}
```
Line-by-line parsing + naive split honoring quotes (full shell AST is deferred to Phase 25). Macro refs inside arguments are preserved (with `expand_to_literal` where possible).

**Rules (parts of 4.3 and 4.5 from the doc).**
- `RPM340 scriptlet-exit-not-guaranteed-zero` — last-cmd / `set -e` / explicit `exit 1` without guard.
- `RPM341 scriptlet-upgrade-test-eq-two` — regex on scriptlet bodies.
- `RPM342 direct-systemctl-in-scriptlet`
- `RPM349 scriptlet-state-outside-rpm-state`
- `RPM380 install-writes-outside-buildroot` — `install/cp/mkdir/touch/ln` into `/usr|/etc|/var` without `%{buildroot}`/`$RPM_BUILD_ROOT`.
- `RPM381 rm-rf-buildroot-in-install`
- `RPM382 makeinstall-without-underscore`
- `RPM383 make-install-missing-destdir`
- `RPM384 install-chown-or-owner`

**Files.**
- New: `crates/analyzer/src/shell/{mod.rs,cmd_index.rs}`, `crates/analyzer/src/rules/scriptlet_*.rs`, `crates/analyzer/src/rules/install_*.rs`.
- Optional refactor of `rules/shell_vars.rs` to ride on `CommandUseIndex` (to avoid duplicate walking).

**Tests.** `CommandUseIndex` — unit tests on parsing and grouping. Rules — inline.

---

## Phase 20 — `ProfilePolicyRegistry` + systemd/tmpfiles/users (P1)

**Goal.** Extend `Profile` with policy maps for distro-specific semantics. Close remaining scriptlet/systemd rules.

**Profile extension.** `crates/profile/src/policy.rs`:
```rust
pub struct PolicyRegistry {
    pub systemd_macros: SystemdMacros,         // %systemd_post / %service_add_post
    pub scriptlet_required_deps: HashMap<&'static str, RequiredDep>,
    pub standard_dirs: HashSet<String>,        // already-expanded
    pub devel_patterns: Vec<DevelPattern>,
    pub build_tool_to_buildrequires: HashMap<&'static str, &'static str>,
    pub disttag_policy: DistTagPolicy,
    pub config_policy: ConfigPolicy,           // e.g. SUSE: warn `%config` without noreplace
}
```
Populated per family in the existing builtin resolver (`profile/src/builtin.rs` or equivalent). Built on top of `MacroRegistry::expand_to_literal` for paths.

**Rules.**
- `RPM303 release-disttag-policy` — Fedora/RHEL profile-gated.
- `RPM343 systemd-unit-without-helper-macros` (Files+Scriptlet cross-check).
- `RPM344 systemd-unit-under-etc-or-config`.
- `RPM345 repeated-service-macro-calls` (openSUSE-only).
- `RPM346 ldconfig-scriptlet-style` (profile-gated).
- `RPM347 tmpfiles-without-create`.
- `RPM348 unsafe-useradd-groupadd`.

**Files.**
- New: `crates/profile/src/policy.rs` + builtin policy for Fedora/RHEL/openSUSE/ALT/Mageia/Generic.
- Registration in `Profile::resolve`.
- New rules: `crates/analyzer/src/rules/systemd_*.rs`, `scriptlet_systemd.rs`, `tmpfiles_create.rs`, `users_groups.rs`, `release_disttag.rs`, `ldconfig_style.rs`.

**Tests.** Builtin policy unit-tested in the profile crate. Rules — paired profiles (Fedora vs openSUSE) for cross-distro contrast.

---

## Phase 21 — Dependency semantics (P0/P1, no new infrastructure)

**Goal.** Extend the depatom walker — close 4.2.

**Rules.**
- `RPM320 duplicate-dependency-atom`
- `RPM321 weak-dep-duplicates-strong-dep`
- `RPM322 self-weak-dependency`
- `RPM323 runtime-requires-looks-like-build-requires`
- `RPM326 unsupported-dependency-feature` (weak deps before rpm 4.13, `meta` before 4.16, rich deps) — uses `RpmlibFeatures`.
- `RPM327 contradictory-dependency-qualifiers` (`Requires(pre,meta)` etc.).

**Files.** Extension of `requires_equal_version.rs`/`util.rs::collect_atoms` or new `rules/dep_*.rs` modules. Uses `BoolDep` (rich) walker, `EVR` normalizer.

**Tests.** Inline.

---

## Phase 22 — build-tool ↔ BuildRequires + pkgconfig (P1, depends on Phases 19+20)

**Goal.** Wire `CommandUseIndex` + `FilesClassifier` + `ProfilePolicyRegistry` for cross-section rules.

**Rules.**
- `RPM324 build-tool-used-without-buildrequires` (CommandUseIndex(%build/%install/%check) ∩ policy.build_tool_to_buildrequires).
- `RPM325 pkgconfig-file-without-pkgconfig-br` (Classifier + BuildRequires walker).
- `RPM328 scriptlet-command-without-requires` (CommandUseIndex(scriptlet) ∩ policy.scriptlet_required_deps).
- `RPM307 patch-status-comment-missing` (openSUSE-only, raw source).

**Files.** `rules/build_tool_brs.rs`, `rules/pkgconfig_br.rs`, `rules/scriptlet_deps.rs`, `rules/patch_status_comment.rs`.

---

## Phase 23 — Build/install policy (P1)

**Goal.** Close remaining 4.5.

**Rules.**
- `RPM385 optflags-overridden`
- `RPM386 werror-not-disabled`
- `RPM387 j1-without-comment`
- `RPM388 network-access-in-build` (profile-gated)
- `RPM389 disabled-check-section`
- `RPM390 buildsystem-macro-modernization` — `%cmake/%meson/%pyproject_*/%cargo_*/%gobuild` suggestions, via `MacroRegistry` (macro presence in the active profile).

**Files.** `rules/optflags.rs`, `rules/werror.rs`, `rules/parallel_make.rs`, `rules/network_in_build.rs`, `rules/disabled_check.rs`, `rules/buildsystem_macros.rs`.

---

## Phase 24 — Conditional builds / macros (4.6, P1)

**Rules.**
- `RPM400 prefer-bcond-new-syntax` (rpm ≥ 4.17.1 profile-gated).
- `RPM401 bcond-defined-but-unused`
- `RPM402 with-condition-without-bcond`
- `RPM403 test-without-counterpart`
- `RPM404 macro-shell-expansion-in-metadata`
- `RPM405 unresolved-nonbuiltin-macro` (conservative, with allowlist of rpm-dynamic macros)
- `RPM406 include-not-expanded`

**Files.** `rules/bcond_modern.rs`, `rules/bcond_usage.rs`, `rules/with_idiom_mixing.rs`, `rules/metadata_shell_macro.rs`, `rules/unresolved_macro.rs`, `rules/include_notice.rs`.

---

## Phase 25 — Deep analyses (P2, separate roadmap)

Each item is its own PR (or PR series). Opt-in via a `[lints.deep]` config flag because of potentially higher cost / FP risk.

1. **Symbolic effective spec evaluator** (5.1) — build a per-profile effective view of the spec; reuse the `path_cond.rs` DNF engine.
2. **Stronger macro abstract interpreter** (5.2) — guarded recursive expansion with fuel, purity classification, taint for `%()`/`%{lua:}`.
3. **Shell/scriptlet CFG analyzer** (5.3) — on top of `CommandUseIndex` add tree-sitter-bash or a mini-CFG with an exit-status lattice. Promotes RPM340 from heuristic to real analysis.
4. **`%install` write-set vs `%files` read-set** (5.4) — derive the set of paths produced by `%install` and compare against `%files`. Uses `CommandUseIndex` + `FilesClassifier`.
5. **PRCO graph solver** (5.6) — normalized capability domain + EVR comparator for cross-subpackage invariants.
6. **SMT-lite domains for `%if`** (5.7) — integer/arch/string/EVR domains on top of the existing DNF.
7. **Declarative build migration** (`RPM391 declarative-build-candidate`, 5.8) — rpm ≥ 4.20 profile-gated.
8. **Repository-level analyzer** (5.9) — multi-spec mode in the CLI.

Independence varies; the ordering inside Phase 25 is a separate discussion after Phase 24 ships.

---

## Reusable components — summary

| Component | Introduced in | Used in | Lives in |
|---|---|---|---|
| `FilesClassifier` | 18 | 18, 20, 22 | `crates/analyzer/src/files/` |
| `CommandUseIndex` | 19 | 19, 20, 22, 23, 25.3, 25.4 | `crates/analyzer/src/shell/` |
| `PolicyRegistry` (extends `Profile`) | 20 | 20, 22, 23, 24 | `crates/profile/src/policy.rs` |
| Spec path in Session | 17 | 17 (RPM312) | `crates/analyzer/src/session.rs` (or Lint API) |

---

## Verification (end-to-end)

Per phase:
1. **Rule unit tests** — inline `#[cfg(test)]`, pattern `run(src) -> Vec<Diagnostic>` via `session::parse` (see existing modules).
2. **Workspace lint/format** — `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
3. **CLI smoke test on a real profile** — add `tests/integration/*.spec` fixtures with snapshot output under `cargo test -p cli`. At least 2 profiles (Fedora vs openSUSE) for profile-gated rules.
4. **Rule documentation** — update `doc/lints.md` (if present) or the table-of-rules comment in `crates/analyzer/src/registry.rs`. Update this file's progress tracker.
5. **Profile check** — after Phase 20, `cargo run -p cli -- profile show --family fedora` should display the new policy sections.

## Out of scope

- Rules requiring a real buildroot / unpacked source / binary payload (SONAME bump, ELF RPATH, "URL actually downloads"). Section 7 of `extendend-lints.md` excludes these explicitly.
- Auto-fixes (`--fix`) for new rules — separate roadmap; diagnostics only here.
- CLI/config changes beyond the minimum needed (`source_path` field for RPM312, optional `[lints.deep]` flag for Phase 25).
