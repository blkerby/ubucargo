# `ubucargo download`

## Synopsis

```console
ubucargo [--profile PROFILE] download SOURCE \
  [--from ORIGIN] [--version VERSION] [--directory DIR]
```

`download` retrieves a published Debian or Ubuntu source package without regenerating its packaging.

## Selection

Without constraints, APT selects the source-package candidate from the profile:

```console
ubucargo download rust-syn
```

`--version` selects an exact Debian source version. `--from` limits candidates to a configured repository or an `ubuntu:SUITE`, `debian:SUITE`, or `ppa:OWNER/NAME` shorthand. Both options may be combined.

Before acquisition, the command prints the selected version and origin:

```text
Downloading rust-syn 2.0.107-1 from ubuntu:noble-proposed/universe
```

Ubuntu release origins use names such as `ubuntu:noble`, `ubuntu:noble-updates`, and `ubuntu:noble-security`.

An archive shorthand becomes a temporary deb822 entry with `Types: deb-src`. Debian uses the official archive, `main`, and Debian keyring; Ubuntu uses the profile components and architecture; a PPA uses the profile series and `main`. The entry exists only for this download.

Transient shorthands use the same [repository trust](apt-view.md#repository-trust) as persistent entries.

## Acquisition

The command downloads the selected `.dsc` and its source files, verifies them with standard Debian tools, and unpacks them into `DIR`. By default, `DIR` is the source package name in the current directory. Existing directories are left unchanged, `debian/` is preserved, and `package` does not run automatically. The completed tree is installed atomically after its source identity is checked.

## Hint normalization

If a known generated file lacks a `.debcargo.hint`, `download` copies the file to the hint path. For Debian packages, a missing hint means the generated file was unchanged, so it is a valid base. Packages already processed by ubucargo should have complete hints.

## Version control

The command writes only the source tree. Version control remains with the maintainer's tools.
