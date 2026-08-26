# `ubucargo package`

## Synopsis

```console
ubucargo package [PACKAGE] [--check] \
  [--keep PATH]... [--replace PATH]...
```

When supplied, `PACKAGE` must be the source-package root. When omitted, ubucargo searches upward from the current directory for the nearest `debian/debcargo.toml`. The staged Cargo metadata step later validates the root `Cargo.toml`.

`package` reads the root crate identity and `debian/debcargo.toml`, generates files in a staging area from the exact crates.io release, and applies them while preserving maintainer overrides. It can populate or refresh packaging.

## Patch state

`package` uses debcargo's registry-backed source acquisition, then applies the package's complete quilt series through a temporary overlay. The exact crate name and version come from the root `Cargo.toml`.

When a quilt patch is applied in the working tree, ubucargo runs `quilt diff -z` without modifying the tree. Unrefreshed changes are an error because they are not present in the patch files supplied to registry-backed generation.

Ubucargo understands on-disk quilt state but does not inspect Git history or VCS-specific patch queues. For example, if a GBP patch queue is used, it must be exported to `debian/patches` and the ordinary packaging branch checked out before running `package`.

Ubucargo copies the complete `debian/patches/` directory into the temporary debcargo overlay. Debcargo regenerates its automatic patches, prepends them to the series, applies the complete patch stack to its extracted crate, and reads the resulting manifest. Patch failures leave the real package unchanged.

If generation would change an automatic patch or the generated `auto/` portion of `debian/patches/series` while the real quilt stack is applied, `package` refuses to write. The maintainer must pop the real stack first; `--check` may still be used to preview the generated changes.

This arrangement prevents existing automatic patches from hiding configuration changes such as a changed `remove_features` value: debcargo always starts from the exact registry release and regenerates those patches itself.

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
- `debian/patches/auto/<patch>`, for debcargo-generated source transformations

Debcargo generates package names, feature layout, dependencies, control, copyright, rules, tests, source format, feature overrides, `cargo-checksum.json`, and `watch` from the registry source. Ubucargo leaves the existing changelog to the maintainer.

For every generated `<file>`, ubucargo stores `<file>.debcargo.hint`. The hint records the latest generator output used to detect maintainer overrides.

The fixed paths above, generated feature-package override names, and files below `debian/patches/auto/` are generator-owned. Other paths are maintainer-owned; a `.debcargo.hint` suffix does not by itself make an arbitrary path generator-owned. Ownership remains even if the current generator stops emitting a known path.

If debcargo emits an unrecognized path, `package` warns and ignores it. The existing changelog copied through the staging overlay is ignored without warnings.

`package` may create `debian/changelog` once, then leaves it to the maintainer. `debian/debcargo.toml` and non-automatic files below `debian/patches/` are also maintainer-owned.

### Generated patches

Debcargo may generate patches for configuration-driven source transformations such as `remove_features`. Ubucargo materializes files below `debian/patches/auto/` using the ordinary hint rules.

`debian/patches/series` has mixed ownership and does not use a hint. Debcargo receives the complete existing series as overlay input, regenerates the `auto/` entries, and preserves all other lines; ubucargo writes that merged output directly. Generated auto-patch files are written before the series is updated; obsolete auto-patch files are removed afterward.

## Override detection and materialization

For each generator-owned path, materialization has three values:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Each value includes whether the path exists, its contents, and its Unix permission mode. A missing file differs from an empty file, so deleting a generated file counts as a maintainer change.

When `base` is present, an override exists when `old != base`.

| Condition | Meaning | Behavior |
| --- | --- | --- |
| `old == base` | Unmodified generated file | Replace both primary and hint with `new` |
| `old != base` | Maintainer override | Preserve `old`; replace the hint with `new` |

This comparison is deliberately conservative. Any content or permission-mode change preserves the primary as an override rather than risking data loss.

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

The hint also records the permission mode. Restoring the primary to the hint value removes the override; if no hint exists, removing the primary does the same.

When generator output changes for an overridden path, `package` preserves the primary, reports the `base`-to-`new` change, and updates the hint. It keeps no older history and does not merge.

`upgrade` uses these same materialization rules against the newly acquired upstream source.

## Check mode

```console
ubucargo package ./rust-serde --check
```

`--check` reports which primary files or hints would change, identifies overrides and missing-baseline ambiguities, and shows generator changes without writing. `--keep` and `--replace` may be supplied with `--check` to preview their result.

## Generation boundary

Debcargo writes candidate Debian files to a staging directory. Ubucargo then compares them with the working tree and its hints and validates the complete change set before writing. Each replacement is written to a temporary file beside its destination and renamed into place; deletions are likewise individual operations. Primary files are changed before their hints, so an interrupted run is interpreted conservatively as an override or deletion. An interrupted run may therefore leave some planned paths updated, but each path remains complete and rerunning `package` safely converges on the intended state.

`package` uses debcargo's registry-backed mode and may update the Cargo registry index or download the exact crate release.

The root `Cargo.toml` must contain `[package]`; Cargo metadata supplies its exact name and version to debcargo. Nested workspace members are internal dependencies. Virtual workspaces require manual packaging.

## Debcargo staging adapter

Ubucargo creates a temporary layout:

```text
stage/
  overlay/
    changelog        # present only when the package already has one
    patches/         # complete staged patch set; debcargo refreshes auto/
  debcargo.toml      # adapted temporary configuration
  output/
```

The temporary TOML keeps debcargo settings, removes `[ubucargo]`, and points path settings at staging:

```toml
overlay = "/absolute/stage/overlay"
```

`overlay` must be omitted or `"."`. `crate_src_path` is rejected because `package` uses registry-backed generation.

The overlay contains the existing changelog, used for copyright years, and the complete staged patch set. When the changelog is present, ubucargo passes `--changelog-ready`. Existing generated packaging overrides such as `control` and `rules` do not affect generation.

The initial invocation is equivalent to:

```console
debcargo package \
  --config /absolute/stage/debcargo.toml \
  --directory /absolute/stage/output \
  --no-overlay-write-back \
  [--changelog-ready] \
  CRATE VERSION
```

Ubucargo uses only `stage/output/debian/` and discards debcargo's staged source tree and orig tarball. Registry metadata supplies `cargo-checksum.json` and `watch`; the existing changelog remains maintainer-owned.

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
