# `ubucargo package`

## Synopsis

```console
ubucargo package [INPUT [VERSION]] [--package-dir DIR] \
  [--force] \
  [--keep-staging] [--keep PATH]... [--replace PATH]...
```

`package` creates or updates a complete Debian source package: upstream source, orig tarball, generated packaging, configuration, the ownership manifest, and generated-file hints.

`--keep-staging` retains the printed temporary debcargo staging directory for inspection, including when generation fails.

## Input and version selection

The input crate/package can be specified as a crates.io crate (`crate:<NAME>`), local Debian source package (`pkg:<PATH>`), local Cargo crate (`local:<PATH>`), Ubuntu Archive source (`archive:<SUITE>/<SOURCE>`), or PPA source (`ppa:<OWNER>/<NAME>/<SERIES>/<SOURCE>`). See [input selectors](inputs.md) for details and shorthand forms.

When a package input is used, `pkg:<PATH>`, it updates the package in place by default.  `--package-dir` can be used to specify a different, new directory, in which case it receives a regenerated copy while the input remains untouched. A different existing destination is rejected. Ubuntu Archive and PPA inputs download the selected Debian source version and regenerate its maintained packaging in staging before writing to the destination. They require a new destination, defaulting to `./<SOURCE>`, where `<SOURCE>` is the published Debian source-package name (for example, `./rust-serde`). Use `--package-dir` to select another new directory; parent-package discovery does not apply to Archive and PPA inputs. Omitting the input selects the nearest parent package and fails if none is found. Local Cargo crates require explicit `local:PATH`; unprefixed directory paths select only existing source packages.

Ubuntu Archive and PPA inputs preserve the upstream release contained in the selected source package. Their positional `VERSION` selects the published Debian source version. They require usable `debian/debcargo.toml`, Cargo metadata, and changelog, and use the same update rules and `--keep`/`--replace` decisions as an existing package. Extraction's applied quilt patches are popped in staging before regeneration. Acquisition and regeneration failures leave the destination untouched.

The selector determines the source series and pocket. Use `archive:resolute-proposed/rust-serde` to select proposed, or `ppa:owner/staging/resolute/rust-serde` to select a PPA series. A bare Archive series considers release, updates, and security. See [input selectors](inputs.md) for the full grammar. Orig tarballs remain beside the destination; an existing orig tarball with different contents causes an error. Regeneration of Archive and PPA inputs uses the main orig tarball as its source-merge baseline.

`--package-dir` selects the destination for every input kind. It never selects the input. Orig tarballs remain beside this directory.

For crates.io and local crate inputs, an existing destination supplies configuration, patches, maintainer overrides, and generated-file ownership state. A nonexistent destination creates a package with default configuration, even when the current directory is inside another package. An existing destination must be a valid source package for the same Cargo crate; an empty or unrelated directory is rejected. A crates.io input also requires configuration without `crate_src_path`; remove that setting to switch from local source to a registry release.

Without `--package-dir`, a crates.io input uses the nearest parent package containing `debian/control` and `debian/changelog` as its destination; if none is found, the destination defaults to the generated Debian source name in the current directory. Regeneration requires usable `debian/debcargo.toml` and Cargo metadata in an existing package. Explicit and default destinations follow the same rules: create when absent, update when an existing package is found. Paths referring to the same directory through symlinks are treated alike.

Output identifies the operation as `Create new package: DIR`, `Update existing package: DIR`, or `Create package from existing packaging: DIR`. The last form describes a package copy or an Archive or PPA input that carries maintained packaging into a new destination.

To select an existing package from outside its directory, use `ubucargo package pkg:PATH`. Supplying only `--package-dir PATH` outside a package fails because no input has been selected.

Package copies use the same staging and writing flow as in-place updates, Archive inputs, and PPA inputs. Maintainer files, local additions, and generated-file ownership state travel with the copy. Relative `crate_src_path` settings are rebased to keep referring to the same local crate. Input and destination trees must not overlap, and the destination must remain separate from any configured local crate. Applied quilt patches are popped and their original position restored only in staging. Existing orig tarballs with different contents cause an error before writing to the destination.

For an existing package:

- with no `CRATE`, ubucargo regenerates the crate and version identified by the root `Cargo.toml`;
- with `CRATE` and `VERSION`, ubucargo selects that exact release;
- with `CRATE` but no `VERSION`, debcargo selects the latest matching release.

When running Ubucargo on an existing package, the top `debian/changelog` entry must describe the upstream source currently present in the working tree.

For a new crates.io package, `CRATE` is required. `VERSION` requests an exact Cargo version; when omitted, debcargo asks Cargo for the greatest release matching an unconstrained dependency, excluding yanked releases and prereleases. An exact request may select a prerelease or yanked release. This process does not filter releases by MSRV, but if the selected crate's `[package]` table in `Cargo.toml` declares `rust-version`, debcargo includes that minimum version in the generated Debian `rustc` dependencies.

`local:DIR` creates or updates a source package from a local Cargo crate and requires an explicit `--package-dir`. The resolved crate and package directories must be separate directory trees: neither may equal, contain, or be contained by the other. Ubucargo reads the exact crate name and version from the local `Cargo.toml`. For an existing package, its other configuration, patches, and maintainer files are retained. The explicit local path replaces any saved `crate_src_path`, including one that no longer exists, and is written relative to `debian/debcargo.toml`. This configuration change is included in the update plan and written after validation succeeds. Later `package` runs resolve that setting without requiring an explicit local input again; `deps` reads the maintained controls directly.

Local-source packaging retains debcargo's limitation that dependencies must be resolvable from crates.io during generation. The generated Debian package still builds against dependencies declared from the Ubuntu Archive. A local working tree is suitable for iteration, but an Archive upload should use a fixed upstream release or snapshot and may not reuse one upstream version number for differing orig-tarball contents.

The selected release must belong to the same Cargo crate as the existing root `Cargo.toml` (allowing normalized case and underscore/hyphen spelling). The Debian source name may change: adding or removing `semver_suffix`, or upgrading a suffixed package to another semver line, regenerates the package under its new identity in the same working directory. The directory itself is not renamed. Maintainer overrides still follow the normal update rules; if the planned `debian/control` has a `Source` field inconsistent with the new identity, ubucargo issues a warning, since the maintainer would need to correct this before the package can build.

These invocations create or update packages:

```console
# Create the latest serde package.
ubucargo package serde

# Create a particular release in a chosen directory.
ubucargo package serde 1.0.220 --package-dir ./rust-serde

# Create a package from an unpublished local crate.
ubucargo package local:../example --package-dir ./rust-example

# Regenerate an explicit package.
ubucargo package ./rust-serde

# Regenerate a copy in a new directory.
ubucargo package ./rust-serde --package-dir ./copies/rust-serde

# Regenerate the current package at its existing version.
cd rust-serde
ubucargo package

# Move the current package to another release.
ubucargo package serde 1.0.229
```

## Updating packages

Ubucargo invokes debcargo's `package` command for the selected exact release. Debcargo and Cargo obtain the crate from crates.io or the configured local source, apply `debian/debcargo.toml`, derive Debian names and versions, copy or repack the orig tarball, extract the upstream source, apply the retained patch stack temporarily, and generate `debian/`.

When creating a package, ubucargo writes the generated source tree and Debian packaging to the destination directory, adds `debian/debcargo.toml`, and records generated-file fingerprints in `debian/ubucargo-state.json`. Primary files that match the generated output have no corresponding `.debcargo.hint` files.

For both crates.io and local crates, the initial configuration includes:

```toml
maintainer = "Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>"
```

This setting gives debcargo the same maintainer for the first generation and subsequent runs, including the generated `debian/*` copyright attribution. Existing configurations are retained; copying a package rebases any relative local-crate path, and an explicit `local:` input updates that setting.

When updating an existing package, ubucargo preserves durable maintainer-owned state:

- `debian/changelog`;
- `debian/debcargo.toml`;
- maintainer patches and non-automatic entries in `debian/patches/series`;
- maintainer scripts, install files, service units, and other unknown `debian/` paths; and
- maintainer changes to generated files such as `debian/control`.

The selected crate source, orig tarball, automatic patches, and generated packaging come from the fresh debcargo result.

### Changelog

The changelog remains primarily maintainer-owned, but each regeneration of a released package starts a new `UNRELEASED` entry, even when the rest of the package is unchanged. Subsequent runs update that entry. Ubucargo updates the version number using `dch --vendor Ubuntu`:

- a new package starts at `<upstream>-0ubuntu1`;
- a released top entry for the same upstream version is advanced with `dch --increment`, creating a new top entry;
- a released top entry for a different upstream version gets a new `<upstream>-0ubuntu1` entry;
- an `UNRELEASED` top entry for the same upstream version is retained, with ubucargo's generated changelog item added or updated in that entry;
- an `UNRELEASED` top entry for a different upstream version is retained and changed to `<upstream>-0ubuntu1`; and
- `dch` handles existing Ubuntu revision forms such as `ubuntuN`, stable-update suffixes, rebuilds, and other derivative revisions.

A change to the Debian source name follows the same rules: a released top entry is preserved and a new entry is created under the new name; an `UNRELEASED` top entry is renamed in place, retaining the maintainer's change notes and its Debian version when the upstream version is unchanged. A different upstream version starts at `<upstream>-0ubuntu1`. A rename item records both source names, and earlier entries retain their original names and contents.

Ubucargo adds a provenance item recording the crate release and both tool versions:

```text
  * Package serde 1.0.229 from crates.io.
    Generated with debcargo 2.8.4 and ubucargo 0.1.0.
```

Local packages use `from local source` in place of `from crates.io`.

An existing ubucargo provenance item in the current `UNRELEASED` entry is updated in place when the crate or either tool version changes. An unchanged item is not duplicated on later runs.

Ubucargo prepares the staged changelog before final generation and always passes `--changelog-ready` to debcargo. Debcargo therefore reads the prepared changelog for generation but does not modify it.

After every package generation, ubucargo runs `update-maintainer` on the staged package. It preserves current Ubuntu maintainer addresses; otherwise it sets Ubuntu Developers and records the previous maintainer in `XSBC-Original-Maintainer`. Debian-derived packages therefore keep their existing configuration while receiving the Ubuntu control adjustment. Packages created directly by ubucargo already use Ubuntu Developers and do not acquire an original-maintainer field. Copyright overrides remain subject to the normal generated-file update rules.

For an existing package without a control manifest entry or hint, an exact match with debcargo's raw control output establishes generator ownership before this Ubuntu adjustment; other differing controls remain ambiguous.

Ubucargo removes debcargo's Debian-specific `Vcs-Git` and `Vcs-Browser` fields from generated control files. A maintainer-overridden `debian/control` remains unchanged under the normal generated-file update rules.

## Orig tarball and source tree

The orig tarball is placed beside the source directory using Debian naming:

```text
<parent>/
  rust-serde_1.0.220.orig.tar.gz
  rust-serde/
```

When repacking is not required, debcargo copies the verified `.crate` archive unchanged. Matching `excludes` entries cause debcargo to rebuild the orig tarball without those paths. `repack_suffix` supplies the suffix added to the Debian upstream version.

The next package's repack suffix comes only from `debian/debcargo.toml`: an explicit `repack_suffix` is used as written, otherwise it defaults to `ds` when `excludes` is present, or no suffix when it is absent. The existing changelog identifies the old package and does not supply a suffix for the next one. To retain `+dfsg` from an existing version such as `1.0.0+dfsg-1`, set `repack_suffix = "dfsg"` in the configuration.

For an existing package, the old orig tarball is the source-merge baseline. Its source name and upstream version come from the top changelog entry. Ubucargo first looks beside the package, then uses `pull-lp-source --download-only SOURCE VERSION` to retrieve that exact Ubuntu source version independently of the host's configured APT series. Acquisition happens before the staged changelog is changed.

When the source name changes, the existing changelog still identifies the old orig baseline. The generated orig is written beside it under the new source name; the old orig remains available.

`pull-lp-source` verifies the downloaded source files against their `.dsc`; ubucargo only checks that the expected orig tarball was produced. If no old orig can be found, ubucargo stops; `--force` does not bypass a missing merge baseline.

If the candidate orig path already exists, ubucargo reuses it when its bytes match the fresh debcargo result and rejects it when they differ, before writing to the destination. Conflicting orig tarballs are rejected for both package creation and updates, even with `--force`. When repacking changes the orig tarball, use a new `repack_suffix`, such as `ds2`, to give the new orig a distinct upstream version and filename. Other orig tarballs beside the package are left unchanged.

### Source merge

Ubucargo compares three trees outside `debian/`:

- `base`: the source extracted from the old orig tarball;
- `old`: a copy of the current working source with quilt patches unapplied; and
- `new`: the fresh source produced by debcargo.

Paths are updated conservatively:

| Condition                              | Behavior                                                |
| -------------------------------------- | ------------------------------------------------------- |
| `old == base`                          | Accept `new`, including upstream additions and removals |
| `old == new`                           | Keep the common result                                  |
| Path absent from both `base` and `new` | Preserve the local-only path                            |
| Any other difference                   | Report a conflict and make no changes                   |

This preserves VCS administration directories, local CI files, and build artifacts when they are absent from both upstream trees, without inspecting a particular VCS. A newly introduced upstream path that conflicts with a local-only path is reported rather than overwritten.

Before writing to the destination, Ubucargo restores the input's quilt position in the updated staging copy, as described below.

`--force` resolves source conflicts by choosing `new` for paths owned by either upstream tree. Paths absent from both upstream trees remain preserved.

## Patch state

Ubucargo copies the complete `debian/patches/` directory into a temporary debcargo overlay. Debcargo regenerates its automatic patches, prepends them to the series, applies the complete patch stack temporarily, and reads the resulting manifest. If patches fail to apply, the real package is left unchanged.

Every existing-package update uses a complete staging copy, including its `.pc` state. Ubucargo reads the last entry of `.pc/applied-patches` to remember the top applied patch and runs `quilt diff -z` in staging. Unrefreshed changes trigger an error, since otherwise a stale version of the patch would be supplied to debcargo, which would likely be unintended. It then pops the staged stack before comparing upstream source and regenerating packaging.

After materializing the final staged packaging with fresh automatic patches, ubucargo pushes through the remembered patch by name. This restores the same position in the updated series, even if automatic patches earlier in the series may have changed. Inputs with no applied patches remain unapplied. A missing remembered patch or a failure to reapply the stack stops the operation without modifying the destination. Ubucargo writes the resulting source and quilt backup state together, so later `quilt pop` operations use the updated upstream baseline.

Maintainers can regenerate packages with applied quilt patches.

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

During each `ubucargo package` run, ubucargo records generated state in `debian/ubucargo-state.json` for files that support maintainer overrides. It writes `<file>.debcargo.hint` only when fresh generated output differs from the resulting primary in content or executable status. The hint contains the generated alternative from that run. Automatic patches use the generator-owned rules below and have neither manifest entries nor hints.

`debian/cargo-checksum.json` uses these same ownership rules; maintainer edits are preserved. `debian/patches/series` retains the special merge behavior described below and has neither a manifest entry nor a hint.

If debcargo emits an unrecognized path, `package` warns and ignores it. The changelog, configuration, and non-automatic patch files remain maintainer-owned; an explicit `local:` input updates the configuration’s source path.

For a new package, ubucargo retains debcargo's `debian/source/format`. On subsequent regenerations it leaves that file unchanged and does not create a `.debcargo.hint` for it.

### Generated patches

Debcargo may generate patches for configuration-driven source transformations such as `remove_features`. The entire `debian/patches/auto/` namespace is exclusively generator-owned. On regeneration, ubucargo silently replaces edited patches, restores deleted generated patches, and removes files no longer generated. No ownership baseline or keep-or-replace decision is required. 
Therefore, maintainers should not edit these automatically generated patches.

`debian/patches/series` has mixed ownership and does not use a hint. Debcargo receives the complete existing series as overlay input, regenerates the `auto/` entries, and preserves all other lines; ubucargo writes that merged output directly. Generated auto-patch files are written before the series is updated; obsolete auto-patch files are removed afterward.

## Override detection and materialization

The manifest travels with the source package. Version 1 is JSON with paths sorted lexically:

```json
{
  "version": 1,
  "files": {
    "debian/control": {
      "sha256": "<64 lowercase hexadecimal characters>",
      "executable": false
    },
    "debian/watch": null
  }
}
```

Keys are package-relative managed paths. Present files have a SHA-256 content hash and an executable boolean matching Git's file-mode semantics. A `null` entry records generated absence; a missing entry means its baseline is unknown. Entries for previously managed paths remain even after the files disappear. Invalid records, unsupported versions, and paths outside the managed namespaces cause errors before any changes are applied.

For each managed path that supports maintainer overrides, materialization has three values:

- `base`: the previous generated fingerprint or recorded absence
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Comparisons include existence, content hash, and executable status. A missing file differs from an empty file, so deleting a generated file counts as a maintainer override.

Copied files and trees retain their source permissions. Atomic writes of generated files and hints also retain the captured source permissions. Newly created directories and content without a source file, such as the ownership manifest, use ordinary creation permissions filtered by umask. Only executable status participates in ownership comparisons; incidental mode changes do not trigger updates. Preserved overrides and otherwise untouched paths retain their existing permissions. Symbolic links are preserved without changing their targets' permissions.

When `base` is known, including recorded absence, an override exists when `old != base`.

| Condition | Meaning | Behavior |
| --- | --- | --- |
| `old == base` | Unmodified generated file | Take `new` as primary; remove any hint |
| `old != base` | Maintainer override | Preserve `old`; write a hint if `new` exists and differs from `old` |

This comparison is deliberately conservative. Any content or executable-status change preserves the primary as an override rather than risking data loss.

After an update the manifest always records fresh generator output, never preserved maintainer contents. Hints are removed when redundant or when generated output no longer exists. Matching the latest generated state, including absence, clears an override. These rules also handle generator removal and later reintroduction.

### Manual edits between runs

`.debcargo.hint` files reflect the last completed `ubucargo package` run, not necessarily the current set of overrides. Ubucargo does not watch for manual edits. For example, a newly generated `debian/copyright` has no hint. Editing it does not immediately create `debian/copyright.debcargo.hint`. On the next run, ubucargo detects the changed fingerprint, preserves the edit, and writes a hint if the generated alternative still differs—even if the generator output itself has not changed.

Conversely, copying a hint's contents and permissions to the primary leaves a redundant hint until the next run removes it. Both situations are expected. The manifest provides ownership information independently of hint presence, so a missing hint does not imply that a file is unmodified.

Maintainers can build or upload a source package with such manual edits without first refreshing its hints. Provided the manifest remains in the source package, a later ubucargo run can still detect and preserve those edits. Running `ubucargo package` refreshes the generated references when wanted; it is not required solely to keep hints synchronized before building.

## Migration and ambiguous baselines

These baseline rules apply to files that support maintainer overrides; automatic patches are always regenerated as described above.

When a manifest entry is missing, an existing `.debcargo.hint` establishes the baseline, including executable status. Migration happens during ordinary generation: establish the manifest, keep hints for overrides, and remove redundant recognized hints. Unrecognized hints remain untouched.

When a manifest entry and hint both exist, they must agree on contents and executable status. A hint also conflicts with a recorded absence. Conflicts require `--keep` or `--replace`, even when the primary matches one of the records. Missing hints are normal and do not invalidate manifest entries.

Without either baseline, `package` initializes only cases that cannot overwrite existing content:

| `old`   | `new`                | Behavior when `base` is absent                        |
| ------- | -------------------- | ----------------------------------------------------- |
| absent  | absent               | Record generated absence                              |
| absent  | present              | Write `new` and record its fingerprint; no hint       |
| present | equal to `old`       | Record the fingerprint; no hint                       |
| present | different from `old` | Stop without writing; require `--keep` or `--replace` |
| present | absent               | Preserve the primary and record generated absence     |

When neither a manifest entry nor a hint exists, ubucargo also checks whether the existing `debian/control` exactly matches debcargo's freshly generated control output before Ubuntu maintainer adjustments and VCS-field removal. This establishes generator ownership so those adjustments can be applied without requiring a one-time `--replace debian/control` decision solely because ubucargo changes those fields. If the existing file does not match the raw output, this inference supplies no baseline and the rules above apply.

For an ambiguous path, the user may disambiguate by supplying a `--keep` or `--replace` option using a package-relative path:

```console
ubucargo package --keep debian/control
ubucargo package --replace debian/control
```

`--keep` accepts ambiguous paths, preserves the primary, and writes a hint if the generated alternative exists and differs. `--replace` accepts any managed path and adopts its freshly generated state: it installs the generated contents and permissions when present, or deletes the primary when generation produces absence. It also removes any hint. Both options record fresh generated state in the manifest for files that support overrides. They may be repeated for several paths; each path may be named by only one option.

If any ambiguity remains, `package` reports every affected path and makes no changes.

Restoring a primary to its hint contents and permissions relinquishes the override; the next run resumes automatic updates and removes the redundant hint. Generated output converging on the override also removes its hint. Ubucargo does not merge file contents or retain older history.

## Staging and applying changes

The command exits 0 on success and 2 on errors or unresolved ambiguities.

Ubucargo stages the complete candidate before modifying the destination. In-place updates, package copies, Archive inputs, and PPA inputs use the same staging and writing flow. The temporary debcargo overlay contains the durable maintainer-owned packaging needed for generation, including the changelog and patch stack. Existing generated packaging and hints do not affect generation.

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

The manifest is not a generator input. Ubucargo materializes primary files, patch series, hints, and generated ownership state in staging, then restores the original quilt position. Before writing, ubucargo compares this completed tree with the destination and changes only differing paths; unchanged source and packaging files retain their modification times. Regular files are written atomically, and the ownership manifest is the final managed-state write. When writing a changed package, `.pc` is copied with its quilt backup timestamps. For files that support maintainer overrides, an interruption while applying changes may require an explicit decision on rerun; it does not authorize overwriting a changed primary.

Ubucargo applies changes to files only. It does not create commits, branches, tags, pristine-tar data, `.dsc` files, source `.changes`, or `.buildinfo` files. Standard Debian and VCS tools remain responsible for those artifacts.

Ubucargo requires debcargo 2.8.4 or a later compatible 2.x release and checks the installed version before running it.
