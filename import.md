# `ubucargo import`

## Synopsis

```console
ubucargo [--profile PROFILE] import CRATE [--version VERSION] [--directory DIR]
```

`import` creates a new packaged source tree and Debian orig tarball from a
crates.io release.

## Version selection

Without `--version`, ubucargo selects the newest non-yanked stable release that
supports the profile Rust version. It uses Cargo semver ordering and ignores
build metadata. Only an exact request may select a prerelease.

A release that requires a newer Rust version is skipped. A missing
`rust_version` produces a warning. Releases with equal precedence produce an
ambiguity error.

```text
Workspace rustc: 1.75
Ignoring foo 4.2.0: requires Rust 1.81
Importing foo 4.1.0
warning: foo 4.1.0 does not declare rust-version; compatibility is unverified
```

For an exact version, a known MSRV mismatch is an error and an undeclared MSRV is
a warning. Selection always resolves to an exact version.

## Output

Ubucargo creates a default staged `debcargo.toml` and invokes debcargo's
registry-backed `package` command with the exact version. Debcargo and Cargo
verify the crate, derive Debian names and versions, create or repack the orig
tarball, extract the source, and generate `debian/`.

Ubucargo then installs `debcargo.toml` and normalizes generated hints.

The source tree is created at `DIR`, which defaults to the Debian source package
name in the current directory. Debian Rust conventions determine the source and
binary package names. The destination must not exist.

Before atomically installing the tree and tarball, ubucargo validates their
identity, names, checksums, root package, and generated packaging.

Use [`ubucargo upgrade`](upgrade.md) for an existing package.

## Orig tarball

The orig tarball is placed beside the source directory using Debian naming:

```text
~/src/
  rust-serde/
  rust-serde_1.0.220.orig.tar.gz
```

Without repacking, debcargo copies the verified `.crate` archive to the orig
filename unchanged. Exclusions or manifest normalization trigger a deterministic
repack and add the configured suffix, such as `+dfsg`, to the upstream version.

Standard Debian tools produce persistent `.dsc`, source `.changes`, and
`.buildinfo` files when needed.
