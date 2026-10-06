# `ubucargo deps`

The command reports the direct Rust library dependencies, represented by `librust-*-dev` package relations that the package needs to build, install every binary package, and run its autopkgtests.

## Synopsis

```console
ubucargo deps [CRATE [VERSION]] [--package-dir DIR] --series SERIES \
  [--proposed] [--ppa ppa:OWNER/NAME]... [--architecture ARCH] [--keep-staging]
ubucargo deps --local-crate DIR --series SERIES \
  [--proposed] [--ppa ppa:OWNER/NAME]... [--architecture ARCH] [--keep-staging]
```

When an existing source package is specified with `--package-dir`, or is implicit based on running from within a source package, `ubucargo deps` reads the existing `debian/control` and optional `debian/tests/control` directly, including all maintainer edits. When a crate is selected from crates.io by supplying `CRATE`, or locally by supplying `--local-crate`, it reads freshly generated versions of those control files.

Source-package mode requires `debian/debcargo.toml` as a marker to identify the source-package root; its configuration and any `crate_src_path` are not read.

In the crates.io case, `VERSION` selects an exact release, while an omitted version selects the latest release using the same rules as [`package`](package.md#target-and-version-selection). `CRATE` and `--package-dir` may not be combined.

`--local-crate` selects a local crate's current contents and reads its name and version from Cargo metadata. Relative paths resolve against the working directory. It may not be combined with `CRATE`, `VERSION`, or `--package-dir`.

Crates.io and explicit local-crate modes resolve the selected crate and generate packaging in a temporary directory. `ubucargo deps` never modifies an existing source package, nor does it create a new source package outside of a temporary directory.

```console
# Inspect the nearest source package.
ubucargo deps --series noble

# Inspect an explicit source package.
ubucargo deps --package-dir ./rust-serde --series noble

# Inspect the latest serde release from crates.io.
ubucargo deps serde --series noble

# Inspect an exact serde release from crates.io.
ubucargo deps serde 1.0.220 --series noble

# Inspect a local checkout without creating a source package.
ubucargo deps --local-crate ../serde --series noble
```

`--keep-staging` retains the temporary debcargo staging directory for inspection in crates.io and explicit local-crate modes, including when generation fails. Its path is printed to standard error. It has no effect in source-package mode, which creates no staging directory.

`--series` selects an Ubuntu release. Ubucargo queries its release, updates, and security pockets from `main` and `universe`. `--proposed` additionally includes the release's proposed pocket with normal candidate consideration. Each `--ppa` adds a public Launchpad PPA's `main` component for the same series; private PPAs are not supported. `--architecture` defaults to `dpkg --print-architecture`.

## Output

The report contains one table for each of these, in order, omitting empty tables:

- `Package: NAME`: the `Depends` of each binary package, in control-file order;
- `Source: NAME`: the source paragraph's `Build-Depends`, `Build-Depends-Arch`, and `Build-Depends-Indep`; and
- `Tests: NAME`: the `Depends` of all autopkgtests, which contain the crate's development dependencies.

The `Source` and `Tests` tables show only relations that no earlier table lists identically. For a library crate, the default build's relations already appear in its binary packages, so the `Source` table usually lists only binaries' dependencies and manual `build_depends` overrides. Relations to the package's own binaries, substitution variables such as `${misc:Depends}`, and autopkgtest's `@` are not reported. Test `Architecture` restrictions are not applied, so every test's dependencies are reported.

When features are collapsed, as debcargo does by default, one binary package lists the dependencies of all features. Otherwise, each feature package lists the dependencies that its feature adds directly; dependencies of features it enables appear in those features' tables.

The report shows each dependency and its candidates:

```text
Package: librust-example-dev
DEPENDENCY  STATUS        LOCATION                            VERSION      REQUIREMENT
serde       selected      ppa:example/rust-staging (noble)    1.0.219-1    1 +derive
            available     noble-updates/universe              1.0.217-1    1 +derive
syn         incompatible  noble/universe                      1.0.109-2    2 -default
foo         missing       -                                   -            3

Tests: rust-example
DEPENDENCY  STATUS        LOCATION                            VERSION      REQUIREMENT
criterion   missing       -                                   -            0.5
```

The dependency appears on the first row for a dependency; additional candidates leave it blank. Each semver line of a crate is a separate dependency, so a package that requires both `librust-rand-0.8-dev` and `librust-rand-0.9-dev` has two `rand` rows, distinguished by their requirements. The requirement is repeated because its colors describe each candidate independently.

The displayed requirement comes entirely from the selected Debian control files. A leading version such as `1` denotes the package-name suffix in `librust-serde-1-dev`. `*` denotes an unsuffixed package name; matching still requires that actual name or a corresponding `Provides`. Explicit bounds follow in parentheses, with the Debian version string retained verbatim.

Identical expressions are factored out across features, for example:

```text
1 (>=1.0.100) +derive +std
```

A feature with different requirements displays its full expression explicitly:

```text
1 (>=1.0.100) +derive(1 (>=1.0.200)) +std
```

Commas join required constraints (AND). Compatibility always uses the original Debian relations, including dependencies absent from Cargo metadata.

Default features are implicit in the report. A negative entry `-default` is used to indicate the absence of a dependency on the crate's `+default-dev` package. Other features remain explicit. The leading version expression combines base and default requirements when present, otherwise uses the base requirements. When neither group exists, it uses every feature's requirements with the feature removed from the package name, because each debcargo feature package depends on the base package from the same source. Other features show their own version expression only when it differs from the leading expression. Each component is colored using its corresponding Debian relations.

Statuses have the following meanings:

- `selected`: the first compatible candidate in descending version order;
- `available`: another version or origin also satisfies the dependency;
- `incompatible`: packages for the crate exist, but none satisfy the required Debian version constraints and features; and
- `missing`: no package for the crate exists in the selected sources.

When standard output is a terminal, statuses are colored green for `selected`, gray for `available`, yellow for `incompatible`, and red for `missing`. The version expression and each `+feature` in `REQUIREMENT` are colored independently: yellow when a corresponding package exists but is incompatible, and red when it is missing. A hidden default-feature requirement is reflected in the base version color. `-default` has the same color as the base version because it is contextual information rather than a requirement that candidates must satisfy. Satisfied components remain neutral on every row, reserving green for the `selected` status. Set `NO_COLOR` to disable colors; redirected output is always plain text.

When a dependency requires multiple feature packages, all of them must resolve from one candidate for the dependency to be selected or available. After architecture and build-profile filtering, `|` alternatives involving Rust libraries are rejected (debcargo does not generate such forms), while wholly non-Rust groups are ignored.

Ubuntu Archive locations are shown as `suite/component`, while PPA locations appear as `ppa:OWNER/NAME (series)` and omit the component, which is always `main`.

Ubucargo classifies candidates from the APT sources constructed from the command arguments. The result predicts a build configured with the same series and PPAs. Standard tooling (`sbuild` and `autopkgtest`) remains the authoritative way to determine whether a build is successful, but `ubucargo deps` aims to simulate the dependency resolution part of this as accurately as possible.

In all modes, candidates are ordered deterministically from the local APT indexes.

## APT metadata

`deps` uses only binary `Packages` indexes. They contain the package versions and versioned `Provides` needed to match Debian Rust feature packages; Archive `Sources` indexes are not downloaded.

Every invocation asks APT to update the selected indexes. APT reuses unchanged files and may apply index deltas. Indexes for all previously requested series and PPAs share the cache described in [`apt-cache.md`](apt-cache.md).

## Exit status

`deps` exits 0 when every dependency is satisfiable, 1 when at least one dependency in any table is incompatible or missing, and 2 on command, staging, network, or metadata errors.
