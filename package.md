# `ubucargo package`

## Synopsis

```console
ubucargo package [PACKAGE] [--check]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

`package` reads the patched source and `debian/debcargo.toml`, generates files in
a staging area, and applies them while preserving maintainer overrides. It can
populate or refresh packaging.

Ubucargo applies patches before invoking the generator, so the temporary
debcargo overlay omits `debian/patches`.

## Generated paths

Generated files may include:

- `debian/cargo-checksum.json`
- `debian/control`
- `debian/copyright`
- `debian/rules`
- `debian/source/format`
- `debian/watch`
- `debian/tests/control`, for library packages
- `debian/<feature-package>.lintian-overrides`, for each generated non-base
  feature package

Debcargo generates package names, feature layout, dependencies, control,
copyright, rules, tests, source format, and feature overrides. Ubucargo generates
origin-dependent files such as `cargo-checksum.json`, `watch`, and the initial
changelog from verified acquisition metadata.

For every generated `<file>`, ubucargo stores `<file>.debcargo.hint`. The hint
records the latest generator output used to detect maintainer overrides.

The paths above, generated feature-package override names, and files with hints
are generator-owned. Other paths are maintainer-owned. Ownership remains even if
the current generator stops emitting a path.

`package` may create `debian/changelog` once, then leaves it to the maintainer.
`debian/debcargo.toml` and `debian/patches/` are also maintainer-owned.

Standard Debian tools produce `.dsc`, source `.changes`, and `.buildinfo` files.

## Override detection and materialization

For each generator-owned path, materialization has three values:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Each value includes whether the path exists, its contents, and its executable bit.
A missing file differs from an empty file, so deleting a generated file counts as
a maintainer change.

`download` creates missing hints from downloaded generated files. Otherwise, a
missing hint means the generated base was absent.

An override exists when `old != base`; no override list is stored.

| Condition | Meaning | Behavior |
| --------- | ------- | -------- |
| `old == base` | Unmodified generated file | Replace both primary and hint with `new` |
| `old != base` | Maintainer override | Preserve `old`; replace the hint with `new` |

The same rule handles deletions:

- primary absent and hint present is a maintainer deletion;
- primary present and hint absent is a maintainer file at a path the generator
  does not emit;
- if the generator stops emitting an unmodified path, both files are removed;
- if the generator stops emitting an overridden path, the primary is preserved
  and the hint is removed.

The hint also records the executable bit. Restoring the primary to the hint value
removes the override; if no hint exists, removing the primary does the same.

When generator output changes for an overridden path, `package` preserves the
primary, reports the `base`-to-`new` change, and updates the hint. It keeps no
older history and does not merge.

## Check mode

```console
ubucargo package ./rust-serde --check
```

`--check` reports which primary files or hints would change, identifies
overrides, and shows generator changes without writing.

## Generation boundary

Generation returns relative paths, contents, and executable bits. A separate step
applies primary and hint changes atomically.

`package` uses only local source and runs debcargo with Cargo network access
disabled.

The root `Cargo.toml` must contain `[package]`; its name and version select the
crate passed to debcargo. Nested workspace members are internal dependencies.
Virtual workspaces require manual packaging.

## Debcargo staging adapter

Ubucargo creates a temporary layout:

```text
stage/
  source/            # effective patched crate source
  overlay/
    changelog        # present only when the package already has one
  debcargo.toml      # adapted temporary configuration
  output/
```

The temporary TOML keeps debcargo settings, removes `[ubucargo]`, and points
path settings at staging:

```toml
overlay = "/absolute/stage/overlay"
crate_src_path = "/absolute/stage/source"
```

`overlay` must be omitted or `"."`. `crate_src_path` must be omitted or point to
the current source package. Other values cause an error.

The overlay contains only an existing changelog, used for copyright years. When
present, ubucargo passes `--changelog-ready`. Existing generated files and
overrides do not affect generation.

The initial invocation is equivalent to:

```console
CARGO_NET_OFFLINE=true \
debcargo package \
  --config /absolute/stage/debcargo.toml \
  --directory /absolute/stage/output \
  --no-overlay-write-back \
  [--changelog-ready] \
  CRATE VERSION
```

Ubucargo uses only `stage/output/debian/` and discards debcargo's staged source
tree and orig tarball. Acquisition metadata supplies `cargo-checksum.json`,
`watch`, and the initial changelog.

`package` leaves the real orig tarball unchanged. Changes to `excludes`,
`repack_suffix`, or similar archive settings require `import` or `upgrade`, even
for the same crate version.

Ubucargo checks the debcargo version before running it. Compatibility tests
should cover representative libraries, binaries, semver-suffixed crates, patched
manifests, feature-heavy packages, and overridden generated files.
