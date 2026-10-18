//! Command-line compatibility tests: the three binaries, Node's flags, the exit
//! codes, and the version manager.
//!
//! These run the real executables, because the point is that `node` and `npm` on
//! `PATH` are drop-in replacements — a unit test of the argument parser would not
//! prove that.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// A scratch directory that lives as long as the test.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("codets-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch directory");
    path
}

fn run(program: PathBuf, args: &[&str], home: Option<&PathBuf>) -> Output {
    run_in(program, args, home, &sandbox())
}

/// A clean working directory for every command.
///
/// The version manager walks up from the working directory looking for a
/// `.codets-version` pin, so running the binaries from a controlled directory is
/// what keeps these tests independent of whatever is around the repository.
fn sandbox() -> PathBuf {
    let path = std::env::temp_dir().join(format!("codets-cli-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&path).expect("sandbox directory");
    path
}

/// The version store for tests that need one but do not care which.
///
/// The path carries the process id, so a previous run's state can never be read
/// back and a test never depends on its neighbours or on the order they ran in.
fn default_home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("codets-cli-nohome-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&path);
    path
}

fn run_in(program: PathBuf, args: &[&str], home: Option<&PathBuf>, cwd: &Path) -> Output {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    match home {
        Some(home) => command.env("CODETS_HOME", home),
        None => command.env("CODETS_HOME", default_home()),
    };
    command.env_remove("CODETS_NODE_VERSION");
    command.output().expect("running the toolchain")
}

/// The `codets` toolchain against a specific version store.
fn codets_with_home(args: &[&str], home: &Path) -> Output {
    run_in(bin("codets"), args, Some(&home.to_path_buf()), &sandbox())
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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
    let _ = std::fs::remove_dir_all(&dir);
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
    let _ = std::fs::remove_dir_all(&dir);
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

    // Shims are written for all three command names.
    for name in ["codets", "node", "npm"] {
        let shim = if cfg!(windows) {
            home.join("bin").join(format!("{name}.cmd"))
        } else {
            home.join("bin").join(name)
        };
        assert!(shim.is_file(), "missing shim {}", shim.display());
    }

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

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_project_pin_beats_the_active_version() {
    let home = scratch("pinned");
    let project = home.join("project");
    std::fs::create_dir_all(&project).expect("project");

    run_in(bin("codets"), &["use", "24"], Some(&home), &project);
    // `--local` writes the pin into the working directory, which is why the test
    // pins a scratch directory rather than wherever the test binary runs.
    let pinned = run_in(bin("codets"), &["use", "22", "--local"], Some(&home), &project);
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
        &project.join("..").join("project"),
    );
    assert_eq!(stdout(&output), "v22.12.0");

    let _ = std::fs::remove_dir_all(&home);
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
