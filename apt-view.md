# Isolated APT metadata view

## Purpose

The packaging profile uses a metadata-only APT view. APT handles repository
metadata, priorities, version ordering, architecture filtering, `Provides`, and
candidate selection. Ubucargo adds Cargo-specific reporting from APT's indexes.
The system APT remains responsible for package installation and configuration.

## Cache identity and layout

The cache key is a hash of the normalized APT configuration:
series, architecture, pockets, components, additional repositories, signing-key
identities, repository order, and preferences.

Repository entries may use the shorthands `ubuntu:SUITE`, `debian:SUITE`, and
`ppa:OWNER/NAME`, with optional `types`, `components`, and `architectures` keys.
Shorthands and explicit `source` entries both normalize to deb822 before use.

Persistent shorthands default to `Types: deb deb-src`. Transient source lookups
use only `Types: deb-src`.

## Repository trust

- Ubuntu and Debian shorthands use their packaged archive keyrings.
- PPA shorthands retrieve the signing key and advertised fingerprint from
  Launchpad over authenticated HTTPS and require them to match. Launchpad is the
  trust anchor.
- An explicit deb822 source must provide `Signed-By` as embedded key material or
  a readable local keyring path.

Every repository must be signed. Ubucargo rejects `trusted=yes`, and explicit
sources require a usable `Signed-By`. A PPA key change accepted by Launchpad
changes the normalized view.

```text
~/.cache/ubucargo/apt/<view-hash>/
  apt.conf
  etc/apt/sources.list.d/ubucargo.sources
  etc/apt/preferences.d/
  etc/apt/apt.conf.d/
  etc/apt/keyrings/
  var/lib/apt/lists/
  var/lib/dpkg/status
  var/cache/apt/
  var/log/apt/
```

Profiles with identical normalized views share a cache. The dpkg status file is
empty unless a modeled installed state is needed.

## Isolation contract

APT runs as the invoking user. Its configuration, sources, preferences,
keyrings, indexes, caches, logs, locks, and dpkg status file all stay inside the
view directory.

Ubucargo copies trusted keys into the view and points each `Signed-By` at its
copy. It does not use host APT or dpkg state. Before running APT, it may reject
resolved paths or hooks outside the view.

## Allowed operations

The view supports only:

- refreshing repository metadata;
- inspecting index targets, package records, and policy;
- downloading an explicitly selected binary or source package.

Commands such as install, upgrade, remove, autoremove, and build-dep are not
allowed.

## Cached metadata

A refresh stores signed Release metadata and the configured `Packages` and
`Sources` indexes. They contain versions, architectures, dependencies,
`Provides`, checksums, download paths, components, origins, and fields such as
`X-Cargo-*`.

Translations, DEP-11 metadata, icons, command-not-found data, Contents indexes,
and unrelated architectures are disabled; `Acquire::Languages` is set to `none`.

After refresh, policy and dependency checks are local. `ubucargo deps` refreshes
once when the cache is missing, stale, or explicitly refreshed. Offline mode may
use stale metadata with a warning.

`deb` entries fetch `Packages` indexes; `deb-src` entries fetch `Sources` indexes.
Repositories used only as binary staging archives may set `types = ["deb"]` and
fetch only `Packages` indexes.

## Candidate and record access

APT selects a candidate from the view's sources, preferences, architecture, and
status database. Ubucargo reads the indexes to explain other candidates,
including Cargo identity and feature `Provides`.

Component, pocket, PPA, and origin labels come from each index target and its
signed Release metadata, not the package version.

Staged packages enter through configured APT repositories. Candidate discovery
is index-driven, whether the repository is local or a PPA.

## Command consumers

- [`deps`](deps.md) inspects candidates from the refreshed local view.
- [`download`](download.md) uses the view to select and retrieve Ubuntu and PPA
  source packages.
- [`import`](import.md) uses its selected `rustc` candidate when the profile has
  no explicit Rust target.
- [`build`](build.md) generates the same sources, preferences, and keyrings inside
  its `sbuild` environment.

The build environment has separate installed-package state but uses the same
normalized repository configuration.

## Safety verification

An integration check should create, refresh, and query a temporary view as an
unprivileged user. It must verify that writes stay inside the view, host APT and
dpkg state remain untouched, dpkg does not run, and cleanup removes only the
validated view directory.
