//! The one place that talks to the network.
//!
//! `codets` fetches two kinds of thing: registry metadata documents and package
//! tarballs. Both are plain GETs, so this module is a thin wrapper that turns
//! transport errors into the messages the command line prints.
//!
//! TLS comes from the platform's own stack (`native-tls`: SChannel on Windows,
//! Security.framework on macOS, OpenSSL on Linux) rather than from a bundled
//! engine, because a bundled engine means compiling C when `codets` is built.
//! That is also why the provider is set explicitly: ureq's default provider is
//! Rustls, and enabling the `native-tls` feature alone would leave the two
//! disagreeing at the first `https://` request.
//!
//! One agent per thread, so a parallel install reuses its connections.

use std::cell::RefCell;

use ureq::config::Config;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::Agent;

thread_local! {
    /// The per-thread agent. `RefCell` keeps the lazily built agent off the hot
    /// path while still sharing one connection pool per worker.
    static AGENT: RefCell<Option<Agent>> = const { RefCell::new(None) };
}

/// Build the agent, configured for the platform's TLS stack.
fn build_agent() -> Agent {
    Config::builder()
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                // The default root store is Mozilla's static bundle. Using the
                // platform verifier instead means the certificates the operating
                // system already trusts are the ones that count, which is what
                // makes `npm install` work behind a corporate proxy.
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

/// Run `work` with this thread's agent.
fn with_agent<T>(work: impl FnOnce(&Agent) -> T) -> T {
    AGENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let agent = slot.get_or_insert_with(build_agent);
        work(agent)
    })
}

/// Fetch a URL into memory.
pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    with_agent(|agent| {
        let response = agent
            .get(url)
            .call()
            .map_err(|error| format!("GET {url} failed: {error}"))?;
        let mut body = response.into_body();
        body.read_to_vec()
            .map_err(|error| format!("reading {url} failed: {error}"))
    })
}

/// Fetch a URL as text.
pub fn get_text(url: &str) -> Result<String, String> {
    let bytes = get_bytes(url)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}
