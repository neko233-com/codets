//! Command-line compatibility tests: the three binaries, Node's flags, the exit
//! codes, and the version manager.
//!
//! These run the real executables, because the point is that `node` and `npm` on
//! `PATH` are drop-in replacements — a unit test of the argument parser would not
//! prove that.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;

use support::Scratch;

/// The `node` personality, run through the `node` binary.
fn node(args: &[&str]) -> Output {
    run(bin("node"), args, None)
}

/// The `codets` toolchain.
fn codets(args: &[&str]) -> Output {
    run(bin("codets"), args, None)
}

/// The `npm` personality.
fn npm(args: &[&str]) -> Output {
    run(bin("npm"), args, None)
}

fn bin(name: &str) -> PathBuf {
    PathBuf::from(match name {
        "node" => env!("CARGO_BIN_EXE_node"),
        "npm" => env!("CARGO_BIN_EXE_npm"),
        _ => env!("CARGO_BIN_EXE_codets"),
    })
}

/// A scratch directory for one test, removed when the test ends — including when it
/// panics, which the trailing `remove_dir_all` this replaces did not manage.
fn scratch(name: &str) -> Scratch {
    Scratch::new(name)
}

fn run(program: PathBuf, args: &[&str], home: Option<&Path>) -> Output {
    run_in(program, args, home, None)
}

/// Run a binary with a chosen version store and working directory.
///
/// Both are created when the caller does not name one, and removed when the call
/// returns. A clean working directory matters on its own: the version manager walks
/// up from it looking for a `.codets-version` pin, so a controlled directory is what
/// keeps these tests independent of whatever is around the repository.
fn run_in(program: PathBuf, args: &[&str], home: Option<&Path>, cwd: Option<&Path>) -> Output {
    let own_home = Scratch::new("home");
    let own_cwd = Scratch::new("cwd");
    let home = home.unwrap_or_else(|| own_home.path());
    let cwd = cwd.unwrap_or_else(|| own_cwd.path());
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd).env("CODETS_HOME", home);
    command.env_remove("CODETS_NODE_VERSION");
    command.output().expect("running the toolchain")
}

/// The `codets` toolchain against a specific version store.
fn codets_with_home(args: &[&str], home: &Path) -> Output {
    run_in(bin("codets"), args, Some(home), None)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The file name a command has in a directory on this platform.
fn command_file(directory: &Path, command: &str) -> PathBuf {
    if cfg!(windows) {
        directory.join(format!("{command}.exe"))
    } else {
        directory.join(command)
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

#[test]
fn node_version_is_what_scripts_parse() {
    let output = node(&["--version"]);
    assert_eq!(stdout(&output), format!("v{}", "24.0.0"));
    assert_eq!(output.status.code(), Some(0));

    let short = node(&["-v"]);
    assert_eq!(stdout(&short), "v24.0.0");
}

#[test]
fn node_claims_the_requested_release() {
    let old = node(&["--node-version=18", "-p", "process.version"]);
    assert_eq!(stdout(&old), "v18.20.4");

    let new = node(&["--node-version=22", "-p", "process.versions.node"]);
    assert_eq!(stdout(&new), "22.12.0");
}

#[test]
fn feature_gates_follow_the_claimed_release() {
    let node18 = node(&["--node-version=18", "-p", "typeof Object.groupBy"]);
    assert_eq!(stdout(&node18), "undefined");

    let node22 = node(&["--node-version=22", "-p", "typeof Object.groupBy"]);
    assert_eq!(stdout(&node22), "function");

    let node24 = node(&["--node-version=24", "-p", "typeof Error.isError"]);
    assert_eq!(stdout(&node24), "function");
}

#[test]
fn eval_and_print_report_the_result() {
    let evaluated = node(&["-e", "console.log(1 + 1)"]);
    assert_eq!(stdout(&evaluated), "2");

    // `-p` prints the completion value; `-e` does not.
    let printed = node(&["-p", "[1, 2, 3].map(n => n * 2).join('-')"]);
    assert_eq!(stdout(&printed), "2-4-6");

    let not_printed = node(&["-e", "1 + 1"]);
    assert_eq!(stdout(&not_printed), "");
}

#[test]
fn typescript_runs_without_a_build_step() {
    let output = node(&[
        "-e",
        "interface P { name: string }\n\
         const greet = (p: P): string => `hi ${p.name}`;\n\
         enum Level { Low = 1, High }\n\
         console.log(greet({ name: 'ts' as string }), Level.High);",
    ]);
    assert_eq!(stdout(&output), "hi ts 2");
}

#[test]
fn position_arguments_reach_the_script() {
    let dir = scratch("argv");
    let script = dir.join("args.ts");
    std::fs::write(
        &script,
        "console.log(process.argv.slice(2).join(','), process.argv0);",
    )
    .expect("write script");
    let output = node(&[script.to_string_lossy().as_ref(), "one", "two"]);
    // `console.log` separates its arguments with a space.
    assert_eq!(stdout(&output), "one,two args.ts");
}

#[test]
fn exit_codes_match_node() {
    // A successful run.
    assert_eq!(node(&["-e", "1"]).status.code(), Some(0));
    // `process.exit(n)` is honoured and is not catchable.
    assert_eq!(node(&["-e", "process.exit(3)"]).status.code(), Some(3));
    assert_eq!(
        node(&["-e", "try { process.exit(7) } catch (e) {}"]).status.code(),
        Some(7)
    );
    // `process.exitCode` is honoured too.
    assert_eq!(node(&["-e", "process.exitCode = 5"]).status.code(), Some(5));
    // An uncaught throw exits 1 with the error on stderr.
    let thrown = node(&["-e", "throw new Error('boom')"]);
    assert_eq!(thrown.status.code(), Some(1));
    assert!(stderr(&thrown).contains("boom"), "{}", stderr(&thrown));
    // An unknown option is a usage error, which Node reports as 9.
    assert_eq!(node(&["-Q"]).status.code(), Some(9));
}

#[test]
fn a_missing_script_is_an_error() {
    let output = node(&["definitely-not-here.ts"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("cannot find module"), "{}", stderr(&output));
}

#[test]
fn check_reports_syntax_errors_without_running() {
    let dir = scratch("check");
    let good = dir.join("good.ts");
    std::fs::write(&good, "const x: number = 1;\nconsole.log('ran');\n").expect("write");
    let bad = dir.join("bad.ts");
    std::fs::write(&bad, "const = ;\n").expect("write");

    let ok = node(&["--check", good.to_string_lossy().as_ref()]);
    assert_eq!(ok.status.code(), Some(0));
    // `--check` must not execute the file.
    assert_eq!(stdout(&ok), "");

    let broken = node(&["--check", bad.to_string_lossy().as_ref()]);
    assert_eq!(broken.status.code(), Some(1));
}

#[test]
fn codets_version_describes_the_toolchain() {
    let output = codets(&["--version"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(text.contains("codets"), "{text}");
    assert!(text.contains("Krypton"), "{text}");
}

#[test]
fn the_version_manager_installs_and_switches() {
    let home = scratch("versions");

    let installed = codets_with_home(&["install", "22"], &home);
    assert_eq!(installed.status.code(), Some(0), "{}", stderr(&installed));
    assert!(stdout(&installed).contains("22.12.0"), "{}", stdout(&installed));

    let listed = codets_with_home(&["versions"], &home);
    assert!(stdout(&listed).contains("22.12.0"));
    assert!(stdout(&listed).contains("Krypton"), "{}", stdout(&listed));

    // Nothing is active until `use` runs.
    let before = codets_with_home(&["current"], &home);
    assert!(stdout(&before).contains("built-in default"), "{}", stdout(&before));

    let used = codets_with_home(&["use", "22"], &home);
    assert_eq!(used.status.code(), Some(0), "{}", stderr(&used));
    let after = codets_with_home(&["current"], &home);
    assert!(stdout(&after).contains("22.12.0"), "{}", stdout(&after));
    assert!(stdout(&after).contains("codets current"), "{}", stdout(&after));

    // The switch changes what `node` reports, because `node` asks the manager.
    let reported = run(bin("node"), &["-p", "process.version"], Some(&home));
    assert_eq!(stdout(&reported), "v22.12.0");

    // `bin/` holds the three commands, linked to the active toolchain.
    for name in ["codets", "node", "npm"] {
        let linked = command_file(&home.join("bin"), name);
        assert!(linked.is_file(), "missing {}", linked.display());
    }
    // `versions/<v>/node` pins that level, so those commands exist as well.
    for name in ["codets", "node", "npm"] {
        let linked = command_file(&home.join("versions").join("22.12.0"), name);
        assert!(linked.is_file(), "missing {}", linked.display());
    }
    // Running the linked `node` directly reports the active level.
    let linked_node = command_file(&home.join("bin"), "node");
    let reported_by_link = Command::new(&linked_node)
        .args(["-p", "process.version"])
        .env("CODETS_HOME", home.path())
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("run the linked node");
    assert_eq!(
        String::from_utf8_lossy(&reported_by_link.stdout).trim(),
        "v22.12.0"
    );
    // Installing another level must not move `node` off the active one.
    let other = codets_with_home(&["install", "18"], &home);
    assert_eq!(other.status.code(), Some(0), "{}", stderr(&other));
    let still_active = codets_with_home(&["current"], &home);
    assert!(
        stdout(&still_active).contains("22.12.0"),
        "installing a level changed the active version: {}",
        stdout(&still_active)
    );
    // ...and a binary inside that level's directory pins it, whatever `current`
    // says, which is what `versions/<v>/node` is for.
    let pinned = command_file(&home.join("versions").join("18.20.4"), "node");
    let reported_by_pinned = Command::new(&pinned)
        .args(["-p", "process.version"])
        .env("CODETS_HOME", home.path())
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("run the pinned node");
    assert_eq!(
        String::from_utf8_lossy(&reported_by_pinned.stdout).trim(),
        "v18.20.4"
    );

    // An explicit `--node-version` still overrides the manager.
    let overridden = run(bin("node"), &["--node-version=24", "-p", "process.version"], Some(&home));
    assert_eq!(stdout(&overridden), "v24.0.0");

    // `codets features` reports the active release's standard library.
    let features = codets_with_home(&["features"], &home);
    assert!(stdout(&features).contains("Promise.withResolvers"), "{}", stdout(&features));
    let old = codets_with_home(&["--node-version=18", "features"], &home);
    assert!(
        stdout(&old).contains("needs Node >="),
        "a Node 18 feature list should say what it lacks: {}",
        stdout(&old)
    );

}

#[test]
fn a_project_pin_beats_the_active_version() {
    let home = scratch("pinned");
    let project = home.join("project");
    std::fs::create_dir_all(&project).expect("project");

    run_in(bin("codets"), &["use", "24"], Some(&home), Some(&project));
    // `--local` writes the pin into the working directory, which is why the test
    // pins a scratch directory rather than wherever the test binary runs.
    let pinned = run_in(bin("codets"), &["use", "22", "--local"], Some(&home), Some(&project));
    assert_eq!(pinned.status.code(), Some(0));
    assert!(
        project.join(".codets-version").is_file(),
        "the pin was not written where it was asked for"
    );

    // `node` resolves the pin by walking up from the working directory, and a pin
    // beats the active version.
    let output = run_in(
        bin("node"),
        &["-p", "process.version"],
        Some(&home),
        Some(&project),
    );
    assert_eq!(stdout(&output), "v22.12.0");
}

#[test]
fn process_events_run_at_exit() {
    let output = node(&[
        "-e",
        "process.on('exit', (code) => console.log('exiting', code)); console.log('body');",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "body\nexiting 0");
}

#[test]
fn an_uncaught_exception_listener_is_consulted_first() {
    let output = node(&[
        "-e",
        "process.on('uncaughtException', (error) => console.log('caught', error.message));\n\
         setTimeout(() => { throw new Error('later'); }, 0);",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("caught later"), "{}", stdout(&output));
}

#[test]
fn npm_reports_a_version_and_help() {
    let version = npm(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(stdout(&version).contains('.'), "{}", stdout(&version));

    let help = npm(&["--help"]);
    assert!(stdout(&help).contains("install"));
    assert!(stdout(&help).contains("run"));
}

#[test]
fn the_node_binary_can_also_be_reached_through_codets() {
    let direct = node(&["-p", "40 + 2"]);
    let delegated = codets(&["node", "-p", "40 + 2"]);
    assert_eq!(stdout(&direct), "42");
    assert_eq!(stdout(&delegated), "42");
}

/// Run `npm` in `dir` with a controlled user config.
///
/// The machine running the tests may have its own `~/.npmrc`, so every registry
/// test points the user config at a file inside the test's directory: a missing
/// file is a valid answer, and the assertion then depends only on what the test
/// set up.
fn npm_here(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let home = Scratch::new("npm-home");
    let mut command = Command::new(bin("npm"));
    command.args(args).current_dir(dir);
    command.env("CODETS_HOME", home.path());
    command.env_remove("CODETS_NODE_VERSION");
    command.env_remove("NPM_CONFIG_REGISTRY");
    command.env_remove("npm_config_registry");
    command.env("NPM_CONFIG_USERCONFIG", dir.join("no-such-user-npmrc"));
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("running npm")
}

fn write_npmrc(dir: &Path, registry: &str) {
    std::fs::write(dir.join(".npmrc"), format!("registry={registry}\n")).expect("npmrc");
}

#[test]
fn npm_config_get_registry_follows_the_chain() {
    let dir = scratch("npm-config");

    // Nothing configured anywhere reaches npm's own registry.
    let plain = npm_here(&dir, &["config", "get", "registry"], &[]);
    assert_eq!(plain.status.code(), Some(0), "{}", stderr(&plain));
    assert_eq!(stdout(&plain), "https://registry.npmjs.org");

    // The environment beats the files.
    let from_env = npm_here(
        &dir,
        &["config", "get", "registry"],
        &[("NPM_CONFIG_REGISTRY", "https://env.example.com/")],
    );
    assert_eq!(stdout(&from_env), "https://env.example.com/");

    // A project `.npmrc` is next, and npm's lowercase spelling works too.
    write_npmrc(&dir, "https://project.example.com/");
    let from_file = npm_here(&dir, &["config", "get", "registry"], &[]);
    assert_eq!(stdout(&from_file), "https://project.example.com/");

    let lowercase = npm_here(
        &dir,
        &["config", "get", "registry"],
        &[("npm_config_registry", "https://lower.example.com/")],
    );
    assert_eq!(stdout(&lowercase), "https://lower.example.com/");

    // A flag beats everything.
    let from_flag = npm_here(
        &dir,
        &["config", "get", "registry", "--registry", "https://flag.example.com/"],
        &[("NPM_CONFIG_REGISTRY", "https://env.example.com/")],
    );
    assert_eq!(stdout(&from_flag), "https://flag.example.com/");

}

#[test]
fn npm_config_get_registry_keeps_stdout_parseable() {
    let dir = scratch("npm-config-origin");
    write_npmrc(&dir, "https://project.example.com/");

    let quiet = npm_here(&dir, &["config", "get", "registry"], &[]);
    assert_eq!(stdout(&quiet), "https://project.example.com/");

    // Where the setting came from is opt-in and goes to stderr, so a script that
    // pipes stdout still sees a bare value.
    let loud = npm_here(&dir, &["config", "get", "registry", "--origin"], &[]);
    assert_eq!(stdout(&loud), "https://project.example.com/");
    assert!(stderr(&loud).contains(".npmrc"), "{}", stderr(&loud));
    assert!(stderr(&loud).contains("https://project.example.com/"), "{}", stderr(&loud));

}

#[test]
fn npm_config_get_refuses_a_key_it_does_not_implement() {
    // Saying so is better than printing nothing and letting a script believe it.
    let output = npm(&["config", "get", "strict-ssl"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("knows only `registry`"), "{}", stderr(&output));
}

#[test]
fn npm_install_uses_the_registry_a_project_configures() {
    // The whole point of the chain: a project pointing at a local registry installs
    // from it, with no flag on the command line. The registry is a directory, which
    // is the offline backend the installer tests use.
    let dir = scratch("npm-config-install");
    let registry = dir.join("registry");
    // The directory backend keeps each version in its own directory, which is what
    // makes it readable in a fixture.
    let package = registry.join("local-only").join("1.0.0");
    std::fs::create_dir_all(&package).expect("registry");
    std::fs::write(
        package.join("package.json"),
        "{ \"name\": \"local-only\", \"version\": \"1.0.0\", \"main\": \"index.js\" }",
    )
    .expect("manifest");
    std::fs::write(package.join("index.js"), "module.exports = 'from the local registry';")
        .expect("module");
    std::fs::write(
        registry.join("local-only.json"),
        "{ \"name\": \"local-only\", \"versions\": { \"1.0.0\": { \"dist\": { \"tarball\": \"local-only/1.0.0.tgz\" } } } }",
    )
    .expect("packument");

    let project = dir.join("project");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join("package.json"),
        "{ \"name\": \"consumer\", \"version\": \"1.0.0\", \"dependencies\": { \"local-only\": \"^1.0.0\" } }",
    )
    .expect("manifest");
    write_npmrc(&project, &registry.to_string_lossy());

    let output = npm_here(&project, &["install"], &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        project.join("node_modules").join("local-only").join("index.js").is_file(),
        "the project's own registry was not used: {}",
        stdout(&output)
    );

}

#[test]
fn env_and_versions_describe_the_store_accurately() {
    let home = scratch("store-output");
    let installed = codets_with_home(&["install", "22"], &home);
    assert_eq!(installed.status.code(), Some(0), "{}", stderr(&installed));

    // `codets env` names the directory that belongs on PATH, and says what happens to
    // it. It holds links now rather than the shim scripts this used to write, and the
    // output is pasted into shell profiles, so the wording is worth a test.
    let env = codets_with_home(&["env"], &home);
    let text = stdout(&env);
    assert!(text.contains("bin directory"), "{text}");
    assert!(!text.contains("shim"), "stale wording in `codets env`: {text}");

    // A level that is installed is marked as such, and the marker cannot run into the
    // description: `... (Krypton)installed` is what a missing separator looks like.
    let versions = codets_with_home(&["versions"], &home);
    let text = stdout(&versions);
    assert!(text.contains("[installed]"), "{text}");
    assert!(!text.contains(")installed"), "a marker ran into the description: {text}");
}

#[test]
fn bootstrap_does_not_record_a_path_that_will_be_gone() {
    let home = scratch("bootstrap-source");
    let bootstrapped = codets_with_home(&["bootstrap", "--level", "22"], &home);
    assert_eq!(bootstrapped.status.code(), Some(0), "{}", stderr(&bootstrapped));

    let manifest = std::fs::read_to_string(home.join("versions").join("22.12.0").join("toolchain.conf"))
        .expect("the manifest");
    assert!(manifest.contains("source=bootstrap"), "{manifest}");
    // The binary that ran is the test build. Recording that path would put a path in
    // the manifest that an installed toolchain never had, and would make two identical
    // installs differ — which is what an installer would produce, every time.
    assert!(
        !manifest.contains("target"),
        "the manifest records where the test binary happened to live: {manifest}"
    );
}
