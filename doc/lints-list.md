   Compiling rpm-spec-profile v0.1.1 (/home/evgenii/experiments/rpm-spec-tool/crates/profile)
   Compiling rpm-spec-repo-core v0.1.1 (/home/evgenii/experiments/rpm-spec-tool/crates/repo-core)
   Compiling rpm-spec-repo-resolver v0.1.1 (/home/evgenii/experiments/rpm-spec-tool/crates/repo-resolver)
   Compiling rpm-spec-repo-metadata v0.1.1 (/home/evgenii/experiments/rpm-spec-tool/crates/repo-metadata)
   Compiling rpm-spec-analyzer v0.1.1 (/home/evgenii/experiments/rpm-spec-tool/crates/analyzer)
   Compiling rpm-spec-tool v0.1.1 (/home/evgenii/experiments/rpm-spec-tool/crates/cli)
    Finished `release` profile [optimized] target(s) in 1m 07s
     Running `target/release/rpm-spec-tool lints --format markdown`
# Lint rules reference

## Correctness

| ID | Name | Severity | Description |
|----|------|----------|-------------|
| RPM031 | `requires-equal-version` | warn | Requires with `=` operator pinned to a full version-release blocks compatible rebuilds. |
| RPM032 | `macro-redefinition` | warn | A macro is redefined at the same scope; the earlier definition is dead code. Definitions in alternative %if/%else branches are not redefinitions and are ignored. |
| RPM033 | `self-obsoletion` | deny | A package declares an Obsoletes entry naming itself, which prevents upgrades. |
| RPM034 | `obsolete-without-provides` | warn | Each unconstrained Obsoletes entry should be matched by a Provides of the same name to keep upgrades smooth. |
| RPM035 | `useless-explicit-provides` | warn | Explicit `Provides:` of the package's own name is redundant with rpm's auto-provides. |
| RPM036 | `macro-in-hash-comment` | warn | `#` comments expand macros — escape each `%` to `%%` or use `%dnl` for a no-expand comment. |
| RPM037 | `empty-changelog-entry` | warn | Changelog entry has no body text — likely a leftover header. |
| RPM038 | `changelog-future-date` | warn | Changelog entry is dated in the future. |
| RPM039 | `changelog-implausible-date` | warn | Changelog entry date has an impossible day or year. |
| RPM040 | `self-conflict` | deny | A package declares a Conflicts entry naming itself, which blocks installation. |
| RPM064 | `patch-defined-not-applied` | warn | `PatchN:` is declared but never applied in `%prep`; declare or apply, don't dangle. |
| RPM071 | `unreachable-elif-branch` | warn | `%elif` with the same expression as an earlier branch can never fire; likely a typo. |
| RPM077 | `ifarch-empty-list` | warn | `%ifarch`/`%ifos` with no architecture tokens is always false; likely a missing argument. |
| RPM090 | `ifarch-noarch` | warn | `%ifarch noarch` is suspicious — `noarch` is a build marker, not an architecture. |
| RPM094 | `line-continuation-in-condition` | warn | `%if` expression spans multiple lines via `\\` — RPM doesn't support continuation here. |
| RPM103 | `inequality-contradiction` | warn | `&&`-chain has incompatible inequalities — the whole guard is always false. |
| RPM106 | `conditional-buildarch` | allow | `BuildArch:` inside a `%if` block — RPM uses last-wins semantics, so this is fragile. |
| RPM107 | `conditional-name-tag` | allow | `Name:` inside a `%if` block — the package will have different names in different build contexts, which confuses downstream tooling. |
| RPM112 | `boolean-contradiction-by-cubes` | warn | Boolean expression is unsatisfiable — every cube collapses to internal contradiction. |
| RPM113 | `unreachable-branch-under-parent` | warn | `%if` branch is unsatisfiable under the conjunction of ancestor conditions; its body can never execute. |
| RPM115 | `dead-elif-after-parent` | warn | `%elif` branch is unsatisfiable under the ancestor path-condition combined with negations of preceding sibling branches; its body is dead. |
| RPM123 | `package-without-description` | allow | A `%package` subpackage was declared but no matching `%description` exists; rpmbuild will reject the build. |
| RPM124 | `package-without-files` | allow | A `%package` subpackage was declared but has no matching `%files` section; no payload will be assembled for it. |
| RPM200 | `shellcheck` | warn | Run shellcheck over %prep/%build/%install and scriptlet/trigger bodies; surface findings as diagnostics. |
| RPM300 | `duplicate-singleton-tag` | warn | A singleton preamble tag (Name, Version, Release, License, URL, Summary, Epoch, BuildArch, AutoReq/AutoProv/AutoReqProv) appears more than once in the same package scope; RPM keeps the last value and the earlier line is dead code. |
| RPM301 | `subpackage-name-collision` | deny | Two `%package` blocks (or `%package` and the main package) resolve to the same canonical name; RPM keeps the last block and the earlier one's `%files` / `%description` becomes dead code. |
| RPM302 | `invalid-name-version-release-epoch-format` | deny | Name/Version/Release contains characters RPM does not accept, or Epoch is literally `0` (the default — drop the tag instead). |
| RPM304 | `source-version-mismatch` | warn | A `SourceN:` URL contains a hard-coded version different from `Version:`. After a version bump this points at the old upstream archive. |
| RPM306 | `patch-applied-more-than-once` | warn | A patch appears to be applied twice in `%prep` — either by two explicit `%patch -P N` / `%patchN` invocations, or by mixing one of those with `%autopatch` / `%autosetup` which applies the patch implicitly. |
| RPM308 | `autoreqprov-disabled-without-comment` | warn | `AutoReqProv` / `AutoReq` / `AutoProv` is set to `no` without a neighbouring comment. Disabling RPM's auto-dependency generation is unusual and almost always needs justification. |
| RPM309 | `buildarch-reparse-hazard` | warn | A `%global` / `%define` with `%(...)` shell or `%{lua:...}` side-effects appears before `BuildArch:`. RPM re-parses the spec at `BuildArch:`, so the side effect runs twice and may yield different values. Move the definition below `BuildArch:` or remove the side effect. |
| RPM310 | `arch-policy-contradiction` | warn | `BuildArch: noarch` is combined with `ExclusiveArch`/`ExcludeArch`, or `ExclusiveArch` and `ExcludeArch` list overlapping architectures. |
| RPM311 | `changelog-order-weekday-evr` | warn | The `%changelog` is not ordered newest-first, contains a weekday that does not match the date, or its latest entry's EVR does not match the spec's `Version-Release`. |
| RPM322 | `self-weak-dependency` | warn | A weak dependency names the package itself. RPM treats self-dependencies as no-ops; the entry is almost always copy-paste from another spec. |
| RPM323 | `runtime-requires-looks-like-build-requires` | warn | `Requires:` mentions a build-only tool (`gcc`, `cmake`, a `*-devel` package, a `pkgconfig(...)` capability, …). Move it to `BuildRequires:`. |
| RPM324 | `build-tool-used-without-buildrequires` | warn | A build script invokes a tool (`cmake`, `meson`, `pkg-config`, ...) without a matching `BuildRequires:`. Clean-chroot builds will fail with command-not-found. |
| RPM326 | `unsupported-dependency-feature` | deny | The spec uses a dependency feature (rich/boolean deps, weak deps, `Requires(meta)` qualifier) that the active profile's rpm does not advertise via `rpmlib(...)`. Builds will fail on that target. |
| RPM327 | `contradictory-dependency-qualifiers` | deny | `Requires(meta, …)` combines the `meta` qualifier with ordered phase qualifiers (`pre`/`post`/`preun`/`postun`/`pretrans`/`posttrans`). The pair is contradictory; rpm silently keeps one. |
| RPM328 | `scriptlet-command-without-requires` | warn | A scriptlet invokes a runtime helper (`useradd`, `getent`, `update-alternatives`, ...) without declaring the providing package in `Requires:`. Minimal images abort the scriptlet with command-not-found. |
| RPM340 | `scriptlet-exit-not-guaranteed-zero` | warn | A scriptlet's last command can fail with no explicit exit guard. RPM aborts the transaction on non-zero exit, leaving the system half-installed. Add `\|\| :` / `\|\| true` / `exit 0`, or use `set +e`. |
| RPM341 | `scriptlet-upgrade-test-eq-two` | warn | Scriptlet compares the install count `$1` to exactly `2` to detect an upgrade. Multilib and error recovery can push `$1` above `2`; use `[ $1 -gt 1 ]` instead. |
| RPM348 | `unsafe-useradd-groupadd` | warn | Scriptlet creates a user/group without a `getent … \|\| …` idempotency guard. Re-installs fail noisily and partial transactions strand state. |
| RPM349 | `scriptlet-state-outside-rpm-state` | warn | Scriptlet writes scratch state under `/tmp` or `/var/tmp`. Those races with parallel transactions and leak on abort; use `$RPM_STATE_DIR` or `/var/lib/rpm-state/<pkg>` instead. |
| RPM380 | `install-writes-outside-buildroot` | deny | An `%install` step writes to a real system path (e.g. `/usr/bin`, `/etc`) without `%{buildroot}` / `$RPM_BUILD_ROOT`. Stage everything under the buildroot so RPM packages exactly what `%install` produced. |
| RPM383 | `make-install-missing-destdir` | deny | `make install` in `%install` without `DESTDIR=%{buildroot}` (or `$RPM_BUILD_ROOT`) installs onto the build host. Use `%make_install`, or pass `DESTDIR=` explicitly. |
| RPM384 | `install-chown-or-owner` | warn | `%install` invokes `chown`/`chgrp` or `install -o`/`install -g`. `%install` runs unprivileged; ownership belongs in `%files` via `%attr(...)`. |
| RPM385 | `optflags-overridden` | warn | Build script assigns `CFLAGS=`/`CXXFLAGS=`/`LDFLAGS=`/`FFLAGS=` without preserving `%{optflags}` (or `$RPM_OPT_FLAGS`). The override drops the distro's hardening flags (FORTIFY_SOURCE, PIE, RELRO, stack-protector). |
| RPM386 | `werror-not-disabled` | warn | Build script passes `-Werror` / `--enable-werror`. New compiler versions add warnings that then break the build; disable `-Werror` for downstream packaging. |
| RPM388 | `network-access-in-build` | warn | Build script invokes a network-fetching command (`curl`, `wget`, `git clone`, `pip install`, …). Mock/Koji/OBS run the build in an offline chroot — the fetch will fail. |
| RPM389 | `disabled-check-section` | warn | `%check` is present but contains no executable statements — only blank lines and comments. Silently disabling the test suite masks regressions; either remove `%check` entirely or restore the test invocation. |
| RPM402 | `with-condition-without-bcond` | warn | A `%{with name}` or `%{without name}` reference has no matching `%bcond` declaration. RPM expands the reference to nothing, so the conditional silently never fires — declare the bcond or fix the typo. |
| RPM404 | `macro-shell-expansion-in-metadata` | warn | An identity tag (`Version`, `Release`, `Source*`, `URL`, …) carries `%(shell)` or `%{lua:...}`. The expansion is evaluated at build time, so the resulting NVR / SRPM filename / changelog changes between builds — reproducibility breaks. |
| RPM450 | `guarded-item-already-unconditional` | warn | A dependency atom appears both unconditionally and inside an `%if` block of the same tag; the conditional copy is dead — drop it. |
| RPM451 | `guarded-item-dominated-by-weaker-guard` | warn | A guarded dependency atom is dominated by another copy of the same atom under a strictly weaker guard — drop the redundant stronger-guarded copy. |
| RPM452 | `complementary-guards-same-item` | warn | Multiple guarded copies of the same dependency atom collectively cover every truth assignment; the atom is effectively unconditional — merge them. |
| RPM453 | `full-domain-conditional-item` | warn | Multiple `%ifarch`-guarded copies of the same dependency atom together cover every arch in the profile's target universe — make the entry unconditional. |
| RPM591 | `richdep-idempotent` | warn | Rich dependency expression repeats an operand inside an `and` / `or` group (`(foo and foo)` / `(foo or foo)`); drop the duplicate. |
| RPM592 | `richdep-absorption` | warn | Rich dep absorbs an inner subterm — `A or (A and B)` reduces to `A`; `A and (A or B)` reduces to `A`. |
| RPM594 | `richdep-same-then-else` | warn | Rich-dep `(X if C else X)` (or `unless`) — both arms pick the same expression, drop the conditional. |
| RPM596 | `dependency-constraint-subsumption` | warn | Unversioned dep atom is subsumed by a versioned one for the same name; drop the unversioned line. |
| RPM597 | `guarded-dependency-constraint-subsumption` | warn | A guarded `Requires:` (or similar) is dominated by an unconditional, versioned requirement on the same name — drop the guarded copy. |
| RPM-REPO-001 | `buildrequires-unresolvable` | deny | A `BuildRequires:` atom has no provider in any configured repo for the active profile. Clean-chroot builds will fail with "nothing provides ...". |
| RPM-REPO-002 | `runtime-requires-unresolvable` | warn | A `Requires:` atom has no provider in any configured repo for the active profile. The package will build but won't install on a target with only these repos. |
| RPM-REPO-003 | `buildrequires-version-unsatisfied` | warn | A `BuildRequires:` atom names a package that exists in the configured repos, but the version constraint is not met by any available release. |
| RPM-REPO-010 | `missing-buildrequires-for-command` | warn | A build-script section invokes a bare command (e.g. `cmake`, `meson`, `cargo`) whose owning package in the configured repo is not declared in `BuildRequires:`. Clean-chroot builds will fail with `command not found`. |
| RPM-REPO-011 | `missing-buildrequires-for-file` | warn | A build-script section invokes a tool by absolute path (e.g. `/usr/bin/xsltproc`) whose owning package is not declared in `BuildRequires:`. Clean-chroot builds will fail with the tool missing. |
| RPM-REPO-020 | `file-conflict-with-existing-package` | warn | A path listed in `%files` is already owned by another package in the configured repos. `dnf install` rejects file collisions; either rename the file, add an `Obsoletes:` for the conflicting package, or mark it `%ghost`. |
| RPM-REPO-030 | `new-evr-not-greater-than-repo` | deny | The spec's EVR is not strictly greater than the highest binary already published in the configured repos. Releases would silently regress for consumers running `dnf upgrade`. |
| RPM-REPO-031 | `epoch-dropped` | deny | The spec omits an `Epoch:` value that the currently published binary carries. Dropping epoch silently demotes the package (rpm treats absent epoch as 0) and breaks `dnf upgrade`. |

## Packaging

| ID | Name | Severity | Description |
|----|------|----------|-------------|
| RPM001 | `missing-changelog` | warn | Every spec file should declare a %changelog section. |
| RPM010 | `missing-name-tag` | deny | Spec file must declare a top-level Name: tag. |
| RPM011 | `missing-version-tag` | deny | Spec file must declare a top-level Version: tag. |
| RPM012 | `missing-release-tag` | deny | Spec file must declare a top-level Release: tag. |
| RPM013 | `missing-license-tag` | deny | Spec file must declare a top-level License: tag. |
| RPM014 | `missing-summary-tag` | deny | Spec file must declare a top-level Summary: tag. |
| RPM015 | `missing-url-tag` | warn | Spec file should declare a top-level URL: tag. |
| RPM016 | `missing-prep-section` | warn | Spec should declare a %prep section to unpack and patch sources. |
| RPM017 | `missing-build-section` | warn | Spec should declare a %build section to compile sources. |
| RPM018 | `missing-install-section` | warn | Spec should declare an %install section to place files into the buildroot. |
| RPM020 | `obsolete-tag` | warn | Preamble uses a tag that's deprecated or forbidden by modern packaging guidelines. |
| RPM021 | `deprecated-clean-section` | warn | The %clean section is unnecessary; modern rpm cleans the buildroot automatically. |
| RPM022 | `multiple-changelog-sections` | deny | Spec file declares more than one top-level %changelog section. rpm processes only the first one and silently drops the rest. Note: %changelog blocks nested inside %if/%endif are ignored on purpose — they're rare and usually intentional cross-distro patterns. |
| RPM023 | `duplicate-buildscript-section` | deny | Spec declares the same build-script section (%prep/%build/%install/...) more than once. |
| RPM024 | `invalid-license` | warn | License: must name a license from the profile's allow-list. |
| RPM025 | `non-standard-group` | warn | Group: must name a group from the profile's allow-list. |
| RPM127 | `legacy-license-syntax` | warn | Fedora ≥ 40 mandates SPDX-only license identifiers; legacy short forms (`GPLv2+`, `BSD`, …) are no longer accepted. |
| RPM128 | `group-tag-required-on-suse` | warn | openSUSE/SLES Specfile Guidelines require every package to declare a Group: tag. |
| RPM129 | `bcond-on-non-fedora` | warn | `%bcond_with` / `%bcond_without` are Fedora/RHEL-specific build-option macros; use `%define NAME 1` + plain `%if` on other distros. |
| RPM303 | `release-disttag-policy` | warn | `Release:` should reference `%{?dist}` and not hard-code a per-distro suffix (`.fc40`, `.el9`, ...). Family-gated to Fedora-derived distros. |
| RPM312 | `spec-filename-mismatch` | warn | Spec file name differs from `<Name>.spec` — most RPM tooling pairs specs to package names by filename. |
| RPM325 | `pkgconfig-file-without-pkgconfig-br` | warn | `%files` ships a `.pc` file but `BuildRequires:` lacks `pkgconfig`. Without the BR, rpm's `pkgconfig(...)` provides generator does not run; downstream `-devel` consumers can't find the capability. |
| RPM342 | `direct-systemctl-in-scriptlet` | warn | A scriptlet invokes `systemctl` directly. Use the distro-provided helpers (`%systemd_post` / `%service_add_post` etc.) so the unit lifecycle is managed by macros that handle non-systemd targets, chroots, and image builds. |
| RPM343 | `systemd-unit-without-helper-macros` | warn | `%files` ships a systemd unit (`.service`/`.socket`/...), but no scriptlet invokes the distro's lifecycle helper macros (`%systemd_*` / `%service_*`). The unit is packaged but not registered. |
| RPM344 | `systemd-unit-under-etc-or-config` | warn | A systemd unit is installed under `/etc/systemd/system` or carries `%config`. Unit files belong in `%{_unitdir}` (typically `/usr/lib/systemd/system`) and should not be `%config`. |
| RPM347 | `tmpfiles-without-create` | warn | `%files` includes a `tmpfiles.d/*.conf` drop-in but no scriptlet runs the distro's `%tmpfiles_create*` macro. The directories described by the drop-in won't exist until the next reboot. |
| RPM360 | `etc-file-not-config` | warn | A file under `/etc` (or `%{_sysconfdir}`) is listed without `%config`. RPM will overwrite local edits on every upgrade — mark it as `%config(noreplace)`. |
| RPM361 | `config-under-usr` | warn | `%config` is applied to a path under `/usr`. The FHS treats `/usr` as read-only — configuration belongs in `/etc`. |
| RPM362 | `plain-config-without-comment` | warn | `%config` without `noreplace` is risky — on upgrade rpm may overwrite local edits with the package default. Either switch to `%config(noreplace)` or leave a comment explaining why plain `%config` is intended. |
| RPM363 | `license-file-marked-doc` | warn | A file whose basename looks like a license (`LICENSE`, `COPYING`, `NOTICE`, …) is marked `%doc` instead of `%license`. `%license` survives `rpm --excludedocs` and is recognised by compliance tooling. |
| RPM364 | `devel-file-in-non-devel-package` | warn | A development artifact (`.h`, `.pc`, CMake config, unversioned `.so`) is shipped in a non-`-devel` package. Move it to a `-devel` subpackage so runtime installs do not drag in the development ecosystem. |
| RPM365 | `locale-file-not-lang` | warn | A `.mo` translation under `/usr/share/locale/` is listed manually without `%lang(...)`. Prefer `%find_lang` in `%install` + `%files -f <name>.lang`, or annotate the entry with `%lang(<code>)`. |
| RPM366 | `duplicate-files-in-files-sections` | warn | The same normalised path appears in `%files` more than once. Within one package it is dead packaging; across subpackages it produces a true file conflict at install time. |
| RPM367 | `standard-dir-owned` | warn | A `%files` entry owns a standard directory (e.g. `%{_bindir}`, `%{_datadir}`) outright. Standard directories belong to `filesystem` (or the distro equivalent); list a package-specific sub-path instead. |
| RPM368 | `broad-files-glob` | warn | A `%files` entry uses a broad glob (`%{_datadir}/*`, `%{_libdir}/*`, …). Such globs hide newly added or misnamed files between upstream releases — list a package-specific subdirectory instead. |
| RPM369 | `var-run-var-lock-not-ghost` | warn | A file under `/var/run`, `/run`, or `/var/lock` is listed without `%ghost`. Those directories are volatile (tmpfs); package the entry as `%ghost` and recreate it with `tmpfiles.d`. |
| RPM370 | `suspicious-attr-permissions` | warn | `%attr(...)` grants suspicious permissions: world-writable, setuid/setgid, or 777 on a regular file. |
| RPM371 | `debuginfo-path-in-main-files` | deny | A `%files` entry points at `/usr/lib/debug` or a `.build-id`/`.debug` path. Those are owned by the auto-generated `-debuginfo` subpackage; remove the manual entry to avoid install-time file conflicts. |

## Style

| ID | Name | Severity | Description |
|----|------|----------|-------------|
| RPM002 | `empty-description` | warn | %description bodies should not be empty. |
| RPM050 | `hardcoded-paths` | warn | Use the matching RPM macro instead of a hardcoded path (e.g. `%{_bindir}` for `/usr/bin`). |
| RPM051 | `tab-indent` | warn | Lines indented with tabs make alignment fragile; use spaces instead. |
| RPM052 | `trailing-whitespace` | allow | Trailing whitespace clutters diffs and serves no purpose. |
| RPM053 | `rpm-buildroot-shell-var` | warn | Use `%{buildroot}` instead of the legacy `$RPM_BUILD_ROOT` environment variable. |
| RPM054 | `rpm-source-dir-shell-var` | warn | Use `%{_sourcedir}` instead of the legacy `$RPM_SOURCE_DIR` environment variable. |
| RPM055 | `summary-ends-with-dot` | warn | Summary should not end with a period. |
| RPM056 | `summary-not-capitalized` | warn | Summary should start with an uppercase letter. |
| RPM057 | `summary-too-long` | warn | Summary is longer than the recommended maximum (80 chars). |
| RPM058 | `name-in-summary` | allow | Package name should not appear in its own Summary. |
| RPM059 | `description-shorter-than-summary` | allow | Main package %description is shorter than its Summary — looks like a placeholder. Subpackage descriptions are not checked yet. |
| RPM060 | `python-setup-test-deprecated` | allow | Replace `python setup.py test` with a modern test runner (pytest / tox / nox). |
| RPM061 | `python-setup-install-deprecated` | allow | Replace `python setup.py install` with `pip install` / `%py3_install` / PEP 517 builder. |
| RPM062 | `egrep-fgrep-deprecated` | warn | Use `grep -E` / `grep -F` instead of the deprecated `egrep` / `fgrep`. |
| RPM063 | `setup-without-q-flag` | warn | `%setup` should always be invoked with `-q` to silence tarball extraction noise. |
| RPM070 | `deep-conditional-nesting` | warn | Conditional nesting beyond 4 levels is hard to read; refactor or split. |
| RPM072 | `constant-condition` | warn | `%if 0` / `%if 1` has a fixed outcome; drop the block or simplify to the live branch. |
| RPM073 | `empty-conditional-branch` | warn | Conditional block has no real content in any branch — drop the block. |
| RPM074 | `identical-conditional-branches` | warn | Every branch of this conditional has the same body — the block is a no-op. |
| RPM075 | `redundant-nested-condition` | warn | Inner `%if` repeats an enclosing `%if`'s condition; the inner test always passes. |
| RPM076 | `adjacent-mergeable-conditionals` | warn | Two adjacent `%if` blocks share the same condition; merge them into one block. |
| RPM080 | `nested-and-collapse` | warn | Two single-branch `%if` blocks nested directly can be merged into one with `&&`. |
| RPM081 | `empty-else-drop` | warn | `%else` clause has no content; drop the empty `%else`. |
| RPM082 | `invert-empty-if-arch` | warn | `%ifarch X %else FOO %endif` — empty `%if` branch with content in `%else`; flip kind. |
| RPM083 | `collapse-elif-into-else` | warn | Final `%elif` with a constant-true expression is equivalent to `%else`. |
| RPM084 | `if-not-x-after-if-x` | warn | Two adjacent `%ifarch X` / `%ifnarch X` blocks form a perfect `%else` — fold them. |
| RPM085 | `constant-tautology-in-expr` | warn | Expression contains a constant operand (`\|\| 1`, `&& 0`, …) that fixes the result. |
| RPM086 | `idempotent-in-expr` | warn | `X && X` / `X \|\| X` repeats an operand — drop the duplicate. |
| RPM087 | `double-negation-in-expr` | warn | Double negation (`!!`) in `%if` expression — drop it. |
| RPM088 | `self-comparison-in-expr` | warn | Comparison of an operand with itself has a fixed outcome. |
| RPM089 | `single-comment-only-branch` | warn | Conditional branch contains only a comment — likely a TODO left after a refactor. |
| RPM091 | `duplicate-arch-in-list` | warn | Duplicate token in `%ifarch`/`%ifos` list — drop the redundant one. |
| RPM092 | `conditional-cyclomatic-complexity` | warn | Section contains more conditional branches than is comfortable to follow; refactor. |
| RPM093 | `condition-mentioned-many-times` | warn | Same `%if` expression appears many times across the spec; consider factoring it into a `%global` flag. |
| RPM095 | `prefer-bcond-for-build-options` | allow | `%if 0%{?with_NAME}` pattern is the build-option idiom; use `%bcond_with NAME` instead. |
| RPM096 | `if-only-buildrequires` | allow | `%if X BuildRequires: foo %endif` is stylistically heavy; consider `%bcond_with` or a conditional dependency clause. |
| RPM097 | `hoist-common-prefix-from-branches` | warn | All branches of this conditional start with the same item(s); lift them above the block. |
| RPM098 | `hoist-common-suffix-from-branches` | warn | All branches of this conditional end with the same item(s); lift them below the block. |
| RPM099 | `merge-elif-same-body` | warn | Two adjacent `%elif` branches share the same body — combine their conditions via `\|\|`. |
| RPM100 | `collapse-else-if-into-elif` | warn | `%else` containing a single `%if` block can be folded into an `%elif` — drops one nesting level. |
| RPM101 | `absorption-in-expr` | warn | Boolean absorption: `A \|\| (A && B)` reduces to `A`; `A && (A \|\| B)` reduces to `A`. |
| RPM102 | `inequality-redundancy` | warn | `X OP a && X OP b` where one constraint subsumes the other — drop the weaker side. |
| RPM104 | `string-set-redundancy` | warn | `X == "a" \|\| X == "a"` repeats the same string in an `\|\|`-chain — drop the duplicate. |
| RPM105 | `inverted-if-else` | warn | `%if !X foo %else bar %endif` reads more naturally when the negation is removed and the branches are swapped. |
| RPM110 | `boolean-dnf-redundancy` | warn | Expression contains operands that are absorbed by others — DNF normalisation reveals shorter equivalent form. |
| RPM111 | `boolean-tautology-by-cubes` | warn | Boolean expression is tautologically true under every assignment — drop the guard. |
| RPM114 | `always-true-branch-under-parent` | warn | `%if` branch is implied by the enclosing path-condition; the test is redundant and the body always runs. |
| RPM116 | `mutex-branches-spell-out-else` | warn | `%if`/`%elif` chain exhausts the path-condition space yet lacks an explicit `%else`; rewriting the last `%elif` as `%else` makes the chain clearer. |
| RPM117 | `macro-defined-makes-if-trivial` | allow | After substituting macro values defined earlier in the spec, the `%if` expression reduces to a constant; the test is redundant. |
| RPM118 | `unused-conditional-global` | allow | `%global` macro is defined but never read elsewhere in the spec — may indicate a leftover or unintended dead code. |
| RPM119 | `common-leaf-line-hoistable` | warn | A line appears on every root-to-leaf path of a nested `%if` tree — it can be hoisted outside the conditional to remove redundant duplication. |
| RPM120 | `make-without-make-build` | warn | Use `%make_build` instead of bare `make …` / `make %{?_smp_mflags} …` so that parallelism and build flags follow distro convention. |
| RPM121 | `make-install-without-make-install` | warn | Use `%make_install` instead of `make install …`; the macro sets `DESTDIR`, `INSTALL` paths and other distro conventions automatically. |
| RPM122 | `configure-without-configure-macro` | warn | Use `%configure` instead of plain `./configure` / `../configure`; the macro supplies `--prefix`, `--libdir`, hardening flags and other distro defaults. |
| RPM125 | `source-without-url` | warn | `SourceN:` should be a URL (http/https/ftp) where the upstream tarball can be downloaded — Fedora packaging guideline. |
| RPM126 | `description-leads-with-this-package` | allow | `%description` body begins with `This package …` / `The X package …` — Fedora style guide prefers leading with the subject of the description. |
| RPM305 | `source-patch-list-mixing` | warn | A spec mixes `SourceN:` tags with `%sourcelist` (or `PatchN:` with `%patchlist`). Use one form consistently. |
| RPM307 | `patch-status-comment-missing` | warn | openSUSE: every `Patch:` tag should be preceded by a comment carrying a status marker (`PATCH-FIX-UPSTREAM`, `PATCH-FIX-OPENSUSE`, `PATCH-FEATURE-*`, `PATCH-NEEDS-*`, ...). Without it reviewers can't tell whether the patch is upstream-bound. |
| RPM320 | `duplicate-dependency-atom` | warn | Same dependency atom appears more than once inside one tag's value list. RPM keeps one and ignores the rest; remove the duplicate. |
| RPM321 | `weak-dep-duplicates-strong-dep` | warn | A weak dependency (Recommends/Suggests/Supplements/Enhances) names a package already covered by a strong `Requires:`. The weak entry is dead weight. |
| RPM346 | `ldconfig-scriptlet-style` | warn | Library package runs `/sbin/ldconfig` from a shell-bodied scriptlet; use the `%post -p /sbin/ldconfig` interpreter shorthand, or drop the call entirely on file-trigger-aware distros. |
| RPM381 | `rm-rf-buildroot-in-install` | warn | `%install` begins with `rm -rf %{buildroot}` / `$RPM_BUILD_ROOT`. Modern RPM (≥ 4.6) cleans the buildroot for you; the manual rm is at best dead, at worst dangerous if `%{buildroot}` resolves to an unexpected path. |
| RPM382 | `makeinstall-without-underscore` | warn | `%makeinstall` is the legacy hard-coded form; prefer `%make_install` (which sets `DESTDIR=%{buildroot}`). |
| RPM387 | `j1-without-comment` | warn | Build script forces serial make (`make -j1`) with no comment explaining why. `-j1` is often leftover debug or an obsolete workaround for an upstream race; add a comment so reviewers can tell intentional from accidental. |
| RPM390 | `buildsystem-macro-modernization` | warn | Build script invokes a build system directly (`cmake`, `meson`, `cargo`, …) instead of the distro's wrapper macro (`%cmake`, `%meson`, `%cargo_build`, …). The wrappers plumb `%{optflags}` and per-arch defaults; bare calls drop them. |
| RPM400 | `prefer-bcond-new-syntax` | warn | `%bcond_with NAME` / `%bcond_without NAME` are pre-rpm-4.17 declarations. The modern `%bcond NAME DEFAULT` form makes the polarity explicit and is preferred on profiles that ship rpm ≥ 4.17.1. |
| RPM401 | `bcond-defined-but-unused` | warn | A `%bcond` / `%bcond_with` / `%bcond_without` declaration has no matching `%{with name}` / `%{without name}` reference anywhere in the spec. The toggle has no effect; remove it or wire it up. |
| RPM406 | `include-not-expanded` | allow | `%include path` directive — the analyzer does not follow includes, so other rules see only the visible spec. Findings may be incomplete for symbols defined inside the included file. |
| RPM430 | `context-redundant-condition-part` | warn | `%if` expression contains a conjunct already implied by the enclosing path — drop the redundant operand. |
| RPM431 | `elif-history-simplify` | warn | `%elif` expression repeats a fact already implied by prior branches' negations or by the enclosing path — drop the redundant operand. |
| RPM432 | `condition-common-factor` | allow | `%if` expression's normalised DNF cubes share a common operand — factor it out (`(A && B) \|\| (A && C)` → `A && (B \|\| C)`). |
| RPM433 | `condition-common-disjunct-factor` | allow | `(A \|\| B) && (A \|\| C)` style `%if` expression — every top-level `&&` clause shares a common disjunct; factor it out to `A \|\| (B && C)`. |
| RPM434 | `negated-comparison-simplify` | warn | `!(X OP Y)` can be rewritten by flipping the comparison operator (e.g. `!(X >= 8)` → `X < 8`). |
| RPM435 | `unnecessary-condition-parentheses` | warn | `%if` expression contains redundant parentheses — wrapping a single atom or nesting parens directly inside parens. |
| RPM436 | `bcond-negation-canonical` | warn | `%if !%{with NAME}` / `%if !%{without NAME}` can be canonicalised by flipping polarity (`!%{with X}` → `%{without X}`). |
| RPM437 | `optional-macro-boolean-shortening` | allow | `%{?N:1}%{!?N:0}` is the verbose form of the macro-presence test; use the shorter `0%{?N:1}` idiom instead. |
| RPM438 | `empty-optional-macro-arm` | warn | Adjacent `%{?foo:…}%{!?foo:…}` pair where one arm is empty — the empty arm is a no-op and can be dropped. |
| RPM439 | `target-cpu-equality-to-ifarch` | warn | `%if "%{_target_cpu}" == "ARCH"` (optionally chained via `\|\|`) should be written as `%ifarch ARCH …`. |
| RPM440 | `arch-condition-domain-simplify` | warn | `%ifarch <list>` covers every architecture the active profile may target — the condition is always true; drop the `%ifarch` wrapper. |
| RPM441 | `arch-complement-shorter` | warn | `%ifnarch` lists more arches than the complement against the profile's target universe — flip to `%ifarch` with the complement set. |
| RPM442 | `arch-subset-under-parent` | warn | Inner `%ifarch` block lists every arch already guaranteed by an enclosing `%ifarch` — the inner test is always true. |
| RPM443 | `equality-chain-to-range` | warn | `X == N1 \|\| X == N2 \|\| X == N3 …` over a contiguous integer range — rewrite as `X >= MIN && X <= MAX`. |
| RPM444 | `adjacent-mutually-exclusive-ifs-to-elif` | warn | Two adjacent `%if` blocks have mutually exclusive conditions — merge into one `%if` / `%elif` chain. |
| RPM445 | `same-body-different-conditions-merge` | warn | Two adjacent `%if` blocks have the same body but different conditions — merge them into one block with the conditions joined by `\|\|`. |
| RPM446 | `same-body-arch-blocks-to-arch-list` | warn | Two adjacent `%ifarch` blocks have the same body but different arch lists — merge into one block listing all arches. |
| RPM454 | `same-guard-clustering-in-commutative-context` | warn | Multiple non-adjacent `%if A` blocks at the same preamble level share a condition and contain only commutative items — cluster into one `%if A` block. |
| RPM455 | `repeated-ifelse-value-extraction` | allow | Two or more `%if X … %else … %endif` blocks share the same condition and each branch picks a single dep/global value — extract the choice into one `%global` selector. |
| RPM456 | `branch-item-subset` | warn | One branch of a conditional contains a strict subset of another branch's items — hoist the shared items above the conditional. |
| RPM470 | `setup-autopatch-to-autosetup` | warn | `%prep` invokes both `%setup` and `%autopatch` — fold into a single `%autosetup` call which handles unpacking and patch application together. |
| RPM471 | `patch-sequence-to-autopatch` | warn | Every declared patch is applied via `%patch -P N -pK` with the same strip value — replace the sequence with a single `%autopatch -pK`. |
| RPM472 | `redundant-setup-default-name` | warn | `%setup -n %{name}-%{version}` repeats the default top-directory; drop the redundant `-n` flag. |
| RPM473 | `manual-tar-cd-to-setup` | warn | Manual `tar xf %{SOURCE<N>}` extraction in `%prep` — prefer `%setup` so RPM picks the right tar flags and integrates with `%autosetup` / `%autopatch`. |
| RPM474 | `manual-patch-command-to-patch-macro` | warn | `patch <flags> < %{PATCH<N>}` is the manual form; use `%patch -P <N> <flags>` so RPM tracks the application against the declared `Patch:` tag. |
| RPM475 | `redundant-cd-after-setup` | warn | `cd %{name}-%{version}` (or the same directory `%setup` already entered) immediately follows `%setup`/`%autosetup`; drop the redundant `cd`. |
| RPM476 | `manual-extra-source-unpack-to-setup-a-b` | warn | Manual `tar xf %{SOURCE<N>}` (N >= 1) alongside `%setup` — fold into `%setup -a N` or `%setup -b N`. |
| RPM477 | `long-patch-list-to-patchlist` | allow | Spec declares many `PatchN:` tags — switch to a single `%patchlist` block (rpm ≥ 4.15) for a more compact preamble. |
| RPM490 | `macro-composition-to-specific-macro` | warn | A macro-composed path (e.g. `%{_prefix}/bin`) has a more specific canonical macro (`%{_bindir}`); switch to the specific form. |
| RPM492 | `single-use-private-macro` | allow | `%global NAME BODY` is referenced exactly once in the spec — inline the body at the call site and drop the `%global`. |
| RPM493 | `macro-alias-of-builtin` | warn | `%global NAME %{builtin}` aliases a well-known RPM macro — reference the builtin directly instead of hiding it behind a local alias. |
| RPM494 | `no-op-conditional-macro` | warn | Standalone `%{?foo:}` or `%{!?foo:}` macro reference expands to nothing — drop the no-op. |
| RPM495 | `unused-macro-parameter` | warn | Parametric macro declares an option flag in its `(…)` option list that the body never references. |
| RPM496 | `macro-called-always-with-same-argument` | allow | Parametric macro is called at every site with the same argument — the parameter may be unnecessary; consider hardcoding the value. |
| RPM497 | `duplicate-macro-bodies` | allow | Two or more macro definitions share the same body — consolidate to one canonical name. |
| RPM498 | `long-literal-prefix-macro-candidate` | allow | Multiple `Source` / `Patch` / `URL` tags share a long literal prefix — extract it into a `%global` and reference the global instead. |
| RPM499 | `macro-name-shadows-bcond-helper` | warn | Local `%global with_NAME` / `%global without_NAME` shadows the visual shape of `%{with NAME}` / `%{without NAME}` bcond accessors — pick a different name to avoid confusion. |
| RPM510 | `adjacent-doc-lines-merge` | warn | Adjacent `%doc PATH` entries within one `%files` section — merge into a single `%doc A B C` line. |
| RPM511 | `adjacent-license-lines-merge` | warn | Adjacent `%license PATH` entries within one `%files` section — merge into a single `%license A B C` line. |
| RPM512 | `redundant-default-defattr` | warn | `%defattr(-,root,root,-)` is the default for modern RPM — drop the line. |
| RPM513 | `files-directory-subsumes-child` | allow | A `%files` entry lists a path already covered by another entry that owns the parent directory and its contents. |
| RPM514 | `files-glob-subsumes-explicit-entry` | warn | An explicit `%files` entry is already matched by a glob entry in the same section — drop the explicit duplicate. |
| RPM516 | `repeated-files-prefix-to-directory-entry` | allow | Many `%files` entries share a deep private directory — consider one entry that owns the whole directory instead. |
| RPM517 | `files-section-sort-blocks` | allow | `%files` entries are not in the canonical order (license → doc → config → other) — group them for easier review. |
| RPM530 | `mkdir-install-to-install-d` | warn | `mkdir -p DIR` followed by `install -m… src DIR/file` — fold into a single `install -D -m… src DIR/file`. |
| RPM531 | `redundant-mkdir-before-install-d` | warn | `mkdir -p DIR` followed by `install -D src DIR/file` — `install -D` already creates the parent directory; drop the `mkdir -p`. |
| RPM532 | `combine-install-dirs` | allow | Adjacent `install -d` (or `mkdir -p`) lines — combine into one invocation with all directories. |
| RPM533 | `cp-chmod-to-install-m` | allow | `cp src dst` + `chmod MODE dst` pair — fold into `install -m MODE src dst`. |
| RPM534 | `repeated-rm-f-combine` | allow | Adjacent `rm -f` lines in a shell-body section — combine into one `rm -f` with all targets. |
| RPM535 | `duplicate-shell-block` | allow | Identical 3+ line shell snippet appears in two or more shell-body sections — extract into a helper macro or function. |
| RPM536 | `near-duplicate-shell-block` | allow | Three or more consecutive shell lines share the same word-shape and differ in one position — fold into a `for` loop over the varying token. |
| RPM550 | `prefer-relative-subpackage-name` | warn | Absolute subpackage reference (`-n <main>-<suffix>` or `-n %{name}-<suffix>`) can be replaced with the bare relative form. |
| RPM551 | `mixed-subpackage-reference-style` | warn | Subpackage referenced both as `foo` (relative) and `-n %{name}-foo` (absolute) — pick one style. |
| RPM552 | `redundant-subpackage-version-release` | warn | Subpackage preamble explicitly sets `Version:` or `Release:` to the main package's value — drop the duplicate; subpackages inherit by default. |
| RPM553 | `repeated-subpackage-boilerplate` | allow | Two or more subpackages share the same preamble boilerplate — extract the common lines into one place. |
| RPM554 | `subpackage-description-copied-from-main` | warn | A subpackage `%description` is byte-for-byte identical to the main package's description — write a description that explains how the subpackage differs. |
| RPM555 | `description-equals-summary` | warn | `%description` body is byte-for-byte identical to the `Summary:` tag — write prose that adds context beyond the summary. |
| RPM570 | `commented-out-spec-code` | allow | Three or more consecutive `#`-commented lines look like commented-out spec syntax — remove or replace with `%dnl` + rationale. |
| RPM571 | `stale-disabled-source-or-patch` | warn | Commented `SourceN:` / `PatchN:` line — delete it or record the reason in the changelog. |
| RPM572 | `excessive-section-separators` | allow | Decorative `#########` / `==========` separator comment — drop it; section boundaries are already obvious. |
| RPM573 | `canonical-major-section-order` | allow | Major sections appear out of canonical order (description → prep → build → install → check → files → scriptlets → changelog). |
| RPM574 | `preamble-tag-clustering` | allow | Preamble tags are not clustered in the canonical packaging order — group identity → sources → build deps → runtime deps. |
| RPM575 | `repeated-comment-before-identical-guards` | allow | The same explanatory comment precedes multiple `%if` blocks — hoist it to a single location instead of repeating it. |
| RPM590 | `richdep-singleton` | warn | Rich-dep declaration wraps a single atom in `(…)` — drop the parentheses. |
| RPM593 | `richdep-common-factor` | allow | Rich-dep `or`-chain whose every `and`-operand shares a common subterm — factor it out (`(A and B) or (A and C)` → `A and (B or C)`). |
| RPM595 | `richdep-nested-same-operator-flatten` | warn | Rich-dep group contains a nested child with the same operator — flatten the parentheses (`A and (B and C)` → `A and B and C`). |
