# Shared APT metadata cache

## Purpose

Ubucargo generates APT sources for dependency checks and published source selection, and stores the current selection in one shared, user-writable cache. It reuses the same configuration paths and binary package cache across invocations rather than creating a separate cache for each combination of distributions, suites, and PPAs.

`deps` requests binary `Packages` and source `Sources` indexes. Translations, DEP-11 data, icons, command-not-found data, and unrelated architectures are disabled.

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

`sources.sources` is written only when its generated contents change, while the shared view is locked. Unchanged invocations preserve source and status modification times. APT decides whether the binary cache remains valid after a selection change.

`apt-get update` receives `pkgCacheFile::Generate=false` to preserve the binary cache while refreshing repository metadata. This override applies only to `update`; `indextargets` validates and reuses `pkgcache.bin`, rebuilding it when needed. `srcpkgcache.bin` remains disabled because the installed-package status is always empty.

Ubucargo holds an exclusive `view.lock` from source preparation through update, query, and index parsing. Concurrent invocations wait for that lock so they cannot replace each other's configuration or indexes during a query. The operating system releases the lock when the file is closed or the process exits.

All invocations share `lists/`. APT list cleanup is disabled so changing the requested distributions, suites, or PPAs does not delete indexes needed by later invocations. The current source configuration determines which files APT loads; cached indexes from unrelated origins do not enter candidate selection.

## Sources

For `deps --suite resolute`, Ubucargo creates binary and source entries for `resolute`, `resolute-updates`, and `resolute-security`, using `main` and `universe` for the selected architecture. `--proposed` adds `resolute-proposed` from the same Archive source. Each `--ppa ppa:<OWNER>/<NAME>` adds a binary and source `main` entry for Resolute. Only public Launchpad PPAs are supported; ubucargo does not read or manage credentials for private PPAs.

The generated deb822 entries select only their required APT targets:

```text
Types: deb deb-src
URIs: https://archive.ubuntu.com/ubuntu
Suites: resolute resolute-updates
Components: main universe
Architectures: amd64
Targets: Packages Sources
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
```

Security pockets use the standard Ubuntu security URI. Ubucargo selects the normal Ubuntu Archive URI for the requested architecture.

For `debian:unstable/<SOURCE>` or `deps --suite debian:unstable`, the selected view uses the exact Debian suite in `main`:

```text
Types: deb deb-src
URIs: https://deb.debian.org/debian
Suites: unstable
Components: main
Architectures: amd64
Targets: Packages Sources
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
```

Debian sources and dependency environments use the same locked cache and index parsing as Ubuntu. Published sources share version selection and source retrieval. The input selector determines the source view; `--suite` or the input default determines the checking view. Debian inputs default to their exact suite, Ubuntu inputs to their base series, and PPA inputs to their Ubuntu series. Crates.io and local inputs default to the current Ubuntu development series.

## Repository trust

- Ubuntu sources use the packaged Ubuntu Archive keyring.
- Debian sources use the packaged Debian archive keyring from `debian-archive-keyring`.
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

APT updates the selected indexes on every invocation. Update failures are fatal (`APT::Update::Error-Mode=any`), so an unavailable repository cannot silently turn into a misleading absence report. It reuses unchanged files and may apply index deltas, so Ubucargo needs no freshness policy or per-view cache identity.

Queries use the same source file and list directory. `apt-get indextargets` identifies the selected package indexes and their repository locations. Ubucargo reads `Packages` files for Rust versions and `Provides` and `Sources` files for source names, Debian versions, declared binaries, locations, and descriptor checksums. Dependency classification uses only checking-suite binaries. Inspection of published inputs across distributions or series queries the input view and checking view sequentially, releasing the shared lock between them; parsed results remain separate. Source metadata uses the same signature verification and index cache as binary metadata. Input selectors determine source distributions, suites, and pockets independently of dependency environment flags; a bare Ubuntu Archive series selects release, updates, and security, while a suffixed Ubuntu suite selects that pocket alone. A Debian selector uses its exact suite in `main`.

Only metadata operations run. Ubucargo never asks this configuration to install, upgrade, remove, or configure packages, and it does not modify the host's APT lists or dpkg status.
