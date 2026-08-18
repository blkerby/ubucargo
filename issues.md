# Open design issues

## Blocking or high-risk

### 1. Source-package artifact lifecycle

The [`import`](import.md#output) command produces an upstream tree and
`debian/debcargo.toml`, while [`build`](build.md#build-contract) assumes that
standard tools have produced a source package before invoking `sbuild` with a
`.dsc`.

The design still needs to define:

- how the verified crates.io archive becomes and is retained as `.orig.tar.*`;
- where acquisition metadata such as the registry checksum is persisted;
- how the initial Debian version and changelog entry are chosen;
- which command runs `dpkg-source -b` or `dpkg-buildpackage -S`; and
- where `.dsc`, `.changes`, `.buildinfo`, and `.deb` outputs are stored.

Until then, `import -> package -> build` is not a complete workflow.

### 2. Debian downloads are outside the Archive model

[`download --from debian:SUITE`](download.md#selection) is supported by the CLI,
but the workspace APT view only defines the configured Ubuntu series and PPAs.

Either remove Debian origins from the initial CLI or define a separate
on-demand Debian metadata view, including mirrors, signing keys, components,
cache identity, and selection rules.

## Important ambiguities

### 3. Cargo workspace package selection has no interface

The [`package` generation boundary](package.md#generation-boundary) says package
selection must be explicit when the source contains multiple applicable Cargo
packages, but no CLI option or `debcargo.toml` setting identifies the package.

### 4. PPA key verification lacks a trust bootstrap

The [`build` design](build.md#invocation) says to retrieve and verify each PPA
signing fingerprint and key, but does not define where the expected fingerprint
comes from or whether it is pinned after first use. Retrieving the key and
expected fingerprint from the same source is not independent verification.

### 5. Workspace binary artifact discovery is unspecified

The [`build` design](build.md#invocation) does not define output locations,
source/version association, architecture filtering, or handling of multiple
stale `.deb` versions. Passing a directory of packages can expose unintended
candidates to APT.

### 6. Strict directory layout conflicts with layout independence

Every checkout must be an immediate child named exactly after its source
package in the [workspace model](ubucargo.md#workspace-model), while the
version-control section says commands are independent of repository layout.
Deriving source-package identity from package metadata would remove this
contradiction.

### 7. MSRV selection depends on moving external behavior

The [`import` design](import.md#version-selection) specifies the same MSRV policy
as `cargo add` rather than a stable algorithm. It should directly define
prerelease handling, yanked releases, missing `rust_version`, ties, and version
ordering.

## Resolved directions

### Reuse debcargo generation through a staging adapter

Ubucargo invokes a supported debcargo version against an effective source and
adapted configuration in a disposable staging area. Debcargo owns Debian Rust
package naming, feature layout, dependency translation, and core generated
packaging. Ubucargo owns acquisition, origin-sensitive files, generated-file
ownership, and materialization.

If the existing debcargo CLI boundary proves too brittle, the preferred next
step is a narrow generator-only debcargo command rather than an independent
implementation.

### Materialize patches before generation

Ubucargo applies the Debian patch series while materializing the effective
source. The synthetic debcargo overlay does not contain the patch series, so
debcargo cannot apply it a second time.

### Package generation is offline

The debcargo staging adapter uses a local crate source with Cargo network access
disabled. Registry selection and downloads remain acquisition operations owned
by `import`.

### Overrides are inferred and never merged

A generator-owned primary is an override exactly when it differs from its
corresponding `.debcargo.hint`, including path presence and executable mode.
`package` preserves overridden primaries and updates their hints with current
generator output. It does not keep historical generator state or perform
three-way merging.

### New upstream versions use fresh imports

There is no in-place `upgrade` command. A new upstream release is imported into
a fresh source tree, usually in another workspace, and the maintainer manually
copies only the old packaging state that remains relevant.

### Native APT provides the Archive view

The workspace configuration is compiled into an isolated, metadata-only APT
view. Native APT refreshes signed indexes and calculates candidate policy;
ubucargo reads the local indexes to explain Cargo-specific dependency results.
The view runs unprivileged, never invokes dpkg, and redirects all configuration,
state, keys, locks, and logs beneath its cache directory. See
[`apt-view.md`](apt-view.md).
