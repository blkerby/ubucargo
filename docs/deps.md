# `ubucargo deps`

## Synopsis

```console
ubucargo deps [CRATE [VERSION]] [--package-dir DIR] --series SERIES \
  [--proposed] [--ppa ppa:OWNER/NAME]... [--architecture ARCH]
```

With no `CRATE` or `--package-dir`, `deps` uses the nearest parent source package.
`--package-dir` selects an existing source package explicitly. `CRATE` instead
selects a crate from crates.io; `VERSION` selects an exact release, while an
omitted version selects the latest release using the same rules as
[`package`](package.md#target-and-version-selection). `CRATE` and `--package-dir`
may not be combined.

```console
# Inspect the nearest source package.
ubucargo deps --series noble

# Inspect an explicit source package.
ubucargo deps --package-dir ./rust-serde --series noble

# Inspect the latest serde release from crates.io.
ubucargo deps serde --series noble

# Inspect an exact serde release from crates.io.
ubucargo deps serde 1.0.220 --series noble
```

Source-package mode uses its `debian/debcargo.toml` and patch stack. When that
configuration contains `crate_src_path`, `deps` reads the local crate selected
by that path.
Crates.io mode generates a temporary package with the default debcargo
configuration and leaves no source package behind. Its results therefore do
not account for configuration or patches that a maintainer might add later.

`--series` selects an Ubuntu release. Ubucargo queries its release, updates, and security pockets from `main` and `universe`. `--proposed` additionally includes the release's proposed pocket with normal candidate consideration. Each `--ppa` adds a public Launchpad PPA's `main` component for the same series; private PPAs are not supported. `--architecture` defaults to `dpkg --print-architecture`.

## Output

The command reads the source paragraph's generated `Build-Depends` and reports
its direct Rust library dependencies, represented by `librust-*-dev` package
expressions. The generated control file is authoritative because it reflects
debcargo configuration, enabled features, development dependencies, target
conditions, and patches.

The report shows each dependency and its candidates:

```text
DEPENDENCY  STATUS        LOCATION                            VERSION      REQUIREMENT
serde       selected      ppa:example/rust-staging (noble)    1.0.219-1    ^1 +derive
            available     noble-updates/universe              1.0.217-1    ^1 +derive
syn         incompatible  noble/universe                      1.0.109-2    ^2
foo         missing       -                                   -            ^3
```

The dependency appears on the first row for a dependency; additional candidates
leave it blank. The requirement is repeated because its colors describe each
candidate independently. `REQUIREMENT` is last so a long, sorted feature list
may extend beyond the nominal column width without disturbing the other columns.
Requirements are not truncated or wrapped by ubucargo.

The displayed semver requirement is inferred from the Debian virtual-package
compatibility line and its strongest applicable lower bound. For example,
`librust-actix-http-3+default-dev (>= 3.13.0)` is shown as `^3.13.0 +default`.
This describes the effective relation checked by `deps`; it does not attempt to
reproduce the original spelling from `Cargo.toml`.

Statuses have the following meanings:

- `selected`: APT's selected package satisfies the complete dependency expression;
- `available`: another version or origin also satisfies it but was not selected by APT;
- `incompatible`: packages for the crate exist, but none satisfy the required semver line and features; and
- `missing`: no package for the crate exists in the selected sources.

When standard output is a terminal, statuses are colored green for `selected`,
gray for `available`, yellow for `incompatible`, and red for `missing`. The
semver expression and each `+feature` in `REQUIREMENT` are colored independently:
yellow when a corresponding package exists but is incompatible, and red when it
is missing. Satisfied components remain neutral on every row, reserving green
for the `selected` status. Set `NO_COLOR` to disable colors; redirected output
is always plain text.

APT alternatives are satisfied when any alternative resolves. When a dependency
requires multiple feature packages, all of them must resolve for the dependency
to be selected or available.

Ubuntu Archive locations use `suite/component`. PPA locations use
`ppa:OWNER/NAME (series)` and omit the component, which is always `main`.

APT selects candidates using the temporary sources constructed from the command arguments. The result predicts a build configured with the same series and PPAs; `sbuild` remains authoritative, and Archive changes may change the result.

To build against the same staging repositories, pass them to `sbuild` with `--extra-repository` and `--extra-repository-key` as appropriate.

In source-package mode, `deps` validates the root Cargo identity and
`debian/debcargo.toml`, rejects unrefreshed quilt changes, and copies the patch
stack into a temporary debcargo overlay. Debcargo applies the copied patches
there and generates the control file used for the report. Refreshed patches may
remain applied in the working tree because `deps` does not modify it.

Unlike `package`, `deps` does not acquire an old orig tarball, reconcile source
trees, require the working quilt stack to be popped, or plan or install source,
changelog, generated-file, or hint changes. In both modes, it orders candidates
deterministically from the local APT indexes.

Staged dependency packages must be published in a PPA supplied with `--ppa`. `deps` does not scan source trees or artifact directories for candidates; PPA publication and build infrastructure remain outside ubucargo.

## APT metadata

`deps` uses only binary `Packages` indexes. They contain the package versions and versioned `Provides` needed to match Debian Rust feature packages; Archive `Sources` indexes are not downloaded.

Every invocation asks APT to update the selected indexes. APT reuses unchanged files and may apply index deltas, so it does not normally download the complete Archive again. Indexes for all previously requested series and PPAs share the cache described in [`apt-cache.md`](apt-cache.md).

## Exit status

`deps` exits 0 when every dependency is satisfiable, 1 when at least one
dependency is incompatible or missing, and 2 on command, staging, network, or
metadata errors.
