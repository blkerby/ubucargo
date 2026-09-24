//! Builds an isolated APT view and reads Rust package candidates from it.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::util::run_command;
use deb822_fast::{Deb822, FromDeb822Paragraph};
use debian_control::lossy::apt::Package;
use debversion::Version;
use serde::Deserialize;
use tempfile::NamedTempFile;

const UBUNTU_KEYRING: &str = "/usr/share/keyrings/ubuntu-archive-keyring.gpg";

/// One binary package version from one configured repository location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCandidate {
    /// Debian binary package version.
    pub version: Version,
    /// Virtual package names and versions supplied by this binary package.
    pub provides: BTreeMap<String, Option<Version>>,
    /// Compact repository location displayed to the user.
    pub location: String,
}

/// Launchpad archive metadata used to configure one public PPA.
#[derive(Deserialize)]
struct LaunchpadArchive {
    /// Whether Launchpad requires authentication for this archive.
    private: bool,
    /// OpenPGP fingerprint advertised for the archive signing key.
    signing_key_fingerprint: Option<String>,
}

/// Persistent APT configuration locked for one metadata query.
struct AptView {
    /// Shared configuration, indexes, signing keys, and binary cache.
    root: PathBuf,
    /// Keeps other invocations from changing the view until parsing finishes.
    _lock: fs::File,
    /// Selected binary architecture.
    architecture: String,
}

impl AptView {
    /// Adds the isolated APT configuration to a command.
    fn configure(&self, command: &mut Command) {
        let root = &self.root;
        for option in [
            format!(
                "Dir::Etc::sourcelist={}",
                root.join("sources.sources").display()
            ),
            format!(
                "Dir::Etc::sourceparts={}",
                root.join("sourceparts").display()
            ),
            format!("Dir::State::lists={}/", root.join("lists").display()),
            format!("Dir::State::status={}", root.join("status").display()),
            format!(
                "Dir::Etc::preferences={}",
                root.join("preferences").display()
            ),
            format!(
                "Dir::Etc::preferencesparts={}",
                root.join("preferences.d").display()
            ),
            format!(
                "Dir::Cache::pkgcache={}",
                root.join("pkgcache.bin").display()
            ),
            "Dir::Cache::srcpkgcache=".to_owned(),
            "APT::Get::List-Cleanup=0".to_owned(),
            "Acquire::Languages=none".to_owned(),
            "Acquire::GzipIndexes=false".to_owned(),
            format!("APT::Architecture={}", self.architecture),
        ] {
            command.arg("-o").arg(option);
        }
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
    let view = prepare_view(&cache_root()?, series, architecture, proposed, ppas)?;

    let mut update = Command::new("apt-get");
    view.configure(&mut update);
    // Let indextargets validate and reuse the binary cache, rebuilding if needed.
    update.args(["-o", "pkgCacheFile::Generate=false"]);
    run_command(update.arg("update"), "apt-get update")?;

    let mut indexes = Command::new("apt-get");
    view.configure(&mut indexes);
    let output = run_command(
        indexes.args([
            "indextargets",
            "--format",
            "$(FILENAME)|$(SITE)|$(RELEASE)|$(COMPONENT)|$(ARCHITECTURE)|$(IDENTIFIER)",
        ]),
        "apt-get indextargets",
    )?;

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

/// Locks the shared view and replaces its sources only when their contents change.
fn prepare_view(
    cache: &Path,
    series: &str,
    architecture: &str,
    proposed: bool,
    ppas: &[String],
) -> Result<AptView> {
    fs::create_dir_all(cache).context("create APT cache directory")?;
    let cache = cache.canonicalize()?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("view.lock"))?;
    // ponytail: serialize shared-view queries; separate views if concurrency matters.
    lock.lock().context("lock shared APT view")?;
    for directory in ["sourceparts", "preferences.d", "lists/partial", "keys"] {
        fs::create_dir_all(cache.join(directory))?;
    }
    for file in ["status", "preferences"] {
        fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(cache.join(file))?;
    }

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
    let source_path = cache.join("sources.sources");
    let previous = match fs::read(&source_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error).context("read cached APT sources"),
    };
    if previous != sources.as_bytes() {
        let mut temporary = NamedTempFile::new_in(&cache)?;
        temporary.write_all(sources.as_bytes())?;
        // Invalidate before replacement, including changes within one timestamp tick.
        match fs::remove_file(cache.join("pkgcache.bin")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("invalidate APT binary cache"),
        }
        temporary
            .persist(&source_path)
            .map_err(|error| error.error)
            .context("replace cached APT sources")?;
    }
    Ok(AptView {
        root: cache,
        _lock: lock,
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
    let output = run_command(
        Command::new("dpkg").arg("--print-architecture"),
        "dpkg --print-architecture",
    )?;
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
    let output = run_command(
        Command::new("curl").args(["--fail", "--silent", "--show-error", "--location", &api]),
        &format!("query Launchpad for ppa:{owner}/{name}"),
    )?;
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
    let output = run_command(
        Command::new("curl").args(["--fail", "--silent", "--show-error", "--location", &url]),
        &format!("download PPA signing key {fingerprint}"),
    )?;
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
    let file =
        fs::File::open(path).with_context(|| format!("read APT index {}", path.display()))?;
    for paragraph in Deb822::iter_paragraphs_from_reader(BufReader::new(file)) {
        let paragraph = paragraph?;
        if paragraph
            .get("Package")
            .is_some_and(|name| name.starts_with("librust-"))
        {
            let package = Package::from_paragraph(&paragraph).map_err(anyhow::Error::msg)?;
            add_package(package, location, candidates, candidate_indexes);
        }
    }
    Ok(())
}

/// Adds one Rust binary package to the candidate set.
fn add_package(
    package: Package,
    location: &str,
    candidates: &mut Vec<PackageCandidate>,
    candidate_indexes: &mut BTreeMap<(String, Version, String), usize>,
) {
    let (source, source_version) = match package.source {
        Some(source) => (
            source.name,
            source.version.unwrap_or_else(|| package.version.clone()),
        ),
        None => (package.name.clone(), package.version.clone()),
    };
    let key = (source, source_version.clone(), location.to_owned());
    let index = if let Some(index) = candidate_indexes.get(&key) {
        *index
    } else {
        let index = candidates.len();
        candidates.push(PackageCandidate {
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
    use indoc::{indoc, writedoc};

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
    /// Preserves an unchanged view, locks it, and invalidates changed selections.
    fn maintains_persistent_view() {
        let cache = tempfile::tempdir().unwrap();
        let normal = prepare_view(cache.path(), "noble", "amd64", false, &[]).unwrap();
        let sources = cache.path().join("sources.sources");
        let status = cache.path().join("status");
        let binary = cache.path().join("pkgcache.bin");
        assert!(
            !fs::read_to_string(&sources)
                .unwrap()
                .contains("noble-proposed")
        );
        let competing = fs::File::options()
            .write(true)
            .open(cache.path().join("view.lock"))
            .unwrap();
        assert!(matches!(
            competing.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let timestamp = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        for path in [&sources, &status] {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(timestamp)
                .unwrap();
        }
        fs::write(&binary, "cached").unwrap();
        drop(normal);

        let unchanged = prepare_view(cache.path(), "noble", "amd64", false, &[]).unwrap();
        for path in [&sources, &status] {
            assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), timestamp);
        }
        assert_eq!(fs::read(&binary).unwrap(), b"cached");
        drop(unchanged);

        for (series, architecture, proposed) in [
            ("noble", "amd64", true),
            ("noble", "arm64", true),
            ("stonking", "arm64", true),
            ("noble", "amd64", false),
        ] {
            fs::write(&binary, "cached").unwrap();
            let _view = prepare_view(cache.path(), series, architecture, proposed, &[]).unwrap();
            assert!(!binary.exists());
            let contents = fs::read_to_string(&sources).unwrap();
            assert!(contents.contains(&format!("Suites: {series} {series}-updates")));
            assert!(contents.contains(&format!("Architectures: {architecture}\n")));
            assert_eq!(contents.contains(&format!("{series}-proposed")), proposed);
        }
        competing.try_lock().unwrap();
    }

    #[test]
    /// Parses versioned virtual packages from an APT Packages paragraph.
    fn parses_package_candidates() {
        let base = indoc! {r"
            Package: librust-serde-dev
            Source: rust-serde
            Version: 1.0.219-1
            Architecture: amd64
            Provides: librust-serde-1+derive-dev (= 1.0.219-1)
        "};
        let feature = indoc! {r"
            Package: librust-serde+std-dev
            Source: rust-serde
            Version: 1.0.219-1
            Architecture: amd64
            Provides: librust-serde-1+std-dev (= 1.0.219-1)
        "};
        let mut packages = NamedTempFile::new().unwrap();
        writedoc! {packages, r"
            {base}
            {feature}"
        }
        .unwrap();
        let mut candidates = Vec::new();
        let mut indexes = BTreeMap::new();
        read_index(
            packages.path(),
            "noble/universe",
            &mut candidates,
            &mut indexes,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].provides["librust-serde-1+derive-dev"]
                .as_ref()
                .unwrap()
                .to_string(),
            "1.0.219-1"
        );
        assert!(candidates[0].provides["librust-serde-1+std-dev"].is_some());
    }
}
