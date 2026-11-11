# Ubucargo

Ubucargo is a tool for creating and maintaining Ubuntu packages for Rust crates, translating Cargo dependencies in `Cargo.toml` into Debian source package data including `debian/control`. It is a wrapper around [`debcargo`](https://salsa.debian.org/rust-team/debcargo/-/blob/master/README.md), the corresponding tool for Debian packages.

Disclaimer: This software is currently **experimental**. It may contain bugs, and its CLI may change.

## Overview

Ubucargo is designed to operate directly on a Debian source package, with its `debcargo.toml` and related configuration embedded in the packaged `debian` directory. This differs from the usual Debian `debcargo` workflow, in which the configuration primarily resides in an external [debcargo-conf](https://salsa.debian.org/rust-team/debcargo-conf) repository. For Ubuntu, a separate configuration repository would be difficult to reconcile with source packages synced from Debian and with independent maintenance across various Ubuntu series. Treating the Archive as the source of truth for the packaging state, including the `debcargo.toml`, keeps things simpler.

## Benefits

Benefits of ubucargo include the following:

- Maintainers can override Debian packaging in place (such as `debian/control`) without risk of them being overwritten by `ubucargo`, and without needing to manage a separate overlay directory. When `ubucargo package` runs, it writes the generated alternative to a corresponding `.debcargo.hint` file wherever it differs from the primary file.
- Regenerating source packaging can be done without interfering with local files such as a `.git` directory. This way `ubucargo` can be conveniently used in conjunction with tools such as `git-ubuntu` and `gbp`.
- Ubucargo invokes `debcargo` internally, to ensure good alignment with Debian Rust packaging policy.

## How it works

The main complication of this approach is that when running `ubucargo package` on an existing package, it must infer which packaging files are generator-owned (eligible to be overwritten by the new generated output) vs. which ones are maintainer overrides that should be preserved. The way that `ubucargo` handles this is to keep track of content hashes for latest generated content in a manifest at `debian/ubucargo-state.json`. Files matching that record are considered generator-owned and can be updated automatically; changed or deleted files are preserved as maintainer overrides. The status of each generated file is displayed in the output. Existing Debian `.debcargo.hint` files establish the baseline when no manifest entry exists (e.g. when running `ubucargo package` for the first time on a package synced from Debian). When the manifest record and hint are both missing or conflicting, a one-time explicit `--keep` or `--replace` decision may be required from the maintainer. See [migration rules](docs/package.md#migration-and-ambiguous-baselines) for details.

Similarly, when an operation affects the upstream source tree, `ubucargo` must infer which files were part of the old upstream and should be replaced, and which files are local and should be retained. This applies, for example, when upgrading a package to a new upstream version, or when repackaging after changing the `excludes` filter in `debcargo.toml`. Ubucargo compares the current source tree with the old and new upstream sources. Unchanged upstream files are updated automatically, and local additions are retained unless they conflict with new upstream files. Missing or modified upstream files cause an error unless they already match the new upstream.

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

## Installation

### Requirements

On Ubuntu 26.04 LTS, the following command installs these requirements:

```sh
sudo apt install cargo debcargo devscripts dpkg-dev distro-info ubuntu-dev-tools \
  ubuntu-keyring quilt curl gnupg pkg-config libssl-dev
```

On Ubuntu 24.04 LTS, the packaged Rust toolchain is too old to build ubucargo, and the packaged debcargo is older than the minimum supported version of 2.8.2. Run the same command without `cargo` and `debcargo`. Then install a newer Rust toolchain, for example with [rustup](https://rustup.rs/), and install debcargo with Cargo, as the CI workflow does:

```sh
cargo install debcargo --version '^2.8.2' --locked
```

Ubucargo follows the installed debcargo's behavior, so generated output can vary between versions. See [debcargo compatibility](docs/package.md#debcargo-compatibility) for details.

Debian sources and dependency environments additionally require the `debian-archive-keyring` package to be installed.

### Install ubucargo

Ubucargo is published on [crates.io](https://crates.io/crates/ubucargo). After installing the requirements, install it with Cargo:

```sh
cargo install ubucargo
```

By default, Cargo installs `ubucargo` in `~/.cargo/bin`, so make sure that directory is on your `PATH`.

Ubucargo writes changelog entries with `dch` without reading devscripts configuration files. To set the name and email address in those entries, set `DEBFULLNAME` and `DEBEMAIL` in your environment, as described in [dch(1)](https://manpages.ubuntu.com/manpages/resolute/man1/dch.1.html).

## Typical workflow

Ubucargo prepares source packages, but it doesn't build them. For example:

```sh
ubucargo deps serde      # Check the dependencies against the current Ubuntu development series
ubucargo package serde   # Create ./rust-serde and its orig tarball
cd rust-serde
grep -rn FIXME debian/   # Find details that debcargo couldn't fill in
```

Ubucargo hides debcargo's FIXME summary, so search the generated files for `FIXME` yourself. After reviewing and editing the files in `debian/`, use `sbuild` and `autopkgtest` to build and test the package. See [Building and testing packages](https://ubuntu.com/project/docs/contributors/building/) in the Ubuntu project documentation. Your edits are preserved when you run `ubucargo package` again.

## Commands

| Command | Purpose | Detailed specification |
| --- | --- | --- |
| `ubucargo package` | Create or update a source package | [`docs/package.md`](docs/package.md) |
| `ubucargo deps` | Inspect dependency candidates | [`docs/deps.md`](docs/deps.md) |
| `ubucargo debian-export` | Export packaging into debcargo-conf | [`docs/debian-export.md`](docs/debian-export.md) |

The `package` and `deps` commands share a common syntax for specifying an input crate, including crates.io crates (`crate:<NAME>`), local Debian source packages (`pkg:<PATH>`), local Cargo crates (`local:<PATH>`), Ubuntu Archive sources (`ubuntu:<SUITE>/<SOURCE>`), Debian archive sources (`debian:<SUITE>/<SOURCE>`), and PPA sources (`ppa:<OWNER>/<NAME>/<SERIES>/<SOURCE>`). See [input and version selection](docs/package.md#input-and-version-selection) for details.

### `package`

```console
ubucargo package [<INPUT> [<VERSION>]] [--package-dir <DIR>] \
  [--config <PATH>] [--prepare-only] [--force] \
  [--keep-staging] [--keep <PATH>]... [--replace <PATH>]... [--crates-io-api <URL>]
```

- Run `ubucargo package <CRATE> [<VERSION>]` outside a package to create or update its default source-package directory.
- Run `ubucargo package` inside an existing package to regenerate its current release.
- Run `ubucargo package <CRATE> <VERSION>` against an existing package to select another release.
- Run `ubucargo package local:<PATH> --package-dir <DIR>` to create or update a package from a local crate that is not on crates.io. The two directories must be separate and non-nested. [Local builds](docs/package.md#local-builds) use a fixed `+local` version suffix and replace their local orig as the source changes.
- Use `--config <PATH>` to supply the initial `debcargo.toml` when creating a new package.
- Use `--prepare-only` to prepare the upstream source tree without generating packaging. It is useful when switching to a new upstream version, where full package generation could fail due to the existing quilt patches not applying cleanly.
- Regenerating a released package creates a new `UNRELEASED` changelog entry; subsequent runs update that entry.
- `package` exits 0 on success and 2 on errors.

```sh
ubucargo package serde
```

A fresh `--prepare-only` tree contains upstream source, `debian/debcargo.toml`, the changelog, source format, and any `auto` patches. Run `ubucargo package` inside it after porting patches to generate the remaining packaging and ownership manifest. On existing packages, preparation retains maintained packaging and hints. Since `deps` reads the existing control files, full packaging generation would be needed to bring them up to date.

See [`docs/package.md`](docs/package.md) for full behavior and options.

### `deps`

```console
ubucargo deps [<INPUT> [<VERSION>]] [--suite <SUITE>] \
  [--proposed] [--ppa ppa:<OWNER>/<NAME>]... [--architecture <ARCH>] [--keep-staging] \
  [--crates-io-api <URL>]
```

`deps` reports the direct Rust library dependencies, represented by `librust-*-dev` packages needed to build and install every binary package and to run autopkgtests applicable to the selected architecture. Each dependency is checked independently for compatible candidates in the selected repositories. Use `sbuild` and `autopkgtest` to validate complete build and test environments.

- Run `ubucargo deps <CRATE> [<VERSION>]` to inspect a crates.io release without creating a source package.
- Run `ubucargo deps` inside a source package, or use `pkg:<PATH>` to select one explicitly.
- Use `--suite ubuntu:<SUITE>` or `--suite debian:<SUITE>` to select the dependency environment. Bare suite names select Ubuntu. Debian inputs default to their exact suite; Ubuntu and PPA inputs default to their base Ubuntu series. Crates.io and local inputs default to the current Ubuntu development series.
- Add `--proposed` to include a base Ubuntu suite's proposed pocket.
- `deps` does not modify the source package.
- It exits 0 when every reported direct Rust dependency has a compatible candidate, 1 when any are incompatible, missing, or unknown, and 2 on errors. Dependencies reported as `unknown` require manual assessment.

See [`docs/deps.md`](docs/deps.md) for details.

### `debian-export`

```console
ubucargo debian-export [PACKAGE_DIR] --debcargo-conf DIR [--overwrite]
```

Exports a local Ubucargo source package into `DIR/src/<crate>/debian/` for contribution to debcargo-conf. Run it inside a source package or supply its directory. The destination repository must already contain `src/`. Add `--overwrite` to replace the entire previous export for that crate.

The export retains maintainer overrides and published Ubuntu changelog history, and creates an initial Debian `UNRELEASED` entry. Review the result and use debcargo-conf's workflow to repackage, build, and test it. See [`docs/debian-export.md`](docs/debian-export.md) for details.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and how to run checks locally.
