# Ubucargo design notes

## High-level overview

Ubucargo adapts Debian's Rust packaging model for Ubuntu. It should remain
compatible with Debian packaging policy while using an Ubuntu-specific process
for generating and maintaining packages. Its basic role in Ubuntu is intended
to be similar to the role of `debcargo` in Debian. `ubucargo` will consume
a `debcargo.toml` config and a `Cargo.toml` manifest and use it to generate
an Ubuntu source package, including a `control` file which translates Cargo
dependencies into Ubuntu package dependencies.

The design of `ubucargo` deviates from `debcargo` in three main ways:
- `ubucargo` treats the source package itself as the authoritative location for
package generator configuration. It has no analogue to Debian's `debcargo-conf`
monorepo. `ubucargo` reads the file `debian/debcargo.toml` within
the source package rather than from an external configuration.
- `ubucargo` is oriented around a concept of a local "workspace" which allows for
convenient, coordinated work on multiple packages: a workspace defines an Archive
base configuration, including a selected series, pockets, and components, with
PPAs and local source trees optionally layered on top. `ubucargo` commands
consistently respect this layered workspace view when resolving
dependencies.
- `ubucargo` aims to be compatible with git-based packaging tooling,
such as `gbp`. When generated files are overridden by maintainers and
subsequently regenerated, it can be configured to produce a three-way merge
based on the stored `.hint` file.

Each source package contains its complete packaging state: debcargo generator
input, generated files, and maintainer-owned files or overrides, for example:

```text
<source-package>/
  Cargo.toml
  src/
  debian/
    debcargo.toml               # generator input
    control                     # generated or overridden
    control.debcargo.hint       # previous generated version
    rules                       # generated or overridden
    rules.debcargo.hint         # previous generated version
    tests/control               # generated or overridden
    tests/control.debcargo.hint # previous generated version
    patches/                    # maintainer-owned
    changelog                   # maintainer-owned
    copyright                   # generated or overridden
    copyright.debcargo.hint     # previous generated version
```

## Command-line interface

The core commands are:

```console
ubucargo init [DIR] --series SERIES --pockets POCKET,... \
  --components COMPONENT,... [--ppa ppa:OWNER/NAME]... \
  [--rust-version VERSION]
ubucargo download SOURCE \
  [--from ubuntu:SUITE|debian:SUITE|ppa:OWNER/NAME] \
  [--version VERSION]
ubucargo import CRATE [--version VERSION]
ubucargo package [PACKAGE] [--check]
ubucargo upgrade [PACKAGE] [--version VERSION]
ubucargo deps [PACKAGE]
ubucargo build [PACKAGE]
```

Except for `init`, commands run inside a workspace. Ubucargo should find the
workspace by walking up to the nearest `ubucargo.toml`. `PACKAGE` is a source
package directory; it may be omitted when the current directory is inside that
source package.

### Workspace layout

Each source package is an immediate child of the workspace root and is named
after its Debian/Ubuntu source package:

```text
rust-transition/
  ubucargo.toml
  rust-serde/
    Cargo.toml
    src/
    debian/
      debcargo.toml
      control
      ...
  rust-syn/
    Cargo.toml
    src/
    debian/
      ...
  rust-syn-1/
    Cargo.toml
    src/
    debian/
      ...
```

An upgrade such as `syn` 2.0.106 to 2.0.107 updates `rust-syn/` in place. A
parallel upstream line uses another directory only when it has a distinct
source package name, such as `rust-syn-1`. A workspace contains at most one
checkout of each source-package identity; comparing two revisions of the same
source package should use separate ubucargo workspaces.

Ubucargo discovers immediate child source-package directories and
validate their Debian source-package name from package metadata. A
directory whose name does not match the package name is invalid. Packages that
ubucargo downloads or imports use this naming convention; any source trees
placed in the workspace by other means must use it as well.

### Initialize a workspace

For example:

```console
ubucargo init rust-transition \
  --series noble \
  --pockets release,updates,security \
  --components main,universe \
  --ppa ppa:example/rust-staging
```

This creates `rust-transition/ubucargo.toml`:

```toml
series = "noble"
pockets = ["release", "updates", "security"]
components = ["main", "universe"]
ppas = ["ppa:example/rust-staging"]
```

The PPA list is ordered because repository order is part of candidate
selection when priorities and versions are otherwise equal. `init` should
record the requested configuration and refuse to overwrite an existing
workspace.

The workspace may explicitly set a Rust compatibility target:

```toml
rust-version = "1.75"
```

When omitted, `ubucargo` should derive the effective Rust version from the
APT-selected binary package named `rustc` in the workspace view. It
should extract the upstream Rust version from that package's Debian version.
An explicit value may be lower than the available compiler when a workspace
needs to remain compatible with an older toolchain.

### Download source packages

Download the source-package candidate selected by the configured Archive view:

```console
ubucargo download rust-syn
ubucargo download rust-syn --version 2.0.106-1
```

Restrict selection to a specific Ubuntu or Debian suite or configured PPA
with:

```console
ubucargo download rust-syn --from ubuntu:noble
ubucargo download rust-syn --from ubuntu:noble-updates
ubucargo download rust-syn --from debian:sid
ubucargo download rust-syn --from ppa:example/rust-staging
```

`download` downloads the selected `.dsc` and its associated source files, then
unpacks them with standard Debian source-package tools into a directory named
after the source package. If specified, `--from` restricts the candidate set
to one Ubuntu suite, Debian suite, or PPA, while `--version` requires an exact
Debian/Ubuntu source version. When both are present, both constraints apply.

Candidate selection must use the same repository priorities, Debian
version ordering, and repository order as the build environment. If an exact
version exists in multiple origins, repository order breaks the tie unless
`--from` identifies one explicitly. Before downloading, ubucargo should print
the selected version and origin, for example:

```text
Downloading rust-syn 2.0.107-1 from ubuntu:noble-proposed/universe
```

Official archive origins are qualified as `ubuntu:SUITE` or `debian:SUITE`.
For Ubuntu, the release pocket is the series name itself, such as
`ubuntu:noble`, while other pockets use names such as `ubuntu:noble-updates`
or `ubuntu:noble-security`.

Downloading preserves the complete existing `debian/` directory and does not
regenerate it automatically. For each known generated file without a companion
`.debcargo.hint`, `download` should copy the downloaded file to that hint path.
This records the downloaded state before local edits. The command should refuse
to overwrite an existing source-package directory. The initial implementation
should not create or modify a version-control repository.

### Import crates

Import one crate from crates.io into a new source-package tree:

```console
ubucargo import serde
ubucargo import serde --version 1.0.219
```

Without `--version`, ubucargo should select a non-yanked release using
the same MSRV policy as `cargo add`: A release whose published `rust_version`
is newer than the workspace Rust version is incompatible and should be
skipped, while a release without `rust_version` is treated as compatible, but
ubucargo should warn that its compatibility is unverified. The newest
remaining release is resolved immediately to an exact version.

When an exact version is requested, a known MSRV incompatibility should be an
error; an undeclared MSRV should remain a warning. The source package is created
in the workspace root under its Debian source package name, such as
`rust-serde/`. Existing Debian naming, feature, and versioning conventions
determine that name and the eventual binary package names.

For example:

```text
Workspace rustc: 1.75
Ignoring foo 4.2.0: requires Rust 1.81
Importing foo 4.1.0
warning: foo 4.1.0 does not declare rust-version; compatibility is unverified
```

`import` should download and verify the crate, unpack its upstream source, and
create a fresh `debian/debcargo.toml`. It should leave generation of the rest
of `debian/` to `package`, refuse to overwrite an existing source-package
directory, and suggest `upgrade` when that package already exists.

### Package source trees

Generate packaging for an existing source tree with:

```console
ubucargo package ./rust-serde
```

From inside the source package, the shorter form is:

```console
ubucargo package
```

`package` should read the effective patched source and
`debian/debcargo.toml`, generate ubucargo-owned files in a staging area, and
reconcile them with the source tree. The same operation can be used to initially
populate `debian/` after a crates.io import, or to refresh generated files in a
downloaded or previously packaged source tree.

Following debcargo, generation produces candidates for the following paths:

- `debian/cargo-checksum.json`
- `debian/control`
- `debian/copyright`
- `debian/rules`
- `debian/source/format`
- `debian/watch`
- `debian/tests/control`, for library packages
- `debian/<feature-package>.lintian-overrides`, for each generated non-base
  feature package

For every generated file `<file>`, ubucargo also stores
`<file>.debcargo.hint`, even when both files have identical contents. The hint
is the last generated version and can be used as a merge base for later changes.
`debian/changelog` is create-only: `package` may create an initial entry when
it is absent, but never replaces or removes an existing changelog.
`debian/debcargo.toml`, `debian/patches/`, and all other paths are not
generator-owned.

```console
ubucargo package ./rust-serde --check
```

`--check` should report whether packaging would change without writing it.

### Upgrade source packages

Upgrade the upstream crate release in an existing source package with:

```console
ubucargo upgrade ./rust-syn --version 2.0.107
```

From inside the source package, the shorter form is:

```console
ubucargo upgrade --version 2.0.107
```

Without `--version`, ubucargo should select the newest non-yanked stable
release that can retain the existing Debian source-package identity. An
upgrade downloads and verifies the crate, constructs the replacement upstream
tree in a staging area, preserves `debian/`, applies the existing Debian patch
series, and runs the same generated-file reconciliation as `package`. It should
replace the working tree only after those steps succeed. Manual files such as
changelog entries and patches remain for maintainer review, and patch failures
must be reported rather than rewritten automatically.

The initial implementation operates only on the source-tree contents. It does
not import upstream history, create commits, switch branches, or otherwise
update a version-control repository.

`upgrade` should apply the same workspace Rust-version selection and validation
rules as `import`.

If the requested release requires a different Debian source-package identity,
`upgrade` should fail rather than rename the directory or silently create a new
package. The maintainer can then use `import` for the new source package and
handle the transition explicitly.

### Inspect dependencies

Show direct Rust library dependencies with:

```console
ubucargo deps ./rust-my-crate
```

The output should show every relevant candidate for each direct dependency,
not only the one the resolver selects. For example:

```text
DEPENDENCY  REQUIREMENT  STATUS        ORIGIN                    VERSION      LOCATION
serde       ^1 +derive   selected      workspace                 1.0.219-1    rust-serde/
                         available     ppa:example/rust-staging  1.0.218-2    noble/main
                         available     Ubuntu Archive            1.0.217-1    noble-updates/universe
syn         ^2           incompatible  Ubuntu Archive            1.0.109-2    noble/universe
foo         ^3           missing       -                         -            -
```

The status should distinguish the selected candidate, other usable candidates,
present but incompatible candidates, and missing dependencies. Archive
locations include the pocket and component. PPA locations include the PPA
identity and its series and component; PPAs should not be presented as Ubuntu
Archive pockets. The selected candidate must be the same one `build` will use.

### Build packages

Build a source package with:

```console
ubucargo build ./rust-my-crate
```

`build` should build only the requested source package. It should expose
already-built binary packages from the workspace and the configured PPAs to
`sbuild`, but it should not automatically build other workspace source trees.
If a required package is unavailable, it should report the missing dependency
and whether a suitable source is available from the Archive, a configured PPA,
or crates.io.

The result remains ordinary Ubuntu source and binary packages.

## Relationship with debcargo

Ubucargo should treat debcargo as the reference implementation for compatible
Debian behavior, not as a library dependency or fork. Package names, feature
layout, dependency translation, registry conventions, and existing
`debcargo.toml` keys should remain compatible unless Ubuntu has a concrete
reason to diverge. Representative debcargo output can serve as compatibility
fixtures.

`debian/debcargo.toml` is the single generator configuration file. Ubuntu-only
settings belong under a validated `[ubucargo]` namespace; existing debcargo
keys must not be reinterpreted. Unsupported settings that affect generation
should fail clearly.

Downloaded source packages should already contain `debcargo.toml` and any
`*.debcargo.hint` files. If configuration is absent, recovering it from
debcargo-conf history or recreating it remains a manual packaging task. On
the first `package` run after download, ubucargo should compare its generated
output with the existing packaging so version-related changes are visible for
review.

## Stable generation boundaries

The normal input is an existing conventional Debian source tree. Ubucargo
should create a staging copy, apply its Debian patch series using standard
source-package tooling, and invoke `cargo metadata --format-version=1` on the
effective patched source. It should deserialize only the JSON fields it uses,
ignore unknown additive fields, and reject unsupported semantic cases. When a
workspace contains multiple applicable packages, selection must be explicit.

Source acquisition is separate from packaging generation. `download` retrieves
an already-packaged source from the Archive or a configured PPA. `import` and
`upgrade` download crates through the Cargo registry protocol and must verify
their checksums. `package` operates only on the existing source tree and must
not require network access.

Rust-version filtering during `import` and `upgrade` rejects only known
incompatibilities. A missing upstream `rust_version` is not evidence of
compatibility, and the filter does not prove that the selected features,
dependencies, build scripts, or Debian patches work with the target compiler.
`build` against the configured Archive toolchain remains the authoritative
compatibility check.

Generation should produce an in-memory set of relative paths and contents. A
separate reconciliation step should compare that set with `debian/`, enforce
ownership, and write accepted changes atomically. Generator code should not
choose the workspace location or write directly to it.

## Version-control integration

Version-control integration is outside the initial implementation. Ubucargo
should not require Git or invoke git-ubuntu, git-buildpackage, git-debrebase, or
other repository-management tools. Its contract is the source-package
filesystem: a package must be materialized in a form that standard Debian tools
can patch and build.

This keeps the core commands independent of repository layout:

```text
materialized Debian source tree
  -> ubucargo package, deps, or upgrade

exportable Debian source tree
  -> standard Debian tools produce a source package
  -> ubucargo build
```

Maintainers may place checkouts managed by existing tools in a workspace, but
they are responsible for presenting an exportable source tree before running
ubucargo. In the initial implementation, exportable means that standard tools
such as `dpkg-source` can produce a valid source package without
repository-specific preparation. States such as an unexported patch queue are
not part of that contract.

A future integration layer may adapt repository-specific operations behind a
small boundary:

```text
initialize repository history from a downloaded .dsc
import a new upstream tarball into repository history
export repository state as a buildable source package
```

Potential adapters include git-buildpackage, git-debrebase, and git-ubuntu.
The choice belongs to each package checkout rather than the workspace or
`debcargo.toml`, since different maintainers may use different history models
for the same source package.

## In-tree reconciliation

Debcargo's overlay behavior cannot be reused literally: it copies an overlay
into an empty directory and treats existing paths as manual overrides. In a
downloaded source tree, that would classify every generated file as manually
overridden.

Ubucargo should instead:

1. Read `debcargo.toml`, including `[ubucargo]` settings.
2. Treat `overlay = "."` as the existing `debian/` directory without copying
   it recursively.
3. Generate candidate files in a staging area.
4. Reconcile them with the working tree, creating a merged version if applicable.
5. Apply changes atomically.

For each generated path, reconciliation has three inputs:

- `base`: the old `<file>.debcargo.hint`
- `old`: the working-tree `<file>`
- `new`: the newly generated staging file

`ubucargo` ensures that all hint files exist after a package is first downloaded
or generated, regardless of whether the file is overridden or not. It records
generator state rather than indicating that the primary file is necessarily
a manual override. This deviation from `debcargo` behavior is necessary in order
to reliably distinguish an overridden file from a previously generated one,
given that overrides are no longer specified in a separate external directory.

| State | Package behavior |
| ----- | ---------------- |
| `old` absent | Write `new` to both `old` and `base` |
| `old == base` | Replace both with `new` |
| `old != base`, merging disabled | Preserve `old`; replace `base` with `new` |
| `old != base`, merging enabled | Three-way merge `base`, `old`, and `new`; replace `base` with `new` |

If `base` is absent, it should be treated as though it were an empty file: this
will result in a merge conflict in case an `old` file existed and differs from `new`.
This situation should not normally arise because `base` should be initialized
when the source package is initially downloaded or created.

Three-way merging is enabled by default but may be disabled by

```toml
[ubucargo]
merge = "manual"
```

A clean merge writes the merged result to `<file>` and the pre-merged newly
generated version to `<file>.debcargo.hint`. In case of a conflict, conflict
markers are included in the merged result and a warning is output. The merge
should be compatible with the standard `diff3` base/local/new behavior and
must not require Git.

Files that must never be replaced or merged can be declared explicitly:

```toml
[ubucargo.files]
"rules" = "manual"
"tests/control" = "manual"
```

Their primary files remain unchanged while their hints are refreshed.

Changelog and patches are always manual. Files such as control, rules, tests,
watch, and copyright may be generated or edited in place. Unknown files must
never be deleted merely because the generator did not emit them.

## Archive index and resolver

Packaging an existing source tree does not require an Archive index.
Archive-aware operations such as `download`, `deps`, and `build` need one to
select source versions, determine dependency and feature-provider availability,
enforce component and architecture constraints, identify missing packages, and
plan test runs. `import` and `upgrade` also need the index when the workspace
omits `rust-version`, so they can resolve the selected `rustc` binary version.

The build index should represent an explicit Ubuntu series and pocket view,
including staging PPAs where requested. It needs source and binary versions,
component and architecture availability, dependency and `Provides` data,
enough binary-package metadata to resolve the selected `rustc`, Cargo identity
from `X-Cargo-*` fields, and relevant test metadata. Release, updates, security,
proposed, backports, and staging sources must remain distinguishable.

The resolver must use the same candidate-selection policy that `build` gives
to APT: configured priorities and pinning first, Debian version ordering next,
and repository order when otherwise equal. `download`, `deps`, and `build`
must share this resolver so they cannot disagree about the selected version or
origin. Archive-derived Rust-version checks must use the same selected `rustc`
candidate.

Signed Archive metadata should be the authority for current availability. The
first archive-aware command should load a cached catalog for the configured
view or construct one from that metadata. A stale catalog should be refreshed
before use. The indexer can download source packages on demand for details such
as `Cargo.toml`, patches, and `debcargo.toml`; database design should wait until
the access pattern requires it.

An explicit `download --from debian:SUITE` may load signed source metadata for
that Debian suite, but those candidates remain outside the configured build
index and resolver view.

The resolver view combines:

```text
signed Ubuntu Archive sources
+ configured PPA sources
+ already-built workspace packages
= configured APT resolver view
```

These sources are not a simple last-wins overlay; their candidates remain
distinguishable and are selected using the policy above.

Launchpad publication history may supplement deleted or superseded versions
but must not override the current signed Archive view.

## Local builds

Ubucargo should not create a private Cargo registry or a separate build system.
A normal build should use Ubuntu's standard package tools:

```text
source tree with generated debian/ files
  -> standard Debian tools produce a source package
  -> sbuild installs Build-Depends
  -> librust-*-dev packages populate /usr/share/cargo/registry
  -> dh-cargo builds and tests the crate
```

`ubucargo build` should invoke `sbuild` without changing package dependency
semantics.

The initial implementation builds one requested source package at a time. It
may expose already-built workspace packages through a temporary apt repository
or sbuild extra packages, but it does not discover or build a dependency
closure. Maintainers choose which source packages to download, import, package,
and build.

The result is ordinary Ubuntu source and binary packages, not a synthetic
Cargo registry. Feature policy must come from package configuration rather
than being silently rewritten during the build.

## Ubuntu-specific concerns

For each Ubuntu series, ubucargo must account for:

- Network-isolated builds and policy-compliant source.
- Available Rust, Cargo, `dh-cargo`, and debhelper versions, including crate
  minimum Rust versions.
- Pocket, component, migration, and Main Inclusion Review constraints.
- Supported architectures and architecture-specific failures.
- Bootstrap cycles, coordinated transitions, and reverse-dependency tests.
- Licensing, repacking, generated files, embedded binaries, and vendored native
  code.
- Static linking: security fixes require tracking and rebuilding applications
  that incorporated affected Rust libraries.
- Accurate `Built-Using` and `Static-Built-Using` data.
- Offline, deterministic tests against installed packaged crate sources.

Introducing new crate graphs into stable releases may require older versions,
patches, backported toolchains, or coordinated dependency updates.

## Suggested implementation sequence

1. Build a small crate dependency closure in an Ubuntu PPA using current
   debcargo and overlays; retain inputs and outputs as fixtures.
2. Read an existing source tree through `cargo metadata`, select its package,
   and generate the minimum compatible naming, dependency, feature, and control
   data required by the fixtures.
3. Import `debcargo.toml` and support validated `[ubucargo]` settings.
4. Implement deterministic reconciliation, ownership, hints, and
   `package --check`.
5. Validate generated packages with clean `sbuild` builds and autopkgtests.
6. Implement workspace initialization, MSRV-aware importing of one crate from
   crates.io, packaging it, and upgrading it in place.
7. Add the Archive catalog, source-package download, and dependency reporting.
8. Add a narrow version-control adapter only if a real git-buildpackage,
   git-debrebase, or git-ubuntu workflow requires it.
