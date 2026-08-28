# Ubucargo design

## Overview

Ubucargo adapts Debian's Rust packaging model for Ubuntu. Ubucargo wraps `debcargo`, translating Cargo dependencies in `Cargo.toml` into Debian source package data such as `debian/control`.

Ubucargo is designed to operate directly on a Debian source package, with its `debcargo.toml` and related configuration embedded in the source tree in the `debian` directory. This differs from the usual Debian `debcargo` workflow, in which the configuration primarily resides in an external `debcargo-conf` repository. For Ubuntu, a separate configuration repository would be difficult to reconcile with source packages synced from Debian; this is avoided by treating the source packages themselves as the authoritative place for this configuration. Overrides to generated packaging such as `debian/control` can be overwritten in place by maintainers, while corresponding `.debcargo.hint` files record the latest generated output and prevent later regeneration from silently replacing those edits.

Detailed command behavior lives in the [command documents](#commands); this document covers shared concepts and boundaries.

## Source-package state

Each source package contains its upstream source, Debian packaging, generator input, and previous generated state. Its orig tarball sits beside the source directory:

```text
<parent>/
  <source>_<upstream-version>.orig.tar.gz
  <source-package>/
    Cargo.toml
    src/
    debian/
      debcargo.toml               # generator input
      control                     # generated or overridden
      control.debcargo.hint       # latest generated state
      rules                       # generated or overridden
      rules.debcargo.hint         # latest generated state
      patches/                    # maintainer-owned except generated auto/
      changelog                   # maintainer-owned
      copyright                   # generated or overridden
      copyright.debcargo.hint     # latest generated state
```

`debian/debcargo.toml` is the generator configuration. Ubuntu-only settings use a `[ubucargo]` namespace; existing debcargo keys keep their standard meaning.

## Commands

| Command | Purpose | Detailed specification |
| --- | --- | --- |
| `ubucargo package` | Create or update a complete source package | [`package.md`](package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`deps.md`](deps.md) |

- Run `ubucargo package CRATE [VERSION]` outside a package to create a new source tree and orig tarball.
- Run `ubucargo package` inside an existing package to regenerate its current release.
- Run `ubucargo package CRATE VERSION` against an existing package to select another release.
- After changing `debcargo.toml`, run `ubucargo package` again. Archive settings, source transformations, generated packaging, and hints are reconciled together.
- Use `--check` to inspect the complete result without writing.
- Run `ubucargo deps` whenever dependency candidates need inspection. It does not modify the source package.
