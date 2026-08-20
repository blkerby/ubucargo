# `ubucargo upgrade`

## Synopsis

```console
ubucargo upgrade PACKAGE [--version VERSION] [--rust-version VERSION] \
  [--directory DIR] [--force]
```

`upgrade` replaces the upstream crate release, preserves maintainer-owned Debian packaging, and regenerates generator-owned files without merging them.

## Version selection

Without `--version`, ubucargo selects the newest non-yanked stable release that keeps the existing Debian source identity. `--rust-version` applies the same MSRV filtering and checks as `import`.

If the source-package identity would change, use `import` to create a new package.

## Durable and regenerated state

The staged debcargo overlay contains only durable maintainer-owned packaging:

- `debian/changelog`;
- `debian/patches/`;
- maintainer scripts, install files, service units, and other unknown paths; and
- files outside generator-owned filename spaces.

`debian/debcargo.toml` remains the generator configuration.

The overlay omits generated files and hints. Ubucargo lists existing overrides, then replaces them with fresh generated files and matching hints.

## Debcargo registry workflow

Ubucargo runs debcargo's registry-backed packaging path with the exact version, a temporary overlay, `--changelog-ready`, and `--no-overlay-write-back`.

Debcargo and Cargo:

1. download and verify the exact crate;
2. derive the Debian source identity and upstream version;
3. copy or repack the crate as the correctly named orig tarball;
4. extract pristine upstream source;
5. apply the retained patch series temporarily while reading the effective manifest; and
6. generate a fresh `debian/` directory.

Patch failures leave the existing tree intact. The orig tarball contains only pristine upstream source.

## Output and safety

Without `--directory`, the staged tree replaces `PACKAGE` only after every step succeeds. The new orig tarball is installed beside it.

In-place replacement requires recoverable non-`debian/` changes or `--force`. For a separate review tree, use:

```console
ubucargo upgrade ~/src/rust-serde \
  --version 1.0.220 \
  --directory ~/src/rust-serde-new
```

`--force` permits discarding unrecorded upstream changes.

If the target orig file exists, ubucargo reuses it only when its contents match. Before atomically installing the source tree and tarball, ubucargo validates the source identity, root package, orig tarball, patches, and generated packaging.

## Version-control boundary

The command changes files only. Commits, branches, tags, and pristine-tar data remain with the maintainer's tools.
