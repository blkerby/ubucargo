# Shared APT metadata cache

## Purpose

`deps` generates APT sources from its command-line arguments and stores the current selection in one shared, user-writable cache. It reuses the same configuration paths and binary package cache across invocations rather than creating a separate cache for each combination of series and PPAs.

`deps` requests only binary `Packages` indexes. Translations, DEP-11 data, icons, command-not-found data, source indexes, and unrelated architectures are disabled.

## Layout

```text
~/.cache/ubucargo/apt/
  view.lock
  sources.sources
  sourceparts/
  status
  preferences
  preferences.d/
  pkgcache.bin
  lists/
    partial/
  keys/
```

The root is `$XDG_CACHE_HOME/ubucargo/apt` when `XDG_CACHE_HOME` is set. `status` and `preferences` are empty files; `sourceparts/` and `preferences.d/` are empty directories. They are created when missing and otherwise left untouched. These files are generated cache state, not user configuration.

`sources.sources` is replaced atomically only when its generated contents change. Before replacement, Ubucargo removes `pkgcache.bin` so selection changes invalidate the binary cache even within one filesystem timestamp tick. Unchanged invocations preserve source and status modification times.

`apt-get update` receives `pkgCacheFile::Generate=false` to preserve the binary cache while refreshing repository metadata. This override applies only to `update`; `indextargets` validates and reuses `pkgcache.bin`, rebuilding it when needed. `srcpkgcache.bin` remains disabled because the installed-package status is always empty.

Ubucargo holds an exclusive `view.lock` from source preparation through update, query, and index parsing. Concurrent invocations wait for that lock so they cannot replace each other's configuration or indexes during a query. The operating system releases the lock when the file is closed or the process exits.

All invocations share `lists/`. APT list cleanup is disabled so changing the requested series or PPAs does not delete indexes needed by later invocations. The current source configuration determines which files APT loads; cached indexes from unrelated origins do not enter candidate selection.

## Sources

For `deps --series noble`, Ubucargo creates binary-only entries for `noble`, `noble-updates`, and `noble-security`, using `main` and `universe` for the selected architecture. `--proposed` adds `noble-proposed` from the same Archive source. Each `--ppa ppa:OWNER/NAME` adds a binary-only `main` entry for Noble. Only public Launchpad PPAs are supported; ubucargo does not read or manage credentials for private PPAs.

The generated deb822 entries select only their required APT targets:

```text
Types: deb
URIs: https://archive.ubuntu.com/ubuntu
Suites: noble noble-updates
Components: main universe
Architectures: amd64
Targets: Packages
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
```

Security pockets use the standard Ubuntu security URI. Ubucargo selects the normal Ubuntu Archive URI for the requested architecture.

## Repository trust

- Ubuntu sources use the packaged Ubuntu Archive keyring.
- Public PPA sources trust Launchpad over authenticated HTTPS. Ubucargo retrieves the signing key and advertised fingerprint, requires them to match, and caches the key by fingerprint.

The PPA key and advertised fingerprint come from the same Launchpad authority, so the fingerprint is not treated as an independent pin. Ubucargo maintains no trust-on-first-use database or separate fingerprint configuration.

Every source uses `Signed-By`. Ubucargo does not enable unsigned repositories or `trusted=yes`.

## APT invocation

Ubucargo runs `apt-get update` with command-line configuration that supplies:

- the generated persistent source file and an empty source-parts directory;
- `~/.cache/ubucargo/apt/lists` as the list directory;
- an empty dpkg status file;
- empty preferences and preferences-parts paths;
- a persistent `pkgcache.bin`, with `srcpkgcache.bin` disabled;
- no list cleanup; and
- no translation downloads.

APT updates the selected indexes on every invocation. It reuses unchanged files and may apply index deltas, so Ubucargo needs no freshness policy or per-view cache identity.

Queries use the same source file and list directory. `apt-get indextargets` identifies the selected package indexes and their repository locations. Ubucargo reads those `Packages` files itself, extracts Rust package versions and `Provides`, and classifies candidates against the dependency requirements using Debian version ordering.

Only metadata operations run. Ubucargo never asks this configuration to install, upgrade, remove, or configure packages, and it does not modify the host's APT lists or dpkg status.
