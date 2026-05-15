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

## Phase 6 — Conditional-block lints (✅ implemented)

Все правила покрывают AST-уровневые условные блоки в трёх контекстах
(top-level `SpecItem`, `%package` preamble, `%files`). Условные блоки
**внутри shell-bodies** (`%build`/`%install`/scriptlets) не парсятся
структурно — это известное ограничение upstream-парсера. Любое
условное выражение, содержащее `%`-макрос, вызывает conservative
bail-out: мы не пытаемся резолвить макросы.

| ID     | Name                              | rpmlint analog | Default | Auto-fix          | Notes |
|--------|-----------------------------------|----------------|---------|-------------------|-------|
| RPM070 | deep-conditional-nesting          | n/a            | warn    | —                 | ✅ phase 6; depth > 4 (hardcoded; TODO per-lint config); правило срабатывает на каждом уровне > порога, не только на самом глубоком |
| RPM071 | unreachable-elif-branch           | n/a            | warn    | —                 | ✅ phase 6; `%elif` с тем же выражением что у одной из ранних веток; conservative bail-out на макросах |
| RPM072 | constant-condition                | n/a            | warn    | Manual            | ✅ phase 6; `%if 1`/`%if 0`/`%if true`/`%if false`/`%if ""` |
| RPM073 | empty-conditional-branch          | n/a            | warn    | MachineApplicable | ✅ phase 6; все ветки + `%else` содержат только Blank/Comment; auto-fix Manual в v1 (требует span-info для keywords) |
| RPM074 | identical-conditional-branches    | n/a            | warn    | Manual            | ✅ phase 6; source-slice сравнение (line-trim-end normalized); silent при body с не-Span-noisable items (`Statement`/`Blank`) |
| RPM075 | redundant-nested-condition        | n/a            | warn    | Manual            | ✅ phase 6; stack ancestors; срабатывает на ЛЮБОМ совпадении с предком, не только с прямым родителем |
| RPM076 | adjacent-mergeable-conditionals   | n/a            | warn    | Manual            | ✅ phase 6; v1 ловит только simple-conditional pairs (один branch, no else); blank lines между блоками не нарушают adjacency |
| RPM077 | ifarch-empty-list                 | n/a            | warn    | —                 | ✅ phase 6; `%ifarch`/`%ifos`/`%ifnarch`/`%ifnos` + `%elifarch`/`%elifos` с пустым ArchList |

**Auto-fix общий статус:** все Phase 6 правила в `Manual` v1, потому
что вычисление byte-ranges для splicing keywords (`%if`/`%endif`/
`%else`) требует per-keyword span-инфраструктуры у `Conditional` /
`CondBranch`, которой пока нет в AST. Diagnostics указывают на блок
целиком; auto-fix-pass отдан на ручное решение автора.

**Что отложено в Phase 6b:**
- Условные внутри shell-bodies (`%build`/`%install`) — нужен upstream-парсер.
- `conditional-name-tag` (`Name:` внутри `%if`) — больше для downstream linter'ов.

## Phase 7 — Conditional optimisation / simplification (✅ implemented)

Расширение Phase 6 в сторону «упростить сложное дерево условий».
Все ID впервые присвоены здесь, после того как Phase 6 закрыла
структурные проверки. Реализация — top-5 первой итерации; остальные
в Phase 7b.

### Реализовано (первая итерация)

| ID     | Name                          | Default | Auto-fix | Что ловит |
|--------|-------------------------------|---------|----------|-----------|
| RPM080 | nested-and-collapse           | warn    | Manual   | ✅ phase 7; `%if A %if B FOO %endif %endif` → `%if (A) && (B) FOO %endif`. Прямо снижает depth (борется с RPM070). Только для plain `%if` (arch/os не комбинируется через `&&`). Сохраняет blank/comment между блоками. |
| RPM081 | empty-else-drop               | warn    | Manual   | ✅ phase 7; `%else` с пустым телом при непустых `%if`/`%elif`. Если все ветки пустые — пропускаем (это уже RPM073). |
| RPM082 | invert-empty-if-arch          | warn    | Manual   | ✅ phase 7; `%ifarch X %else FOO %endif` (пустой `%if`-блок) → `%ifnarch X FOO %endif`. Только для arch/os kinds (где flip — просто смена ключевого слова). |
| RPM085 | constant-tautology-in-expr    | warn    | Manual   | ✅ phase 7; паттерны `X \|\| 1`, `1 \|\| X`, `X && 0`, `0 && X` в литеральной части выражения. Поддерживает `true`/`false` (normalised). Conservative bail-out на `%`. |
| RPM087 | double-negation-in-expr       | warn    | Manual   | ✅ phase 7; буквальное `!!` в выражении (включая случаи с макросами после `!!`). Не ловит `! !` с пробелом (нужна реальная токенизация). |

Все auto-fix в v1 — **Manual** по той же причине что и Phase 6:
parsed `Conditional` / `CondBranch` несут span только всего блока,
без позиций ключевых слов (`%if`/`%else`/`%endif`/`%elif`) и
expression-текста. Когда AST научится отдавать эти спаны (upstream-fix
в rpm-spec), переведём в `MachineApplicable`.

## Phase 7b — Extended conditional lints (✅ implemented)

Prerequisite: upstream-расширение `rpm-spec` — добавлен полноценный
`ExprAst<T>` AST для `%if`/`%elif`-выражений (новый модуль
`crates/rpm-spec/src/ast/expr.rs` + `crates/rpm-spec/src/parser/expr.rs`).
`CondExpr` теперь `pub enum CondExpr<T>` с тремя вариантами:
`Raw(Text)` (fallback), `Parsed(Box<ExprAst<T>>)` (typed), `ArchList(...)`.
Парсер пытается structured-parse; на неудаче — `Raw` fallback (бекоторая
совместимость сохраняется).

Это разблокировало **MachineApplicable** auto-fix для трёх правил
(RPM086/088/091) — раньше все Phase 6/7 правила были `Manual` из-за
отсутствия per-token spans.

| ID     | Name                              | Default | Auto-fix          | Notes |
|--------|-----------------------------------|---------|-------------------|-------|
| RPM083 | collapse-elif-into-else           | warn    | Manual            | ✅ phase 7b; финальный `%elif <true>` → `%else`. Использует `is_constant_true_condition` (теперь Parsed-aware). |
| RPM084 | if-not-x-after-if-x               | warn    | Manual            | ✅ phase 7b; arch-only — `%ifarch X foo %endif %ifnarch X bar %endif` → `%ifarch X foo %else bar %endif`. Plain `%if` отложен. |
| RPM086 | idempotent-in-expr                | warn    | Manual            | ✅ phase 7b; AST-walking ловит `X && X` / `X \|\| X`. v1 Manual — для MachineApplicable нужны parens-spans. |
| RPM088 | self-comparison-in-expr           | warn    | Manual            | ✅ phase 7b; AST distinguishes `Eq/Le/Ge` (always-true) vs `Ne/Lt/Gt` (always-false). |
| RPM089 | single-comment-only-branch        | warn    | —                 | ✅ phase 7b; ветка содержит ровно один `Comment` без других items. |
| RPM090 | ifarch-noarch                     | warn    | —                 | ✅ phase 7b; `noarch` token в ArchList. |
| RPM091 | duplicate-arch-in-list            | warn    | **MachineApplicable** | ✅ phase 7b; source-byte dedupe edit (word-boundary search в branch span). |
| RPM092 | conditional-cyclomatic-complexity | warn    | —                 | ✅ phase 7b; общая сумма веток > 15 — proxy для spec-wide сложности. |
| RPM093 | condition-mentioned-many-times    | warn    | —                 | ✅ phase 7b; HashMap-aggregation по canonicalised expression text, threshold 5. Срабатывает на каждое вхождение. |
| RPM094 | line-continuation-in-condition    | warn    | —                 | ✅ phase 7b; `\n` в literal после `logical_line` join — RPM не поддерживает многострочные `%if`. |
| RPM095 | prefer-bcond-for-build-options    | allow   | Manual            | ✅ phase 7b; точный pattern `0%{?with_NAME}` → suggest `%bcond_with NAME` + `%{with NAME}`. Default Allow. |
| RPM096 | if-only-buildrequires             | allow   | Manual            | ✅ phase 7b; ветки содержат только `BuildRequires:` items. Default Allow. |

**Auto-fix общий статус Phase 7b:**
- **3 MachineApplicable** (RPM091; RPM086/088 — потенциально MachineApplicable когда добавим helper для byte-precise rewrites операндов).
- 7 Manual (требуют semantic-level решения от автора, или нужны keyword-level spans на `%if`/`%endif` — отдельная upstream-задача).
- 2 без auto-fix (RPM089 — hint; RPM092 — нужен manual refactor; RPM093 — нужен выбор имени `%global`).

**Smoke на real-world third-party spec** показывает реальный value:
- 31 × RPM093 (condition-mentioned-many-times) — `0%{?rhel}`/`0%{?fedora}` повторяются десятками раз, кандидаты на `%global`.
- 1 × RPM092 (cyclomatic-complexity) — общая сложность дерева выше 15.

### Out-of-scope (отдельные PR / нужен upstream)

- Cross-section refactoring (общие условия в разных секциях) — слишком
  сложно автоматизировать корректно без интерпретации макросов.
- Авто-фиксы для всех Phase 6/7 — нужен upstream-fix в `rpm-spec`
  (per-keyword spans на `Conditional`/`CondBranch`).
- Semantic anti-patterns в `%global`/`%define` внутри веток — частично
  ловит RPM032 macro-redefinition.

## Phase 7c — Multi-branch refactoring (in progress)

Фокус: оптимизации, работающие **на уровне всей elif-цепочки** или
всего блока — там, где упрощение требует знания всех веток сразу, а
не отдельной ветки. Дополняет Phase 7b (local-expression-level).

### Реализуется (первая итерация)

| ID     | Name                              | Default | Auto-fix          | Notes |
|--------|-----------------------------------|---------|-------------------|-------|
| RPM097 | hoist-common-prefix-from-branches | warn    | MachineApplicable | Все ветки + `%else` начинаются с одних и тех же items — вынести их перед блоком. Часто встречается с `BuildRequires:` строками. |
| RPM098 | hoist-common-suffix-from-branches | warn    | MachineApplicable | Симметрично: общее окончание выносится после `%endif`. |
| RPM099 | merge-elif-same-body              | warn    | Manual            | Соседние `%elif`-ы с идентичными телами объединяются через `\|\|`. v1 Manual — нужны spans expression. |
| RPM100 | collapse-else-if-into-elif        | warn    | Manual            | `%else %if Y ... %endif %endif` → `%elif Y ... %endif`. Снижает depth на 1. |
| RPM101 | absorption-in-expr                | warn    | Manual            | `A \|\| (A && B)` → `A`, `A && (A \|\| B)` → `A`. ExprAst pattern match. |

RPM097/098 — source-slice based; используют `set_source` и сравнение
лексических байт. RPM099/100 ловят структурно; auto-fix Manual до
появления per-keyword spans на `Conditional`/`CondBranch`.
RPM101 — pure ExprAst walking, без source.

## Phase 7d — Interval analysis + structural anti-patterns (✅ implemented)

Закрывает последние 6 правил Phase 7-серии. Тема — то, что требует
более тяжёлой логики (interval-analysis на `&&`-цепочках, обход тел
условных блоков на конкретные теги). Refactor: `exprs_equiv` поднят
из `conditional_optimize.rs` в `util.rs` как `pub(crate)` — теперь
используется в RPM086/088/101/102/103/104.

| ID     | Name                          | Default | Auto-fix | Notes |
|--------|-------------------------------|---------|----------|-------|
| RPM102 | inequality-redundancy         | warn    | Manual   | ✅ phase 7d; `X >= 8 && X >= 6` — слабый bound поглощён сильным. Конструирует `IntConstraint` из `Binary{cmp, lhs, Integer}` пар, группирует по `exprs_equiv(lhs)`, проверяет `implies(a, b)`. v1: только top-level `&&`-chain, integer rhs only. |
| RPM103 | inequality-contradiction      | warn    | —        | ✅ phase 7d; `X >= 10 && X < 5` — пустое пересечение. Тот же `IntConstraint`-pipeline, `contradicts(a, b)` проверяет `min > max`. Special-case: `X > 6 && X < 6` тоже ловит (boundary contradiction). |
| RPM104 | string-set-redundancy         | warn    | Manual   | ✅ phase 7d; `X == "a" \|\| X == "b" \|\| X == "a"` — дубль. Собирает `(lhs_key, string_value)` пары из top-level `\|\|`-chain, ищет совпадение по `exprs_equiv(lhs) && s1 == s2`. |
| RPM105 | inverted-if-else              | warn    | Manual   | ✅ phase 7d; `%if !X foo %else bar %endif` → suggest swap. Pattern: `Parsed(Not{..})` или `Raw` начинается с `!` (не `!=`). Только plain `%if` (arch-формы покрыты RPM082). |
| RPM106 | conditional-buildarch         | allow   | —        | ✅ phase 7d; `BuildArch:` внутри `%if` — last-wins semantics RPM делает поведение хрупким. Default Allow — иногда legit на flavored packages. Anchor на span самого tag-item. |
| RPM107 | conditional-name-tag          | allow   | —        | ✅ phase 7d; `Name:` внутри `%if` — package имеет разные имена в разных профилях. Default Allow по той же причине. |

**Refactor:** `expr_ast_eq` (приватная в util.rs) и `exprs_equiv`
(приватная в conditional_optimize.rs) были code-identical. Объединены
в `pub(crate) exprs_equiv` в `util.rs`; теперь переиспользуется в 6
правилах (RPM086, RPM088, RPM101, RPM102, RPM103, RPM104) и сохраняет
single source of truth для structural-equality.

**Auto-fix общий статус Phase 7d:**
- 4 Manual (RPM102/104/105 — нужны spans операторов; RPM103 — нет
  очевидного фикса для contradiction).
- 2 без фикса (RPM106/107 — семантическое решение автора).

**Ограничения v1:**
- RPM102/103 — только integer rhs, только top-level `&&`-chain;
  `||`/`!` пропускаются (conservative).
- RPM104 — только string rhs, только top-level `||`-chain.
- RPM106/107 — anchor на конкретном tag-item (а не на блоке), чтобы
  пользователь видел точную строку.

## Phase 8 — Unified condition analysis (planned)

После 36 правил Phase 6/7 многие из них — частные случаи общих
compiler-side техник: dataflow analysis, abstract interpretation,
boolean normalization, SMT. Этот этап вводит **унифицирующую
инфраструктуру** и заменяет/обобщает несколько existing правил.

### Phase 8a — Boolean DNF normalizer (✅ implemented)

DNF (Disjunctive Normal Form) — каноничная форма для булевых
выражений. Любое `%if EXPR` после De Morgan + distribution
представляется как `Dnf = Set<Cube>`, где `Cube = Set<Literal>` —
конъюнкция атомов или их отрицаний. Сравнение двух DNF даёт
**семантическое** равенство булевой части (не syntactic).

**Инфраструктура** (`crates/analyzer/src/rules/boolean_dnf.rs`):
- `AtomTable` — интернирует атомарные подвыражения по canonical-text.
- `to_dnf(&ExprAst) -> Option<Dnf>` — De Morgan + distribution с
  explosion guard (bail out при > 100 cubes).
- `simplify_subsumption(dnf)` — удаляет cubes, доминирующие другими
  (`{A} ∪ {A,B}` → `{A}`).
- `is_tautology(dnf, n_atoms)` — truth-table enumeration для n ≤ 8.
- `is_contradiction(dnf)` — empty DNF после filter cube-contradictions.

**Новые правила** (RPM110-112, все `warn`, все Manual fix):
| ID | Что ловит | Пример |
|----|-----------|--------|
| RPM110 | boolean-dnf-redundancy | `(A && B) \|\| (A && B && C)` — второй cube доминирован первым |
| RPM111 | boolean-tautology-by-cubes | `X \|\| !X` — всегда true (truth-table) |
| RPM112 | boolean-contradiction-by-cubes | `X && !X` — всегда false (cube-internal contradiction) |

### Phase 8b — Path-condition engine (✅ implemented)

При обходе AST носим **path_cond** — накопленную конъюнкцию всех
ancestor-условий. На каждом branch entry спрашиваем у solver-а
(reuse Phase 8a DNF/SAT):

- `UNSAT(path_cond ∧ branch_cond)` → dead branch
- `path_cond → branch_cond` (truth-table enumeration в `path_implies`)
  → branch always true under parent

Это **обобщает**:
- RPM072 (constant-condition) — частный случай UNSAT/TAUTOLOGY на пустом path_cond
- RPM075 (redundant-nested-condition) — syntactic-equality case implication
- RPM103 (inequality-contradiction) — для interval domain

**Инфраструктура** (`crates/analyzer/src/rules/path_cond.rs`, ~220 строк):
- `PathConditions { atoms: AtomTable, stack: Vec<Option<Dnf>> }` —
  `None` frame = tainted (некоторый ancestor unparseable).
- `cond_to_dnf(CondExpr, atoms, negate)` поддерживает `Parsed`,
  `Raw` (→ taint), и `ArchList` (атомы вида `"ARCH=<name>"`).
- `branch_effective_dnf` / `else_effective_dnf` собирают эффективное
  условие ветви через `cond_N ∧ ¬cond_{N-1} ∧ ... ∧ ¬cond_0`.
- `path_implies(path, branch, n)` — truth-table на ≤ 8 атомах.

**Реализованные правила:**
- RPM113 `unreachable-branch-under-parent` — UNSAT(path ∧ branch_eff) на первом `%if`
- RPM114 `always-true-branch-under-parent` — path ⊨ branch_eff
- RPM115 `dead-elif-after-parent` — то же, что RPM113, но на `%elif`
- RPM116 `mutex-branches-spell-out-else` — chain без `%else`,
  объединение branch_eff'ов накрывает path → последний `%elif` ≡ `%else`

**Known limitations v1** (закрываются последующими фазами):
- **Integer mutex** (`X == 8` и `X == 9` — opaque-разные атомы) — Phase 8d
  через interval domain.
- **Macro propagation** (`%global X 1` где-то выше, далее `%if X == 1`) —
  Phase 8c.
- **Arch mutex** (`%ifarch i686` vs `%ifarch x86_64` — взаимоисключающие)
  — solver не знает; конечный список архитектур не захардкожен в v1.
- **`%global`/`%define` внутри ветвей** — side-effects игнорируются:
  solver работает над expression-tree, не control-flow.

### Phase 8c — Macro value propagation (✅ implemented)

Reaching-definitions для `%global`/`%define`. При обходе spec
накапливаем `MacroTable: HashMap<String, MacroBinding>` с
литеральным значением (Integer/String) тела, если оно single-segment.
Внутри `%if EXPR` post-order rewrite заменяет `%{X}`, `%{?X}`,
`%{!?X}` ссылки на найденные значения через `fold_expr`. После fold
выражение может стать constant → срабатывает RPM117 (RPM072 не
ловится, потому что исходное выражение содержало макрос).

**Реализованные правила:**
- RPM117 `macro-defined-makes-if-trivial` — после fold выражение
  свернулось в constant (Integer 0/non-zero, String empty/non-empty).
  Default: **`allow`** — `%define FLAG <default>` идиоматически
  служит CLI-knob'ом для `rpmbuild --define`; линтер не видит
  переопределений с командной строки и заваливал бы каждый knob.
  Опт-ин через `--warn macro-defined-makes-if-trivial`.
- RPM118 `unused-conditional-global` — `%global X V` определён, но
  ни одного `%{X}` / `%{?X}` / `%{!?X}` чтения. Default: **`allow`** —
  `%global` часто используется как public knob для downstream
  `rpmbuild --define`, и `${VAR}` shell-refs мы не парсим (риск FP).
  Опт-ин через `--warn unused-conditional-global`.

**Scope rules**: must-define reaching definitions. Snapshot
macro-table перед входом в `%if`/`%elif`/`%else` ветвь; на выходе
restore. Определения внутри ветви теряются после блока — иначе FP
на `%if A %global X 1 %else %global X 2 %endif`.

**Known limitations v1**:
- **Macro chains** (`%global Y %{X}`): `value: None` (conservative
  bail) — не propagate'им через несколько уровней.
- **Parametric macros** (`%define foo(b:) ...`): пропускаем — `opts`
  нетривиальны.
- **Built-in macros** (`%{_bindir}`, `%{name}`): не определены в
  spec → unknown в таблице → не fold'им.
- **Shell `${VAR}` references** в `%post`/`%pre` skiped — не
  structured macro segment.
- **EVR/DepExpr Text-fragments** в preamble values полностью не
  обходим для read-detection (RPM118 conservative).
- **Cross-branch must-define merging** — деферим: если все ветви
  определяют X одинаково, можно было бы пропагировать наружу.

### Phase 8d — SMT-style integer-relational solver (deferred, отдельный PR)

Полноценный interval+linear constraint solver для целых ineq. Раздвигает
RPM102/103 на:
- `%if X >= 10 %if X >= 5 ...` — внутренний redundant under outer.
- `%if X >= 0 && X * 2 < 10 ...` — простая linear arithmetic.

Требует более продвинутого engine. Возможно через Rust crate (`good_lp`?)
или own (~1000 строк).

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
