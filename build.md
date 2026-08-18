# `ubucargo build`

## Synopsis

```console
ubucargo build [PACKAGE]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

## Build contract

Ubucargo uses Ubuntu's standard package tools rather than creating a private
Cargo registry or build system:

```text
source tree with generated debian/ files
  -> standard Debian tools produce a source package
  -> sbuild installs Build-Depends
  -> librust-*-dev packages populate /usr/share/cargo/registry
  -> dh-cargo builds and tests the crate
```

`build` builds only the requested source package. It exposes already-built
workspace binary packages and configured PPAs to `sbuild`, but does not discover
or build a dependency closure or other workspace source trees. Missing build
dependencies are reported by `sbuild`; [`deps`](deps.md) provides candidate
details.

Creation and storage of the input `.dsc` and resulting artifacts remains open;
see [issue 1](issues.md#1-source-package-artifact-lifecycle).

## Unshare build environment

`build` uses `sbuild`'s `unshare` backend. Ubucargo generates an APT source file
for the workspace's official Ubuntu Archive base view and passes it to the
backend's automatic `mmdebstrap` invocation.

For a Noble `amd64` workspace using `release`, `updates`, and `security` from
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

Ubucargo selects the standard Ubuntu Archive URI appropriate for the workspace
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
PPAs and local packages because those are per-build overlays. Workspaces with an
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
  --extra-repository='deb PPA_HTTPS_URI noble main' \
  --extra-repository-key=/tmp/ubucargo-XXXX/ppa-signing-key.gpg \
  --extra-package=/path/to/workspace-package.deb \
  /path/to/source-package.dsc
```

`--extra-repository` and `--extra-repository-key` are repeated for each PPA in
workspace order. PPAs are added only to the ephemeral build session and do not
modify the cached base tarball.

Ubucargo retrieves and verifies PPA signing keys rather than using
`trusted=yes`. The fingerprint trust and pinning mechanism remains open; see
[issue 5](issues.md#5-ppa-key-verification-lacks-a-trust-bootstrap).

`--extra-package` is repeated for applicable workspace binary packages, or may
name a directory containing them. `sbuild` exposes these through its temporary
APT archive, so ubucargo does not create another local repository. Artifact
selection remains open; see
[issue 6](issues.md#6-workspace-binary-artifact-discovery-is-unspecified).

## Implementation strategy

1. Validate the requested source tree and generated packaging state.
2. Produce or locate the source-package `.dsc`.
3. Normalize the workspace's base Archive view and generate the deb822 source
   file.
4. Hash the normalized base-root inputs and generate the temporary `sbuild`
   configuration.
5. Retrieve and validate configured PPA keys and construct ordered repository
   arguments.
6. Select applicable workspace binary packages for the workspace architecture.
7. Invoke `sbuild` with the unshare cache name and native build/host
   architectures.
8. Report the build result and artifact locations.
