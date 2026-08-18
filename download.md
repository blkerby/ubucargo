# `ubucargo download`

## Synopsis

```console
ubucargo [--profile PROFILE] download SOURCE \
  [--from ORIGIN] [--version VERSION] [--directory DIR]
```

`download` retrieves an existing Debian or Ubuntu source package without
regenerating its packaging.

## Selection

Without constraints, the command selects the source-package candidate from the
profile APT view:

```console
ubucargo download rust-syn
```

`--version` requires an exact Debian source version. `--from` restricts the
candidate set to one configured repository name or explicit Ubuntu, Debian, or
PPA origin. When both are present, both constraints apply.

Before acquisition, the command prints the selected version and origin:

```text
Downloading rust-syn 2.0.107-1 from ubuntu:noble-proposed/universe
```

Ubuntu release origins use the series name, such as `ubuntu:noble`; other
pockets use names such as `ubuntu:noble-updates` and
`ubuntu:noble-security`.

The definition of on-demand Debian suite metadata remains open; see
[issue 2](issues.md#2-debian-downloads-are-outside-the-archive-model).

## Acquisition

The command downloads the selected `.dsc` and every associated source file,
verifies them using standard Debian source-package tooling, and unpacks them
into `DIR`, which defaults to a directory named after the source package in the
current directory.

It must refuse to overwrite an existing source-package directory. It preserves
the complete downloaded `debian/` directory and does not run `package`
automatically.

## Hint normalization

For each known generated file without a companion `.debcargo.hint`, `download`
copies the file to the corresponding hint path. For a package originating from
Debian, an absent hint means the generated content was retained unchanged, so
the downloaded file is a valid generated base. Packages previously processed by
ubucargo should already contain a complete set of hints for emitted files.

## Version control

The initial implementation does not create a repository, import history, or
make commits.

## Implementation strategy

1. Ensure the shared [isolated APT metadata view](apt-view.md) is present and
   sufficiently fresh.
2. Ask APT to select the source candidate, then apply origin and exact-version
   constraints.
3. Download all files named by the selected `.dsc` into a staging directory.
4. Verify and unpack the source package with standard Debian tools.
5. Validate the source-package identity and normalize missing hints.
6. Atomically move the completed source tree to the requested destination.
