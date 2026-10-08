# `ubucargo import`

```sh
ubucargo import INPUT [VERSION] [--package-dir DIR] \
    [--keep-staging]
```

`import` downloads and extracts a published source package without regenerating its packaging. It accepts `archive:<SUITE>/<SOURCE>` (or `<SUITE>/<SOURCE>`) and `ppa:<OWNER>/<NAME>/<SERIES>/<SOURCE>`. `VERSION` is an exact Debian source version; omitted means the highest published version in the selected repositories.

The destination defaults to `./<SOURCE>`, where `<SOURCE>` is the published Debian source-package name (for example, `./rust-serde`). `--package-dir` selects another new directory. Existing destinations are rejected, including when running inside a maintained package. Orig tarballs, including supplementary orig components, are retained beside the destination. An existing orig with identical contents is reused; different contents cause an error before writing to the destination. The downloaded descriptor and Debian archive are not copied to the output.

The selector determines the source location. A bare Archive series such as `resolute` considers release, updates, and security; a suite such as `resolute-proposed` selects only that pocket. PPA inputs require an explicit Ubuntu series, for example `ppa:owner/staging/resolute/rust-serde`. See [input selectors](inputs.md) for all fields and supported pockets. Selection and retrieval use the same signed APT Sources indexes and descriptor verification as `deps`.

Extraction uses `dpkg-source -x`, including its normal quilt patch application. Source and packaging files are copied as extracted; no configuration or ownership state is invented. Unlike `package`, `import` does not require `debian/debcargo.toml` or Cargo metadata.

The command exits 0 on success and 2 on errors. `--keep-staging` retains downloaded/extracted staging, including on failure.

```sh
ubucargo import resolute/rust-serde
ubucargo import ppa:owner/staging/resolute/rust-serde --package-dir ./serde
```
