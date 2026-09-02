# Shared APT metadata cache

## Purpose

`deps` constructs temporary APT sources from its command-line arguments. APT refreshes and queries those sources using one shared, user-writable cache. Ubucargo has no persistent Archive configuration and does not create a separate cache for each combination of series and PPAs.

`deps` requests only binary `Packages` indexes. Translations, DEP-11 data, icons, command-not-found data, source indexes, and unrelated architectures are disabled.

## Layout

```text
~/.cache/ubucargo/apt/
  lists/
    partial/
  keys/
```

APT's `pkgcache.bin` and `srcpkgcache.bin` files are disabled. It builds any in-memory cache needed for the current query from the downloaded indexes.

All invocations share `lists/`. APT list cleanup is disabled so changing the requested series or PPAs does not delete indexes needed by later invocations. The temporary source configuration determines which files APT loads; cached indexes from unrelated origins do not enter candidate selection.

## Sources

For `deps --series noble`, Ubucargo creates binary-only entries for `noble`, `noble-updates`, and `noble-security`, using `main` and `universe` for the selected architecture. Each `--ppa ppa:OWNER/NAME` adds a binary-only `main` entry for Noble. Only public Launchpad PPAs are supported; ubucargo does not read or manage credentials for private PPAs.

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

- the temporary source file and an empty source-parts directory;
- `~/.cache/ubucargo/apt/lists` as the list directory;
- an empty dpkg status file;
- empty preferences and preferences-parts paths, with no default release;
- no persistent APT package-cache files;
- no list cleanup; and
- no translation downloads.

APT updates the selected indexes on every invocation. It reuses unchanged files and may apply index deltas, so Ubucargo needs no freshness policy or per-view cache identity.

Queries use the same source file and list directory. APT supplies candidate policy, Debian version ordering, architecture filtering, and `Provides` handling. `apt-get indextargets` supplies index filenames and signed repository metadata such as origin, suite, component, and architecture.

Only metadata operations run. Ubucargo never asks this configuration to install, upgrade, remove, or configure packages, and it does not modify the host's APT lists or dpkg status.
