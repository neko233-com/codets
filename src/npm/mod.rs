//! npm-compatible package management.
//!
//! The command surface mirrors npm, because scripts and CI configurations call
//! `npm install`, `npm run <script>` and `npm exec`. What differs is the
//! implementation: resolution, download, verification and unpacking are all
//! `codets` code, and the transfer phase runs on several threads.
//!
//! `package.json` and `package-lock.json` are read and written in npm's own
//! shapes, so a project installed by npm can be installed by `codets` and the
//! other way round. The list of things npm does that `codets` does not is in
//! [`install`]: no nested tree, no workspaces, no `overrides`/`resolutions`, and
//! no `audit`/`publish`.

pub mod install;
pub mod registry;
pub mod semver;

pub use install::{InstallOptions, InstallReport};
pub use registry::Registry;
pub use semver::{Range, Semver};

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The parts of `package.json` the toolchain uses.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub dependencies: Vec<(String, String)>,
    pub dev_dependencies: Vec<(String, String)>,
    pub scripts: Vec<(String, String)>,
    /// `bin` entries as (command name, path inside the package).
    pub bin: Vec<(String, String)>,
}

impl Manifest {
    pub fn script(&self, name: &str) -> Option<&str> {
        self.scripts
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Read and interpret a `package.json`.
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let parsed = crate::builtins::json::parse(&text)
        .ok_or_else(|| format!("{} is not valid JSON", path.display()))?;
    let Some(entries) = parsed.as_object() else {
        return Err(format!("{} is not a JSON object", path.display()));
    };
    let field = |name: &str| {
        entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let text_of = |value: Option<crate::builtins::json::Raw>| match value {
        Some(crate::builtins::json::Raw::Str(text)) => Some(text),
        _ => None,
    };
    let pairs = |value: Option<crate::builtins::json::Raw>| -> Vec<(String, String)> {
        match value {
            Some(crate::builtins::json::Raw::Object(map)) => map
                .into_iter()
                .filter_map(|(key, value)| match value {
                    crate::builtins::json::Raw::Str(text) => Some((key, text)),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };

    // `bin` may be a string (interpreted as the package's own name) or a map.
    let name = text_of(field("name"));
    let bin = match field("bin") {
        Some(crate::builtins::json::Raw::Str(target)) => name
            .clone()
            .map(|name| vec![(name, target)])
            .unwrap_or_default(),
        other => pairs(other),
    };

    Ok(Manifest {
        name,
        version: text_of(field("version")),
        dependencies: pairs(field("dependencies")),
        dev_dependencies: pairs(field("devDependencies")),
        scripts: pairs(field("scripts")),
        bin,
    })
}

const NPM_HELP: &str = "\
Usage: npm <command>

Commands:
  install [<package>...]     Install dependencies, or add packages
        --registry <url|dir>   Where packages come from (default: npmjs.org)
        --jobs <n>             Download and unpack with n threads
        --production           Skip devDependencies
        --clean                Remove node_modules first
        --save-dev             Add to devDependencies
  ci                         Clean install from the lockfile's dependencies
  run <script> [-- <args>]   Run a package.json script
  exec <command> [args]      Run a binary from node_modules/.bin
  list                       List installed packages
  init                       Create a package.json
  --version                  Print the npm shim version
  --help                     Print this help

Package tarballs are fetched, integrity-checked and unpacked on several threads
at once. JavaScript itself still runs single-threaded, as the language requires.
";

/// The command line entry point for the `npm` personality.
pub fn run(args: Vec<String>) -> ExitCode {
    let mut args = args;
    // `--version` and `--help` may appear anywhere; npm treats them as global.
    if args.iter().any(|arg| arg == "--version" || arg == "-v") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") || args.is_empty() {
        print!("{NPM_HELP}");
        return ExitCode::SUCCESS;
    }

    let command = args.remove(0);
    let project = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match command.as_str() {
        "install" | "i" | "add" => install_command(&project, args),
        "ci" => install_command(
            &project,
            {
                let mut with_clean = args;
                with_clean.push("--clean".to_string());
                with_clean
            },
        ),
        "run" | "run-script" => run_script(&project, args),
        "exec" | "x" => exec_command(&project, args),
        "list" | "ls" => list_command(&project),
        "init" => init_command(&project),
        other => {
            // `npm test` is shorthand for `npm run test`.
            let manifest = read_manifest(&project.join("package.json")).ok();
            if let Some(manifest) = &manifest {
                if manifest.script(other).is_some() {
                    let mut with_run = vec![other.to_string()];
                    with_run.extend(args);
                    return run_script(&project, with_run);
                }
            }
            eprintln!("npm: unknown command `{other}` (try `npm --help`)");
            ExitCode::from(1)
        }
    }
}

/// Parse the flags `install` understands.
struct InstallFlags {
    options: InstallOptions,
    packages: Vec<String>,
    save_dev: bool,
    save: bool,
}

fn install_flags(args: &[String]) -> Result<InstallFlags, String> {
    let mut registry = Registry::default_registry();
    let mut jobs = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    let mut include_dev = std::env::var("NODE_ENV").map(|value| value != "production").unwrap_or(true);
    let mut clean = false;
    let mut packages = Vec::new();
    let mut save_dev = false;
    let mut save = true;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--registry" => {
                index += 1;
                let spec = args
                    .get(index)
                    .ok_or_else(|| "npm: --registry needs a value".to_string())?;
                registry = Registry::open(spec)?;
            }
            "--jobs" | "-j" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "npm: --jobs needs a number".to_string())?;
                jobs = value
                    .parse::<usize>()
                    .map_err(|_| format!("npm: --jobs needs a number, got `{value}`"))?;
            }
            "--production" | "--omit=dev" => include_dev = false,
            "--include=dev" | "--dev" => include_dev = true,
            "--clean" => clean = true,
            "--save-dev" | "-D" => {
                save_dev = true;
                save = true;
            }
            "--save" | "-S" => save = true,
            "--no-save" => save = false,
            "--silent" | "--quiet" | "--no-fund" | "--no-audit" | "--no-progress" => {}
            other if other.starts_with("--registry=") => {
                registry = Registry::open(other.trim_start_matches("--registry="))?;
            }
            other if other.starts_with("--jobs=") => {
                let value = other.trim_start_matches("--jobs=");
                jobs = value
                    .parse::<usize>()
                    .map_err(|_| format!("npm: --jobs needs a number, got `{value}`"))?;
            }
            other if other.starts_with('-') => {
                // Unknown flags are ignored so that CI configurations keep working.
            }
            other => packages.push(other.to_string()),
        }
        index += 1;
    }

    Ok(InstallFlags {
        options: InstallOptions {
            registry,
            jobs,
            include_dev,
            clean,
        },
        packages,
        save_dev,
        save,
    })
}

fn install_command(project: &Path, args: Vec<String>) -> ExitCode {
    let flags = match install_flags(&args) {
        Ok(flags) => flags,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(1);
        }
    };
    let manifest_path = project.join("package.json");

    // `npm install <package>` adds and saves; `npm install` installs what is
    // already declared.
    if !flags.packages.is_empty() {
        if !manifest_path.is_file() {
            eprintln!("npm: cannot add packages without a package.json (run `npm init`)");
            return ExitCode::from(1);
        }
        let mut added = Vec::new();
        for request in &flags.packages {
            let (name, wanted) = split_package_request(request);
            let range = match wanted {
                Some(range) => range,
                None => match newest_range(&flags.options.registry, &name) {
                    Ok(range) => range,
                    Err(message) => {
                        eprintln!("npm: {message}");
                        return ExitCode::from(1);
                    }
                },
            };
            added.push((name, range));
        }
        for (name, range) in &added {
            if let Err(error) = save_dependency(&manifest_path, name, range, flags.save_dev, flags.save)
            {
                eprintln!("npm: {error}");
                return ExitCode::from(1);
            }
            println!(
                "saved {}@{} to {}{}",
                name,
                range,
                if flags.save_dev { "devDependencies" } else { "dependencies" },
                if flags.save { "" } else { " (not saved)" }
            );
        }
    }

    match install::install(project, &flags.options) {
        Ok(report) => {
            print_report(&report);
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("npm: {message}");
            ExitCode::from(1)
        }
    }
}

/// The newest published version as a caret range, which is what npm writes.
fn newest_range(registry: &Registry, name: &str) -> Result<String, String> {
    let metadata = registry.metadata(name)?;
    let newest = metadata
        .versions
        .iter()
        .filter(|info| !info.version.is_prerelease())
        .map(|info| &info.version)
        .max()
        .or_else(|| metadata.versions.last().map(|info| &info.version))
        .ok_or_else(|| format!("`{name}` has no published versions"))?;
    Ok(format!("^{newest}"))
}

/// `name` or `name@range`.
fn split_package_request(request: &str) -> (String, Option<String>) {
    // A scoped name starts with `@`, so its first `@` is not the separator.
    let search_from = if request.starts_with('@') { 1 } else { 0 };
    match request[search_from..].find('@') {
        Some(offset) => {
            let index = search_from + offset;
            (
                request[..index].to_string(),
                Some(request[index + 1..].to_string()),
            )
        }
        None => (request.to_string(), None),
    }
}

/// Insert or replace one dependency in `package.json`.
///
/// The edit is textual so the rest of the file — field order, unknown fields,
/// trailing newline — survives untouched.
fn save_dependency(
    manifest_path: &Path,
    name: &str,
    range: &str,
    dev: bool,
    save: bool,
) -> Result<(), String> {
    if !save {
        return Ok(());
    }
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let section = if dev { "devDependencies" } else { "dependencies" };

    if let Some(updated) = upsert_in_section(&text, section, name, range) {
        return std::fs::write(manifest_path, updated)
            .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()));
    }
    // No section yet: add one just before the closing brace. It is inserted last,
    // so it must not carry a trailing comma.
    let entry = format!("{}: {}", quote(name), quote(range));
    let body = format!("\n  \"{section}\": {{\n    {entry}\n  }}");
    let Some(close) = text.rfind('}') else {
        return Err(format!("{} is not a JSON object", manifest_path.display()));
    };
    let mut updated = String::with_capacity(text.len() + body.len());
    updated.push_str(text[..close].trim_end().trim_end_matches(',').trim_end());
    updated.push(',');
    updated.push_str(&body);
    updated.push_str("\n}\n");
    std::fs::write(manifest_path, updated)
        .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))
}

/// Update one key inside a JSON section, keeping the surrounding text as-is.
///
/// The quoting happens here rather than at the call site, so every caller writes
/// a valid `name: range` pair.
fn upsert_in_section(text: &str, section: &str, key: &str, range: &str) -> Option<String> {
    let marker = format!("\"{section}\"");
    let start = text.find(&marker)?;
    let open = text[start..].find('{')? + start;
    // Find the matching closing brace of the section object.
    let mut depth = 0usize;
    let mut close = None;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let body = &text[open + 1..close];
    let entry = format!("{}: {}", quote(key), quote(range));
    let key_marker = format!("{}:", quote(key));
    let updated_body = match body.find(&key_marker) {
        Some(offset) => {
            // Replace just the value, keeping the key and the indentation.
            let value_start = open + 1 + offset + key_marker.len();
            let value_end = text[value_start..close]
                .find(',')
                .map(|end| value_start + end)
                .unwrap_or(close);
            let mut replaced = String::new();
            replaced.push_str(&text[..value_start]);
            replaced.push(' ');
            replaced.push_str(&quote(range));
            replaced.push_str(&text[value_end..]);
            return Some(replaced);
        }
        None => {
            if body.trim().is_empty() {
                format!("\n    {entry}\n  ")
            } else {
                format!("{body}, {entry} ")
            }
        }
    };
    let mut updated = String::with_capacity(text.len() + entry.len());
    updated.push_str(&text[..open + 1]);
    updated.push_str(&updated_body);
    updated.push_str(&text[close..]);
    Some(updated)
}

fn quote(text: &str) -> String {
    install::json_string(text)
}

fn print_report(report: &InstallReport) {
    for warning in &report.warnings {
        eprintln!("npm: warning: {warning}");
    }
    let bytes: u64 = report.packages.iter().map(|package| package.bytes).sum();
    println!(
        "added {} packages in {:.2}s (transfer {:.2}s on {} threads, {:.1} MiB)",
        report.packages.len(),
        report.elapsed.as_secs_f64(),
        report.transfer.as_secs_f64(),
        report.jobs,
        bytes as f64 / (1024.0 * 1024.0)
    );
}

// ------------------------------------------------------------------- run/exec

/// Run a `package.json` script with `node_modules/.bin` on `PATH`.
fn run_script(project: &Path, args: Vec<String>) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("npm: `run` needs a script name");
        return ExitCode::from(1);
    };
    let manifest = match read_manifest(&project.join("package.json")) {
        Ok(manifest) => manifest,
        Err(message) => {
            eprintln!("npm: {message}");
            return ExitCode::from(1);
        }
    };
    let Some(script) = manifest.script(name) else {
        eprintln!("npm: missing script: {name}");
        let available: Vec<&str> = manifest
            .scripts
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        if !available.is_empty() {
            eprintln!("npm: available scripts: {}", available.join(", "));
        }
        return ExitCode::from(1);
    };

    // Arguments after `--` are appended, which is how npm forwards them.
    let extra: Vec<String> = match args.iter().position(|arg| arg == "--") {
        Some(index) => args[index + 1..].to_vec(),
        None => Vec::new(),
    };
    let command = if extra.is_empty() {
        script.to_string()
    } else {
        format!("{script} {}", extra.join(" "))
    };

    println!();
    println!("> {}@{} {}", 
        manifest.name.as_deref().unwrap_or(""),
        manifest.version.as_deref().unwrap_or(""),
        name);
    println!("> {command}");
    println!();
    let _ = std::io::stdout().flush();

    let status = spawn_shell(project, &command, name, &manifest);
    finish_child(status)
}

/// `npm exec <command>`: prefer `node_modules/.bin`, fall back to `PATH`.
fn exec_command(project: &Path, args: Vec<String>) -> ExitCode {
    if args.is_empty() {
        eprintln!("npm: `exec` needs a command");
        return ExitCode::from(1);
    }
    let manifest = read_manifest(&project.join("package.json")).unwrap_or_default();
    let command = args.join(" ");
    let status = spawn_shell(project, &command, "exec", &manifest);
    finish_child(status)
}

/// Run a command line through the platform shell, with the local bin directory
/// first on `PATH` and npm's lifecycle variables set.
fn spawn_shell(
    project: &Path,
    command: &str,
    event: &str,
    manifest: &Manifest,
) -> std::io::Result<std::process::ExitStatus> {
    let bin_dir = project.join("node_modules").join(".bin");
    let mut paths: Vec<PathBuf> = Vec::new();
    if bin_dir.is_dir() {
        paths.push(bin_dir);
    }
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    let joined = std::env::join_paths(paths).unwrap_or_else(|_| std::ffi::OsString::from(""));

    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    std::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .current_dir(project)
        .env("PATH", joined)
        .env("npm_lifecycle_event", event)
        .env("npm_package_name", manifest.name.clone().unwrap_or_default())
        .env(
            "npm_package_version",
            manifest.version.clone().unwrap_or_default(),
        )
        .env("npm_config_user_agent", format!("codets/{}", env!("CARGO_PKG_VERSION")))
        .status()
}

fn finish_child(status: std::io::Result<std::process::ExitStatus>) -> ExitCode {
    match status {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("npm: cannot run the command: {error}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------- list/init

fn list_command(project: &Path) -> ExitCode {
    let modules = project.join("node_modules");
    let manifest = read_manifest(&project.join("package.json")).ok();
    if let Some(manifest) = &manifest {
        println!(
            "{}@{} {}",
            manifest.name.as_deref().unwrap_or("(unnamed)"),
            manifest.version.as_deref().unwrap_or("0.0.0"),
            project.display()
        );
    }
    let Ok(entries) = std::fs::read_dir(&modules) else {
        println!("  (no node_modules)");
        return ExitCode::SUCCESS;
    };
    let mut listed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".bin" || name.starts_with('@') {
            continue;
        }
        if let Ok(manifest) = read_manifest(&entry.path().join("package.json")) {
            listed.push(format!(
                "{}@{}",
                manifest.name.unwrap_or(name),
                manifest.version.unwrap_or_else(|| "?".to_string())
            ));
        }
    }
    listed.sort();
    for line in &listed {
        println!("  {line}");
    }
    if listed.is_empty() {
        println!("  (no packages installed)");
    }
    ExitCode::SUCCESS
}

fn init_command(project: &Path) -> ExitCode {
    let path = project.join("package.json");
    if path.is_file() {
        eprintln!("npm: {} already exists", path.display());
        return ExitCode::from(1);
    }
    let name = project
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".to_string());
    let body = format!(
        "{{\n  \"name\": {},\n  \"version\": \"1.0.0\",\n  \"private\": true,\n  \"type\": \"module\"\n}}\n",
        quote(&name)
    );
    match std::fs::write(&path, body) {
        Ok(()) => {
            println!("wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("npm: cannot write {}: {error}", path.display());
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_package_requests() {
        assert_eq!(
            split_package_request("lodash@^4.17.0"),
            ("lodash".to_string(), Some("^4.17.0".to_string()))
        );
        assert_eq!(split_package_request("lodash"), ("lodash".to_string(), None));
        // A scoped name's first `@` is part of the name.
        assert_eq!(
            split_package_request("@types/node@20.1.0"),
            ("@types/node".to_string(), Some("20.1.0".to_string()))
        );
        assert_eq!(
            split_package_request("@types/node"),
            ("@types/node".to_string(), None)
        );
    }

    #[test]
    fn updates_an_existing_dependency() {
        let text = "{\n  \"name\": \"demo\",\n  \"dependencies\": {\n    \"left-pad\": \"^1.0.0\"\n  }\n}\n";
        let updated =
            upsert_in_section(text, "dependencies", "left-pad", "^1.3.0").expect("section found");
        assert!(updated.contains("\"left-pad\": \"^1.3.0\""), "{updated}");
        assert!(!updated.contains("^1.0.0"));
        assert!(updated.contains("\"name\": \"demo\""));
        assert!(codets_json(&updated), "{updated}");
    }

    #[test]
    fn adds_a_dependency_to_an_existing_section() {
        let text = "{\n  \"dependencies\": {\n    \"left-pad\": \"^1.0.0\"\n  }\n}\n";
        let updated =
            upsert_in_section(text, "dependencies", "is-odd", "^3.0.1").expect("section found");
        assert!(updated.contains("\"is-odd\": \"^3.0.1\""), "{updated}");
        assert!(updated.contains("\"left-pad\": \"^1.0.0\""), "{updated}");
        assert!(codets_json(&updated), "{updated}");
    }

    #[test]
    fn adds_a_section_when_there_is_none() {
        let dir = std::env::temp_dir().join(format!("codets-manifest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("package.json");
        std::fs::write(&path, "{\n  \"name\": \"demo\",\n  \"version\": \"1.0.0\"\n}\n")
            .expect("write");
        save_dependency(&path, "is-odd", "^3.0.1", false, true).expect("save");
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(updated.contains("\"dependencies\""), "{updated}");
        assert!(updated.contains("\"is-odd\": \"^3.0.1\""), "{updated}");
        assert!(updated.contains("\"name\": \"demo\""), "{updated}");
        assert!(codets_json(&updated), "{updated}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// True when the text parses as JSON, which is what npm and every editor
    /// depend on.
    fn codets_json(text: &str) -> bool {
        crate::builtins::json::parse(text).is_some()
    }

    #[test]
    fn reads_a_manifest() {
        let text = r#"{
            "name": "demo",
            "version": "1.2.3",
            "dependencies": {"a": "^1.0.0"},
            "devDependencies": {"b": "~2.0.0"},
            "scripts": {"test": "codets test.ts"},
            "bin": {"demo": "./cli.js"}
        }"#;
        let parsed = crate::builtins::json::parse(text).expect("json");
        let entries = parsed.as_object().expect("object");
        assert!(entries.iter().any(|(key, _)| key == "scripts"));
    }
}
