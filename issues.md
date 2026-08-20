# Open design issues

## Important ambiguities

None are currently open.

## Resolved directions

### Repository trust follows the requested authority

Ubuntu and Debian origins trust their packaged archive keyrings. PPA arguments trust Launchpad over authenticated HTTPS; ubucargo verifies that the retrieved key matches Launchpad's advertised fingerprint, but does not treat that same-source fingerprint as an independent pin. Ubucargo maintains no trust-on-first-use database or separate fingerprint configuration.

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

Ubucargo owns the materialized source tree and sibling orig tarball. Builds are run directly with `sbuild` or another standard build service. Persistent `.dsc`, source `.changes`, `.buildinfo`, signing, and upload artifacts are produced by `dpkg-buildpackage -S`, GBP/dgit, or other standard Debian workflows.

### Native APT provides dependency candidates

`deps` constructs temporary Ubuntu Archive and PPA sources from `--series` and `--ppa`. Native APT refreshes their binary indexes in one shared, user-writable list cache and calculates candidate policy; ubucargo reads the indexes to explain Cargo-specific dependency results. There is no persistent Archive configuration or cache per argument combination. See [`apt-cache.md`](apt-cache.md).

### Archive arguments normalize to temporary deb822

`deps --series` expands to the standard Ubuntu release, updates, and security binary sources from `main` and `universe`; repeated `--ppa` arguments add binary-only PPA sources for that series. `download --from` expands `ubuntu:SUITE`, `debian:SUITE`, or `ppa:OWNER/NAME` to one source-only entry.

### Rust compatibility targets are explicit

`import` and `upgrade` apply MSRV filtering only when `--rust-version` is supplied. Ubucargo does not infer a compiler version from the host or an Archive.

### Staged dependency packages are supplied by PPAs

`deps` does not scan source trees or artifact directories for candidate packages. Staged packages are supplied through repeated `--ppa` arguments. PPA publication and build infrastructure remain outside ubucargo.
