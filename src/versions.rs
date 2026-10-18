//! The built-in toolchain version manager.
//!
//! `codets` manages its own toolchains the way `rustup` and `fnm` do, so a project
//! can pin the Node.js release it is written against and switching is one command.
//!
//! ```text
//! ~/.codets/                     (or $CODETS_HOME)
//!   bin/                         what belongs on PATH
//!     codets  node  npm          links to the active version's binary
//!   versions/
//!     22.12.0/                   one installed toolchain
//!       codets                   the real binary for this level
//!       node  npm                links to it, so `node` works from here
//!       toolchain.conf           version, Node level, where it came from
//!   current                      the active version
//!   settings.conf                `default=<version>`
//! ```
//!
//! Three decisions make this work on every platform without shell shims:
//!
//! * **A version directory contains its own binary.** Every level is
//!   self-contained, so `install`, `bootstrap` and `upgrade` are the same
//!   operation: put a binary in a directory and describe it.
//! * **`bin/` holds links, not scripts.** `bin/node` is a link to the active
//!   version's binary, and the binary picks its command personality from
//!   `argv[0]`, so `node` and `npm` need no dispatch of their own. Links avoid the
//!   `node.exe` versus `node.cmd` precedence trap that `PATHEXT` creates on
//!   Windows, and the `chmod +x` that a shell script would need on Unix.
//! * **A binary knows which level it is by where it lives.** A binary under
//!   `versions/22.12.0/` reports Node 22 even when `current` says otherwise, so
//!   `~/.codets/versions/22.12.0/node app.ts` pins a level without any arguments.
//!
//! Resolution order for the active version, first match wins:
//!
//! 1. `$CODETS_NODE_VERSION`
//! 2. the level implied by the running binary's own directory
//! 3. the nearest `.codets-version` file, walking up from the working directory
//! 4. the `current` pointer
//! 5. `default` in `settings.conf`
//! 6. the built-in default (the newest supported Node LTS line)

use std::fs;
use std::path::{Path, PathBuf};

use crate::node::{NodeCompat, Version};

/// The environment variable that forces a version, for scripts and CI.
pub const VERSION_ENV: &str = "CODETS_NODE_VERSION";

/// The per-project pin file, in the spirit of `.nvmrc`.
pub const VERSION_FILE: &str = ".codets-version";

/// The commands every version and the `bin` directory publish.
pub const COMMANDS: [&str; 3] = ["codets", "node", "npm"];

/// The file name of the toolchain binary on this platform.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "codets.exe"
    } else {
        "codets"
    }
}

/// The command file name for a command on this platform.
pub fn command_name(command: &str) -> String {
    if cfg!(windows) {
        format!("{command}.exe")
    } else {
        command.to_string()
    }
}

/// One installed toolchain.
#[derive(Clone, Debug)]
pub struct InstalledVersion {
    pub version: Version,
    pub node: NodeCompat,
    pub path: PathBuf,
    /// Where the toolchain came from, recorded for `codets versions`.
    pub source: String,
}

impl InstalledVersion {
    pub fn describe(&self) -> String {
        format!("{} — {}", self.version, self.node.describe())
    }

    /// The real binary inside this toolchain.
    pub fn binary(&self) -> PathBuf {
        self.path.join(binary_name())
    }
}

/// Why a particular version is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VersionSource {
    Environment,
    BinaryLocation,
    ProjectFile,
    CurrentPointer,
    SettingsDefault,
    BuiltIn,
}

impl VersionSource {
    pub fn describe(self) -> &'static str {
        match self {
            VersionSource::Environment => "environment (CODETS_NODE_VERSION)",
            VersionSource::BinaryLocation => "the toolchain this binary belongs to",
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
        VersionStore::at(default_root())
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

    /// The directory that belongs on `PATH`.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// The directory holding one toolchain.
    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.versions_dir().join(version.to_string())
    }

    /// The real binary for a version, whether or not it is installed.
    pub fn version_binary(&self, version: &Version) -> PathBuf {
        self.version_dir(version).join(binary_name())
    }

    fn current_file(&self) -> PathBuf {
        self.root.join("current")
    }

    fn settings_file(&self) -> PathBuf {
        self.root.join("settings.conf")
    }

    pub fn create_layout(&self) -> Result<(), String> {
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
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(version) = Version::parse(&name) else {
                continue;
            };
            // A directory without a binary is a leftover, not a toolchain.
            if !entry.path().join(binary_name()).is_file() {
                continue;
            }
            let source = read_conf(&entry.path().join("toolchain.conf"))
                .into_iter()
                .find(|(key, _)| key == "source")
                .map(|(_, value)| value)
                .unwrap_or_else(|| "materialised".to_string());
            found.push(InstalledVersion {
                version,
                node: NodeCompat::for_major(version.major),
                path: entry.path(),
                source,
            });
        }
        found.sort_by_key(|entry| entry.version);
        found
    }

    pub fn is_installed(&self, version: &Version) -> bool {
        self.version_binary(version).is_file()
    }

    /// The installed toolchain for a version, if any.
    pub fn find(&self, version: &Version) -> Option<InstalledVersion> {
        self.installed()
            .into_iter()
            .find(|entry| entry.version == *version)
    }

    /// Install a toolchain from the binary that is running.
    pub fn install(&self, version: Version) -> Result<InstalledVersion, String> {
        let from = std::env::current_exe()
            .map_err(|error| format!("cannot find the running executable: {error}"))?;
        self.install_from(version, &from)
    }

    /// Install a toolchain whose binary is `binary`.
    ///
    /// The binary is copied into the version directory, so the installed toolchain
    /// keeps working after whatever it came from is deleted — which is what lets
    /// the installers, `bootstrap` and `upgrade` share one code path.
    ///
    /// Installing deliberately does *not* change which version is active.
    pub fn install_from(&self, version: Version, binary: &Path) -> Result<InstalledVersion, String> {
        self.install_from_labeled(version, binary, &binary.display().to_string())
    }

    /// Install a toolchain whose binary is `binary`, recording `source` as where it
    /// came from.
    ///
    /// `bootstrap` uses this: it installs the binary the user just ran, and when an
    /// installer ran it that binary is an unpacked copy in a temporary directory that
    /// is deleted moments later. Recording that path would put a path in the manifest
    /// that never exists again, and would make two identical installs differ.
    pub fn install_from_labeled(
        &self,
        version: Version,
        binary: &Path,
        source: &str,
    ) -> Result<InstalledVersion, String> {
        self.install_placing(version, source, |target| place_binary(binary, target))
    }

    /// Install a toolchain from bytes that were downloaded rather than read from
    /// disk, which is what `codets upgrade` does.
    pub fn install_from_bytes(
        &self,
        version: Version,
        bytes: &[u8],
        source: &str,
    ) -> Result<InstalledVersion, String> {
        self.install_placing(version, source, |target| {
            fs::write(target, bytes)
                .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
            make_executable(target);
            Ok(())
        })
    }

    /// The shared body of the two `install_from*` methods.
    fn install_placing(
        &self,
        version: Version,
        source: &str,
        place: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<InstalledVersion, String> {
        self.create_layout()?;
        let directory = self.version_dir(&version);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;

        let target = directory.join(binary_name());
        place(&target)?;

        let compat = NodeCompat::for_major(version.major);
        let manifest = format!(
            "version={version}\nnode={}\ncodets={}\nsource={source}\n",
            compat.version_string(),
            env!("CARGO_PKG_VERSION")
        );
        fs::write(directory.join("toolchain.conf"), manifest)
            .map_err(|error| format!("cannot write the manifest: {error}"))?;

        let installed = InstalledVersion {
            version,
            node: compat,
            path: directory,
            source: source.to_string(),
        };
        // `node` and `npm` inside the version directory run the same binary, which
        // is what makes `versions/22.12.0/node app.ts` pin a level.
        for command in COMMANDS {
            if command == "codets" {
                continue;
            }
            link_or_copy(&installed.binary(), &installed.path.join(command_name(command)))?;
        }
        Ok(installed)
    }

    /// Point `bin/` at `version`. This is what makes a version *active*.
    pub fn relink_bin(&self, version: &Version) -> Result<Vec<PathBuf>, String> {
        self.create_layout()?;
        let installed = self
            .find(version)
            .ok_or_else(|| format!("{version} is not installed"))?;
        let mut linked = Vec::new();
        for command in COMMANDS {
            let path = self.bin_dir().join(command_name(command));
            link_or_copy(&installed.binary(), &path)?;
            linked.push(path);
        }
        Ok(linked)
    }

    /// Delete a toolchain. The active version cannot be removed.
    pub fn remove(&self, version: &Version) -> Result<(), String> {
        if self.current() == Some(*version) {
            return Err(format!(
                "{version} is the active version; run `codets use <other>` first"
            ));
        }
        let directory = self.version_dir(version);
        if !directory.is_dir() {
            return Err(format!("{version} is not installed"));
        }
        fs::remove_dir_all(&directory)
            .map_err(|error| format!("cannot remove {}: {error}", directory.display()))
    }

    /// The active version, or `None` when nothing is active.
    pub fn current(&self) -> Option<Version> {
        read_pointer(&self.current_file())
    }

    /// Write the `current` pointer.
    pub fn set_current(&self, version: &Version) -> Result<(), String> {
        self.create_layout()?;
        write_pointer(&self.current_file(), version)
    }

    /// Make `version` active: write the pointer and relink `bin/`.
    pub fn activate(&self, version: &Version) -> Result<Vec<PathBuf>, String> {
        self.set_current(version)?;
        self.relink_bin(version)
    }

    pub fn default_version(&self) -> Option<Version> {
        read_conf(&self.settings_file())
            .into_iter()
            .find(|(key, _)| key == "default")
            .and_then(|(_, value)| Version::parse(&value))
    }

    pub fn set_default(&self, version: &Version) -> Result<(), String> {
        self.create_layout()?;
        fs::write(self.settings_file(), format!("default={version}\n"))
            .map_err(|error| format!("cannot write {}: {error}", self.settings_file().display()))
    }

    /// The level implied by the running binary's own directory.
    ///
    /// `~/.codets/versions/22.12.0/node` must report Node 22 even when `current`
    /// points at 24, which is what makes a version directory a *pinned* toolchain.
    pub fn level_from_binary_path(&self, binary: &Path) -> Option<NodeCompat> {
        let relative = binary.strip_prefix(self.versions_dir()).ok()?;
        let first = relative.components().next()?;
        let name = first.as_os_str().to_string_lossy().to_string();
        NodeCompat::parse(&name)
    }

    /// Which version applies when running from `from`, and why.
    pub fn resolve(&self, from: &Path) -> Resolution {
        if let Ok(forced) = std::env::var(VERSION_ENV) {
            if let Some(node) = NodeCompat::request(&forced) {
                return Resolution {
                    node,
                    source: VersionSource::Environment,
                };
            }
        }
        if let Some(pinned) = find_project_pin(from) {
            if let Some(node) = NodeCompat::request(&pinned) {
                return Resolution {
                    node,
                    source: VersionSource::ProjectFile,
                };
            }
        }
        if let Some(current) = self.current() {
            return Resolution {
                node: NodeCompat::for_major(current.major),
                source: VersionSource::CurrentPointer,
            };
        }
        if let Some(default) = self.default_version() {
            return Resolution {
                node: NodeCompat::for_major(default.major),
                source: VersionSource::SettingsDefault,
            };
        }
        Resolution {
            node: NodeCompat::default(),
            source: VersionSource::BuiltIn,
        }
    }
}

/// The default store root: `~/.codets`.
pub fn default_root() -> PathBuf {
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join(".codets")
}

/// Copy `source` to `destination` if that is not already where it lives.
fn place_binary(source: &Path, destination: &Path) -> Result<(), String> {
    let same = match (fs::canonicalize(source), fs::canonicalize(destination)) {
        (Ok(left), Ok(right)) => left == right,
        _ => source == destination,
    };
    if same {
        return Ok(());
    }
    fs::copy(source, destination).map_err(|error| {
        format!(
            "cannot install the binary: copying {} to {} failed: {error}",
            source.display(),
            destination.display()
        )
    })?;
    make_executable(destination);
    Ok(())
}

/// Point `destination` at `source` with a hard link, falling back to a copy when
/// the two are on different volumes.
fn link_or_copy(source: &Path, destination: &Path) -> Result<(), String> {
    if fs::symlink_metadata(destination).is_ok() {
        fs::remove_file(destination).map_err(|error| {
            format!("cannot replace {}: {error}", destination.display())
        })?;
    }
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, destination).map_err(|error| {
                format!(
                    "cannot link {} to {}: {error}",
                    destination.display(),
                    source.display()
                )
            })?;
            make_executable(destination);
            Ok(())
        }
    }
}

/// Mark a file executable on platforms that have the concept.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Look for `.codets-version`, starting at `from` and walking up.
pub fn find_project_pin(from: &Path) -> Option<String> {
    // Starting from a file path is fine: the first probe looks for a pin beside it,
    // fails, and `pop` moves to its directory — which is where the walk was always
    // going to go.
    let mut directory = from.to_path_buf();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a fresh temporary directory, removed by the caller.
    fn scratch(name: &str) -> VersionStore {
        let root = std::env::temp_dir().join(format!(
            "codets-store-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        VersionStore::at(root)
    }

    #[test]
    fn install_places_a_binary_and_its_commands() {
        let store = scratch("install");
        let installed = store.install(Version::new(22, 12, 0)).expect("install");
        assert_eq!(installed.version, Version::new(22, 12, 0));
        assert_eq!(installed.node.major(), 22);
        assert!(installed.path.join("toolchain.conf").is_file());
        assert!(store.version_binary(&Version::new(22, 12, 0)).is_file());

        // `node` and `npm` inside the version directory run the same binary.
        for command in ["node", "npm"] {
            assert!(
                installed.path.join(command_name(command)).is_file(),
                "missing {} in the version directory",
                command_name(command)
            );
        }

        let listed = store.installed();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].version, Version::new(22, 12, 0));
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn installing_does_not_change_the_active_version() {
        let store = scratch("no-switch");
        store.install(Version::new(24, 0, 0)).expect("install 24");
        store.activate(&Version::new(24, 0, 0)).expect("activate 24");
        assert_eq!(store.current(), Some(Version::new(24, 0, 0)));

        // Installing another level used to rewrite `bin/`, which silently moved
        // `node` to the new level. It must not.
        store.install(Version::new(22, 12, 0)).expect("install 22");
        assert_eq!(store.current(), Some(Version::new(24, 0, 0)));
        let linked = fs::read_link(store.bin_dir().join(command_name("node")));
        // Hard links do not read back as links; compare content instead.
        assert!(
            fs::read(store.bin_dir().join(command_name("node"))).is_ok(),
            "bin/node is missing ({linked:?})"
        );
        let active = fs::read(store.version_binary(&Version::new(24, 0, 0))).expect("24 binary");
        let linked_bytes = fs::read(store.bin_dir().join(command_name("node"))).expect("bin node");
        assert_eq!(active, linked_bytes, "bin/node does not point at the active level");
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn activating_links_every_command() {
        let store = scratch("activate");
        store.install(Version::new(22, 12, 0)).expect("install");
        let linked = store.activate(&Version::new(22, 12, 0)).expect("activate");
        assert_eq!(linked.len(), COMMANDS.len());
        for command in COMMANDS {
            let path = store.bin_dir().join(command_name(command));
            assert!(path.is_file(), "missing {}", path.display());
        }
        // Nothing is on PATH before anything is activated or installed into bin.
        let empty = scratch("empty-bin");
        empty.create_layout().expect("layout");
        assert!(!empty.bin_dir().join(command_name("node")).exists());
        let _ = fs::remove_dir_all(store.root());
        let _ = fs::remove_dir_all(empty.root());
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
        let resolved = store.resolve(store.root());
        assert_eq!(resolved.source, VersionSource::CurrentPointer);
        assert_eq!(resolved.node.major(), 24);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_project_pin_beats_the_current_pointer() {
        let store = scratch("project");
        store.install(Version::new(24, 0, 0)).expect("install");
        store.set_current(&Version::new(24, 0, 0)).expect("current");

        let project = store.root().join("project");
        let nested = project.join("nested");
        fs::create_dir_all(&nested).expect("project dir");
        write_project_pin(&project, &Version::new(22, 12, 0)).expect("pin");

        let resolved = store.resolve(&nested);
        assert_eq!(resolved.source, VersionSource::ProjectFile);
        assert_eq!(resolved.node.major(), 22);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_binary_under_a_version_directory_knows_its_level() {
        let store = scratch("level");
        let binary = store.version_binary(&Version::new(22, 12, 0));
        let level = store.level_from_binary_path(&binary).expect("level");
        assert_eq!(level.major(), 22);
        // The directory name is the version it pins, so the exact release is kept.
        assert_eq!(level.version_string(), "22.12.0");
        // A binary somewhere else implies nothing.
        assert!(store
            .level_from_binary_path(&store.bin_dir().join(binary_name()))
            .is_none());
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

    #[test]
    fn a_directory_without_a_binary_is_not_a_toolchain() {
        let store = scratch("leftover");
        store.create_layout().expect("layout");
        fs::create_dir_all(store.version_dir(&Version::new(22, 12, 0))).expect("leftover dir");
        assert!(store.installed().is_empty());
        assert!(!store.is_installed(&Version::new(22, 12, 0)));
        let _ = fs::remove_dir_all(store.root());
    }
}
