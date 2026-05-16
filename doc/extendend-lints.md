Ниже — исследовательская сводка с учётом вашего текущего каталога проверок. Я сознательно не предлагаю дубли вроде `missing-license`, `hardcoded-paths`, `macro-in-hash-comment`, `make → %make_build`, `setup -q`, базовой DNF-оптимизации условий и уже реализованного `shellcheck`.

## 1. Главный вывод

У вас уже сильное покрытие по синтаксису, обязательным тегам, устаревшим конструкциям, базовой гигиене зависимостей и особенно по условным блокам. Самые ценные следующие направления:

1. **scriptlets / systemd / tmpfiles / users-groups** — высокая цена ошибки: сломанные транзакции, неверный порядок установки, сервисы стартуют/включаются неправильно, отсутствуют runtime/scriptlet dependencies.
2. **`%files`-семантика** — конфиги, лицензии, devel-файлы, locales, systemd units, tmpfiles, `%ghost`, `%attr`, дубли и слишком широкие glob’ы.
3. **cross-tag consistency** — `Source0` vs `Version`, `Release` vs dist-tag policy, changelog EVR/date consistency, duplicate singleton tags, subpackage name collisions.
4. **profile-aware feature gating** — weak deps, rich deps, `%bcond`, `BuildSystem`, `%license`, `%artifact`, scriptlet section append/prepend, rpm-version-specific semantics.
5. **глубокий анализ shell/build/install flow** — не вместо shellcheck, а поверх него: “пишет ли `%install` мимо `%{buildroot}`”, “создан ли `%ghost`”, “соответствует ли `%files` тому, что install script явно кладёт”.

Смысловая причина: RPM spec — не просто декларативный manifest. RPM сначала раскрывает макросы, затем парсит секции; макросы раскрываются даже в комментариях, а `BuildArch` может вызвать повторный парсинг spec’а с повторным раскрытием макросов. Это создаёт класс ошибок, которые обычный grep и даже базовый AST-линт часто пропускают. ([RPM][1])

---

## 2. С какими болями сталкиваются пользователи spec-файлов

### 2.1. Неочевидная модель исполнения spec-файла

Пользователи часто воспринимают spec как “текстовый рецепт”, но RPM обрабатывает его в несколько фаз: макро-раскрытие, условные блоки, парсинг секций, исполнение build/runtime scriptlets. Важные сюрпризы: макросы раскрываются в комментариях; `%if` не является макросом; `BuildArch` может рекурсивно перепарсить spec и повторно раскрыть макросы до этой строки. ([RPM][1])

**Что это означает для линтера:** нужны проверки не только “что написано”, но и “когда это будет раскрыто/исполнено”.

### 2.2. Scriptlets ломают транзакции

Fedora scriptlet guidelines прямо требуют, чтобы scriptlets завершались с нулевым кодом; иначе install/upgrade/erase может оборваться, оставить старую версию, stale files и неконсистентные rpmdb entries. Там же описана семантика `$1` и рекомендация использовать `$1 -gt 1`, а не точное сравнение с `2`, потому что multiversion/multilib и ошибки транзакций меняют ожидаемые значения. ([docs.pagure.org][2])

**Что это означает для линтера:** scriptlet-анализ должен быть отдельным приоритетом, даже при наличии `shellcheck`.

### 2.3. Distro-specific conventions конфликтуют

Fedora и openSUSE имеют разные systemd-ритуалы. Fedora-подход использует `%systemd_post`, `%systemd_preun`, `%systemd_postun_with_restart` и `%{?systemd_requires}`. openSUSE, наоборот, описывает `%service_*` macros и прямо discourages `Requires(post): systemd` / `%systemd_requires` для обычных systemd scriptlet macros, потому что эти macros умеют работать при отсутствии systemd. ([docs.pagure.org][2])

**Что это означает для линтера:** проверки должны быть profile-aware, иначе будет много ложных срабатываний между Fedora/RHEL/openSUSE/ALT/Mageia.

### 2.4. `%files` — источник многих скрытых ошибок

openSUSE guidelines и packaging checks выделяют типовые проблемы: non-config files under `/etc`, конфиги под `/usr`, devel-файлы в main package, `.pc` files без `BuildRequires: pkgconfig`, локали без `%lang`/`%find_lang`, файлы под `/var/run`/`/var/lock`, standard directories owned by package, duplicate files. ([openSUSE Wiki][3])

**Что это означает для линтера:** даже без доступа к buildroot можно извлечь много пользы из декларативного `%files`.

### 2.5. Зависимости: explicit vs automatic, strong vs weak, scriptlet deps

RPM автоматически добавляет `Provides: name = EVR`, поддерживает ordered scriptlet qualifiers (`pre`, `post`, `preun`, `postun`, `pretrans`, `posttrans`), weak deps начиная с rpm 4.13, и `meta` qualifier начиная с rpm 4.16. Ошибка в qualifier или смешение install-time deps с runtime deps часто не очевидны пользователю. ([RPM][1])

**Что это означает для линтера:** стоит проверять не только atom-level зависимости, но и контекст: где dependency используется, зачем, и поддерживается ли она целевым rpm.

### 2.6. Boilerplate и модернизация

RPM 4.20 добавил declarative builds: `BuildSystem:` и `BuildOption:` могут заменить повторяющиеся `%prep/%build/%install` boilerplate для типовых build systems. Это не универсальная рекомендация для старых distro, но на новых профилях это перспективное направление. ([rpm-software-management.github.io][4])

**Что это означает для линтера:** стоит добавить opt-in рекомендации по миграции на современные macros/build systems, но с низкой severity и strict profile-gating.

---

## 3. Best practices, которые стоит кодировать в правила

1. **Spec должен быть profile-aware.** Fedora/RHEL/openSUSE/ALT/Mageia нельзя проверять одним набором “истин”. Особенно это касается `Group:`, `%bcond_*`, systemd macros, `%files` conventions, dist-tag policy и helper macros.

2. **Макросы лучше проверять по фазам.** Одно дело — macro reference в preamble tag, другое — в shell body, третье — в comment, четвёртое — перед `BuildArch`. RPM раскрывает macro lines до дальнейшей обработки, а shell variables в build sections исполняются позднее; это различие критично. ([RPM][1])

3. **Scriptlets должны быть минимальными.** Где есть distro-provided macros или file triggers, лучше использовать их; ручной `systemctl`, `ldconfig`, cache update, `useradd`/`groupadd` должны рассматриваться как подозрительные паттерны, а не как нормальный baseline. Fedora прямо отмечает, что file triggers могут устранить необходимость большинства scriptlets. ([docs.pagure.org][2])

4. **`%files` должен классифицировать файлы, а не просто перечислять.** Конфиги — `%config(noreplace)` где применимо; лицензии — `%license`, а не `%doc`; development artifacts — в `-devel`; локали — через `%find_lang`; transient state — `%ghost`/tmpfiles. ([openSUSE Wiki][3])

5. **Build scripts не должны писать вне разрешённых областей.** openSUSE формулирует это явно: build scripts должны менять только `%buildroot`, `%_builddir` и допустимые temp locations, с разной матрицей по `%prep/%build/%install/%check/%clean`. ([openSUSE Wiki][3])

6. **Нужно быть консервативным по false positives.** Даже rpmlint-документация openSUSE подчёркивает, что lint не всегда прав и иногда нужны suppressions; для spec-only анализатора это ещё важнее, потому что он не видит buildroot, исходные архивы и итоговые RPM payloads. ([openSUSE Wiki][5])

---

## 4. Новые полезные проверки: высокий ROI

Ниже — правила, которые можно реализовать преимущественно на уже имеющейся инфраструктуре: AST, preamble tags, dependency walker, profile macros, raw source, `%files`, scriptlets, shell bodies.

### 4.1. Metadata / cross-tag consistency

| Proposed ID                                        | Проверка                                                                                                                                       |                                                           Что ловит |                 Severity по умолчанию | Реализация                                                                                                                                                                                                         |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------: | ------------------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `RPM300 duplicate-singleton-tag`                   | Повтор singleton-тегов: `Name`, `Version`, `Release`, `License`, `URL`, `Summary`, `Epoch`, `BuildArch`, `AutoReqProv` в одной preamble scope. |                 Last-wins/неочевидная семантика, copy-paste ошибки. | Warn/Deny для `Name/Version/Release`. | AST preamble per package.                                                                                                                                                                                          |
| `RPM301 subpackage-name-collision`                 | Два `%package` дают одинаковое canonical name: `%package devel` и `%package -n %{name}-devel`, либо collision main/subpackage.                 |                 Перетирание секций, неверные `%files/%description`. |                                 Deny. | Subpackage view + macro literal expansion where safe.                                                                                                                                                              |
| `RPM302 invalid-name-version-release-epoch-format` | `Name` с whitespace / numeric operators; `Epoch` не integer или `Epoch: 0`; `Version/Release` с явно недопустимыми символами.                  |                           RPM-level invalid or misleading metadata. |                            Deny/Warn. | Literal-only; macro → conservative skip. RPM docs указывают ограничения для `Name`, `Version`, `Release`, `Epoch`. ([RPM][1])                                                                                      |
| `RPM303 release-disttag-policy`                    | Fedora/RHEL: `Release:` без `%{?dist}`; hardcoded `.fcNN`/`.elN`; dist-tag продублирован.                                                      |                         Неправильные rebuilds и branch portability. |                  Warn, profile-gated. | `Profile.family`, `dist_tag`, Text segments.                                                                                                                                                                       |
| `RPM304 source-version-mismatch`                   | `Source0` содержит hardcoded version, не совпадающую с `Version:`, или одновременно `%{version}` и другой literal version.                     |                                Stale source URL after version bump. |                                 Warn. | Source tags + Version literal + simple URL/path tokenization.                                                                                                                                                      |
| `RPM305 source-patch-list-mixing`                  | Одновременно используются `SourceN:` и `%sourcelist`, либо `PatchN:` и `%patchlist`.                                                           |                RPM docs считают mixing not recommended for clarity. |                                 Warn. | AST has `%sourcelist/%patchlist`. ([RPM][1])                                                                                                                                                                       |
| `RPM306 patch-applied-more-than-once`              | `PatchN:` применён вручную и через `%autopatch`/`%autosetup`, либо два раза через `%patch -P N`.                                               |                Сборка может падать или silently reverse/fuzz patch. |                            Warn/Deny. | Расширить существующий setup/patch tracking.                                                                                                                                                                       |
| `RPM307 patch-status-comment-missing`              | Для openSUSE: нет комментария над `PatchN:` или status marker.                                                                                 |                        Review pain: непонятно, upstreamed ли patch. |            Allow/Warn, openSUSE-only. | Raw source near Patch span. openSUSE требует status comment для patches. ([openSUSE Wiki][3])                                                                                                                      |
| `RPM308 autoreqprov-disabled-without-comment`      | `AutoReqProv: no`, `AutoReq: no`, `AutoProv: no` без соседнего комментария.                                                                    |               Отключение auto deps часто маскирует dependency bugs. |                                 Warn. | Preamble tags + source context. RPM docs описывают AutoReq/AutoProv как управление automatic dependency generation; openSUSE says AutoReqProv should not be used unless turning this off intentionally. ([RPM][1]) |
| `RPM309 buildarch-reparse-hazard`                  | `%global`/`%define` с `%()` shell expansion, counters, date/random, или side-effect macro до `BuildArch`.                                      |       `BuildArch` triggers parse recursion; macro may expand twice. |                                 Warn. | MacroDef before BuildArch; detect `%(`, `%{lua:}`, date-like commands. ([RPM][1])                                                                                                                                  |
| `RPM310 arch-policy-contradiction`                 | `BuildArch: noarch` + `ExclusiveArch`/`ExcludeArch`; `ExclusiveArch` ∩ `ExcludeArch` contradictions; unknown arch names vs profile.            | Spec says “noarch”, but restricts arch set or uses impossible arch. |                            Warn/Deny. | `ArchInfo.compatible`, `ArchList`.                                                                                                                                                                                 |
| `RPM311 changelog-order-weekday-evr`               | Changelog не отсортирован по дате, weekday не соответствует дате, latest EVR отсутствует или не совпадает с current `Version-Release`.         |                    Review/rpmlint pain; stale changelog after bump. |                                 Warn. | Existing changelog parser + EVR normalizer.                                                                                                                                                                        |
| `RPM312 spec-filename-mismatch`                    | Filename не равен `%{name}.spec`.                                                                                                              |                                             Common packaging check. |                                 Warn. | Нужен путь файла в lint session. openSUSE rpmlint имеет `invalid-spec-name`. ([openSUSE Wiki][5])                                                                                                                  |

### 4.2. Dependency semantics

| Proposed ID                                         | Проверка                                                                                                                                                          |                                                                  Что ловит |                         Severity | Реализация                                                                                                                                                         |
| --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------: | -------------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `RPM320 duplicate-dependency-atom`                  | Один и тот же atom повторяется в `Requires`, `BuildRequires`, `Provides`, weak deps внутри одного package scope.                                                  |                                                      Шум, merge artifacts. |                            Warn. | DepAtom walker + normalized EVR.                                                                                                                                   |
| `RPM321 weak-dep-duplicates-strong-dep`             | `Requires: X` и `Recommends: X` / `Suggests: X`.                                                                                                                  |                           Weak dep бессмысленна, если strong dep уже есть. |                            Warn. | Strong/weak dependency sets.                                                                                                                                       |
| `RPM322 self-weak-dependency`                       | `Recommends/Suggests/Supplements/Enhances` указывает на собственное имя пакета.                                                                                   |                      Обычно copy-paste или неверное reverse dep выражение. |                            Warn. | Subpackage-aware.                                                                                                                                                  |
| `RPM323 runtime-requires-looks-like-build-requires` | `Requires: gcc`, `cmake`, `pkgconfig(...)`, `*-devel`, `python3-devel`, `rust`, `go`, etc.                                                                        |                                         Build tools попали в runtime deps. |      Warn, allowlist exceptions. | Rule-configurable package-name patterns.                                                                                                                           |
| `RPM324 build-tool-used-without-buildrequires`      | В `%build/%install/%check` используется `cmake`, `meson`, `ninja`, `desktop-file-install`, `appstreamcli`, `pkg-config`, но нет соответствующего `BuildRequires`. |                           Сборка проходит локально, падает в clean chroot. |                            Warn. | ShellBody command extraction + profile command→package map. Mageia guidelines тоже подчёркивают, что внешние tools должны быть в BuildRequires. ([Mageia Вики][6]) |
| `RPM325 pkgconfig-file-without-pkgconfig-br`        | `%files` содержит `.pc`, но нет `BuildRequires: pkgconfig`.                                                                                                       | У `-devel` package может не появиться корректный runtime dep on pkgconfig. | Warn, openSUSE/SUSE high signal. | `%files` classifier. openSUSE explicitly requires this. ([openSUSE Wiki][3])                                                                                       |
| `RPM326 unsupported-dependency-feature`             | Weak deps, rich deps, `meta` qualifier, file triggers, `%artifact`, etc. используются на profile/rpm без поддержки.                                               |                                          Spec не собирается на target rpm. |                            Deny. | `RpmlibFeatures` + syntax scan. RPM docs list weak deps since 4.13 and `meta` since 4.16. ([RPM][1])                                                               |
| `RPM327 contradictory-dependency-qualifiers`        | `Requires(pre,meta): X`, `Requires(post,meta): X`, etc.                                                                                                           |                                     `meta` contradicts ordered qualifiers. |                            Deny. | Requires qualifier parser. RPM docs note contradiction. ([RPM][1])                                                                                                 |
| `RPM328 scriptlet-command-without-requires`         | `%post` calls external binary but no corresponding `Requires(post):` or macro-provided dependency.                                                                |                                                  Transaction ordering bug. |                            Warn. | Scriptlet command extraction + profile macro knowledge. openSUSE rpmlint has `no-prereq-on`. ([openSUSE Wiki][5])                                                  |

### 4.3. Scriptlets, systemd, tmpfiles, users/groups

| Proposed ID                                 | Проверка                                                                                                                                                      |                                                    Что ловит |                                           Severity | Реализация                                                                                                                                                                    |       |                                                                                                                              |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- | -----------------------------------------------------------: | -------------------------------------------------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ---------------------------------------------------------------------------------------------------------------------------- |
| `RPM340 scriptlet-exit-not-guaranteed-zero` | Scriptlet ends with command not followed by `                                                                                                                 |                                                              | :`, `:`, or `exit 0`; explicit `exit 1`; `set -e`. | Broken install/upgrade/erase transaction.                                                                                                                                     | Warn. | ShellBody line analysis + conservative shell parser later. Fedora says all scriptlets must exit zero. ([docs.pagure.org][2]) |
| `RPM341 scriptlet-upgrade-test-eq-two`      | `if [ "$1" = 2 ]`, `-eq 2`, `== 2` in `%pre/%post`.                                                                                                           | Wrong for multiversion/multilib/error cases; prefer `-gt 1`. |                                              Warn. | Regex on scriptlet bodies. ([docs.pagure.org][2])                                                                                                                             |       |                                                                                                                              |
| `RPM342 direct-systemctl-in-scriptlet`      | Manual `systemctl daemon-reload`, `enable`, `start`, `restart`, etc.                                                                                          |     Should use distro macros; enable/start policy violation. |                      Warn/Deny for `enable/start`. | Scriptlet body scan; profile-specific macro suggestions. Fedora/openSUSE both prescribe helper macros. ([docs.pagure.org][2])                                                 |       |                                                                                                                              |
| `RPM343 systemd-unit-without-helper-macros` | `%files` includes `.service/.socket/.timer/.path`, but missing required `%systemd_*` or `%service_*` scriptlets for profile.                                  |            Unit not registered/reloaded/restarted correctly. |                                              Warn. | `%files` classifier + scriptlet macro scan.                                                                                                                                   |       |                                                                                                                              |
| `RPM344 systemd-unit-under-etc-or-config`   | Unit installed/listed under `/etc/systemd/system` or marked `%config`.                                                                                        |  Unit files should live in `%_unitdir` and not be `%config`. |                                              Warn. | `%files` + `%install` path scan. openSUSE states this explicitly. ([openSUSE Wiki][7])                                                                                        |       |                                                                                                                              |
| `RPM345 repeated-service-macro-calls`       | Multiple `%service_add_post foo` lines instead of one macro call with all units.                                                                              |                       Larger scriptlets, harder maintenance. |                               Warn, openSUSE-only. | Count macro calls per scriptlet. openSUSE recommends calling each macro at most once with all units. ([openSUSE Wiki][7])                                                     |       |                                                                                                                              |
| `RPM346 ldconfig-scriptlet-style`           | Library package has manual `/sbin/ldconfig` shell scriptlet instead of `%post -p /sbin/ldconfig`, or missing ldconfig where target profile still requires it. |                      Cache not updated or unnecessary shell. |                               Warn, profile-gated. | `%files` library classifier + scriptlet header/body. Fedora shows `-p /sbin/ldconfig` pattern; openSUSE rpmlint has `library-without-ldconfig-postin`. ([docs.pagure.org][2]) |       |                                                                                                                              |
| `RPM347 tmpfiles-without-create`            | `%files` contains `tmpfiles.d/*.conf`, but `%post` lacks `%tmpfiles_create` / profile macro.                                                                  |                Runtime dirs/files not created after install. |                                              Warn. | `%files` classifier + scriptlet macro scan. openSUSE packaging checks list `postin-without-tmpfile-creation`. ([openSUSE Wiki][5])                                            |       |                                                                                                                              |
| `RPM348 unsafe-useradd-groupadd`            | `useradd/groupadd/usermod` without `getent` guard, fixed UID/GID without profile policy, missing scriptlet deps/macros.                                       |                  Non-idempotent install; account collisions. |                                              Warn. | Scriptlet scan + profile user/group macro registry.                                                                                                                           |       |                                                                                                                              |
| `RPM349 scriptlet-state-outside-rpm-state`  | Scriptlets store state under `/tmp`, `/var/tmp`, package dirs instead of rpm-state dir pattern.                                                               |                         Race/stale state across transaction. |                                              Warn. | Scriptlet path scan. Fedora recommends rpm-state location for state shared between scriptlets. ([docs.pagure.org][2])                                                         |       |                                                                                                                              |

### 4.4. `%files` checks

| Proposed ID                                | Проверка                                                                                                                               |                                                   Что ловит |                                         Severity | Реализация                                                                                                                    |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------: | -----------------------------------------------: | ----------------------------------------------------------------------------------------------------------------------------- |
| `RPM360 etc-file-not-config`               | Literal `%files` entry under `%{_sysconfdir}` or `/etc` not marked `%config`/`%config(noreplace)` and not executable exception.        |                          User config overwritten/untracked. |                                            Warn. | `%files` directive parser + path macro expansion. openSUSE rpmlint has `non-conffile-in-etc`. ([openSUSE Wiki][5])            |
| `RPM361 config-under-usr`                  | `%config` or `%config(noreplace)` on `/usr/...`.                                                                                       |                   `/usr` should not contain mutable config. |                      Warn, openSUSE/SUSE strong. | `%files` directive parser. ([openSUSE Wiki][3])                                                                               |
| `RPM362 plain-config-without-comment`      | `%config /etc/foo` without `noreplace` and no nearby justification comment.                                                            |                           Local changes may be overwritten. |                                            Warn. | Raw source context. openSUSE recommends `%config(noreplace)` and comment for plain `%config`. ([openSUSE Wiki][3])            |
| `RPM363 license-file-marked-doc`           | `LICENSE`, `COPYING`, `NOTICE`, etc. marked `%doc` instead of `%license`, or no `%license` in package with obvious license file entry. |                      Legal metadata/runtime doc separation. |                                            Warn. | `%files` classifier. ([openSUSE Wiki][3])                                                                                     |
| `RPM364 devel-file-in-non-devel-package`   | `.h`, unversioned `libfoo.so`, `.pc`, CMake package config files in non-`-devel` package.                                              |               Pulls development files into runtime package. |                                            Warn. | Subpackage view + path classifier. openSUSE lists these as `-devel` artifacts. ([openSUSE Wiki][3])                           |
| `RPM365 locale-file-not-lang`              | `.mo` under locale dirs listed manually without `%lang` or `%files -f %{name}.lang`; `%find_lang` absent.                              |    Localization packaging bugs after upstream adds locales. |                                            Warn. | `%files` + `%install` scan. ([openSUSE Wiki][3])                                                                              |
| `RPM366 duplicate-files-in-files-sections` | Same normalized path appears twice in same package or across subpackages; simple glob overlaps.                                        | Duplicate ownership, wrong directive applying through glob. |   Warn/Deny for exact duplicate across packages. | Normalize with profile macros; glob overlap heuristics. openSUSE warns files should be listed only once. ([openSUSE Wiki][3]) |
| `RPM367 standard-dir-owned`                | `%files` owns standard dirs like `%{_bindir}`, `%{_datadir}`, `%{_libdir}`, `/usr`, `/etc` without package-specific subdir.            |                            Package owns common directories. |                                            Warn. | Standard-dir table from profile. openSUSE rpmlint has `standard-dir-owned-by-package`. ([openSUSE Wiki][5])                   |
| `RPM368 broad-files-glob`                  | `%{_datadir}/*`, `%{_libdir}/*`, `/usr/*`, `%{_bindir}/*`.                                                                             |                     Hides newly added files, owns too much. |                                            Warn. | `%files` literal/glob analysis. openSUSE explicitly warns against `%_datadir/*` for locales. ([openSUSE Wiki][3])             |
| `RPM369 var-run-var-lock-not-ghost`        | Files under `/var/run` or `/var/lock`, especially not `%ghost`.                                                                        |                   Volatile dirs; wrong ownership semantics. |                                            Warn. | Path classifier. openSUSE says `/var/run` is symlink-like/volatile and suggests `%ghost`/tmpfiles. ([openSUSE Wiki][5])       |
| `RPM370 suspicious-attr-permissions`       | `%attr(777,...)`, `%attr(666,...)`, setuid/setgid modes, non-root owners in unexpected dirs.                                           |                                      Security/review issue. | Warn/Deny for world-writable without sticky bit. | Parse `%attr/%defattr`. openSUSE packaging checks include strange permissions/security-related checks. ([openSUSE Wiki][5])   |
| `RPM371 debuginfo-path-in-main-files`      | `%files` lists `/usr/lib/debug`, `.build-id`, debuginfo paths.                                                                         |            Debuginfo accidentally packaged in main package. |                                       Deny/Warn. | Path classifier. openSUSE rpmlint has `filelist-forbidden-debuginfo`. ([openSUSE Wiki][5])                                    |

### 4.5. Build/install/check sections

| Proposed ID                               | Проверка                                                                                                                             |                                                           Что ловит |                                  Severity | Реализация                                                                                                                                                                      |             |                           |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------: | ----------------------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- | ------------------------- |
| `RPM380 install-writes-outside-buildroot` | `%install` runs `install/cp/mkdir/touch/ln` to `/usr`, `/etc`, `/var` without `%{buildroot}`/`$RPM_BUILD_ROOT`.                      |                             Local machine pollution; build failure. |                                Deny/Warn. | ShellBody command heuristic. openSUSE says build scripts should alter only allowed dirs and `%install` may write `%buildroot`. ([openSUSE Wiki][3])                             |             |                           |
| `RPM381 rm-rf-buildroot-in-install`       | `rm -rf %{buildroot}` / `$RPM_BUILD_ROOT` at start of `%install`.                                                                    |                            Obsolete and potentially unsafe pattern. |                                     Warn. | ShellBody scan. openSUSE calls this bad style and explains race risk. ([openSUSE Wiki][3])                                                                                      |             |                           |
| `RPM382 makeinstall-without-underscore`   | `%makeinstall` instead of `%make_install`.                                                                                           |   Confusing macro; may not set `DESTDIR` correctly on some targets. |                                     Warn. | ShellBody scan. openSUSE says avoid `%makeinstall`. ([openSUSE Wiki][3])                                                                                                        |             |                           |
| `RPM383 make-install-missing-destdir`     | Manual `make install` without `DESTDIR=%{buildroot}` and not `%make_install`.                                                        |                                           Installs into host paths. |                                     Warn. | Shell command parser.                                                                                                                                                           |             |                           |
| `RPM384 install-chown-or-owner`           | `%install` uses `chown`, `chgrp`, `install -o`, `install -g`.                                                                        | Ownership should be set in `%files`, not during unprivileged build. |                                     Warn. | Shell scan. openSUSE says `%install` must not chown directly/indirectly. ([openSUSE Wiki][3])                                                                                   |             |                           |
| `RPM385 optflags-overridden`              | `CFLAGS=...`, `CXXFLAGS=...`, `FFLAGS=...` assigned without `%{optflags}`/`$RPM_OPT_FLAGS`; `LDFLAGS` without rpm flags.             |                                  Lost hardening/optimization flags. |                                     Warn. | Shell dataflow-lite. openSUSE says package compilers should honor `%{optflags}` and document overrides. ([openSUSE Wiki][3])                                                    |             |                           |
| `RPM386 werror-not-disabled`              | `%build` passes `-Werror` or lacks `--disable-werror` where obvious.                                                                 |                                         New compilers break builds. |                               Warn/Allow. | Shell scan. openSUSE recommends disabling upstream `-Werror` for packages. ([openSUSE Wiki][3])                                                                                 |             |                           |
| `RPM387 j1-without-comment`               | `make -j1` / `%make_build -j1` without nearby comment.                                                                               |              Serial build may be justified, but should explain why. |                               Allow/Warn. | Shell scan + source context. ([openSUSE Wiki][3])                                                                                                                               |             |                           |
| `RPM388 network-access-in-build`          | `curl`, `wget`, `git clone`, `go get`, `pip install` from network, `npm install` without offline flags, tests hitting network.       |    Clean build systems often block network; non-reproducible build. |                  Warn/Deny profile-gated. | Command scan; allowlist local loopback. Fedora discussions/guidelines consistently treat build-time network access as forbidden for official packages. ([Fedora Discussion][8]) |             |                           |
| `RPM389 disabled-check-section`           | `%check` contains only `:`, `true`, `exit 0`, `                                                                                      |                                                                     | :`, or all tests skipped without comment. | Tests silently disabled.                                                                                                                                                        | Warn/Allow. | ShellBody classification. |
| `RPM390 buildsystem-macro-modernization`  | CMake/Meson/Python/Rust/Go commands used manually while profile has `%cmake`, `%meson`, `%pyproject_*`, `%cargo_*`, `%gobuild`, etc. |                                Boilerplate and missed distro flags. |                  Warn/Allow, macro-gated. | MacroRegistry + shell patterns.                                                                                                                                                 |             |                           |
| `RPM391 declarative-build-candidate`      | rpm ≥ 4.20 profile, simple `%prep/%build/%install` matching known macro sequence; suggest `BuildSystem:`.                            |                               Deep modernization; less boilerplate. |                                    Allow. | Pattern matcher over sections. RPM 4.20 declarative builds centralize common steps. ([rpm-software-management.github.io][4])                                                    |             |                           |

### 4.6. Conditional builds / macros

| Proposed ID                                | Проверка                                                                                             |                                            Что ловит |            Severity | Реализация                                                                                                              |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------- | ---------------------------------------------------: | ------------------: | ----------------------------------------------------------------------------------------------------------------------- |
| `RPM400 prefer-bcond-new-syntax`           | rpm ≥ 4.17.1 profile: `%bcond_with/%bcond_without` could be `%bcond name default`.                   |            Reduces confusing legacy bcond semantics. |         Allow/Warn. | Raw scan + profile rpm version/features. RPM docs say `%bcond` was added because old macros confused people. ([RPM][9]) |
| `RPM401 bcond-defined-but-unused`          | `%bcond foo` exists, but no `%{with foo}`/`%{without foo}`/`%{?with_foo}`.                           |                                   Dead build option. |         Warn/Allow. | BuildCondition + MacroRef scan.                                                                                         |
| `RPM402 with-condition-without-bcond`      | `%{with foo}` or `%{without foo}` used but no local bcond and profile lacks definition.              |     Build option may be undeclared or external-only. |         Warn/Allow. | MacroRegistry + local bconds.                                                                                           |
| `RPM403 test-without-counterpart`          | Uses `%{?with_foo}` or `%{!?with_foo}` in some places and `%{with foo}` elsewhere.                   |         Mixed idioms; easier to invert accidentally. |              Allow. | MacroRef scan.                                                                                                          |
| `RPM404 macro-shell-expansion-in-metadata` | `%(...)` in `Version`, `Release`, `Source`, `Patch`, `License`, dependency tags.                     |         Non-reproducible parse-time shell execution. |               Warn. | MacroRef form scan.                                                                                                     |
| `RPM405 unresolved-nonbuiltin-macro`       | Macro reference not defined locally and not in `Profile.macros`, excluding known rpm dynamic macros. |                            Typo or profile mismatch. | Warn, conservative. | Local macro table + MacroRegistry + allowlist.                                                                          |
| `RPM406 include-not-expanded`              | `%include` is present; analyzer cannot see included body.                                            | Warn about incomplete analysis, not packaging error. |         Allow/Warn. | IncludeDirective.                                                                                                       |

---

## 5. Проверки с глубоким анализом кода

Это отдельный класс: не просто “ещё одно правило”, а новые analysis engines. Их стоит делать как opt-in или higher-cost pass, потому что они будут дороже и сложнее по false positives.

### 5.1. Symbolic spec evaluator по профилям

**Идея:** построить executable model spec-файла для конкретного профиля: раскрыть known macros, применить conditionals, получить effective package graph: packages, tags, deps, files, scriptlets.

**Что найдёт:**

| Проверка                                                                                  | Скрытая проблема                                                                             |
| ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Effective tag differs unexpectedly between Fedora/RHEL/openSUSE profiles                  | Один spec собирает разные `Name`, `Release`, `Source0`, deps без явного намерения.           |
| Package exists under one condition, but `%files`/`%description` отсутствует under another | Текущие RPM123/124 ограничены conditional parser issue; symbolic evaluator может снизить FP. |
| Conditional `BuildRequires` only present on path where build command is not used          | Лишний BR или missing BR на другом path.                                                     |
| “Dead profile branch”                                                                     | Ветка для `%{?fedora}` никогда не reachable на выбранных profile sets.                       |

**Что нужно добавить:** path-condition engine уже есть; нужен слой “effective spec view” с per-node path condition и profile evaluation.

### 5.2. Более сильный macro abstract interpreter

**Идея:** не просто one-level propagation, а guarded recursive expansion with fuel, purity classification и taint tracking.

**Что найдёт:**

| Проверка                                                          | Скрытая проблема                                                          |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Macro expands to different value before/after `BuildArch` reparse | Особенно опасно с `%()` shell expansion.                                  |
| Macro chain hides hardcoded path or stale version                 | `%global srcver 1.2`, `Source0: foo-%{srcver}.tar.gz` при `Version: 1.3`. |
| Undefined macro masked inside rich dep or files path              | Ошибка видна только после expansion.                                      |
| Macro purity violation                                            | `%(date)`, `%(git rev-parse)`, `%(curl ...)` в metadata.                  |

**Ограничение:** параметрические macros и `%{lua:...}` надо taint’ить, а не интерпретировать полноценно.

### 5.3. Shell/scriptlet control-flow analyzer

**Идея:** поверх `shellcheck` добавить маленький CFG/dataflow-анализ для scriptlets и build sections. Shellcheck ловит shell bugs; вам нужны RPM-specific semantics.

**Что найдёт:**

| Проверка                                                   | Скрытая проблема                                                                           |   |                         |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------ | - | ----------------------- |
| Scriptlet may exit non-zero on some path                   | Последняя команда в branch без `                                                           |   | :`; `exit 1`; `set -e`. |
| `$1` branch misses erase/upgrade case                      | Например `%preun` handles `$1 == 0`, but upgrade path `$1 == 1` falls through incorrectly. |   |                         |
| Manual service lifecycle violates policy                   | `systemctl enable/start` on install; restart even when service was not running.            |   |                         |
| External command used only in branch but dependency absent | `install-info`, `update-alternatives`, `useradd`, `glib-compile-schemas`.                  |   |                         |
| Non-idempotent account creation                            | `useradd foo` without `getent passwd foo`.                                                 |   |                         |

**Что нужно добавить:** tree-sitter-bash или другой shell AST; command extraction; simple exit-status lattice; profile command→package mapping.

### 5.4. `%install` write-set vs `%files` read-set

**Идея:** статически вывести приблизительный set файлов/директорий, создаваемых в `%install`, и сравнить с `%files`.

**Что найдёт:**

| Проверка                                 | Скрытая проблема                                                                                                              |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| File installed but likely not packaged   | `install -D foo %{buildroot}%{_bindir}/foo`, но `%files` не содержит `%{_bindir}/foo` и нет broad glob.                       |
| File listed but not visibly installed    | `%files` lists generated file, but `%install` never creates it; полезно только как weak signal.                               |
| `%ghost` file not touched in buildroot   | RPM expects ghost file entry to exist in buildroot in some policies; openSUSE docs mention this pattern. ([openSUSE Wiki][5]) |
| `make install` into host path            | Write-set points outside `%{buildroot}`.                                                                                      |
| Broad glob hides unowned/generated files | `%{_datadir}/*` masks exact ownership.                                                                                        |

**Ограничение:** make/ninja/cmake internals не видны. Нужно помечать confidence: high для explicit `install/cp/mkdir/touch/ln`, low для buildsystem commands.

### 5.5. `%files` classifier + package split optimizer

**Идея:** классифицировать file entries по типу: runtime binary, library, plugin, header, pkgconfig, cmake config, systemd unit, tmpfiles, sysusers, locale, man/info, desktop/appstream, config, license, doc, debuginfo.

**Что найдёт:**

| Проверка                                                  | Скрытая проблема                                   |
| --------------------------------------------------------- | -------------------------------------------------- |
| `-devel` split missing or incomplete                      | Headers/.pc/unversioned `.so` in main.             |
| Runtime package accidentally depends on development tools | `.pc` files and headers pull build ecosystem.      |
| `-lang` split candidate                                   | Большое количество locale entries manually listed. |
| systemd/tmpfiles/sysusers scriptlets missing              | Classifier feeds scriptlet rules.                  |
| License/doc misclassification                             | `%doc LICENSE` vs `%license LICENSE`.              |

**Почему это важно:** это даст много high-signal правил без фактического buildroot, потому что `%files` уже декларативно описывает payload.

### 5.6. Dependency solver for PRCO graph

**Идея:** построить graph для Provides/Requires/Conflicts/Obsoletes/Recommends/Suggests/Supplements/Enhances по всем subpackages.

**Что найдёт:**

| Проверка                                                   | Скрытая проблема                                                                                    |
| ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| Strong/weak dep redundancy                                 | `Requires: X` + `Recommends: X`.                                                                    |
| Obsoletes/Provides migration incomplete across subpackages | Переименование пакета сделано только для main, но не для `-devel`/`-libs`.                          |
| Self-cycle through rich deps                               | Условно `Requires: (self if feature)` или nonsensical self supplements.                             |
| Conflicts contradicts Provides/Obsoletes                   | Package both provides and conflicts with same virtual capability.                                   |
| Version lock inconsistency                                 | `Requires: %{name}-libs = %{version}` вместо full EVR, или mixed lockstep forms across subpackages. |

**Что нужно добавить:** normalized capability domain + EVR comparator + subpackage graph. У вас уже есть DepAtom walking и EVR normalization — база хорошая.

### 5.7. SMT-lite solver для `%if`

У вас уже есть DNF и частичный interval/string analysis. Следующий шаг — домены:

| Домен             | Что даст                                                                              |   |                |
| ----------------- | ------------------------------------------------------------------------------------- | - | -------------- |
| Integer macros    | `0%{?rhel} >= 8 && 0%{?rhel} < 8`, `fedora == 40                                      |   | fedora == 41`. |
| Arch sets         | `%ifarch x86_64` vs `%ifarch i686` как mutually exclusive; subset/superset reasoning. |   |                |
| String equalities | `%{name} == "foo"` после safe macro expansion.                                        |   |                |
| RPM EVR ordering  | Conditions or dependency constraints involving versions.                              |   |                |

**Новые проверки:** stronger `unreachable-branch`, arch-branch-coverage, redundant arch conditions, contradiction between `ExclusiveArch` and `%ifarch`-guarded package sections.

### 5.8. Build-system recognizer and modernization engine

**Идея:** распознавать build-system idioms и давать profile-gated suggestions.

| Pattern                                                            | Suggestion                                                         |
| ------------------------------------------------------------------ | ------------------------------------------------------------------ |
| `%cmake` + `%cmake_build` + `%cmake_install` on rpm ≥ 4.20 profile | Optional `BuildSystem: cmake` / `BuildOption:` migration.          |
| Manual `python3 -m pip install .` with pyproject metadata markers  | `%pyproject_wheel`, `%pyproject_install`, `%pyproject_save_files`. |
| Manual `meson setup`, `ninja`, `ninja install`                     | `%meson`, `%meson_build`, `%meson_install`.                        |
| `cargo build` / `go build` without distro macros                   | `%cargo_*` / `%gobuild` where profile provides them.               |

**Важно:** это должен быть `Allow` или opt-in `Performance/Style`, потому что cross-distro macros сильно различаются.

### 5.9. Repository-level analyzer

Если позже появится multi-spec анализ, это даст проверки, невозможные на одном spec:

| Проверка                                                    | Что ловит                                                            |
| ----------------------------------------------------------- | -------------------------------------------------------------------- |
| Duplicate Provides across unrelated specs                   | Repository conflict.                                                 |
| Rename/split migration consistency                          | New package has `Obsoletes/Provides`, old subpackages accounted for. |
| Shared macro drift                                          | В одном repo разные specs копируют разные версии одного `%global`.   |
| Cross-spec dependency cycles                                | Особенно bootstrap / conditional build cycles.                       |
| Inconsistent license/source URL style across package family | Maintainer hygiene.                                                  |

---

## 6. Приоритет внедрения

Я бы ранжировал так.

### P0 — высокий сигнал, реализуемо сейчас

1. `patch-applied-more-than-once`
2. `duplicate-singleton-tag`
3. `subpackage-name-collision`
4. `invalid-name-version-release-epoch-format`
5. `source-patch-list-mixing`
6. `autoreqprov-disabled-without-comment`
7. `buildarch-reparse-hazard`
8. `changelog-order-weekday-evr`
9. `scriptlet-exit-not-guaranteed-zero`
10. `scriptlet-upgrade-test-eq-two`
11. `direct-systemctl-in-scriptlet`
12. `systemd-unit-under-etc-or-config`
13. `etc-file-not-config`
14. `config-under-usr`
15. `license-file-marked-doc`
16. `devel-file-in-non-devel-package`
17. `locale-file-not-lang`
18. `rm-rf-buildroot-in-install`
19. `makeinstall-without-underscore`
20. `install-writes-outside-buildroot`

### P1 — очень полезно, но нужен профильный справочник

1. `build-tool-used-without-buildrequires`
2. `scriptlet-command-without-requires`
3. `systemd-unit-without-helper-macros`
4. `tmpfiles-without-create`
5. `pkgconfig-file-without-pkgconfig-br`
6. `unsupported-dependency-feature`
7. `release-disttag-policy`
8. `source-version-mismatch`
9. `optflags-overridden`
10. `network-access-in-build`

### P2 — глубокие/дорогие проверки

1. Symbolic effective spec evaluator
2. Shell/scriptlet CFG analyzer
3. `%install` write-set vs `%files` read-set
4. PRCO graph solver
5. SMT-lite solver for arch/integer/string/EVR conditions
6. Declarative build migration suggestions
7. Repository-level analyzer

---

## 7. Что лучше не делать первым

Не стоит начинать с проверок, требующих реального buildroot или unpacked source tree: “файл действительно существует”, “URL реально скачивается”, “архив содержит нужную директорию”, “SONAME bump корректен”, “binary has RPATH”, “ELF under `/usr/share`”. Это зоны rpmlint/post-build checks, а не spec-only анализатора. openSUSE packaging checks прямо разделяют brp scripts, post-build checks и rpmlint, потому что у них разный доступ к buildroot/build result. ([openSUSE Wiki][5])

Также стоит избегать жёстких distro-neutral правил по systemd dependencies: Fedora и openSUSE здесь расходятся, поэтому одно и то же правило должно менять рекомендацию по `Profile.family`. ([docs.pagure.org][2])

---

## 8. Практическая архитектурная рекомендация

Для следующего этапа полезно добавить три reusable компонента:

1. **`FilesClassifier`**
   Вход: `%files` entries + profile macro expansion.
   Выход: classified paths: config, license, doc, devel, locale, systemd unit, tmpfiles, sysusers, library, plugin, debuginfo, standard dir, volatile path.

2. **`CommandUseIndex`**
   Вход: shell-bearing sections/scriptlets.
   Выход: команды, абсолютные пути, write targets, service operations, network operations, user/group operations, build-system markers.

3. **`ProfilePolicyRegistry`**
   Не только macro registry, а policy map:
   `systemd_macros`, `scriptlet_required_deps`, `standard_dirs`, `devel_patterns`, `build_tool_to_buildrequires`, `runtime_tool_to_requires`, `supported_rpm_features`, `disttag_policy`, `config_policy`.

Это позволит добавлять новые правила не как набор regex’ов, а как composable analyses. Самый быстрый выигрыш даст связка `FilesClassifier + CommandUseIndex`: она покрывает systemd/tmpfiles/config/devel/lang/scriptlet deps/install-to-buildroot — то есть самые болезненные зоны, которые сейчас почти не закрыты.

[1]: https://rpm.org/docs/4.20.x/manual/spec.html "rpm.org - Spec file format"
[2]: https://docs.pagure.org/packaging-guidelines/Packaging%3AScriptlets.html "Blog Template for Bootstrap"
[3]: https://en.opensuse.org/openSUSE%3ASpecfile_guidelines "openSUSE:Specfile guidelines - openSUSE Wiki"
[4]: https://rpm-software-management.github.io/rpm/manual/buildsystem.html "rpm.org - Declarative builds"
[5]: https://en.opensuse.org/openSUSE%3APackaging_checks "openSUSE:Packaging checks - openSUSE Wiki"
[6]: https://wiki.mageia.org/en/Packaging_guidelines?utm_source=chatgpt.com "Packaging guidelines"
[7]: https://en.opensuse.org/openSUSE%3ASystemd_packaging_guidelines "openSUSE:Systemd packaging guidelines - openSUSE Wiki"
[8]: https://discussion.fedoraproject.org/t/dotnet-restore-nuget-packages-at-build/143498?utm_source=chatgpt.com "Dotnet restore nuget packages at build - Fedora Discussion"
[9]: https://rpm.org/docs/4.19.x/manual/conditionalbuilds.html "rpm.org - Conditional Builds"
