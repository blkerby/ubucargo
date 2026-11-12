# Exporting packaging to debcargo-conf

```console
ubucargo debian-export [PACKAGE_DIR] --debcargo-conf DIR [--overwrite]
```

`debian-export` prepares an overlay for contributing an Ubuntu Rust package to Debian through debcargo-conf. Its input is an existing local Ubucargo source package backed by a registry release. `PACKAGE_DIR` defaults to the nearest source-package directory at or above the current directory. Packages configured with `crate_src_path` require conversion to a registry release before export.

`--debcargo-conf DIR` names an existing debcargo-conf root containing `src/`. The export creates `DIR/src/<crate>/debian/`, using Debian's normalized crate name and the semver suffix selected by `semver_suffix` when applicable.

## Existing exports

An existing target crate directory causes an error suggesting `--overwrite`. With that flag, Ubucargo validates the input tree, removes the complete target crate directory, and writes the export directly to that location. This discards edits and obsolete files from the previous export. An error during replacement can leave a partial export. The source package and export must be separate, non-nested trees.

## Packaging files

Export requires the ownership manifest, `debian/ubucargo-state.json`, from a completed `ubucargo package` run. Ubucargo compares managed files with the generated hashes and executable status recorded in that manifest. Matching files are omitted from the overlay; modified files are preserved. A missing manifest or required baseline causes an error directing the maintainer to run `ubucargo package`.

A deleted generated file produces a warning: debcargo may regenerate that file when using the exported overlay. Other packaging files are retained, with file permissions and symbolic links preserved. After filtering, the export retains directories containing exported files or symbolic links.

The exporter applies these transformations:

- `debcargo.toml` retains its settings and comments, sets `overlay = "."`, and removes the standard Ubuntu `maintainer` setting so debcargo uses its Debian default.
- An overridden `control` receives the Debian Rust team maintainer and debcargo-conf VCS fields, honoring explicit maintainer and VCS settings in the exported configuration. Ubuntu original-maintainer fields are removed. Other fields retain their contents.
- Maintained patches and series text are retained. Automatic patches under `patches/auto/` and their series entries are omitted for debcargo to regenerate. An empty filtered series is omitted.
- Debcargo supplies the standard `source/format` containing `3.0 (quilt)` during generation. Custom source formats and other maintained files such as `source/options` are retained in the overlay.
- `ubucargo-state.json` and `.debcargo.hint` files are omitted. The `.debcargo.hint` files are expected to be recreated in the overlay when the maintainer runs the debcargo-conf workflow.

## Changelog

The exporter removes consecutive `UNRELEASED` entries from the top of the Ubuntu changelog and preserves the remaining history verbatim. It prepends an initial Debian `UNRELEASED` entry using the current package's upstream version and epoch with Debian revision `1`. The entry says `Exported using ubucargo <VERSION>.`, recording the version used for the export. When all Ubuntu entries are unreleased, the result contains just the new Debian entry.

The new entry is created with `dch`, using `DEBFULLNAME` and `DEBEMAIL` as described in the README. Published history may describe an older upstream release; the current package's top entry supplies the exported version.

## Review and validation

Export operates on the current packaging files. After export, review the configuration, overrides, patches, and changelog, then use debcargo-conf's workflow to regenerate, build, and test the Debian package.

The command exits 0 on success, including exports with deletion warnings, and 2 on errors.
