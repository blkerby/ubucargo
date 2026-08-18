# `ubucargo upgrade`

## Synopsis

```console
ubucargo upgrade [PACKAGE] [--version VERSION]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

## Version selection

Without `--version`, ubucargo selects the newest non-yanked stable crates.io
release compatible with the workspace Rust target and capable of retaining the
existing Debian source-package identity. An exact requested version is checked
using the same Rust-version rules as `import`.

If the release requires a different source-package identity, `upgrade` fails.
The maintainer must import the new source package and handle the transition
explicitly.

## Upgrade operation

The command:

1. downloads and verifies the selected crate;
2. constructs a replacement upstream tree in a staging area;
3. preserves `debian/`;
4. applies the existing Debian patch series to materialize the effective
   replacement source; and
5. runs the reconciliation defined by [`package`](package.md#reconciliation).

The working tree is replaced only after acquisition, verification, unpacking,
and patch application succeed. Patch failures are reported and never rewritten
automatically.

Reconciliation may leave conflicts for maintainer resolution. In that case, the
replacement tree, conflicted files, and updated generator state are retained,
and the command exits non-zero.

Manual changelog entries and patches remain for maintainer review.

## Version control

The initial implementation operates only on source-tree contents. It does not
import upstream history, create commits, switch branches, or otherwise update a
repository.

Protection against modified non-`debian/` files remains open; see
[issue 2](issues.md#2-upgrade-can-destroy-maintainer-changes).

The new original-tarball lifecycle remains open; see
[issue 1](issues.md#1-source-package-artifact-lifecycle).

## Implementation strategy

1. Validate the current source-package identity and conflict state.
2. Resolve and verify an exact crate release.
3. Retain the exact origin, checksum, and downloaded crate archive.
4. Safely unpack it into a staging tree.
5. Copy the current `debian/` directory into the staging tree.
6. Apply the patch series using standard source-package tooling.
7. Pass the resulting effective source to the debcargo staging adapter and run
   ubucargo reconciliation.
8. Replace the working tree only after the pre-reconciliation steps succeed.
