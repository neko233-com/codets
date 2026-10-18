//! The installer: dependency resolution, then a multi-threaded fetch.
//!
//! This is the only place in `codets` that runs work in parallel. JavaScript
//! execution stays single-threaded by specification, so the parallelism lives
//! where it is genuinely useful: resolving the graph, then downloading,
//! verifying and unpacking every package at once.
//!
//! Dependency resolution is deliberately flat, the way npm's default install is
//! for compatible ranges: each package name is resolved exactly once, against
//! every range requested for it. When no single version can satisfy them all, the
//! installer picks the best version for the first request and records a warning,
//! instead of building a nested tree. That covers the common case and is honest
//! about the rest — see the crate documentation's list of limits.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha512};

use super::registry::{PackageSource, Registry, VersionInfo};
use super::semver::{Range, Semver};
use super::Manifest;

/// How to install.
pub struct InstallOptions {
    pub registry: Registry,
    /// Worker threads for download and unpack.
    pub jobs: usize,
    /// Include `devDependencies`.
    pub include_dev: bool,
    /// Delete `node_modules` before installing.
    pub clean: bool,
}

/// One package that ended up on disk.
#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub name: String,
    pub version: Semver,
    pub path: PathBuf,
    /// Size of what was written, for the summary.
    pub bytes: u64,
}

/// The result of an install.
pub struct InstallReport {
    pub packages: Vec<InstalledPackage>,
    /// Wall-clock time for the download and unpack phase.
    pub transfer: Duration,
    pub elapsed: Duration,
    pub jobs: usize,
    pub warnings: Vec<String>,
}

/// Install the dependencies declared in `project`'s `package.json`.
pub fn install(project: &Path, options: &InstallOptions) -> Result<InstallReport, String> {
    let started = Instant::now();
    let manifest_path = project.join("package.json");
    let manifest = super::read_manifest(&manifest_path)?;
    let modules_dir = project.join("node_modules");

    if options.clean {
        let _ = std::fs::remove_dir_all(&modules_dir);
    }
    std::fs::create_dir_all(&modules_dir)
        .map_err(|error| format!("cannot create {}: {error}", modules_dir.display()))?;

    // ---- resolve
    let mut requirements: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut push = |name: &str, range: &str| {
        requirements
            .entry(name.to_string())
            .or_default()
            .push(range.to_string());
        if !order.contains(&name.to_string()) {
            order.push(name.to_string());
        }
    };
    for (name, range) in &manifest.dependencies {
        push(name, range);
    }
    if options.include_dev {
        for (name, range) in &manifest.dev_dependencies {
            push(name, range);
        }
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut resolved: BTreeMap<String, (Semver, VersionInfo)> = BTreeMap::new();
    let mut metadata_cache: HashMap<String, super::registry::PackageMetadata> = HashMap::new();
    let mut index = 0;
    while index < order.len() {
        let name = order[index].clone();
        index += 1;
        if resolved.contains_key(&name) {
            continue;
        }
        let ranges = requirements.get(&name).cloned().unwrap_or_default();
        if ranges
            .iter()
            .any(|range| range.starts_with("file:") || range.starts_with("link:") || range.starts_with("git"))
        {
            warnings.push(format!(
                "{name}: range `{}` is not a registry version, so it was skipped",
                ranges.join(", ")
            ));
            continue;
        }
        let metadata = match metadata_cache.get(&name) {
            Some(cached) => cached.clone(),
            None => match options.registry.metadata(&name) {
                Ok(metadata) => {
                    metadata_cache.insert(name.clone(), metadata.clone());
                    metadata
                }
                Err(error) => {
                    warnings.push(format!("{name}: {error}"));
                    continue;
                }
            },
        };

        let parsed: Vec<Range> = ranges
            .iter()
            .filter_map(|range| Range::parse(range))
            .collect();
        let chosen = metadata
            .versions
            .iter()
            .rev()
            .find(|info| {
                parsed
                    .iter()
                    .all(|range| range.matches(&info.version))
            })
            .or_else(|| {
                // No version satisfies everything: take the best for the first
                // range so the install still makes progress, and say so.
                let range = parsed.first()?;
                let best = range.best(metadata.versions.iter().map(|info| &info.version))?;
                metadata.version(&best)
            });
        let Some(chosen) = chosen else {
            warnings.push(format!(
                "{name}: no published version satisfies {}",
                ranges.join(", ")
            ));
            continue;
        };
        if parsed.len() > 1
            && !parsed.iter().all(|range| range.matches(&chosen.version))
        {
            warnings.push(format!(
                "{name}: requested as {} but only {} was installable; a nested copy would be needed to satisfy both",
                ranges.join(", "),
                chosen.version
            ));
        }

        for (dependency, range) in &chosen.dependencies {
            if !requirements.contains_key(dependency) {
                order.push(dependency.clone());
            }
            requirements
                .entry(dependency.clone())
                .or_default()
                .push(range.clone());
        }
        resolved.insert(name.clone(), (chosen.version.clone(), chosen.clone()));
    }

    // ---- fetch, verify and unpack, in parallel
    let transfer_started = Instant::now();
    let targets: Vec<(String, VersionInfo)> = resolved
        .iter()
        .map(|(name, (_, info))| (name.clone(), info.clone()))
        .collect();
    let registry = &options.registry;
    let modules = modules_dir.clone();
    let outcomes = parallel_map(targets, options.jobs, |(name, info)| {
        fetch_and_place(registry, &modules, &name, &info)
    });
    let transfer = transfer_started.elapsed();

    let mut packages = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(package) => packages.push(package),
            Err(error) => warnings.push(error),
        }
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));

    // ---- bin links and lockfile
    link_binaries(&modules_dir, &packages);
    write_lockfile(project, &manifest, &packages, options)?;

    Ok(InstallReport {
        packages,
        transfer,
        elapsed: started.elapsed(),
        jobs: options.jobs.min(targets_len(&resolved)),
        warnings,
    })
}

fn targets_len(resolved: &BTreeMap<String, (Semver, VersionInfo)>) -> usize {
    resolved.len().max(1)
}

/// Download, verify and unpack one package into `node_modules`.
fn fetch_and_place(
    registry: &Registry,
    modules: &Path,
    name: &str,
    info: &VersionInfo,
) -> Result<InstalledPackage, String> {
    let source = registry.source(name, info)?;
    let written = match source {
        PackageSource::Tarball(bytes) => {
            verify_integrity(name, info, &bytes)?;
            let destination = modules.join(name);
            if destination.exists() {
                let _ = std::fs::remove_dir_all(&destination);
            }
            std::fs::create_dir_all(&destination)
                .map_err(|error| format!("cannot create {}: {error}", destination.display()))?;
            unpack(&bytes, &destination)
                .map_err(|error| format!("{name}: cannot unpack the tarball: {error}"))?
        }
        PackageSource::Extracted(from) => {
            let destination = modules.join(name);
            if destination.exists() {
                let _ = std::fs::remove_dir_all(&destination);
            }
            copy_tree(&from, &destination)
                .map_err(|error| format!("{name}: cannot copy {}: {error}", from.display()))?
        }
    };
    Ok(InstalledPackage {
        name: name.to_string(),
        version: info.version.clone(),
        path: modules.join(name),
        bytes: written,
    })
}

/// Check a tarball against the registry's `sha512-...` digest.
fn verify_integrity(name: &str, info: &VersionInfo, bytes: &[u8]) -> Result<(), String> {
    let Some(integrity) = &info.integrity else {
        return Ok(());
    };
    let Some(expected) = integrity.strip_prefix("sha512-") else {
        // Other algorithms exist in the wild; the installer verifies sha512 only.
        return Ok(());
    };
    let digest = Sha512::digest(bytes);
    let actual = base64_encode(&digest);
    if actual.trim_end_matches('=') == expected.trim_end_matches('=') {
        Ok(())
    } else {
        Err(format!(
            "{name}@{} failed integrity verification: expected sha512-{expected}, got sha512-{actual}",
            info.version
        ))
    }
}

/// Gunzip and untar, stripping the archive's leading `package/` directory.
///
/// Returns the number of bytes written. Entries that would escape the
/// destination are rejected rather than followed.
fn unpack(bytes: &[u8], destination: &Path) -> Result<u64, String> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut written = 0u64;
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path().map_err(|error| error.to_string())?.to_path_buf();
        let relative: PathBuf = path.components().skip(1).collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("refusing to extract the escaping path {}", path.display()));
        }
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        entry
            .unpack(&target)
            .map_err(|error| format!("{}: {error}", relative.display()))?;
        written += entry.size();
    }
    Ok(written)
}

/// Copy a directory tree, returning the bytes written.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    let mut written = 0u64;
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            written += copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
            written += metadata.len();
        }
    }
    Ok(written)
}

/// Write shims for every `bin` a package declares into `node_modules/.bin`.
fn link_binaries(modules: &Path, packages: &[InstalledPackage]) {
    let bin_dir = modules.join(".bin");
    if std::fs::create_dir_all(&bin_dir).is_err() {
        return;
    }
    for package in packages {
        let manifest = match super::read_manifest(&package.path.join("package.json")) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        for (name, relative) in &manifest.bin {
            let script = package.path.join(relative);
            if !script.is_file() {
                continue;
            }
            let shim = if cfg!(windows) {
                bin_dir.join(format!("{name}.cmd"))
            } else {
                bin_dir.join(name)
            };
            let codets = std::env::current_exe()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| "codets".to_string());
            let script = script.to_string_lossy().to_string();
            let body = if cfg!(windows) {
                format!("@echo off\r\n\"{codets}\" \"{script}\" %*\r\n")
            } else {
                format!("#!/bin/sh\nexec \"{codets}\" \"{script}\" \"$@\"\n")
            };
            let _ = std::fs::write(&shim, body);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
}

/// Write `package-lock.json` in npm's v3 shape.
fn write_lockfile(
    project: &Path,
    manifest: &Manifest,
    packages: &[InstalledPackage],
    options: &InstallOptions,
) -> Result<(), String> {
    let mut lock = String::new();
    lock.push_str("{\n");
    lock.push_str(&format!(
        "  \"name\": {},\n",
        json_string(manifest.name.as_deref().unwrap_or(""))
    ));
    lock.push_str(&format!(
        "  \"version\": {},\n",
        json_string(manifest.version.as_deref().unwrap_or("0.0.0"))
    ));
    lock.push_str("  \"lockfileVersion\": 3,\n  \"requires\": true,\n  \"packages\": {\n");
    lock.push_str("    \"\": {\n");
    lock.push_str(&format!(
        "      \"name\": {},\n      \"version\": {},\n",
        json_string(manifest.name.as_deref().unwrap_or("")),
        json_string(manifest.version.as_deref().unwrap_or("0.0.0"))
    ));
    lock.push_str("      \"dependencies\": ");
    lock.push_str(&json_map(&manifest.dependencies));
    if options.include_dev && !manifest.dev_dependencies.is_empty() {
        lock.push_str(",\n      \"devDependencies\": ");
        lock.push_str(&json_map(&manifest.dev_dependencies));
    }
    lock.push_str("\n    }");
    for package in packages {
        lock.push_str(",\n");
        lock.push_str(&format!("    \"node_modules/{}\": {{\n", package.name));
        lock.push_str(&format!(
            "      \"version\": {},\n      \"resolved\": {},\n      \"license\": \"\"\n    }}",
            json_string(&package.version.to_string()),
            json_string(&options.registry.describe())
        ));
    }
    lock.push_str("\n  }\n}\n");
    let path = project.join("package-lock.json");
    std::fs::write(&path, lock).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// A JSON string literal.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON object literal from name/value pairs.
pub fn json_map(entries: &[(String, String)]) -> String {
    let body: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("{}: {}", json_string(key), json_string(value)))
        .collect();
    format!("{{{}}}", body.join(", "))
}

/// Standard base64, which is how npm spells an integrity digest.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Run `work` over `items` on `jobs` threads, preserving input order.
///
/// A shared work queue means a slow item does not idle the other workers, which
/// is the point of a package installer being parallel: the registry's latency
/// dominates, not the CPU.
pub fn parallel_map<T, R, F>(items: Vec<T>, jobs: usize, work: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    let jobs = jobs.max(1).min(items.len());
    let queue = Arc::new(Mutex::new(
        items.into_iter().enumerate().collect::<VecDeque<(usize, T)>>(),
    ));
    let results: Arc<Mutex<Vec<(usize, R)>>> = Arc::new(Mutex::new(Vec::new()));

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let work = &work;
            scope.spawn(move || loop {
                let next = {
                    let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
                    queue.pop_front()
                };
                let Some((index, item)) = next else {
                    break;
                };
                let result = work(item);
                results
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push((index, result));
            });
        }
    });

    let mut collected = Arc::try_unwrap(results)
        .map(|inner| inner.into_inner().unwrap_or_default())
        .unwrap_or_default();
    collected.sort_by_key(|(index, _)| *index);
    collected.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_preserves_order() {
        let items: Vec<u32> = (0..64).collect();
        let doubled = parallel_map(items, 8, |value| value * 2);
        assert_eq!(doubled, (0..64).map(|value| value * 2).collect::<Vec<_>>());
    }

    #[test]
    fn parallel_map_uses_the_requested_threads() {
        // Each item sleeps, so the workers have to overlap for the run to finish
        // quickly; that makes "more than one thread ran" observable rather than a
        // scheduling accident.
        let seen = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let items: Vec<u32> = (0..32).collect();
        let seen_inside = Arc::clone(&seen);
        let _ = parallel_map(items, 4, move |value| {
            std::thread::sleep(Duration::from_millis(10));
            let thread = format!("{:?}", std::thread::current().id());
            seen_inside.lock().unwrap().insert(thread);
            value
        });
        assert!(
            seen.lock().unwrap().len() > 1,
            "expected more than one worker thread, saw {:?}",
            seen.lock().unwrap()
        );
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
