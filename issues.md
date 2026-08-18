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
- how upgrades create or locate the new original tarball;
- which command runs `dpkg-source -b` or `dpkg-buildpackage -S`; and
- where `.dsc`, `.changes`, `.buildinfo`, and `.deb` outputs are stored.

Until then, `import -> package -> build` is not a complete workflow.

### 2. Upgrade can destroy maintainer changes

[`upgrade`](upgrade.md#upgrade-operation) replaces the upstream tree while
preserving only `debian/`, but does not require version control or check whether
the existing upstream files were modified.

Ubucargo should compare the existing upstream tree with its known crate or
Archive origin and refuse replacement if it differs, unless the maintainer
explicitly forces the operation.

### 3. Resolver duplicates APT candidate selection

The [Archive resolver](ubucargo.md#archive-view-and-resolver) uses the same
normalized repository view as the build, but still promises to reproduce APT's
candidate-selection policy.

The custom catalog should provide Cargo identity, source availability, and
explanatory output, while an isolated APT cache remains the authority for the
selected binary candidate. `deps` should be described as a prediction from its
current metadata because the Archive can change before a later build.

### 4. Debian downloads are outside the Archive model

[`download --from debian:SUITE`](download.md#selection) is supported by the CLI,
but the Archive index only defines the configured Ubuntu series and PPAs.

Either remove Debian origins from the initial CLI or define the on-demand
Debian mirrors, signing keys, components, cache identity, and selection rules.

## Important ambiguities

### 5. Cargo workspace package selection has no interface

The [`package` generation boundary](package.md#generation-boundary) says package
selection must be explicit when the source contains multiple applicable Cargo
packages, but no CLI option or `debcargo.toml` setting identifies the package.

### 6. PPA key verification lacks a trust bootstrap

The [`build` design](build.md#invocation) says to retrieve and verify each PPA
signing fingerprint and key, but does not define where the expected fingerprint
comes from or whether it is pinned after first use. Retrieving the key and
expected fingerprint from the same source is not independent verification.

### 7. Workspace binary artifact discovery is unspecified

The [`build` design](build.md#invocation) does not define output locations,
source/version association, architecture filtering, or handling of multiple
stale `.deb` versions. Passing a directory of packages can expose unintended
candidates to APT.

### 8. Strict directory layout conflicts with layout independence

Every checkout must be an immediate child named exactly after its source
package in the [workspace model](ubucargo.md#workspace-model), while the
version-control section says commands are independent of repository layout.
Deriving source-package identity from package metadata would remove this
contradiction.

### 9. MSRV selection depends on moving external behavior

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
ownership, and reconciliation.

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
by `import` and `upgrade`.
