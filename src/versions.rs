//! The built-in toolchain version manager.
//!
//! `codets` manages its own toolchain the way `rustup` and `fnm` do, so a project
//! can pin the Node.js release it is written against and switching is one command.
//!
//! ```text
//! ~/.codets/                     (or $CODETS_HOME)
//!   bin/                         shims to put on PATH: codets, node, npm
//!   versions/
//!     24.0.0/                    one installed toolchain
//!       toolchain.conf           version, node compat level, install source
//!       node, npm, codets        shims that pin this version
//!   current                      the active version
//!   settings.conf                `default=<version>`
//! ```
//!
//! Resolution order for the active version, first match wins:
//!
//! 1. `$CODETS_NODE_VERSION`
//! 2. the nearest `.codets-version` file, walking up from the working directory
//! 3. the `current` pointer
//! 4. `default` in `settings.conf`
//! 5. the built-in default (the newest supported Node LTS line)
//!
//! Installing a version materialises a toolchain directory and its shims. That is
//! what makes `node`, `npm` and `codets` switch together, and it needs no network:
//! the shims pin the compat level of the `codets` binary that wrote them. Fetching
//! a *newer* `codets` binary is a separate operation (`codets upgrade`), because
//! that is the only step that needs a release server.

use std::fs;
use std::path::{Path, PathBuf};

use crate::node::{NodeCompat, Version};

/// The environment variable that forces a version, for scripts and CI.
pub const VERSION_ENV: &str = "CODETS_NODE_VERSION";

/// The per-project pin file, in the spirit of `.nvmrc`.
pub const VERSION_FILE: &str = ".codets-version";

/// One installed toolchain.
#[derive(Clone, Debug)]
pub struct InstalledVersion {
    pub version: Version,
    pub node: NodeCompat,
    pub path: PathBuf,
    /// Where the toolchain came from: `"materialised"` or a fetch URL.
    pub source: String,
}

impl InstalledVersion {
    pub fn describe(&self) -> String {
        format!("{} — {}", self.version, self.node.describe())
    }
}

/// Why a particular version is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VersionSource {
    Environment,
    ProjectFile,
    CurrentPointer,
    SettingsDefault,
    BuiltIn,
}

impl VersionSource {
    pub fn describe(self) -> &'static str {
        match self {
            VersionSource::Environment => "environment (CODETS_NODE_VERSION)",
            VersionSource::ProjectFile => ".codets-version",
            VersionSource::CurrentPointer => "codets current",
            VersionSource::SettingsDefault => "codets default",
            VersionSource::BuiltIn => "built-in default",
        }
    }
}

/// The version that applies, and why.
#[derive(Clone, Copy, Debug)]
pub struct Resolution {
    pub node: NodeCompat,
    pub source: VersionSource,
}

/// The on-disk toolchain store.
pub struct VersionStore {
    root: PathBuf,
}

impl VersionStore {
    /// The store in `$CODETS_HOME`, or `~/.codets`.
    pub fn discover() -> VersionStore {
        if let Ok(home) = std::env::var("CODETS_HOME") {
            if !home.trim().is_empty() {
                return VersionStore::at(home);
            }
        }
        let base = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        VersionStore::at(base.join(".codets"))
    }

    pub fn at(root: impl Into<PathBuf>) -> VersionStore {
        VersionStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    fn current_file(&self) -> PathBuf {
        self.root.join("current")
    }

    fn settings_file(&self) -> PathBuf {
        self.root.join("settings.conf")
    }

    fn create_layout(&self) -> Result<(), String> {
        for directory in [self.root.clone(), self.versions_dir(), self.bin_dir()] {
            fs::create_dir_all(&directory)
                .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        }
        Ok(())
    }

    /// Every installed toolchain, oldest first.
    pub fn installed(&self) -> Vec<InstalledVersion> {
        let mut found = Vec::new();
        let Ok(entries) = fs::read_dir(self.versions_dir()) else {
            return found;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(version) = Version::parse(&name) else {
                continue;
            };
            let manifest = entry.path().join("toolchain.conf");
            let source = read_conf(&manifest)
                .into_iter()
                .find(|(key, _)| key == "source")
                .map(|(_, value)| value)
                .unwrap_or_else(|| "materialised".to_string());
            found.push(InstalledVersion {
                version,
                node: NodeCompat::parse(&version.to_string())
                    .unwrap_or_else(|| NodeCompat::for_major(version.major)),
                path: entry.path(),
                source,
            });
        }
        found.sort_by_key(|entry| entry.version);
        found
    }

    pub fn is_installed(&self, version: &Version) -> bool {
        self.versions_dir().join(version.to_string()).join("toolchain.conf").is_file()
    }

    pub fn find(&self, version: &Version) -> Option<InstalledVersion> {
        self.installed()
            .into_iter()
            .find(|entry| entry.version == *version)
    }

    /// Install a toolchain by materialising its directory and shims.
    pub fn install(&self, version: Version) -> Result<InstalledVersion, String> {
        self.create_layout()?;
        let directory = self.versions_dir().join(version.to_string());
        fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;

        let compat = NodeCompat::for_major(version.major);
        let manifest = format!(
            "version={version}\nnode={}\ncodets={}\nsource=materialised\n",
            compat.version_string(),
            env!("CARGO_PKG_VERSION")
        );
        fs::write(directory.join("toolchain.conf"), manifest)
            .map_err(|error| format!("cannot write the manifest: {error}"))?;

        let installed = InstalledVersion {
            version,
            node: compat,
            path: directory,
            source: "materialised".to_string(),
        };
        write_version_shims(&installed)?;
        write_bin_shims(self, &installed)?;
        Ok(installed)
    }

    /// Delete a toolchain. The active version cannot be removed.
    pub fn remove(&self, version: &Version) -> Result<(), String> {
        if self.current() == Some(*version) {
            return Err(format!(
                "{} is the active version; run `codets use <other>` first",
                version
            ));
        }
        let directory = self.versions_dir().join(version.to_string());
        if !directory.is_dir() {
            return Err(format!("{version} is not installed"));
        }
        fs::remove_dir_all(&directory)
            .map_err(|error| format!("cannot remove {}: {error}", directory.display()))
    }

    /// The active version, or `None` when nothing is installed.
    pub fn current(&self) -> Option<Version> {
        read_pointer(&self.current_file())
    }

    pub fn set_current(&self, version: &Version) -> Result<(), String> {
        self.create_layout()?;
        write_pointer(&self.current_file(), version)
    }

    pub fn default_version(&self) -> Option<Version> {
        read_conf(&self.settings_file())
            .into_iter()
            .find(|(key, _)| key == "default")
            .and_then(|(_, value)| Version::parse(&value))
    }

    pub fn set_default(&self, version: &Version) -> Result<(), String> {
        self.create_layout()?;
        fs::write(
            self.settings_file(),
            format!("default={version}\n"),
        )
        .map_err(|error| format!("cannot write {}: {error}", self.settings_file().display()))
    }

    /// Which version applies when running from `from`, and why.
    pub fn resolve(&self, from: &Path) -> Resolution {
        if let Ok(forced) = std::env::var(VERSION_ENV) {
            if let Some(node) = NodeCompat::parse(&forced) {
                return Resolution {
                    node,
                    source: VersionSource::Environment,
                };
            }
        }
        if let Some(pinned) = find_project_pin(from) {
            if let Some(node) = NodeCompat::parse(&pinned) {
                return Resolution {
                    node,
                    source: VersionSource::ProjectFile,
                };
            }
        }
        if let Some(current) = self.current() {
            if let Some(node) = NodeCompat::parse(&current.to_string()) {
                return Resolution {
                    node,
                    source: VersionSource::CurrentPointer,
                };
            }
        }
        if let Some(default) = self.default_version() {
            if let Some(node) = NodeCompat::parse(&default.to_string()) {
                return Resolution {
                    node,
                    source: VersionSource::SettingsDefault,
                };
            }
        }
        Resolution {
            node: NodeCompat::default(),
            source: VersionSource::BuiltIn,
        }
    }
}

/// Look for `.codets-version`, starting at `from` and walking up.
pub fn find_project_pin(from: &Path) -> Option<String> {
    let mut directory = if from.is_dir() {
        from.to_path_buf()
    } else {
        from.parent()?.to_path_buf()
    };
    loop {
        let candidate = directory.join(VERSION_FILE);
        if let Ok(contents) = fs::read_to_string(&candidate) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        if !directory.pop() {
            return None;
        }
    }
}

/// Write the pin file that makes one directory use a specific version.
pub fn write_project_pin(directory: &Path, version: &Version) -> Result<PathBuf, String> {
    let path = directory.join(VERSION_FILE);
    fs::write(&path, format!("{version}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

fn read_pointer(path: &Path) -> Option<Version> {
    let contents = fs::read_to_string(path).ok()?;
    Version::parse(contents.trim())
}

fn write_pointer(path: &Path, version: &Version) -> Result<(), String> {
    fs::write(path, format!("{version}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn read_conf(path: &Path) -> Vec<(String, String)> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// The executable the shims should call.
fn self_executable() -> String {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| "codets".to_string())
}

/// The shim body for the current platform.
fn shim_body(executable: &str, args: &[String]) -> String {
    let quoted_args = args
        .iter()
        .map(|arg| format!("\"{arg}\""))
        .collect::<Vec<_>>()
        .join(" ");
    if cfg!(windows) {
        format!("@echo off\r\n\"{executable}\" {quoted_args} %*\r\n")
    } else {
        format!("#!/bin/sh\nexec \"{executable}\" {quoted_args} \"$@\"\n")
    }
}

/// The flags a shim passes, so one executable can stand in for all three names.
fn shim_args(command: &str, version: &Version) -> Vec<String> {
    let mut args = vec![format!("--node-version={version}")];
    if command != "codets" {
        args.push(format!("--codets-as={command}"));
    }
    args
}

/// Every shim name a version directory publishes.
const SHIM_COMMANDS: [&str; 3] = ["codets", "node", "npm"];

fn write_shim(path: &Path, body: &str) -> Result<(), String> {
    fs::write(path, body).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// The shim file names for the current platform.
fn shim_names(command: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{command}.cmd")]
    } else {
        vec![command.to_string()]
    }
}

/// Shims inside a version directory, which pin that version.
fn write_version_shims(installed: &InstalledVersion) -> Result<(), String> {
    let executable = self_executable();
    for command in SHIM_COMMANDS {
        let body = shim_body(&executable, &shim_args(command, &installed.version));
        for name in shim_names(command) {
            write_shim(&installed.path.join(name), &body)?;
        }
    }
    Ok(())
}

/// The shims on `PATH`, which follow whatever `current` points at.
fn write_bin_shims(store: &VersionStore, installed: &InstalledVersion) -> Result<(), String> {
    let executable = self_executable();
    for command in SHIM_COMMANDS {
        let body = shim_body(&executable, &shim_args(command, &installed.version));
        for name in shim_names(command) {
            write_shim(&store.bin_dir().join(name), &body)?;
        }
    }
    Ok(())
}

/// Rewrite the `bin/` shims so they follow `version`.
pub fn refresh_bin_shims(store: &VersionStore, version: &Version) -> Result<(), String> {
    store.create_layout()?;
    let installed = InstalledVersion {
        version: *version,
        node: NodeCompat::for_major(version.major),
        path: store.versions_dir().join(version.to_string()),
        source: "materialised".to_string(),
    };
    write_bin_shims(store, &installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a fresh temporary directory, removed by the caller.
    fn scratch(name: &str) -> VersionStore {
        let root = std::env::temp_dir().join(format!("codets-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        VersionStore::at(root)
    }

    #[test]
    fn install_materialises_a_toolchain() {
        let store = scratch("install");
        let installed = store.install(Version::new(22, 12, 0)).expect("install");
        assert_eq!(installed.version, Version::new(22, 12, 0));
        assert_eq!(installed.node.major(), 22);
        assert!(installed.path.join("toolchain.conf").is_file());

        let listed = store.installed();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, Version::new(22, 12, 0));
        assert!(store.is_installed(&Version::new(22, 12, 0)));
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn install_writes_shims_named_for_the_platform() {
        let store = scratch("shims");
        store.install(Version::new(24, 0, 0)).expect("install");
        let names = shim_names("node");
        for name in &names {
            assert!(
                store.versions_dir().join("24.0.0").join(name).is_file(),
                "missing version shim {name}"
            );
            assert!(
                store.bin_dir().join(name).is_file(),
                "missing bin shim {name}"
            );
        }
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn current_and_default_are_separate() {
        let store = scratch("pointers");
        store.install(Version::new(22, 12, 0)).expect("install");
        store.install(Version::new(24, 0, 0)).expect("install");

        assert_eq!(store.current(), None);
        store.set_default(&Version::new(22, 12, 0)).expect("default");
        assert_eq!(store.default_version(), Some(Version::new(22, 12, 0)));

        store.set_current(&Version::new(24, 0, 0)).expect("current");
        assert_eq!(store.current(), Some(Version::new(24, 0, 0)));
        // A current pointer beats the default.
        let resolved = store.resolve(&store.root().to_path_buf());
        assert_eq!(resolved.source, VersionSource::CurrentPointer);
        assert_eq!(resolved.node.major(), 24);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_project_pin_beats_the_current_pointer() {
        let store = scratch("project");
        store.install(Version::new(24, 0, 0)).expect("install");
        store.set_current(&Version::new(24, 0, 0)).expect("current");

        let project = store.root().join("project").join("nested");
        fs::create_dir_all(&project).expect("project dir");
        write_project_pin(&store.root().join("project"), &Version::new(22, 12, 0)).expect("pin");

        let resolved = store.resolve(&project);
        assert_eq!(resolved.source, VersionSource::ProjectFile);
        assert_eq!(resolved.node.major(), 22);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn the_active_version_cannot_be_removed() {
        let store = scratch("remove");
        store.install(Version::new(24, 0, 0)).expect("install");
        store.set_current(&Version::new(24, 0, 0)).expect("current");
        assert!(store.remove(&Version::new(24, 0, 0)).is_err());

        store.install(Version::new(22, 12, 0)).expect("install");
        store.remove(&Version::new(22, 12, 0)).expect("remove idle");
        assert!(!store.is_installed(&Version::new(22, 12, 0)));
        let _ = fs::remove_dir_all(store.root());
    }
}
