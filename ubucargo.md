# Ubucargo design

## Overview

Ubucargo adapts Debian's Rust packaging model for Ubuntu. It provides a workflow for packaging and maintaining Rust source packages while following Debian policy.

Like `debcargo`, ubucargo consumes `Cargo.toml` and `debcargo.toml` and translates Cargo dependencies and features into Debian source and binary package metadata. Ubucargo differs in three ways:

- Generator configuration is stored in each source package as `debian/debcargo.toml`, replacing debcargo-conf's external monorepo.
- A packaging profile defines the Archive view shared by dependency inspection, acquisition, and local builds.
- Generated files may be edited in place. Each `.debcargo.hint` records the latest generator output; a difference marks a maintainer override.

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

## Packaging profile

`ubucargo.toml` defines a packaging profile, not a directory for source checkouts. Archive-aware commands accept any source-package path and find a profile through `--profile PATH` or by walking up from the source tree. Offline `package` generation needs no profile.

The profile defines one native Archive view:

```toml
series = "noble"
architecture = "amd64"
pockets = ["release", "updates", "security"]
components = ["main", "universe"]
rust-version = "1.75" # optional

[[repositories]]
name = "rust-staging"
archive = "ppa:example/rust-staging"
types = ["deb", "deb-src"]
components = ["main"]

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

- Profiles support one architecture, used as both the Debian build and host architecture.
- The `release` pocket is required; other Ubuntu pockets are overlays.
- Pocket, component, and repository ordering is retained.
- When `rust-version` is omitted, ubucargo derives the target from the APT-selected `rustc` package in this view.

Repository entries are APT sources. PPA syntax expands Launchpad metadata and keys; shorthands such as `ubuntu:noble-proposed` and `debian:sid` work the same way. Structured `types`, `components`, and `architectures` keys override shorthand defaults before deb822 normalization. Other repositories may use explicit deb822 sources. Ubucargo consumes repositories but does not build or publish them.

Repository trust is defined in [`apt-view.md`](apt-view.md#repository-trust).

## Command documents

| Command | Purpose | Detailed specification |
| --- | --- | --- |
| `ubucargo init` | Create a packaging profile | [`init.md`](init.md) |
| `ubucargo download` | Acquire an existing source package | [`download.md`](download.md) |
| `ubucargo import` | Create a source tree from crates.io | [`import.md`](import.md) |
| `ubucargo upgrade` | Upgrade source and packaging | [`upgrade.md`](upgrade.md) |
| `ubucargo package` | Generate and materialize packaging | [`package.md`](package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`deps.md`](deps.md) |
| `ubucargo build` | Build with standard Ubuntu tooling | [`build.md`](build.md) |

`PACKAGE` arguments identify arbitrary source-package directories and may be omitted when the current directory is inside one.

## Relationship with debcargo

Debcargo is the reference implementation and initial generator. Ubucargo keeps its package names, feature layout, dependency translation, registry conventions, and configuration behavior unless Ubuntu needs a change.

Ubucargo runs a supported debcargo version in a staging area in two modes:

- `import` and `upgrade` use debcargo's registry-backed full packaging path for exact crate acquisition, checksum verification, Debian version conversion, optional repacking, orig-tarball creation, source extraction, and initial packaging generation.
- `package` uses debcargo's local-crate and separate-output support to refresh generated candidates from local sources. The temporary orig tarball from local-source mode is discarded.

Ubucargo then materializes files according to its ownership rules.

Downloaded packages should contain `debcargo.toml` and any relevant `.debcargo.hint` files. Recovery from debcargo-conf history is a manual migration task.

Representative debcargo-conf packages serve as compatibility fixtures.

## Acquisition and generation boundaries

- `download` retrieves a packaged source from an Archive origin.
- `import` creates a new package from the registry.
- `upgrade` creates a new upstream tree and orig tarball while preserving durable Debian state and regenerating generated files from scratch.
- `package` operates on an existing source tree using local data only.

Ubucargo applies Debian patches to staged source before generation, so each patch is applied once. Debcargo then runs against that source with network access disabled.

Generation returns relative paths, contents, and file modes. A separate step preserves overrides and applies the result atomically.

The root `Cargo.toml` must contain `[package]`. Nested workspace members may join the build, but ubucargo packages the root crate. Virtual workspaces require manual packaging.

Rust-version filtering rejects known incompatibilities. A build with the configured Archive toolchain is the final compatibility test.

## Source-package artifact boundary

`import` and `upgrade` produce a source tree and sibling orig tarball. `package` refreshes the tree without changing the tarball. Standard Debian tools produce `.dsc`, source `.changes`, `.buildinfo`, signing, and upload artifacts.

For local builds, `build` passes the source directory to `sbuild`, which prepares a temporary source package. For persistent artifacts, use standard tooling such as:

```console
dpkg-buildpackage -S --no-sign
```

or their existing GBP/dgit workflow.

## Isolated APT metadata view

The profile uses a metadata-only APT view in the user's cache. The profile supplies sources, preferences, keys, architecture, and repository order. APT handles refreshes, policy, version ordering, architecture filtering, `Provides`, and candidate selection.

`download`, `deps`, and Archive-derived Rust-version checks share the cached view. `build` constructs the same normalized repository configuration inside `sbuild`.

The view runs unprivileged, allows only metadata operations and downloads, and keeps all APT state in its own directory. See [`apt-view.md`](apt-view.md).

## Version-control boundary

Ubucargo works with source-package files and leaves version control to the maintainer's tools:

```text
materialized Debian source tree
  -> package or deps

exportable Debian source tree
  -> standard Debian tools produce a source package
  -> build
```

Repository-specific workflows must provide a tree that standard Debian tools can export.
