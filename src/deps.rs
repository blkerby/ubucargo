//! Inspects Ubuntu binary package candidates for direct Rust dependencies.

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
    apt,
    config::{get_new_local_package_config, get_new_package_config},
    generate,
    input::{Input, parse_input, validate_version},
    resolve,
};

use crate::apt::PackageCandidate;

use self::control::{
    DependencyOrigin, DependencyRequirement, DependencySection, PackageRequirement,
    parse_rust_package_name,
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
    /// Input selector: crate:NAME, archive:SUITE/SOURCE, ppa:OWNER/NAME/SERIES/SOURCE, pkg:PATH, or local:PATH.
    #[arg(value_name = "INPUT")]
    pub input: Option<String>,

    /// Exact Cargo or Debian source version; local inputs reject versions.
    #[arg(value_name = "VERSION", requires = "input")]
    pub version: Option<String>,

    /// Checking series; defaults to the published input series, otherwise the current Ubuntu development series.
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
    /// Complex expressions in declaration order, displayed after checked dependencies.
    complex_requirements: Vec<String>,
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
    /// The expression is retained for inspection and its availability is unknown.
    Unknown,
}

/// One independently colored component of a compact Debian requirement.
#[derive(Debug, Eq, PartialEq)]
struct RequirementPart {
    /// Version expression, feature name with its `+` prefix, or complex relation.
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
    /// Requirement components classified for this candidate or retained as unknown.
    requirement: Vec<RequirementPart>,
}

/// Reads existing packaging or stages a crate, then reports direct Rust dependency candidates.
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
    } else if let Some(series) = crate::input::read_input_series(&input) {
        series
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
    let input_records = if crate::input::read_input_series(&input).is_some() {
        Some(apt::load_source_records(&input, &architecture)?)
    } else {
        None
    };
    let records = apt::load_records(series, &architecture, args.proposed, &ppas)?;
    let (sections, header, mut identity) = match &input {
        Input::Crate(_) | Input::Local(_) => {
            let (request, config) = match &input {
                Input::Crate(name) => (
                    resolve::CrateRequest::Registry {
                        name,
                        version: args.version.as_deref(),
                    },
                    get_new_package_config()?,
                ),
                Input::Local(path) => (
                    resolve::CrateRequest::Local(path),
                    get_new_local_package_config(path, None)?,
                ),
                _ => unreachable!(),
            };
            let resolved = resolve::resolve_package(request, config, None)?;
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
                input_records.as_ref().unwrap(),
                source,
                ppa,
                args.version.as_deref(),
            )?;
            let (_stage, root) = apt::retrieve_source(source, args.keep_staging)?;
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
        let rows = classify(table, &candidates);
        for row in &rows {
            if matches!(row.status, "incompatible" | "missing" | "unknown") {
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
        let mut complex_requirements = Vec::new();
        for requirement in section.requirements {
            if seen.insert(requirement.clone()) || section.origin == DependencyOrigin::Package {
                match requirement {
                    DependencyRequirement::Package(requirement) => shown.push(requirement),
                    DependencyRequirement::Complex(expression) => {
                        complex_requirements.push(expression);
                    }
                }
            }
        }
        let dependencies = group_dependencies(shown);
        if !dependencies.is_empty() || !complex_requirements.is_empty() {
            let label = match section.origin {
                DependencyOrigin::Package => "Package",
                DependencyOrigin::Source => "Source",
                DependencyOrigin::Tests => "Tests",
            };
            tables.push(DependencyTable {
                heading: format!("{label}: {}", section.name),
                dependencies,
                complex_requirements,
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

/// Classifies candidates in deterministic order and appends unknown complex expressions.
fn classify(table: &DependencyTable, candidates: &[PackageCandidate]) -> Vec<Row> {
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
    for dependency in &table.dependencies {
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
                "preferred"
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
    for expression in &table.complex_requirements {
        rows.push(Row {
            dependency: "(complex dependency)".to_owned(),
            status: "unknown",
            location: "-".to_owned(),
            version: "-".to_owned(),
            requirement: vec![RequirementPart {
                text: expression.clone(),
                status: RequirementStatus::Unknown,
            }],
        });
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
                    "preferred" => GREEN,
                    "available" | "unknown" => GRAY,
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
                        RequirementStatus::Unknown => GRAY,
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
