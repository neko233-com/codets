//! Package registries: where metadata and tarballs come from.
//!
//! Two backends, one interface. The HTTP backend speaks the npm registry protocol
//! (`GET <base>/<name>` for the packument, then the `dist.tarball` URL). The
//! directory backend reads the same shape off disk, which is what makes the
//! installer testable offline and usable against an internal mirror:
//!
//! ```text
//! <registry root>/
//!   lodash.json            a packument: {"versions": {"4.17.21": {...}}}
//!   lodash/4.17.21.tgz     the tarball, whose entries start with `package/`
//!   lodash/4.17.21/        ...or the extracted package, for fixture-friendly trees
//! ```

use std::path::{Path, PathBuf};

use super::semver::Semver;

/// One published version of a package.
#[derive(Clone, Debug)]
pub struct VersionInfo {
    pub version: Semver,
    /// Absolute URL for HTTP registries, or a path relative to the root for a
    /// directory registry.
    pub tarball: String,
    /// `sha512-<base64>` when the registry publishes one.
    pub integrity: Option<String>,
    pub dependencies: Vec<(String, String)>,
}

/// A package's published versions.
#[derive(Clone, Debug)]
pub struct PackageMetadata {
    pub name: String,
    pub versions: Vec<VersionInfo>,
}

impl PackageMetadata {
    pub fn version(&self, version: &Semver) -> Option<&VersionInfo> {
        self.versions.iter().find(|info| &info.version == version)
    }
}

/// What a registry hands back for one version.
pub enum PackageSource {
    /// A gzipped tarball, which the installer unpacks.
    Tarball(Vec<u8>),
    /// An already extracted directory, which the installer copies.
    Extracted(PathBuf),
}

/// Where packages come from.
pub enum Registry {
    Http { base: String },
    Directory { root: PathBuf },
}

impl Registry {
    /// Open a registry from `--registry`, which is either a URL or a path.
    pub fn open(spec: &str) -> Result<Registry, String> {
        if spec.starts_with("http://") || spec.starts_with("https://") {
            Ok(Registry::Http {
                base: spec.trim_end_matches('/').to_string(),
            })
        } else {
            let root = PathBuf::from(spec);
            if !root.is_dir() {
                return Err(format!(
                    "registry `{spec}` is neither an http(s) URL nor an existing directory"
                ));
            }
            Ok(Registry::Directory { root })
        }
    }

    /// The default registry, which is npm's.
    pub fn default_registry() -> Registry {
        Registry::Http {
            base: "https://registry.npmjs.org".to_string(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Registry::Http { base } => base.clone(),
            Registry::Directory { root } => format!("directory {}", root.display()),
        }
    }

    /// Fetch a package's published versions.
    pub fn metadata(&self, name: &str) -> Result<PackageMetadata, String> {
        match self {
            Registry::Http { base } => {
                let url = format!("{base}/{}", encode_package_name(name));
                let document = crate::net::get_text(&url)?;
                parse_packument(name, &document)
            }
            Registry::Directory { root } => {
                let path = root.join(format!("{name}.json"));
                let document = std::fs::read_to_string(&path).map_err(|error| {
                    format!("cannot read {} for package `{name}`: {error}", path.display())
                })?;
                parse_packument(name, &document)
            }
        }
    }

    /// Fetch one version's files.
    pub fn source(&self, name: &str, info: &VersionInfo) -> Result<PackageSource, String> {
        match self {
            Registry::Http { .. } => Ok(PackageSource::Tarball(crate::net::get_bytes(
                &info.tarball,
            )?)),
            Registry::Directory { root } => {
                // An extracted directory wins, because it needs no archive tool
                // and keeps fixtures readable.
                let version = info.version.to_string();
                let extracted = root.join(name).join(&version);
                if extracted.is_dir() {
                    return Ok(PackageSource::Extracted(extracted));
                }
                let archive = root.join(name).join(format!("{version}.tgz"));
                if archive.is_file() {
                    let bytes = std::fs::read(&archive)
                        .map_err(|error| format!("cannot read {}: {error}", archive.display()))?;
                    return Ok(PackageSource::Tarball(bytes));
                }
                // Fall back to whatever `dist.tarball` named, relative to the root.
                let named = root.join(&info.tarball);
                if named.is_file() {
                    let bytes = std::fs::read(&named)
                        .map_err(|error| format!("cannot read {}: {error}", named.display()))?;
                    return Ok(PackageSource::Tarball(bytes));
                }
                Err(format!(
                    "{} is missing from the registry at {}: expected {} or {}",
                    version,
                    root.display(),
                    archive.display(),
                    extracted.display()
                ))
            }
        }
    }
}

/// npm encodes a scoped name for the registry path.
pub fn encode_package_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('@') {
        format!("%40{}", rest.replace('/', "%2F"))
    } else {
        name.to_string()
    }
}

/// Read an npm packument.
fn parse_packument(name: &str, document: &str) -> Result<PackageMetadata, String> {
    use crate::builtins::json::Raw;
    let parsed = crate::builtins::json::parse(document)
        .ok_or_else(|| format!("the registry returned invalid JSON for `{name}`"))?;
    let Some(entries) = parsed.as_object() else {
        return Err(format!("the registry document for `{name}` is not an object"));
    };
    let versions_raw = entries
        .iter()
        .find(|(key, _)| key == "versions")
        .map(|(_, value)| value)
        .ok_or_else(|| format!("the registry document for `{name}` has no `versions`"))?;
    let Some(version_map) = versions_raw.as_object() else {
        return Err(format!("`versions` for `{name}` is not an object"));
    };

    let mut versions = Vec::new();
    for (version_text, body) in version_map {
        let Some(version) = Semver::parse(version_text) else {
            continue;
        };
        let field = |key: &str| {
            body.as_object()?
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        let tarball = match field("dist").as_ref().and_then(|dist| {
            dist.as_object()?
                .iter()
                .find(|(key, _)| key == "tarball")
                .map(|(_, value)| value.clone())
        }) {
            Some(Raw::Str(url)) => url,
            _ => String::new(),
        };
        let integrity = match field("dist").as_ref().and_then(|dist| {
            dist.as_object()?
                .iter()
                .find(|(key, _)| key == "integrity")
                .map(|(_, value)| value.clone())
        }) {
            Some(Raw::Str(value)) => Some(value),
            _ => None,
        };
        let mut dependencies = Vec::new();
        for key in ["dependencies", "optionalDependencies"] {
            if let Some(Raw::Object(map)) = field(key) {
                for (dependency, range) in map {
                    if let Raw::Str(range) = range {
                        dependencies.push((dependency.clone(), range.clone()));
                    }
                }
            }
        }
        versions.push(VersionInfo {
            version,
            tarball,
            integrity,
            dependencies,
        });
    }
    if versions.is_empty() {
        return Err(format!("`{name}` has no usable versions in the registry"));
    }
    versions.sort_by(|left, right| left.version.cmp(&right.version));
    Ok(PackageMetadata {
        name: name.to_string(),
        versions,
    })
}

/// True when `path` is inside `root`, which guards every extraction.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let candidate = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    candidate.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_scoped_names() {
        assert_eq!(encode_package_name("lodash"), "lodash");
        assert_eq!(encode_package_name("@types/node"), "%40types%2Fnode");
    }

    #[test]
    fn reads_a_packument() {
        let document = r#"{
            "name": "demo",
            "versions": {
                "1.0.0": {"dist": {"tarball": "http://x/demo-1.0.0.tgz", "integrity": "sha512-abc"},
                           "dependencies": {"left-pad": "^1.0.0"}},
                "1.1.0": {"dist": {"tarball": "http://x/demo-1.1.0.tgz"}}
            }
        }"#;
        let metadata = parse_packument("demo", document).expect("packument");
        assert_eq!(metadata.versions.len(), 2);
        let first = &metadata.versions[0];
        assert_eq!(first.version, Semver::new(1, 0, 0));
        assert_eq!(first.integrity.as_deref(), Some("sha512-abc"));
        assert_eq!(first.dependencies, vec![("left-pad".to_string(), "^1.0.0".to_string())]);
        // The highest version is last, which is what resolution wants.
        assert_eq!(metadata.versions[1].version, Semver::new(1, 1, 0));
    }
}
