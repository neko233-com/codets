//! Scratch directories for the integration tests.
//!
//! Every test that touches the filesystem puts its state in a [`Scratch`], which
//! removes its directory when it goes out of scope. The trailing
//! `std::fs::remove_dir_all` this replaces looked equivalent and was not: a panicking
//! test never reaches the line, so the directories piled up in the temp directory —
//! 110 MiB of them, one of which was a whole toolchain, the first time
//! `scripts/check-temp-clean` was run.
//!
//! Two properties matter:
//!
//! * **Unique per call.** The name carries the process id and a counter, so parallel
//!   tests in the same binary can never share a directory and a test can never read
//!   the previous run's state.
//! * **Removed on drop**, including while unwinding from a panic.
//!
//! [`Scratch`] derefs to `Path`, so `scratch.join("project")` and passing `&scratch`
//! to something that wants `&Path` both work.

// Not every test binary uses every helper here, and each binary compiles this module
// on its own.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use sha2::{Digest, Sha256, Sha512};

/// Hands out the counter that makes each directory unique.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory that deletes itself.
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Create an empty directory named for `suite`, which should say what is being
    /// tested: `codets-test-<suite>-<pid>-<n>`.
    pub fn new(suite: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "codets-test-{suite}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Scratch { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A path below the scratch directory, created as a directory.
    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        std::fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    /// A path below the scratch directory, created as a file with `body`.
    pub fn write(&self, relative: &str, body: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("scratch parent");
        }
        std::fs::write(&path, body).expect("scratch file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a file still open elsewhere should not turn a passing test
        // into a failing one. `scripts/check-temp-clean` reports what survives.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// Every file below `directory` as `relative path:content hash`, sorted.
///
/// Comparing this before and after something that claims to be idempotent is what
/// "nothing changed" means; comparing a file count would miss a rewritten file.
pub fn tree(directory: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(directory, directory, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, directory: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            walk(root, &path, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read(&path).unwrap_or_default();
        found.push(format!("{relative}:{}", hex(&Sha512::digest(&body))));
    }
}

/// Lower-case hex, for the digests written into checksum files and compared here.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The file name the toolchain binary has on this platform.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "codets.exe"
    } else {
        "codets"
    }
}

// ------------------------------------------------------------- a fake release

/// What the published checksum file should say about the archive.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Checksum {
    /// The real digest, as the workflow writes it.
    Correct,
    /// A digest that cannot match, for testing that a mismatch is fatal.
    Wrong,
    /// No checksum file at all, which an older release may not have.
    Missing,
}

/// Write a release into `directory` the way `.github/workflows/release.yml`
/// publishes it, and return the two asset names.
///
/// The archive holds the binary the tests were built with, so an install that
/// downloads it produces a working toolchain rather than a placeholder.
pub fn publish_release(
    directory: &Path,
    version: &str,
    triple: &str,
    checksum: Checksum,
) -> (String, String) {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_codets")).expect("the built codets binary");
    let release = directory.join(format!("v{version}"));
    std::fs::create_dir_all(&release).expect("release directory");

    let stage = format!("codets-{version}-{triple}");
    let payload = tar_gz(&stage, binary_name(), &binary);
    let archive = format!("{stage}.tar.gz");
    std::fs::write(release.join(&archive), &payload).expect("archive");

    let checksums = format!("codets-{version}-checksums.txt");
    let digest = match checksum {
        Checksum::Correct => hex(&Sha256::digest(&payload)),
        Checksum::Wrong => "0".repeat(64),
        Checksum::Missing => {
            return (archive, checksums);
        }
    };
    std::fs::write(release.join(&checksums), format!("{digest}  {archive}\n"))
        .expect("checksums");
    (archive, checksums)
}

/// A `.tar.gz` holding `name` under a `prefix/` directory, executable, which is the
/// layout the workflow publishes.
fn tar_gz(prefix: &str, name: &str, body: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, body) in [
        (format!("{prefix}/{name}"), body),
        (format!("{prefix}/README.md"), &b"# codets"[..]),
    ] {
        // A fresh header per entry: the size lives in the header, and reusing one
        // produces an archive that only a lenient tar will read.
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, path, body).expect("append");
    }
    let tar_bytes = builder.into_inner().expect("finish the tar");

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("finish the gzip")
}

/// A minimal HTTP server for release assets.
///
/// `codets upgrade` and both installers reconstruct asset URLs from the version and
/// the target triple, so exercising them needs something to download from. This
/// speaks just enough HTTP for `curl`, `wget` and PowerShell's `Invoke-WebRequest`,
/// with one connection per request and no keep-alive.
pub struct ReleaseServer {
    base: String,
    root: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ReleaseServer {
    /// Serve `root` on a free loopback port.
    pub fn serve(root: &Path) -> ReleaseServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let base = format!("http://{}", listener.local_addr().expect("a bound address"));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let served = root.to_path_buf();

        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut stream) = stream else {
                    continue;
                };
                if let Some(path) = request_path(&mut stream) {
                    respond(&mut stream, &served, &path);
                }
            }        });

        ReleaseServer {
            base,
            root: root.to_path_buf(),
            stop,
            thread: Some(thread),
        }
    }

    /// The base URL to hand to `CODETS_DOWNLOAD_BASE`.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ReleaseServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // One connection to wake `accept`, so the thread notices the flag and the
        // test does not leave a server running.
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The path of the request on the first line, or `None` when it is not a GET.
///
/// The whole request is read, headers included. Leaving them in the socket makes the
/// kernel reset the connection when the stream is dropped instead of closing it, and
/// a reset discards whatever the client has not read yet — which shows up as a
/// truncated download rather than as a server bug.
fn request_path(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    while buffer.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                buffer.push(byte[0]);
                if buffer.ends_with(b"\r\n\r\n") || buffer.ends_with(b"\n\n") {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut parts = text.lines().next()?.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(path)) => Some(path.to_string()),
        _ => None,
    }
}

/// Answer one request from the served directory.
fn respond(stream: &mut TcpStream, root: &Path, path: &str) {
    let relative = path.trim_start_matches('/');
    // Nothing may reach outside the served directory.
    let contained = !relative.is_empty()
        && !relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    let body = if contained {
        std::fs::read(root.join(relative)).ok()
    } else {
        None
    };
    match body {
        Some(body) => write_response(stream, 200, "OK", &body),
        None => write_response(stream, 404, "Not Found", b"no such asset\n"),
    }
}

fn write_response(stream: &mut TcpStream, code: u16, reason: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    // Half-close rather than just dropping the stream, so the client reads a clean
    // end of response.
    let _ = stream.shutdown(std::net::Shutdown::Write);
}
