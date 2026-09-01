# Ubucargo

Ubucargo maintains Debian packaging for Rust crates while preserving local
changes to generated files.

The initial MVP implements:

```console
ubucargo package [CRATE [VERSION]] [--directory DIR] [--check] [--force] \
  [--keep PATH]... [--replace PATH]...
```

`package` creates a new source package when no destination exists, regenerates
an existing package at its current version when no crate is supplied, and
selects another release when `CRATE` and `VERSION` are supplied.

It currently requires Cargo, quilt, devscripts, ubuntu-dev-tools, and debcargo
2.8.4 or a later compatible 2.x release. See
[`docs/package.md`](docs/package.md) for behavior.

`--check` exits 0 when clean, 1 when files would change, and 2 on errors or
unresolved ambiguities.
