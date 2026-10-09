# `ubucargo deps`

The command reports the direct Rust library dependencies, represented by `librust-*-dev` package relations that the package needs to build, install every binary package, and run its autopkgtests.

## Synopsis

```console
ubucargo deps [<INPUT> [<VERSION>]] [--suite <SUITE>] \
  [--proposed] [--ppa ppa:<OWNER>/<NAME>]... [--architecture <ARCH>] [--keep-staging]
```

Inputs can be crates.io crates (`serde` or `crate:serde`), local source packages (`./rust-serde` or `pkg:<PATH>`), local Cargo crates (`local:<PATH>`), Ubuntu Archive sources (`ubuntu:<SUITE>/<SOURCE>` or `<SUITE>/<SOURCE>`), Debian archive sources (`debian:<SUITE>/<SOURCE>`), or PPA sources (`ppa:<OWNER>/<NAME>/<SERIES>/<SOURCE>`). Omitting the input reads the nearest parent package containing `debian/control` and `debian/changelog`. An optional positional `<VERSION>` selects an exact Cargo version for crates.io or Debian version for published inputs; these inputs default to the highest published version. Package and local Cargo inputs use their current version. See [input and version selection](package.md#input-and-version-selection) for details.

Crates.io and `local:` inputs generate fresh packaging with default debcargo configuration in temporary directories. Local package and published inputs read maintained `debian/control` and optional `debian/tests/control` directly. For these inputs, inspection needs the maintained control files and changelog. Existing package files remain unchanged during inspection.

`--suite` selects the checking environment as `ubuntu:<SUITE>` or `debian:<SUITE>`. A bare suite name is Ubuntu shorthand, so `--suite resolute` and `--suite ubuntu:resolute` select the same environment. An explicit option takes precedence over the input's default.

A Debian input defaults to its exact Debian suite: `debian:unstable/rust-serde` checks against `debian:unstable`. An Ubuntu Archive input defaults to its base Ubuntu series, and a PPA input defaults to its Ubuntu series. Crates.io and local inputs default to the current Ubuntu development series reported by `ubuntu-distro-info --devel`. Development-series detection runs only when both an explicit suite and a published-input default are absent; if it fails, supply `--suite` explicitly.

For Ubuntu checking suites, a PPA input automatically adds its PPA to the checking repositories, using the checking suite's base series. Repeated PPA arguments are deduplicated. A PPA input checked against Debian supplies the maintained packaging, while the checking repositories come from Debian.

The input selector determines the source location; `--suite`, `--proposed`, and additional `--ppa` arguments select the repositories used to check dependencies. For example, `ubuntu:resolute-proposed/rust-serde --suite jammy` selects packaging from Resolute proposed and checks it against Jammy's release, updates, and security pockets.

```console
ubucargo deps --suite resolute
ubucargo deps serde
ubucargo deps serde --suite resolute
ubucargo deps serde 1.0.220 --suite resolute
ubucargo deps ubuntu:resolute/rust-serde
ubucargo deps debian:unstable/rust-serde
ubucargo deps debian:unstable/rust-serde --suite resolute
ubucargo deps ubuntu:resolute/rust-serde --suite debian:unstable
ubucargo deps serde --suite debian:unstable
ubucargo deps resolute/rust-serde --suite jammy
ubucargo deps ppa:myuser/rust-staging/resolute/rust-serde --suite resolute
ubucargo deps pkg:./rust-serde --suite resolute
ubucargo deps local:../serde --suite resolute
```

`--keep-staging` retains and prints staging directories for generated packaging or extracted published sources, including on failure. Existing local package inspection creates no staging directory.

A base Ubuntu checking suite selects release, updates, and security pockets in `main` and `universe`; a suite with a pocket suffix selects that pocket alone. `--proposed` adds proposed to a base Ubuntu suite. Each public `--ppa ppa:<OWNER>/<NAME>` adds `main` for the checking suite's base series.

A Debian checking suite selects exactly the named suite in `main` from `https://deb.debian.org/debian`, using `debian-archive-keyring`. `--proposed` and explicit `--ppa` options require an Ubuntu checking suite.

`--architecture` selects the Debian architecture used for dependency checking. It defaults to the host's native architecture, as reported by `dpkg --print-architecture`.

## Output

A single line identifies the resolved input name, version, and location:

```text
Input: rust-rand 0.8.5-1 from resolute/universe
Input: rust-rand 0.8.5-1 from debian:unstable/main
Input: rust-rand 0.8.5-1 from ppa:owner/staging (resolute)
Input: rand 0.8.5 from crates.io (generated packaging)
Input: rand 0.8.5 from local:../rand (generated packaging)
Input: rust-rand 0.8.5-1 from pkg:./rust-rand
```

Local-package identity and version come from the top changelog entry; a missing or malformed changelog is an error. Unusable Cargo metadata or debcargo configuration does not prevent inspection. Checks across distributions or suites show the actual input location. The header is present even when all dependency tables are empty. Advisory availability lines follow the input header, for example:

```text
crates.io availability: 0.9.2
ubuntu:resolute availability: rust-rand 0.8.5-1
```

For a Debian checking suite, the archive line uses its qualified selector, for example `debian:unstable availability: rust-rand 0.8.5-1`.

Crates.io information comes from its metadata API, selecting the highest non-yanked stable version without downloading a crate archive. A missing crate displays `not published`, a crate with no eligible stable version displays `no stable release`, and request or metadata failures display `unavailable`. Requests have an eight-second overall timeout. These outcomes do not change dependency results or exit status.

The archive entry describes the qualified checking suite and its selected pockets/components, excludes PPAs, and shows the highest full Debian version for each matching source package, retaining parallel semver lines grouped under a single suite label. Matching uses conventional Rust source names and declared Rust library binaries; absence displays `not packaged`. Local package and published inputs infer the crate name from control-file Rust binaries or conventional source names without requiring usable Cargo metadata.

An unversioned crates.io input reuses its resolved version and avoids another API request. An unversioned archive input omits the archive availability line when its source is the only matching package and the distribution, exact suite, source name, and version match. When parallel source packages exist, the line includes every matching source, including the selected input. Explicit versions, local inputs, and checks across distributions or suites retain their relevant latest entries.

The report contains one table for each of these, in order, omitting empty tables:

- `Package: <NAME>`: the `Depends` of each binary package, in control-file order;
- `Source: <NAME>`: the source paragraph's `Build-Depends`, `Build-Depends-Arch`, and `Build-Depends-Indep`; and
- `Tests: <NAME>`: the `Depends` of autopkgtests applicable to the selected architecture, which contain the crate's development dependencies.

The `Source` and `Tests` tables show only relations that no earlier table lists identically. For a library crate, the default build's relations already appear in its binary packages, so the `Source` table usually lists only binaries' dependencies and manual `build_depends` overrides. The report covers external Rust package relations with concrete package names and version constraints. Test `Architecture` fields filter whole test paragraphs before their dependencies are read, using Debian architecture names, wildcards, and exclusions. An omitted field includes the test on every architecture.

When features are collapsed, as debcargo does by default, one binary package lists the dependencies of all features. Otherwise, each feature package lists the dependencies that its feature adds directly; dependencies of features it enables appear in those features' tables.

The report shows each dependency and its candidates:

```text
Package: librust-example-dev
DEPENDENCY  STATUS        LOCATION                            VERSION      REQUIREMENT
serde       preferred     ppa:example/rust-staging (resolute)    1.0.219-1    1 +derive
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

- `preferred`: the first compatible candidate in descending version order;
- `available`: another version or origin also satisfies the dependency;
- `incompatible`: packages for the crate exist, but none satisfy the required Debian version constraints and features;
- `missing`: no package for the crate exists in the selected sources; and
- `unknown`: requires manual assessment; see [dependency alternatives](#dependency-alternatives).

When standard output is a terminal, statuses are colored green for `preferred`, dark gray for `available` and `unknown`, yellow for `incompatible`, and red for `missing`. The version expression and each `+feature` in `REQUIREMENT` are colored independently: yellow when a corresponding package exists but is incompatible, and red when it is missing. A hidden default-feature requirement is reflected in the base version color. `-default` has the same color as the base version because it is contextual information rather than a requirement that candidates must satisfy. Satisfied components remain neutral on every row, reserving green for the `preferred` status. Set `NO_COLOR` to disable colors; redirected output is always plain text.

When a dependency requires multiple feature packages, one candidate must provide all of them for the dependency to be preferred or available.

Ubuntu Archive locations are shown as `<SUITE>/<COMPONENT>`, Debian archive locations as `debian:<SUITE>/<COMPONENT>`, and PPA locations appear as `ppa:<OWNER>/<NAME> (<SERIES>)` and omit the component, which is always `main`.

The report checks direct Rust dependency candidate availability in the APT sources constructed from the command arguments. For supported Rust relations, each dependency is checked independently against its required package names, Debian version constraints, and features. `preferred` identifies the highest-version compatible candidate for that dependency. Use `sbuild` and `autopkgtest` to validate complete build and test environments, including transitive dependencies and joint installability.

In all modes, candidates are ordered deterministically from the local APT indexes.

## APT metadata

`deps` downloads binary `Packages` and source `Sources` indexes through its isolated, signature-verified APT view. Packages supply versions and versioned `Provides` for dependency classification; Sources supply publication selection and authenticated `.dsc` checksums.

Published inputs are retrieved with `dget` using the descriptor URL from the selected repository and its signed Sources index. Ubucargo verifies descriptor size and SHA512 (preferred) or SHA256 against the signed source index, retains downloaded-file checksum verification, and extracts with `dpkg-source`. Input and checking APT views are queried sequentially; dependency classification uses only the checking view’s binary records.

Every invocation asks APT to update the selected indexes. APT reuses unchanged files and may apply index deltas. Indexes for all previously requested distributions, suites, and PPAs share the cache described in [`apt-cache.md`](apt-cache.md).

## Exit status

`deps` exits 0 when every reported direct Rust dependency has a compatible candidate, 1 when at least one dependency in any table is incompatible, missing, or unknown, and 2 on command, staging, network, or metadata errors. The report completes before returning status 1.

## Dependency alternatives

For each comma-separated dependency group, `deps` first filters its alternatives by architecture and build profile. A group contributes to the Rust report when at least one applicable alternative names a `librust-*-dev` package. A group with a single applicable Rust alternative is checked and displayed normally.

A group with multiple applicable alternatives and at least one Rust library appears as a single `(complex dependency)` row with status `unknown`. This applies equally to alternatives between versions or features of the same crate, between different crates, and between Rust and non-Rust packages. `unknown` marks the group for manual assessment, regardless of candidate availability in the selected repositories. `debcargo` does not normally generate requirements of this form, but they could arise from manual dependency overrides in `debcargo.toml` or edits to the generated control files.

The row shows `-` for location and version and the full declared alternative expression in `REQUIREMENT`, including any architecture and build-profile restrictions, colored dark gray on a terminal. Complex rows follow checked dependencies in their table and use the same rules for suppressing repeated relations across tables. Other dependencies are checked normally.

```text
DEPENDENCY            STATUS   LOCATION  VERSION  REQUIREMENT
(complex dependency)  unknown  -         -        librust-foo-1-dev | librust-foo-2-dev
(complex dependency)  unknown  -         -        librust-foo-dev | librust-bar-dev
(complex dependency)  unknown  -         -        librust-foo-dev | libfoo-dev
```
