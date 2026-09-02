# `ubucargo package`

## Synopsis

```console
ubucargo package [CRATE [VERSION]] [--directory DIR] [--check] [--force] \
  [--keep-staging] [--keep PATH]... [--replace PATH]...
```

`package` creates or updates a complete Debian source package: upstream source, orig tarball, generated packaging, configuration, and generated-file hints.

`--keep-staging` retains the printed temporary debcargo staging directory for inspection, including when generation fails.

## Target and version selection

`--directory` selects the source-package directory. An existing directory is reconciled as a package; a nonexistent directory always creates a clean package there, even when the current directory is inside another package. Without `--directory`, ubucargo uses the nearest parent containing `debian/debcargo.toml`. If no existing package is found and `CRATE` is supplied, the destination defaults to the generated Debian source name in the current directory.

For an existing package:

- with no `CRATE`, ubucargo regenerates the crate and version identified by the root `Cargo.toml`;
- with `CRATE` and `VERSION`, ubucargo selects that exact release;
- with `CRATE` but no `VERSION`, debcargo selects the latest matching release.

When running Ubucargo on an existing package, the top `debian/changelog` entry must describe the upstream source currently present in the working tree.

For a new package, `CRATE` is required. `VERSION` requests an exact Cargo version; when omitted, debcargo asks Cargo for the greatest release matching an unconstrained dependency, excluding yanked releases and prereleases. An exact request may select a prerelease or yanked release. This process does not filter releases by MSRV, but if the selected crate's `[package]` table in `Cargo.toml` declares `rust-version`, debcargo includes that minimum version in the generated Debian `rustc` dependencies.

The selected release must keep the existing Debian source identity when updating a package in place. A release with a different source identity must be packaged into a new directory.

These invocations use the same reconciliation pipeline:

```console
# Create the latest serde package.
ubucargo package serde

# Create a particular release in a chosen directory.
ubucargo package serde 1.0.220 --directory ./rust-serde

# Regenerate the current package at its existing version.
cd rust-serde
ubucargo package

# Move the current package to another release.
ubucargo package serde 1.0.229
```

## Package reconciliation

Ubucargo invokes debcargo's registry-backed `package` command for the selected exact release. Debcargo and Cargo download and verify the crate, apply `debian/debcargo.toml`, derive Debian names and versions, copy or repack the orig tarball, extract the upstream source, apply the retained patch stack temporarily, and generate `debian/`.

When creating a package, ubucargo installs the complete generated result, adds `debian/debcargo.toml`, and creates matching hints for generated files.

When reconciling an existing package, ubucargo preserves durable maintainer-owned state:

- `debian/changelog`;
- `debian/debcargo.toml`;
- maintainer patches and non-automatic entries in `debian/patches/series`;
- maintainer scripts, install files, service units, and other unknown `debian/` paths; and
- maintainer changes to generated files such as `debian/control`.

The selected crate source, orig tarball, automatic patches, and generated packaging come from the fresh debcargo result.

### Changelog

The changelog remains primarily maintainer-owned, but ubucargo updates the version number using `dch --vendor Ubuntu`:

- a new package starts at `<upstream>-0ubuntu1`;
- a released top entry for the same upstream version is advanced with `dch --increment`, creating a new top entry;
- a released top entry for a different upstream version gets a new `<upstream>-0ubuntu1` entry;
- an `UNRELEASED` top entry for the same upstream version is retained, with ubucargo's generated changelog item added or updated in that entry;
- an `UNRELEASED` top entry for a different upstream version is retained and changed to `<upstream>-0ubuntu1`; and
- `dch` handles existing Ubuntu revision forms such as `ubuntuN`, stable-update suffixes, rebuilds, and other derivative revisions.

Ubucargo adds a provenance item recording the crate release and both tool versions:

```text
  * Package serde 1.0.229 from crates.io.
    Generated with debcargo 2.8.4 and ubucargo 0.1.0.
```

An existing ubucargo provenance item in the current `UNRELEASED` entry is updated in place when the crate or either tool version changes. An unchanged item is not duplicated on later runs.

Ubucargo prepares the staged changelog before final generation and always passes `--changelog-ready` to debcargo. Debcargo therefore reads the prepared changelog for generation but does not modify it.

After generation, ubucargo runs `update-maintainer` on the staged package so Ubuntu revisions use the Ubuntu Developers maintainer and retain Debian's maintainer in `XSBC-Original-Maintainer`. For an existing package without a control hint, an exact match with debcargo's raw control output establishes generator ownership before this Ubuntu adjustment; other differing controls remain ambiguous.

Ubucargo removes debcargo's Debian-specific `Vcs-Git` and `Vcs-Browser` fields from generated control files. A maintainer-overridden `debian/control` remains unchanged under the normal generated-file reconciliation rules.

## Orig tarball and source tree

The orig tarball is placed beside the source directory using Debian naming:

```text
<parent>/
  rust-serde_1.0.220.orig.tar.gz
  rust-serde/
```

When repacking is not required, debcargo copies the verified `.crate` archive unchanged. Matching `excludes` entries cause debcargo to rebuild the archive without those paths. `repack_suffix` supplies the suffix added to the Debian upstream version.

An explicit `repack_suffix` in `debian/debcargo.toml` always wins. Otherwise, when regenerating an existing package, ubucargo preserves a suffix already present after the Cargo version in the top changelog entry, such as `dfsg` in `1.0.0+dfsg-1`. The inferred value is passed only through the temporary debcargo configuration; ubucargo does not add it to the maintainer-owned `debian/debcargo.toml`. New packages default to `ds` when `excludes` is present.

For an existing package, the old orig tarball is the source-merge baseline. Its source name and upstream version come from the top changelog entry. Ubucargo first looks beside the package, then uses `pull-lp-source --download-only SOURCE VERSION` to retrieve that exact Ubuntu source version independently of the host's configured APT series. Acquisition happens before the staged changelog is changed.

`pull-lp-source` verifies the downloaded source files against their `.dsc`; ubucargo only checks that the expected orig tarball was produced. If no old orig can be found, ubucargo stops; `--force` does not bypass a missing merge baseline.

If the candidate orig path already exists, ubucargo replaces it with the fresh debcargo result when its contents differ. Other orig tarballs beside the package are left unchanged.

### Source merge

Ubucargo compares three trees outside `debian/`:

- `base`: the source extracted from the old orig tarball;
- `old`: the current working source; and
- `new`: the fresh source produced by debcargo.

Paths are reconciled conservatively:

| Condition | Behavior |
| --- | --- |
| `old == base` | Accept `new`, including upstream additions and removals |
| `old == new` | Keep the common result |
| Path absent from both `base` and `new` | Preserve the local-only path |
| Any other difference | Report a conflict and make no changes |

This preserves VCS administration directories, local CI files, and build artifacts when they are absent from both upstream trees, without inspecting a particular VCS. A newly introduced upstream path that conflicts with a local-only path is reported rather than overwritten.

The reconciled source tree is always installed with quilt patches unapplied.

`--force` resolves source conflicts by choosing `new` for paths owned by either upstream tree. Paths absent from both upstream trees remain preserved.

## Patch state

Ubucargo copies the complete `debian/patches/` directory into a temporary debcargo overlay. Debcargo regenerates its automatic patches, prepends them to the series, applies the complete patch stack temporarily, and reads the resulting manifest. If patches fail to apply, the real package is left unchanged.

When a quilt patch is applied in the working tree, ubucargo runs `quilt diff -z` without modifying the tree. Unrefreshed changes trigger an error, since otherwise a stale version of the patch would be supplied to debcargo, which would likely be unintended.

If the candidate changes the upstream source, all real quilt patches must be unapplied in both normal and `--check` modes. Changes limited to an automatic patch or the generated `auto/` portion of `debian/patches/series` may be previewed with `--check`, but the real quilt stack must be popped before writing them.

Ubucargo understands ordinary on-disk quilt state but does not inspect Git history or VCS-specific patch queues. A GBP patch queue must be exported to `debian/patches` and the ordinary packaging branch checked out before running `package`.

## Generated paths

Generated files may include:

- `debian/cargo-checksum.json`
- `debian/control`
- `debian/copyright`
- `debian/rules`
- `debian/watch`
- `debian/tests/control`, for library packages
- `debian/<feature-package>.lintian-overrides`, for each generated non-base feature package
- `debian/patches/auto/<patch>`, for debcargo-generated source transformations

For every generated `<file>`, ubucargo stores `<file>.debcargo.hint`. The hint records the latest generator output and is used to detect maintainer overrides to the primary `<file>`.

If debcargo emits an unrecognized path, `package` warns and ignores it. The changelog, configuration, and non-automatic patch files remain maintainer-owned.

For a new package, ubucargo retains debcargo's `debian/source/format`. On
subsequent regenerations it leaves that file unchanged and does not create a
`.debcargo.hint` for it.

### Generated patches

Debcargo may generate patches for configuration-driven source transformations such as `remove_features`. Ubucargo materializes files below `debian/patches/auto/` using the ordinary hint rules.

`debian/patches/series` has mixed ownership and does not use a hint. Debcargo receives the complete existing series as overlay input, regenerates the `auto/` entries, and preserves all other lines; ubucargo writes that merged output directly. Generated auto-patch files are written before the series is updated; obsolete auto-patch files are removed afterward.

## Override detection and materialization

For each generator-owned path, materialization has three values:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Each value includes whether the path exists, its contents, and its Unix permission mode. A missing file differs from an empty file, so deleting a generated file counts as a maintainer override.

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
| present | equal to `old` | Keep the primary and write the matching hint |
| present | different from `old` | Stop without writing; require `--keep` or `--replace` |
| present | absent | Preserve the primary; no generated base exists |

For an ambiguous path, the user may disambiguate by supplying a `--keep` or `--replace` option using a package-relative path:

```console
ubucargo package --keep debian/control
ubucargo package --replace debian/control
```

`--keep` preserves the existing primary and writes `new` as its hint, establishing an override. `--replace` writes `new` to both the primary and hint. The options are accepted only for ambiguous paths, cannot both name the same path, and may be repeated to resolve several paths.

If any ambiguity remains, `package` reports every affected path and makes no changes.

The same rules handle generated-file creation and deletion. Restoring a primary to its hint value removes its override. When generator output changes for an overridden path, `package` preserves the primary and updates the hint to the fresh generator output; it does not merge or retain older history.

## Check mode

```console
ubucargo package --check
ubucargo package serde 1.0.229 --check
```

`--check` reports source-tree, orig-tarball, generated-file, hint, and patch changes without writing. It also identifies overrides, unrecorded source changes, and missing-baseline ambiguities. `--keep`, `--replace`, and `--force` may be supplied to preview their result.

Check mode exits 0 when clean, 1 when the complete package would change, and 2 on errors or unresolved ambiguities.

## Staging and installation

Ubucargo stages the complete candidate before modifying the destination. The temporary debcargo overlay contains the durable maintainer-owned packaging needed for generation, including the changelog and patch stack. Existing generated packaging and hints do not affect generation.

The staged invocation is equivalent to:

```console
debcargo package \
  --config /<TEMPORARY PATH>/stage/debcargo.toml \
  --directory /<TEMPORARY PATH>/stage/output \
  --no-overlay-write-back \
  --changelog-ready \
  CRATE VERSION
```

Ubucargo validates the selected crate identity, Debian source identity, source tree, orig filename and contents, patch stack, generated packaging, and complete materialization plan before writing.

Installation changes files only. It does not create commits, branches, tags, pristine-tar data, `.dsc` files, source `.changes`, or `.buildinfo` files. Standard Debian and VCS tools remain responsible for those artifacts.

Ubucargo requires debcargo 2.8.4 or a later compatible 2.x release and checks
the installed version before running it.
