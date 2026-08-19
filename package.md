# `ubucargo package`

## Synopsis

```console
ubucargo package [PACKAGE] [--check]
```

`PACKAGE` is a source-package directory and may be omitted when the current
directory is inside one.

`package` reads the effective source and `debian/debcargo.toml`, generates
ubucargo-owned packaging files in a staging area, and materializes them in the
working tree while preserving inferred maintainer overrides. It is used both to
populate packaging after `import` and to refresh downloaded or previously
generated packaging.

Ubucargo materializes the effective patched source before invoking the
generator. The synthetic debcargo overlay does not contain `debian/patches`, so
debcargo does not apply the patch series a second time.

## Generated paths

The generated candidate set may contain:

- `debian/cargo-checksum.json`
- `debian/control`
- `debian/copyright`
- `debian/rules`
- `debian/source/format`
- `debian/watch`
- `debian/tests/control`, for library packages
- `debian/<feature-package>.lintian-overrides`, for each generated non-base
  feature package

Debcargo initially generates package naming, feature layout, dependency
translation, control, copyright, rules, tests, source format, and feature
overrides. Ubucargo generates origin-sensitive files such as
`cargo-checksum.json`, `watch`, and an initial changelog from verified
acquisition metadata because debcargo's local-crate mode does not retain the
crates.io checksum or origin.

For every emitted `<file>`, ubucargo stores `<file>.debcargo.hint`, even when
their contents are identical. The hint records the latest generator state and
provides the comparison value used to detect a maintainer override.

The fixed paths above and the generated feature-package override naming pattern
are generator-owned filename spaces. A primary file with a companion hint is
also generator-owned. Existing paths in these spaces are considered even when
the current generator stops emitting them; paths outside these spaces are
ignored.

`debian/changelog` is create-only: `package` may create an initial entry when
absent but never replaces or removes one. `debian/debcargo.toml`,
`debian/patches/`, and all other paths are maintainer-owned.

Persistent `.dsc`, source `.changes`, and `.buildinfo` artifacts are produced by
standard Debian tooling outside `package`.

## Override detection and materialization

For each generator-owned path, materialization has three values:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Each value includes path presence and, when present, file contents and the
executable bit. An absent file is distinct from a present empty file, and
equality compares all represented state.

During `download`, Debian's absent-hint convention is normalized by creating
the missing hint. During normal materialization, an absent hint represents an
absent generated base.

An override exists exactly when `old != base`; no explicit override list is
stored in `debcargo.toml`.

| Condition | Meaning | Behavior |
| --------- | ------- | -------- |
| `old == base` | Unmodified generated file | Replace both primary and hint with `new` |
| `old != base` | Maintainer override | Preserve `old`; replace the hint with `new` |

Writing a value creates or replaces the path when present and removes it when
absent. The same rule therefore handles deletions:

- primary absent and hint present is a maintainer deletion;
- primary present and hint absent is a maintainer file at a currently
  ungenerated path;
- if the generator stops emitting an unmodified path, both files are removed;
- if the generator stops emitting an overridden path, the primary is preserved
  and the hint is removed.

The hint's executable bit records the generated mode and participates in
equality. Restoring the primary to the hint value removes the override. If the
hint is absent, removing the primary removes the override.

When an overridden path receives changed generator output, `package` preserves
the primary, reports the generator delta from `base` to `new`, and updates the
hint. It does not retain historical generator state or perform a merge.

## Check mode

```console
ubucargo package ./rust-serde --check
```

`--check` performs generation and materialization analysis without writing. It
reports whether primary files or hints would change, identifies inferred
overrides, and shows generator changes for overridden paths.

## Generation boundary

Generation creates an in-memory set of relative paths, contents, and executable
bits. Generator code does not choose the source-tree location or write directly
into it. A separate materialization step applies primary and hint changes
atomically.

`package` must not require network access. The staged debcargo process uses a
local crate source and runs with Cargo network access disabled.

The root `Cargo.toml` must contain `[package]`; its name and version define the
primary crate passed to debcargo. Nested workspace members are treated only as
internal build dependencies. A virtual workspace root without `[package]` is
unsupported and fails with a manual-packaging diagnostic.

## Debcargo staging adapter

Ubucargo constructs a disposable layout similar to:

```text
stage/
  source/            # effective patched crate source
  overlay/
    changelog        # present only when the package already has one
  debcargo.toml      # adapted temporary configuration
  output/
```

The temporary TOML preserves debcargo settings, removes the `[ubucargo]`
namespace, and replaces path-valued inputs with staging paths:

```toml
overlay = "/absolute/stage/overlay"
crate_src_path = "/absolute/stage/source"
```

An existing `overlay` must be absent or `"."`, and an existing
`crate_src_path` must be absent or identify the current source package. Other
values fail clearly rather than being silently reinterpreted.

The real `debian/` directory is not copied into the overlay. Existing generated
files and maintainer overrides must not influence the clean candidate set;
ubucargo materializes them afterward. An existing changelog is the only initial
overlay input because debcargo uses it for copyright years. When present,
ubucargo passes `--changelog-ready`.

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

Ubucargo consumes only generated candidates from `stage/output/debian/` and
discards debcargo's staged source tree and orig tarball. It replaces the
local-source versions of `cargo-checksum.json`, `watch`, and an initial
changelog with candidates derived from acquisition metadata.

The canonical orig tarball beside the real source tree is never replaced by
`package`. Changes to archive-filtering settings such as `excludes` or
`repack_suffix` require registry-backed orig preparation through `import` or
`upgrade`, even when reusing the same crate version.

Ubucargo checks the debcargo version before invocation. Compatibility tests
should run the adapter over representative debcargo-conf packages, including
libraries, binaries, semver-suffixed crates, patched manifests, feature-heavy
packages, and manually overridden generated files.

## Implementation strategy

1. Validate the source-package identity and root Cargo package.
2. Materialize the effective patched source in a staging directory.
3. Adapt `debian/debcargo.toml` and prepare the minimal synthetic overlay.
4. Invoke a supported debcargo version without network access or write-back.
5. Discard the local-source orig tarball, extract debcargo candidates, and
   replace origin-sensitive candidates using
   acquisition metadata.
6. Build the complete in-memory generated path set, including executable bits.
7. Materialize the union of generated and existing generator-owned paths using
   inferred override state.
8. In check mode, report the result and stop.
9. Otherwise apply all primary and hint changes atomically.
