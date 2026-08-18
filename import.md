# `ubucargo import`

## Synopsis

```console
ubucargo [--profile PROFILE] import CRATE [--version VERSION] [--directory DIR]
```

`import` creates a new source-package tree from a crates.io release.

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

The command downloads and verifies the crate, unpacks its upstream source, and
creates a fresh `debian/debcargo.toml`. It leaves generation of the remaining
packaging to [`ubucargo package`](package.md).

The exact crate version, registry checksum, and crates.io origin are retained as
acquisition metadata for `package`, which uses them to generate
`cargo-checksum.json`, `watch`, and the initial changelog independently of
debcargo's local-source mode. The persistent artifact representation remains
part of the source-package lifecycle issue below.

The source tree is created at `DIR`, which defaults to a directory named after
the Debian source package in the current directory. Existing Debian Rust naming,
feature, and versioning conventions determine the source and eventual binary
package names; the filesystem directory name is not authoritative.

The command refuses to overwrite an existing source-package directory.

## New upstream releases

Ubucargo has no in-place `upgrade` command. Packaging a new upstream release
starts from a fresh import, normally in a separate directory when the previous
source tree is still present:

```console
ubucargo --profile ~/profiles/noble-rust \
  import serde --version 1.0.220 --directory ~/src/rust-serde-new
ubucargo --profile ~/profiles/noble-rust package ~/src/rust-serde-new
```

The maintainer then copies only the still-relevant state from the previous
package, such as `debcargo.toml` settings, changelog history, patches, copyright
corrections, or custom generated-file overrides. Ubucargo does not decide which
old-version customizations should survive.

The original-tarball and Debian-version lifecycle remains open; see
[issue 1](issues.md#1-source-package-artifact-lifecycle).

## Implementation strategy

1. Resolve the profile Rust target.
2. Query crate release metadata and select an exact version.
3. Download the crate through the Cargo registry protocol and verify its
   registry checksum.
4. Retain the exact origin, checksum, and downloaded crate archive.
5. Safely unpack it into a staging directory.
6. Determine the Debian source-package identity and create
   `debian/debcargo.toml`.
7. Atomically move the staged source tree to the requested destination.
