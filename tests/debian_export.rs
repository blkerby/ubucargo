//! Offline CLI coverage for exporting overlays to debcargo-conf.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

use indoc::{formatdoc, indoc};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const CONTROL: &str = indoc! {"
    Source: rust-example-crate
    Maintainer: Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>
    XSBC-Original-Maintainer: Debian Rust Maintainers <pkg-rust-maintainers@alioth-lists.debian.net>
    Build-Depends: debhelper-compat (= 13)

    Package: librust-example-crate-dev
    Architecture: any
    Description: Example
"};
const RELEASED_CHANGELOG: &str = indoc! {"
    rust-example-crate (1.1.0-0ubuntu1) noble; urgency=medium

      * Published Ubuntu package.

     -- Ubuntu Maintainer <ubuntu@example.com>  Mon, 01 Jan 2024 12:00:00 +0000
"};

/// Creates registry-backed packaging with known generator baselines and two unpublished entries.
fn create_fixture() -> TempDir {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fs::create_dir_all(root.join("package/debian")).unwrap();
    fs::create_dir(root.join("package/src")).unwrap();
    fs::create_dir_all(root.join("repository/src")).unwrap();
    fs::write(
        root.join("package/Cargo.toml"),
        indoc! {r#"
            [package]
            name = "example_crate"
            version = "1.2.0"
        "#},
    )
    .unwrap();
    fs::write(
        root.join("package/debian/debcargo.toml"),
        indoc! {r#"
            # Maintained configuration
            collapse_features = true
            maintainer = "Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>"
        "#},
    )
    .unwrap();
    let mut changelog = String::new();
    for version in ["1.2.0", "1.1.1"] {
        changelog.push_str(&formatdoc! {"
            rust-example-crate ({version}-0ubuntu1) UNRELEASED; urgency=medium

              * Unpublished changes.

             -- Ubuntu Maintainer <ubuntu@example.com>  Tue, 02 Jan 2024 12:00:00 +0000

        "});
    }
    changelog.push_str(RELEASED_CHANGELOG);
    fs::write(root.join("package/debian/changelog"), changelog).unwrap();
    let mut files = serde_json::Map::new();
    for (name, contents, executable) in [
        ("control", CONTROL, false),
        ("copyright", "Generated copyright\n", false),
        (
            "rules",
            indoc! {"
                #!/usr/bin/make -f
                %:
                \tdh $@
            "},
            true,
        ),
        ("watch", "version=4\n", false),
        (
            "upstream/metadata",
            "Repository: https://example.com/crate\n",
            false,
        ),
        ("tests/control", "Tests: generated\n", false),
    ] {
        let path = root.join("package/debian").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )
        .unwrap();
        let mut sha256 = String::new();
        for byte in Sha256::digest(contents.as_bytes()) {
            sha256.push_str(&format!("{byte:02x}"));
        }
        files.insert(
            format!("debian/{name}"),
            serde_json::json!({
                "sha256": sha256,
                "executable": executable,
            }),
        );
    }
    fs::write(
        root.join("package/debian/ubucargo-state.json"),
        serde_json::to_vec(&serde_json::json!({"version": 1, "files": files})).unwrap(),
    )
    .unwrap();
    fixture
}

/// Runs the export command with a fixed identity and includes output in status failures.
fn run_export(root: &Path, arguments: &[&str], status: i32) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_ubucargo"))
        .arg("debian-export")
        .args(arguments)
        .current_dir(root)
        .env("DEBFULLNAME", "Debian Maintainer")
        .env("DEBEMAIL", "debian@example.com")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(status),
        indoc! {"
            stdout:
            {}
            stderr:
            {}"},
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Preserves overrides, restores Debian metadata, and replaces stale output only when requested.
#[test]
fn export_overrides_and_replace_previous_export() {
    let fixture = create_fixture();
    let root = fixture.path();
    let debian = root.join("package/debian");
    fs::write(
        debian.join("control"),
        CONTROL.replace(
            "debhelper-compat (= 13)",
            "debhelper-compat (= 13), libcustom-dev",
        ),
    )
    .unwrap();
    fs::write(debian.join("control.debcargo.hint"), CONTROL).unwrap();
    fs::write(debian.join("copyright"), "Reviewed copyright\n").unwrap();
    fs::remove_file(debian.join("watch")).unwrap();
    fs::create_dir_all(debian.join("patches/auto")).unwrap();
    fs::write(debian.join("patches/auto/generated.patch"), "generated\n").unwrap();
    fs::write(debian.join("patches/fix.patch"), "maintained\n").unwrap();
    fs::write(
        debian.join("patches/series"),
        indoc! {"
            auto/generated.patch
            # Maintained patches
            fix.patch -p1
        "},
    )
    .unwrap();
    std::os::unix::fs::symlink("copyright", debian.join("copyright-link")).unwrap();
    let output = run_export(root, &["package", "--debcargo-conf", "repository"], 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("deletion of debian/watch"));
    let exported = root.join("repository/src/example-crate/debian");
    let control = fs::read_to_string(exported.join("control")).unwrap();
    assert!(control.contains("Maintainer: Debian Rust Maintainers"));
    assert!(control.contains(
        "Vcs-Git: https://salsa.debian.org/rust-team/debcargo-conf.git [src/example-crate]"
    ));
    assert!(control.contains("Vcs-Browser: https://salsa.debian.org/rust-team/debcargo-conf/tree/master/src/example-crate"));
    assert!(control.contains("libcustom-dev"));
    assert!(!control.contains("Original-Maintainer:"));
    assert!(!exported.join("control.debcargo.hint").exists());
    assert!(!exported.join("ubucargo-state.json").exists());
    assert!(!exported.join("rules").exists());
    assert!(!exported.join("patches/auto").exists());
    assert_eq!(
        fs::read_to_string(exported.join("copyright")).unwrap(),
        "Reviewed copyright\n"
    );
    assert_eq!(
        fs::read_link(exported.join("copyright-link")).unwrap(),
        Path::new("copyright")
    );
    assert_eq!(
        fs::read_to_string(exported.join("patches/series")).unwrap(),
        indoc! {"
            # Maintained patches
            fix.patch -p1
        "}
    );
    let changelog = fs::read_to_string(exported.join("changelog")).unwrap();
    assert!(changelog.starts_with("rust-example-crate (1.2.0-1) UNRELEASED;"));
    assert!(changelog.ends_with(RELEASED_CHANGELOG));
    assert!(changelog.contains(&format!(
        "* Exported using ubucargo {}.\n",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(!changelog.contains("Unpublished changes"));
    assert!(changelog.contains("Debian Maintainer <debian@example.com>"));
    let config = fs::read_to_string(exported.join("debcargo.toml")).unwrap();
    assert!(config.contains("# Maintained configuration"));
    assert!(config.contains("overlay = \".\""));
    assert!(!config.contains("maintainer ="));

    fs::write(exported.join("stale"), "previous edit").unwrap();
    let output = run_export(root, &["package", "--debcargo-conf", "repository"], 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Use --overwrite"));
    fs::write(debian.join("control.debcargo.hint"), "conflicting hint").unwrap();
    let manifest_path = debian.join("ubucargo-state.json");
    let manifest = fs::read(&manifest_path).unwrap();
    fs::write(&manifest_path, "invalid manifest").unwrap();
    run_export(
        root,
        &["package", "--debcargo-conf", "repository", "--overwrite"],
        2,
    );
    assert!(exported.join("stale").exists());
    fs::write(&manifest_path, manifest).unwrap();
    fs::write(
        debian.join("debcargo.toml"),
        indoc! {r#"
            maintainer = "Custom Team <team@example.com>"
            [source]
            vcs_git = "https://example.com/package.git"
            vcs_browser = "https://example.com/package"
        "#},
    )
    .unwrap();
    fs::create_dir(debian.join("source")).unwrap();
    fs::write(debian.join("source/format"), "1.0\n").unwrap();
    run_export(
        root,
        &["package", "--debcargo-conf", "repository", "--overwrite"],
        0,
    );
    assert!(!exported.join("stale").exists());
    assert!(!exported.join("control.debcargo.hint").exists());
    let control = fs::read_to_string(exported.join("control")).unwrap();
    assert!(control.contains("Maintainer: Custom Team <team@example.com>"));
    assert!(control.contains("Vcs-Git: https://example.com/package.git"));
    assert!(control.contains("Vcs-Browser: https://example.com/package"));
    assert_eq!(
        fs::read_to_string(exported.join("source/format")).unwrap(),
        "1.0\n"
    );
    assert!(
        fs::read_to_string(debian.join("control"))
            .unwrap()
            .contains("Maintainer: Ubuntu Developers")
    );
}

/// Discovers the package, omits generated copyright, and handles wholly unpublished packaging.
#[test]
fn export_unpublished_package_with_semver_suffix() {
    let fixture = create_fixture();
    let root = fixture.path();
    let debian = root.join("package/debian");
    let changelog = fs::read_to_string(debian.join("changelog"))
        .unwrap()
        .replace(RELEASED_CHANGELOG, "");
    fs::write(debian.join("changelog"), changelog).unwrap();
    for directory in ["tests/nested", "source", "patches/auto"] {
        fs::create_dir_all(debian.join(directory)).unwrap();
    }
    fs::write(debian.join("source/format"), "3.0 (quilt)\n").unwrap();
    fs::write(debian.join("patches/auto/generated.patch"), "generated\n").unwrap();
    fs::write(debian.join("patches/series"), "auto/generated.patch\n").unwrap();
    fs::write(debian.join("debcargo.toml"), "semver_suffix = true\n").unwrap();
    run_export(
        &root.join("package/src"),
        &["--debcargo-conf", "../../repository"],
        0,
    );
    let exported = root.join("repository/src/example-crate-1/debian");
    assert!(!exported.join("control").exists());
    assert!(!exported.join("copyright").exists());
    for directory in ["upstream", "tests", "source", "patches"] {
        assert!(
            !exported.join(directory).exists(),
            "{directory} was exported"
        );
    }
    let changelog = fs::read_to_string(exported.join("changelog")).unwrap();
    assert!(changelog.starts_with("rust-example-crate-1 (1.2.0-1) UNRELEASED;"));
    assert_eq!(changelog.matches("urgency=").count(), 1);
    assert!(changelog.contains(&format!(
        "* Exported using ubucargo {}.\n",
        env!("CARGO_PKG_VERSION")
    )));
}
