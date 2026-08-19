# `ubucargo upgrade`

## Synopsis

```console
ubucargo [--profile PROFILE] upgrade PACKAGE \
  [--version VERSION] [--directory DIR] [--force]
```

`upgrade` replaces the upstream crate release, preserves durable Debian
packaging, and regenerates all generator-owned files from scratch. It performs
no generated-file merge.

## Version selection

Without `--version`, ubucargo selects the newest non-yanked stable release
compatible with the profile Rust target and capable of retaining the existing
Debian source-package identity. An exact requested version uses the same MSRV
validation and release-selection algorithm as `import`.

If the selected release requires a different source-package identity,
`upgrade` fails and directs the maintainer to create a new package with
`import`.

## Durable and regenerated state

The staged debcargo overlay contains only durable maintainer-owned packaging:

- `debian/changelog`;
- `debian/patches/`;
- maintainer scripts, install files, service units, and other unknown paths; and
- other files outside generator-owned filename spaces.

`debian/debcargo.toml` is passed as the authoritative external generator config
and copied into the completed source tree.

Generator-owned primaries and hints are not copied into the overlay. Existing
overrides are listed before replacement but are not carried into the new
upstream release. The resulting generated primaries and hints begin equal.

## Debcargo registry workflow

Ubucargo invokes debcargo's registry-backed full packaging path with the exact
selected crate version, a synthetic overlay, `--changelog-ready`, and
`--no-overlay-write-back`.

Debcargo and Cargo:

1. download and verify the exact crate;
2. derive the Debian source identity and upstream version;
3. copy or repack the crate as the correctly named orig tarball;
4. extract pristine upstream source;
5. apply the retained patch series temporarily while reading the effective
   manifest; and
6. generate a fresh `debian/` directory.

Patch failures abort before the existing tree is replaced. Debian patches are
not included in the orig tarball.

## Output and safety

Without `--directory`, the completed staged source tree replaces `PACKAGE` only
after acquisition, orig preparation, patch application, and generation succeed.
The new orig tarball is installed beside the source directory.

Ubucargo should refuse in-place replacement when it cannot establish that
non-`debian/` changes are recoverable. The maintainer may instead prepare the
new release non-destructively:

```console
ubucargo upgrade ~/src/rust-serde \
  --version 1.0.220 \
  --directory ~/src/rust-serde-new
```

`--force` explicitly permits discarding unrecorded upstream-tree changes.

If the target orig filename already exists, ubucargo reuses it only when its
contents match the staged artifact and otherwise fails. Source-tree and orig
installation must be rollback-safe.

## Version-control boundary

The command modifies filesystem state only. It does not create commits, import
upstream history, switch branches, create tags, or update pristine-tar data.
Maintainers requiring those operations may use the generated orig tarball with
their normal repository tooling.

## Implementation strategy

1. Read and validate the existing source identity, root Cargo package, and
   `debcargo.toml`.
2. Resolve an exact compatible crate release.
3. Build a synthetic overlay containing durable maintainer-owned packaging but
   no generator-owned primary or hint files.
4. Invoke a supported debcargo version in registry-backed package mode.
5. Validate the resulting source identity, root Cargo package, orig tarball,
   applied patches, and generated packaging.
6. Copy the authoritative config into the staged `debian/` directory and
   normalize all generated hints.
7. Report generated-file overrides that were intentionally reset.
8. Atomically install the completed source tree and orig tarball, or leave them
   at `DIR` for non-destructive review.
