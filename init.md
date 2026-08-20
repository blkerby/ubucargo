# `ubucargo init`

## Synopsis

```console
ubucargo init [DIR] --series SERIES --pockets POCKET,... \
  --components COMPONENT,... [--architecture ARCH] \
  [--ppa ppa:OWNER/NAME]... [--rust-version VERSION]
```

`init` creates a packaging profile; it does not contain source packages.

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

Profiles support one native architecture. If omitted, `--architecture` uses `dpkg --print-architecture`; the resolved value is always stored. `build` uses it as both the build and host architecture, including for `Architecture: all` packages.

The `release` pocket is required; other Ubuntu pockets are overlays. Pocket, component, and repository order are preserved.

Additional repositories are ordered APT sources. `archive` accepts `ubuntu:SUITE`, `debian:SUITE`, and `ppa:OWNER/NAME`. Structured keys can refine these shorthands before they expand to deb822:

```toml
[[repositories]]
name = "debian-sid"
archive = "debian:sid"
types = ["deb", "deb-src"]
components = ["main", "contrib", "non-free", "non-free-firmware"]
architectures = ["amd64"]
```

`deb` enables binary indexes for candidate selection and builds. `deb-src` enables source downloads and Dose analysis. Persistent repositories default to both; temporary `download --from` views use only `deb-src`.

Ubuntu shorthands inherit profile components unless overridden. Debian defaults to `main`; PPAs use `main`. Component and type order are preserved.

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

Explicit sources must provide `Signed-By` as embedded key material or a readable keyring path. Ubuntu and Debian use packaged keyrings. PPAs retrieve their key and fingerprint from Launchpad over authenticated HTTPS and require them to match. Profiles do not store a separate fingerprint pin.

An optional Rust compatibility target is stored as:

```toml
rust-version = "1.75"
```

When omitted, commands use the APT-selected `rustc` version from the profile.

## Behavior

- `DIR` defaults to the current directory.
- The target directory is created when needed.
- An existing `ubucargo.toml` causes an error.
- Invalid configuration fails before writing.
- The completed configuration is written atomically.
