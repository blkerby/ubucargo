# Isolated APT metadata view

## Purpose

The packaging profile's Archive implementation is an isolated, metadata-only APT view.
APT remains the authority for repository metadata, pin priorities, Debian
version ordering, architecture filtering, `Provides`, and candidate selection.
Ubucargo interprets APT's local indexes for Cargo-specific dependency reporting
but does not implement a second package resolver.

The view is not a chroot or an alternate package-management environment. It
never installs, upgrades, removes, or configures packages.

## Cache identity and layout

The view is keyed by a deterministic hash of the normalized profile APT
configuration, including series, architecture, pockets, components, additional repositories,
signing-key identities, repository order, and preferences.

Repository entries may use the shorthands `ubuntu:SUITE`, `debian:SUITE`, and
`ppa:OWNER/NAME` plus optional `types`, `components`, and `architectures` keys.
They are expanded immediately into the same normalized deb822 representation as
an explicit `source` entry. Shorthand identity does not affect later APT logic.

Persistent shorthand repositories default to `Types: deb deb-src`. A transient
source acquisition view uses `Types: deb-src`, preventing a Debian or foreign
Ubuntu source lookup from adding its binary packages to the profile candidate
universe.

## Repository trust

Repository trust follows the authority named by the configuration:

- Ubuntu and Debian shorthands use their packaged archive keyrings.
- PPA shorthands trust Launchpad over authenticated HTTPS. Ubucargo retrieves
  the PPA signing key and advertised fingerprint from Launchpad, computes the
  key's fingerprint, and rejects a missing or mismatched value. The comparison
  checks Launchpad's response for consistency; it is not an independent pin.
- An explicit deb822 source must provide `Signed-By` as embedded key material or
  a readable local keyring path. Ubucargo does not discover keys for explicit
  repositories.

Ubucargo rejects `trusted=yes`, unsigned repositories, and explicit sources
without a usable `Signed-By`. It does not maintain trust-on-first-use state or a
separate fingerprint configuration. A PPA key change accepted by Launchpad is
therefore accepted by ubucargo and produces a different normalized key identity.

```text
~/.cache/ubucargo/apt/<view-hash>/
  apt.conf
  etc/
    apt/
      sources.list.d/
        ubucargo.sources
      preferences.d/
      apt.conf.d/
      keyrings/
  var/
    lib/
      apt/lists/
      dpkg/status
    cache/
      apt/
    log/
      apt/
```

Profiles with identical normalized views may share this cache. The synthetic
dpkg status file is empty unless the design later requires a modeled installed
state.

## Isolation contract

Ubucargo runs APT as the invoking user and must not require `sudo`. The generated
configuration redirects the complete APT state beneath the view directory,
including:

- main and fragment configuration files;
- source lists and source fragments;
- preferences;
- trusted keyrings;
- downloaded package and source indexes;
- package caches;
- logs, locks, and the dpkg status file.

The custom configuration and fragment directories do not inherit files from
`/etc/apt`. Ubucargo copies each resolved trusted key into the view and rewrites
repository stanzas to use that specific `Signed-By` key. Ubucargo never reads or
writes the host APT or dpkg state and never invokes host dpkg through this view.

Before running APT, ubucargo may inspect `apt-config dump` and reject resolved
paths or hooks outside the view directory.

## Allowed operations

The view exposes a small internal operation set rather than arbitrary APT
command execution:

- refresh repository metadata;
- inspect configured index targets;
- inspect package records and policy;
- download an explicitly selected binary package;
- download an explicitly selected source package.

Install, upgrade, remove, autoremove, build-dep, and arbitrary user-supplied APT
operations are not supported.

## Cached metadata

A refresh stores signed Release metadata plus the configured binary `Packages`
and source `Sources` indexes. These records provide package and source versions,
architectures, dependencies, `Provides`, checksums, download paths, components,
origins, and custom fields such as `X-Cargo-*`.

Translations, DEP-11 metadata, icons, command-not-found data, Contents indexes,
and unrelated architectures are disabled. `Acquire::Languages` is set to
`none`.

After a successful refresh, package policy and dependency inspection are local
operations. `ubucargo deps` does not issue one request per package or dependency.
It refreshes the whole view only when the cache is missing, explicitly
refreshed, or considered stale.

An offline mode may use existing metadata while warning when it is stale. A
forced-refresh option bypasses the normal freshness check.

`deb` entries fetch binary `Packages` indexes. `deb-src` entries fetch source
`Sources` indexes. Repositories that are used only as binary staging archives
may set `types = ["deb"]` to avoid downloading source indexes.

## Candidate and record access

APT calculates the selected candidate using the view's sources, preferences,
architecture, status database, and local package repository. Ubucargo reads the
downloaded index records to enumerate and explain every relevant candidate,
including Cargo identity and feature `Provides`.

The index-target interface supplies the local filename and repository metadata
for each downloaded index. Component, pocket, PPA, and origin labels are derived
from that target and its signed Release metadata rather than guessed from a
package version.

Staged packages enter the view only through configured APT repositories. A
local repository and a PPA are equivalent on the consumption side; ubucargo
does not scan source trees or directories of `.deb` files for additional
candidates.

## Command consumers

- [`deps`](deps.md) performs all candidate inspection from the refreshed local
  view.
- [`download`](download.md) uses the view to select and retrieve Ubuntu and PPA
  source packages.
- [`import`](import.md) uses its selected `rustc` candidate when the profile
  has no explicit Rust target.
- [`build`](build.md) generates the same sources, preferences, and keyrings
  inside its `sbuild` environment.

The build environment has its own installed package state, but repository
selection must derive from the same normalized profile configuration.

## Safety verification

One integration check should create a temporary view, refresh and query it as an
unprivileged user, and assert that:

- every filesystem write remains under the temporary view;
- no host APT or dpkg lock is opened;
- no host configuration fragment or hook is loaded;
- no dpkg process is executed; and
- cache cleanup removes only the validated view directory.
