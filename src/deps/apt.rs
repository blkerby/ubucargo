//! Builds an isolated APT view and reads Rust package candidates from it.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use debian_control::lossy::apt::Package;
use debversion::Version;
use serde::Deserialize;
use tempfile::{NamedTempFile, TempDir};

use super::control::parse_rust_package_name;

const UBUNTU_KEYRING: &str = "/usr/share/keyrings/ubuntu-archive-keyring.gpg";

/// One binary package version from one configured repository location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCandidate {
    /// Debian source package that produced the candidate binaries.
    pub source: String,
    /// Debian binary package version.
    pub version: Version,
    /// Virtual package names and versions supplied by this binary package.
    pub provides: BTreeMap<String, Option<Version>>,
    /// Compact repository location displayed to the user.
    pub location: String,
}

impl PackageCandidate {
    /// Returns the supplied version for a concrete or virtual package name.
    pub fn provided_version(&self, name: &str) -> Option<&Version> {
        self.provides.get(name).and_then(Option::as_ref)
    }

    /// Reports whether the candidate belongs to one normalized Rust crate.
    pub fn belongs_to(&self, crate_name: &str) -> bool {
        self.provides.keys().any(|provided| {
            parse_rust_package_name(provided).is_some_and(|parsed| parsed.0 == crate_name)
        })
    }
}

/// Launchpad archive metadata used to configure one public PPA.
#[derive(Deserialize)]
struct LaunchpadArchive {
    /// Whether Launchpad requires authentication for this archive.
    private: bool,
    /// OpenPGP fingerprint advertised for the archive signing key.
    signing_key_fingerprint: Option<String>,
}

/// Temporary APT configuration backed by the shared Ubucargo list cache.
struct AptView {
    /// Temporary files that isolate APT from host configuration and state.
    temporary: TempDir,
    /// Persistent APT lists shared by all invocations.
    lists: PathBuf,
    /// Selected binary architecture.
    architecture: String,
}

impl AptView {
    /// Adds the isolated APT configuration to a command.
    fn configure(&self, command: &mut Command) {
        let root = self.temporary.path();
        command
            .arg("-o")
            .arg(format!(
                "Dir::Etc::sourcelist={}",
                root.join("sources.sources").display()
            ))
            .arg("-o")
            .arg(format!(
                "Dir::Etc::sourceparts={}",
                root.join("sourceparts").display()
            ))
            .arg("-o")
            .arg(format!("Dir::State::lists={}/", self.lists.display()))
            .arg("-o")
            .arg(format!(
                "Dir::State::status={}",
                root.join("status").display()
            ))
            .arg("-o")
            .arg(format!(
                "Dir::Etc::preferences={}",
                root.join("preferences").display()
            ))
            .arg("-o")
            .arg(format!(
                "Dir::Etc::preferencesparts={}",
                root.join("preferences.d").display()
            ))
            .arg("-o")
            .arg("Dir::Cache::pkgcache=")
            .arg("-o")
            .arg("Dir::Cache::srcpkgcache=")
            .arg("-o")
            .arg("APT::Get::List-Cleanup=0")
            .arg("-o")
            .arg("Acquire::Languages=none")
            .arg("-o")
            .arg("Acquire::GzipIndexes=false")
            .arg("-o")
            .arg(format!("APT::Architecture={}", self.architecture));
    }
}

/// Refreshes the selected repositories and returns their Rust package records.
pub fn load_candidates(
    series: &str,
    architecture: &str,
    proposed: bool,
    ppas: &[String],
) -> Result<Vec<PackageCandidate>> {
    validate_name("series", series)?;
    validate_name("architecture", architecture)?;
    let view = prepare_view(series, architecture, proposed, ppas)?;

    eprintln!("updating APT metadata for {series}/{architecture}");
    let mut update = Command::new("apt-get");
    view.configure(&mut update);
    let output = update
        .arg("update")
        .output()
        .context("run apt-get update")?;
    if !output.status.success() {
        bail!(
            "apt-get update failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut indexes = Command::new("apt-get");
    view.configure(&mut indexes);
    let output = indexes
        .args([
            "indextargets",
            "--format",
            "$(FILENAME)|$(SITE)|$(RELEASE)|$(COMPONENT)|$(ARCHITECTURE)|$(IDENTIFIER)",
        ])
        .output()
        .context("run apt-get indextargets")?;
    if !output.status.success() {
        bail!(
            "apt-get indextargets failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut candidates = Vec::new();
    let mut candidate_indexes = BTreeMap::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let fields: Vec<_> = line.split('|').collect();
        if fields.len() != 6 || fields[5] != "Packages" || fields[4] != architecture {
            continue;
        }
        let location = format_location(fields[1], fields[2], fields[3]);
        read_index(
            Path::new(fields[0]),
            &location,
            &mut candidates,
            &mut candidate_indexes,
        )?;
    }
    Ok(candidates)
}

/// Creates the temporary APT files and source list for one invocation.
fn prepare_view(
    series: &str,
    architecture: &str,
    proposed: bool,
    ppas: &[String],
) -> Result<AptView> {
    let temporary = tempfile::tempdir().context("create APT configuration directory")?;
    fs::create_dir(temporary.path().join("sourceparts"))?;
    fs::create_dir(temporary.path().join("preferences.d"))?;
    fs::write(temporary.path().join("status"), "")?;
    fs::write(temporary.path().join("preferences"), "")?;
    let cache = cache_root()?;
    fs::create_dir_all(cache.join("lists/partial"))?;
    fs::create_dir_all(cache.join("keys"))?;

    let mut sources = String::new();
    for ppa in ppas {
        let (owner, name) = parse_ppa(ppa)?;
        let key = get_ppa_key(owner, name, &cache.join("keys"))?;
        sources.push_str(&format!(
            "Types: deb\nURIs: https://ppa.launchpadcontent.net/{owner}/{name}/ubuntu\nSuites: {series}\nComponents: main\nArchitectures: {architecture}\nTargets: Packages\nSigned-By: {}\n\n",
            key.display()
        ));
    }

    let ports = !matches!(architecture, "amd64" | "i386");
    let archive = if ports {
        "https://ports.ubuntu.com/ubuntu-ports"
    } else {
        "https://archive.ubuntu.com/ubuntu"
    };
    let security = if ports {
        archive
    } else {
        "https://security.ubuntu.com/ubuntu"
    };
    let proposed_suite = if proposed {
        format!(" {series}-proposed")
    } else {
        String::new()
    };
    sources.push_str(&format!(
        "Types: deb\nURIs: {archive}\nSuites: {series} {series}-updates{proposed_suite}\nComponents: main universe\nArchitectures: {architecture}\nTargets: Packages\nSigned-By: {UBUNTU_KEYRING}\n\nTypes: deb\nURIs: {security}\nSuites: {series}-security\nComponents: main universe\nArchitectures: {architecture}\nTargets: Packages\nSigned-By: {UBUNTU_KEYRING}\n"
    ));
    fs::write(temporary.path().join("sources.sources"), sources)?;
    Ok(AptView {
        temporary,
        lists: cache.join("lists"),
        architecture: architecture.to_owned(),
    })
}

/// Returns Ubucargo's user-writable APT cache directory.
fn cache_root() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(path).join("ubucargo/apt"));
    }
    let home = env::var_os("HOME").context("neither XDG_CACHE_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".cache/ubucargo/apt"))
}

/// Reads the host's native Debian architecture.
pub fn read_architecture() -> Result<String> {
    let output = Command::new("dpkg")
        .arg("--print-architecture")
        .output()
        .context("run dpkg --print-architecture")?;
    if !output.status.success() {
        bail!("dpkg --print-architecture failed");
    }
    let architecture = String::from_utf8(output.stdout)?.trim().to_owned();
    if architecture.is_empty() {
        bail!("dpkg --print-architecture returned no architecture");
    }
    Ok(architecture)
}

/// Validates a value inserted into an APT source stanza.
fn validate_name(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
    {
        bail!("invalid {kind} {value:?}");
    }
    Ok(())
}

/// Parses the documented `ppa:OWNER/NAME` syntax.
fn parse_ppa(ppa: &str) -> Result<(&str, &str)> {
    let value = ppa
        .strip_prefix("ppa:")
        .with_context(|| format!("invalid PPA {ppa:?}; expected ppa:OWNER/NAME"))?;
    let (owner, name) = value
        .split_once('/')
        .with_context(|| format!("invalid PPA {ppa:?}; expected ppa:OWNER/NAME"))?;
    if name.contains('/') {
        bail!("invalid PPA {ppa:?}; expected ppa:OWNER/NAME");
    }
    validate_name("PPA owner", owner)?;
    validate_name("PPA name", name)?;
    Ok((owner, name))
}

/// Retrieves, validates, and caches the signing key for one public PPA.
fn get_ppa_key(owner: &str, name: &str, key_directory: &Path) -> Result<PathBuf> {
    let api = format!("https://api.launchpad.net/1.0/~{owner}/+archive/ubuntu/{name}");
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", &api])
        .output()
        .with_context(|| format!("query Launchpad for ppa:{owner}/{name}"))?;
    if !output.status.success() {
        bail!(
            "could not query ppa:{owner}/{name}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let archive: LaunchpadArchive = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse Launchpad metadata for ppa:{owner}/{name}"))?;
    if archive.private {
        bail!("private PPA ppa:{owner}/{name} is not supported");
    }
    let fingerprint = archive
        .signing_key_fingerprint
        .context("PPA has no signing key fingerprint")?
        .to_ascii_uppercase();
    if fingerprint.len() != 40 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Launchpad returned invalid signing key fingerprint {fingerprint:?}");
    }
    let destination = key_directory.join(format!("{fingerprint}.asc"));
    if destination.is_file() {
        verify_key(&fs::read(&destination)?, &fingerprint)?;
        return Ok(destination);
    }

    let url = format!("https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x{fingerprint}");
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", &url])
        .output()
        .with_context(|| format!("download PPA signing key {fingerprint}"))?;
    if !output.status.success() {
        bail!(
            "could not download PPA signing key {fingerprint}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    verify_key(&output.stdout, &fingerprint)?;
    let mut temporary = NamedTempFile::new_in(key_directory)?;
    temporary.write_all(&output.stdout)?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)
        .with_context(|| format!("cache PPA signing key at {}", destination.display()))?;
    Ok(destination)
}

/// Requires an armored key to contain the fingerprint advertised by Launchpad.
fn verify_key(contents: &[u8], fingerprint: &str) -> Result<()> {
    let home = tempfile::tempdir().context("create temporary GnuPG home")?;
    let mut child = Command::new("gpg")
        .args(["--batch", "--no-options", "--homedir"])
        .arg(home.path())
        .args(["--show-keys", "--with-colons"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run gpg --show-keys")?;
    child.stdin.take().unwrap().write_all(contents)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "could not inspect PPA signing key:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let listing = String::from_utf8(output.stdout)?;
    let mut public_keys = 0;
    let mut primary_fingerprint = None;
    for line in listing.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.first() == Some(&"pub") {
            public_keys += 1;
        } else if fields.first() == Some(&"fpr") && primary_fingerprint.is_none() {
            primary_fingerprint = fields.get(9).copied();
        }
    }
    if public_keys != 1 || primary_fingerprint != Some(fingerprint) {
        bail!("downloaded PPA key does not match fingerprint {fingerprint}");
    }
    Ok(())
}

/// Reads all binary package paragraphs from one APT index.
fn read_index(
    path: &Path,
    location: &str,
    candidates: &mut Vec<PackageCandidate>,
    candidate_indexes: &mut BTreeMap<(String, Version, String), usize>,
) -> Result<()> {
    let mut child = Command::new("/usr/lib/apt/apt-helper")
        .arg("cat-file")
        .arg(path)
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("read APT index {}", path.display()))?;
    let stdout = child.stdout.take().context("capture apt-helper output")?;
    let mut paragraph = String::new();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if line.is_empty() {
            add_package(&paragraph, location, candidates, candidate_indexes)?;
            paragraph.clear();
        } else {
            paragraph.push_str(&line);
            paragraph.push('\n');
        }
    }
    add_package(&paragraph, location, candidates, candidate_indexes)?;
    let status = child.wait()?;
    if !status.success() {
        bail!("apt-helper could not read {}", path.display());
    }
    Ok(())
}

/// Adds one Rust binary package paragraph to the candidate set.
fn add_package(
    paragraph: &str,
    location: &str,
    candidates: &mut Vec<PackageCandidate>,
    candidate_indexes: &mut BTreeMap<(String, Version, String), usize>,
) -> Result<()> {
    if paragraph.is_empty() || !paragraph.starts_with("Package: librust-") {
        return Ok(());
    }
    let package: Package = paragraph.parse().map_err(anyhow::Error::msg)?;
    let (source, source_version) = match package.source {
        Some(source) => (
            source.name,
            source.version.unwrap_or_else(|| package.version.clone()),
        ),
        None => (package.name.clone(), package.version.clone()),
    };
    let key = (source.clone(), source_version.clone(), location.to_owned());
    let index = if let Some(index) = candidate_indexes.get(&key) {
        *index
    } else {
        let index = candidates.len();
        candidates.push(PackageCandidate {
            source,
            version: source_version,
            provides: BTreeMap::new(),
            location: location.to_owned(),
        });
        candidate_indexes.insert(key, index);
        index
    };
    let candidate = &mut candidates[index];
    candidate
        .provides
        .insert(package.name, Some(package.version.clone()));
    if let Some(relations) = package.provides {
        for entry in relations.0 {
            for relation in entry {
                let version = relation.version.map(|(_, version)| version);
                candidate.provides.insert(relation.name, version);
            }
        }
    }
    Ok(())
}

/// Formats repository metadata as the documented compact location.
fn format_location(site: &str, release: &str, component: &str) -> String {
    if let Some(path) = site
        .strip_prefix("https://ppa.launchpadcontent.net/")
        .or_else(|| site.strip_prefix("http://ppa.launchpadcontent.net/"))
        && let Some(path) = path.strip_suffix("/ubuntu")
    {
        return format!("ppa:{path} ({release})");
    }
    format!("{release}/{component}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Parses public PPA shorthand without accepting extra path components.
    fn parses_ppa_names() {
        assert_eq!(parse_ppa("ppa:example/rust").unwrap(), ("example", "rust"));
        assert!(parse_ppa("example/rust").is_err());
        assert!(parse_ppa("ppa:example/rust/extra").is_err());
    }

    #[test]
    /// Formats Archive and PPA index locations compactly.
    fn formats_locations() {
        assert_eq!(
            format_location(
                "https://archive.ubuntu.com/ubuntu",
                "noble-updates",
                "universe"
            ),
            "noble-updates/universe"
        );
        assert_eq!(
            format_location(
                "https://ppa.launchpadcontent.net/example/rust-staging/ubuntu",
                "noble",
                "main"
            ),
            "ppa:example/rust-staging (noble)"
        );
    }

    #[test]
    /// Adds the proposed pocket only when explicitly requested.
    fn configures_proposed_pocket() {
        let normal = prepare_view("noble", "amd64", false, &[]).unwrap();
        let normal = fs::read_to_string(normal.temporary.path().join("sources.sources")).unwrap();
        assert!(!normal.contains("noble-proposed"));

        let proposed = prepare_view("noble", "amd64", true, &[]).unwrap();
        let proposed =
            fs::read_to_string(proposed.temporary.path().join("sources.sources")).unwrap();
        assert!(proposed.contains("Suites: noble noble-updates noble-proposed"));
    }

    #[test]
    /// Parses versioned virtual packages from an APT Packages paragraph.
    fn parses_package_candidates() {
        let base = "Package: librust-serde-dev\nSource: rust-serde\nVersion: 1.0.219-1\nArchitecture: amd64\nProvides: librust-serde-1+derive-dev (= 1.0.219-1)\n";
        let feature = "Package: librust-serde+std-dev\nSource: rust-serde\nVersion: 1.0.219-1\nArchitecture: amd64\nProvides: librust-serde-1+std-dev (= 1.0.219-1)\n";
        let mut candidates = Vec::new();
        let mut indexes = BTreeMap::new();
        add_package(base, "noble/universe", &mut candidates, &mut indexes).unwrap();
        add_package(feature, "noble/universe", &mut candidates, &mut indexes).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, "rust-serde");
        assert_eq!(
            candidates[0]
                .provided_version("librust-serde-1+derive-dev")
                .unwrap()
                .to_string(),
            "1.0.219-1"
        );
        assert!(
            candidates[0]
                .provided_version("librust-serde-1+std-dev")
                .is_some()
        );
    }
}
