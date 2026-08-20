# `ubucargo build`

## Synopsis

```console
ubucargo [--profile PROFILE] build [PACKAGE] [--output DIR]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

## Build contract

Ubucargo uses Ubuntu's standard package tools:

```text
source tree with generated debian/ files
  -> sbuild prepares a temporary source package
  -> sbuild installs Build-Depends
  -> librust-*-dev packages populate /usr/share/cargo/registry
  -> dh-cargo builds and tests the crate
```

`build` is a local, one-package smoke test around `sbuild`. It uses the profile's repositories and writes results to the selected output directory. `sbuild` reports missing build dependencies; [`deps`](deps.md) shows candidate details. Other tools handle dependency closure, publishing, uploads, and repository updates.

`build` passes the source directory to `sbuild`, which creates a temporary source package. Use `dpkg-buildpackage -S`, GBP, or similar tools for persistent source artifacts.

## Unshare build environment

`build` uses `sbuild`'s `unshare` backend. Ubucargo passes the profile's Ubuntu Archive sources to the backend's automatic `mmdebstrap` invocation.

Sources, preferences, keys, and order come from the same configuration as the [isolated APT view](apt-view.md). The build has separate installed-package state but sees the same candidates as `deps`.

For a Noble `amd64` profile using `release`, `updates`, and `security` from `main` and `universe`, the source file is equivalent to:

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

Ubucargo selects the standard Ubuntu Archive URI for the profile architecture and preserves pocket and component order. It uses normal APT priorities.

## Chroot cache identity

The unshare chroot name includes a deterministic hash of the normalized base APT view, for example:

```text
noble-amd64-8b73cf2407ad
```

The hash covers inputs that affect the base root, including the series, architecture, pockets, components, Archive URIs, and key identity. Additional repositories are per-build overlays, so identical base views share an `sbuild` cache entry.

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

The generated source file is `mmdebstrap`'s mirror input during base-root creation.

The chroot argument is a cache name, not a path. `sbuild` creates and refreshes the cached tarball and unpacks a temporary session for each build.

## Invocation

Ubucargo points `SBUILD_CONFIG` at the generated configuration and invokes `sbuild` equivalently to:

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

Each additional repository adds an `--extra-repository` and `--extra-repository-key` in profile order. These affect only the temporary build session. A local `file:` repository must exist at the same path inside the session or use a reachable URI.

Ubucargo uses the trusted key from the profile's [APT view](apt-view.md#repository-trust).

When Dose3 is installed, `build` may configure `sbuild` to use it as the build-dependency uninstallability explainer.
