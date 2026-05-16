# Git hooks

Versioned git hooks for this repository. Enable once per clone:

```sh
git config core.hooksPath .githooks
```

## `pre-commit`

Regenerates [`doc/lints-list.md`](../doc/lints-list.md) when staged
changes touch the analyzer rule registry — `crates/analyzer/src/rules/**`,
`registry.rs`, `lint.rs`, or `lib.rs`. The regenerated file is staged
automatically; the hook is a no-op when no rule-related path is in the
staged diff.

The same regeneration is verified in CI by the `lints-doc-check` job,
so PRs that bypass the hook still get caught — the hook is an
ergonomics affordance, not the source of truth.
