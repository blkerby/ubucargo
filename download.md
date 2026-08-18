# `ubucargo download`

## Synopsis

```console
ubucargo download SOURCE \
  [--from ubuntu:SUITE|debian:SUITE|ppa:OWNER/NAME] \
  [--version VERSION]
```

`download` retrieves an existing Debian or Ubuntu source package without
regenerating its packaging.

## Selection

Without constraints, the command selects the source-package candidate from the
workspace Archive view:

```console
ubucargo download rust-syn
```

`--version` requires an exact Debian source version. `--from` restricts the
candidate set to one Ubuntu suite, Debian suite, or configured PPA. When both
are present, both constraints apply.

Before acquisition, the command prints the selected version and origin:

```text
Downloading rust-syn 2.0.107-1 from ubuntu:noble-proposed/universe
```

Ubuntu release origins use the series name, such as `ubuntu:noble`; other
pockets use names such as `ubuntu:noble-updates` and
`ubuntu:noble-security`.

The definition of on-demand Debian suite metadata remains open; see
[issue 4](issues.md#4-debian-downloads-are-outside-the-archive-model).

## Acquisition

The command downloads the selected `.dsc` and every associated source file,
verifies them using standard Debian source-package tooling, and unpacks them
into an immediate workspace child named after the source package.

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

1. Query the shared Archive catalog and candidate resolver.
2. Apply origin and exact-version constraints.
3. Download all files named by the selected `.dsc` into a staging directory.
4. Verify and unpack the source package with standard Debian tools.
5. Validate the source-package identity and normalize missing hints.
6. Atomically move the completed source tree into the workspace.
