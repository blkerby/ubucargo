# Open design issues

## Important ambiguities

None are currently open.

## Resolved directions

### Repository trust follows the configured authority

Ubuntu and Debian shorthands trust their packaged archive keyrings. PPA shorthands trust Launchpad over authenticated HTTPS; ubucargo verifies that the retrieved key matches Launchpad's advertised fingerprint, but does not treat that same-source fingerprint as an independent pin. Explicit repositories must supply `Signed-By` key material or a local keyring path. Ubucargo maintains no trust-on-first-use database or separate fingerprint configuration.

### Reuse debcargo generation through a staging adapter

Ubucargo invokes a supported debcargo version against an effective source and adapted configuration in a disposable staging area. Debcargo owns Debian Rust package naming, feature layout, dependency translation, and core generated packaging. Ubucargo owns acquisition, origin-sensitive files, generated-file ownership, and materialization.

If the existing debcargo CLI boundary proves too brittle, the preferred next step is a narrow generator-only debcargo command rather than an independent implementation.

### Require a root Cargo package

The source root must contain `Cargo.toml` with `[package]`. That root package is the single primary crate used by `package` and `deps`. Nested workspace members are internal build dependencies and are not packaged independently. Virtual workspace roots without `[package]` require manual packaging.

### Materialize patches before generation

Ubucargo applies the Debian patch series while materializing the effective source. The synthetic debcargo overlay does not contain the patch series, so debcargo cannot apply it a second time during offline package refreshes.

### Package generation is offline

The debcargo staging adapter uses a local crate source with Cargo network access disabled. Registry selection and downloads remain acquisition operations owned by `import` and `upgrade`.

### Overrides are inferred and never merged

A generator-owned primary is an override exactly when it differs from its corresponding `.debcargo.hint`, including path presence and executable mode. `package` preserves overridden primaries and updates their hints with current generator output. It does not keep historical generator state or perform three-way merging.

### Debcargo prepares orig tarballs

`import` and `upgrade` use debcargo's registry-backed full packaging path. Debcargo and Cargo acquire and verify the exact crate, apply configured repack policy, derive the Debian upstream version, and create the correctly named orig tarball. Offline `package` refreshes use local-source mode and discard its temporary orig tarball.

### Standard tools own persistent source artifacts

Ubucargo owns the materialized source tree and sibling orig tarball. Local `build` passes the source directory to `sbuild`, which prepares temporary source state for the build. Persistent `.dsc`, source `.changes`, `.buildinfo`, signing, and upload artifacts are produced by `dpkg-buildpackage -S`, GBP/dgit, or other standard Debian workflows.

### Native APT provides the Archive view

The profile configuration is compiled into an isolated, metadata-only APT view. Native APT refreshes signed indexes and calculates candidate policy; ubucargo reads the local indexes to explain Cargo-specific dependency results. The view runs unprivileged, never invokes dpkg, and redirects all configuration, state, keys, locks, and logs beneath its cache directory. See [`apt-view.md`](apt-view.md).

### Official archive shorthands normalize to deb822

`ubuntu:SUITE`, `debian:SUITE`, and `ppa:OWNER/NAME` are convenience inputs that expand immediately into the same normalized repository representation as raw deb822. Structured `types`, `components`, and `architectures` keys refine their defaults. Transient `download --from` expansions are source-only and cannot change the profile's binary candidate universe.

### Profiles do not own source checkouts

A profile contains Archive, repository, architecture, and Rust packaging configuration. Source trees may live anywhere and are passed to commands by path. Directory names do not define source-package identity.

### Staged packages are supplied by APT repositories

Ubucargo does not scan source trees or artifact directories for candidate packages. Local repositories, PPAs, and other staging archives are ordinary ordered APT sources. Their publication and build infrastructure remain outside ubucargo.
