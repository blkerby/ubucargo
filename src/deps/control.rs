//! Extracts Rust dependency requirements from generated Debian control and test files.

use std::{fs, io, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use deb822_fast::Deb822;
use debian_control::{
    lossy::{Relation, Relations},
    relations::{BuildProfile, VersionConstraint},
};
use debversion::Version;

/// One Debian package relation in a dependency expression.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PackageRequirement {
    /// Binary or virtual package name.
    pub name: String,
    /// Optional Debian version constraint.
    pub version: Option<(VersionConstraint, Version)>,
}

/// Origin of dependency requirements in the generated Debian files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyOrigin {
    /// A binary package's Depends field.
    Package,
    /// The source package's Build-Depends fields.
    Source,
    /// All autopkgtests' Depends fields.
    Tests,
}

/// Applicable Rust relations declared by a binary package, source package, or tests.
#[derive(Debug, Eq, PartialEq)]
pub struct DependencySection {
    /// Paragraph or file declaring these requirements.
    pub origin: DependencyOrigin,
    /// Binary package name, or source package name for the source and tests.
    pub name: String,
    /// Parsed relations in declaration order, including duplicates.
    pub requirements: Vec<PackageRequirement>,
}

/// Reads binary Depends, source Build-Depends fields, and autopkgtest Depends.
/// Retains empty sections and repeated relations for the caller to interpret.
pub fn read_dependency_sections(
    source_root: &Path,
    architecture: &str,
) -> Result<Vec<DependencySection>> {
    let control_path = source_root.join("debian/control");
    let contents = fs::read_to_string(&control_path)
        .with_context(|| format!("read {}", control_path.display()))?;
    let control: Deb822 = contents
        .parse()
        .with_context(|| format!("parse {}", control_path.display()))?;
    let mut sections = Vec::new();
    let mut source = None;
    for paragraph in control.iter() {
        let mut requirements = Vec::new();
        if let Some(name) = paragraph.get("Source") {
            for field in ["Build-Depends", "Build-Depends-Arch", "Build-Depends-Indep"] {
                if let Some(value) = paragraph.get(field) {
                    collect_requirements(value, architecture, &mut requirements)
                        .with_context(|| format!("parse {field} in {}", control_path.display()))?;
                }
            }
            source = Some((name.to_owned(), requirements));
        } else if let Some(name) = paragraph.get("Package") {
            if let Some(value) = paragraph.get("Depends") {
                collect_requirements(value, architecture, &mut requirements).with_context(
                    || format!("parse Depends of {name} in {}", control_path.display()),
                )?;
            }
            sections.push(DependencySection {
                origin: DependencyOrigin::Package,
                name: name.to_owned(),
                requirements,
            });
        }
    }
    let (source_name, build_requirements) =
        source.context("control file has no source paragraph")?;
    sections.push(DependencySection {
        origin: DependencyOrigin::Source,
        name: source_name.clone(),
        requirements: build_requirements,
    });

    let tests_path = source_root.join("debian/tests/control");
    let mut test_requirements = Vec::new();
    match fs::read_to_string(&tests_path) {
        Ok(contents) => {
            let tests: Deb822 = contents
                .parse()
                .with_context(|| format!("parse {}", tests_path.display()))?;
            // Test Architecture restrictions are ignored, so a test that never
            // runs on the selected architecture still contributes its dependencies.
            for paragraph in tests.iter() {
                if let Some(value) = paragraph.get("Depends") {
                    collect_requirements(value, architecture, &mut test_requirements)
                        .with_context(|| format!("parse Depends in {}", tests_path.display()))?;
                }
            }
        }
        // Debcargo writes no tests for crates without a library.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", tests_path.display()));
        }
    }
    sections.push(DependencySection {
        origin: DependencyOrigin::Tests,
        name: source_name,
        requirements: test_requirements,
    });
    Ok(sections)
}

/// Adds the applicable Rust relations from one relation field,
/// rejecting `|` alternatives involving Rust packages, which debcargo never generates.
fn collect_requirements(
    value: &str,
    architecture: &str,
    requirements: &mut Vec<PackageRequirement>,
) -> Result<()> {
    // Debcargo writes substitution variables only in entries such as `${misc:Depends}` and in
    // relations to its own packages, such as `librust-foo-dev (= ${binary:Version})`, and
    // autopkgtest uses `@` entries for the tested packages; none of these need checking.
    let mut entries = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if !entry.contains('$') && !entry.starts_with('@') {
            entries.push(entry);
        }
    }
    let relations: Relations = entries.join(", ").parse().map_err(anyhow::Error::msg)?;
    for entry in relations.iter() {
        let mut applicable = Vec::new();
        for relation in &entry {
            if relation_applies(relation, architecture)? {
                applicable.push(relation);
            }
        }
        let [relation] = applicable.as_slice() else {
            for relation in &applicable {
                if parse_rust_package_name(&relation.name).is_some() {
                    bail!("Rust dependency alternatives are not supported: {entry:?}");
                }
            }
            continue;
        };
        if parse_rust_package_name(&relation.name).is_some() {
            requirements.push(PackageRequirement {
                name: relation.name.clone(),
                version: relation.version.clone(),
            });
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
    let profile_valid = relation.profiles.is_empty()
        || relation.profiles.iter().any(|group| {
            group
                .iter()
                .all(|profile| matches!(profile, BuildProfile::Disabled(_)))
        });
    Ok(profile_valid)
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

/// Splits a Debian Rust package name into its crate, optional semver line, and optional feature.
pub fn parse_rust_package_name(package: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let body = package.strip_prefix("librust-")?.strip_suffix("-dev")?;
    let (base, feature) = match body.split_once('+') {
        Some((base, feature)) => (base, Some(feature)),
        None => (body, None),
    };
    let mut name = base;
    let mut line = None;
    if let Some((prefix, suffix)) = base.rsplit_once('-')
        && suffix
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        name = prefix;
        line = Some(suffix);
    }
    Some((name, line, feature))
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;

    /// Writes a staged source tree containing `debian/control` and optional autopkgtests.
    fn write_source(control: &str, tests: Option<&str>) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("debian/tests")).unwrap();
        fs::write(root.path().join("debian/control"), control).unwrap();
        if let Some(tests) = tests {
            fs::write(root.path().join("debian/tests/control"), tests).unwrap();
        }
        root
    }

    /// Reads a source paragraph whose only relations are `relations` in Build-Depends.
    fn read_build_depends(relations: &str) -> Result<Vec<DependencySection>> {
        let root = write_source(
            &format!("Source: rust-example\nBuild-Depends: {relations}\n"),
            None,
        );
        read_dependency_sections(root.path(), "amd64")
    }

    #[test]
    /// Retains origins, empty sections, and duplicate relations in declaration order.
    fn reads_dependency_sections() {
        let root = write_source(
            indoc! {r"
                Source: rust-example
                Build-Depends: librust-serde-1-dev, librust-serde-1-dev

                Package: librust-example-dev
                Depends: librust-serde-1-dev

                Package: librust-example+std-dev
                Depends: ${misc:Depends}
            "},
            Some("Depends: librust-serde-1-dev, @\n"),
        );
        let sections = read_dependency_sections(root.path(), "amd64").unwrap();
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].origin, DependencyOrigin::Package);
        assert_eq!(sections[0].name, "librust-example-dev");
        assert_eq!(sections[1].origin, DependencyOrigin::Package);
        assert_eq!(sections[1].name, "librust-example+std-dev");
        assert!(sections[1].requirements.is_empty());
        assert_eq!(sections[2].origin, DependencyOrigin::Source);
        assert_eq!(sections[2].name, "rust-example");
        assert_eq!(sections[2].requirements.len(), 2);
        assert_eq!(sections[2].requirements[0], sections[2].requirements[1]);
        assert_eq!(sections[2].requirements[0], sections[0].requirements[0]);
        assert_eq!(sections[3].origin, DependencyOrigin::Tests);
        assert_eq!(sections[3].name, "rust-example");
        assert_eq!(sections[3].requirements, sections[0].requirements);
    }

    #[test]
    /// Accepts crates without autopkgtests, such as binary-only crates.
    fn reads_missing_tests() {
        let tables = read_build_depends("librust-serde-1-dev").unwrap();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].origin, DependencyOrigin::Source);
        assert_eq!(tables[0].name, "rust-example");
        assert_eq!(tables[1].origin, DependencyOrigin::Tests);
        assert!(tables[1].requirements.is_empty());
    }

    #[test]
    /// Rejects malformed dependencies in each source Build-Depends field.
    fn rejects_invalid_source_dependencies() {
        for field in ["Build-Depends", "Build-Depends-Arch", "Build-Depends-Indep"] {
            let root = write_source(
                &format!("Source: rust-example\n{field}: librust-serde-dev (>= )\n"),
                None,
            );
            let error = read_dependency_sections(root.path(), "amd64").unwrap_err();
            assert!(error.to_string().contains(&format!("parse {field}")));
        }
    }

    #[test]
    /// Rejects applicable alternatives involving Rust packages.
    fn rejects_rust_alternatives() {
        for relations in [
            "librust-serde-1-dev | librust-serde-2-dev",
            "librust-serde+alloc-dev | librust-serde+std-dev",
            "librust-serde-dev | librust-syn-dev",
            "librust-serde-dev | other-package",
            "other-package | librust-serde-dev",
        ] {
            let error = read_build_depends(relations).unwrap_err();
            assert!(
                format!("{error:#}").contains("alternatives are not supported"),
                "{relations}: {error:#}"
            );
        }
    }

    #[test]
    /// Ignores non-Rust groups and drops inapplicable alternatives before rejecting the rest.
    fn accepts_applicable_alternatives() {
        for (relations, expected_rust) in [
            ("other-package | another-package", false),
            ("librust-serde-dev | other-package [arm64]", true),
            ("other-package [arm64] | librust-serde-dev", true),
            ("librust-serde-dev | other-package <stage1>", true),
            ("other-package <stage1> | librust-serde-dev", true),
            ("librust-serde-dev [arm64] | other-package", false),
            ("other-package | librust-serde-dev <stage1>", false),
            ("librust-serde-dev [arm64] | other-package <stage1>", false),
        ] {
            let tables = read_build_depends(relations).unwrap();
            if expected_rust {
                let requirements = &tables[0].requirements;
                assert_eq!(requirements.len(), 1, "{relations}");
                assert_eq!(requirements[0].name, "librust-serde-dev", "{relations}");
            } else {
                assert!(tables[0].requirements.is_empty(), "{relations}");
            }
        }
    }

    #[test]
    /// Parses crate names containing digits without confusing them with semver suffixes.
    fn parses_rust_package_names() {
        assert_eq!(
            parse_rust_package_name("librust-sha2-0.10+default-dev"),
            Some(("sha2", Some("0.10"), Some("default")))
        );
        assert_eq!(
            parse_rust_package_name("librust-serde-dev"),
            Some(("serde", None, None))
        );
        for (package, name) in [
            ("librust-sha2-dev", "sha2"),
            ("librust-some-1-crate-dev", "some-1-crate"),
            ("librust-some-1..2-dev", "some-1..2"),
            ("librust-some--dev", "some-"),
        ] {
            assert_eq!(parse_rust_package_name(package), Some((name, None, None)));
        }
        assert_eq!(
            parse_rust_package_name("librust-some-crate-1.2-dev"),
            Some(("some-crate", Some("1.2"), None))
        );
        assert_eq!(parse_rust_package_name("serde-dev"), None);
        assert_eq!(parse_rust_package_name("librust-serde"), None);
    }
}
