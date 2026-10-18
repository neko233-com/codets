//! Running the same command twice must leave the same state behind.
//!
//! These are the commands that write to the version store or to a project, driven
//! through the real binaries. Each one is run twice and the result compared byte for
//! byte, because "idempotent" is a claim that has to be checked rather than
//! reasoned about: a re-run that rewrites a file differently is a diff in somebody's
//! repositories, and a re-run that installs a second copy is a disk full of them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;

use support::{tree, Scratch};

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// Run a command against a chosen store, from a chosen directory.
fn run_in(program: &str, args: &[&str], home: &Path, cwd: &Path) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("CODETS_HOME", home)
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("running the toolchain")
}

fn codets() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codets"))
}

fn npm() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_npm"))
}

/// Run `action` twice, requiring both runs to succeed and the store to be identical
/// afterwards. Returns what the store looked like.
fn twice(home: &Path, what: &str, mut action: impl FnMut() -> Output) -> Vec<String> {
    let first = action();
    assert_eq!(first.status.code(), Some(0), "first {what}: {}", stderr(&first));
    let after_first = tree(home);

    let second = action();
    assert_eq!(second.status.code(), Some(0), "second {what}: {}", stderr(&second));
    let after_second = tree(home);

    assert_eq!(
        after_first, after_second,
        "running `{what}` a second time changed the store"
    );
    after_first
}

#[test]
fn installing_a_level_twice_leaves_the_same_store() {
    let scratch = Scratch::new("idempotent-install");
    let home = scratch.dir("home");
    let cwd = scratch.dir("cwd");
    let binary = codets();

    let store = twice(&home, "codets install 22", || {
        run_in(binary.to_str().expect("a utf-8 path"), &["install", "22"], &home, &cwd)
    });
    assert!(!store.is_empty(), "the first install wrote nothing");
    assert!(
        home.join("versions").join("22.12.0").is_dir(),
        "the level directory is missing"
    );
    // Installing must not switch the active level; only `use` does that.
    assert!(
        !home.join("bin").join(if cfg!(windows) { "node.exe" } else { "node" }).exists(),
        "installing a level put it on PATH"
    );
}

#[test]
fn setting_the_default_twice_leaves_the_same_settings() {
    let scratch = Scratch::new("idempotent-default");
    let home = scratch.dir("home");
    let cwd = scratch.dir("cwd");
    let binary = codets();

    twice(&home, "codets default 22", || {
        run_in(binary.to_str().expect("a utf-8 path"), &["default", "22"], &home, &cwd)
    });
    let settings = std::fs::read_to_string(home.join("settings.conf")).expect("settings");
    assert_eq!(settings, "default=22.12.0\n");
}

#[test]
fn bootstrapping_twice_leaves_the_same_store() {
    let scratch = Scratch::new("idempotent-bootstrap");
    let home = scratch.dir("home");
    let cwd = scratch.dir("cwd");
    let binary = codets();

    let store = twice(&home, "codets bootstrap", || {
        run_in(binary.to_str().expect("a utf-8 path"), &["bootstrap", "--level", "22"], &home, &cwd)
    });
    assert!(store.iter().any(|entry| entry.contains("bin/")), "{store:?}");
}

#[test]
fn upgrading_from_the_same_build_twice_leaves_the_same_store() {
    let scratch = Scratch::new("idempotent-upgrade");
    let home = scratch.dir("home");
    let cwd = scratch.dir("cwd");
    let binary = codets();
    let source = binary.to_str().expect("a utf-8 path").to_string();

    twice(&home, "codets upgrade --from", || {
        run_in(&source, &["upgrade", "--from", &source], &home, &cwd)
    });
    assert!(home.join("current").is_file(), "the upgrade did not activate a level");
}

#[test]
fn pinning_a_project_twice_leaves_the_same_file() {
    let scratch = Scratch::new("idempotent-pin");
    let home = scratch.dir("home");
    let cwd = scratch.dir("project");
    let binary = codets();

    twice(&home, "codets use --local", || {
        run_in(binary.to_str().expect("a utf-8 path"), &["use", "22", "--local"], &home, &cwd)
    });
    let pin = std::fs::read_to_string(cwd.join(".codets-version")).expect("the pin file");
    assert_eq!(pin.trim(), "22.12.0");
}

#[test]
fn npm_install_twice_leaves_the_same_project() {
    let scratch = Scratch::new("idempotent-npm");
    let registry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npm/registry");
    let project = scratch.dir("project");
    std::fs::write(
        project.join("package.json"),
        "{ \"name\": \"consumer\", \"version\": \"1.0.0\", \"dependencies\": { \"greeter\": \"^1.0.0\" } }",
    )
    .expect("manifest");
    let home = scratch.dir("home");
    let registry = registry.to_string_lossy().to_string();
    let install = || {
        run_in(
            npm().to_str().expect("a utf-8 path"),
            &["install", "--registry", &registry, "--jobs", "4"],
            &home,
            &project,
        )
    };

    let first = install();
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    // The project is what has to stay the same here, not the store: the lockfile, the
    // packages and the bin shims all live under it.
    let after_first = tree(&project);
    assert!(
        project.join("node_modules/greeter/index.js").is_file(),
        "nothing was installed: {}",
        stdout(&first)
    );

    let second = install();
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(
        after_first,
        tree(&project),
        "a second `npm install` rewrote the project"
    );
}

#[test]
fn a_second_install_does_not_duplicate_packages() {
    // The same property from the other side: `npm install` twice must not end up with
    // two copies, which is what a "write beside the old one" bug looks like.
    let scratch = Scratch::new("idempotent-duplicates");
    let registry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npm/registry");
    let project = scratch.dir("project");
    std::fs::write(
        project.join("package.json"),
        "{ \"name\": \"consumer\", \"version\": \"1.0.0\", \"dependencies\": { \"greeter\": \"^1.0.0\" } }",
    )
    .expect("manifest");
    let home = scratch.dir("home");
    let registry = registry.to_string_lossy().to_string();

    for _ in 0..2 {
        let output = run_in(
            npm().to_str().expect("a utf-8 path"),
            &["install", "--registry", &registry],
            &home,
            &project,
        );
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    }

    let modules = std::fs::read_dir(project.join("node_modules")).expect("node_modules");
    let mut names: Vec<String> = modules
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec![".bin", "greeter", "shout"], "unexpected entries: {names:?}");
    // A summary that reports the same count both times is the other half of "no
    // duplicates".
    let summary = run_in(
        npm().to_str().expect("a utf-8 path"),
        &["install", "--registry", &registry],
        &home,
        &project,
    );
    assert!(stdout(&summary).contains("added 2 packages"), "{}", stdout(&summary));
}
