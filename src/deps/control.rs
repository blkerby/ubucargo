//! Extracts Rust dependency requirements from generated Debian control files.

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use debian_control::{
    lossy::{Control, Relation, Relations},
    relations::{BuildProfile, VersionConstraint},
};
use debversion::Version;
use semver::{BuildMetadata, Comparator, Version as SemverVersion};

/// One Debian package alternative in a dependency expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRequirement {
    /// Binary or virtual package name.
    pub name: String,
    /// Optional Debian version constraint.
    pub version: Option<(VersionConstraint, Version)>,
}

/// All generated Debian requirements belonging to one direct Rust crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Normalized Cargo crate name.
    pub name: String,
    /// Human-readable Cargo-style semver and feature requirement.
    pub requirement: String,
    /// Comma-separated requirements containing pipe-separated alternatives.
    pub entries: Vec<Vec<PackageRequirement>>,
}

/// Requirements accumulated while grouping generated package relations by crate.
#[derive(Default)]
struct DependencyParts {
    /// Comma-separated package requirements and their alternatives.
    entries: Vec<Vec<PackageRequirement>>,
    /// Version requirements grouped by Cargo feature, with `None` for the base crate.
    requirements: BTreeMap<Option<String>, VersionRange>,
    /// Whether the original relations require exact Debian-syntax rendering.
    raw: bool,
}

/// One inclusive or exclusive endpoint of a semantic-version range.
#[derive(Clone, Debug)]
struct VersionEndpoint {
    /// Fully specified semantic version used for comparisons.
    version: SemverVersion,
    /// Whether the endpoint includes its version.
    inclusive: bool,
}

impl PartialEq for VersionEndpoint {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && self.inclusive == other.inclusive
    }
}

impl Eq for VersionEndpoint {}

/// One conjunctive semantic-version range.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct VersionRange {
    /// Lower endpoint, or no lower bound.
    lower: Option<VersionEndpoint>,
    /// Upper endpoint, or no upper bound.
    upper: Option<VersionEndpoint>,
}

/// Reads direct Rust build dependencies from a generated control file.
pub fn read_dependencies(path: &Path, architecture: &str) -> Result<Vec<Dependency>> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_dependencies(&contents, architecture)
        .with_context(|| format!("parse Rust dependencies from {}", path.display()))
}

/// Parses direct Rust build dependencies from generated control text.
fn parse_dependencies(contents: &str, architecture: &str) -> Result<Vec<Dependency>> {
    let source = contents.split("\n\n").next().unwrap_or(contents);
    let control: Control = source.parse().map_err(anyhow::Error::msg)?;
    let mut grouped: BTreeMap<String, DependencyParts> = BTreeMap::new();
    for relations in [
        control.source.build_depends,
        control.source.build_depends_arch,
        control.source.build_depends_indep,
    ]
    .into_iter()
    .flatten()
    {
        collect_dependencies(&relations, architecture, &mut grouped)?;
    }

    let mut dependencies = Vec::new();
    for (name, mut parts) in grouped {
        let default = parts.requirements.remove(&Some("default".to_owned()));
        let base = parts.requirements.remove(&None);
        let default_required = default.is_some();
        let mut common = match (default, base) {
            (Some(default), Some(base)) => Some(intersect_version_ranges(&default, &base)),
            (Some(default), None) => Some(default),
            (None, Some(base)) => Some(base),
            (None, None) => None,
        };
        for requirement in parts.requirements.values() {
            match &common {
                Some(common) if version_ranges_equal(common, requirement) => {}
                Some(_) => parts.raw = true,
                None => common = Some(requirement.clone()),
            }
        }
        if common.as_ref().is_some_and(is_empty_version_range) {
            parts.raw = true;
        }

        let mut requirement = if parts.raw {
            format_raw_requirement(&parts.entries)
        } else {
            format_version_range(common.as_ref().unwrap())
        };
        if !parts.raw {
            if !default_required {
                requirement.push_str(" -default");
            }
            for feature in parts.requirements.keys().flatten() {
                requirement.push_str(" +");
                requirement.push_str(feature);
            }
        }
        dependencies.push(Dependency {
            name,
            requirement,
            entries: parts.entries,
        });
    }
    Ok(dependencies)
}

/// Formats one semantic-version range, using caret syntax only when exact.
fn format_version_range(range: &VersionRange) -> String {
    if let Some(caret) = format_caret_range(range) {
        return caret;
    }
    if let (Some(lower), Some(upper)) = (&range.lower, &range.upper)
        && lower.inclusive
        && upper.inclusive
        && lower.version == upper.version
    {
        return format!("={}", lower.version);
    }

    let mut parts = Vec::new();
    if let Some(lower) = &range.lower {
        parts.push(format!(
            "{}{}",
            if lower.inclusive { ">=" } else { ">" },
            if lower.inclusive {
                format_partial_version(&lower.version)
            } else {
                lower.version.to_string()
            }
        ));
    }
    if let Some(upper) = &range.upper {
        parts.push(format!(
            "{}{}",
            if upper.inclusive { "<=" } else { "<" },
            if upper.inclusive {
                upper.version.to_string()
            } else {
                format_partial_version(&upper.version)
            }
        ));
    }
    if parts.is_empty() {
        "*".to_owned()
    } else {
        parts.join(",")
    }
}

/// Formats the shortest caret expression exactly matching one range.
fn format_caret_range(range: &VersionRange) -> Option<String> {
    let (Some(lower), Some(upper)) = (&range.lower, &range.upper) else {
        return None;
    };
    if !lower.inclusive || upper.inclusive {
        return None;
    }
    let candidates = [
        lower.version.major.to_string(),
        format!("{}.{}", lower.version.major, lower.version.minor),
        lower.version.to_string(),
    ];
    for candidate in candidates {
        let Ok(caret) = format!("^{candidate}").parse::<Comparator>() else {
            continue;
        };
        let Some(candidate_range) = make_caret_range(&caret) else {
            continue;
        };
        if version_ranges_equal(range, &candidate_range) {
            return Some(format!("^{candidate}"));
        }
    }
    None
}

/// Omits trailing zero components when comparator semantics remain unchanged.
fn format_partial_version(version: &SemverVersion) -> String {
    if !version.pre.is_empty() || version.patch != 0 {
        version.to_string()
    } else if version.minor != 0 {
        format!("{}.{}", version.major, version.minor)
    } else {
        version.major.to_string()
    }
}

/// Formats the original Debian relations when semantic reduction is unsafe.
fn format_raw_requirement(entries: &[Vec<PackageRequirement>]) -> String {
    let mut output = "debian:".to_owned();
    for (entry_index, alternatives) in entries.iter().enumerate() {
        if entry_index > 0 {
            output.push_str(", ");
        }
        for (alternative_index, requirement) in alternatives.iter().enumerate() {
            if alternative_index > 0 {
                output.push_str(" | ");
            }
            output.push_str(&requirement.name);
            if let Some((constraint, version)) = &requirement.version {
                output.push_str(" (");
                output.push_str(format_version_constraint(constraint));
                output.push(' ');
                output.push_str(&version.to_string());
                output.push(')');
            }
        }
    }
    output
}

/// Returns the Debian spelling of one version comparison operator.
fn format_version_constraint(constraint: &VersionConstraint) -> &'static str {
    match constraint {
        VersionConstraint::GreaterThanEqual => ">=",
        VersionConstraint::LessThanEqual => "<=",
        VersionConstraint::Equal => "=",
        VersionConstraint::GreaterThan => ">>",
        VersionConstraint::LessThan => "<<",
    }
}

/// Constructs a version range from one applicable package relation.
fn make_relation_range(
    version: Option<&str>,
    bound: Option<&(VersionConstraint, Version)>,
) -> Option<VersionRange> {
    let mut range = match version {
        Some(version) => make_caret_range(&format!("^{version}").parse().ok()?)?,
        None => VersionRange::default(),
    };
    if let Some((constraint, version)) = bound {
        let mut endpoint = parse_version_endpoint(version)?;
        match constraint {
            VersionConstraint::GreaterThanEqual => {
                endpoint.inclusive = true;
                tighten_lower_bound(&mut range.lower, endpoint);
            }
            VersionConstraint::GreaterThan => {
                endpoint.inclusive = false;
                tighten_lower_bound(&mut range.lower, endpoint);
            }
            VersionConstraint::LessThanEqual => {
                endpoint.inclusive = true;
                tighten_upper_bound(&mut range.upper, endpoint);
            }
            VersionConstraint::LessThan => {
                endpoint.inclusive = false;
                tighten_upper_bound(&mut range.upper, endpoint);
            }
            VersionConstraint::Equal => {
                endpoint.inclusive = true;
                tighten_lower_bound(&mut range.lower, endpoint.clone());
                tighten_upper_bound(&mut range.upper, endpoint);
            }
        }
    }
    Some(range)
}

/// Constructs the semantic-version interval represented by one caret comparator.
fn make_caret_range(caret: &Comparator) -> Option<VersionRange> {
    let lower = VersionEndpoint {
        version: make_comparator_version(caret),
        inclusive: true,
    };
    let upper_version = calculate_caret_ceiling(caret)?;
    let upper = VersionEndpoint {
        version: upper_version,
        inclusive: false,
    };
    Some(VersionRange {
        lower: Some(lower),
        upper: Some(upper),
    })
}

/// Parses a Debian version endpoint that has an exact semantic-version meaning.
fn parse_version_endpoint(version: &Version) -> Option<VersionEndpoint> {
    if version.epoch.is_some()
        || version
            .debian_revision
            .as_deref()
            .is_some_and(|revision| revision != "~~")
    {
        return None;
    }
    let text = version
        .upstream_version
        .strip_suffix("-~~")
        .unwrap_or(&version.upstream_version);
    let comparator: Comparator = format!("={text}").parse().ok()?;
    Some(VersionEndpoint {
        version: make_comparator_version(&comparator),
        inclusive: true,
    })
}

/// Intersects two semantic-version ranges.
fn intersect_version_ranges(first: &VersionRange, second: &VersionRange) -> VersionRange {
    let mut range = first.clone();
    if let Some(lower) = &second.lower {
        tighten_lower_bound(&mut range.lower, lower.clone());
    }
    if let Some(upper) = &second.upper {
        tighten_upper_bound(&mut range.upper, upper.clone());
    }
    range
}

/// Intersects an existing range with one lower endpoint.
fn tighten_lower_bound(current: &mut Option<VersionEndpoint>, candidate: VersionEndpoint) {
    if current.as_ref().is_none_or(|current| {
        candidate.version > current.version
            || candidate.version == current.version && !candidate.inclusive && current.inclusive
    }) {
        *current = Some(candidate);
    }
}

/// Intersects an existing range with one upper endpoint.
fn tighten_upper_bound(current: &mut Option<VersionEndpoint>, candidate: VersionEndpoint) {
    if current.as_ref().is_none_or(|current| {
        candidate.version < current.version
            || candidate.version == current.version && !candidate.inclusive && current.inclusive
    }) {
        *current = Some(candidate);
    }
}

/// Reports whether one semantic-version range contains no versions.
fn is_empty_version_range(range: &VersionRange) -> bool {
    let (Some(lower), Some(upper)) = (&range.lower, &range.upper) else {
        return false;
    };
    lower.version > upper.version
        || lower.version == upper.version && (!lower.inclusive || !upper.inclusive)
}

/// Reports whether two ranges have the same semantic endpoints.
fn version_ranges_equal(first: &VersionRange, second: &VersionRange) -> bool {
    first.lower == second.lower && first.upper == second.upper
}

/// Converts a semantic-version comparator endpoint into a concrete version.
fn make_comparator_version(comparator: &Comparator) -> SemverVersion {
    SemverVersion {
        major: comparator.major,
        minor: comparator.minor.unwrap_or(0),
        patch: comparator.patch.unwrap_or(0),
        pre: comparator.pre.clone(),
        build: BuildMetadata::EMPTY,
    }
}

/// Returns the exclusive upper endpoint of a plain caret requirement.
fn calculate_caret_ceiling(comparator: &Comparator) -> Option<SemverVersion> {
    if !comparator.pre.is_empty() {
        return None;
    }
    if comparator.major > 0 || comparator.minor.is_none() {
        return Some(SemverVersion::new(comparator.major.checked_add(1)?, 0, 0));
    }
    let minor = comparator.minor.unwrap();
    if minor > 0 || comparator.patch.is_none() {
        return Some(SemverVersion::new(0, minor.checked_add(1)?, 0));
    }
    Some(SemverVersion::new(
        0,
        0,
        comparator.patch.unwrap().checked_add(1)?,
    ))
}

/// Adds applicable Rust relations to their logical crate dependency.
fn collect_dependencies(
    relations: &Relations,
    architecture: &str,
    grouped: &mut BTreeMap<String, DependencyParts>,
) -> Result<()> {
    for entry in relations.iter() {
        let mut alternatives = Vec::new();
        let mut crate_name = None;
        let mut requirement = None;
        let mut raw = false;
        for relation in &entry {
            if !relation_applies(relation, architecture)? {
                continue;
            }
            let Some((name, version, feature)) = parse_rust_package_name(&relation.name) else {
                continue;
            };
            if crate_name
                .as_ref()
                .is_some_and(|existing| existing != &name)
            {
                bail!("Rust dependency alternatives refer to different crates: {entry:?}");
            }
            crate_name = Some(name);
            let range = make_relation_range(version.as_deref(), relation.version.as_ref());
            if range.is_none() {
                raw = true;
            }
            if requirement.is_some() {
                raw = true;
            } else {
                requirement = Some((feature, range));
            }
            alternatives.push(PackageRequirement {
                name: relation.name.clone(),
                version: relation.version.clone(),
            });
        }
        if let Some(crate_name) = crate_name {
            let grouped = grouped.entry(crate_name).or_default();
            grouped.entries.push(alternatives);
            if let Some((feature, Some(requirement))) = requirement {
                match grouped.requirements.entry(feature) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(requirement);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let intersection = intersect_version_ranges(entry.get(), &requirement);
                        if is_empty_version_range(&intersection) {
                            raw = true;
                        }
                        entry.insert(intersection);
                    }
                }
            }
            grouped.raw |= raw;
        }
    }
    Ok(())
}

/// Reports whether a relation applies for the selected architecture and default profiles.
fn relation_applies(relation: &Relation, architecture: &str) -> Result<bool> {
    if let Some(architectures) = &relation.architectures {
        let mut has_positive = false;
        let mut positive_match = false;
        for candidate in architectures {
            if let Some(excluded) = candidate.strip_prefix('!') {
                if matches_architecture(architecture, excluded)? {
                    return Ok(false);
                }
            } else {
                has_positive = true;
                if matches_architecture(architecture, candidate)? {
                    positive_match = true;
                }
            }
        }
        if has_positive && !positive_match {
            return Ok(false);
        }
    }
    if relation.profiles.is_empty() {
        return Ok(true);
    }
    for group in &relation.profiles {
        let mut matches = true;
        for profile in group {
            match profile {
                BuildProfile::Enabled(_) => matches = false,
                BuildProfile::Disabled(_) => {}
            }
        }
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Matches one Debian architecture against an architecture restriction.
fn matches_architecture(architecture: &str, restriction: &str) -> Result<bool> {
    let status = Command::new("dpkg-architecture")
        .arg(format!("-a{architecture}"))
        .arg(format!("-i{restriction}"))
        .status()
        .context("run dpkg-architecture")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("dpkg-architecture could not match {architecture:?} against {restriction:?}"),
    }
}

/// Splits a Debian Rust development package into crate, semver line, and feature.
pub(crate) fn parse_rust_package_name(
    package: &str,
) -> Option<(String, Option<String>, Option<String>)> {
    let body = package.strip_prefix("librust-")?.strip_suffix("-dev")?;
    let (base, feature) = match body.split_once('+') {
        Some((base, feature)) => (base, Some(feature.to_owned())),
        None => (body, None),
    };
    let mut name = base;
    let mut version = None;
    for (index, character) in base.char_indices().rev() {
        if character != '-' {
            continue;
        }
        let suffix = &base[index + 1..];
        if !suffix.is_empty()
            && suffix
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            name = &base[..index];
            version = Some(suffix.to_owned());
            break;
        }
    }
    Some((name.to_owned(), version, feature))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Extracts and groups semver and feature package requirements.
    fn extracts_rust_dependencies() {
        let control = r#"Source: rust-example
Build-Depends: debhelper-compat (= 13),
 librust-serde-1+derive-dev (>= 1.0.100-~~),
 librust-serde-1+std-dev,
 librust-syn-2-dev | librust-syn-dev,
 librust-disabled-1-dev [arm64]

Package: librust-example-dev
Architecture: any
Provides: ${cargo:Provides}
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();
        assert_eq!(dependencies.len(), 2);
        assert_eq!(dependencies[0].name, "serde");
        assert_eq!(
            dependencies[0].requirement,
            concat!(
                "debian:librust-serde-1+derive-dev (>= 1.0.100-~~), ",
                "librust-serde-1+std-dev"
            )
        );
        assert_eq!(dependencies[0].entries.len(), 2);
        assert_eq!(dependencies[1].name, "syn");
        assert_eq!(
            dependencies[1].requirement,
            "debian:librust-syn-2-dev | librust-syn-dev"
        );
        assert_eq!(dependencies[1].entries[0].len(), 2);
    }

    #[test]
    /// Includes explicit bounds alongside a package-name semver suffix.
    fn includes_suffixed_version_bounds() {
        let control = r#"Source: rust-example
Build-Depends: librust-actix-http-3+default-dev (>= 3.13.0),
 librust-actix-http-3+default-dev (<< 3.14.0)

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();

        assert_eq!(dependencies[0].requirement, ">=3.13,<3.14");
    }

    #[test]
    /// Simplifies only bounds that preserve the caret requirement's upper endpoint.
    fn simplifies_equivalent_caret_bounds() {
        let control = r#"Source: rust-example
Build-Depends: librust-narrow-0.2-dev (>= 0.2.3),
 librust-narrow-0.2-dev (<< 0.3),
 librust-redundant-1.2-dev (>= 1.1),
 librust-redundant-1.2-dev (<< 2),
 librust-wide-0-dev (>= 0.2.3)

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();

        assert_eq!(dependencies[0].requirement, "^0.2.3 -default");
        assert_eq!(dependencies[1].requirement, "^1.2 -default");
        assert_eq!(dependencies[2].requirement, ">=0.2.3,<1 -default");
    }

    #[test]
    /// Includes explicit bounds for packages without a semver suffix.
    fn includes_unsuffixed_version_bounds() {
        let control = r#"Source: rust-example
Build-Depends: librust-derive-more+as-ref-dev (<< 3),
 librust-derive-more+as-ref-dev (>= 1.0.0),
 librust-derive-more+default-dev (<< 3),
 librust-derive-more+default-dev (>= 1.0.0)

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();

        assert_eq!(dependencies[0].requirement, ">=1,<3 +as-ref");
    }

    #[test]
    /// Preserves Debian-specific versions in the raw fallback.
    fn preserves_unreducible_debian_versions() {
        let control = r#"Source: rust-example
Build-Depends: librust-example-1+default-dev (>= 1:1.2.3-1)

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();

        assert_eq!(
            dependencies[0].requirement,
            "debian:librust-example-1+default-dev (>= 1:1.2.3-1)"
        );
    }

    #[test]
    /// Keeps disjoint alternatives and inclusive or exclusive endpoints structured.
    fn reduces_version_ranges() {
        let control = r#"Source: rust-example
Build-Depends: librust-bounded+default-dev (>> 1.0),
 librust-bounded+default-dev (<= 2.0),
 librust-choice-1-dev | librust-choice-3-dev

Package: librust-example-dev
Architecture: any
Description: example
"#;
        let dependencies = parse_dependencies(control, "amd64").unwrap();

        assert_eq!(dependencies[0].requirement, ">1.0.0,<=2.0.0");
        assert_eq!(
            dependencies[1].requirement,
            "debian:librust-choice-1-dev | librust-choice-3-dev"
        );
    }

    #[test]
    /// Parses crate names containing digits without confusing them with semver suffixes.
    fn parses_rust_package_names() {
        assert_eq!(
            parse_rust_package_name("librust-sha2-0.10+default-dev"),
            Some((
                "sha2".to_owned(),
                Some("0.10".to_owned()),
                Some("default".to_owned())
            ))
        );
        assert_eq!(
            parse_rust_package_name("librust-serde-dev"),
            Some(("serde".to_owned(), None, None))
        );
    }
}
