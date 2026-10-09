//! Advisory crates.io and Ubuntu version information, separate from dependency classification.
use std::{collections::BTreeMap, process::Command};

use anyhow::Result;
use semver::Version;
use serde::Deserialize;

use super::control::parse_rust_package_name;
use crate::apt::SourceCandidate;
use crate::{input::Input, resolve::normalize_crate_name};

/// Resolved input identity used to compare available versions without changing the input.
pub struct InputIdentity {
    /// Cargo crate name, when known independently of generation metadata.
    pub crate_name: Option<String>,
    /// Maintained or generated Debian source-package name.
    pub source_name: String,
    /// Exact Cargo or Debian version according to the input kind.
    pub version: String,
}

/// Outcome of an advisory crates.io lookup.
#[derive(Debug, PartialEq, Eq)]
pub enum LatestRelease {
    /// Highest stable release that has not been yanked.
    Version(Version),
    /// The crate does not exist on crates.io.
    NotPublished,
    /// The crate exists but has no non-yanked stable release.
    NoStableRelease,
    /// The request or metadata could not be read.
    Unavailable,
}

/// Version records returned by the crates.io crate metadata endpoint.
#[derive(Deserialize)]
struct CrateMetadata {
    /// Published versions, whose publication order need not match semantic ordering.
    versions: Vec<ReleaseMetadata>,
}

/// Fields needed to select a stable crates.io release.
#[derive(Deserialize)]
struct ReleaseMetadata {
    /// Semantic release version.
    num: String,
    /// Whether new resolution should exclude this release.
    yanked: bool,
}

/// Selects the greatest non-yanked stable version from crate metadata.
fn select_latest_release(contents: &[u8]) -> Result<Option<Version>> {
    let metadata: CrateMetadata = serde_json::from_slice(contents)?;
    let mut latest: Option<Version> = None;
    for release in metadata.versions {
        let version = Version::parse(&release.num)?;
        if release.yanked || !version.pre.is_empty() {
            continue;
        }
        if latest
            .as_ref()
            .is_none_or(|old| version.cmp_precedence(old).is_gt())
        {
            latest = Some(version);
        }
    }
    Ok(latest)
}

/// Interprets an HTTP response without letting lookup failures affect dependency results.
fn parse_latest_response(status: &str, contents: &[u8]) -> LatestRelease {
    match status {
        "404" => LatestRelease::NotPublished,
        "200" => match select_latest_release(contents) {
            Ok(Some(version)) => LatestRelease::Version(version),
            Ok(None) => LatestRelease::NoStableRelease,
            Err(_) => LatestRelease::Unavailable,
        },
        _ => LatestRelease::Unavailable,
    }
}

/// Queries crates.io metadata with bounded latency and no crate archive download.
pub fn read_latest_release(crate_name: Option<&str>) -> LatestRelease {
    let Some(crate_name) = crate_name else {
        return LatestRelease::Unavailable;
    };
    if crate_name.is_empty()
        || !crate_name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return LatestRelease::Unavailable;
    }
    let response = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "3",
            "--max-time",
            "8",
            "--user-agent",
            concat!("ubucargo/", env!("CARGO_PKG_VERSION")),
            "--write-out",
            "\n%{http_code}",
        ])
        .arg(format!("https://crates.io/api/v1/crates/{crate_name}"))
        .output();
    let Ok(response) = response else {
        return LatestRelease::Unavailable;
    };
    if !response.status.success() {
        return LatestRelease::Unavailable;
    }
    let Some(separator) = response.stdout.iter().rposition(|c| *c == b'\n') else {
        return LatestRelease::Unavailable;
    };
    let Ok(status) = std::str::from_utf8(&response.stdout[separator + 1..]) else {
        return LatestRelease::Unavailable;
    };
    parse_latest_response(status, &response.stdout[..separator])
}

/// Infers a crate identity from conventional Rust source names, including parallel semver suffixes.
pub fn infer_crate_name(source: &str) -> Option<String> {
    let body = source.strip_prefix("rust-")?;
    let binary = format!("librust-{body}-dev");
    let (name, _, _) = parse_rust_package_name(&binary)?;
    Some(normalize_crate_name(name))
}

/// Formats latest versions, excluding PPAs and suppressing matching implicitly selected inputs.
pub fn format_latest(
    input: &Input,
    explicit_version: bool,
    identity: &InputIdentity,
    series: &str,
    sources: &[SourceCandidate],
    release: Option<LatestRelease>,
) -> String {
    let mut entries = Vec::new();
    if let Some(release) = release {
        let description = match release {
            LatestRelease::Version(version) => version.to_string(),
            LatestRelease::NotPublished => "not published".to_owned(),
            LatestRelease::NoStableRelease => "no stable release".to_owned(),
            LatestRelease::Unavailable => "unavailable".to_owned(),
        };
        entries.push(format!("crates.io availability: {description}"));
    }
    let mut latest: BTreeMap<&str, &SourceCandidate> = BTreeMap::new();
    for source in sources {
        if source.location.starts_with("ppa:") {
            continue;
        }
        let mut matches = source.source == identity.source_name;
        if let Some(name) = &identity.crate_name {
            let normalized = normalize_crate_name(name);
            matches |= infer_crate_name(&source.source).as_deref() == Some(normalized.as_str());
            for binary in &source.binaries {
                if let Some((name, _, _)) = parse_rust_package_name(binary) {
                    matches |= normalize_crate_name(name) == normalized;
                }
            }
        }
        if !matches {
            continue;
        }
        if latest.get(source.source.as_str()).is_none_or(|old| {
            source.version > old.version
                || (source.version == old.version && source.location < old.location)
        }) {
            latest.insert(&source.source, source);
        }
    }
    if latest.is_empty() {
        entries.push(format!("{series} availability: not packaged"));
    }
    let mut archive_entries = Vec::new();
    for source in latest.values() {
        if !explicit_version
            && let Input::Archive {
                suite: input_suite,
                source: input_source,
            } = input
            && crate::input::split_archive_suite(input_suite).0 == series
            && *input_source == source.source
            && identity.version == source.version.to_string()
        {
            continue;
        }
        archive_entries.push(format!("{} {}", source.source, source.version));
    }
    if !archive_entries.is_empty() {
        entries.push(format!(
            "{series} availability: {}",
            archive_entries.join(", ")
        ));
    }
    if entries.is_empty() {
        String::new()
    } else {
        format!("{}\n", entries.join("\n"))
    }
}
