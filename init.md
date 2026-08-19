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
archive = "ppa:example/rust-staging"
types = ["deb", "deb-src"]
components = ["main"]
```

The initial implementation supports one native architecture. When omitted,
`--architecture` defaults to `dpkg --print-architecture`, but the resolved value
is always written to the configuration. `build` uses it as both the Debian build
and host architecture. `Architecture: all` is not a profile architecture.

The `release` pocket is required because the other Ubuntu pockets are overlays.
Pocket, component, and repository order are preserved.

Additional repositories are ordered APT sources. `archive` accepts the official
shorthands `ubuntu:SUITE`, `debian:SUITE`, and `ppa:OWNER/NAME`. Shorthands are
expanded to deb822 at runtime and may be refined with structured keys:

```toml
[[repositories]]
name = "debian-sid"
archive = "debian:sid"
types = ["deb", "deb-src"]
components = ["main", "contrib", "non-free", "non-free-firmware"]
architectures = ["amd64"]
```

`deb` enables binary `Packages` indexes used for candidate selection and builds.
`deb-src` enables source `Sources` indexes used for source downloads and Dose
build-dependency analysis. Persistent repositories default to both types;
transient `download --from` views force `deb-src` only.

Ubuntu shorthands inherit profile components unless overridden. Debian defaults
to `main`; PPA repositories use `main`. Component and type order are preserved.

A generic local or remote repository uses a deb822 `source` string:

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

Explicit sources must provide `Signed-By` as embedded key material or a readable
local keyring path. Ubuntu and Debian shorthands use their packaged archive
keyrings; PPA shorthands obtain their key from Launchpad over authenticated
HTTPS and verify it against Launchpad's advertised fingerprint. Profiles do not
store a separate fingerprint pin or trust-on-first-use state.

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
- Invalid series, pocket, component, architecture, repository, repository trust,
  and Rust-version configuration must fail before writing.
- The completed configuration is written atomically.

## Implementation strategy

1. Parse and validate all command-line values.
2. Resolve the native architecture if it was omitted.
3. Normalize ordered lists without reordering them.
4. Serialize the complete configuration to a temporary file in the target
   directory.
5. Atomically rename it to `ubucargo.toml`.
