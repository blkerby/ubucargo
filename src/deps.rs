//! Inspects Ubuntu binary package candidates for direct Rust dependencies.

mod apt;
mod control;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use debian_control::relations::VersionConstraint;

use crate::{cargo, generate, resolve, util::run_command};

use self::{
    apt::PackageCandidate,
    control::{Dependency, PackageRequirement, parse_rust_package_name},
};

const GREEN: &str = "\x1b[32m";
const GRAY: &str = "\x1b[90m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// Inspect Ubuntu candidates for a crate's direct Rust dependencies.
#[derive(clap::Args)]
pub struct DepArgs {
    /// Crate name from crates.io; conflicts with --package-dir.
    #[arg(value_name = "CRATE", conflicts_with = "package_dir")]
    pub crate_name: Option<String>,

    /// Exact crate version; defaults to the latest release when a crate is named.
    #[arg(value_name = "VERSION", requires = "crate_name")]
    pub version: Option<String>,

    /// Existing source package directory; defaults to the nearest parent package.
    #[arg(long = "package-dir", value_name = "DIR")]
    pub package_dir: Option<PathBuf>,

    /// Ubuntu series to query.
    #[arg(long, value_name = "SERIES")]
    pub series: String,

    /// Include the Ubuntu proposed pocket.
    #[arg(long)]
    pub proposed: bool,

    /// Public Launchpad PPA to include.
    #[arg(long, value_name = "ppa:OWNER/NAME")]
    pub ppa: Vec<String>,

    /// Debian architecture; defaults to dpkg --print-architecture.
    #[arg(long, value_name = "ARCH")]
    pub architecture: Option<String>,
}

/// Availability of one displayed requirement component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequirementStatus {
    /// The displayed candidate satisfies the relation.
    Satisfied,
    /// A corresponding package exists but does not satisfy the relation.
    Incompatible,
    /// No corresponding package exists for the displayed candidate.
    Missing,
}

/// One independently colored component of a Cargo-style requirement.
#[derive(Debug, Eq, PartialEq)]
struct RequirementPart {
    /// Semver expression or feature name, including its `+` prefix.
    text: String,
    /// Candidate availability for this component.
    status: RequirementStatus,
}

/// Feature identity used to recognize a related package with the wrong relation.
enum RequirementKind<'a> {
    /// Any package for the crate when no featureless relation was generated.
    Any,
    /// A package relation without a feature component.
    Base,
    /// A package relation for the named Cargo feature.
    Feature(&'a str),
}

/// One printable dependency-candidate row.
#[derive(Debug, Eq, PartialEq)]
struct Row {
    /// Dependency name, omitted on continuation rows.
    dependency: String,
    /// Candidate classification.
    status: &'static str,
    /// Repository location.
    location: String,
    /// Debian package version.
    version: String,
    /// Cargo-style requirement components classified for this candidate.
    requirement: Vec<RequirementPart>,
}

/// Stages a crate, queries APT, prints the report, and returns whether it is unsatisfied.
pub fn run(args: DepArgs) -> Result<bool> {
    let architecture = match args.architecture {
        Some(architecture) => architecture,
        None => apt::read_architecture()?,
    };
    let current = if args.crate_name.is_some() {
        None
    } else {
        Some(
            env::current_dir()
                .context("get current directory")?
                .canonicalize()
                .context("resolve current directory")?,
        )
    };
    let resolved = resolve::resolve_package(
        current.as_deref(),
        args.package_dir.as_deref(),
        args.crate_name.as_deref(),
        args.version.as_deref(),
        None,
    )?;
    let generated = generate::generate_package(&resolved, false)?;
    let dependencies = read_staged_dependencies(&generated.source, &architecture)?;
    let candidates = apt::load_candidates(&args.series, &architecture, args.proposed, &args.ppa)?;
    let rows = classify(&dependencies, &candidates);
    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    print!("{}", format_table(&rows, color));
    Ok(rows
        .iter()
        .any(|row| matches!(row.status, "incompatible" | "missing")))
}

/// Applies staged quilt patches and reads Cargo and Debian dependency requirements.
fn read_staged_dependencies(source: &Path, architecture: &str) -> Result<Vec<Dependency>> {
    if source.join("debian/patches/series").is_file() {
        run_command(
            Command::new("quilt")
                .args(["push", "-a", "--quiltrc=-"])
                .env("QUILT_PATCHES", "debian/patches")
                .current_dir(source),
            "apply staged quilt patches",
        )?;
    }
    let cargo_dependencies = cargo::read_root_package(source)?.dependencies;
    control::read_dependencies(
        &source.join("debian/control"),
        architecture,
        &cargo_dependencies,
    )
}

/// Classifies all candidates for each dependency in deterministic order.
fn classify(dependencies: &[Dependency], candidates: &[PackageCandidate]) -> Vec<Row> {
    let mut candidates_by_crate: BTreeMap<&str, Vec<&PackageCandidate>> = BTreeMap::new();
    for candidate in candidates {
        let mut crate_names = BTreeSet::new();
        for provided in candidate.provides.keys() {
            if let Some((name, _)) = parse_rust_package_name(provided) {
                crate_names.insert(name);
            }
        }
        for name in crate_names {
            candidates_by_crate.entry(name).or_default().push(candidate);
        }
    }

    let mut rows = Vec::new();
    for dependency in dependencies {
        let mut matching = candidates_by_crate
            .get(dependency.name.as_str())
            .cloned()
            .unwrap_or_default();
        matching.sort_by(|first, second| second.version.cmp(&first.version));
        if matching.is_empty() {
            rows.push(Row {
                dependency: dependency.name.clone(),
                status: "missing",
                location: "-".to_owned(),
                version: "-".to_owned(),
                requirement: make_requirement(dependency, None),
            });
            continue;
        }
        let mut satisfying = Vec::new();
        for candidate in &matching {
            if satisfies(dependency, candidate) {
                satisfying.push(*candidate);
            }
        }
        let compatible = !satisfying.is_empty();
        let mut candidates = if compatible { satisfying } else { matching };
        let mut displayed = BTreeSet::new();
        candidates.retain(|candidate| displayed.insert((&candidate.version, &candidate.location)));
        for (index, candidate) in candidates.into_iter().enumerate() {
            let status = if !compatible {
                "incompatible"
            } else if index == 0 {
                "selected"
            } else {
                "available"
            };
            rows.push(Row {
                dependency: if index == 0 {
                    dependency.name.clone()
                } else {
                    String::new()
                },
                status,
                location: candidate.location.clone(),
                version: candidate.version.to_string(),
                requirement: make_requirement(dependency, Some(candidate)),
            });
        }
    }
    rows
}

/// Builds independently classified semver and feature display components.
fn make_requirement(
    dependency: &Dependency,
    candidate: Option<&PackageCandidate>,
) -> Vec<RequirementPart> {
    let mut output = Vec::new();
    let default = dependency
        .debian_requirements
        .get(&Some("default".to_owned()));
    let base = dependency.debian_requirements.get(&None);
    let mut requirements = Vec::new();
    let kind = if let Some(default) = default {
        for alternatives in default {
            requirements.push(alternatives.iter().collect());
        }
        if let Some(base) = base {
            for alternatives in base {
                requirements.push(alternatives.iter().collect());
            }
        }
        RequirementKind::Feature("default")
    } else if let Some(base) = base {
        for alternatives in base {
            requirements.push(alternatives.iter().collect());
        }
        RequirementKind::Base
    } else {
        let mut alternatives = Vec::new();
        for feature in dependency.debian_requirements.values() {
            for entry in feature {
                alternatives.extend(entry);
            }
        }
        requirements.push(alternatives);
        RequirementKind::Any
    };
    let version_status = classify_requirement(&requirements, candidate, &dependency.name, kind);
    output.push(RequirementPart {
        text: dependency.cargo_requirement.clone(),
        status: version_status,
    });

    if default.is_none() {
        output.push(RequirementPart {
            text: "-default".to_owned(),
            status: version_status,
        });
    }
    for (feature, feature_requirements) in &dependency.debian_requirements {
        let Some(feature) = feature else {
            continue;
        };
        if feature == "default" {
            continue;
        }
        let mut requirements = Vec::new();
        for alternatives in feature_requirements {
            requirements.push(alternatives.iter().collect());
        }
        output.push(RequirementPart {
            text: format!("+{feature}"),
            status: classify_requirement(
                &requirements,
                candidate,
                &dependency.name,
                RequirementKind::Feature(feature),
            ),
        });
    }
    output
}

/// Classifies one visible requirement component against a package candidate.
fn classify_requirement(
    requirements: &[Vec<&PackageRequirement>],
    candidate: Option<&PackageCandidate>,
    crate_name: &str,
    kind: RequirementKind<'_>,
) -> RequirementStatus {
    let Some(candidate) = candidate else {
        return RequirementStatus::Missing;
    };
    let mut satisfied = !requirements.is_empty();
    for alternatives in requirements {
        if !alternatives
            .iter()
            .any(|requirement| satisfies_package(requirement, candidate))
        {
            satisfied = false;
            break;
        }
    }
    if satisfied {
        return RequirementStatus::Satisfied;
    }
    for provided in candidate.provides.keys() {
        let Some((name, provided_feature)) = parse_rust_package_name(provided) else {
            continue;
        };
        let related = match kind {
            RequirementKind::Any => true,
            RequirementKind::Base => provided_feature.is_none(),
            RequirementKind::Feature(feature) => provided_feature == Some(feature),
        };
        if name == crate_name && related {
            return RequirementStatus::Incompatible;
        }
    }
    RequirementStatus::Missing
}

/// Reports whether one binary package satisfies every entry for a dependency.
fn satisfies(dependency: &Dependency, candidate: &PackageCandidate) -> bool {
    for feature in dependency.debian_requirements.values() {
        for alternatives in feature {
            let mut entry_satisfied = false;
            for requirement in alternatives {
                if satisfies_package(requirement, candidate) {
                    entry_satisfied = true;
                    break;
                }
            }
            if !entry_satisfied {
                return false;
            }
        }
    }
    true
}

/// Reports whether one candidate satisfies one concrete or virtual package relation.
fn satisfies_package(requirement: &PackageRequirement, candidate: &PackageCandidate) -> bool {
    let Some((constraint, expected_version)) = &requirement.version else {
        return candidate.provides.contains_key(&requirement.name);
    };
    let Some(actual_version) = candidate
        .provides
        .get(&requirement.name)
        .and_then(Option::as_ref)
    else {
        return false;
    };
    match constraint {
        VersionConstraint::GreaterThanEqual => actual_version >= expected_version,
        VersionConstraint::LessThanEqual => actual_version <= expected_version,
        VersionConstraint::Equal => actual_version == expected_version,
        VersionConstraint::GreaterThan => actual_version > expected_version,
        VersionConstraint::LessThan => actual_version < expected_version,
    }
}

/// Formats report rows as an unbordered, space-aligned table.
/// The last column is not padded, so that an outlier long field
/// avoids making the entire output too wide.
fn format_table(rows: &[Row], color: bool) -> String {
    let headers = ["DEPENDENCY", "STATUS", "LOCATION", "VERSION"];
    let mut widths = headers.map(str::len);
    for row in rows {
        widths[0] = widths[0].max(row.dependency.len());
        widths[1] = widths[1].max(row.status.len());
        widths[2] = widths[2].max(row.location.len());
        widths[3] = widths[3].max(row.version.len());
    }

    let mut output = format!(
        "{:<dependency_width$}  {:<status_width$}  {:<location_width$}  {:<version_width$}  REQUIREMENT\n",
        headers[0],
        headers[1],
        headers[2],
        headers[3],
        dependency_width = widths[0],
        status_width = widths[1],
        location_width = widths[2],
        version_width = widths[3],
    );
    for row in rows {
        let status = if color {
            let code = match row.status {
                "selected" => GREEN,
                "available" => GRAY,
                "incompatible" => YELLOW,
                "missing" => RED,
                _ => "",
            };
            format!("{code}{:<width$}{RESET}", row.status, width = widths[1])
        } else {
            format!("{:<width$}", row.status, width = widths[1])
        };
        let mut requirement = String::new();
        for (index, part) in row.requirement.iter().enumerate() {
            if index > 0 {
                requirement.push(' ');
            }
            let code = if color {
                match part.status {
                    RequirementStatus::Satisfied => "",
                    RequirementStatus::Incompatible => YELLOW,
                    RequirementStatus::Missing => RED,
                }
            } else {
                ""
            };
            requirement.push_str(code);
            requirement.push_str(&part.text);
            if !code.is_empty() {
                requirement.push_str(RESET);
            }
        }
        output.push_str(&format!(
            "{:<dependency_width$}  {}  {:<location_width$}  {:<version_width$}  {}\n",
            row.dependency,
            status,
            row.location,
            row.version,
            requirement,
            dependency_width = widths[0],
            location_width = widths[2],
            version_width = widths[3],
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use debversion::Version;
    use indoc::indoc;

    use super::*;

    #[test]
    /// Reads dependency requirements after applying staged quilt patches.
    fn reads_patched_dependency_metadata() {
        let stage = tempfile::tempdir().unwrap();
        let output = stage.path().join("output");
        fs::create_dir_all(output.join("src")).unwrap();
        fs::create_dir_all(output.join("debian/patches")).unwrap();
        fs::write(
            output.join("Cargo.toml"),
            indoc! {r#"
                [package]
                name = "example"
                version = "1.0.0"
                edition = "2024"

                [dependencies]
                serde = "1"
            "#},
        )
        .unwrap();
        fs::write(output.join("src/lib.rs"), "").unwrap();
        fs::write(
            output.join("debian/control"),
            indoc! {r"
                Source: rust-example
                Build-Depends: librust-serde-dev (>= 2)
            "},
        )
        .unwrap();
        fs::write(
            output.join("debian/patches/series"),
            indoc! {r"
                version.patch
            "},
        )
        .unwrap();
        fs::write(
            output.join("debian/patches/version.patch"),
            indoc! {r#"
                --- a/Cargo.toml
                +++ b/Cargo.toml
                @@ -7 +7 @@
                -serde = "1"
                +serde = "2"
            "#},
        )
        .unwrap();

        let dependencies = read_staged_dependencies(&output, "amd64").unwrap();

        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].name, "serde");
        assert_eq!(dependencies[0].cargo_requirement, "^2");
    }

    /// Creates one candidate with a set of versioned virtual packages.
    fn candidate(version: &str, location: &str, provides: &[&str]) -> PackageCandidate {
        let version: Version = version.parse().unwrap();
        let mut provided = BTreeMap::new();
        for name in provides {
            provided.insert((*name).to_owned(), Some(version.clone()));
        }
        PackageCandidate {
            version,
            provides: provided,
            location: location.to_owned(),
        }
    }

    #[test]
    /// Selects the newest satisfying candidate and retains older alternatives.
    fn classifies_candidates() {
        let dependencies = [
            Dependency {
                name: "serde".to_owned(),
                cargo_requirement: "^1".to_owned(),
                debian_requirements: BTreeMap::from([(
                    Some("derive".to_owned()),
                    vec![vec![PackageRequirement {
                        name: "librust-serde-1+derive-dev".to_owned(),
                        version: None,
                    }]],
                )]),
            },
            Dependency {
                name: "serde".to_owned(),
                cargo_requirement: "^2".to_owned(),
                debian_requirements: BTreeMap::from([(
                    None,
                    vec![vec![PackageRequirement {
                        name: "librust-serde-2-dev".to_owned(),
                        version: None,
                    }]],
                )]),
            },
            Dependency {
                name: "missing".to_owned(),
                cargo_requirement: "^1".to_owned(),
                debian_requirements: BTreeMap::from([(
                    None,
                    vec![vec![PackageRequirement {
                        name: "librust-missing-1-dev".to_owned(),
                        version: None,
                    }]],
                )]),
            },
        ];
        let duplicate = candidate(
            "1.0.219-1",
            "ppa:example/rust-staging (noble)",
            &["librust-serde-1+derive-dev"],
        );
        let candidates = [
            // A duplicate display identity must not hide a satisfying candidate.
            candidate(
                "1.0.219-1",
                "ppa:example/rust-staging (noble)",
                &["librust-serde-1-dev"],
            ),
            candidate(
                "1.0.219-1",
                "ppa:example/rust-staging (noble)",
                &["librust-serde-1+derive-dev"],
            ),
            duplicate,
            candidate(
                "1.0.217-1",
                "noble-updates/universe",
                &["librust-serde-1+derive-dev"],
            ),
        ];
        let rows = classify(&dependencies, &candidates);
        assert_eq!(rows[0].status, "selected");
        assert_eq!(rows[0].version, "1.0.219-1");
        assert_eq!(rows[1].status, "available");
        assert_eq!(rows[1].dependency, "");
        assert!(
            rows[1]
                .requirement
                .iter()
                .all(|part| part.status == RequirementStatus::Satisfied)
        );
        assert_eq!(rows[2].status, "incompatible");
        assert_eq!(rows[3].status, "incompatible");
        assert_eq!(rows[4].status, "missing");
    }

    #[test]
    /// Keeps requirements last and repeats them on continuation rows.
    fn formats_rows() {
        let mut rows = [
            Row {
                dependency: "serde".to_owned(),
                status: "selected",
                location: "noble/universe".to_owned(),
                version: "1.0.219-1".to_owned(),
                requirement: vec![
                    RequirementPart {
                        text: "^1".to_owned(),
                        status: RequirementStatus::Satisfied,
                    },
                    RequirementPart {
                        text: "+derive".to_owned(),
                        status: RequirementStatus::Satisfied,
                    },
                ],
            },
            Row {
                dependency: String::new(),
                status: "available",
                location: "noble-updates/universe".to_owned(),
                version: "1.0.217-1".to_owned(),
                requirement: vec![
                    RequirementPart {
                        text: "^1".to_owned(),
                        status: RequirementStatus::Satisfied,
                    },
                    RequirementPart {
                        text: "+derive".to_owned(),
                        status: RequirementStatus::Satisfied,
                    },
                ],
            },
        ];
        assert_eq!(
            format_table(&rows, false),
            indoc! {r"
                DEPENDENCY  STATUS     LOCATION                VERSION    REQUIREMENT
                serde       selected   noble/universe          1.0.219-1  ^1 +derive
                            available  noble-updates/universe  1.0.217-1  ^1 +derive
            "}
        );
        let colored = format_table(&rows, true);
        assert!(colored.contains("\x1b[32mselected \x1b[0m"));
        assert!(colored.contains("\x1b[90mavailable\x1b[0m"));
        rows[0].requirement[0].status = RequirementStatus::Incompatible;
        rows[0].requirement[1].status = RequirementStatus::Missing;
        assert!(format_table(&rows, true).contains("\x1b[33m^1\x1b[0m \x1b[31m+derive\x1b[0m"));
    }

    #[test]
    /// Classifies each requirement component according to its matching package relation.
    fn classifies_requirement_components() {
        let dependency = Dependency {
            name: "serde".to_owned(),
            cargo_requirement: "^1".to_owned(),
            debian_requirements: BTreeMap::from([
                (
                    None,
                    vec![vec![PackageRequirement {
                        name: "librust-serde-1-dev".to_owned(),
                        version: None,
                    }]],
                ),
                (
                    Some("alloc".to_owned()),
                    vec![vec![PackageRequirement {
                        name: "librust-serde-1+alloc-dev".to_owned(),
                        version: None,
                    }]],
                ),
                (
                    Some("derive".to_owned()),
                    vec![vec![PackageRequirement {
                        name: "librust-serde-1+derive-dev".to_owned(),
                        version: Some((
                            VersionConstraint::GreaterThanEqual,
                            "1.0.200-~~".parse().unwrap(),
                        )),
                    }]],
                ),
                (
                    Some("std".to_owned()),
                    vec![vec![PackageRequirement {
                        name: "librust-serde-1+std-dev".to_owned(),
                        version: None,
                    }]],
                ),
            ]),
        };
        let mut candidate = candidate(
            "1.0.219-1",
            "noble/universe",
            &["librust-serde-1-dev", "librust-serde-0.9+std-dev"],
        );
        candidate.provides.insert(
            "librust-serde-1+derive-dev".to_owned(),
            Some("1.0.100-1".parse().unwrap()),
        );
        let parts = make_requirement(&dependency, Some(&candidate));
        for (part, (text, status)) in parts.iter().zip([
            ("^1", RequirementStatus::Satisfied),
            ("-default", RequirementStatus::Satisfied),
            ("+alloc", RequirementStatus::Missing),
            ("+derive", RequirementStatus::Incompatible),
            ("+std", RequirementStatus::Incompatible),
        ]) {
            assert_eq!((part.text.as_str(), part.status), (text, status));
        }
        assert_eq!(parts.len(), 5);
    }

    #[test]
    /// Anchors version coloring to hidden defaults and copies it to `-default`.
    fn colors_implicit_default_markers() {
        let default_dependency = Dependency {
            name: "foo".to_owned(),
            cargo_requirement: "^1".to_owned(),
            debian_requirements: BTreeMap::from([
                (
                    Some("default".to_owned()),
                    vec![
                        vec![PackageRequirement {
                            name: "librust-foo-1+default-dev".to_owned(),
                            version: Some((
                                VersionConstraint::GreaterThanEqual,
                                "1.0.0".parse().unwrap(),
                            )),
                        }],
                        vec![PackageRequirement {
                            name: "librust-foo-1+default-dev".to_owned(),
                            version: Some((VersionConstraint::LessThan, "2.0.0".parse().unwrap())),
                        }],
                    ],
                ),
                (
                    Some("special".to_owned()),
                    vec![vec![PackageRequirement {
                        name: "librust-foo-1+special-dev".to_owned(),
                        version: None,
                    }]],
                ),
            ]),
        };
        let default_parts = make_requirement(
            &default_dependency,
            Some(&candidate(
                "2.0.0",
                "noble/universe",
                &["librust-foo-1+default-dev", "librust-foo-1+special-dev"],
            )),
        );
        assert_eq!(default_parts[0].status, RequirementStatus::Incompatible);
        assert_eq!(default_parts[1].status, RequirementStatus::Satisfied);

        let no_default_dependency = Dependency {
            name: "foo".to_owned(),
            cargo_requirement: "^0.3".to_owned(),
            debian_requirements: BTreeMap::from([(
                Some("formatting".to_owned()),
                vec![vec![PackageRequirement {
                    name: "librust-foo-0.3+formatting-dev".to_owned(),
                    version: None,
                }]],
            )]),
        };
        let no_default_parts = make_requirement(
            &no_default_dependency,
            Some(&candidate(
                "0.2.0",
                "noble/universe",
                &["librust-foo-0.2+formatting-dev"],
            )),
        );
        assert_eq!(no_default_parts[0].status, RequirementStatus::Incompatible);
        assert_eq!(no_default_parts[1].status, no_default_parts[0].status);
    }

    #[test]
    /// Requires every bound attached to a displayed feature.
    fn checks_all_feature_bounds() {
        let dependency = Dependency {
            name: "foo".to_owned(),
            cargo_requirement: "^1".to_owned(),
            debian_requirements: BTreeMap::from([
                (
                    Some("default".to_owned()),
                    vec![vec![PackageRequirement {
                        name: "librust-foo-1+default-dev".to_owned(),
                        version: None,
                    }]],
                ),
                (
                    Some("special".to_owned()),
                    vec![
                        vec![PackageRequirement {
                            name: "librust-foo-1+special-dev".to_owned(),
                            version: Some((
                                VersionConstraint::GreaterThanEqual,
                                "1.0.0".parse().unwrap(),
                            )),
                        }],
                        vec![PackageRequirement {
                            name: "librust-foo-1+special-dev".to_owned(),
                            version: Some((VersionConstraint::LessThan, "1.5.0".parse().unwrap())),
                        }],
                    ],
                ),
            ]),
        };
        let parts = make_requirement(
            &dependency,
            Some(&candidate(
                "1.5.0",
                "noble/universe",
                &["librust-foo-1+default-dev", "librust-foo-1+special-dev"],
            )),
        );

        assert_eq!(parts[0].status, RequirementStatus::Satisfied);
        assert_eq!(parts[1].status, RequirementStatus::Incompatible);
    }

    #[test]
    /// Applies Debian version constraints to versioned virtual packages.
    fn checks_candidate_constraints() {
        let requirement = PackageRequirement {
            name: "librust-serde-1-dev".to_owned(),
            version: Some((
                VersionConstraint::GreaterThanEqual,
                "1.0.200-~~".parse().unwrap(),
            )),
        };
        assert!(satisfies_package(
            &requirement,
            &candidate("1.0.219-1", "noble/universe", &["librust-serde-1-dev"])
        ));

        let unversioned = PackageCandidate {
            version: "1.0.219-1".parse().unwrap(),
            provides: BTreeMap::from([("librust-serde-1-dev".to_owned(), None)]),
            location: "noble/universe".to_owned(),
        };
        let unversioned_requirement = PackageRequirement {
            name: "librust-serde-1-dev".to_owned(),
            version: None,
        };
        assert!(satisfies_package(&unversioned_requirement, &unversioned));
    }
}
