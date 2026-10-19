//! Tests that reach the real GitHub release.
//!
//! **Not run by `cargo test`.** Like `tests/live_registry.rs`, they carry `#[ignore]`
//! *and* check an environment variable, so that `cargo test -- --ignored` does not
//! silently start using the network either. CI never runs them.
//!
//! The moment to run them is right after publishing, while `Cargo.toml` still names
//! the version that was released. On a tree that has moved past the release, set
//! `CODETS_LIVE_RELEASE_VERSION` to the published version.
//!
//! `scripts/verify-release.{sh,ps1}` are the same idea driven further: they download
//! every platform's asset and install through the one-line installers. These tests
//! are the part that belongs to `cargo test`'s world — same doubles, same tooling.

mod support;

use std::process::{Command, Output};

use codets::release;

use support::Scratch;

/// Whether the live-release tests were asked for.
fn enabled() -> bool {
    if std::env::var("CODETS_LIVE_RELEASE").as_deref() == Ok("1") {
        true
    } else {
        eprintln!("skipping: set CODETS_LIVE_RELEASE=1 to run this against the real release");
        false
    }
}

/// The release to check: an override, or the one this tree was built as.
fn version() -> String {
    std::env::var("CODETS_LIVE_RELEASE_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

#[test]
#[ignore = "needs the real GitHub release: run with --ignored and CODETS_LIVE_RELEASE=1"]
fn the_published_checksum_file_covers_every_platform() {
    if !enabled() {
        return;
    }
    let version = version();

    // The download host, not the releases API: an anonymous caller of the API is
    // rate-limited hard, and a shared egress address can exhaust that quota on its
    // own, while the asset URL is not.
    let document = codets::net::get_text(&release::checksums_url(&version))
        .unwrap_or_else(|error| panic!("cannot fetch the checksum file for {version}: {error}"));

    for triple in release::published_triples() {
        let asset = release::asset_name(&version, triple);
        assert!(
            document.contains(&asset),
            "the checksum file does not list {asset}:\n{document}"
        );
    }
    // macOS is Apple Silicon only, so nothing else may be listed for it.
    assert!(
        !document.contains("x86_64-apple-darwin"),
        "the checksum file lists an Intel macOS asset:\n{document}"
    );
}

#[test]
#[ignore = "needs the real GitHub release: run with --ignored and CODETS_LIVE_RELEASE=1"]
fn upgrade_checks_the_published_archive_for_this_platform() {
    if !enabled() {
        return;
    }
    let scratch = Scratch::new("live-release-upgrade");
    let home = scratch.dir("home");
    let version = version();

    // `--to` is pinned for the same reason the scripts pin it: the API is the part
    // that rate-limits, the download is not.
    let output = Command::new(env!("CARGO_BIN_EXE_codets"))
        .args(["upgrade", "--to", &version])
        .env("CODETS_HOME", &home)
        .env_remove("CODETS_NODE_VERSION")
        .output()
        .expect("running codets");
    assert_eq!(
        output.status.code(),
        Some(0),
        "upgrading to the published {version} failed: {}",
        stderr(&output)
    );

    let reported = stdout(&output);
    if version == env!("CARGO_PKG_VERSION") {
        // The archive was downloaded and its checksum verified, and then the running
        // version turned out to be the one asked for.
        assert!(
            reported.contains(&format!("already running codets {version}")),
            "expected the already-running answer, got: {reported}"
        );
        assert!(
            !home.join("versions").exists(),
            "an upgrade that had nothing to do wrote to the store anyway"
        );
    } else {
        assert!(
            reported.contains(&format!("downloaded codets {version}")),
            "expected the download to be reported, got: {reported}"
        );
        assert!(
            home.join("versions").is_dir(),
            "nothing was installed: {reported}"
        );
    }
}
