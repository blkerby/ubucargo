# Ubucargo design notes

## Objective

Ubucargo adapts Debian's Rust packaging model for Ubuntu. It should remain
compatible with Debian packaging policy while using an Ubuntu-specific process
for generating and maintaining packages.

## Source and repository model

The source package published in the Ubuntu Archive is authoritative. A
`debcargo-conf`-style monorepo is unnecessary: git-ubuntu's Launchpad imports
provide an editable history of published packages, while pending work can live
on local or personal branches.

Each source package should contain both the generator input and generated
packaging, for example:

```text
upstream source/
debian/
  debcargo.toml
  control
  rules
  tests/control
  patches/
  changelog
  copyright
  copyright.debcargo.hint    # when copyright is manually maintained
```

Ubucargo is a maintainer tool, not a build dependency. Generated files such as
`debian/control` must be included in the source package; Launchpad must not run
ubucargo or access crates.io during a build.

The in-tree interface should be:

```console
ubucargo update
```

`update` should generate deterministic output in a staging area, update only
files it owns, preserve manual files, and leave an ordinary Git diff for
review.

The normal flow is:

```text
Ubuntu Archive -> git-ubuntu import -> working branch -> ubucargo update
               -> review, build, test, and upload
```

Coordinated transitions may use local packages or a staging PPA as an overlay
on the Archive. They do not require another canonical packaging repository.

## Relationship with debcargo

Ubucargo should treat debcargo as the reference implementation for compatible
Debian behavior, not as a library dependency or fork. Package names, feature
layout, dependency translation, registry conventions, and existing
`debcargo.toml` keys should remain compatible unless Ubuntu has a concrete
reason to diverge. Representative debcargo output can serve as compatibility
fixtures.

`debian/debcargo.toml` is the single generator configuration file. Ubuntu-only
settings belong under a validated `[ubucargo]` namespace; existing debcargo
keys must not be reinterpreted. Unsupported settings that affect generation
should fail clearly.

Imported source packages should already contain `debcargo.toml` and any
`*.debcargo.hint` files. If configuration is absent, recovering it from
debcargo-conf history or recreating it remains a manual packaging task. On
first import, ubucargo should compare its generated output with the existing
packaging so version-related changes are visible for review.

## Stable generation boundaries

The normal input is an existing source tree. Ubucargo should invoke
`cargo metadata --format-version=1`, deserialize only the JSON fields it uses,
ignore unknown additive fields, and reject unsupported semantic cases. When a
workspace contains multiple applicable packages, selection must be explicit.

Crate acquisition is separate from regeneration. An optional fetch operation
may download a crate through the registry protocol and verify its checksum,
but updating an imported source tree must not require crates.io access.

Generation should produce an in-memory set of relative paths and contents. A
separate reconciliation step should compare that set with `debian/`, enforce
ownership, and write accepted changes atomically. Generator code should not
choose the workspace location or write directly to it.

## In-tree reconciliation

Debcargo's overlay behavior cannot be reused literally: it copies an overlay
into an empty directory and treats existing paths as manual overrides. In an
imported source tree, that would classify every generated file as manual.

Ubucargo should instead:

1. Read `debcargo.toml`, including `[ubucargo]` settings.
2. Treat `overlay = "."` as the existing `debian/` directory without copying
   it recursively.
3. Generate candidate files in a staging area.
4. Reconcile them with the working tree according to explicit ownership.
5. Apply changes atomically.

| File state | Update behavior |
|---|---|
| Generator-owned | Replace with staged output |
| Manual override | Preserve; optionally refresh its generated hint |
| Always manual | Never replace |
| Unknown | Preserve |
| Obsolete generated | Remove only when ownership is known |

Known generated files without companion hints can normally be treated as
generator-owned. A file paired with `FILE.debcargo.hint` is manual, with the
hint holding the generated alternative. Ubucargo should retain this suffix for
compatibility rather than introduce `*.ubucargo.hint`.

Ambiguous exceptions can be declared explicitly:

```toml
[ubucargo.files]
"rules" = "manual"
"tests/control" = "manual"
```

Changelog and patches are always manual. Files such as control, rules, tests,
watch, and copyright may be generated or explicitly overridden. Unknown files
must never be deleted merely because the generator did not emit them.

## Ubuntu Archive index

Regenerating or building one crate does not require a package index. Optional
archive-aware operations need one to determine dependency and feature-provider
availability, enforce component and architecture constraints, identify missing
packages, and plan transitions or test runs.

The index should represent an explicit Ubuntu series and pocket view, including
staging PPAs where requested. It needs source and binary versions, component
and architecture availability, dependency and `Provides` data, Cargo identity
from `X-Cargo-*` fields, and relevant test metadata. Release, updates,
security, proposed, backports, and staging sources must remain distinguishable.

Signed Archive metadata should be the authority for current availability. The
indexer can build a catalog from that metadata and download source packages on
demand for details such as `Cargo.toml`, patches, and `debcargo.toml`. Cache or
database design should wait until the access pattern requires it.

Pending packages form an overlay:

```text
signed Ubuntu archive snapshot
+ staging PPA or locally built packages
= ubucargo resolver view
```

Git and Launchpad history may supplement deleted or superseded versions but
must not override the current signed Archive view.

## Local builds and dependency chains

Ubucargo should not create a private Cargo registry or a separate build system.
A normal build should use Ubuntu's standard package tools:

```text
source tree with generated debian/ files
  -> sbuild installs Build-Depends
  -> librust-*-dev packages populate /usr/share/cargo/registry
  -> dh-cargo builds and tests the crate
```

An optional ubucargo wrapper may invoke `sbuild`, but it must not change package
dependency semantics.

Dependency-chain building is needed only when required crates are absent from
the target Archive view. Given an explicit series and pockets, it should:

1. Combine the Archive with pending source trees, built packages, or a staging
   PPA.
2. Determine an order for missing source packages.
3. Build each package in a fresh `sbuild` environment.
4. Expose previously built packages to later builds and autopkgtests through a
   temporary apt repository or sbuild extra packages.

The result is ordinary Ubuntu source and binary packages, not a synthetic
Cargo registry. Feature policy must come from package configuration rather
than being silently rewritten during the build.

## Ubuntu-specific concerns

For each Ubuntu series, ubucargo must account for:

- Network-isolated builds and policy-compliant source.
- Available Rust, Cargo, `dh-cargo`, and debhelper versions, including crate
  minimum Rust versions.
- Pocket, component, migration, and Main Inclusion Review constraints.
- Supported architectures and architecture-specific failures.
- Bootstrap cycles, coordinated transitions, and reverse-dependency tests.
- Licensing, repacking, generated files, embedded binaries, and vendored native
  code.
- Static linking: security fixes require tracking and rebuilding applications
  that incorporated affected Rust libraries.
- Accurate `Built-Using` and `Static-Built-Using` data.
- Offline, deterministic tests against installed packaged crate sources.

Introducing new crate graphs into stable releases may require older versions,
patches, backported toolchains, or coordinated dependency updates.

## Suggested implementation sequence

1. Build a small crate dependency closure in an Ubuntu PPA using current
   debcargo and overlays; retain inputs and outputs as fixtures.
2. Read an existing source tree through `cargo metadata`, select its package,
   and generate the minimum compatible naming, dependency, feature, and control
   data required by the fixtures.
3. Import `debcargo.toml` and support validated `[ubucargo]` settings.
4. Implement deterministic reconciliation, ownership, hints, and
   `update --check`.
5. Validate generated packages with clean `sbuild` builds and autopkgtests.
6. Add crates.io fetching and archive-aware checks only when imported source
   trees and apt/sbuild are insufficient.
7. Add dependency ordering and local-package overlays when a real transition
   requires them.
