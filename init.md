# `ubucargo init`

## Synopsis

```console
ubucargo init [DIR] --series SERIES --pockets POCKET,... \
  --components COMPONENT,... [--architecture ARCH] \
  [--ppa ppa:OWNER/NAME]... [--rust-version VERSION]
```

`init` creates a packaging profile. Source-package checkouts do not need to live
inside the profile directory.

## Configuration

For example:

```console
ubucargo init rust-transition \
  --series noble \
  --architecture amd64 \
  --pockets release,updates,security \
  --components main,universe \
  --ppa ppa:example/rust-staging
```

creates `rust-transition/ubucargo.toml`:

```toml
series = "noble"
architecture = "amd64"
pockets = ["release", "updates", "security"]
components = ["main", "universe"]

[[repositories]]
name = "rust-staging"
ppa = "ppa:example/rust-staging"
```

The initial implementation supports one native architecture. When omitted,
`--architecture` defaults to `dpkg --print-architecture`, but the resolved value
is always written to the configuration. `build` uses it as both the Debian build
and host architecture. `Architecture: all` is not a profile architecture.

The `release` pocket is required because the other Ubuntu pockets are overlays.
Pocket, component, and repository order are preserved.

Additional repositories are ordered APT sources. A `ppa` entry is a convenience
shorthand; a generic local or remote repository uses a deb822 `source` string:

```toml
[[repositories]]
name = "local-staging"
source = """
Types: deb deb-src
URIs: file:///srv/ubuntu-rust-staging
Suites: noble
Components: main
Architectures: amd64
Signed-By: /srv/ubuntu-rust-staging/archive-keyring.gpg
"""
```

An optional Rust compatibility target is stored as:

```toml
rust-version = "1.75"
```

When omitted, commands that need it derive the effective version from the
APT-selected `rustc` binary package in the profile view.

## Behavior

- `DIR` defaults to the current directory.
- The target directory may be created if it does not exist.
- `init` must refuse to overwrite an existing `ubucargo.toml`.
- Invalid series, pocket, component, architecture, repository, and Rust-version
  syntax must fail before writing.
- The completed configuration is written atomically.

## Implementation strategy

1. Parse and validate all command-line values.
2. Resolve the native architecture if it was omitted.
3. Normalize ordered lists without reordering them.
4. Serialize the complete configuration to a temporary file in the target
   directory.
5. Atomically rename it to `ubucargo.toml`.
