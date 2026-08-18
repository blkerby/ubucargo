# `ubucargo deps`

## Synopsis

```console
ubucargo [--profile PROFILE] deps [PACKAGE]
```

`PACKAGE` may be omitted when the current directory is inside a source package.

## Output

The command shows direct Rust library dependencies and every relevant candidate,
not only the selected one:

```text
DEPENDENCY  REQUIREMENT  STATUS        ORIGIN                    VERSION      LOCATION
serde       ^1 +derive   selected      local:rust-staging        1.0.219-1    noble/main
                         available     ppa:example/rust-staging  1.0.218-2    noble/main
                         available     Ubuntu Archive            1.0.217-1    noble-updates/universe
syn         ^2           incompatible  Ubuntu Archive            1.0.109-2    noble/universe
foo         ^3           missing       -                         -            -
```

Statuses distinguish:

- the selected candidate;
- other usable candidates;
- present but incompatible candidates; and
- missing dependencies.

Ubuntu Archive locations include pocket and component. PPA locations retain the
PPA identity, series, and component and are not presented as Ubuntu Archive
pockets. Local repositories are presented by their configured profile name.

The selected candidate predicts what APT will choose during `build` from the
same current metadata. Archive changes between commands may change that result.

## Implementation strategy

1. Read direct dependency requirements from the same effective source metadata
   used by `package`.
2. Ensure the shared [isolated APT metadata view](apt-view.md) is present and
   sufficiently fresh.
3. Read its local binary indexes for package versions, `Provides`, Cargo
   identity, architecture, origin, and component data.
4. Ask APT for the selected candidate.
5. Classify and print all candidates in deterministic order without further
   network access.

## Buildability analysis

Dose3 may be used as an optional repository-level check after `package` has
generated `debian/control`. `dose-builddebcheck` consumes the same cached binary
and source indexes and determines whether the complete Build-Depends can be
satisfied, including alternatives, conflicts, version constraints, virtual
packages, architecture restrictions, and Multi-Arch relationships.

Dose does not select Cargo dependencies, build packages, or publish artifacts.
APT remains responsible for candidate policy; Dose answers whether a valid
installation solution exists. The initial implementation may rely on `sbuild`'s
Dose3 uninstallability explainer rather than invoke Dose directly.
