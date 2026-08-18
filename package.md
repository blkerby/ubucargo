# `ubucargo package`

## Synopsis

```console
ubucargo package [PACKAGE] [--check]
```

`PACKAGE` is a source-package directory and may be omitted when the current
directory is inside one.

`package` reads the effective source and `debian/debcargo.toml`, generates
ubucargo-owned packaging files in a staging area, and reconciles them with the
working tree. It is used both to populate packaging after `import` and to refresh
downloaded or previously generated packaging.

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
is the merge base for future changes.

The fixed paths above and the generated feature-package override naming pattern
are generator-owned filename spaces. A primary file with a companion hint is
also generator-owned. Existing paths in these spaces are reconciled even when
the current generator stops emitting them; paths outside these spaces are
ignored.

`debian/changelog` is create-only: `package` may create an initial entry when
absent but never replaces or removes one. `debian/debcargo.toml`,
`debian/patches/`, and all other paths are maintainer-owned.

The source-package artifact lifecycle remains open; see
[issue 1](issues.md#1-source-package-artifact-lifecycle).

## Reconciliation

For each generator-owned path, reconciliation has three values:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the generated staging file

Each value includes path presence and, when present, file contents and the
executable bit. An absent file is distinct from a present empty file, and
equality compares all represented state.

During `download`, Debian's absent-hint convention is normalized by creating
the missing hint. During normal reconciliation, an absent hint represents an
absent generated base.

| Condition | Behavior |
| --------- | -------- |
| `old == base` | The maintainer made no change; take `new` |
| `new == base` | The generator made no change; keep `old` |
| `old == new` | Both reached the same value; keep it |
| Otherwise | Three-way merge; report a conflict if it cannot be reconciled cleanly |

After reconciliation, the stored generator state becomes `new`: write the hint
when `new` is present and remove it when `new` is absent. Taking a value likewise
creates, replaces, or removes the primary path according to its presence.
The hint's executable bit records the generated base mode and follows the same
three-way value rules.

When `old` and `new` are present, content merging should be compatible with
standard `diff3` behavior. An absent `base` may be supplied as an empty temporary
input for content merging, while remaining distinct from a present empty file
for path-level comparisons. The merge must not require Git.

## Conflicts

Conflicted files use `diff3`-style markers labeled `maintainer`,
`previous generated`, and `new generated`.

If one side deletes a file while the other changes it, ubucargo creates a
primary file whose absent side is an empty marker section labeled
`maintainer (deleted)` or `new generated (deleted)`. The hint still records the
new generator state: it is written for a generator modification and removed for
a generator deletion.

The command reports every conflicted path and exits non-zero. Conflict markers
persist until the maintainer resolves the primary path to either a marker-free
file or an absent file. `package` refuses to reconcile a path containing markers
from a previous run, and `build` likewise refuses to proceed.

## Check mode

```console
ubucargo package ./rust-serde --check
```

`--check` performs generation and reconciliation analysis without writing. It
reports whether the primary files or hints would change and whether conflicts
would result.

## Generation boundary

Generation creates an in-memory set of relative paths, contents, and executable
bits. Generator code does not choose the workspace location or write directly
into it. A separate reconciliation step applies accepted changes atomically.

`package` must not require network access. The staged debcargo process uses a
local crate source and runs with Cargo network access disabled.

When the source contains multiple applicable Cargo packages, selection must be
explicit. The selection interface remains open; see
[issue 5](issues.md#5-cargo-workspace-package-selection-has-no-interface).

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
ubucargo reconciles them afterward. An existing changelog is the only initial
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

Ubucargo checks the debcargo version before invocation. Compatibility tests
should run the adapter over representative debcargo-conf packages, including
libraries, binaries, semver-suffixed crates, patched manifests, feature-heavy
packages, and manually overridden generated files.

## Implementation strategy

1. Validate the source-package identity and unresolved-conflict state.
2. Materialize the effective patched source in a staging directory.
3. Adapt `debian/debcargo.toml` and prepare the minimal synthetic overlay.
4. Invoke a supported debcargo version without network access or write-back.
5. Extract debcargo candidates and replace origin-sensitive candidates using
   acquisition metadata.
6. Build the complete in-memory generated path set, including executable bits.
7. Reconcile the union of generated and existing generator-owned paths.
8. In check mode, report the result and stop.
9. Otherwise apply all primary and hint changes atomically.
