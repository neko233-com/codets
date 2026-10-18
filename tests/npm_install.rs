//! Installer tests: resolution, the parallel transfer, integrity, bin links and
//! the lockfile.
//!
//! Tarballs are generated here rather than committed, so the tests exercise the
//! real gunzip + untar path with real digests instead of trusting a fixture.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use codets::npm::install::{base64_encode, install, InstallOptions};
use codets::npm::registry::Registry;
use sha2::{Digest, Sha512};

/// A unique scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "codets-npm-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.subsec_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, body).expect("write");
}

/// Bundle `entries` into a `.tgz` the way `npm pack` would.
fn tarball(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, *path, body.as_bytes())
            .expect("append");
    }
    let tar_bytes = builder.into_inner().expect("finish tar");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("gzip finish")
}

/// The `sha512-...` integrity string for a tarball.
fn integrity_of(bytes: &[u8]) -> String {
    format!("sha512-{}", base64_encode(&Sha512::digest(bytes)))
}

/// Build a registry holding one package, published as a tarball.
fn registry_with_tarball(root: &Path, name: &str, version: &str, claimed: Option<&str>) -> Vec<u8> {
    let body = format!("module.exports = '{name}@{version}';");
    let bytes = tarball(&[
        (
            "package/package.json",
            &format!(
                "{{ \"name\": \"{name}\", \"version\": \"{version}\", \"main\": \"index.js\" }}"
            ),
        ),
        ("package/index.js", &body),
    ]);
    let integrity = claimed
        .map(|value| value.to_string())
        .unwrap_or_else(|| integrity_of(&bytes));
    write(&root.join(format!("{name}.json")), &format!(
        "{{\n  \"name\": \"{name}\",\n  \"versions\": {{\n    \"{version}\": {{\n      \"dist\": {{ \"tarball\": \"{name}-{version}.tgz\", \"integrity\": \"{integrity}\" }}\n    }}\n  }}\n}}"
    ));
    std::fs::create_dir_all(root.join(name)).expect("package dir");
    std::fs::write(root.join(name).join(format!("{version}.tgz")), &bytes).expect("tarball");
    bytes
}

fn project_json(root: &Path, dependencies: &str) {
    write(
        &root.join("package.json"),
        &format!(
            "{{ \"name\": \"consumer\", \"version\": \"1.0.0\", \"dependencies\": {{ {dependencies} }} }}"
        ),
    );
}

fn options(registry: &Path, jobs: usize) -> InstallOptions {
    InstallOptions {
        registry: Registry::open(&registry.to_string_lossy()).expect("registry"),
        jobs,
        include_dev: true,
        clean: true,
    }
}

#[test]
fn installs_a_package_from_a_tarball() {
    let root = scratch("tarball");
    let registry = root.join("registry");
    std::fs::create_dir_all(&registry).expect("registry");
    registry_with_tarball(&registry, "solo", "1.2.3", None);
    let project = root.join("project");
    project_json(&project, "\"solo\": \"^1.0.0\"");

    let report = install(&project, &options(&registry, 4)).expect("install");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.packages.len(), 1);
    assert_eq!(report.packages[0].version.to_string(), "1.2.3");

    let installed = project.join("node_modules").join("solo");
    assert!(installed.join("package.json").is_file());
    assert!(installed.join("index.js").is_file());
    // The leading `package/` component must be stripped on the way out.
    assert!(!installed.join("package").exists(), "archive prefix was kept");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_bad_integrity_stops_that_package() {
    let root = scratch("integrity");
    let registry = root.join("registry");
    std::fs::create_dir_all(&registry).expect("registry");
    registry_with_tarball(
        &registry,
        "tampered",
        "1.0.0",
        Some("sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="),
    );
    let project = root.join("project");
    project_json(&project, "\"tampered\": \"^1.0.0\"");

    let report = install(&project, &options(&registry, 2)).expect("install runs");
    assert!(report.packages.is_empty(), "a tampered package was installed");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("failed integrity verification")),
        "{:?}",
        report.warnings
    );
    assert!(!project.join("node_modules").join("tampered").join("index.js").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verifies_a_correct_integrity() {
    let root = scratch("integrity-ok");
    let registry = root.join("registry");
    std::fs::create_dir_all(&registry).expect("registry");
    registry_with_tarball(&registry, "signed", "2.0.0", None);
    let project = root.join("project");
    project_json(&project, "\"signed\": \"^2.0.0\"");
    let report = install(&project, &options(&registry, 3)).expect("install");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.packages.len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn resolves_and_installs_transitively() {
    // The committed fixture registry holds `greeter`, whose 1.1.0 depends on
    // `shout`; both are stored as extracted directories.
    let registry = fixture_registry();
    let root = scratch("transitive");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project");
    project_json(&project, "\"greeter\": \"^1.0.0\"");

    let report = install(&project, &options(&registry, 4)).expect("install");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let names: Vec<&str> = report.packages.iter().map(|package| package.name.as_str()).collect();
    assert_eq!(names, vec!["greeter", "shout"]);
    // `^1.0.0` must pick the newest matching version, not the first published.
    let greeter = report
        .packages
        .iter()
        .find(|package| package.name == "greeter")
        .expect("greeter");
    assert_eq!(greeter.version.to_string(), "1.1.0");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn one_thread_and_many_threads_agree() {
    let registry = fixture_registry();
    let root = scratch("jobs");
    let serial_project = root.join("serial");
    let parallel_project = root.join("parallel");
    std::fs::create_dir_all(&serial_project).expect("project");
    std::fs::create_dir_all(&parallel_project).expect("project");
    project_json(&serial_project, "\"greeter\": \"^1.0.0\"");
    project_json(&parallel_project, "\"greeter\": \"^1.0.0\"");

    let serial = install(&serial_project, &options(&registry, 1)).expect("serial install");
    let parallel = install(&parallel_project, &options(&registry, 8)).expect("parallel install");

    // Same packages, same versions, same files: the only difference should be time.
    let serial_versions: Vec<String> = serial
        .packages
        .iter()
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect();
    let parallel_versions: Vec<String> = parallel
        .packages
        .iter()
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect();
    assert_eq!(serial_versions, parallel_versions);
    assert_eq!(parallel.jobs, 2, "two packages cannot use more than two workers");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn writes_an_npm_shaped_lockfile() {
    let registry = fixture_registry();
    let root = scratch("lockfile");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project");
    project_json(&project, "\"greeter\": \"^1.0.0\"");
    install(&project, &options(&registry, 2)).expect("install");

    let lock = std::fs::read_to_string(project.join("package-lock.json")).expect("lockfile");
    assert!(lock.contains("\"lockfileVersion\": 3"), "{lock}");
    assert!(lock.contains("\"node_modules/greeter\""), "{lock}");
    assert!(lock.contains("\"node_modules/shout\""), "{lock}");
    // It must be valid JSON, which is the property another tool would depend on.
    assert!(codets::builtins::json::parse(&lock).is_some(), "{lock}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn links_declared_binaries_into_dot_bin() {
    let root = scratch("bins");
    let registry = root.join("registry");
    std::fs::create_dir_all(&registry).expect("registry");
    // A package with a `bin`, published extracted so the test stays about links.
    write(
        &registry.join("cli.json"),
        "{ \"versions\": { \"1.0.0\": { \"dist\": { \"tarball\": \"cli-1.0.0.tgz\" } } } }",
    );
    write(
        &registry.join("cli/1.0.0/package.json"),
        "{ \"name\": \"cli\", \"version\": \"1.0.0\", \"bin\": { \"cli-tool\": \"bin.js\" } }",
    );
    write(
        &registry.join("cli/1.0.0/bin.js"),
        "console.log('cli ran');",
    );
    let project = root.join("project");
    project_json(&project, "\"cli\": \"^1.0.0\"");
    install(&project, &options(&registry, 2)).expect("install");

    let bin_dir = project.join("node_modules").join(".bin");
    let shim = if cfg!(windows) {
        bin_dir.join("cli-tool.cmd")
    } else {
        bin_dir.join("cli-tool")
    };
    assert!(shim.is_file(), "missing shim {}", shim.display());
    let body = std::fs::read_to_string(&shim).expect("shim");
    assert!(body.contains("bin.js"), "{body}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_package_range_that_matches_nothing_is_a_warning_not_a_panic() {
    let registry = fixture_registry();
    let root = scratch("nomatch");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project");
    project_json(&project, "\"greeter\": \"^99.0.0\"");
    let report = install(&project, &options(&registry, 2)).expect("install runs");
    assert!(report.packages.is_empty());
    assert!(!report.warnings.is_empty(), "expected a warning");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nothing_installed_leaves_an_empty_node_modules() {
    let registry = fixture_registry();
    let root = scratch("nodeps");
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project");
    project_json(&project, "");
    let report = install(&project, &options(&registry, 2)).expect("install");
    assert!(report.packages.is_empty());
    assert!(project.join("node_modules").is_dir());
    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------------------ the whole chain

/// The committed fixture registry.
fn fixture_registry() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npm/registry")
}

#[test]
fn npm_install_then_node_runs_the_program() {
    // The end-to-end property: `npm install` followed by `node index.js` produces
    // the program's output, with the transitive `require` resolving.
    let root = scratch("e2e");
    let project = root.join("app");
    std::fs::create_dir_all(&project).expect("project");
    let fixture_project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npm/project");
    for entry in std::fs::read_dir(&fixture_project).expect("fixture project") {
        let entry = entry.expect("entry");
        std::fs::copy(entry.path(), project.join(entry.file_name())).expect("copy fixture");
    }

    let install_output = Command::new(env!("CARGO_BIN_EXE_npm"))
        .args([
            "install",
            "--registry",
            fixture_registry().to_string_lossy().as_ref(),
            "--jobs",
            "4",
        ])
        .current_dir(&project)
        .output()
        .expect("npm install");
    assert!(
        install_output.status.success(),
        "{}",
        String::from_utf8_lossy(&install_output.stderr)
    );
    let summary = String::from_utf8_lossy(&install_output.stdout);
    assert!(summary.contains("added 2 packages"), "{summary}");
    assert!(summary.contains("threads"), "{summary}");

    let run_output = Command::new(env!("CARGO_BIN_EXE_node"))
        .arg("index.js")
        .current_dir(&project)
        .output()
        .expect("node index.js");
    assert!(
        run_output.status.success(),
        "{}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    let printed = String::from_utf8_lossy(&run_output.stdout);
    // `shout` upper-cases what `greeter` passes it, which proves the transitive
    // require resolved through `node_modules`.
    assert!(printed.contains("HI WORLD"), "{printed}");
    let _ = std::fs::remove_dir_all(&root);
}
