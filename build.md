# `ubucargo build`

## Synopsis

```console
ubucargo [--profile PROFILE] build [PACKAGE] [--output DIR]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

## Build contract

Ubucargo uses Ubuntu's standard package tools rather than creating a private
Cargo registry or build system:

```text
source tree with generated debian/ files
  -> sbuild prepares a temporary source package
  -> sbuild installs Build-Depends
  -> librust-*-dev packages populate /usr/share/cargo/registry
  -> dh-cargo builds and tests the crate
```

`build` is a local, one-package smoke-test wrapper around `sbuild`. It uses the
profile's configured repositories but does not discover or build a dependency
closure, publish artifacts, upload to a PPA, or modify a local repository.
Missing build dependencies are reported by `sbuild`; [`deps`](deps.md) provides
candidate details. Build outputs are written to the explicitly selected output
directory.

`build` passes the source directory directly to `sbuild`. It does not retain the
temporary `.dsc` or create source upload artifacts. Maintainers use
`dpkg-buildpackage -S`, GBP, or other standard tooling when persistent source
artifacts are required.

## Unshare build environment

`build` uses `sbuild`'s `unshare` backend. Ubucargo generates an APT source file
for the profile's official Ubuntu Archive base view and passes it to the
backend's automatic `mmdebstrap` invocation.

The sources, preferences, keys, and repository order derive from the same
normalized configuration as the
[isolated APT metadata view](apt-view.md). The unshare environment has its own
installed package state, but its available candidate universe must match the
view used by `deps`.

For a Noble `amd64` profile using `release`, `updates`, and `security` from
`main` and `universe`, the source file is equivalent to:

```text
Types: deb
URIs: https://archive.ubuntu.com/ubuntu
Suites: noble
Components: main universe
Architectures: amd64
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: https://archive.ubuntu.com/ubuntu
Suites: noble-updates
Components: main universe
Architectures: amd64
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: https://security.ubuntu.com/ubuntu
Suites: noble-security
Components: main universe
Architectures: amd64
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
```

Ubucargo selects the standard Ubuntu Archive URI appropriate for the profile
architecture. Source stanzas preserve configured pocket and component order.
The initial implementation uses normal APT priorities and adds no custom
preferences.

## Chroot cache identity

The unshare chroot name includes a deterministic hash of the normalized base
APT view, for example:

```text
noble-amd64-8b73cf2407ad
```

The hash covers the series, architecture, pockets, components, Archive URIs,
key identity, and other `mmdebstrap` inputs affecting the base root. It excludes
additional repositories because those are per-build overlays. Profiles with an
identical base view therefore share an `sbuild` cache entry without allowing
different views to collide.

Ubucargo generates a temporary additional `sbuild` configuration:

```perl
$unshare_mmdebstrap_auto_create = 1;
$unshare_mmdebstrap_keep_tarball = 1;
$unshare_mmdebstrap_max_age = 604800;

$unshare_mmdebstrap_extra_args = [
    "noble-amd64-8b73cf2407ad" => [
        "/tmp/ubucargo-XXXX/ubuntu.sources",
    ],
];

1;
```

The generated source file is the `mmdebstrap` mirror input, so it defines APT
sources during base-root creation rather than modifying the completed root.

The chroot argument passed to `sbuild` is a cache name, not a filesystem path.
`sbuild` owns its normal cache, creates the tarball when absent, refreshes it
according to `unshare_mmdebstrap_max_age`, and unpacks an ephemeral session for
each build. Ubucargo does not maintain another chroot cache.

## Invocation

Ubucargo points `SBUILD_CONFIG` at the generated configuration and invokes
`sbuild` equivalently to:

```console
SBUILD_CONFIG=/tmp/ubucargo-XXXX/sbuild.conf \
sbuild --chroot-mode=unshare \
  --chroot=noble-amd64-8b73cf2407ad \
  --dist=noble \
  --build=amd64 \
  --host=amd64 \
  --extra-repository='deb STAGING_REPOSITORY noble main' \
  --extra-repository-key=/tmp/ubucargo-XXXX/repository-key.gpg \
  /path/to/source-tree
```

`--extra-repository` and `--extra-repository-key` are repeated for each
configured additional repository in profile order. They are added only to the
ephemeral build session and do not modify the cached base tarball. A local
`file:` repository must be exposed at the same path inside the unshare session
or served over a reachable URI.

Ubucargo passes the exact trusted key resolved for the profile's APT view rather
than using `trusted=yes`. Ubuntu and Debian shorthands use packaged archive
keyrings, PPA shorthands trust the key reported by Launchpad over authenticated
HTTPS, and explicit repositories use their configured `Signed-By` key. The
shared trust contract is defined in
[`apt-view.md`](apt-view.md#repository-trust).

When Dose3 is installed, `build` may configure `sbuild` to use it as the
build-dependency uninstallability explainer.

## Implementation strategy

1. Validate the requested source tree and generated packaging state.
2. Normalize the profile's base Archive view and generate the deb822 source
   file.
3. Hash the normalized base-root inputs and generate the temporary `sbuild`
   configuration.
4. Resolve the profile's trusted repository keys and construct ordered
   repository arguments using the same key material as the APT view.
5. Invoke `sbuild` with the source directory, unshare cache name, and native
   build/host architectures.
6. Move or report build results in the requested output directory.
