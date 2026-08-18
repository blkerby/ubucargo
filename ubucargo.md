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
- Commands operate within a workspace that defines one layered Ubuntu Archive
  view shared by dependency inspection, acquisition, and builds.
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

## Workspace model

Except for `init`, commands run inside a workspace. Ubucargo finds the workspace
by walking upward to the nearest `ubucargo.toml`.

The workspace defines one native Archive view:

```toml
series = "noble"
architecture = "amd64"
pockets = ["release", "updates", "security"]
components = ["main", "universe"]
ppas = ["ppa:example/rust-staging"]
rust-version = "1.75" # optional
```

- The initial implementation supports one architecture, used as both Debian
  build and host architecture.
- The `release` pocket is required; other Ubuntu pockets are overlays.
- Pocket, component, and PPA ordering is retained. PPA order is significant.
- When `rust-version` is absent, ubucargo derives the target from the
  APT-selected `rustc` package in this view.

Each source package is an immediate child named after its Debian/Ubuntu source
package:

```text
rust-transition/
  ubucargo.toml
  rust-serde/
  rust-syn/
  rust-syn-1/
```

A workspace contains at most one checkout of each source-package identity.
Ubucargo validates directory names against source-package metadata. Comparing
two revisions of the same source package requires separate workspaces. The
layout restriction is tracked as [open issue 7](issues.md#7-strict-directory-layout-conflicts-with-layout-independence).

## Command documents

| Command | Purpose | Detailed specification |
| ------- | ------- | ---------------------- |
| `ubucargo init` | Create a workspace | [`init.md`](init.md) |
| `ubucargo download` | Acquire an existing source package | [`download.md`](download.md) |
| `ubucargo import` | Create a source tree from crates.io | [`import.md`](import.md) |
| `ubucargo package` | Generate and materialize packaging | [`package.md`](package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`deps.md`](deps.md) |
| `ubucargo build` | Build with standard Ubuntu tooling | [`build.md`](build.md) |

`PACKAGE` arguments identify source-package directories and may be omitted when
the current directory is inside one.

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
file modes. Generator code does not select workspace locations or write directly
into source trees. A separate materialization step preserves inferred overrides
and applies generated state atomically.

Rust-version filtering rejects known incompatibilities but does not prove that
selected features, dependencies, build scripts, patches, or generated packaging
work with the target compiler. A build against the configured Archive toolchain
is authoritative.

## Archive view and resolver

The shared Archive catalog supports source selection, dependency inspection,
workspace Rust-version discovery, and build configuration. It represents the
configured Ubuntu series, architecture, pockets, components, PPAs, and available
workspace binary packages.

The catalog needs:

- source and binary versions;
- origin, pocket, component, and architecture availability;
- dependency and `Provides` data; and
- Cargo identity from `X-Cargo-*` fields.

Candidate selection follows APT policy: priority first, Debian version ordering
among equal priorities next, and repository order for identical versions.
`download`, `deps`, and Archive-derived Rust-version checks share this view.
`build` constructs the same normalized repository configuration and leaves
build-dependency selection to APT inside `sbuild`.

Signed Ubuntu Archive and configured PPA metadata are authoritative for current
availability. Archive-aware commands may use a cached catalog but must refresh
stale metadata before use. Indexing does not download or unpack source packages.

The exact division of responsibility between the catalog and native APT policy
is tracked as [open issue 2](issues.md#2-resolver-duplicates-apt-candidate-selection).

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
