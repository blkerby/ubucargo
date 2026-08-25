# `ubucargo package`

## Synopsis

```console
ubucargo package [PACKAGE] [--check] \
  [--keep PATH]... [--replace PATH]...
```

`PACKAGE` may be omitted when the current directory is inside a source package.

`package` reads the patched source and `debian/debcargo.toml`, generates files in a staging area, and applies them while preserving maintainer overrides. It can populate or refresh packaging.

## Patch state

`package` prepares the effective source in staging without modifying the working tree. It preserves quilt's `.pc` state and any unrefreshed edits to the current patch, then applies only the remaining patches listed in `debian/patches/series`.

Before generation, ubucargo verifies that the complete series is applied. An inconsistent quilt state, a partially applied patch with rejects, or a remaining patch that cannot be applied is an error and leaves the package unchanged. When `.pc` is absent, all patches are considered unapplied.

Ubucargo understands on-disk quilt state but does not inspect Git history or VCS-specific patch queues. A GBP patch queue must be exported to `debian/patches` and the ordinary packaging branch checked out before running `package`.

The temporary debcargo overlay omits `debian/patches`, because the staged source already has each patch applied once.

## Generated paths

Generated files may include:

- `debian/cargo-checksum.json`
- `debian/control`
- `debian/copyright`
- `debian/rules`
- `debian/source/format`
- `debian/watch`
- `debian/tests/control`, for library packages
- `debian/<feature-package>.lintian-overrides`, for each generated non-base feature package

Debcargo generates package names, feature layout, dependencies, control, copyright, rules, tests, source format, and feature overrides. Ubucargo generates origin-dependent files such as `cargo-checksum.json`, `watch`, and the initial changelog from verified acquisition metadata.

For every generated `<file>`, ubucargo stores `<file>.debcargo.hint`. The hint records the latest generator output used to detect maintainer overrides.

The fixed paths above and generated feature-package override names are generator-owned. Other paths are maintainer-owned; a `.debcargo.hint` suffix does not by itself make an arbitrary path generator-owned. Ownership remains even if the current generator stops emitting a known path.

If debcargo emits an unrecognized path, `package` warns and ignores it. Known local-source placeholders such as `cargo-checksum.json` and `watch`, and the existing changelog copied through the staging overlay, are ignored without warnings.

`package` may create `debian/changelog` once, then leaves it to the maintainer. `debian/debcargo.toml` and `debian/patches/` are also maintainer-owned.

## Override detection and materialization

For each generator-owned path, materialization has three values:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Each value includes whether the path exists, its contents, and its executable bit. A missing file differs from an empty file, so deleting a generated file counts as a maintainer change.

When `base` is present, an override exists when `old != base`.

| Condition | Meaning | Behavior |
| --- | --- | --- |
| `old == base` | Unmodified generated file | Replace both primary and hint with `new` |
| `old != base` | Maintainer override | Preserve `old`; replace the hint with `new` |

This byte-for-byte comparison is deliberately conservative. Any content or executable-mode change preserves the primary as an override rather than risking data loss.

## Missing baselines

Existing source trees may not contain a hint for every generated file. `package` initializes only cases that cannot overwrite existing content:

| `old` | `new` | Behavior when `base` is absent |
| --- | --- | --- |
| absent | absent | No change |
| absent | present | Write `new` to both primary and hint |
| equal to `new` | present | Keep the primary and write the matching hint |
| different from `new` | present | Stop without writing; require `--keep` or `--replace` |
| present | absent | Preserve the primary; no generated base exists |

For an ambiguous path, repeat one of these options using a package-relative path:

```console
ubucargo package --keep debian/control
ubucargo package --replace debian/control
```

`--keep` preserves the existing primary and writes `new` as its hint, establishing an override. `--replace` writes `new` to both the primary and hint. The options are accepted only for ambiguous paths, cannot both name the same path, and may be repeated to resolve several paths.

If any ambiguous path lacks a decision, `package` reports every ambiguity and makes no changes. This allows source trees acquired through Git, APT, dgit, GBP, or other tooling to be initialized without trusting their acquisition method or silently replacing local edits.

The same rule handles deletions:

- primary absent and hint present is a maintainer deletion;
- primary present and hint absent is a maintainer file at a path the generator does not emit;
- if the generator stops emitting an unmodified path, both files are removed;
- if the generator stops emitting an overridden path, the primary is preserved and the hint is removed.

The hint also records the executable bit. Restoring the primary to the hint value removes the override; if no hint exists, removing the primary does the same.

When generator output changes for an overridden path, `package` preserves the primary, reports the `base`-to-`new` change, and updates the hint. It keeps no older history and does not merge.

`upgrade` uses these same materialization rules against the newly acquired upstream source.

## Check mode

```console
ubucargo package ./rust-serde --check
```

`--check` reports which primary files or hints would change, identifies overrides and missing-baseline ambiguities, and shows generator changes without writing. `--keep` and `--replace` may be supplied with `--check` to preview their result.

## Generation boundary

Debcargo writes candidate Debian files to a staging directory. Ubucargo then compares them with the working tree and its hints and validates the complete change set before writing. Each replacement is written to a temporary file beside its destination and renamed into place; deletions are likewise individual operations. Primary files are changed before their hints, so an interrupted run is interpreted conservatively as an override or deletion. An interrupted run may therefore leave some planned paths updated, but each path remains complete and rerunning `package` safely converges on the intended state.

`package` uses only local source and runs debcargo with Cargo offline mode enabled.

The staged root `Cargo.toml` must contain `[package]`; Cargo metadata supplies its name to debcargo, while debcargo reads the local package version itself. Nested workspace members are internal dependencies. Virtual workspaces require manual packaging.

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

The temporary TOML keeps debcargo settings, removes `[ubucargo]`, and points path settings at staging:

```toml
overlay = "/absolute/stage/overlay"
crate_src_path = "/absolute/stage/source"
```

`overlay` must be omitted or `"."`. `crate_src_path` must be omitted or point to the current source package. Other values cause an error.

The overlay contains only an existing changelog, used for copyright years. When present, ubucargo passes `--changelog-ready`. Existing generated files and overrides do not affect generation.

The initial invocation is equivalent to:

```console
CARGO_NET_OFFLINE=true \
debcargo package \
  --config /absolute/stage/debcargo.toml \
  --directory /absolute/stage/output \
  --no-overlay-write-back \
  [--changelog-ready] \
  CRATE
```

Ubucargo uses only `stage/output/debian/` and discards debcargo's staged source tree and orig tarball. Registry-backed `import` and `upgrade` supply `cargo-checksum.json`, `watch`, and the initial changelog from verified acquisition metadata. Offline `package` preserves those existing files and their hints rather than replacing them with local-source placeholders.

`package` leaves the real orig tarball unchanged. Changes to `excludes`, `repack_suffix`, or similar archive settings require `import` or `upgrade`, even for the same crate version.

Ubucargo checks the debcargo version before running it. Representative debcargo-conf packages serve as compatibility fixtures for libraries, binaries, semver-suffixed crates, patched manifests, feature-heavy packages, and overridden generated files.

If debcargo's current CLI boundary proves too brittle, the preferred next step is a narrow generator-only debcargo command rather than an independent implementation of its generation behavior.

## Build and version-control boundary

`package` changes source-package files only. It does not build, sign, or upload the package, or create commits, branches, tags, or pristine-tar data.

Maintainers use `sbuild`, Launchpad, or another build service and standard source-package tools directly. For example, persistent `.dsc`, source `.changes`, and `.buildinfo` files can be produced with:

```console
dpkg-buildpackage -S --no-sign
```

Repository-specific Git, GBP, or dgit workflows must provide a tree that standard Debian tools can export.
