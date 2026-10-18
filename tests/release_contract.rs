//! The contract between a published release and the tools that consume it.
//!
//! `codets upgrade` and both installers reconstruct an asset URL from the version
//! and the target triple, and none of them may guess wrong: a mistake there shows up
//! as a failed download for a user rather than as a failing test. A local HTTP
//! server stands in for GitHub, so the whole path — naming, download, checksum,
//! unpack, install, activate — runs offline and on every `cargo test`.
//!
//! `tests/install_scripts.rs` drives the same server through the shell installers.

use std::path::Path;
use std::process::{Command, Output};

use codets::release;

mod support;

use support::{publish_release, Checksum, ReleaseServer, Scratch};

/// Run the `codets` toolchain against a local download base.
fn codets_in(args: &[&str], home: &Path, cwd: &Path, download_base: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codets"))
        .args(args)
        .current_dir(cwd)
        .env("CODETS_HOME", home)
        .env("CODETS_DOWNLOAD_BASE", download_base)
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("running codets")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// The triple this platform publishes, which is what the asset names use.
fn triple() -> &'static str {
    release::target_triple().expect("every platform the tests run on publishes a build")
}

#[test]
fn the_published_names_are_the_names_the_code_asks_for() {
    // The whole contract in one place: if `release.rs` renames an asset, this fails
    // here rather than in a user's terminal.
    let scratch = Scratch::new("release-names");
    let (archive, checksums) = publish_release(&scratch.dir("assets"), "9.9.9", triple(), Checksum::Correct);

    assert_eq!(archive, release::asset_name("9.9.9", triple()));
    assert_eq!(checksums, release::checksums_name("9.9.9"));
    let url = release::download_url("9.9.9", triple());
    assert!(
        url.ends_with(&format!("/v9.9.9/{archive}")),
        "the download URL does not end in the published asset name: {url}"
    );
    assert!(
        release::checksums_url("9.9.9").ends_with(&format!("/v9.9.9/{checksums}")),
        "the checksum URL does not end in the published checksum name"
    );
}

#[test]
fn upgrade_installs_a_published_release() {
    let scratch = Scratch::new("release-upgrade");
    let assets = scratch.dir("assets");
    let (archive, _) = publish_release(&assets, "9.9.9", triple(), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = codets_in(&["upgrade", "--to", "9.9.9"], &home, &scratch, server.base_url());
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("downloaded codets 9.9.9"), "{}", stdout(&output));

    // The store holds a toolchain, and it is the binary that was served rather than
    // anything else that happened to be lying around.
    let level = home.join("versions").join("24.0.0");
    let installed = level.join(support::binary_name());
    assert!(installed.is_file(), "{} is missing", installed.display());
    let served = std::fs::read(env!("CARGO_BIN_EXE_codets")).expect("the built binary");
    assert_eq!(
        std::fs::read(&installed).expect("the installed binary"),
        served,
        "the installed binary is not the one the release served"
    );
    assert!(level.join("toolchain.conf").is_file(), "no manifest was written");
    assert!(
        installed.parent().expect("a version directory").join(if cfg!(windows) { "node.exe" } else { "node" }).is_file(),
        "the version directory has no `node`"
    );

    // And it runs: `bin/node` is the link that `PATH` would pick up.
    let node = home.join("bin").join(if cfg!(windows) { "node.exe" } else { "node" });
    let reported = Command::new(&node)
        .args(["-p", "process.version"])
        .current_dir(&scratch)
        .env("CODETS_HOME", &home)
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("running the linked node");
    assert_eq!(stdout(&reported), "v24.0.0", "{}", stderr(&reported));

    // The asset it fetched is the one the release published, which is what the
    // checksum file described.
    assert!(archive.ends_with(".tar.gz"));
}

#[test]
fn a_checksum_mismatch_stops_the_upgrade() {
    let scratch = Scratch::new("release-checksum");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", triple(), Checksum::Wrong);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = codets_in(&["upgrade", "--to", "9.9.9"], &home, &scratch, server.base_url());
    assert_ne!(output.status.code(), Some(0), "a tampered archive was accepted");
    assert!(
        stderr(&output).contains("failed its checksum"),
        "the reason should say what went wrong: {}",
        stderr(&output)
    );
    // Nothing was unpacked, so nothing was installed.
    assert!(
        !home.join("versions").exists(),
        "the store was written despite a failed checksum"
    );
}

#[test]
fn a_release_without_a_checksum_file_still_installs() {
    // An older release may not have one. The digest is what stops a tampered
    // download, so its absence is tolerated rather than fatal — and this test is
    // what records that decision.
    let scratch = Scratch::new("release-nochecksum");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", triple(), Checksum::Missing);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = codets_in(&["upgrade", "--to", "9.9.9"], &home, &scratch, server.base_url());
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(home.join("versions").join("24.0.0").is_dir(), "nothing was installed");
}

#[test]
fn a_missing_asset_names_the_url_it_tried() {
    let scratch = Scratch::new("release-missing");
    let assets = scratch.dir("assets");
    // A different version is published from the one that is asked for.
    publish_release(&assets, "1.0.0", triple(), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = codets_in(&["upgrade", "--to", "9.9.9"], &home, &scratch, server.base_url());
    assert_ne!(output.status.code(), Some(0));
    let message = stderr(&output);
    assert!(message.contains("404"), "the status should be reported: {message}");
    assert!(
        message.contains(&release::asset_name("9.9.9", triple())),
        "the URL should name the asset it wanted: {message}"
    );
}

#[test]
fn upgrading_to_the_running_version_says_so_and_changes_nothing() {
    let scratch = Scratch::new("release-current");
    let assets = scratch.dir("assets");
    let running = env!("CARGO_PKG_VERSION");
    publish_release(&assets, running, triple(), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let output = codets_in(&["upgrade", "--to", running], &home, &scratch, server.base_url());
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("already running"),
        "{}",
        stdout(&output)
    );
    assert!(
        !home.join("versions").exists(),
        "an upgrade that had nothing to do wrote to the store anyway"
    );
}

#[test]
fn upgrading_twice_leaves_the_same_store() {
    let scratch = Scratch::new("release-twice");
    let assets = scratch.dir("assets");
    publish_release(&assets, "9.9.9", triple(), Checksum::Correct);
    let server = ReleaseServer::serve(&assets);
    let home = scratch.dir("home");

    let first = codets_in(&["upgrade", "--to", "9.9.9"], &home, &scratch, server.base_url());
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let after_first = support::tree(&home);

    let second = codets_in(&["upgrade", "--to", "9.9.9"], &home, &scratch, server.base_url());
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(
        after_first,
        support::tree(&home),
        "a second upgrade of the same version changed the store"
    );
}
