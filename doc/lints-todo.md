# Lint Roadmap

Полный каталог планируемых правил линтера. ID присваиваются нами один раз и стабильны после реализации; rpmlint-аналог даётся ради грепабельности при гуглинге.

## Phase 1 — Packaging essentials (✅ implemented)

| ID     | Name                          | rpmlint analog                    | Default | Auto-fix              | Notes |
|--------|-------------------------------|-----------------------------------|---------|-----------------------|-------|
| RPM001 | missing-changelog             | no-changelogname-tag              | warn    | —                     | ✅ done |
| RPM002 | empty-description             | description-shorter-than-summary  | warn    | —                     | ✅ done |
| RPM010 | missing-name-tag              | no-name-tag                       | deny    | —                     | ✅ phase 1 |
| RPM011 | missing-version-tag           | no-version-tag                    | deny    | —                     | ✅ phase 1 |
| RPM012 | missing-release-tag           | no-release-tag                    | deny    | —                     | ✅ phase 1 |
| RPM013 | missing-license-tag           | no-license                        | deny    | —                     | ✅ phase 1 |
| RPM014 | missing-summary-tag           | no-summary-tag                    | deny    | —                     | ✅ phase 1 |
| RPM015 | missing-url-tag               | no-url-tag                        | warn    | —                     | ✅ phase 1 |
| RPM020 | obsolete-tag                  | obsolete-tag / hardcoded-packager-tag / prereq-use | warn | MachineApplicable (drop line) | ✅ phase 1; tags: BuildRoot, Packager, Vendor, Other("Copyright"/"Serial"/"PreReq"/"BuildPreReq") |
| RPM021 | deprecated-clean-section      | superfluous-%clean-section        | warn    | MachineApplicable (drop section) | ✅ phase 1 |
| RPM022 | multiple-changelog-sections   | more-than-one-%changelog-section  | deny    | —                     | ✅ phase 1 |

## Phase 2 — Correctness (✅ implemented except RPM030)

| ID     | Name                          | rpmlint analog                  | Default | Auto-fix          | Notes |
|--------|-------------------------------|---------------------------------|---------|-------------------|-------|
| RPM030 | requires-no-version           | explicit-lib-dependency         | warn    | Manual            | **deferred to phase 3** — needs configurable name whitelist (lib-prefix heuristic alone yields too many false-positives without per-profile tuning) |
| RPM031 | requires-equal-version        | requires-on-release             | warn    | Manual            | ✅ phase 2 |
| RPM032 | macro-redefinition            | n/a                             | warn    | —                 | ✅ phase 2; `%undefine` correctly clears the seen-set |
| RPM033 | self-obsoletion               | self-obsoletion                 | deny    | —                 | ✅ phase 2; **subpackage-aware** |
| RPM034 | obsolete-without-provides     | obsolete-not-provided           | warn    | —                 | ✅ phase 2; **subpackage-aware**; skips macroized names and `/path` obsoletes |
| RPM035 | useless-explicit-provides     | useless-provides                | warn    | MachineApplicable | ✅ phase 2; **subpackage-aware**; only flags unversioned form |
| RPM040 | self-conflict                 | n/a                             | deny    | —                 | ✅ phase 2; **subpackage-aware** |

Phase 2 infrastructure landed in `rules/util.rs`: `PackageView { name, items, header_span }`, `iter_packages(spec)` (main + every `%package` block), `collect_dep_atoms_in_items(items, tag_matcher)` walking through rich deps.

## Phase 3 — Sections, changelog, parser-bridge (✅ implemented)

| ID     | Name                          | rpmlint analog                  | Default | Auto-fix          | Notes |
|--------|-------------------------------|---------------------------------|---------|-------------------|-------|
| RPM016 | missing-prep-section          | no-%prep-section                | warn    | —                 | ✅ phase 3 |
| RPM017 | missing-build-section         | no-%build-section               | warn    | —                 | ✅ phase 3 |
| RPM018 | missing-install-section       | no-%install-section             | warn    | —                 | ✅ phase 3 |
| RPM023 | duplicate-buildscript-section | n/a                             | deny    | —                 | ✅ phase 3; ловит дубли `%prep`/`%build`/`%install`/etc. |
| RPM037 | empty-changelog-entry         | n/a                             | warn    | —                 | ✅ phase 3 |
| RPM038 | changelog-future-date         | changelog-time-in-future        | warn    | —                 | ✅ phase 3; `time` crate для current year |
| RPM039 | changelog-implausible-date    | (parser `rpmspec/W0025`)        | warn    | —                 | ✅ phase 3; AST-уровневая проверка day∈1..=31, year∈1990..=current+1 |

Также в фазе 3 добавлена инфраструктура **parser-bridge** в `analyzer::session`: parser-эмитируемые диагностики (`rpmspec/E*`, `rpmspec/W*`) переэмиттятся как обычные `Diagnostic` с lint-id вида `parse/<code>`. Мэппинг покрывает 7 кодов (no-progress, unterminated-conditional, stray-percent, line-not-recognized, unterminated-macro, multiple-else, malformed-changelog-header). Severity управляется через `.rpmspec.toml` так же, как у обычных правил.

### Phase 3 — deferred to dedicated PR

**Distribution profile system** (`profile = "fedora" | "opensuse" | "altlinux"` в `.rpmspec.toml`) отложен — это отдельный PR с дизайнерскими развилками: какой формат, какие списки лицензий/групп, как сочетается с per-lint config.

С ним же придут:
- RPM024 invalid-license (требует списка валидных лицензий из профиля),
- RPM025 non-standard-group (требует списка валидных групп),
- RPM030 requires-no-version (требует whitelist'а имён через профиль).

## Phase 4 — Style / source-text (✅ implemented)

Только два из 11 правил оказались source-bytes-aware (RPM051/052); остальные
работают через существующие AST visit-points. `Lint::set_source` (заложенный в
фазе 1) теперь имеет реальных потребителей.

| ID     | Name                          | rpmlint analog                  | Default | Auto-fix          | Notes |
|--------|-------------------------------|---------------------------------|---------|-------------------|-------|
| RPM036 | macro-in-hash-comment         | macro-in-comment                | warn    | MachineApplicable | ✅ phase 4; escape `%` → `%%` per `%` byte в comment span (через `set_source`) |
| RPM050 | hardcoded-paths               | hardcoded-library-path          | warn    | Manual            | ✅ phase 4; 10 prefix mappings (`/usr/bin → %{_bindir}`, ...); пропускает Source/Patch/URL/Summary/License/Group |
| RPM051 | tab-indent                    | mixed-use-of-spaces-and-tabs    | warn    | MachineApplicable | ✅ phase 4; source-bytes scan; tab → 8 spaces |
| RPM052 | trailing-whitespace           | n/a                             | allow   | MachineApplicable | ✅ phase 4; source-bytes scan; default `Allow` (turn on per-project) |
| RPM053 | rpm-buildroot-shell-var       | rpm-buildroot-usage             | warn    | Manual            | ✅ phase 4; `$RPM_BUILD_ROOT` в любом ShellBody |
| RPM054 | rpm-source-dir-shell-var      | use-of-RPM_SOURCE_DIR           | warn    | Manual            | ✅ phase 4; `$RPM_SOURCE_DIR` |
| RPM055 | summary-ends-with-dot         | summary-ended-with-dot          | warn    | MachineApplicable | ✅ phase 4 |
| RPM056 | summary-not-capitalized       | summary-not-capitalized         | warn    | MachineApplicable | ✅ phase 4 |
| RPM057 | summary-too-long              | summary-too-long                | warn    | —                 | ✅ phase 4; hardcoded `MAX_SUMMARY_LEN = 80`. TODO: переезд в per-lint config с profile system. |
| RPM058 | name-in-summary               | name-repeated-in-summary        | allow   | —                 | ✅ phase 4; whole-word case-insensitive match |
| RPM059 | description-shorter-than-summary | description-shorter-than-summary | allow | —             | ✅ phase 4; **main package only**; subpackage `%description -n foo` — known limitation, требует pairing helper |

Auto-fix для RPM053/054 и RPM050 — `Manual` (а не `MachineApplicable`),
потому что `TextSegment` в AST не несёт per-segment span, и precise byte-range
edit невозможен без расширения upstream-парсера. Диагностика всё равно
показывает что заменить.

## Phase 5 — Modernization (✅ implemented)

| ID     | Name                          | rpmlint analog                  | Default | Auto-fix          | Notes |
|--------|-------------------------------|---------------------------------|---------|-------------------|-------|
| RPM060 | python-setup-test-deprecated  | python-setup-test               | allow   | Manual            | ✅ phase 5; needle `setup.py test` в shell body с word-boundary; default Allow (включать per-project) |
| RPM061 | python-setup-install-deprecated | python-setup-install          | allow   | Manual            | ✅ phase 5; needle `setup.py install`; default Allow |
| RPM062 | egrep-fgrep-deprecated        | deprecated-grep                 | warn    | MachineApplicable | ✅ phase 5; `egrep` → `grep -E`, `fgrep` → `grep -F`, точный sub-span на каждое вхождение |
| RPM063 | setup-without-q-flag          | setup-not-quiet                 | warn    | Manual            | ✅ phase 5; `%setup` без `-q` (включая комбинированные `-qn`/`-nq` и `--quiet`); conservative bail-out на макросах в args. v2: MachineApplicable когда появится per-segment span у `MacroRef`. |
| RPM064 | patch-defined-not-applied     | patch-not-applied               | warn    | —                 | ✅ phase 5; пары `PatchN:` ↔ `%patchN`/`%patch -P N`/`%patch -PN`; `%autopatch`/`%autosetup` = all applied; macro-arg в `-P` = conservative all applied; silent при отсутствии `%prep` |

## Maintenance rules

- Новые правила получают **следующий свободный ID в своей сотне** (Packaging: 0xx, Correctness: 03x, Sections: 01x/02x/03x на конкретный диапазон, Style: 05x, Modernization: 06x).
- `name` в kebab-case, читается как утверждение об ошибке (`missing-name-tag`, не `name-tag-check`).
- Default severity консервативный: `deny` только для явных багов (missing mandatory, self-obsoletion), `warn` для стилистики и устаревших практик, `low` для cosmetic.
- `Applicability::MachineApplicable` — только если фикс гарантированно не меняет семантику; иначе `MaybeIncorrect` или `Manual`.

## Источники

- [rpmlint SpecCheck.py](https://github.com/rpm-software-management/rpmlint/blob/main/rpmlint/checks/SpecCheck.py)
- [rpmlint TagsCheck.py](https://github.com/rpm-software-management/rpmlint/blob/main/rpmlint/checks/TagsCheck.py)
- [Fedora FrequentlyMadePackagingMistakes](https://fedoraproject.org/wiki/FrequentlyMadePackagingMistakes)
- [Fedora Packaging Guidelines](https://docs.fedoraproject.org/en-US/packaging-guidelines/)
- [openSUSE Specfile guidelines](https://en.opensuse.org/openSUSE:Specfile_guidelines)
- AST `rpm-spec` v0.1.0: см. `crates/analyzer/src/visit.rs` для актуальных visit-точек.
