//! Reaching `codets`'s own releases.
//!
//! Two callers need this: `codets upgrade`, which replaces the running toolchain
//! with a published build, and the one-line installers, which need to know which
//! asset to fetch before any `codets` exists. Both therefore agree on one naming
//! scheme:
//!
//! ```text
//! https://github.com/neko233-com/codets/releases/download/v0.2.0/codets-0.2.0-x86_64-pc-windows-msvc.tar.gz
//! https://github.com/neko233-com/codets/releases/download/v0.2.0/codets-0.2.0-checksums.txt
//! ```
//!
//! Every platform publishes a `.tar.gz`, including Windows. Windows has shipped
//! `tar.exe` since Windows 10, and `codets` unpacks its own archives anyway, so a
//! second format would only be another thing to get wrong.
//!
//! Two environment variables make mirrors and air-gapped setups possible:
//! `CODETS_DOWNLOAD_BASE` and `CODETS_API_BASE`.

use std::path::Path;

use crate::builtins::json;

/// The GitHub repository releases are published from.
pub const REPO: &str = "neko233-com/codets";

/// The release tag prefix.
pub const TAG_PREFIX: &str = "v";

/// The target triple this build runs on, as it appears in an asset name.
///
/// `None` on a platform with no published build, which callers report rather
/// than guessing a nearby triple.
pub fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// Every triple a release publishes, for the documentation and the pipeline.
pub fn published_triples() -> &'static [&'static str] {
    &[
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ]
}

/// The asset file name for one version and platform.
pub fn asset_name(version: &str, triple: &str) -> String {
    format!("codets-{version}-{triple}.tar.gz")
}

/// The checksum file name, which covers every asset of one release.
pub fn checksums_name(version: &str) -> String {
    format!("codets-{version}-checksums.txt")
}

/// The base URL assets are downloaded from.
pub fn download_base() -> String {
    std::env::var("CODETS_DOWNLOAD_BASE").unwrap_or_else(|_| {
        format!("https://github.com/{REPO}/releases/download")
    })
}

/// The base URL of the releases API.
pub fn api_base() -> String {
    std::env::var("CODETS_API_BASE")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}"))
}

/// Where one asset lives.
pub fn download_url(version: &str, triple: &str) -> String {
    format!(
        "{}/{TAG_PREFIX}{version}/{}",
        download_base().trim_end_matches('/'),
        asset_name(version, triple)
    )
}

/// Where the checksum file lives.
pub fn checksums_url(version: &str) -> String {
    format!(
        "{}/{TAG_PREFIX}{version}/{}",
        download_base().trim_end_matches('/'),
        checksums_name(version)
    )
}

/// The newest published version, without its tag prefix.
pub fn latest_version() -> Result<String, String> {
    let url = format!("{}/releases/latest", api_base().trim_end_matches('/'));
    let document = crate::net::get_text(&url)?;
    let parsed = json::parse(&document)
        .ok_or_else(|| format!("the release API returned invalid JSON from {url}"))?;
    let tag = parsed
        .as_object()
        .and_then(|entries| {
            entries
                .iter()
                .find(|(key, _)| key == "tag_name")
                .map(|(_, value)| value.clone())
        })
        .and_then(|value| match value {
            json::Raw::Str(tag) => Some(tag),
            _ => None,
        })
        .ok_or_else(|| format!("the release API did not report a tag_name for {url}"))?;
    Ok(tag.strip_prefix(TAG_PREFIX).unwrap_or(&tag).to_string())
}

/// Fetch and verify the public binaries for `version`, or the newest release.
///
/// Returns the version and the raw binary, ready to be installed.
pub fn fetch_binary(version: Option<&str>) -> Result<(String, Vec<u8>), String> {
    let version = match version {
        Some(version) => version.trim_start_matches(TAG_PREFIX).to_string(),
        None => latest_version()?,
    };
    let triple = target_triple().ok_or_else(|| {
        format!(
            "there is no published build for {} {}; the supported platforms are {:?}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            published_triples()
        )
    })?;
    let url = download_url(&version, triple);
    let archive = crate::net::get_bytes(&url)?;
    verify_checksum(&version, &asset_name(&version, triple), &archive)?;
    let binary = extract_binary(&archive)?;
    Ok((version, binary))
}

/// Check an archive against the release's checksum file.
///
/// A missing checksum file is not fatal — an older release may not have one — but
/// a *mismatch* always is.
fn verify_checksum(version: &str, asset: &str, archive: &[u8]) -> Result<(), String> {
    let Ok(checksums) = crate::net::get_text(&checksums_url(version)) else {
        return Ok(());
    };
    let Some(expected) = checksums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if name == asset {
            Some(digest.to_ascii_lowercase())
        } else {
            None
        }
    }) else {
        return Ok(());
    };
    let actual = sha256_hex(archive);
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{asset} failed its checksum: expected sha256 {expected}, got {actual}"
        ))
    }
}

/// The lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Pull the toolchain binary out of a release archive.
pub fn extract_binary(archive: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let wanted = crate::versions::binary_name();
    for entry in tar.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path().map_err(|error| error.to_string())?.to_path_buf();
        if path.file_name().map(|name| name == wanted).unwrap_or(false) {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)
                .map_err(|error| format!("cannot read {wanted} from the archive: {error}"))?;
            return Ok(bytes);
        }
    }
    Err(format!(
        "the archive does not contain {wanted}; it may be for another platform"
    ))
}

/// True when `path` looks like a `codets` binary of the right platform.
pub fn looks_like_a_toolchain(path: &Path) -> bool {
    path.is_file() && path.file_name().map(|name| name == crate::versions::binary_name()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_assets_the_way_the_pipeline_does() {
        assert_eq!(
            asset_name("0.2.0", "x86_64-pc-windows-msvc"),
            "codets-0.2.0-x86_64-pc-windows-msvc.tar.gz"
        );
        assert_eq!(checksums_name("0.2.0"), "codets-0.2.0-checksums.txt");
        assert!(download_url("0.2.0", "x86_64-unknown-linux-gnu").ends_with(
            "/v0.2.0/codets-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        ));
    }

    #[test]
    fn the_running_platform_has_a_triple() {
        // Every platform the project publishes for must resolve, and the list must
        // agree with `published_triples`.
        if let Some(triple) = target_triple() {
            assert!(published_triples().contains(&triple), "{triple}");
        }
    }

    #[test]
    fn hashes_are_sha256_hex() {
        // The empty string and "abc" are the standard test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn extracts_the_binary_from_an_archive() {
        // Build the same layout the pipeline publishes.
        let mut builder = tar::Builder::new(Vec::new());
        for (name, body) in [
            ("README.md", "# codets"),
            (crate::versions::binary_name(), "binary bytes"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, body.as_bytes())
                .expect("append");
        }
        let tar_bytes = builder.into_inner().expect("finish");
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar_bytes).expect("gzip");
        let archive = encoder.finish().expect("gzip finish");

        let binary = extract_binary(&archive).expect("extract");
        assert_eq!(binary, b"binary bytes");
    }

    #[test]
    fn a_foreign_archive_is_reported() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "something-else", &b"data"[..])
            .expect("append");
        let tar_bytes = builder.into_inner().expect("finish");
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar_bytes).expect("gzip");
        let archive = encoder.finish().expect("gzip finish");
        assert!(extract_binary(&archive).is_err());
    }
}
