//! The transport layer's own rules.
//!
//! Every network call in the toolchain goes through `codets::net` — registry
//! metadata, package tarballs, release archives — so what it refuses to read is what
//! the whole thing refuses to install.

mod support;

use support::{ReleaseServer, Scratch};

#[test]
fn a_response_larger_than_ten_megabytes_is_read() {
    // ureq's default body limit is 10 MiB. A registry packument can be larger: vite's
    // is, and with the default the whole package failed to install with
    // "the response body is larger than request limit" — which is how this was found.
    let scratch = Scratch::new("net-large");
    let served = scratch.dir("served");
    let size = 12 * 1024 * 1024;
    std::fs::write(served.join("packument.json"), vec![b'x'; size]).expect("the document");
    let server = ReleaseServer::serve(&served);

    let text = codets::net::get_text(&format!("{}/packument.json", server.base_url()))
        .expect("a 12 MiB response should be read");
    assert_eq!(text.len(), size);
}

#[test]
fn a_missing_document_reports_the_status_and_the_url() {
    // The message is what a person sees when a package cannot be found, so it has to
    // carry both the URL that was tried and what the server answered.
    let scratch = Scratch::new("net-404");
    let served = scratch.dir("served");
    std::fs::write(served.join("present.json"), "{}").expect("a file");
    let server = ReleaseServer::serve(&served);

    let url = format!("{}/absent.json", server.base_url());
    let error = match codets::net::get_text(&url) {
        Ok(_) => panic!("a 404 should be an error"),
        Err(error) => error,
    };
    assert!(error.contains(&url), "the URL should be in the message: {error}");
    assert!(error.contains("404"), "the status should be in the message: {error}");
}
