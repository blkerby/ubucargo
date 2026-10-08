# Input selectors

`deps` and `package` share input notation; `import` accepts the published Archive/PPA forms:

| Input selector | Selected input | Positional `VERSION` |
|---|---|---|
| `crate:serde` | Crates.io upstream release | Exact Cargo version; default latest |
| `archive:resolute/rust-serde` | Published Ubuntu source package | Exact Debian version; default highest published |
| `ppa:<OWNER>/<NAME>/<SERIES>/<SOURCE>` | Published public PPA source package | Exact Debian version; default highest published |
| `pkg:./rust-serde` | Existing maintained source package | Rejected |
| `local:../serde` | Current local Cargo source | Rejected |

Recognized prefixes select their explicit kind. Unknown prefixes, empty fields, and malformed inputs are errors. Local paths resolve against the working directory.

Automatic spellings have fixed precedence:

1. `.`, `..`, and paths beginning with `./`, `../`, or `/` select a directory. A `debian/debcargo.toml` marker is required to select an existing package. Local Cargo crates must use the explicit `local:PATH` form.
2. `<SUITE>/<SOURCE>` selects an Ubuntu Archive source package.
3. A bare name selects crates.io, regardless of whether a directory with that name exists.

Use `./` for nested filesystem paths to distinguish them from Archive inputs. Explicit `pkg:` requires the package marker. Explicit `local:` ignores packaging in its input tree. For `package`, a new destination uses default configuration; an existing destination supplies maintained packaging, with `crate_src_path` updated to the selected local source. `crate:` inputs also create or update the destination package.

With no input, both commands select the nearest parent package and fail if none is found. `package --package-dir` selects only the destination; it cannot supply a missing input. Explicit and implicit package inputs are updated in place by default. With `--package-dir`, the same directory is updated in place; a different, new directory receives a regenerated copy while the input remains untouched. A different existing destination is rejected.

`package` supports every input kind above. Published inputs are downloaded and regenerated in staging before writing to a new destination. `import` accepts only published Archive/PPA inputs and writes the extracted package unchanged. Both default to `./<SOURCE>`, where `<SOURCE>` is the published Debian source-package name (for example, `./rust-serde`). They accept `--package-dir` and reject existing destinations rather than merging remote and local packaging.

Published selectors fully specify the input location. `<OWNER>` and `<NAME>` identify the PPA, `<SERIES>` is its Ubuntu series (for example, `resolute`), and `<SOURCE>` is the Debian source-package name (for example, `rust-serde`). PPA inputs require all four fields; the former `ppa:OWNER/NAME/SOURCE` form is rejected.

For Archive inputs, `<SUITE>` is either a base series or a series with a pocket suffix. A bare `resolute` considers release, updates, and security. `resolute-updates`, `resolute-security`, `resolute-proposed`, and `resolute-backports` each select only that pocket. Examples:

```sh
ubucargo import archive:resolute-proposed/rust-serde
ubucargo import ppa:owner/staging/resolute/rust-serde
ubucargo deps ppa:owner/staging/resolute/rust-serde --series jammy --proposed
```

Only `deps` accepts `--series` and `--proposed`. They control the dependency environment and never change the input series or pocket. Without `--series`, a published input supplies its base series for dependency checking; other inputs default to the development series. PPA inputs also add their PPA to the checking repositories, queried for the checking series.

The previous `--local-crate` input flag, and `deps --package-dir`, have been removed without compatibility aliases.
