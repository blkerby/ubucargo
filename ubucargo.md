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
      patches/                    # maintainer-owned
      changelog                   # maintainer-owned
      copyright                   # generated or overridden
      copyright.debcargo.hint     # latest generated state
```

`debian/debcargo.toml` is the generator configuration. Ubuntu-only settings use a `[ubucargo]` namespace; existing debcargo keys keep their standard meaning.

## Commands

| Command | Purpose | Detailed specification |
| --- | --- | --- |
| `ubucargo import` | Create a source tree from crates.io | [`import.md`](import.md) |
| `ubucargo upgrade` | Upgrade source and packaging | [`upgrade.md`](upgrade.md) |
| `ubucargo package` | Generate and materialize packaging | [`package.md`](package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`deps.md`](deps.md) |

- To create a new source package based on an upstream crate from crates.io, run `ubucargo import`. It creates the source tree, orig tarball, initial packaging, configuration, and hints.
- For an existing source package, use `ubucargo package` to refresh generated files and hints without changing the upstream source or orig tarball.
- To adopt a new upstream crate release, run `ubucargo upgrade`. It creates the new source tree and orig tarball while preserving existing Debian packaging and generated-file overrides.
- After changing debcargo.toml or other inputs to packaging generation, run ubucargo package again. It may also be run with `--check` to inspect generated changes without writing.
- Run `ubucargo deps` whenever dependency candidates need inspection. It does not modify the source package.

## Generation boundaries

`package` applies Debian patches to staged source before generation, so each patch is applied once. It then runs debcargo against that source with Cargo offline mode enabled.

Generation writes candidate Debian files to a staging directory. Ubucargo then compares them with the working tree and its hints and applies the resulting changes atomically.

The root `Cargo.toml` must contain `[package]`. Nested workspace members may join the build, but ubucargo packages the root crate. Virtual workspaces require manual packaging.

## Source-package artifact boundary

`import` and `upgrade` produce a source tree and sibling orig tarball. `package` refreshes the tree without changing the tarball. Standard Debian tools produce `.dsc`, source `.changes`, `.buildinfo`, signing, and upload artifacts.

Ubucargo does not wrap builds. Maintainers use `sbuild` directly, adding staging PPAs with `--extra-repository` and `--extra-repository-key` as needed. For persistent source artifacts, use standard tooling such as:

```console
dpkg-buildpackage -S --no-sign
```

or their existing GBP/dgit workflow.

## Shared APT metadata cache

`deps` constructs temporary Ubuntu Archive and PPA sources from its command-line arguments. Native APT refreshes and queries their indexes using one shared cache beneath `~/.cache/ubucargo/apt`; there is no persistent Archive configuration or cache per argument combination.

`deps` downloads only binary `Packages` indexes. APT reuses unchanged indexes and applies its normal candidate policy, version ordering, architecture filtering, and `Provides` handling. See [`apt-cache.md`](apt-cache.md).

## Version-control boundary

Ubucargo works with source-package files and leaves version control to the maintainer's tools:

```text
source tree from Git, APT, dgit, GBP, or another standard tool
  -> package or deps

materialized Debian source tree
  -> standard Debian tools produce a source package
  -> sbuild, Launchpad, or another standard build service
```

Repository-specific workflows must provide a tree that standard Debian tools can export.

Ubucargo understands on-disk quilt state but does not inspect VCS-specific patch queues. A GBP patch queue must be exported to `debian/patches` before running `package` or `deps`.
