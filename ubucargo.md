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
generator input, and previous generated state:

```text
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
checkouts. Commands accept arbitrary source-package paths and select a profile
with the global `--profile PATH` option. Walking upward to the nearest
`ubucargo.toml` remains a convenience when the current source tree happens to be
beneath the profile directory.

The profile defines one native Archive view:

```toml
series = "noble"
architecture = "amd64"
pockets = ["release", "updates", "security"]
components = ["main", "universe"]
rust-version = "1.75" # optional

[[repositories]]
name = "rust-staging"
ppa = "ppa:example/rust-staging"

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
expanding Launchpad repository metadata and keys; local, HTTPS-hosted, and
Debusine experiment repositories use explicit deb822 source definitions.
Ubucargo consumes these repositories but does not own their build or publication
infrastructure.

## Command documents

| Command | Purpose | Detailed specification |
| ------- | ------- | ---------------------- |
| `ubucargo init` | Create a packaging profile | [`init.md`](init.md) |
| `ubucargo download` | Acquire an existing source package | [`download.md`](download.md) |
| `ubucargo import` | Create a source tree from crates.io | [`import.md`](import.md) |
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

Ubucargo does not run debcargo in the real source tree. It materializes the
effective source and an adapted configuration in a staging area, invokes a
supported debcargo version to generate packaging candidates, and then performs
its own ownership-aware materialization. This preserves ubucargo's in-tree workflow
without maintaining a second implementation of Debian Rust package generation.

The initial adapter uses debcargo's local-crate and separate-output support.
Origin-sensitive files whose local-crate output would be incorrect, such as the
crate checksum, watch file, and initial changelog, are produced by ubucargo from
verified acquisition metadata. If the existing CLI boundary proves too brittle,
the preferred next step is a narrow generator-only debcargo command rather than
reimplementation.

Downloaded packages should contain `debcargo.toml` and any relevant
`.debcargo.hint` files. If configuration is absent, recovering it from
debcargo-conf history or recreating it remains a manual migration task.

Representative debcargo-conf packages serve as compatibility fixtures for the
staging adapter.

## Acquisition and generation boundaries

Source acquisition is separate from packaging generation:

- `download` retrieves a packaged source from an Archive origin.
- `import` retrieves crates through the Cargo registry protocol and verifies
  registry checksums. New upstream releases are packaged through a fresh import
  rather than an in-place upgrade operation.
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

Rust-version filtering rejects known incompatibilities but does not prove that
selected features, dependencies, build scripts, patches, or generated packaging
work with the target compiler. A build against the configured Archive toolchain
is authoritative.

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
