//! Where the registry comes from when no flag says otherwise.
//!
//! npm resolves its registry from a flag, an environment variable, a `.npmrc` file
//! and a built-in default, and so does this. The precedence is npm's, highest
//! first:
//!
//! 1. `--registry <url|dir>`
//! 2. `NPM_CONFIG_REGISTRY`, then `npm_config_registry` (the spelling npm's own
//!    child processes see)
//! 3. the project's `.npmrc` — the nearest one from the project directory up to the
//!    project root, which is the closest directory holding a `package.json` or a
//!    `node_modules`
//! 4. the user's `.npmrc` — `NPM_CONFIG_USERCONFIG`, `CODETS_NPMRC`, or `~/.npmrc`
//! 5. `https://registry.npmjs.org`
//!
//! The file format is npm's, restricted to the one key that matters here:
//! `registry=`. Comments, blank lines and `${VAR}` expansion work as they do
//! there. Scoped registries (`@scope:registry=`) are read as ordinary keys and
//! ignored, so a project that needs one has to say so on the command line.
//!
//! Mirrors and internal registries are the whole point of this file: pointing at
//! `https://registry.npmmirror.com` or at a Nexus is a one-line `.npmrc`.

use std::path::{Path, PathBuf};

use super::registry::Registry;

/// The config file name, in every directory it is looked for.
pub const CONFIG_FILE: &str = ".npmrc";

/// The environment lookups the chain needs.
///
/// Passing this in rather than reading the process environment is what makes the
/// precedence testable: a test supplies its own answers instead of mutating a
/// global that other tests share.
pub trait Variables {
    fn get(&self, name: &str) -> Option<String>;
}

/// The real environment.
pub struct ProcessEnvironment;

impl Variables for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Where a registry setting came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Flag,
    Environment,
    ProjectFile(PathBuf),
    UserFile(PathBuf),
    Default,
}

impl Origin {
    pub fn describe(&self) -> String {
        match self {
            Origin::Flag => "--registry".to_string(),
            Origin::Environment => "NPM_CONFIG_REGISTRY".to_string(),
            Origin::ProjectFile(path) => path.display().to_string(),
            Origin::UserFile(path) => path.display().to_string(),
            Origin::Default => "the built-in default".to_string(),
        }
    }
}

/// The registry that applies, and why.
pub struct Choice {
    pub registry: Registry,
    /// The setting as written, which is what `npm config get registry` prints.
    pub spec: String,
    pub origin: Origin,
}

impl Choice {
    pub fn describe(&self) -> String {
        format!("{} (from {})", self.spec, self.origin.describe())
    }
}

/// Resolve the registry for `project` from the real environment.
pub fn resolve(explicit: Option<&str>, project: &Path) -> Result<Choice, String> {
    resolve_in(explicit, project, &ProcessEnvironment)
}

/// Resolve the registry for `project`, with the environment supplied by the caller.
pub fn resolve_in(
    explicit: Option<&str>,
    project: &Path,
    vars: &dyn Variables,
) -> Result<Choice, String> {
    if let Some(spec) = explicit {
        let spec = spec.trim();
        if !spec.is_empty() {
            return Ok(Choice {
                registry: Registry::open(spec)?,
                spec: spec.to_string(),
                origin: Origin::Flag,
            });
        }
    }

    for name in ["NPM_CONFIG_REGISTRY", "npm_config_registry"] {
        if let Some(spec) = non_empty(vars.get(name)) {
            return Ok(Choice {
                registry: Registry::open(&spec)?,
                spec,
                origin: Origin::Environment,
            });
        }
    }

    let user = user_config(vars);
    if let Some(path) = nearest_config(project) {
        if let Some(spec) = read_registry(&path) {
            let origin = match &user {
                Some(user_path) if same_file(user_path, &path) => Origin::UserFile(path),
                _ => Origin::ProjectFile(path),
            };
            return Ok(Choice {
                registry: Registry::open(&spec)?,
                spec,
                origin,
            });
        }
    }
    if let Some(path) = user {
        if let Some(spec) = read_registry(&path) {
            return Ok(Choice {
                registry: Registry::open(&spec)?,
                spec,
                origin: Origin::UserFile(path),
            });
        }
    }

    let spec = Registry::default_registry().describe();
    Ok(Choice {
        registry: Registry::default_registry(),
        spec,
        origin: Origin::Default,
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// The project root: the nearest directory at or above `from` that holds a
/// `package.json` or a `node_modules`, which is where npm draws the line too.
///
/// Walking to the filesystem root instead would let a `.npmrc` in some unrelated
/// ancestor directory decide where a project installs from. On a machine with a
/// `~/.npmrc` that is exactly what happens, and the project's own settings never
/// get a chance.
pub fn project_root(from: &Path) -> PathBuf {
    let mut directory = from.to_path_buf();
    loop {
        if directory.join("package.json").is_file() || directory.join("node_modules").is_dir() {
            return directory;
        }
        if !directory.pop() {
            return from.to_path_buf();
        }
    }
}

/// The `.npmrc` that applies to `from`: the nearest one, no higher than the project
/// root.
pub fn nearest_config(from: &Path) -> Option<PathBuf> {
    let stop = project_root(from);
    let mut directory = from.to_path_buf();
    loop {
        let candidate = directory.join(CONFIG_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        if directory == stop || !directory.pop() {
            return None;
        }
    }
}

/// The user-level config file, if there is one.
fn user_config(vars: &dyn Variables) -> Option<PathBuf> {
    for name in ["NPM_CONFIG_USERCONFIG", "CODETS_NPMRC"] {
        if let Some(path) = non_empty(vars.get(name)) {
            return Some(PathBuf::from(path));
        }
    }
    let home = non_empty(vars.get("USERPROFILE")).or_else(|| non_empty(vars.get("HOME")))?;
    let path = PathBuf::from(home).join(CONFIG_FILE);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// One path compared to another, tolerating a symlinked home directory.
fn same_file(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// The `registry` value in a `.npmrc`, or `None` when the file is missing or says
/// nothing about it. A missing file is not an error: the chain has a next step.
fn read_registry(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    parse_registry(&contents, &ProcessEnvironment)
}

/// The `registry` value in one `.npmrc` document.
pub fn parse_registry(contents: &str, vars: &dyn Variables) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "registry" {
            continue;
        }
        let value = expand(value.trim(), vars);
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Expand `${VAR}`, which npm allows in a config value. An unknown variable
/// expands to nothing, and an unterminated `${` is literal text.
pub fn expand(value: &str, vars: &dyn Variables) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                out.push_str(&vars.get(&after[..end]).unwrap_or_default());
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers exactly what a test tells it to, so the chain can be driven
    /// without touching the process environment. Values are owned because a test
    /// usually points at a path it just created.
    struct FixedEnv(Vec<(String, String)>);

    impl FixedEnv {
        fn new(pairs: &[(&str, &str)]) -> FixedEnv {
            FixedEnv(
                pairs
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            )
        }
    }

    impl Variables for FixedEnv {
        fn get(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// A scratch directory that removes itself, so even a failing test leaves
    /// nothing behind in the temp directory.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path = std::env::temp_dir().join(format!(
                "codets-test-npmrc-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|value| value.subsec_nanos())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch");
            Scratch(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// Write a file below the scratch directory, creating parents.
        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(&path, body).expect("write");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn empty() -> FixedEnv {
        FixedEnv::new(&[])
    }

    #[test]
    fn reads_the_registry_key() {
        let vars = empty();
        assert_eq!(
            parse_registry("registry=https://example.com/", &vars).as_deref(),
            Some("https://example.com/")
        );
        // Spaces around the `=` are allowed, as in npm.
        assert_eq!(
            parse_registry("registry = https://example.com", &vars).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn ignores_comments_blank_lines_and_other_keys() {
        let vars = empty();
        let document = "\
# a comment
; another comment

strict-ssl=false
   registry=https://example.com/
registry-mirror=https://ignored.example.com/
";
        assert_eq!(
            parse_registry(document, &vars).as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(parse_registry("# registry=https://x\n", &vars), None);
        assert_eq!(parse_registry("", &vars), None);
        assert_eq!(parse_registry("no-equals-sign\n", &vars), None);
    }

    #[test]
    fn the_first_registry_key_wins() {
        let vars = empty();
        let document = "registry=https://first.example.com/\nregistry=https://second.example.com/\n";
        assert_eq!(
            parse_registry(document, &vars).as_deref(),
            Some("https://first.example.com/")
        );
    }

    #[test]
    fn expands_variables_and_tolerates_the_missing_ones() {
        let vars = FixedEnv::new(&[("MIRROR", "https://mirror.example.com")]);
        assert_eq!(
            expand("${MIRROR}/npm/", &vars),
            "https://mirror.example.com/npm/"
        );
        assert_eq!(expand("prefix-${NOT_SET}-suffix", &vars), "prefix--suffix");
        // An unterminated `${` is text, not a broken value.
        assert_eq!(expand("${MIRROR", &vars), "${MIRROR");
        assert_eq!(expand("plain", &vars), "plain");

        // A registry that expands to nothing is treated as unset, so the chain
        // keeps looking rather than installing from an empty URL.
        assert_eq!(parse_registry("registry=${NOT_SET}\n", &vars), None);
        assert_eq!(
            parse_registry("registry=${MIRROR}\n", &vars).as_deref(),
            Some("https://mirror.example.com")
        );
    }

    #[test]
    fn the_flag_beats_everything() {
        let scratch = Scratch::new("flag");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        let user = scratch.write(".npmrc", "registry=https://user.example.com/\n");
        scratch.write("project/.npmrc", "registry=https://project.example.com/\n");
        let vars = FixedEnv::new(&[
            ("NPM_CONFIG_REGISTRY", "https://env.example.com/"),
            ("NPM_CONFIG_USERCONFIG", user.to_str().expect("utf-8")),
        ]);

        let choice = resolve_in(Some("https://flag.example.com/"), &project, &vars).expect("resolve");
        assert_eq!(choice.spec, "https://flag.example.com/");
        assert_eq!(choice.origin, Origin::Flag);
    }

    #[test]
    fn the_environment_beats_the_files() {
        let scratch = Scratch::new("env");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        scratch.write("project/.npmrc", "registry=https://project.example.com/\n");
        let vars = FixedEnv::new(&[("NPM_CONFIG_REGISTRY", "https://env.example.com/")]);

        let choice = resolve_in(None, &project, &vars).expect("resolve");
        assert_eq!(choice.spec, "https://env.example.com/");
        assert_eq!(choice.origin, Origin::Environment);
    }

    #[test]
    fn the_project_file_beats_the_user_file() {
        let scratch = Scratch::new("project");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        let user = scratch.write(".npmrc", "registry=https://user.example.com/\n");
        let project_file = scratch.write("project/.npmrc", "registry=https://project.example.com/\n");
        let vars = FixedEnv::new(&[("NPM_CONFIG_USERCONFIG", user.to_str().expect("utf-8"))]);

        let choice = resolve_in(None, &project, &vars).expect("resolve");
        assert_eq!(choice.spec, "https://project.example.com/");
        assert_eq!(choice.origin, Origin::ProjectFile(project_file));
    }

    #[test]
    fn a_project_file_is_found_by_walking_up() {
        let scratch = Scratch::new("walk");
        let nested = scratch.path().join("a/b/c");
        std::fs::create_dir_all(&nested).expect("nested");
        // A monorepo: the package.json marks the root, and the config sits at it.
        scratch.write("a/package.json", "{ \"name\": \"root\" }\n");
        let file = scratch.write("a/.npmrc", "registry=https://walked.example.com/\n");

        let choice = resolve_in(None, &nested, &FixedEnv::new(&[])).expect("resolve");
        assert_eq!(choice.spec, "https://walked.example.com/");
        assert_eq!(choice.origin, Origin::ProjectFile(file));
    }

    #[test]
    fn a_config_file_above_the_project_root_is_not_used() {
        // A machine may well have `~/.npmrc`, and the temp directory lives under
        // it. Nothing above the project root may change where a project installs
        // from, or the project's own settings never get a chance.
        let scratch = Scratch::new("boundary");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        scratch.write("project/package.json", "{ \"name\": \"consumer\" }\n");
        scratch.write(".npmrc", "registry=https://above.example.com/\n");

        let choice = resolve_in(None, &project, &FixedEnv::new(&[])).expect("resolve");
        assert_eq!(choice.spec, "https://registry.npmjs.org");
        assert_eq!(choice.origin, Origin::Default);
    }

    #[test]
    fn the_user_file_is_used_when_the_project_has_none() {
        let scratch = Scratch::new("user");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        let user = scratch.write("home/.npmrc", "registry=https://user.example.com/\n");
        let vars = FixedEnv::new(&[("NPM_CONFIG_USERCONFIG", user.to_str().expect("utf-8"))]);

        let choice = resolve_in(None, &project, &vars).expect("resolve");
        assert_eq!(choice.spec, "https://user.example.com/");
        assert_eq!(choice.origin, Origin::UserFile(user));
    }

    #[test]
    fn a_user_file_inside_the_walk_reports_itself_as_the_user_file() {
        // The walk can reach `~/.npmrc` when the project lives under the home
        // directory; reporting it as a project file would be misleading.
        let scratch = Scratch::new("home-walk");
        let project = scratch.path().join("home/project");
        std::fs::create_dir_all(&project).expect("project");
        let user = scratch.write("home/.npmrc", "registry=https://user.example.com/\n");
        let vars = FixedEnv::new(&[("NPM_CONFIG_USERCONFIG", user.to_str().expect("utf-8"))]);

        let choice = resolve_in(None, &project, &vars).expect("resolve");
        assert_eq!(choice.origin, Origin::UserFile(user));
    }

    #[test]
    fn a_file_without_the_key_falls_through_to_the_default() {
        let scratch = Scratch::new("nokey");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        scratch.write("project/.npmrc", "strict-ssl=false\n");

        let choice = resolve_in(None, &project, &FixedEnv::new(&[])).expect("resolve");
        assert_eq!(choice.spec, "https://registry.npmjs.org");
        assert_eq!(choice.origin, Origin::Default);
    }

    #[test]
    fn a_directory_registry_is_still_usable_through_the_chain() {
        // The offline installer tests point the chain at a directory, so the
        // flag path has to keep accepting a path.
        let scratch = Scratch::new("directory");
        let registry = scratch.path().join("registry");
        std::fs::create_dir_all(&registry).expect("registry");
        let spec = registry.to_string_lossy().to_string();

        let choice = resolve_in(Some(&spec), scratch.path(), &FixedEnv::new(&[])).expect("resolve");
        assert_eq!(choice.registry.describe(), format!("directory {}", registry.display()));
        assert_eq!(choice.origin, Origin::Flag);
    }

    #[test]
    fn an_unusable_setting_is_an_error_rather_than_a_silent_default() {
        let scratch = Scratch::new("bad");
        let vars = FixedEnv::new(&[("NPM_CONFIG_REGISTRY", "not-a-url-or-directory")]);
        let error = match resolve_in(None, scratch.path(), &vars) {
            Ok(choice) => panic!("a broken setting resolved to {}", choice.spec),
            Err(error) => error,
        };
        assert!(error.contains("neither an http(s) URL nor an existing directory"), "{error}");
    }

    #[test]
    fn nearest_config_reports_the_closest_file() {
        let scratch = Scratch::new("nearest");
        let nested = scratch.path().join("a/b");
        std::fs::create_dir_all(&nested).expect("nested");
        assert_eq!(nearest_config(&nested), None);
        scratch.write("a/package.json", "{ \"name\": \"root\" }\n");
        let file = scratch.write("a/.npmrc", "registry=https://example.com/\n");
        assert_eq!(nearest_config(&nested), Some(file));
    }
}
