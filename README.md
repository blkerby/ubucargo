# Ubucargo

Ubucargo maintains Debian packaging for Rust crates while preserving local
changes to generated files.

The initial MVP implements:

```console
ubucargo package [PACKAGE] [--check] [--keep PATH]... [--replace PATH]...
```

It currently requires Cargo, quilt, debcargo 2.8.4, and an existing source
package containing `Cargo.toml`, `debian/debcargo.toml`, and
`debian/changelog`. See [`docs/package.md`](docs/package.md) for behavior.

`--check` exits 0 when clean, 1 when files would change, and 2 on errors or
unresolved ambiguities.
