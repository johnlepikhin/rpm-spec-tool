# rpm-spec-profile

[![CI](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/johnlepikhin/rpm-spec-tool/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/rpm-spec-profile.svg)](https://crates.io/crates/rpm-spec-profile)
[![docs.rs](https://img.shields.io/docsrs/rpm-spec-profile)](https://docs.rs/rpm-spec-profile)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Distribution profile model for the [`rpm-spec-tool`](https://crates.io/crates/rpm-spec-tool)
ecosystem. A `Profile` is the resolved target environment a `.spec` file
is analyzed against — distribution identity (family, vendor, dist-tag),
the full macro registry (typically 400–600 entries scraped from
`rpm --showrc`), rpmlib feature flags, and license / group whitelists.
Not useful in isolation: this crate is consumed by
[`rpm-spec-analyzer`](https://crates.io/crates/rpm-spec-analyzer) to
drive family-gated lints and macro-aware diagnostics.

## Overview

Profiles are layered, low to high precedence:

1. **Builtin baseline** — a TOML metadata file compiled into the binary
   (`data/<name>.toml`). 24 distribution profiles ship by default
   (RHEL 8/9/10, Fedora derivatives, ALT Linux, openSUSE, REDos, Rosa,
   plus a `generic` fallback).
2. **`rpm --showrc` dump** — verbatim output captured on the target
   machine. Distribution builtins bundle a snapshot; users may layer a
   fresh dump on top via `showrc-file = "..."`.
3. **User overrides** — `[profiles.<name>.*]` sections in
   `.rpmspec.toml` plus `rpmbuild`-style `--define NAME VALUE` on the
   CLI (always wins last).

See [`doc/profiles.md`](../../doc/profiles.md) in the workspace root
for the full reference: layer merge semantics, identity auto-detect
rules, and the catalogue of built-in profiles.

## Quick start

```rust
use std::path::Path;
use rpm_spec_profile::{
    MacroValue, ProfileSection, ResolveOptions, resolve_profile,
};

// Resolve the `rhel-9-x86_64` built-in profile — no .rpmspec.toml needed.
let section = ProfileSection::default();
let profile = resolve_profile(
    &section,
    Path::new("."),
    ResolveOptions::with_override(Some("rhel-9-x86_64")),
)?;

// Identity is auto-detected from the bundled showrc dump.
assert_eq!(profile.identity.name, "rhel-9-x86_64");
println!("vendor: {:?}, dist: {:?}", profile.identity.vendor, profile.identity.dist_tag);

// Look up a macro from the registry.
if let Some(entry) = profile.macros.get("_libdir") {
    match &entry.value {
        MacroValue::Literal(s) => println!("_libdir = {s}"),
        MacroValue::Raw { body, .. } => println!("_libdir = (raw) {body}"),
        MacroValue::Builtin => println!("_libdir is a builtin"),
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ResolveOptions::with_override` mirrors the CLI's `--profile <name>`
flag; pair it with `.with_defines(&["foo bar".into()])` to inject
`rpmbuild`-style `--define` macros at resolve time.

## Re-exported types

The crate root re-exports the public surface; module paths are
internal. Key types: `Profile`, `Identity`, `Family`, `MacroRegistry`,
`MacroEntry`, `MacroValue`, `Provenance`, `ResolveOptions`,
`ResolveError`. The entry point is `resolve_profile` (re-exported from
`resolve::resolve`).

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
