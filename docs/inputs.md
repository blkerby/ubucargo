# Input selectors

`deps` and `package` share input notation:

| Input selector | Selected input | Positional `VERSION` |
|---|---|---|
| `crate:serde` | Crates.io release, freshly generated packaging | Exact Cargo version; default latest |
| `archive:noble/rust-serde` | Published Ubuntu source package | Exact Debian version; default highest published |
| `ppa:OWNER/NAME/SOURCE` | Published public PPA source package | Exact Debian version; default highest published |
| `pkg:./rust-serde` | Existing maintained source package | Rejected |
| `local:../serde` | Current local Cargo crate, ignoring packaging | Rejected |

Recognized prefixes select their explicit kind. Unknown prefixes, empty fields, and malformed inputs are errors. Local paths resolve against the working directory.

Automatic spellings have fixed precedence:

1. `.`, `..`, and paths beginning with `./`, `../`, or `/` select a directory. A `debian/debcargo.toml` marker is required to select an existing package. Local Cargo crates must use the explicit `local:PATH` form.
2. `SERIES/SOURCE` selects an Ubuntu Archive source package.
3. A bare name selects crates.io, regardless of whether a directory with that name exists.

Use `./` for nested filesystem paths to distinguish them from Archive inputs. Explicit `pkg:` requires the package marker. Explicit `local:` ignores maintained packaging and generates with default debcargo configuration.

With no input, both commands retain nearest-parent package selection. `package` also retains its explicit `--package-dir` behavior.

`package` supports crates.io, existing-package, and local-crate inputs. Published-package imports are deferred and produce a clear error; remote/local packaging merge semantics are not defined.

The previous `--local-crate` input flag, and `deps --package-dir`, have been removed without compatibility aliases.
