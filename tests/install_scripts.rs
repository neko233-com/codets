//! The one-line installers, run for real.
//!
//! `install.sh` and `install.ps1` are the first thing a user touches, and they are
//! shell scripts, so nothing in the Rust suite would otherwise notice a typo in them.
//! These tests download from a local HTTP server, install the binary they find, and
//! then run it — the sequence `scripts/verify-demo.*` performs against the real
//! thing, minus the network.
//!
//! A machine without a shell (or without PowerShell) skips the affected cases and
//! says so, rather than passing quietly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use codets::release;

mod support;

use support::{publish_release, tree, Checksum, ReleaseServer, Scratch};

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// The file name a command has in a directory on this platform.
fn command_file(directory: &Path, command: &str) -> PathBuf {
    if cfg!(windows) {
        directory.join(format!("{command}.exe"))
    } else {
        directory.join(command)
    }
}

/// Every program with one of these names that `PATH` offers, in order.
fn on_path(names: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Some(path) = std::env::var_os("PATH") else {
        return found;
    };
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_string())
            .split(';')
            .map(|extension| extension.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for directory in std::env::split_paths(&path) {
        for name in names {
            for extension in &extensions {
                let candidate = directory.join(format!("{name}{extension}"));
                if candidate.is_file() {
                    found.push(candidate);
                }
            }
        }
    }
    found
}

/// `sh` or `bash`, if one of them can actually run this repository's scripts.
///
/// Candidates are probed rather than trusted, because a program called `bash` is not
/// necessarily a shell that can see this working tree: on Windows the `bash.exe` in
/// `System32` is the WSL launcher, which fails outright without a distribution and
/// would look at Linux paths with one.
fn find_shell() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(windows) {
        // Git for Windows, which is what this repository's scripts are developed
        // against on Windows.
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\sh.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            candidates.push(PathBuf::from(candidate));
        }
    }
    candidates.extend(on_path(&["sh", "bash"]));
    candidates.retain(|candidate| candidate.is_file());
    candidates.into_iter().find(|candidate| shell_can_run_the_scripts(candidate))
}

/// True when `candidate` is a shell that can see `install.sh` in this repository.
fn shell_can_run_the_scripts(candidate: &Path) -> bool {
    match Command::new(candidate)
        .args(["-c", "test -f install.sh && printf codets-shell-ok"])
        .current_dir(repository())
        .output()
    {
        Ok(output) => {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("codets-shell-ok")
        }
        Err(_) => false,
    }
}

/// PowerShell, from `PATH` or from where Windows keeps it.
fn find_powershell() -> Option<PathBuf> {
    if let Some(shell) = on_path(&["pwsh", "powershell"]).into_iter().next() {
        return Some(shell);
    }
    if cfg!(windows) {
        let path = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Run a script from the repository root with a controlled environment.
fn run_script(program: &Path, args: &[&str], repository: &Path, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(program);
    command.args(args).current_dir(repository).env_remove("CODETS_NODE_VERSION");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("running the installer")
}

/// The repository root, which is where the scripts are run from.
fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// What every install has to produce: three commands and a manifest, linked together.
fn assert_installed(home: &Path, level: &str) {
    for command in ["codets", "node", "npm"] {
        let path = command_file(&home.join("bin"), command);
        assert!(path.is_file(), "missing {}", path.display());
        let in_level = command_file(&home.join("versions").join(level), command);
        assert!(in_level.is_file(), "missing {}", in_level.display());
    }
    assert!(
        home.join("versions").join(level).join("toolchain.conf").is_file(),
        "no manifest was written for {level}"
    );

    // And it runs, reporting the level it was installed as.
    let node = command_file(&home.join("bin"), "node");
    let reported = Command::new(&node)
        .args(["-p", "process.version"])
        .env("CODETS_HOME", home)
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("running the installed node");
    assert_eq!(
        String::from_utf8_lossy(&reported.stdout).trim(),
        format!("v{level}"),
        "{}",
        String::from_utf8_lossy(&reported.stderr)
    );
}

#[test]
fn install_sh_installs_a_working_toolchain_and_repeats_itself() {
    let Some(shell) = find_shell() else {
        eprintln!("skipping install.sh: no sh or bash on this machine");
        return;
    };
    let scratch = Scratch::new("install-sh");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", release::target_triple().expect("a published platform"), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");
    let env = [
        ("CODETS_DOWNLOAD_BASE", server.base_url()),
        ("CODETS_VERSION", "9.9.9"),
        ("CODETS_HOME", home.to_str().expect("a utf-8 path")),
    ];

    let first = run_script(&shell, &["install.sh", "--level", "22"], &repository(), &env);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(stdout(&first).contains("checksum matches"), "{}", stdout(&first));
    assert_installed(&home, "22.12.0");
    let after_first = tree(&home);

    // Running it again must land on the same store, byte for byte: an installer that
    // is not idempotent turns every reinstall into a diff.
    let second = run_script(&shell, &["install.sh", "--level", "22"], &repository(), &env);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(
        after_first,
        tree(&home),
        "a second run of install.sh changed the store"
    );
}

#[test]
fn install_ps1_installs_a_working_toolchain_and_repeats_itself() {
    let Some(shell) = find_powershell() else {
        eprintln!("skipping install.ps1: no pwsh or powershell on this machine");
        return;
    };
    let scratch = Scratch::new("install-ps1");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", release::target_triple().expect("a published platform"), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");
    let env = [
        ("CODETS_DOWNLOAD_BASE", server.base_url()),
        ("CODETS_VERSION", "9.9.9"),
        ("CODETS_HOME", home.to_str().expect("a utf-8 path")),
    ];

    let first = run_script(
        &shell,
        &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "install.ps1", "--level", "24"],
        &repository(),
        &env,
    );
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(stdout(&first).contains("checksum matches"), "{}", stdout(&first));
    assert_installed(&home, "24.0.0");
    let after_first = tree(&home);

    let second = run_script(
        &shell,
        &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "install.ps1", "--level", "24"],
        &repository(),
        &env,
    );
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(
        after_first,
        tree(&home),
        "a second run of install.ps1 changed the store"
    );
}

#[test]
fn install_sh_refuses_a_tampered_archive() {
    let Some(shell) = find_shell() else {
        eprintln!("skipping install.sh: no sh or bash on this machine");
        return;
    };
    let scratch = Scratch::new("install-sh-tampered");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", release::target_triple().expect("a published platform"), Checksum::Wrong);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = run_script(
        &shell,
        &["install.sh"],
        &repository(),
        &[
            ("CODETS_DOWNLOAD_BASE", server.base_url()),
            ("CODETS_VERSION", "9.9.9"),
            ("CODETS_HOME", home.to_str().expect("a utf-8 path")),
        ],
    );
    assert_ne!(output.status.code(), Some(0), "a tampered archive was installed");
    assert!(
        stderr(&output).contains("failed its checksum"),
        "the reason should say what went wrong: {}",
        stderr(&output)
    );
    assert!(
        !home.join("versions").exists(),
        "the store was written despite a failed checksum"
    );
}

#[test]
fn install_ps1_refuses_a_tampered_archive() {
    let Some(shell) = find_powershell() else {
        eprintln!("skipping install.ps1: no pwsh or powershell on this machine");
        return;
    };
    let scratch = Scratch::new("install-ps1-tampered");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", release::target_triple().expect("a published platform"), Checksum::Wrong);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = run_script(
        &shell,
        &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "install.ps1"],
        &repository(),
        &[
            ("CODETS_DOWNLOAD_BASE", server.base_url()),
            ("CODETS_VERSION", "9.9.9"),
            ("CODETS_HOME", home.to_str().expect("a utf-8 path")),
        ],
    );
    assert_ne!(output.status.code(), Some(0), "a tampered archive was installed");
    assert!(
        stderr(&output).contains("failed its checksum"),
        "the reason should say what went wrong: {}",
        stderr(&output)
    );
    assert!(
        !home.join("versions").exists(),
        "the store was written despite a failed checksum"
    );
}

#[test]
fn install_sh_can_read_a_mounted_mirror() {
    // `file://` is the air-gapped case: a directory of published assets on a share.
    // The PowerShell installer cannot use it — `Invoke-WebRequest` speaks http only —
    // so this covers the shell script alone.
    let Some(shell) = find_shell() else {
        eprintln!("skipping install.sh: no sh or bash on this machine");
        return;
    };
    let scratch = Scratch::new("install-sh-file");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", release::target_triple().expect("a published platform"), Checksum::Correct);
    let home = scratch.dir("home");
    let base = format!("file:///{}", assets.to_string_lossy().replace('\\', "/"));

    let output = run_script(
        &shell,
        &["install.sh", "--level", "20"],
        &repository(),
        &[
            ("CODETS_DOWNLOAD_BASE", &base),
            ("CODETS_VERSION", "9.9.9"),
            ("CODETS_HOME", home.to_str().expect("a utf-8 path")),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_installed(&home, "20.19.0");
}

#[test]
fn install_sh_demands_a_version_when_the_base_is_a_mirror() {
    // With a mirror configured there is no releases API to ask, so the version has
    // to be given. Guessing "latest" from a mirror that may not follow npm's layout
    // would be worse than saying so.
    let Some(shell) = find_shell() else {
        eprintln!("skipping install.sh: no sh or bash on this machine");
        return;
    };
    let output = run_script(
        &shell,
        &["install.sh"],
        &repository(),
        &[("CODETS_DOWNLOAD_BASE", "https://mirror.example.invalid/npm")],
    );
    assert_ne!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains("CODETS_VERSION"),
        "the message should say what to set: {}",
        stderr(&output)
    );
}

#[test]
fn install_ps1_does_not_take_tar_from_path() {
    // Windows has a `tar.exe` of its own, and `PATH` is not a reliable way to find it:
    // Git for Windows ships one in `/usr/bin` which comes first for anyone working in a
    // Git shell, and GNU tar reads a `C:\...` argument as a remote host, failing with
    // "Cannot connect to C: resolve failed" instead of unpacking. The installer takes
    // the system one by absolute path; this puts a decoy in front to prove it.
    if !cfg!(windows) {
        eprintln!("skipping: this is about Windows' PATH layout");
        return;
    }
    let Some(shell) = find_powershell() else {
        eprintln!("skipping install.ps1: no pwsh or powershell on this machine");
        return;
    };
    let scratch = Scratch::new("install-ps1-decoy");
    let decoy = scratch.dir("decoy");
    // Any program will do: what matters is that it must never be reached.
    let decoy_tar = decoy.join("tar.exe");
    std::fs::copy(r"C:\Windows\System32\where.exe", &decoy_tar).expect("a decoy tar.exe");

    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", release::target_triple().expect("a published platform"), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");
    let with_decoy_first = format!(
        "{};{}",
        decoy.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run_script(
        &shell,
        &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "install.ps1", "--level", "22"],
        &repository(),
        &[
            ("CODETS_DOWNLOAD_BASE", server.base_url()),
            ("CODETS_VERSION", "9.9.9"),
            ("CODETS_HOME", home.to_str().expect("a utf-8 path")),
            ("PATH", &with_decoy_first),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "the installer unpacked with PATH's tar: {}",
        stderr(&output)
    );
    assert_installed(&home, "22.12.0");
}

#[test]
fn the_installers_answer_help_without_installing_anything() {
    // `install.sh --help` used to fall through to the install itself, which on a
    // repository with no releases yet ended in a 404 — while the person was asking
    // for the usage. The answer has to come from the script, with no network and no
    // store written, so the download base points at a port nothing listens on: a
    // regression fails here instead of reaching GitHub.
    let scratch = Scratch::new("install-help");
    let home = scratch.dir("home");
    let home = home.to_str().expect("a utf-8 path");
    let env = [
        ("CODETS_HOME", home),
        ("CODETS_VERSION", "9.9.9"),
        ("CODETS_DOWNLOAD_BASE", "http://127.0.0.1:9/nowhere"),
    ];

    match find_shell() {
        Some(shell) => {
            let output = run_script(&shell, &["install.sh", "--help"], &repository(), &env);
            assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
            assert!(
                stdout(&output).contains("CODETS_DOWNLOAD_BASE"),
                "the help should describe the environment: {}",
                stdout(&output)
            );
            assert!(
                !std::path::Path::new(home).join("versions").exists(),
                "asking for help installed something"
            );
        }
        None => eprintln!("skipping install.sh --help: no sh or bash on this machine"),
    }

    match find_powershell() {
        Some(shell) => {
            let output = run_script(
                &shell,
                &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "install.ps1", "--help"],
                &repository(),
                &env,
            );
            assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
            assert!(
                stdout(&output).contains("Usage: install.ps1"),
                "{}",
                stdout(&output)
            );
            assert!(
                !std::path::Path::new(home).join("versions").exists(),
                "asking for help installed something"
            );
        }
        None => eprintln!("skipping install.ps1 --help: no pwsh or powershell on this machine"),
    }
}

#[test]
fn the_install_scripts_are_lf_and_bom_free() {
    // A checked-out CRLF or a byte-order mark is enough to break `sh` on the very
    // first line, and both are easy to introduce from Windows.
    for name in ["install.sh", "scripts/pre-release.sh", "scripts/check-temp-clean.sh"] {
        let bytes = std::fs::read(repository().join(name)).expect("the script");
        assert!(
            !bytes.starts_with(&[0xef, 0xbb, 0xbf]),
            "{name} starts with a byte-order mark"
        );
        assert!(
            !bytes.contains(&b'\r'),
            "{name} contains a carriage return, so it is not LF-only"
        );
    }
}
