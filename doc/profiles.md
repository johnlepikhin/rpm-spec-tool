# Профили дистрибутивов

Профиль — это описание целевого окружения сборки: identity дистрибутива
(family / vendor / dist-tag), макросы, rpmlib-фичи, whitelists лицензий
и групп. Лента ничего не «угадывает» о системе — она читает то, что
профиль ей сообщил.

Профили задаются в `.rpmspec.toml`. Дополнительно можно подложить дамп
команды `rpm --showrc`, выполненной на целевой машине: парсер
анализатора извлечёт оттуда identity и все макросы автоматически.

## Минимальный конфиг

```toml
profile = "rhel-9"

[profiles.rhel-9]
showrc-file = "vendor/rpm-showrc-rhel9.txt"
```

Дамп `vendor/rpm-showrc-rhel9.txt` генерируется на целевой машине:

```bash
rpm --showrc > vendor/rpm-showrc-rhel9.txt
```

Этого достаточно: identity (`family=RHEL`, `vendor=redhat`,
`dist-tag=.el9`) и все 700+ макросов извлекаются из дампа
автоматически.

## Активный профиль

Выбор активного профиля идёт в порядке:

1. CLI-флаг `--profile <name>` (для подкоманды `lint` / `check`).
2. Ключ `profile = "<name>"` в `.rpmspec.toml`.
3. Встроенный профиль `generic` (пустой template).

Имя `<name>` может ссылаться на:

- встроенный профиль (см. список ниже в разделе «Встроенные профили
  дистрибутивов»);
- ключ из секции `[profiles.<name>]`.

Несколько `[profiles.*]` могут сосуществовать в одном конфиге — это
удобно, если репозиторий собирает один и тот же spec под несколько
целевых дистрибутивов. Активен всегда **ровно один** профиль за прогон;
никакого автоматического слияния разных профилей нет.

## Слои профиля

При резолве активного профиля анализатор накладывает слои в порядке от
низкого приоритета к высокому:

1. **Builtin baseline** — `data/<extends>.toml` (по умолчанию `generic`).
2. **Builtin showrc layer** — для дистрибутивных встроенных профилей в
   бинарь вшит реальный дамп `rpm --showrc` с целевой машины
   (`data/<name>.showrc`). Применяется сразу за TOML-метаданными.
3. **Showrc layer** — содержимое файла из `showrc-file`, если задан.
   Накладывается поверх builtin-showrc; user-дамп выигрывает при
   коллизиях имён макросов.
4. **Auto-detect identity** — `vendor`, `dist-tag`, `family` выводятся
   из showrc-макросов (только для полей, которые пользователь не задал
   явно). Auto-detect отрабатывает по обоим showrc-слоям.
5. **Inline overrides** — секции `[profiles.<name>.*]` в конфиге.

Подкоманда `rpm-spec-tool profile show [NAME]` печатает резолвнутый
профиль и список применившихся слоёв. С `--full` выводится весь
макрорегистр с пометками источника.

## Auto-detect identity

Из showrc извлекаются:

| Поле в `Profile` | Источник в showrc |
|---|---|
| `identity.vendor` | макрос `_vendor` (literal value) |
| `identity.dist_tag` | макрос `dist` (literal value) |
| `identity.family` | первый из маркеров: `altlinux`, `mageia`, `suse_version`, `rhel`, `fedora` |

Приоритет в выборе family фиксированный. Производные дистрибутивы
(AlmaLinux, Rocky, CentOS Stream) экспонируют сразу несколько маркеров —
порядок гарантирует, что они оказываются в семействе родителя. Любое
поле, которое пользователь явно прописал в `[profiles.X.identity]`,
выигрывает у auto-detect.

## Полный конфиг

```toml
profile = "rhel-9-prod"

[profiles.rhel-9-prod]
extends = "generic"                          # базовый builtin; default = "generic"
showrc-file = "vendor/rpm-showrc-rhel9.txt"  # путь относительно .rpmspec.toml

# Все секции ниже — опциональны. Прописываются, только если нужно
# переопределить то, что пришло из showrc / builtin.

[profiles.rhel-9-prod.identity]
name = "RHEL 9 production"                   # человекочитаемый label
family = "rhel"                              # fedora | rhel | opensuse | alt | mageia | generic
vendor = "mycompany"
dist-tag = ".mc1"

[profiles.rhel-9-prod.macros]
# Короткая форма: literal value.
_vendor = "mycompany"
# Расширенная форма (для параметризованных или multiline макросов).
custom = { value = "...", opts = "(n:)" }

[profiles.rhel-9-prod.licenses]
mode = "strict"            # off (default) | warn | strict
replace = false            # false (default) = union с builtin/showrc; true = заменить целиком
allow = ["GPL-2.0-or-later", "Proprietary"]

[profiles.rhel-9-prod.groups]
mode = "warn"
allow = ["System Environment/Daemons"]

# Второй профиль — например, для сборки под ALT.
[profiles.altp10]
showrc-file = "vendor/rpm-showrc-altp10.txt"
```

## Семантика слияния

- **Macros**: при конфликте имени последний слой выигрывает; в записи
  обновляется `provenance` (виден в `profile show --full`).
- **Identity-поля**: explicit override в `[profiles.X.identity]`
  выигрывает над auto-detect.
- **Licenses / groups**: при `replace = false` (default) списки объединяются
  union'ом; при `replace = true` сбрасываются и переинициализируются.
  Поле `mode` — sticky: оно меняется только тогда, когда какой-то слой
  его задал явно. Default — `off` (соответствующие ленты молчат).

## Подкоманды `profile`

### `profile list`

Табличный обзор всех доступных профилей: встроенных и определённых в
`.rpmspec.toml`. Активный профиль помечен `*` в первой колонке.

```bash
# Полный список (builtin + user).
rpm-spec-tool profile list

# Только встроенные.
rpm-spec-tool profile list --builtin-only

# Только user-профили из найденного .rpmspec.toml.
rpm-spec-tool profile list --user-only
```

Для встроенных колонки: `NAME · FAMILY · VENDOR · DIST-TAG · MACROS · ARCH`.
Для user-профилей: `NAME · EXTENDS · DETAILS` (DETAILS суммирует
`showrc-file` и перечисляет переопределённые секции — `vendor`, `family`,
`macros`, `licenses`, …).

### `profile show`

Подробности по одному профилю — identity, цепочка слоёв, счётчики.

```bash
# Показать профиль, выбранный конфигом или CLI.
rpm-spec-tool profile show

# Резолвить конкретный профиль по имени.
rpm-spec-tool profile show rhel-9-x86_64

# Вывести весь макро-регистр (с provenance каждой записи).
rpm-spec-tool profile show --full
```

Полезно при отладке: с `--full` дампит каждый макрос с пометкой источника
(`showrc:-13`, `override`, `builtin:generic`).

### `profile macros`

Список макрорегистра одного профиля с фильтрацией по имени и/или
источнику. В отличие от `show --full`, не печатает identity-блок и
позволяет сузить вывод.

```bash
# Все макросы активного профиля.
rpm-spec-tool profile macros

# Все макросы конкретного профиля.
rpm-spec-tool profile macros rhel-9-x86_64

# По подстроке имени (case-insensitive).
rpm-spec-tool profile macros rhel-9-x86_64 --filter optflags

# По источнику: builtin / showrc / override.
rpm-spec-tool profile macros rhel-9-x86_64 --source override
```

Колонки выравнены, длинные multiline-значения сворачиваются в маркер
`<multiline N chars>` — для полного тела используй `profile macro`.

### `profile common`

Пересечение макрорегистров двух или более профилей — отвечает на вопрос
«какие макросы общие». Два режима через `--mode`:

* **`--mode existence`** (по умолчанию) — макрос считается общим, если
  определён во всех профилях, независимо от значения. Выводит просто
  список имён.
* **`--mode value`** — добавляет требование одинакового значения (`opts`
  + тело макроса). `Provenance` игнорируется: макрос, унаследованный
  из showrc в одном профиле и переопределённый в `.rpmspec.toml`
  в другом, считается одинаковым при совпадении значения. Выводит
  `name = value`.

```bash
# Без аргументов: пересечение по всем builtin-профилям с непустым
# регистром (т.е. без generic).
$ rpm-spec-tool profile common
# Common macros across 23 profile(s): 188

  ___build_args
  ___build_cmd
  ...

# Конкретный набор профилей по существованию.
$ rpm-spec-tool profile common rhel-8-x86_64 rhel-9-x86_64 rhel-10-x86_64
# Common macros across 3 profile(s): 292

  __7zip
  ___build_args
  ...

# То же по значению — узкий срез действительно портативных дефолтов.
$ rpm-spec-tool profile common --mode value rhel-8-x86_64 rhel-9-x86_64 rhel-10-x86_64
# Macros with identical values across 3 profile(s): 242

  __7zip          = /usr/bin/7za
  ___build_args   = -e
  ___build_shell  = %{?_buildshell:%{_buildshell}}%{!?_buildshell:/bin/sh}
  ...

# С фильтром по имени.
$ rpm-spec-tool profile common --filter build rhel-8-x86_64 rhel-9-x86_64
# Common macros across 2 profile(s): 358 total, 45 matching "build"
  ...
```

Длинные значения в `--mode value` обрезаются до 80 символов,
multiline-тела сворачиваются в `<multiline N chars>`. Минимум два
профиля — одиночный аргумент отклоняется с exit code `2`. Пустое
пересечение — exit `0` с маркером `(no common macros)`.

При активном `--filter` заголовок печатает оба числа:
`# Common macros across N profile(s): {total} total, {matching} matching "X"`
— total — размер полного пересечения, matching — после применения фильтра.

### `profile macro`

Универсальный lookup значения одного макроса. Поведение зависит от
количества переданных профилей:

| Аргументы            | Поведение                                                  | Exit code           |
| -------------------- | ---------------------------------------------------------- | ------------------- |
| `<macro>`            | Таблица значения по **всем доступным** профилям            | `0`                 |
| `<macro> <p>`        | Компактный вывод значения в одном профиле; multiline-тело раскрыто построчно | `0` / `2` если не определён |
| `<macro> <p1> <p2>…` | Таблица сравнения по перечисленным профилям                | `0`                 |

```bash
# Сравнить макрос по всем профилям (24 builtin + user-определённые).
$ rpm-spec-tool profile macro dist
# Macro `dist` across 24 profile(s)

  generic                 = (undefined)
  rhel-8-x86_64           = .el8                                [showrc:-13]
  rhel-9-x86_64           = %{!?distprefix0:...}.el9%{?distsuf… [showrc:-13]
  altlinux-10-x86_64      = (undefined)
  ...

# Конкретный профиль — компактно, с раскрытием multiline-тела.
$ rpm-spec-tool profile macro dist rhel-9-x86_64
dist = %{!?distprefix0:...}.el9...  [showrc:-13]

$ rpm-spec-tool profile macro ___build_pre rhel-9-x86_64
___build_pre =  [showrc:-13]
    RPM_SOURCE_DIR="%{u2p:%{_sourcedir}}"
    RPM_BUILD_DIR="%{u2p:%{_builddir}}"
    ...

# Сравнить произвольный набор профилей.
$ rpm-spec-tool profile macro dist rhel-8-x86_64 rhel-9-x86_64 altlinux-10-x86_64
# Macro `dist` across 3 profile(s)

  rhel-8-x86_64      = .el8                                  [showrc:-13]
  rhel-9-x86_64      = %{!?distprefix0:...}.el9...           [showrc:-13]
  altlinux-10-x86_64 = (undefined)
```

В режимах с таблицей длинные значения обрезаются до 80 символов; полное
тело — через одно-профильный вариант. Exit code `2` возможен **только**
в одно-профильном режиме (макрос не определён), что удобно для shell:

```bash
if ! rpm-spec-tool profile macro __python3 sles-15-x86_64 >/dev/null; then
    echo "macro missing — fall back"
fi
```

## Встроенные профили дистрибутивов

В бинарь вшиты предопределённые профили для типовых целевых систем —
работают без `.rpmspec.toml`:

```bash
rpm-spec-tool check --profile rhel-9-x86_64 build.spec
rpm-spec-tool profile show altlinux-10-e2k
```

Каждый дистрибутивный профиль включает дамп `rpm --showrc`, снятый с
живой машины: настоящий макрорегистр (400–600 макросов), rpmlib-фичи,
arch / build-os, и identity (`family` / `vendor` / `dist-tag`).
Identity для распознанных через marker-макросы distro вычисляется
автоматически; для тех, где marker отсутствует (REDos, ALT Linux,
Rosa, MOSos), `family` зафиксирована в `data/<name>.toml`.

| Семейство     | Профили                                                                                                                                |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| RHEL          | `rhel-8-x86_64`, `rhel-8-aarch64`, `rhel-9-x86_64`, `rhel-9-aarch64`, `rhel-10-x86_64`, `rhel-10-aarch64`                              |
| REDos         | `redos-7.3-x86_64`, `redos-7.3-aarch64`, `redos-8-x86_64`, `redos-8-aarch64`                                                          |
| ALT Linux     | `altlinux-10-x86_64`, `altlinux-10-aarch64`, `altlinux-10-e2k`, `altlinux-10-e2kv4`, `altlinux-11-x86_64`, `altlinux-11-aarch64`      |
| ALT Linux SPT | `altlinux-spt-10-x86_64`, `altlinux-spt-10-aarch64`, `altlinux-spt-10-e2k`, `altlinux-spt-10-e2kv4`                                    |
| openSUSE-like | `sles-15-x86_64`, `mosos-15-x86_64`                                                                                                    |
| Rosa          | `rosa-2021.1-x86_64`                                                                                                                   |
| baseline      | `generic` (пустой template, fallback по умолчанию)                                                                                     |

Любой встроенный профиль можно использовать как базу для собственных
override'ов:

```toml
profile = "ourbuild"

[profiles.ourbuild]
extends = "rhel-9-x86_64"

[profiles.ourbuild.identity]
vendor = "mycompany"
dist-tag = ".mc1"

[profiles.ourbuild.macros]
_vendor = "mycompany"
```

## Связь с ленточными правилами

В данном PR профиль доступен через `Lint::set_profile` (метод трейта
`Lint` в анализаторе), но существующие правила его пока не читают. Профильно-зависимые правила
(RPM024 invalid-license, RPM025 non-standard-group, RPM030
requires-no-version, RPM050 hardcoded-paths) подключаются отдельными
PR'ами и опираются на `profile.licenses`, `profile.groups` и
`profile.macros`. При `mode = "off"` (default) такие правила не должны
эмиттить ни одного диагноса — это часть контракта.
