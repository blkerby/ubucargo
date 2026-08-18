# `ubucargo import`

## Synopsis

```console
ubucargo [--profile PROFILE] import CRATE [--version VERSION] [--directory DIR]
```

`import` creates a new packaged source tree and Debian orig tarball from a
crates.io release.

## Version selection

Without `--version`, ubucargo selects the newest non-yanked release compatible
with the profile Rust version. A release whose declared `rust_version` is
newer than the profile target is skipped. A release without `rust_version`
is treated as potentially compatible, with a warning.

```text
Workspace rustc: 1.75
Ignoring foo 4.2.0: requires Rust 1.81
Importing foo 4.1.0
warning: foo 4.1.0 does not declare rust-version; compatibility is unverified
```

When an exact version is requested, a known MSRV incompatibility is an error and
an undeclared MSRV remains a warning. The selected release is immediately fixed
to an exact version.

The precise release-selection algorithm remains open; see
[issue 5](issues.md#5-msrv-selection-depends-on-moving-external-behavior).

## Output

Ubucargo creates a default `debcargo.toml` in staging and invokes debcargo's
registry-backed `package` command with the exact selected version. Debcargo and
Cargo download and verify the crate, derive the Debian source identity and
upstream version, create or repack the orig tarball, extract the source, and
generate the initial `debian/` packaging.

Ubucargo copies the staged `debcargo.toml` into the resulting `debian/`
directory and normalizes generated hints before installation.

The source tree is created at `DIR`, which defaults to a directory named after
the Debian source package in the current directory. Existing Debian Rust naming,
feature, and versioning conventions determine the source and eventual binary
package names; the filesystem directory name is not authoritative.

The command refuses to overwrite an existing source-package directory.

## New upstream releases

Existing packages use [`ubucargo upgrade`](upgrade.md), which preserves durable
Debian state while using the same debcargo orig-tarball path for the new crate
release.

## Orig tarball

The orig tarball is placed beside the source directory using Debian naming:

```text
~/src/
  rust-serde/
  rust-serde_1.0.220.orig.tar.gz
```

When no repack is required, debcargo copies the verified `.crate` archive
byte-for-byte to the orig filename. Configured exclusions or manifest
normalization cause a deterministic repack and add the configured suffix, such
as `+dfsg`, to the Debian upstream version.

The remaining `.dsc`, source `.changes`, and `.buildinfo` production lifecycle
is tracked in [issue 1](issues.md#1-source-package-build-artifact-lifecycle).

## Implementation strategy

1. Resolve the profile Rust target.
2. Query crate release metadata and select an exact version.
3. Create and validate the initial staged `debcargo.toml`.
4. Invoke a supported debcargo version in registry mode with the exact crate
   version and staged output directory.
5. Validate the resulting source identity, orig filename, checksum metadata,
   and generated packaging.
6. Copy the authoritative in-tree config into the staged `debian/` directory.
7. Atomically install the source tree and orig tarball without overwriting an
   existing destination.
