# `ubucargo deps`

## Synopsis

```console
ubucargo deps [PACKAGE] --series SERIES \
  [--ppa ppa:OWNER/NAME]... [--architecture ARCH]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

`--series` selects an Ubuntu release. Ubucargo queries its release, updates, and security pockets from `main` and `universe`. Each `--ppa` adds that PPA's `main` component for the same series. `--architecture` defaults to `dpkg --print-architecture`.

## Output

The command shows direct Rust library dependencies and their candidates:

```text
DEPENDENCY  REQUIREMENT  STATUS        ORIGIN                    VERSION      LOCATION
serde       ^1 +derive   selected      ppa:example/rust-staging  1.0.219-1    noble/main
                         available     Ubuntu Archive            1.0.217-1    noble-updates/universe
syn         ^2           incompatible  Ubuntu Archive            1.0.109-2    noble/universe
foo         ^3           missing       -                         -            -
```

Statuses mark selected, usable, incompatible, and missing candidates.

Ubuntu Archive locations include pocket and component. PPA locations show the PPA, series, and component.

APT selects candidates using the temporary sources constructed from the command arguments. The result predicts a build configured with the same series and PPAs; `sbuild` remains authoritative, and Archive changes may change the result.

To build against the same staging repositories, pass them to `sbuild` with `--extra-repository` and `--extra-repository-key` as appropriate.

`deps` uses the same staged source and [patch-state handling](package.md#patch-state) as `package`, then orders candidates deterministically from the local APT indexes.

Staged dependency packages must be published in a PPA supplied with `--ppa`. `deps` does not scan source trees or artifact directories for candidates; PPA publication and build infrastructure remain outside ubucargo.

## APT metadata

`deps` uses only binary `Packages` indexes. They contain the package versions and versioned `Provides` needed to match Debian Rust feature packages; Archive `Sources` indexes are not downloaded.

Every invocation asks APT to update the selected indexes. APT reuses unchanged files and may apply index deltas, so it does not normally download the complete Archive again. Indexes for all previously requested series and PPAs share the cache described in [`apt-cache.md`](apt-cache.md).
