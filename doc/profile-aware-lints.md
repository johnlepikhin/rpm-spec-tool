# Profile-aware lint refactor — survey + roadmap

## Контекст

С момента приземления `feat(profile)` (commit `75d5175`) у анализатора есть
полностью наполненный `Profile`:

- `identity` — `family` / `vendor` / `dist_tag` / `name`
- `macros` — `BTreeMap<String, MacroEntry>` (400–700 макросов на distro:
  `_libdir`, `_bindir`, `dist`, `optflags`, `___build_pre`, …)
- `rpmlib` — `BTreeMap<String, String>` фич с минимальной версией rpm
  (`rpmlib(RichDependencies) = 4.12.0-1`)
- `arch` — `build_arch`, `build_os`, `compatible_archs`, `optflags_template`
- `licenses` — `BTreeSet<String>` allowed + `ValidationMode { Off, Warn, Strict }`
- `groups` — то же, что licenses
- `layers` — provenance trail

Профиль доходит до каждого правила через `Lint::set_profile(&Profile)`
(`crates/analyzer/src/lint.rs:57`), вызываемый в `LintSession`
(`crates/analyzer/src/session.rs:316`).

**Проблема:** ни одно из 46 правил в `crates/analyzer/src/rules/` сейчас
**не переопределяет** `set_profile`. Профильные данные текут вхолостую.
Несколько правил с захардкоженными таблицами (RPM050 hardcoded-paths)
дают subtly-wrong suggestions на не-RHEL/не-x86_64 distro. Несколько
правил из проектного roadmap (RPM024 invalid-license, RPM025
non-standard-group) спроектированы как profile-зависимые, но
не реализованы.

## Контракт `ValidationMode`

Для правил, использующих `profile.licenses` / `profile.groups`:

> Когда `mode == Off`, правило **обязано** не эмитить ни одной
> диагностики. `mode == Warn` / `Strict` различаются только дефолтным
> уровнем severity у consumer'а (фактический severity всё равно
> задаётся через `[lints]` в `.rpmspec.toml`).

Это закреплено в doc-комментариях `Profile::licenses` /
`Profile::groups` и в тестах `license_mode_off_contract_default`,
`group_mode_off_contract_default` (`crates/profile/src/types.rs`).

## Каталог правил с разбиением по приоритету

### Уровень 1 — критично (правила не работают / не существуют без profile data)

| ID | Имя | Файл | Статус | Профильные поля | Что нужно сделать |
|---|---|---|---|---|---|
| **RPM024** | invalid-license | (нет) | Не реализовано | `licenses` | Создать с нуля. Эмитить ТОЛЬКО при `profile.licenses.mode != Off`. Проверять `License:` тег (включая subpackage `%package`) против `profile.licenses.allowed`. SPDX-выражения с `OR`/`AND`/`WITH` разбирать на atoms и проверять каждый. |
| **RPM025** | non-standard-group | (нет) | Не реализовано | `groups` | Аналогично RPM024 для `Group:` тега против `profile.groups.allowed`. Допустимо подсказать ближайшую известную группу через простое substring-сравнение. |

**Замечание:** для активации потребуется наполнить `allowed`-листы в
bundled-профилях. Источники: SPDX 3.x для лицензий (Fedora использует
полный SPDX); для групп — `/usr/share/doc/rpm-*/GROUPS` каждого distro
(их можно собрать тем же SSH-механизмом, что и showrc-дампы).

### Уровень 2 — высокий impact (захардкоженные таблицы → данные профиля)

| ID | Имя | Файл | Что не так | Доработка | Поля |
|---|---|---|---|---|---|
| **RPM050** | hardcoded-paths | `hardcoded_paths.rs` | Hardcoded таблица из ~9 entries (`/usr/bin → %{_bindir}`, `/usr/lib64 → %{_libdir}`, `/etc → %{_sysconfdir}`, …) | Брать значения из `profile.macros.get("_bindir/_libdir/_sysconfdir/_datadir/_includedir/_mandir/_localstatedir")`. На e2k/aarch64 / SLES / Alt — значения отличаются; текущая таблица иногда даёт неправильные suggestions (например `_libdir` на e2k = `/usr/lib64` всё равно, но цепочка `_libexecdir`, `_prefix`, `_rundir` зависит от distro). | `macros` |
| **RPM053/054** | rpm-buildroot-shell-var, rpm-source-dir-shell-var | `shell_vars.rs` | Подсказывает замену `$RPM_BUILD_ROOT → %{buildroot}` универсально | На большинстве distro работает; но если `profile.macros.get("buildroot")` отсутствует — это сигнал, что замена не сработает. Гейтить присутствием макроса (consistency check). | `macros` |

### Уровень 3 — family-gated (suggest применим не во всех distro)

| ID | Имя | Файл | Доработка | Поля |
|---|---|---|---|---|
| **RPM021** | deprecated-clean-section | `deprecated_clean_section.rs` | Гейтить fix только на `profile.identity.family ∈ {Fedora, Rhel}` (modern rpm выкинул `%clean`). На SLES/SUSE и старых ALT секция может быть нужна — оставить warn без autofix. | `identity.family` |
| **RPM120/121/122** | use-make-build / use-make-install / use-configure-macro | `shell_modernization.rs` | `%make_build`, `%make_install`, `%configure` — Fedora/RHEL macros. Сверять с `profile.macros.contains_key("make_build")` etc. перед suggest'ом; на distro, где макроса нет — silence. | `macros` |
| **RPM031** | requires-equal-version | `requires_equal_version.rs` | Если `profile.rpmlib` не содержит `rpmlib(RichDependencies)` — не предлагать rich-deps синтаксис, только традиционный `Requires(post)`. | `rpmlib` |
| **RPM126** | description-leads-with-this-package | `description_health.rs` | Распространенный шаблон в Fedora packaging guidelines; на SUSE/ALT — не canonical convention. Гейтить severity по family. | `identity.family` |

### Уровень 4 — пропустить (purely AST/syntactic)

Правила, чья логика не зависит от distro данных:

- **Conditional refactoring**: RPM070 (deep-nesting), RPM072 (constant-condition), RPM076 (mergeable-conditionals), RPM080 (nested-and-collapse), RPM093 (condition-mentioned-many-times), RPM097 (hoist-common-prefix), RPM102 (inequality-redundancy), RPM110 (boolean-dnf), RPM113-117 (unreachable/always-true/dead-elif/mutex-branches/macro-trivial), RPM119 (leaf-line-hoist)
- **Metadata presence**: RPM001 (missing-changelog), RPM002 (empty-description), RPM010-RPM015 (missing tags), RPM016-RPM018 (missing sections), RPM022 (multiple-changelog), RPM023 (duplicate-buildscript), RPM037 (empty-changelog-entry), RPM038/039 (changelog dates), RPM040 (self-conflict)
- **Style**: RPM036 (macro-in-hash-comment), RPM051 (tab-indent), RPM052 (trailing-whitespace), RPM055 (summary-style), RPM059 (description-shorter-than-summary)
- **Semantic (AST)**: RPM030 (requires-no-version, отложено — без profile-tuning высокий FP-rate), RPM032 (macro-redefinition), RPM033 (self-obsoletion), RPM034 (obsolete-without-provides), RPM035 (useless-explicit-provides)
- **Source/patch**: RPM063 (setup-without-q-flag), RPM064 (patch-not-applied), RPM125 (source-without-url)
- **Subpackage**: RPM123 (package-without-description), RPM124 (subpackage-tag-mismatch)
- **External**: RPM200/RPM201 (shellcheck)

## План реализации

### Phase A — RPM024 + RPM025 (новые правила)

1. Создать `crates/analyzer/src/rules/invalid_license.rs` и
   `non_standard_group.rs` по шаблону существующих rules (mod, `Lint` trait,
   `set_profile` storing `licenses: BTreeSet<String>` + `mode`).
2. Зарегистрировать в `crates/analyzer/src/rules/mod.rs`.
3. Парсер License: SPDX-expression splitter (regex по `\b(OR|AND|WITH)\b`,
   trim brackets) — пока примитивный, расширенный SPDX-парсер отложить.
4. Парсер Group: literal substring сравнение.
5. Тесты — unit-тесты с in-memory профилями (allowed=`{MIT,GPL-2.0-or-later}`,
   mode=Strict) против spec'ов с `License: MIT`, `License: GPL-2.0+`,
   `License: WTFPL`, `License: MIT OR Apache-2.0`.
6. Добавить запись в `doc/lints-todo.md` (Phase 13).

### Phase B — RPM050 enhancement

1. Подменить hardcoded таблицу в `hardcoded_paths.rs` на функцию-builder
   `build_path_table(profile: &Profile) -> Vec<(String, &'static str)>`,
   читающую `profile.macros.get("_bindir/...")`.
2. Если профиль не предоставил макроса — оставить хардкод для этого
   конкретного пути как fallback (а не выключать всё правило).
3. Тесты — против `rhel-9-x86_64` (`_libdir=/usr/lib64`), `altlinux-10-x86_64`
   (`_libdir` may be `/usr/lib`), `altlinux-10-e2k` (другая arch).

### Phase C — family-gating (по одной задаче на правило)

Серия мелких PR'ов: RPM021, RPM120-122, RPM031, RPM126. Каждый — несколько
строк изменений + один gating-тест.

### Phase D — bootstrap data

Один раз: собрать SPDX-список Fedora через `dnf list -e 0 spdx-licenses`
или `/usr/share/spdx`; группы — `cat /usr/share/doc/rpm-*/GROUPS`. Залить
в `crates/profile/data/<name>.toml`.

## Тестовая стратегия

Каждое profile-aware правило получает 3 уровня тестов:

1. **Unit (`#[cfg(test)] mod tests`)** — синтетический `Profile`,
   проверяет логику + `mode = Off` контракт.
2. **Integration в `crates/analyzer/tests/`** — реальные bundled-профили
   (`rhel-9-x86_64`, `altlinux-10-x86_64`), проверка против fixture-spec'ов.
3. **CLI smoke в `crates/cli/tests/cli.rs`** — `rpm-spec-tool lint
   --profile rhel-9-x86_64 fixture.spec` → ожидаемые exit code + stderr.

## Open questions

- **SPDX-парсер**: писать свой минимальный или взять `spdx-rs`? Зависимость
  ~50K LOC; для нашего use case достаточно split по `OR/AND/WITH` + bracket
  trim.
- **Group fuzzy match**: предлагать ближайшее имя из allowed? Levenshtein
  или substring? Скорее substring (короткая иерархия типа `System/Libraries`).
- **Где взять SPDX/Groups allowlists для каждого distro**: SSH-bootstrap
  как для showrc, или однократный manual seed для топовых distro?

## Шаблоны profile-gating'а

После Phase 14 + 15 у нас четыре устоявшихся шаблона. Выбор зависит от того,
*как* профиль влияет на правило:

| Когда правило применяется | Шаблон | Пример |
|---|---|---|
| Всегда; severity / suggestion применимости меняется | `set_profile` + per-emit check | RPM021 (autofix `MachineApplicable` ↔ `Manual` по family) |
| Только когда профиль предоставил allowlist | `licenses.mode != Off` / `groups.mode != Off` | RPM024, RPM025 |
| Только когда конкретный макрос определён | `profile.macros.get("X").is_some()` | RPM120-122 (`make_build` / `make_install` / `configure`) |
| **Правило логически не применимо на distro** | **`Lint::applies_to_profile` → `false`** (session-level skip) | **RPM127** (Fedora ≥ 40 only), **RPM128** (openSUSE only), **RPM129** (non-Fedora/RHEL only) |

`applies_to_profile` — для случая «правило в принципе не имеет смысла»;
session дропает rule из active set, visit-pass даже не запускается.
`set_profile` + inline check — для случая «правило применимо везде, но
детали зависят».

### Когда использовать `applies_to_profile`

* Правило указывает на distro-specific convention (RPM128 — openSUSE Group:
  guideline; Fedora и ALT эту guideline не разделяют).
* Правило срабатывает на конструкцию, которая на других distro безвредна
  или невозможна (RPM129 — `%bcond_with` не существует / ведёт себя
  иначе вне RHEL-семейства).
* Composite gate с family + dist_tag / rpmlib (RPM127 — Fedora-only AND
  релиз ≥ 40, т.к. SPDX-only policy появилась в F40).

### Когда оставаться на `set_profile` + inline check

* Severity / wording suggestion'а меняется по distro, но сама диагностика
  применима везде (RPM021 — `%clean` *вреден* и в Fedora, и в Debian-ish
  rpm-системах, но autofix safety зависит от distro).
* Гейтинг по содержимому profile, не по identity (RPM024 — `mode == Off`
  закрывает rule даже на «правильном» distro, если пользователь не
  настроил allowlist).

### Test helper `make_test_profile`

Для unit-тестов profile-aware rules — общий helper в
`crates/analyzer/src/rules/util.rs`:

```rust
#[cfg(test)]
pub fn make_test_profile(
    family: Option<Family>,
    dist_tag: Option<&str>,
    macros: &[(&str, &str)],
    rpmlib: &[(&str, &str)],
) -> Profile { ... }
```

Заменяет ручную мутацию `Profile::default()` и keeps тесты hermetic
(никаких bundled-профилей в unit-тестах).
