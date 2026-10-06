//! Inspects Ubuntu binary package candidates for direct Rust dependencies.

mod apt;
mod control;
mod latest;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    io::{self, IsTerminal},
};

use anyhow::{Context, Result};
use debian_control::relations::VersionConstraint;

use crate::{
    generate,
    input::{Input, parse_input, validate_version},
    resolve,
};

use self::{
    apt::PackageCandidate,
    control::{DependencyOrigin, DependencySection, PackageRequirement, parse_rust_package_name},
};

const GREEN: &str = "\x1b[32m";
const GRAY: &str = "\x1b[90m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const RESET: &str = "\x1b[0m";

/// Inspect Ubuntu candidates for a crate's direct Rust dependencies.
#[derive(clap::Args)]
pub struct DepArgs {
    /// Input selector: crate:NAME, archive:SERIES/SOURCE, ppa:OWNER/NAME/SOURCE, pkg:PATH, or local:PATH.
    #[arg(value_name = "INPUT")]
    pub input: Option<String>,

    /// Exact Cargo or Debian source version; local inputs reject versions.
    #[arg(value_name = "VERSION", requires = "input")]
    pub version: Option<String>,

    /// Checking series; defaults to the Archive input series, otherwise the current Ubuntu development series.
    #[arg(long, value_name = "SERIES")]
    pub series: Option<String>,

    /// Include the Ubuntu proposed pocket.
    #[arg(long)]
    pub proposed: bool,

    /// Public Launchpad PPA to include.
    #[arg(long, value_name = "ppa:OWNER/NAME")]
    pub ppa: Vec<String>,

    /// Debian architecture; defaults to dpkg --print-architecture.
    #[arg(long, value_name = "ARCH")]
    pub architecture: Option<String>,

    /// Retain generated or published-input staging, including on failure.
    #[arg(long)]
    pub keep_staging: bool,
}

/// Debian requirements grouped by Cargo feature, with `None` for the base crate.
/// Every relation in a vector is required (comma-separated AND).
type FeatureRequirements = BTreeMap<Option<String>, Vec<PackageRequirement>>;

/// Debian requirements belonging to one semver line of one Rust crate.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Dependency {
    /// Normalized Cargo crate name.
    name: String,
    /// Debian package requirements used for availability and feature checks.
    debian_requirements: FeatureRequirements,
}

/// One display table after grouping requirements and suppressing covered relations.
#[derive(Debug, Eq, PartialEq)]
struct DependencyTable {
    /// Formatted heading naming the declaring section.
    heading: String,
    /// Dependencies in crate and semver-line order.
    dependencies: Vec<Dependency>,
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

/// One independently colored component of a compact Debian requirement.
#[derive(Debug, Eq, PartialEq)]
struct RequirementPart {
    /// Semver expression or feature name, including its `+` prefix.
    text: String,
    /// Candidate availability for this component.
    status: RequirementStatus,
}

/// Feature identity used to recognize a related package with the wrong relation.
enum RequirementKind<'a> {
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
    /// Version and feature requirement components classified for this candidate.
    requirement: Vec<RequirementPart>,
}

/// Reads existing packaging or stages a crate, then reports whether APT satisfies its dependencies.
pub fn run(args: DepArgs) -> Result<bool> {
    let current = env::current_dir().context("get current directory")?;
    let input = if let Some(value) = &args.input {
        parse_input(value, &current)?
    } else {
        Input::Package(
            resolve::find_parent_package(&current.canonicalize()?)
                .context("not inside a source package; supply INPUT")?,
        )
    };
    validate_version(&input, args.version.as_deref())?;
    let default_series;
    let series = if let Some(series) = args.series.as_deref() {
        series
    } else if let Input::Archive { series, .. } = &input {
        series.as_str()
    } else {
        default_series = apt::read_development_series()?;
        &default_series
    };
    let local_changelog = match &input {
        Input::Package(root) => Some(crate::changelog::read_top_changelog(
            &root.join("debian/changelog"),
        )?),
        _ => None,
    };
    let architecture = match args.architecture {
        Some(architecture) => architecture,
        None => apt::read_architecture()?,
    };
    let mut ppas = BTreeSet::new();
    for ppa in args.ppa {
        ppas.insert(ppa);
    }
    if let Input::Ppa { ppa, .. } = &input {
        ppas.insert(ppa.clone());
    }
    let ppas: Vec<_> = ppas.into_iter().collect();
    let mut input_records = None;
    if let Input::Archive {
        series: input_series,
        ..
    } = &input
    {
        if input_series != series {
            input_records = Some(apt::load_records(
                input_series,
                &architecture,
                args.proposed,
                &[],
            )?);
        }
    }
    let records = apt::load_records(series, &architecture, args.proposed, &ppas)?;
    let (sections, header, mut identity) = match &input {
        Input::Crate(_) | Input::Local(_) => {
            let (name, local) = match &input {
                Input::Crate(name) => (Some(name.as_str()), None),
                Input::Local(path) => (None, Some(path.as_path())),
                _ => unreachable!(),
            };
            let resolved =
                resolve::resolve_package(None, None, name, args.version.as_deref(), local)?;
            let location = match &input {
                Input::Local(_) => args.input.as_deref().unwrap(),
                _ => "crates.io",
            };
            let header = format!(
                "Input: {} {} from {location} (generated packaging)",
                resolved.crate_selection.crate_name, resolved.crate_selection.version
            );
            let generated = generate::generate_package(&resolved, args.keep_staging)?;
            (
                control::read_dependency_sections(&generated.source, &architecture)?,
                header,
                latest::InputIdentity {
                    crate_name: Some(resolved.crate_selection.crate_name.clone()),
                    source_name: resolved.source_name.clone(),
                    version: resolved.crate_selection.version.clone(),
                },
            )
        }
        Input::Package(root) => {
            let top = local_changelog.as_ref().unwrap();
            let location = match args.input.as_deref() {
                Some(value) if value.starts_with("pkg:") => value.to_owned(),
                Some(value) => format!("pkg:{value}"),
                None => format!("pkg:{}", root.display()),
            };
            (
                control::read_dependency_sections(root, &architecture)?,
                format!("Input: {} {} from {location}", top.source, top.version),
                latest::InputIdentity {
                    crate_name: None,
                    source_name: top.source.clone(),
                    version: top.version.clone(),
                },
            )
        }
        Input::Archive { source, .. } | Input::Ppa { source, .. } => {
            let ppa = match &input {
                Input::Ppa { ppa, .. } => Some(ppa.as_str()),
                _ => None,
            };
            let source = apt::select_source(
                &input_records.as_ref().unwrap_or(&records).sources,
                source,
                ppa,
                args.version.as_deref(),
            )?;
            let (_stage, root) = apt::retrieve_source(source, ppa, args.keep_staging)?;
            (
                control::read_dependency_sections(&root, &architecture)?,
                format!(
                    "Input: {} {} from {}",
                    source.source, source.version, source.location
                ),
                latest::InputIdentity {
                    crate_name: None,
                    source_name: source.source.clone(),
                    version: source.version.to_string(),
                },
            )
        }
    };
    if identity.crate_name.is_none() {
        for section in &sections {
            if section.origin == DependencyOrigin::Package {
                if let Some((name, _, _)) = parse_rust_package_name(&section.name) {
                    identity.crate_name = Some(name.to_owned());
                    break;
                }
            }
        }
        if identity.crate_name.is_none() {
            identity.crate_name = latest::infer_crate_name(&identity.source_name);
        }
    }
    let release = if matches!(input, Input::Crate(_)) && args.version.is_none() {
        None
    } else {
        Some(latest::read_latest_release(identity.crate_name.as_deref()))
    };
    let latest = latest::format_latest(
        &input,
        args.version.is_some(),
        &identity,
        series,
        &records.sources,
        release,
    );
    println!("{header}\n{latest}");
    let tables = prepare_tables(sections);
    let candidates = records.packages;
    let mut classified = Vec::new();
    let mut unsatisfied = false;
    for table in &tables {
        let rows = classify(&table.dependencies, &candidates);
        for row in &rows {
            if matches!(row.status, "incompatible" | "missing") {
                unsatisfied = true;
            }
        }
        classified.push((table, rows));
    }
    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    print!("{}", format_tables(&classified, color));
    Ok(unsatisfied)
}

/// Prepares display tables, retaining all binary relations and only new source/test relations.
fn prepare_tables(sections: Vec<DependencySection>) -> Vec<DependencyTable> {
    let mut seen = BTreeSet::new();
    let mut tables = Vec::new();
    for section in sections {
        let mut shown = Vec::new();
        for requirement in section.requirements {
            if seen.insert(requirement.clone()) || section.origin == DependencyOrigin::Package {
                shown.push(requirement);
            }
        }
        let dependencies = group_dependencies(shown);
        if !dependencies.is_empty() {
            let label = match section.origin {
                DependencyOrigin::Package => "Package",
                DependencyOrigin::Source => "Source",
                DependencyOrigin::Tests => "Tests",
            };
            tables.push(DependencyTable {
                heading: format!("{label}: {}", section.name),
                dependencies,
            });
        }
    }
    tables
}

/// Groups Rust relations by crate and semver line, then by feature, dropping duplicates.
fn group_dependencies(requirements: Vec<PackageRequirement>) -> Vec<Dependency> {
    let mut grouped: BTreeMap<(String, Option<String>), FeatureRequirements> = BTreeMap::new();
    for requirement in requirements {
        // The collector keeps only names that parse as Rust packages.
        let (name, line, feature) = parse_rust_package_name(&requirement.name).unwrap();
        let relations = grouped
            .entry((name.to_owned(), line.map(str::to_owned)))
            .or_default()
            .entry(feature.map(str::to_owned))
            .or_default();
        if !relations.contains(&requirement) {
            relations.push(requirement);
        }
    }
    let mut dependencies = Vec::new();
    for ((name, _), debian_requirements) in grouped {
        dependencies.push(Dependency {
            name,
            debian_requirements,
        });
    }
    dependencies
}

/// Classifies all candidates for each dependency in deterministic order.
fn classify(dependencies: &[Dependency], candidates: &[PackageCandidate]) -> Vec<Row> {
    let mut candidates_by_crate: BTreeMap<&str, Vec<&PackageCandidate>> = BTreeMap::new();
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

/// Factors identical Debian version expressions out of feature display components.
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
        requirements.extend_from_slice(default);
        if let Some(base) = base {
            requirements.extend_from_slice(base);
        }
        RequirementKind::Feature("default")
    } else if let Some(base) = base {
        requirements.extend_from_slice(base);
        RequirementKind::Base
    } else {
        // Debcargo feature packages depend on the bare library from the same source,
        // so feature-only relations imply the same relations on the bare library.
        for feature in dependency.debian_requirements.values() {
            for requirement in feature {
                // The collector has already validated the crate and feature identity.
                let body = requirement.name.strip_suffix("-dev").unwrap();
                let base = body.split('+').next().unwrap();
                requirements.push(PackageRequirement {
                    name: format!("{base}-dev"),
                    version: requirement.version.clone(),
                });
            }
        }
        RequirementKind::Base
    };
    let base_version_status =
        classify_requirement(&requirements, candidate, &dependency.name, kind);
    let base_version_text = format_version_requirements(&dependency.name, &requirements);
    output.push(RequirementPart {
        text: base_version_text.clone(),
        status: base_version_status,
    });

    if default.is_none() {
        output.push(RequirementPart {
            text: "-default".to_owned(),
            status: base_version_status,
        });
    }
    for (feature, feature_requirements) in &dependency.debian_requirements {
        let Some(feature) = feature else {
            continue;
        };
        if feature == "default" {
            continue;
        }
        let feature_version_text =
            format_version_requirements(&dependency.name, feature_requirements);
        output.push(RequirementPart {
            text: if feature_version_text == base_version_text {
                format!("+{feature}")
            } else {
                format!("+{feature}({feature_version_text})")
            },
            status: classify_requirement(
                feature_requirements,
                candidate,
                &dependency.name,
                RequirementKind::Feature(feature),
            ),
        });
    }
    output
}

/// Formats package-name version suffixes and Debian bounds as a comma-separated AND list.
fn format_version_requirements(crate_name: &str, requirements: &[PackageRequirement]) -> String {
    let prefix = format!("librust-{crate_name}");
    let mut relations = BTreeSet::new();
    let mut lines = BTreeSet::new();
    for requirement in requirements {
        // The collector has already validated the crate and feature identity.
        let body = requirement.name.strip_suffix("-dev").unwrap();
        let base = body.split('+').next().unwrap();
        let line = base
            .strip_prefix(&prefix)
            .unwrap()
            .strip_prefix('-')
            .unwrap_or("*");
        let bound = match &requirement.version {
            Some((constraint, version)) => format!("{constraint}{version}"),
            None => String::new(),
        };
        lines.insert(line);
        relations.insert((line, bound));
    }
    if let [line] = Vec::from_iter(lines).as_slice() {
        // Factor out the shared line; an unbounded relation adds nothing beyond it.
        let mut bounds = Vec::new();
        for (_, bound) in relations {
            if !bound.is_empty() {
                bounds.push(bound);
            }
        }
        if bounds.is_empty() {
            return (*line).to_owned();
        }
        return format!("{line} ({})", bounds.join(", "));
    }
    let mut expressions = Vec::new();
    for (line, bound) in relations {
        if bound.is_empty() {
            expressions.push(line.to_owned());
        } else {
            expressions.push(format!("{line} ({bound})"));
        }
    }
    expressions.join(", ")
}

/// Classifies one visible requirement component against a package candidate.
fn classify_requirement(
    requirements: &[PackageRequirement],
    candidate: Option<&PackageCandidate>,
    crate_name: &str,
    kind: RequirementKind<'_>,
) -> RequirementStatus {
    let Some(candidate) = candidate else {
        return RequirementStatus::Missing;
    };
    if requirements
        .iter()
        .all(|requirement| satisfies_package(requirement, candidate))
    {
        return RequirementStatus::Satisfied;
    }
    for provided in candidate.provides.keys() {
        let Some((name, _, provided_feature)) = parse_rust_package_name(provided) else {
            continue;
        };
        let related = match kind {
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
    dependency.debian_requirements.values().all(|feature| {
        feature
            .iter()
            .all(|requirement| satisfies_package(requirement, candidate))
    })
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

/// Formats one headed, unbordered table per dependency table, separated by blank lines.
/// Columns are aligned across all tables.
/// The last column is not padded, so that an outlier long field
/// avoids making the entire output too wide.
fn format_tables(tables: &[(&DependencyTable, Vec<Row>)], color: bool) -> String {
    let headers = ["DEPENDENCY", "STATUS", "LOCATION", "VERSION"];
    let mut widths = headers.map(str::len);
    for (_, rows) in tables {
        for row in rows {
            widths[0] = widths[0].max(row.dependency.len());
            widths[1] = widths[1].max(row.status.len());
            widths[2] = widths[2].max(row.location.len());
            widths[3] = widths[3].max(row.version.len());
        }
    }
    let header = format!(
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

    let mut output = String::new();
    for (index, (table, rows)) in tables.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        if color {
            output.push_str(BOLD_CYAN);
        }
        output.push_str(&table.heading);
        if color {
            output.push_str(RESET);
        }
        output.push('\n');
        output.push_str(&header);
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
    }
    output
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use debversion::Version;
    use indoc::indoc;

    use super::*;

    /// Creates one candidate with a set of versioned virtual packages.
    fn candidate(version: &str, location: &str, provides: &[&str]) -> PackageCandidate {
        let version: Version = version.parse().unwrap();
        let mut provided = BTreeMap::new();
        for name in provides {
            provided.insert((*name).to_owned(), Some(version.clone()));
        }
        PackageCandidate {
            source: "rust-example".to_owned(),
            version,
            provides: provided,
            location: location.to_owned(),
        }
    }

    #[test]
    /// Reads existing controls despite unusable generation metadata, without changing the package.
    fn reads_existing_package_unchanged() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian/tests")).unwrap();
        fs::create_dir(root.path().join(".pc")).unwrap();
        let control = "Source: rust-example\nBuild-Depends: librust-serde-1-dev\n";
        let tests = "Depends: librust-cmake-dev, librust-fs-extra-dev\n";
        for (path, contents) in [
            ("Cargo.toml", "invalid Cargo metadata"),
            ("debian/debcargo.toml", "invalid debcargo configuration"),
            (
                "debian/changelog",
                "rust-example (1.0-1) noble; urgency=medium\n\n  * Example.\n\n -- Example <example@example.com>  Tue, 06 Oct 2026 12:00:00 +0000\n",
            ),
            ("debian/ubucargo-state.json", "invalid ownership manifest"),
            ("debian/control", control),
            ("debian/tests/control", tests),
            (
                "debian/tests/control.debcargo.hint",
                "Depends: librust-olm-rs-2-dev\n",
            ),
            (".pc/applied-patches", "unrefreshed.patch\n"),
        ] {
            fs::write(root.path().join(path), contents).unwrap();
        }
        let nested = root.path().join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            resolve::find_parent_package(&nested),
            Some(root.path().to_path_buf())
        );
        let relative = pathdiff::diff_paths(root.path(), env::current_dir().unwrap()).unwrap();
        for directory in [root.path().to_path_buf(), relative] {
            // Invalid repository arguments must fail before generation or filesystem changes.
            // The direct reader below verifies maintained requirements independently.
            let error = run(DepArgs {
                input: Some(format!("pkg:{}", directory.display())),
                version: None,
                series: Some("invalid/series".to_owned()),
                proposed: false,
                ppa: Vec::new(),
                architecture: Some("amd64".to_owned()),
                keep_staging: true,
            })
            .unwrap_err();
            assert!(error.to_string().contains("invalid series"), "{error:#}");
        }
        let sections = control::read_dependency_sections(root.path(), "amd64").unwrap();
        assert_eq!(sections[1].requirements[0].name, "librust-cmake-dev");
        assert_eq!(sections[1].requirements[1].name, "librust-fs-extra-dev");
        assert_eq!(
            fs::read_to_string(root.path().join("debian/control")).unwrap(),
            control
        );
        assert_eq!(
            fs::read_to_string(root.path().join("debian/tests/control")).unwrap(),
            tests
        );
        assert_eq!(
            fs::read_to_string(root.path().join("debian/ubucargo-state.json")).unwrap(),
            "invalid ownership manifest"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("debian/tests/control.debcargo.hint")).unwrap(),
            "Depends: librust-olm-rs-2-dev\n"
        );
    }

    #[test]
    /// Selects the newest satisfying candidate and retains older alternatives.
    fn classifies_candidates() {
        let dependencies = [
            Dependency {
                name: "serde".to_owned(),
                debian_requirements: BTreeMap::from([(
                    Some("derive".to_owned()),
                    vec![PackageRequirement {
                        name: "librust-serde-1+derive-dev".to_owned(),
                        version: None,
                    }],
                )]),
            },
            Dependency {
                name: "serde".to_owned(),
                debian_requirements: BTreeMap::from([(
                    None,
                    vec![PackageRequirement {
                        name: "librust-serde-2-dev".to_owned(),
                        version: None,
                    }],
                )]),
            },
            Dependency {
                name: "missing".to_owned(),
                debian_requirements: BTreeMap::from([(
                    None,
                    vec![PackageRequirement {
                        name: "librust-missing-1-dev".to_owned(),
                        version: None,
                    }],
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
                &["librust-serde-1-dev", "librust-serde-1+derive-dev"],
            ),
            duplicate,
            candidate(
                "1.0.217-1",
                "noble-updates/universe",
                &["librust-serde-1-dev", "librust-serde-1+derive-dev"],
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
    /// Prepares headed tables without repeating covered relations or combining semver lines.
    fn prepares_tables() {
        let control = indoc! {r"
            Source: rust-example
            Build-Depends: debhelper-compat (= 13),
             librust-serde-1+derive-dev (>= 1.0.100-~~),
             librust-criterion-0.5+default-dev,
             librust-disabled-1-dev [arm64]

            Package: librust-example-dev
            Architecture: any
            Depends:
             ${misc:Depends},
             librust-serde-1+derive-dev (>= 1.0.100-~~),
             librust-rand-0.8-dev (>= 0.8.4),
             librust-rand-0.10-dev
            Provides: librust-example-1-dev (= ${binary:Version})
            Description: example

            Package: librust-example+std-dev
            Architecture: any
            Depends:
             ${misc:Depends},
             librust-example-dev (= ${binary:Version}),
             librust-example+alloc-dev (= ${binary:Version})
            Description: example

            Package: librust-example+json-dev
            Architecture: any
            Depends:
             ${misc:Depends},
             librust-example-dev (= ${binary:Version}),
             librust-serde-1+derive-dev (>= 1.0.100-~~),
             librust-serde-json-1+default-dev
            Description: example
        "};
        let tests = indoc! {r"
            Test-Command: /usr/share/cargo/bin/cargo-auto-test example 1.0.0 --all-targets
            Features: test-name=rust-example:@
            Depends: dh-cargo (>= 33~), rustc (>= 1.70), librust-criterion-0.5+default-dev, librust-quickcheck-1+default-dev, @
            Restrictions: allow-stderr
        "};
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("debian/tests")).unwrap();
        std::fs::write(root.path().join("debian/control"), control).unwrap();
        std::fs::write(root.path().join("debian/tests/control"), tests).unwrap();
        let sections = control::read_dependency_sections(root.path(), "amd64").unwrap();
        let tables = prepare_tables(sections);

        let mut summary = Vec::new();
        for table in &tables {
            let mut names = Vec::new();
            for dependency in &table.dependencies {
                names.push(dependency.name.as_str());
            }
            summary.push((table.heading.as_str(), names));
        }
        assert_eq!(
            summary,
            [
                (
                    "Package: librust-example-dev",
                    vec!["rand", "rand", "serde"]
                ),
                (
                    "Package: librust-example+json-dev",
                    vec!["serde", "serde-json"]
                ),
                ("Source: rust-example", vec!["criterion"]),
                ("Tests: rust-example", vec!["quickcheck"]),
            ]
        );
        // Each semver line of a crate is classified independently.
        assert_eq!(
            tables[0].dependencies[0].debian_requirements[&None][0].name,
            "librust-rand-0.10-dev"
        );
        assert_eq!(
            tables[0].dependencies[1].debian_requirements[&None][0].name,
            "librust-rand-0.8-dev"
        );
    }

    #[test]
    /// Heads each table, aligns columns across tables, and keeps requirements last.
    fn formats_tables() {
        let rows = vec![
            Row {
                dependency: "serde".to_owned(),
                status: "selected",
                location: "noble/universe".to_owned(),
                version: "1.0.219-1".to_owned(),
                requirement: vec![
                    RequirementPart {
                        text: "1".to_owned(),
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
                        text: "1".to_owned(),
                        status: RequirementStatus::Satisfied,
                    },
                    RequirementPart {
                        text: "+derive".to_owned(),
                        status: RequirementStatus::Satisfied,
                    },
                ],
            },
        ];
        let base = DependencyTable {
            heading: "Package: librust-example-dev".to_owned(),
            dependencies: Vec::new(),
        };
        let tests = DependencyTable {
            heading: "Tests: rust-example".to_owned(),
            dependencies: Vec::new(),
        };
        let test_rows = vec![Row {
            dependency: "quickcheck".to_owned(),
            status: "missing",
            location: "-".to_owned(),
            version: "-".to_owned(),
            requirement: vec![RequirementPart {
                text: "1".to_owned(),
                status: RequirementStatus::Missing,
            }],
        }];
        assert_eq!(
            format_tables(&[(&base, rows), (&tests, test_rows)], false),
            indoc! {r"
                Package: librust-example-dev
                DEPENDENCY  STATUS     LOCATION                VERSION    REQUIREMENT
                serde       selected   noble/universe          1.0.219-1  1 +derive
                            available  noble-updates/universe  1.0.217-1  1 +derive

                Tests: rust-example
                DEPENDENCY  STATUS     LOCATION                VERSION    REQUIREMENT
                quickcheck  missing    -                       -          1
            "}
        );
    }

    #[test]
    /// Colors every section heading and resets the style before column headers.
    fn colors_headings() {
        for heading in [
            "Package: librust-example-dev",
            "Source: rust-example",
            "Tests: rust-example",
        ] {
            let table = DependencyTable {
                heading: heading.to_owned(),
                dependencies: Vec::new(),
            };
            let tables = [(&table, Vec::new())];
            assert!(
                format_tables(&tables, true)
                    .starts_with(&format!("\x1b[1;36m{heading}\x1b[0m\nDEPENDENCY"))
            );
            let plain = format_tables(&tables, false);
            assert!(plain.starts_with(&format!("{heading}\nDEPENDENCY")));
            assert!(!plain.contains('\x1b'));
        }
    }

    #[test]
    /// Colors statuses and each requirement part independently.
    fn colors_rows() {
        let table = DependencyTable {
            heading: "Package: librust-example-dev".to_owned(),
            dependencies: Vec::new(),
        };
        let mut rows = vec![
            Row {
                dependency: "serde".to_owned(),
                status: "selected",
                location: "noble/universe".to_owned(),
                version: "1.0.219-1".to_owned(),
                requirement: vec![
                    RequirementPart {
                        text: "1".to_owned(),
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
                requirement: Vec::new(),
            },
        ];
        rows[1].requirement.clear();
        let colored = format_tables(&[(&table, rows)], true);
        assert!(colored.contains("\x1b[32mselected \x1b[0m"));
        assert!(colored.contains("\x1b[90mavailable\x1b[0m"));
        let rows = vec![Row {
            dependency: "serde".to_owned(),
            status: "incompatible",
            location: "noble/universe".to_owned(),
            version: "1.0.219-1".to_owned(),
            requirement: vec![
                RequirementPart {
                    text: "1".to_owned(),
                    status: RequirementStatus::Incompatible,
                },
                RequirementPart {
                    text: "+derive".to_owned(),
                    status: RequirementStatus::Missing,
                },
            ],
        }];
        assert!(
            format_tables(&[(&table, rows)], true)
                .contains("\x1b[33m1\x1b[0m \x1b[31m+derive\x1b[0m")
        );
    }

    #[test]
    /// Compacts real control relations without losing feature exceptions.
    fn formats_debian_requirements() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("debian")).unwrap();
        for (relations, expected) in [
            (
                "librust-foo-1+default-dev (>= 1.0.100-~~), librust-foo-1+derive-dev (>= 1.0.100-~~), librust-foo-1+std-dev (>= 1.0.100-~~)",
                "1 (>=1.0.100-~~) +derive +std",
            ),
            (
                "librust-foo-1-dev, librust-foo-1+derive-dev (>= 1.0.200-~~)",
                "1 -default +derive(1 (>=1.0.200-~~))",
            ),
            (
                "librust-foo-1+default-dev (>= 1), librust-foo-1+default-dev (<< 2), librust-foo-1+derive-dev (<< 2), librust-foo-1+derive-dev (>= 1)",
                "1 (<<2, >=1) +derive",
            ),
            (
                "librust-foo-dev (>= 1:1.0-2~ubuntu1), librust-foo+derive-dev (>= 1:1.0-2~ubuntu1)",
                "* (>=1:1.0-2~ubuntu1) -default +derive",
            ),
            (
                "librust-foo-1-dev, librust-foo-1+default-dev, librust-foo-1+derive-dev",
                "1 +derive",
            ),
        ] {
            std::fs::write(
                root.path().join("debian/control"),
                format!("Source: rust-example\nBuild-Depends: {relations}\n"),
            )
            .unwrap();
            let sections = control::read_dependency_sections(root.path(), "amd64").unwrap();
            let tables = prepare_tables(sections);
            let parts = make_requirement(&tables[0].dependencies[0], None);
            let mut texts = Vec::new();
            for part in parts {
                texts.push(part.text);
            }
            assert_eq!(texts.join(" "), expected, "{relations}");
        }
    }

    #[test]
    /// Classifies each requirement component according to its matching package relation.
    fn classifies_requirement_components() {
        let dependency = Dependency {
            name: "serde".to_owned(),
            debian_requirements: BTreeMap::from([
                (
                    None,
                    vec![PackageRequirement {
                        name: "librust-serde-1-dev".to_owned(),
                        version: None,
                    }],
                ),
                (
                    Some("alloc".to_owned()),
                    vec![PackageRequirement {
                        name: "librust-serde-1+alloc-dev".to_owned(),
                        version: None,
                    }],
                ),
                (
                    Some("derive".to_owned()),
                    vec![PackageRequirement {
                        name: "librust-serde-1+derive-dev".to_owned(),
                        version: Some((
                            VersionConstraint::GreaterThanEqual,
                            "1.0.200-~~".parse().unwrap(),
                        )),
                    }],
                ),
                (
                    Some("std".to_owned()),
                    vec![PackageRequirement {
                        name: "librust-serde-1+std-dev".to_owned(),
                        version: None,
                    }],
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
            ("1", RequirementStatus::Satisfied),
            ("-default", RequirementStatus::Satisfied),
            ("+alloc", RequirementStatus::Missing),
            ("+derive(1 (>=1.0.200-~~))", RequirementStatus::Incompatible),
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
            debian_requirements: BTreeMap::from([
                (
                    Some("default".to_owned()),
                    vec![
                        PackageRequirement {
                            name: "librust-foo-1+default-dev".to_owned(),
                            version: Some((
                                VersionConstraint::GreaterThanEqual,
                                "1.0.0".parse().unwrap(),
                            )),
                        },
                        PackageRequirement {
                            name: "librust-foo-1+default-dev".to_owned(),
                            version: Some((VersionConstraint::LessThan, "2.0.0".parse().unwrap())),
                        },
                    ],
                ),
                (
                    Some("special".to_owned()),
                    vec![PackageRequirement {
                        name: "librust-foo-1+special-dev".to_owned(),
                        version: None,
                    }],
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
            debian_requirements: BTreeMap::from([(
                Some("formatting".to_owned()),
                vec![PackageRequirement {
                    name: "librust-foo-0.3+formatting-dev".to_owned(),
                    version: None,
                }],
            )]),
        };
        let no_default_parts = make_requirement(
            &no_default_dependency,
            Some(&candidate(
                "0.2.0",
                "noble/universe",
                &["librust-foo-0.2-dev", "librust-foo-0.2+formatting-dev"],
            )),
        );
        assert_eq!(no_default_parts[0].status, RequirementStatus::Incompatible);
        assert_eq!(no_default_parts[1].status, no_default_parts[0].status);

        // A missing feature package does not make the implied bare-library version incompatible.
        let feature_parts = make_requirement(
            &no_default_dependency,
            Some(&candidate(
                "0.3.1",
                "noble/universe",
                &["librust-foo-0.3-dev"],
            )),
        );
        assert_eq!(feature_parts[0].status, RequirementStatus::Satisfied);
        assert_eq!(feature_parts[2].status, RequirementStatus::Missing);
    }

    #[test]
    /// Requires every bound attached to a displayed feature.
    fn checks_all_feature_bounds() {
        let dependency = Dependency {
            name: "foo".to_owned(),
            debian_requirements: BTreeMap::from([
                (
                    Some("default".to_owned()),
                    vec![PackageRequirement {
                        name: "librust-foo-1+default-dev".to_owned(),
                        version: None,
                    }],
                ),
                (
                    Some("special".to_owned()),
                    vec![
                        PackageRequirement {
                            name: "librust-foo-1+special-dev".to_owned(),
                            version: Some((
                                VersionConstraint::GreaterThanEqual,
                                "1.0.0".parse().unwrap(),
                            )),
                        },
                        PackageRequirement {
                            name: "librust-foo-1+special-dev".to_owned(),
                            version: Some((VersionConstraint::LessThan, "1.5.0".parse().unwrap())),
                        },
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
            source: "rust-example".to_owned(),
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
    #[test]
    /// Rejects invalid versions before APT, development-series detection, or generation.
    fn rejects_invalid_inspection_arguments() {
        for (input, version, expected) in [
            ("crate:serde", Some("bad-version"), "exact Cargo version"),
            ("pkg:.", Some("1.0.0"), "VERSION cannot"),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            fs::create_dir(temporary.path().join("debian")).unwrap();
            fs::write(temporary.path().join("debian/debcargo.toml"), "invalid").unwrap();
            let input = if input == "pkg:." {
                format!("pkg:{}", temporary.path().display())
            } else {
                input.to_owned()
            };
            let error = run(DepArgs {
                input: Some(input),
                version: version.map(str::to_owned),
                series: None,
                proposed: false,
                ppa: Vec::new(),
                architecture: Some("amd64".to_owned()),
                keep_staging: false,
            })
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }
}
