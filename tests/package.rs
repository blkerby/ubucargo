//! Offline CLI tests for source-package creation and reconciliation.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

use indoc::{formatdoc, indoc};
use tempfile::TempDir;

/// Filesystem state used to detect writes during checks and rejected operations.
#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory(u32),
    File(Vec<u8>, u32, SystemTime),
    Symlink(PathBuf, SystemTime),
}

/// Creates a dependency-free local crate and a private Cargo cache.
fn create_fixture() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir_all(root.join("crate/src")).unwrap();
    fs::create_dir(root.join("cargo-home")).unwrap();
    fs::write(
        root.join("crate/src/lib.rs"),
        "/// Example value.\npub const VALUE: u8 = 1;\n",
    )
    .unwrap();
    write_crate_manifest(root, "1.0.0");
    directory
}

/// Selects the upstream release without using a registry.
fn write_crate_manifest(root: &Path, version: &str) {
    fs::write(
        root.join("crate/Cargo.toml"),
        formatdoc! {r#"
            [package]
            name = "example"
            version = "{version}"
            edition = "2021"
            license = "MIT"
            description = "Example library"
        "#},
    )
    .unwrap();
}

/// Runs the package CLI with offline Cargo and an isolated cache.
fn run_package(root: &Path, arguments: &[&str], expected_status: i32) -> Output {
    run_command(
        Command::new(env!("CARGO_BIN_EXE_ubucargo"))
            .arg("package")
            .args(arguments)
            .current_dir(root)
            .env("CARGO_HOME", root.join("cargo-home"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("DEBFULLNAME", "Example Maintainer")
            .env("DEBEMAIL", "example@example.com")
            .env("LC_ALL", "C.UTF-8")
            .env("TZ", "UTC"),
        expected_status,
    )
}

/// Checks an external command's exit status, retaining output for failure diagnostics.
fn run_command(command: &mut Command, expected_status: i32) -> Output {
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{command:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

/// Captures the complete package directory and adjacent orig archives.
fn read_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    let mut tree = BTreeMap::new();
    let mut directories = vec![PathBuf::new()];
    while let Some(relative) = directories.pop() {
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path).unwrap();
        let mode = metadata.permissions().mode() & 0o7777;
        let modified = metadata.modified().unwrap();
        let entry = if metadata.is_dir() {
            for child in fs::read_dir(&path).unwrap() {
                directories.push(relative.join(child.unwrap().file_name()));
            }
            TreeEntry::Directory(mode)
        } else if metadata.is_symlink() {
            TreeEntry::Symlink(fs::read_link(&path).unwrap(), modified)
        } else {
            TreeEntry::File(fs::read(&path).unwrap(), mode, modified)
        };
        tree.insert(relative, entry);
    }
    tree
}

/// Reports the first changed path when a check or rejected operation alters package state.
fn assert_tree_unchanged(root: &Path, before: &BTreeMap<PathBuf, TreeEntry>) {
    let after = read_tree(root);
    assert_eq!(
        after.len(),
        before.len(),
        "number of filesystem entries changed"
    );
    for (path, entry) in before {
        assert_eq!(after.get(path), Some(entry), "{} changed", path.display());
    }
}

/// Creates and upgrades a package, preserving released changelogs and converging on reruns.
#[test]
fn create_and_upgrade_package() {
    let fixture = create_fixture();
    let root = fixture.path();
    let packages = root.join("packages");
    let package = packages.join("rust-example");
    run_package(
        root,
        &[
            "local:crate",
            "--package-dir",
            "packages/rust-example",
            "--check",
        ],
        1,
    );
    assert!(!packages.exists());

    run_package(
        root,
        &["local:crate", "--package-dir", "packages/rust-example"],
        0,
    );
    assert!(packages.join("rust-example_1.0.0.orig.tar.gz").is_file());
    assert!(package.join("debian/debcargo.toml").is_file());
    assert!(package.join("debian/ubucargo-state.json").is_file());
    assert_eq!(
        fs::read_to_string(package.join("debian/source/format")).unwrap(),
        "3.0 (quilt)\n"
    );
    let control = fs::read_to_string(package.join("debian/control")).unwrap();
    assert!(control.contains("Source: rust-example\n"));
    assert!(control.contains("Maintainer: Ubuntu Developers"));
    assert!(!control.contains("Vcs-Git:"));
    assert!(!control.contains("Vcs-Browser:"));

    let before = read_tree(&packages);
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);
    run_package(root, &["--package-dir", "packages/rust-example"], 0);
    assert_tree_unchanged(&packages, &before);

    write_crate_manifest(root, "1.0.1");
    run_package(root, &["pkg:packages/rust-example", "--check"], 1);
    assert_tree_unchanged(&packages, &before);
    run_package(root, &["pkg:packages/rust-example"], 0);
    assert!(packages.join("rust-example_1.0.1.orig.tar.gz").is_file());
    assert!(packages.join("rust-example_1.0.0.orig.tar.gz").is_file());
    assert!(
        fs::read_to_string(package.join("Cargo.toml"))
            .unwrap()
            .contains("version = \"1.0.1\"")
    );

    let changelog_path = package.join("debian/changelog");
    let changelog = fs::read_to_string(&changelog_path).unwrap();
    assert!(changelog.starts_with("rust-example (1.0.1-0ubuntu1) UNRELEASED;"));
    assert_eq!(changelog.matches("Generated with debcargo").count(), 1);
    let released = changelog.replacen("UNRELEASED", "noble", 1);
    fs::write(&changelog_path, &released).unwrap();
    run_package(root, &["pkg:packages/rust-example"], 0);
    let changelog = fs::read_to_string(&changelog_path).unwrap();
    assert!(changelog.starts_with("rust-example (1.0.1-0ubuntu2) UNRELEASED;"));
    assert!(changelog.ends_with(&released));

    let before = read_tree(&packages);
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);
    run_package(root, &["pkg:packages/rust-example"], 0);
    assert_tree_unchanged(&packages, &before);
}

/// Preserves edits, deletions, and modes, and resolves unknown or conflicting ownership.
#[test]
fn preserve_overrides_and_resolve_ambiguities() {
    let fixture = create_fixture();
    let root = fixture.path();
    run_package(
        root,
        &["local:crate", "--package-dir", "packages/rust-example"],
        0,
    );
    let packages = root.join("packages");
    let debian = packages.join("rust-example/debian");
    let control = fs::read_to_string(debian.join("control")).unwrap();
    let edited = format!("{control}\n# Maintainer override.\n");
    fs::write(debian.join("control"), &edited).unwrap();
    fs::remove_file(debian.join("copyright")).unwrap();
    let mode = fs::metadata(debian.join("rules"))
        .unwrap()
        .permissions()
        .mode();
    fs::set_permissions(
        debian.join("rules"),
        fs::Permissions::from_mode(mode & !0o111),
    )
    .unwrap();
    fs::write(debian.join("local-notes"), "Maintainer notes.\n").unwrap();

    let before = read_tree(&packages);
    run_package(root, &["pkg:packages/rust-example", "--check"], 1);
    assert_tree_unchanged(&packages, &before);
    run_package(root, &["pkg:packages/rust-example"], 0);
    assert_eq!(fs::read_to_string(debian.join("control")).unwrap(), edited);
    assert!(!debian.join("copyright").exists());
    assert_eq!(
        fs::metadata(debian.join("rules"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        fs::read_to_string(debian.join("local-notes")).unwrap(),
        "Maintainer notes.\n"
    );
    for name in ["control", "copyright", "rules"] {
        assert!(debian.join(format!("{name}.debcargo.hint")).is_file());
    }
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);

    // Adopting the generated alternatives relinquishes the overrides.
    for name in ["control", "copyright", "rules"] {
        fs::copy(
            debian.join(format!("{name}.debcargo.hint")),
            debian.join(name),
        )
        .unwrap();
    }
    run_package(root, &["pkg:packages/rust-example"], 0);
    for name in ["control", "copyright", "rules"] {
        assert!(!debian.join(format!("{name}.debcargo.hint")).exists());
    }
    assert_ne!(
        fs::metadata(debian.join("rules"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );

    // An edited primary without a manifest or hint requires an explicit decision.
    fs::remove_file(debian.join("ubucargo-state.json")).unwrap();
    fs::write(debian.join("control"), &edited).unwrap();
    let before = read_tree(&packages);
    let output = run_package(root, &["pkg:packages/rust-example"], 2);
    assert!(String::from_utf8_lossy(&output.stdout).contains("ambiguous debian/control"));
    assert_tree_unchanged(&packages, &before);
    run_package(
        root,
        &[
            "pkg:packages/rust-example",
            "--keep",
            "debian/control",
            "--check",
        ],
        1,
    );
    assert_tree_unchanged(&packages, &before);
    run_package(
        root,
        &["pkg:packages/rust-example", "--keep", "debian/control"],
        0,
    );
    assert_eq!(fs::read_to_string(debian.join("control")).unwrap(), edited);
    assert_eq!(
        fs::read_to_string(debian.join("control.debcargo.hint")).unwrap(),
        control
    );

    // Changing the hint conflicts with the recorded baseline; replacement restores ownership.
    fs::write(debian.join("control.debcargo.hint"), "Conflicting hint.\n").unwrap();
    let before = read_tree(&packages);
    run_package(root, &["pkg:packages/rust-example", "--check"], 2);
    run_package(
        root,
        &[
            "pkg:packages/rust-example",
            "--replace",
            "debian/control",
            "--check",
        ],
        1,
    );
    assert_tree_unchanged(&packages, &before);
    run_package(
        root,
        &["pkg:packages/rust-example", "--replace", "debian/control"],
        0,
    );
    assert_eq!(fs::read_to_string(debian.join("control")).unwrap(), control);
    assert!(!debian.join("control.debcargo.hint").exists());
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);
}

/// Rejects source conflicts before writing, and preserves local additions during a forced upgrade.
#[test]
fn preserve_local_source_and_reject_conflicts() {
    let fixture = create_fixture();
    let root = fixture.path();
    let upstream = root.join("crate");
    fs::write(upstream.join("data.txt"), "Original data.\n").unwrap();
    fs::write(upstream.join("obsolete.txt"), "Old upstream file.\n").unwrap();
    fs::write(upstream.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(upstream.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    run_package(
        root,
        &["local:crate", "--package-dir", "packages/rust-example"],
        0,
    );
    let packages = root.join("packages");
    let package = packages.join("rust-example");
    fs::create_dir(package.join(".git")).unwrap();
    fs::write(package.join(".git/config"), "Local repository.\n").unwrap();
    fs::write(package.join("notes.txt"), "Local notes.\n").unwrap();
    symlink("notes.txt", package.join("notes-link")).unwrap();
    fs::write(package.join("src/lib.rs"), "// Locally edited upstream.\n").unwrap();
    fs::remove_file(package.join("data.txt")).unwrap();
    fs::write(package.join("collision.txt"), "Local addition.\n").unwrap();

    write_crate_manifest(root, "1.0.1");
    fs::write(upstream.join("data.txt"), "New upstream data.\n").unwrap();
    fs::write(upstream.join("collision.txt"), "New upstream file.\n").unwrap();
    fs::remove_file(upstream.join("obsolete.txt")).unwrap();
    let before = read_tree(&packages);
    let output = run_package(root, &["pkg:packages/rust-example"], 2);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("source conflicts:"));
    for path in ["src/lib.rs", "data.txt", "collision.txt"] {
        assert!(error.contains(path), "{error}");
    }
    assert_tree_unchanged(&packages, &before);
    run_package(
        root,
        &["pkg:packages/rust-example", "--force", "--check"],
        1,
    );
    assert_tree_unchanged(&packages, &before);
    run_package(root, &["pkg:packages/rust-example", "--force"], 0);
    assert_eq!(
        fs::read(package.join("src/lib.rs")).unwrap(),
        fs::read(upstream.join("src/lib.rs")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(package.join("data.txt")).unwrap(),
        "New upstream data.\n"
    );
    assert_eq!(
        fs::read_to_string(package.join("collision.txt")).unwrap(),
        "New upstream file.\n"
    );
    assert!(!package.join("obsolete.txt").exists());
    assert_ne!(
        fs::metadata(package.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        fs::read_to_string(package.join(".git/config")).unwrap(),
        "Local repository.\n"
    );
    assert_eq!(
        fs::read_to_string(package.join("notes.txt")).unwrap(),
        "Local notes.\n"
    );
    assert_eq!(
        fs::read_link(package.join("notes-link")).unwrap(),
        Path::new("notes.txt")
    );
    let before = read_tree(&packages);
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);
    assert_tree_unchanged(&packages, &before);
}

/// Repacks orig archives while retaining maintainer patches and verifying their application.
#[test]
fn repack_source_and_preserve_patches() {
    let fixture = create_fixture();
    let root = fixture.path();
    fs::write(root.join("crate/bundled.txt"), "Excluded upstream data.\n").unwrap();
    run_package(
        root,
        &["local:crate", "--package-dir", "packages/rust-example"],
        0,
    );
    let packages = root.join("packages");
    let package = packages.join("rust-example");
    let debian = package.join("debian");
    let original_orig = packages.join("rust-example_1.0.0.orig.tar.gz");
    let original_bytes = fs::read(&original_orig).unwrap();
    let patch = indoc! {"
        Description: Change the example value.
        --- a/src/lib.rs
        +++ b/src/lib.rs
        @@ -1,2 +1,2 @@
         /// Example value.
        -pub const VALUE: u8 = 1;
        +pub const VALUE: u8 = 2;
    "};
    fs::create_dir_all(debian.join("patches")).unwrap();
    fs::write(debian.join("patches/example.patch"), patch).unwrap();
    let series = "# Maintainer patch stack.\nexample.patch\n";
    fs::write(debian.join("patches/series"), series).unwrap();
    let config_path = debian.join("debcargo.toml");
    let original_config = fs::read_to_string(&config_path).unwrap();
    let repack_config =
        format!("{original_config}excludes = [\"bundled.txt\"]\nrepack_suffix = \"dfsg\"\n");
    fs::write(&config_path, &repack_config).unwrap();

    let before = read_tree(&packages);
    run_package(root, &["pkg:packages/rust-example", "--check"], 1);
    assert_tree_unchanged(&packages, &before);
    run_package(root, &["pkg:packages/rust-example"], 0);
    let repacked_orig = packages.join("rust-example_1.0.0+dfsg.orig.tar.gz");
    assert!(repacked_orig.is_file());
    assert_eq!(fs::read(&original_orig).unwrap(), original_bytes);
    assert!(!package.join("bundled.txt").exists());
    let output = run_command(
        Command::new("tar")
            .arg("--list")
            .arg("--file")
            .arg(&repacked_orig),
        0,
    );
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(listing.contains("/src/lib.rs"));
    assert!(!listing.contains("bundled.txt"));
    assert!(
        fs::read_to_string(debian.join("changelog"))
            .unwrap()
            .starts_with("rust-example (1.0.0+dfsg-0ubuntu1) UNRELEASED;")
    );
    assert_eq!(fs::read_to_string(&config_path).unwrap(), repack_config);
    assert_eq!(
        fs::read_to_string(debian.join("patches/example.patch")).unwrap(),
        patch
    );
    assert_eq!(
        fs::read_to_string(debian.join("patches/series")).unwrap(),
        series
    );
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);

    // Removing the filter on the next release restores the file and drops the suffix.
    fs::write(&config_path, &original_config).unwrap();
    write_crate_manifest(root, "1.0.1");
    run_package(root, &["pkg:packages/rust-example"], 0);
    assert!(package.join("bundled.txt").is_file());
    assert!(packages.join("rust-example_1.0.1.orig.tar.gz").is_file());
    assert_eq!(fs::read(&original_orig).unwrap(), original_bytes);
    assert!(
        fs::read_to_string(debian.join("changelog"))
            .unwrap()
            .starts_with("rust-example (1.0.1-0ubuntu1) UNRELEASED;")
    );
    run_package(root, &["pkg:packages/rust-example", "--check"], 0);

    run_command(
        Command::new("quilt")
            .args(["push", "--quiltrc=-", "-a"])
            .env("QUILT_PATCHES", "debian/patches")
            .current_dir(&package),
        0,
    );
    assert!(
        fs::read_to_string(package.join("src/lib.rs"))
            .unwrap()
            .contains("VALUE: u8 = 2")
    );
    // Unrefreshed changes must be rejected before any package files are written.
    fs::write(
        package.join("src/lib.rs"),
        "/// Example value.\npub const VALUE: u8 = 3;\n",
    )
    .unwrap();
    let before = read_tree(&packages);
    let output = run_package(root, &["pkg:packages/rust-example", "--check"], 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrefreshed changes"));
    assert_tree_unchanged(&packages, &before);
}
