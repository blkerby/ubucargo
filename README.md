# Ubucargo

Ubucargo is a tool for creating and maintaining Ubuntu packages for Rust crates, translating Cargo dependencies in `Cargo.toml` into Debian source package data including `debian/control`. It is a wrapper around `debcargo`, the corresponding tool for Debian packages.

Disclaimer: This software is currently **experimental**. It may contain bugs, and its CLI may change.

## Overview

Ubucargo is designed to operate directly on a Debian source package, with its `debcargo.toml` and related configuration embedded in the packaged `debian` directory. This differs from the usual Debian `debcargo` workflow, in which the configuration primarily resides in an external [debcargo-conf](https://salsa.debian.org/rust-team/debcargo-conf) repository. For Ubuntu, a separate configuration repository would be difficult to reconcile with source packages synced from Debian and with independent maintenance across various Ubuntu series. Treating the Archive as the source of truth for the packaging state, including the `debcargo.toml`, keeps things simpler.

## Benefits

Benefits of Ubucargo include the following:

- Maintainers can override Debian packaging in place (such as `debian/control`) without risk of them being overwritten by `ubucargo`, and without needing to manage a separate overlay directory. When `ubucargo package` runs, it writes the generated alternative to a corresponding `.debcargo.hint` file wherever it differs from the primary file.
- Regenerating source packaging can be done without interfering with local files such as a `.git` directory. This way `ubucargo` can be conveniently used in conjunction with tools such as `git-ubuntu` and `gbp`.
- Ubucargo invokes `debcargo` internally, to ensure good alignment with Debian Rust packaging policy.

## How it works

The main complication of this approach is that when running `ubucargo package` on an existing package, it must infer which packaging files are generator-owned (eligible to be overwritten by the new generated output) vs. which ones are maintainer overrides that should be preserved. The way that `ubucargo` handles this is to keep track of content hashes for latest generated content in a manifest at `debian/ubucargo-state.json`. Files matching that record are considered generator-owned and can be updated automatically; changed or deleted files are preserved as maintainer overrides. The status of each generated file is displayed in the output. Existing Debian `.debcargo.hint` files establish the baseline when no manifest entry exists (e.g. when running `ubucargo package` for the first time on a package synced from Debian). When the manifest record and hint are both missing or conflicting, a one-time explicit `--keep` or `--replace` decision is required from the maintainer.

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
      ubucargo-state.json         # latest generated hashes
      control                     # generated
      rules                       # generated
      patches/                    # maintainer-owned except generated auto/
      changelog                   # maintainer-owned, but generator can initialize/update it.
      copyright                   # maintainer override in this example
      copyright.debcargo.hint     # latest generated alternative
```

## Commands

| Command            | Purpose                           | Detailed specification               |
| ------------------ | --------------------------------- | ------------------------------------ |
| `ubucargo package` | Create or update a source package | [`docs/package.md`](docs/package.md) |
| `ubucargo import` | Import a published source package unchanged | [`docs/import.md`](docs/import.md) |
| `ubucargo deps`    | Inspect dependency candidates     | [`docs/deps.md`](docs/deps.md)       |

These commands share a common syntax for specifying an input crate or syntax, including crates.io crates (`crate:<NAME>`), local Debian source packages (`pkg:<PATH>`), local Cargo crates (`local:<PATH>`), Ubuntu Archive sources (`archive:<SUITE>/<SOURCE>`), and PPA sources (`ppa:<OWNER>/<NAME>/<SERIES>/<SOURCE>`). See [input selectors](docs/inputs.md) for details.

### `package`

```console
ubucargo package [INPUT [VERSION]] [--package-dir DIR] \
  [--check] [--force] \
  [--keep-staging] [--keep PATH]... [--replace PATH]...
```

- Run `ubucargo package CRATE [VERSION]` outside a package to create a new source tree and orig tarball.
- Run `ubucargo package` inside an existing package to regenerate its current release.
- Run `ubucargo package CRATE VERSION` against an existing package to select another release.
- Run `ubucargo package local:CRATE-DIR --package-dir PACKAGE-DIR` to create a package from a local crate that is not on crates.io. The two directories must be separate and non-nested.
- After changing `debcargo.toml`, run `ubucargo package` again.
- `--check` exits 0 when clean, 1 when files would change, and 2 on errors or unresolved ambiguities.

```sh
ubucargo package serde
```

See [`docs/package.md`](docs/package.md) for full behavior and options.

### `import`

```console
ubucargo import INPUT [VERSION] [--package-dir DIR] \
  [--keep-staging]
```

`import` downloads and extracts a published Archive/PPA source package without regenerating it, placing its orig tarballs beside the source directory:

```sh
ubucargo import resolute/rust-serde
```

See [`docs/import.md`](docs/import.md) for full behavior and options.

### `deps`

```console
ubucargo deps [INPUT [VERSION]] [--series SERIES] \
  [--proposed] [--ppa ppa:OWNER/NAME]... [--architecture ARCH]
```

`deps` reports the direct Rust library dependencies, represented by `librust-*-dev` packages needed to build and install every binary package and to run autopkgtests.

- Run `ubucargo deps CRATE [VERSION]` to inspect a crates.io release without creating a source package.
- Run `ubucargo deps` inside a source package, or use `pkg:<DIR>` to select one explicitly.
- Use `--series SERIES` to select an Ubuntu series for resolving dependencies. This defaults to the current Ubuntu development series.
- Add `--proposed` to include the selected series' proposed pocket.
- `deps` does not modify the source package.
- It exits 0 when all dependencies are satisfiable, 1 when any are incompatible or missing, and 2 on errors.

See [`docs/deps.md`](docs/deps.md) for details.

## Requirements

It currently requires APT, Cargo, curl, GnuPG, quilt, devscripts, distro-info (for automatic development-series selection), GNU coreutils (including `sha256sum` and `sha512sum`), ubuntu-dev-tools, and debcargo 2.8.4 or a later compatible 2.x release.
