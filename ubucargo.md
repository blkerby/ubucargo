# Ubucargo high-level design

## Overview

Ubucargo adapts Debian's Rust packaging model for Ubuntu. It should remain
compatible with Debian packaging policy while providing an Ubuntu-specific
workflow for acquiring, generating, maintaining, inspecting, and building Rust
source packages.

Like `debcargo`, ubucargo consumes `Cargo.toml` and `debcargo.toml` and translates
Cargo dependencies and features into Debian source and binary package metadata.
Ubucargo differs in three main ways:

- Generator configuration is stored in each source package as
  `debian/debcargo.toml`; there is no equivalent of the external
  `debcargo-conf` monorepo.
- Commands use a packaging profile that defines one layered Ubuntu Archive view
  shared by dependency inspection, acquisition, and optional local builds.
- Generated packaging may be edited in place. A corresponding
  `.debcargo.hint` records current generator output; differing primary and hint
  values identify a maintainer override without an explicit override list.

Detailed command behavior and implementation strategy live in the
[command documents](#command-documents). This document defines only shared
concepts and boundaries.

## Source-package state

Each source package contains its upstream source, complete Debian packaging,
generator input, and previous generated state. Its Debian orig tarball is stored
beside the source directory:

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
      tests/control               # generated or overridden
      tests/control.debcargo.hint # latest generated state
      patches/                    # maintainer-owned
      changelog                   # maintainer-owned
      copyright                   # generated or overridden
      copyright.debcargo.hint     # latest generated state
```

`debian/debcargo.toml` is the single generator configuration file. Ubuntu-only
settings belong under a validated `[ubucargo]` namespace; existing debcargo keys
must not be reinterpreted. Unsupported settings that affect generation should
fail clearly.

Generated files and their hints are owned by filename spaces defined by the
generator. Changelog, patches, configuration, and unknown paths remain
maintainer-owned. The complete ownership and materialization rules are specified
by [`package`](package.md#override-detection-and-materialization).

## Packaging profile

`ubucargo.toml` defines a packaging profile rather than a container for source
checkouts. Archive-aware commands accept arbitrary source-package paths and
select a profile with the global `--profile PATH` option. Walking upward to the
nearest `ubucargo.toml` remains a convenience when the current source tree
happens to be beneath the profile directory. Offline `package` generation is
profile-independent.

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

- The initial implementation supports one architecture, used as both Debian
  build and host architecture.
- The `release` pocket is required; other Ubuntu pockets are overlays.
- Pocket, component, and repository ordering is retained.
- When `rust-version` is absent, ubucargo derives the target from the
  APT-selected `rustc` package in this view.

Repository entries are ordinary APT sources. PPA syntax is a convenience for
expanding Launchpad repository metadata and keys. Official archive shorthands
such as `ubuntu:noble-proposed` and `debian:sid` expand through the same path.
Structured `types`, `components`, and `architectures` keys override shorthand
defaults before normalization to deb822. Local, HTTPS-hosted, and Debusine
experiment repositories may use explicit deb822 source definitions. Ubucargo
consumes these repositories but does not own their build or publication
infrastructure.

Ubuntu and Debian shorthands trust their packaged archive keyrings. PPA
shorthands trust Launchpad over authenticated HTTPS and require the retrieved
key to match Launchpad's advertised fingerprint. Explicit sources must provide
`Signed-By` key material or a local keyring path. Ubucargo does not add a
separate fingerprint pin or trust-on-first-use state; the complete trust and
materialization contract is defined in [`apt-view.md`](apt-view.md#repository-trust).

## Command documents

| Command | Purpose | Detailed specification |
| ------- | ------- | ---------------------- |
| `ubucargo init` | Create a packaging profile | [`init.md`](init.md) |
| `ubucargo download` | Acquire an existing source package | [`download.md`](download.md) |
| `ubucargo import` | Create a source tree from crates.io | [`import.md`](import.md) |
| `ubucargo upgrade` | Replace upstream source and regenerate packaging | [`upgrade.md`](upgrade.md) |
| `ubucargo package` | Generate and materialize packaging | [`package.md`](package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`deps.md`](deps.md) |
| `ubucargo build` | Build with standard Ubuntu tooling | [`build.md`](build.md) |

`PACKAGE` arguments identify arbitrary source-package directories and may be
omitted when the current directory is inside one.

## Relationship with debcargo

Debcargo is both the reference implementation and the initial packaging
generator. Package names, feature layout, dependency translation, registry
conventions, and existing `debcargo.toml` keys remain debcargo behavior unless
Ubuntu has a concrete reason to diverge.

Ubucargo does not run debcargo in the real source tree. It invokes a supported
debcargo version in a staging area and uses two modes:

- `import` and `upgrade` use debcargo's registry-backed full packaging path for
  exact crate acquisition, checksum verification, Debian version conversion,
  optional repacking, orig-tarball creation, source extraction, and initial
  packaging generation.
- `package` uses debcargo's local-crate and separate-output support to refresh
  generated candidates without network access. The temporary orig tarball from
  local-source mode is discarded.

Ubucargo then applies its own ownership-aware materialization. This preserves
the in-tree workflow without maintaining a second implementation of Debian Rust
package or orig-tarball generation.

Downloaded packages should contain `debcargo.toml` and any relevant
`.debcargo.hint` files. If configuration is absent, recovering it from
debcargo-conf history or recreating it remains a manual migration task.

Representative debcargo-conf packages serve as compatibility fixtures for the
staging adapter.

## Acquisition and generation boundaries

Source acquisition is separate from packaging generation:

- `download` retrieves a packaged source from an Archive origin.
- `import` creates a genuinely new package using debcargo's registry-backed
  packaging path.
- `upgrade` creates a new upstream tree and orig tarball while preserving
  durable Debian state and regenerating generated files from scratch.
- `package` operates on an existing source tree and must not require network
  access.

Ubucargo is responsible for materializing the effective patched source before
generation. Debian patches are not passed through debcargo's synthetic overlay,
so they cannot be applied twice. Debcargo is invoked against the staged source
with network access disabled.

Generation produces an in-memory set of relative paths, contents, and relevant
file modes. Generator code does not select source-tree locations or write directly
into source trees. A separate materialization step preserves inferred overrides
and applies generated state atomically.

The supported source layout has a root `Cargo.toml` containing `[package]`.
Nested workspace members may participate in the build but are not independently
selected or packaged. A virtual workspace root without `[package]` requires
manual packaging outside ubucargo's initial scope.

Rust-version filtering rejects known incompatibilities but does not prove that
selected features, dependencies, build scripts, patches, or generated packaging
work with the target compiler. A build against the configured Archive toolchain
is authoritative.

## Source-package artifact boundary

`import` and `upgrade` produce a materialized source tree and sibling orig
tarball. `package` refreshes that tree without replacing the orig. Ubucargo does
not own persistent `.dsc`, source `.changes`, `.buildinfo`, signing, or upload
artifacts.

For local builds, `build` passes the source directory to `sbuild`, which prepares
the temporary source package required for the build. Maintainers who need
persistent upload artifacts use standard tooling such as:

```console
dpkg-buildpackage -S --no-sign
```

or their existing GBP/dgit workflow.

## Isolated APT metadata view

The profile Archive implementation is a metadata-only APT view stored beneath
the user's cache directory. The profile configuration supplies sources,
preferences, keys, architecture, and repository order; native APT remains the
authority for metadata refresh, package policy, Debian version ordering,
architecture filtering, `Provides`, and candidate selection.

Ubucargo reads APT's downloaded binary and source indexes to explain every
relevant candidate, including origin, pocket, component, dependency fields, and
Cargo identity. It does not maintain an independent Archive resolver or issue
remote requests for individual dependency queries.

`download`, `deps`, and Archive-derived Rust-version checks share the cached
view. `build` constructs the same normalized repository configuration inside
`sbuild`. Signed metadata from every configured repository is authoritative for
current availability.

The view runs unprivileged, never installs packages, and redirects all APT
configuration, state, keys, locks, and logs beneath its own directory. Its full
layout, operation allowlist, caching behavior, and safety contract are specified
in [`apt-view.md`](apt-view.md).

## Version-control boundary

Version-control integration is outside the initial implementation. Ubucargo does
not require Git or invoke git-ubuntu, git-buildpackage, git-debrebase, or other
repository-management tools. Its contract is a materialized source-package
filesystem:

```text
materialized Debian source tree
  -> package or deps

exportable Debian source tree
  -> standard Debian tools produce a source package
  -> build
```

Maintainers using repository-specific patch queues or history models are
responsible for presenting a source tree that standard Debian tools can export.
A future integration layer may initialize history from a `.dsc`, import an
upstream tarball, or export repository state, but those operations remain
outside the core command contracts.

## Open issues

Unresolved design decisions are tracked in [`issues.md`](issues.md).
