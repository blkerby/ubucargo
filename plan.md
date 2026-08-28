# Consolidate `import` into `ubucargo package`

## Summary

Replace the separate `import` command with one `package` command that creates clean new packages or reconciles existing packages, including source, orig tarball, changelog, patches, generated files, and hints. Reuse the current debcargo staging and generated-file materialization code; add only the source-tree merge and Ubuntu changelog handling required by the unified behavior.

## CLI and Command Modes

- Change the interface to:
  ```console
  ubucargo package [CRATE [VERSION]] [--directory DIR] [--check] [--force] \
    [--keep PATH]... [--replace PATH]...
  ```
- Remove the `Import` subcommand and `src/import.rs`.
- Resolve the mode as follows:
  - Existing `--directory`: reconcile that package.
  - Nonexistent `--directory`: create a clean new package, ignoring any package around the current directory.
  - No `--directory`: reconcile the nearest parent package, or create a new default-named package when none exists and `CRATE` is supplied.
- Existing package with no crate/version uses its root Cargo identity; `CRATE` without `VERSION` selects latest; `CRATE VERSION` selects that exact release.
- Add `semver` for exact-version validation and conversion to Debian upstream syntax. Use a preliminary temporary `debcargo extract` only when latest-version resolution is needed.

## Reconciliation Pipeline

- Validate the pinned debcargo version, target locking, crate identity, and top changelog invariant before staging.
- For existing packages:
  - Parse only the top changelog entry; require it to describe the current root Cargo version, allowing its repack suffix.
  - Acquire the old orig before changing the changelog: local matching orig, then exact-version `pull-lp-source --download-only`; fail if unavailable, even with `--force`.
  - Extract the old orig as `base`; scan `base`, working `old`, and debcargo `new` as deterministic trees supporting regular files, modes, symlinks, and directories while rejecting special files.
  - Apply the documented merge rules. `--force` selects `new` for conflicting upstream-owned paths; a structural collision also lets `new` discard blocked local-only descendants. Local-only paths with no collision remain untouched.
  - If `base != new`, require every quilt patch to be unapplied in both normal and `--check` modes. Unrefreshed patches always fail.
- Prepare the staged changelog with `dch --vendor Ubuntu`:
  - Create new packages at `<upstream>-0ubuntu1`.
  - Increment released same-upstream entries, create `-0ubuntu1` for a new upstream, and reuse/update matching `UNRELEASED` entries.
  - Normalize the legacy debcargo unreleased marker and add or replace one provenance item containing crate, debcargo, and Ubucargo versions.
  - Always pass the prepared changelog to debcargo with `--changelog-ready`.
- Run final debcargo generation with the exact selected version, then validate source identity, Cargo identity, orig name, and essential packaging.
- For existing packages, combine the source merge plan with the current hint-based Debian plan. Install the candidate orig first, source changes next, generated Debian files and patches afterward, and changelog/hints last; interrupted runs remain rerunnable.
- For clean new packages, write the default `[ubucargo]` config, initialize generated hints, and install the staged tree and orig without inheriting state from an existing package.
- Keep `--check` fully non-writing while reporting orig, source, changelog, patch, generated-file, and hint changes with the existing 0/1/2 exit convention.

## Tests and Documentation

- Add CLI parsing and mode-resolution tests, including explicit nonexistent `--directory` producing a clean package.
- Add unit tests for Cargo-to-Debian version conversion, top-changelog validation, changelog action selection, and provenance replacement.
- Add source-merge tests covering the complete `base`/`old`/`new` table, `--force`, additions/removals, local-only paths, file-type and symlink changes, and structural collisions.
- Add orig-discovery tests for local, Launchpad-download, missing-base, and checksum/name rejection paths using temporary fixtures or fake executables.
- Preserve and rerun existing generated-file, hint, patch-series, and override tests.
- Smoke-test creation, same-version regeneration, latest selection, exact-version change, repacking/exclusions, conflict refusal, forced reconciliation, and `package --check`; finish with formatting, locked tests, Clippy, and documentation link checks.
- Update the docs where implementation decisions refined the contract: nonexistent `--directory` is always a clean package, applied patches block source-changing `--check`, structural `--force` conflicts favor the candidate, and `ds` is explicitly described as debcargo’s default.
