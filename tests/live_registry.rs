//! Tests that talk to the real npm registry.
//!
//! **Not run by `cargo test`.** They carry `#[ignore]`, which is what keeps the
//! suite offline by default, and they check `CODETS_LIVE_REGISTRY=1` as well so that
//! `cargo test -- --ignored` does not silently start reaching the network either.
//! CI never runs them: Actions minutes are limited, and a registry hiccup should not
//! look like a broken build. They are run by hand before a release, which is what
//! `scripts/verify-demo.*` does with a whole project.
//!
//! Every one of them points the user config at a file that does not exist, so the
//! machine's own `~/.npmrc` cannot decide what they test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;

use support::Scratch;

/// Whether the live tests were asked for.
fn enabled() -> bool {
    if std::env::var("CODETS_LIVE_REGISTRY").as_deref() == Ok("1") {
        true
    } else {
        eprintln!("skipping: set CODETS_LIVE_REGISTRY=1 to run this against the real registry");
        false
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Run a binary with the machine's own npm configuration kept out of the way.
fn run(program: &str, args: &[&str], home: &Path, cwd: &Path) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("CODETS_HOME", home)
        .env_remove("CODETS_NODE_VERSION")
        .env_remove("NPM_CONFIG_REGISTRY")
        .env_remove("npm_config_registry")
        .env("NPM_CONFIG_USERCONFIG", cwd.join("no-such-user-npmrc"))
        .output()
        .expect("running the toolchain")
}

fn npm() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_npm"))
}

fn node() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_node"))
}

#[test]
#[ignore = "needs the real npm registry: run with --ignored and CODETS_LIVE_REGISTRY=1"]
fn installs_a_real_package_tree_and_runs_it() {
    if !enabled() {
        return;
    }
    let scratch = Scratch::new("live-install");
    let project = scratch.dir("project");
    let home = scratch.dir("home");
    std::fs::write(
        project.join("package.json"),
        "{ \"name\": \"live\", \"version\": \"1.0.0\", \"dependencies\": { \"is-odd\": \"^3.0.1\" } }",
    )
    .expect("manifest");
    // `is-odd` depends on `is-number`, so a successful run proves the transitive
    // fetch and the `node_modules` walk both work against the real thing.
    std::fs::write(
        project.join("index.js"),
        "const isOdd = require('is-odd');\nconsole.log(isOdd(3), isOdd(2));\n",
    )
    .expect("program");

    let install = run(
        npm().to_str().expect("a utf-8 path"),
        &["install", "--jobs", "4"],
        &home,
        &project,
    );
    assert_eq!(install.status.code(), Some(0), "{}", stderr(&install));
    assert!(
        project.join("node_modules/is-odd/package.json").is_file(),
        "is-odd was not installed: {}",
        stdout(&install)
    );
    assert!(
        project.join("node_modules/is-number/package.json").is_file(),
        "the transitive dependency was not installed"
    );

    // The lockfile records where the packages came from, and against the real
    // registry that is an https URL rather than a local path.
    let lock = std::fs::read_to_string(project.join("package-lock.json")).expect("lockfile");
    assert!(lock.contains("\"resolved\": \"https://"), "{lock}");

    let executed = run(
        node().to_str().expect("a utf-8 path"),
        &["index.js"],
        &home,
        &project,
    );
    assert_eq!(executed.status.code(), Some(0), "{}", stderr(&executed));
    assert_eq!(stdout(&executed), "true false");
}

#[test]
#[ignore = "needs the real npm registry: run with --ignored and CODETS_LIVE_REGISTRY=1"]
fn a_project_can_point_at_a_mirror() {
    if !enabled() {
        return;
    }
    // The mirror case, through the chain the users would use: a `.npmrc` in the
    // project, no flag on the command line. npmmirror is a read-only mirror of
    // npmjs, so the same package resolves either way.
    let scratch = Scratch::new("live-mirror");
    let project = scratch.dir("project");
    let home = scratch.dir("home");
    std::fs::write(
        project.join("package.json"),
        "{ \"name\": \"mirror\", \"version\": \"1.0.0\", \"dependencies\": { \"is-odd\": \"^3.0.1\" } }",
    )
    .expect("manifest");
    std::fs::write(project.join(".npmrc"), "registry=https://registry.npmmirror.com/\n")
        .expect("npmrc");

    let chosen = run(
        npm().to_str().expect("a utf-8 path"),
        &["config", "get", "registry", "--origin"],
        &home,
        &project,
    );
    // The value is printed as the file spells it, trailing slash and all, which is
    // what npm does; where it came from goes to stderr.
    assert_eq!(stdout(&chosen), "https://registry.npmmirror.com/");
    assert!(stderr(&chosen).contains(".npmrc"), "{}", stderr(&chosen));

    let install = run(
        npm().to_str().expect("a utf-8 path"),
        &["install"],
        &home,
        &project,
    );
    assert_eq!(install.status.code(), Some(0), "{}", stderr(&install));
    assert!(
        project.join("node_modules/is-odd/package.json").is_file(),
        "the mirror did not serve the package: {}",
        stdout(&install)
    );
}

#[test]
#[ignore = "needs the real npm registry: run with --ignored and CODETS_LIVE_REGISTRY=1"]
fn a_package_that_does_not_exist_fails_readably() {
    if !enabled() {
        return;
    }
    let scratch = Scratch::new("live-missing");
    let project = scratch.dir("project");
    let home = scratch.dir("home");
    std::fs::write(
        project.join("package.json"),
        "{ \"name\": \"missing\", \"version\": \"1.0.0\", \"dependencies\": { \"codets-package-that-does-not-exist-9x8y7z\": \"^1.0.0\" } }",
    )
    .expect("manifest");

    let install = run(
        npm().to_str().expect("a utf-8 path"),
        &["install"],
        &home,
        &project,
    );
    // A missing package is a warning and an empty install, not a panic and not a
    // command failure, which is what npm does. The warning names the registry's
    // answer, because "added 0 packages" on its own says nothing about why.
    assert_eq!(install.status.code(), Some(0), "{}", stderr(&install));
    assert!(
        stdout(&install).contains("added 0 packages"),
        "{}",
        stdout(&install)
    );
    let warnings = stderr(&install);
    assert!(
        warnings.contains("404"),
        "the warning should say what the registry answered: {warnings}"
    );
    assert!(
        warnings.contains("codets-package-that-does-not-exist-9x8y7z"),
        "the warning should name the package: {warnings}"
    );
}
