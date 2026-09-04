//! Inspects Ubuntu binary package candidates for direct Rust dependencies.

mod apt;
mod control;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    io::{self, IsTerminal},
    path::Path,
};

use anyhow::Result;
use debian_control::relations::VersionConstraint;

use self::{
    apt::PackageCandidate,
    control::{Dependency, PackageRequirement, parse_rust_package_name},
};

const GREEN: &str = "\x1b[32m";
const GRAY: &str = "\x1b[90m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

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
pub fn run(
    crate_name: Option<&str>,
    version: Option<&str>,
    package_dir: Option<&Path>,
    series: &str,
    proposed: bool,
    ppas: &[String],
    architecture: Option<&str>,
) -> Result<bool> {
    let architecture = match architecture {
        Some(architecture) => architecture.to_owned(),
        None => apt::read_architecture()?,
    };
    let inspection =
        crate::package::stage_for_dependency_inspection(crate_name, version, package_dir)?;
    let dependencies = control::read_dependencies(
        &inspection.stage.path().join("output/debian/control"),
        &architecture,
        &inspection.cargo_dependencies,
    )?;
    let candidates = apt::load_candidates(series, &architecture, proposed, ppas)?;
    let rows = classify(&dependencies, &candidates);
    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    print!("{}", format_table(&rows, color));
    Ok(rows
        .iter()
        .any(|row| matches!(row.status, "incompatible" | "missing")))
}

/// Classifies all candidates for each dependency in deterministic order.
fn classify(dependencies: &[Dependency], candidates: &[PackageCandidate]) -> Vec<Row> {
    let mut candidates_by_crate: BTreeMap<String, Vec<&PackageCandidate>> = BTreeMap::new();
    for candidate in candidates {
        let mut crate_names = BTreeSet::new();
        for provided in candidate.provides.keys() {
            if let Some((name, _, _)) = parse_rust_package_name(provided) {
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
            .get(&dependency.name)
            .cloned()
            .unwrap_or_default();
        matching.sort_by(|first, second| second.version.cmp(&first.version));
        let mut satisfying: Vec<_> = matching
            .iter()
            .copied()
            .filter(|candidate| satisfies(dependency, candidate))
            .collect();
        let mut displayed = BTreeSet::new();
        satisfying.retain(|candidate| {
            displayed.insert((candidate.version.clone(), candidate.location.clone()))
        });
        if !satisfying.is_empty() {
            for (index, candidate) in satisfying.into_iter().enumerate() {
                rows.push(make_row(
                    dependency,
                    candidate,
                    if index == 0 { "selected" } else { "available" },
                    index == 0,
                ));
            }
        } else if !matching.is_empty() {
            let mut displayed = BTreeSet::new();
            matching.retain(|candidate| {
                displayed.insert((candidate.version.clone(), candidate.location.clone()))
            });
            for (index, candidate) in matching.into_iter().enumerate() {
                rows.push(make_row(dependency, candidate, "incompatible", index == 0));
            }
        } else {
            rows.push(Row {
                dependency: dependency.name.clone(),
                status: "missing",
                location: "-".to_owned(),
                version: "-".to_owned(),
                requirement: make_requirement(dependency, None),
            });
        }
    }
    rows
}

/// Creates one report row from a dependency and candidate.
fn make_row(
    dependency: &Dependency,
    candidate: &PackageCandidate,
    status: &'static str,
    first: bool,
) -> Row {
    Row {
        dependency: if first {
            dependency.name.clone()
        } else {
            String::new()
        },
        status,
        location: candidate.location.clone(),
        version: candidate.version.to_string(),
        requirement: make_requirement(dependency, Some(candidate)),
    }
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
        let Some((name, _, provided_feature)) = parse_rust_package_name(provided) else {
            continue;
        };
        let related = match kind {
            RequirementKind::Any => true,
            RequirementKind::Base => provided_feature.is_none(),
            RequirementKind::Feature(feature) => provided_feature.as_deref() == Some(feature),
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
    let Some(actual_version) = candidate.provided_version(&requirement.name) else {
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
    use std::collections::BTreeMap;

    use debversion::Version;

    use super::*;

    /// Creates one candidate with a set of versioned virtual packages.
    fn candidate(version: &str, location: &str, provides: &[&str]) -> PackageCandidate {
        let version: Version = version.parse().unwrap();
        let mut provided = BTreeMap::new();
        for name in provides {
            provided.insert((*name).to_owned(), Some(version.clone()));
        }
        PackageCandidate {
            source: "rust-serde".to_owned(),
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
        let mut duplicate = candidate(
            "1.0.219-1",
            "ppa:example/rust-staging (noble)",
            &["librust-serde-1+derive-dev"],
        );
        duplicate.source = "rust-serde-1".to_owned();
        let candidates = [
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
    /// Keeps the requirement last and blank on continuation rows.
    fn formats_rows() {
        let rows = [
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
            concat!(
                "DEPENDENCY  STATUS     LOCATION                VERSION    REQUIREMENT\n",
                "serde       selected   noble/universe          1.0.219-1  ^1 +derive\n",
                "            available  noble-updates/universe  1.0.217-1  ^1 +derive\n",
            )
        );
        assert_eq!(
            format_table(&rows, true),
            concat!(
                "DEPENDENCY  STATUS     LOCATION                VERSION    REQUIREMENT\n",
                "serde       \x1b[32mselected \x1b[0m  noble/universe          1.0.219-1  ^1 +derive\n",
                "            \x1b[90mavailable\x1b[0m  noble-updates/universe  1.0.217-1  ^1 +derive\n",
            )
        );
    }

    #[test]
    /// Colors each requirement component according to its matching package relation.
    fn colors_requirement_components() {
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
        let row = make_row(&dependency, &candidate, "incompatible", true);

        assert_eq!(
            format_table(&[row], true),
            concat!(
                "DEPENDENCY  STATUS        LOCATION        VERSION    REQUIREMENT\n",
                "serde       \x1b[33mincompatible\x1b[0m  noble/universe  1.0.219-1  ",
                "^1 -default \x1b[31m+alloc\x1b[0m ",
                "\x1b[33m+derive\x1b[0m \x1b[33m+std\x1b[0m\n",
            )
        );
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
        let default_row = make_row(
            &default_dependency,
            &candidate(
                "2.0.0",
                "noble/universe",
                &["librust-foo-1+default-dev", "librust-foo-1+special-dev"],
            ),
            "incompatible",
            true,
        );
        assert_eq!(
            default_row.requirement[0].status,
            RequirementStatus::Incompatible
        );
        assert_eq!(
            default_row.requirement[1].status,
            RequirementStatus::Satisfied
        );

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
        let no_default_row = make_row(
            &no_default_dependency,
            &candidate(
                "0.2.0",
                "noble/universe",
                &["librust-foo-0.2+formatting-dev"],
            ),
            "incompatible",
            true,
        );
        assert_eq!(
            no_default_row.requirement[0].status,
            RequirementStatus::Incompatible
        );
        assert_eq!(
            no_default_row.requirement[1].status,
            no_default_row.requirement[0].status
        );
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
        let row = make_row(
            &dependency,
            &candidate(
                "1.5.0",
                "noble/universe",
                &["librust-foo-1+default-dev", "librust-foo-1+special-dev"],
            ),
            "incompatible",
            true,
        );

        assert_eq!(row.requirement[0].status, RequirementStatus::Satisfied);
        assert_eq!(row.requirement[1].status, RequirementStatus::Incompatible);
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
            source: "rust-serde".to_owned(),
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
