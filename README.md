# rpm-spec-tool

Pretty-printer and static analyzer CLI for RPM `.spec` files.

Built on top of the [`rpm-spec`](https://crates.io/crates/rpm-spec) parser and a
visitor-based analyzer that ships with 24 built-in distribution profiles
(generic, RHEL 8/9/10, Fedora-derived families, SUSE, ALT Linux variants, and
more).

## Features

- **`lint`** — run static-analysis rules with rich `codespan`-style diagnostics
- **`format`** / **`pretty`** — canonical pretty-printer (`--check`, `--in-place`, `--diff`)
- **`ast`** — dump the parsed AST as JSON or YAML
- **`check`** — combined lint + format-check shorthand for CI
- **`profile`** — inspect the resolved distribution profile (macros, rpmlib features, whitelists)
- Optional [`shellcheck`](https://www.shellcheck.net/) integration for `%prep` / `%build` / `%install` shell sections (lints `RPM200`/`RPM201`)

## Usage

```sh
rpm-spec-tool lint  my-package.spec
rpm-spec-tool check my-package.spec        # CI: lint + format-check
rpm-spec-tool format --in-place my.spec
rpm-spec-tool profile show rhel-9          # inspect a built-in profile
rpm-spec-tool profile list                 # list every available profile
```

A project-level config can be placed in `.rpmspec.toml`; CLI flags
`--define KEY=value` (rpmbuild-compatible) and `--config PATH` override it.

## Installation

Binary releases (`tar.gz`, `.deb`, `.rpm`) for Linux `x86_64` and `aarch64` are
published on the [GitHub Releases](https://github.com/johnlepikhin/rpm-spec-tool/releases)
page for every tag `vX.Y.Z`.

From source:

```sh
cargo install --path crates/cli
```

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
