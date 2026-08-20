# Ubucargo design

## Overview

Ubucargo adapts Debian's Rust packaging model for Ubuntu. Ubucargo wraps `debcargo`, translating Cargo dependencies in `Cargo.toml` into Debian source package data such as `debian/control`.

Ubucargo is designed to operate directly on a Debian source package, with its `debcargo.toml` and related configuration embedded in the source tree in the `debian` directory. This differs from the usual Debian `debcargo` workflow, in which the configuration primarily resides in an external `debcargo-conf` repository. For Ubuntu, a separate configuration repository would be difficult to reconcile with source packages synced from Debian; this is avoided by treating the source packages themselves as the authoritative place for this configuration. Overrides to generated packaging such as `debian/control` can be overwritten in place by maintainers, while corresponding `.debcargo.hint` files record the latest generated output and prevent later regeneration from silently replacing those edits.

Detailed command behavior lives in the [command documents](#command-documents); this document covers shared concepts and boundaries.

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

`debian/debcargo.toml` is the generator configuration. Ubuntu-only settings use a validated `[ubucargo]` namespace; existing debcargo keys keep their standard meaning. Unknown generation settings cause an error.

The generator owns defined filename spaces and their hints. The maintainer owns the changelog, patches, configuration, and unknown paths. See [`package`](package.md#override-detection-and-materialization) for the full rules.

## Command documents

| Command | Purpose | Detailed specification |
| --- | --- | --- |
| `ubucargo import` | Create a source tree from crates.io | [`import.md`](import.md) |
| `ubucargo upgrade` | Upgrade source and packaging | [`upgrade.md`](upgrade.md) |
| `ubucargo package` | Generate and materialize packaging | [`package.md`](package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`deps.md`](deps.md) |

`PACKAGE` arguments identify arbitrary source-package directories and may be omitted when the current directory is inside one.

## Relationship with debcargo

Debcargo is the reference implementation and initial generator. Ubucargo keeps its package names, feature layout, dependency translation, registry conventions, and configuration behavior unless Ubuntu needs a change.

Ubucargo runs a supported debcargo version in a staging area in two modes:

- `import` and `upgrade` use debcargo's registry-backed full packaging path for exact crate acquisition, checksum verification, Debian version conversion, optional repacking, orig-tarball creation, source extraction, and initial packaging generation.
- `package` uses debcargo's local-crate and separate-output support to refresh generated candidates from local sources. The temporary orig tarball from local-source mode is discarded.

Ubucargo then materializes files according to its ownership rules.

Existing source trees may initially lack complete `.debcargo.hint` files. `package` initializes unambiguous baselines and refuses to replace an existing file when its missing baseline makes ownership ambiguous. Recovery of `debcargo.toml` from debcargo-conf history remains a manual migration task.

Representative debcargo-conf packages serve as compatibility fixtures.

If debcargo's existing CLI boundary proves too brittle, the preferred next step is a narrow generator-only debcargo command rather than an independent implementation of its generation behavior.

## Acquisition and generation boundaries

- Existing source packages are acquired with the maintainer's normal tooling, such as `git ubuntu clone`, `apt source`, dgit, or GBP.
- `import` creates a new package from the registry.
- `upgrade` creates a new upstream tree and orig tarball while preserving durable Debian state and regenerating generated files from scratch.
- `package` operates on an existing source tree using local data only.

Ubucargo applies Debian patches to staged source before generation, so each patch is applied once. Debcargo then runs against that source with network access disabled.

Generation returns relative paths, contents, and file modes. A separate step preserves overrides and applies the result atomically.

The root `Cargo.toml` must contain `[package]`. Nested workspace members may join the build, but ubucargo packages the root crate. Virtual workspaces require manual packaging.

When a Rust target is supplied, version filtering rejects known incompatibilities. A build with the target Archive toolchain is the final compatibility test.

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
