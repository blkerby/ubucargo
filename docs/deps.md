# `ubucargo deps`

The command reports the direct Rust library dependencies, represented by `librust-*-dev` package relations that the package needs to build, install every binary package, and run its autopkgtests.

## Synopsis

```console
ubucargo deps [INPUT [VERSION]] [--series SERIES] \
  [--proposed] [--ppa ppa:OWNER/NAME]... [--architecture ARCH] [--keep-staging]
```

See [input selectors](inputs.md) for the shared explicit grammar, automatic precedence, and version rules. Omitting the input reads the nearest parent package. Existing-package inputs require `debian/debcargo.toml` only as a marker; inspection requires a valid top `debian/changelog` entry for the source identity and version, but does not read Cargo metadata, debcargo configuration, or `crate_src_path`. Local Cargo crates require explicit `local:PATH`; unprefixed directory paths select only existing source packages.

Crates.io and `local:` inputs generate fresh packaging with default debcargo configuration in temporary directories. Local packages and published inputs read maintained `debian/control` and optional `debian/tests/control` directly. Published inspection does not require a debcargo configuration. Inspection never modifies an existing package.

A published input supplies the default checking series: its base Archive series or explicit PPA series. `resolute-proposed/rust-serde --series jammy` reads Resolute proposed packaging and checks its dependencies against Jammy. Otherwise, checking defaults to the current Ubuntu development series reported by `ubuntu-distro-info --devel`. Explicit `--series` takes precedence over the published input's base series. Development-series detection runs only when both are absent; if it fails, supply `--series` explicitly. A PPA input automatically adds its PPA to dependency repositories, queried for the checking series. Repeated PPA arguments are deduplicated.

Source selection is independent of `--series` and `--proposed`. A bare Archive series considers release, updates, and security in `main` and `universe`. A suffixed suite (for example, `resolute-proposed`) selects only that pocket. PPA selectors require their own series. Additional PPAs cannot replace an Archive input's source. An explicit Debian source version must exist in the input's source indexes. Otherwise the highest Debian version wins, with deterministic location ordering for ties.

```console
ubucargo deps --series resolute
ubucargo deps serde
ubucargo deps serde --series resolute
ubucargo deps serde 1.0.220 --series resolute
ubucargo deps archive:resolute/rust-serde
ubucargo deps resolute/rust-serde --series jammy
ubucargo deps ppa:myuser/rust-staging/resolute/rust-serde --series resolute
ubucargo deps pkg:./rust-serde --series resolute
ubucargo deps local:../serde --series resolute
```

`--keep-staging` retains and prints generated or published-input staging, including on failure. Existing local package inspection creates no staging directory.

`--series` selects the dependency environment: release, updates, and security pockets in `main` and `universe`. `--proposed` adds proposed. Each public `--ppa ppa:OWNER/NAME` adds `main`; private PPAs are unsupported. `--architecture` defaults to `dpkg --print-architecture`.

## Output

A single line identifies the resolved input name, version, and location:

```text
Input: rust-rand 0.8.5-1 from resolute/universe
Input: rust-rand 0.8.5-1 from ppa:owner/staging (resolute)
Input: rand 0.8.5 from crates.io (generated packaging)
Input: rand 0.8.5 from local:../rand (generated packaging)
Input: rust-rand 0.8.5-1 from pkg:./rust-rand
```

Local-package identity and version come from the top changelog entry; a missing or malformed changelog is an error. Unusable Cargo metadata or debcargo configuration does not prevent inspection. Cross-series checks show the actual input location. The header is present even when all dependency tables are empty. Advisory availability lines follow the input header, for example:

```text
crates.io availability: 0.9.2
resolute availability: rust-rand 0.8.5-1
```

Crates.io information comes from its metadata API, selecting the highest non-yanked stable version without downloading a crate archive. A missing crate displays `not published`, a crate with no eligible stable version displays `no stable release`, and request or metadata failures display `unavailable`. Requests have an eight-second overall timeout. These outcomes do not change dependency results or exit status.

The Archive entry describes the checking series and its selected pockets/components, excludes PPAs, and shows the highest full Debian version for each matching source package, retaining parallel semver lines grouped under a single series label. Matching uses conventional Rust source names and declared Rust library binaries; absence displays `not packaged`. Local and published inputs infer the crate name from control-file Rust binaries or conventional source names without requiring usable Cargo metadata.

An entry is omitted when that origin already supplied an implicitly selected latest input. An unversioned crates.io input reuses its resolved version and avoids another API request. An unversioned Archive input omits its matching Archive entry only when the checking series and version match; parallel source packages remain visible. Explicit versions, local inputs, and cross-series checks retain their relevant latest entries.

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
serde       selected      ppa:example/rust-staging (resolute)    1.0.219-1    1 +derive
            available     resolute-updates/universe              1.0.217-1    1 +derive
syn         incompatible  resolute/universe                      1.0.109-2    2 -default
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

`deps` downloads binary `Packages` and source `Sources` indexes through its isolated, signature-verified APT view. Packages supply versions and versioned `Provides` for dependency classification; Sources supply publication selection and authenticated `.dsc` checksums.

Published inputs are retrieved at the indexed exact version with `pull-lp-source` or `pull-ppa-source`. Ubucargo verifies descriptor size and SHA512 (preferred) or SHA256 against the signed source index, retains downloaded-file checksum verification, and extracts with `dpkg-source`. Input and checking APT views are queried sequentially; input-series binaries cannot enter dependency classification.

Every invocation asks APT to update the selected indexes. APT reuses unchanged files and may apply index deltas. Indexes for all previously requested series and PPAs share the cache described in [`apt-cache.md`](apt-cache.md).

## Exit status

`deps` exits 0 when every dependency is satisfiable, 1 when at least one dependency in any table is incompatible or missing, and 2 on command, staging, network, or metadata errors.
