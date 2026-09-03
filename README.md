# Ubucargo

Ubucargo is tool for creating and maintaining Ubuntu packages for Rust crates, translating Cargo dependencies in `Cargo.toml` into Debian source package data such as `debian/control`. It is a wrapper around `debcargo`, the corresponding tool for Debian packages.

## Overview

Ubucargo is designed to operate directly on a Debian source package, with its `debcargo.toml` and related configuration embedded in the packaged `debian` directory. This differs from the usual Debian `debcargo` workflow, in which the configuration primarily resides in an external [debcargo-conf](https://salsa.debian.org/rust-team/debcargo-conf) repository. For Ubuntu, a separate configuration repository would be difficult to reconcile with source packages synced from Debian and with independent maintenance across various Ubuntu series. Treating the Archive as the source of truth for the packaging state, including the `debcargo.toml`, keeps things simpler.

## Benefits

Key benefits of Ubucargo include the following:

- Maintainers can override Debian packaging in place (such as `debian/control`) without risk of them being overwritten by `ubucargo`, and without needing to manage a separate overlay directory. Regardless of overrides, the generated output of `ubucargo` is available as corresponding `.debcargo.hint` files.
- Regenerating source packaging can be done without interfering with local files such as a `.git` directory. This way `ubucargo` can be conveniently used in conjunction with tools such as `git-ubuntu` and `gbp`.
- Ubucargo invokes `debcargo` internally, to ensure good alignment with Debian Rust packaging policy.

## Drawbacks

The main complication of this approach is that `ubucargo` must infer which packaging files are generator-owned (eligible to be overwritten by the new generated output) vs. which ones are maintainer overrides. The basic rule is that a packaging file matching its `.debcargo.hint` byte-for-byte is generator-owned, while one deviating from its `.debcargo.hint` is a maintainer override. If a `.debcargo.hint` file is missing and the new generated output would deviate from the existing file, it is treated as ambiguous and `ubucargo` will prompt the maintainer to disambiguate it. To minimize such ambiguity, `ubucargo` writes `.debcargo.hint` files unconditionally; this deviates from the `debcargo` behavior which only writes hint files if a maintainer override exists for it.

Similarly, when an operation affects the upstream source tree, `ubucargo` must infer which files were part of the old upstream and should be replaced, and which files are local and should be retained. This applies, for example, when upgrading a package to a new upstream version, or when repackaging after changing the `excludes` filter in `debcargo.toml`. To resolve this in a general way, `ubucargo` compares the current source tree with the orig tarball referenced in the top-most `changelog` entry: files in the source tree that are not present in the orig tarball are treated as local additions to be retained, while missing or modified files are treated as inconsistencies resulting in an error.

## Source-package structure

Each source package contains its upstream source, generator input `debcargo.toml`, and Debian packaging, including previous generated state. Its orig tarball sits beside the source directory:

```text
<parent>/
  <source>_<upstream-version>.orig.tar.gz
  <source-package>/
    Cargo.toml
    src/
    debian/
      debcargo.toml               # generator input
      control                     # generated or overridden
      control.debcargo.hint       # latest generated state
      rules                       # generated or overridden
      rules.debcargo.hint         # latest generated state
      patches/                    # maintainer-owned except generated auto/
      changelog                   # maintainer-owned
      copyright                   # generated or overridden
      copyright.debcargo.hint     # latest generated state
```

## Commands

| Command | Purpose | Detailed specification |
| --- | --- | --- |
| `ubucargo package` | Create or update a source package | [`docs/package.md`](docs/package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`docs/deps.md`](docs/deps.md) |

### `package`

```console
ubucargo package [CRATE [VERSION]] [--local-crate DIR] [--package-dir DIR] \
  [--check] [--force] \
  [--keep-staging] [--keep PATH]... [--replace PATH]...
```

- Run `ubucargo package CRATE [VERSION]` outside a package to create a new source tree and orig tarball.
- Run `ubucargo package` inside an existing package to regenerate its current release.
- Run `ubucargo package CRATE VERSION` against an existing package to select another release.
- Run `ubucargo package --local-crate CRATE-DIR --package-dir PACKAGE-DIR` to create a package from a local crate that is not on crates.io. The two directories must be separate and non-nested.
- After changing `debcargo.toml`, run `ubucargo package` again. Archive settings, source transformations, generated packaging, and hints are reconciled together.
- `--check` exits 0 when clean, 1 when files would change, and 2 on errors or unresolved ambiguities.

See [`docs/package.md`](docs/package.md) for full behavior and options.

### `deps`

```console
ubucargo deps [CRATE [VERSION]] [--package-dir DIR] --series SERIES \
  [--proposed] [--ppa ppa:OWNER/NAME]... [--architecture ARCH]
```

- Run `ubucargo deps --series SERIES` inside a source package, or use `--package-dir DIR` to select one explicitly.
- Run `ubucargo deps CRATE [VERSION] --series SERIES` to inspect a crates.io release without creating a source package.
- Add `--proposed` to include the selected series' proposed pocket.
- `deps` does not modify the source package.
- It exits 0 when all dependencies are satisfiable, 1 when any are incompatible or missing, and 2 on errors.

See [`docs/deps.md`](docs/deps.md) for details.

## Requirements

It currently requires APT, Cargo, curl, GnuPG, quilt, devscripts,
ubuntu-dev-tools, and debcargo 2.8.4 or a later compatible 2.x release.
