# `ubucargo deps`

## Synopsis

```console
ubucargo [--profile PROFILE] deps [PACKAGE]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

## Output

The command shows direct Rust library dependencies and their candidates:

```text
DEPENDENCY  REQUIREMENT  STATUS        ORIGIN                    VERSION      LOCATION
serde       ^1 +derive   selected      local:rust-staging        1.0.219-1    noble/main
                         available     ppa:example/rust-staging  1.0.218-2    noble/main
                         available     Ubuntu Archive            1.0.217-1    noble-updates/universe
syn         ^2           incompatible  Ubuntu Archive            1.0.109-2    noble/universe
foo         ^3           missing       -                         -            -
```

Statuses mark selected, usable, incompatible, and missing candidates.

Ubuntu Archive locations include pocket and component. PPA locations show the
PPA, series, and component. Local repositories use their profile name.

The selected candidate is what APT would choose during `build` from the same
metadata. Archive changes may change the result.

`deps` reads the same patched source metadata as `package` and orders candidates
deterministically from the local APT indexes.

## Buildability analysis

Dose3 can check a repository after `package` generates `debian/control`.
`dose-builddebcheck` uses the cached indexes to test the full Build-Depends,
including alternatives, conflicts, versions, virtual packages, architecture
restrictions, and Multi-Arch relationships.

Dose checks only build-dependency satisfiability. APT selects candidates, and
other commands handle Cargo selection, building, and publishing. `build` may use
`sbuild`'s Dose3 uninstallability explainer.
