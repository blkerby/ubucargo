//! Inspects Ubuntu binary package candidates for direct Rust dependencies.

mod apt;
mod control;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::Result;
use debian_control::relations::VersionConstraint;

use self::{
    apt::PackageCandidate,
    control::{Dependency, PackageRequirement, parse_rust_package_name},
};

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
    /// Cargo-style requirement, omitted on continuation rows.
    requirement: String,
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
    let stage = crate::package::stage_for_dependency_inspection(crate_name, version, package_dir)?;
    let dependencies =
        control::read_dependencies(&stage.path().join("output/debian/control"), &architecture)?;
    let candidates = apt::load_candidates(series, &architecture, proposed, ppas)?;
    eprintln!("Processing dependencies");
    let rows = classify(&dependencies, &candidates);
    print!("{}", format_table(&rows));
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
                requirement: dependency.requirement.clone(),
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
        requirement: if first {
            dependency.requirement.clone()
        } else {
            String::new()
        },
    }
}

/// Reports whether one binary package satisfies every entry for a dependency.
fn satisfies(dependency: &Dependency, candidate: &PackageCandidate) -> bool {
    for alternatives in &dependency.entries {
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
fn format_table(rows: &[Row]) -> String {
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
        output.push_str(&format!(
            "{:<dependency_width$}  {:<status_width$}  {:<location_width$}  {:<version_width$}  {}\n",
            row.dependency,
            row.status,
            row.location,
            row.version,
            row.requirement,
            dependency_width = widths[0],
            status_width = widths[1],
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
                requirement: "^1 +derive".to_owned(),
                entries: vec![vec![PackageRequirement {
                    name: "librust-serde-1+derive-dev".to_owned(),
                    version: None,
                }]],
            },
            Dependency {
                name: "serde".to_owned(),
                requirement: "^2".to_owned(),
                entries: vec![vec![PackageRequirement {
                    name: "librust-serde-2-dev".to_owned(),
                    version: None,
                }]],
            },
            Dependency {
                name: "missing".to_owned(),
                requirement: "^1".to_owned(),
                entries: vec![vec![PackageRequirement {
                    name: "librust-missing-1-dev".to_owned(),
                    version: None,
                }]],
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
                requirement: "^1 +derive".to_owned(),
            },
            Row {
                dependency: String::new(),
                status: "available",
                location: "noble-updates/universe".to_owned(),
                version: "1.0.217-1".to_owned(),
                requirement: String::new(),
            },
        ];
        assert_eq!(
            format_table(&rows),
            concat!(
                "DEPENDENCY  STATUS     LOCATION                VERSION    REQUIREMENT\n",
                "serde       selected   noble/universe          1.0.219-1  ^1 +derive\n",
                "            available  noble-updates/universe  1.0.217-1  \n",
            )
        );
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
