//! The `codets` command line.
//!
//! One implementation serves three command names, which is what makes the
//! toolchain a drop-in replacement: `argv[0]` decides the personality, so the same
//! binary installed as `codets`, `node` or `npm` behaves like the tool it replaces.
//! The `codets` personality also exposes the version manager and can delegate to
//! either of the others (`codets node ...`, `codets npm ...`).
//!
//! Node compatibility notes worth knowing before reading the parser:
//!
//! * `node --version` prints exactly `v24.0.0`, because scripts parse it.
//!   `codets --version` prints the toolchain identity instead.
//! * Unknown `--long-options` are accepted and recorded in `process.execArgv`
//!   rather than rejected, so a wrapper passing a flag `codets` has not heard of
//!   still gets its script run. Unknown *short* options are an error, like Node's.
//! * Options that would change semantics `codets` cannot honour yet (`-r`,
//!   `--loader`, `--import`) are accepted with a warning on stderr instead of
//!   being silently dropped.

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::builtins::inspect;
use crate::node::{NodeCompat, Version};
use crate::versions::{self, VersionSource, VersionStore, VERSION_ENV, VERSION_FILE};
use crate::{Error, Language, Runtime};

/// What this binary is standing in for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Personality {
    /// The `codets` toolchain: run, version manager, delegation.
    Codets,
    /// Node-compatible: `node [options] [script] [args]`.
    Node,
    /// npm-compatible: package management.
    Npm,
}

/// Exit code Node uses for an unusable command line.
const BAD_OPTION: u8 = 9;

impl Personality {
    /// Pick a personality from the program name.
    pub fn detect(argv0: &str) -> Personality {
        let name = Path::new(argv0)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match name.as_str() {
            "node" | "nodejs" => Personality::Node,
            "npm" | "npx" => Personality::Npm,
            _ => Personality::Codets,
        }
    }

    /// Run with the arguments after the program name.
    pub fn run(self, args: Vec<String>) -> ExitCode {
        match self {
            Personality::Codets => run_codets(args),
            Personality::Node => run_node(args),
            Personality::Npm => crate::npm::run(args),
        }
    }
}

/// Entry point shared by all three binaries: `argv[0]` decides the personality.
pub fn main() -> ExitCode {
    let argv0 = std::env::args().next().unwrap_or_else(|| "codets".to_string());
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let personality =
        personality_override(&mut args).unwrap_or_else(|| Personality::detect(&argv0));
    personality.run(args)
}

/// Entry point for a binary that hard-codes its personality.
pub fn main_as(personality: Personality) -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let personality = personality_override(&mut args).unwrap_or(personality);
    personality.run(args)
}

/// The marker the version manager's shims pass so that one executable can stand
/// in for `node` and `npm` under a filename the operating system chose.
const PERSONALITY_FLAG: &str = "--codets-as=";

/// Pull `--codets-as=node` out of the argument list, if present.
fn personality_override(args: &mut Vec<String>) -> Option<Personality> {
    let index = args
        .iter()
        .position(|arg| arg.starts_with(PERSONALITY_FLAG))?;
    let flag = args.remove(index);
    Some(match flag.trim_start_matches(PERSONALITY_FLAG) {
        "node" => Personality::Node,
        "npm" => Personality::Npm,
        _ => Personality::Codets,
    })
}

// ---------------------------------------------------------------- node surface

/// Which Node-compatible mode was asked for.
enum NodeMode {
    Script {
        path: PathBuf,
        args: Vec<String>,
    },
    Eval {
        code: String,
        print: bool,
    },
    Check {
        path: PathBuf,
    },
    Repl,
    Help,
    Version,
}

struct NodeOptions {
    mode: NodeMode,
    /// Node level requested with `--node-version`, if any.
    compat: Option<NodeCompat>,
    /// `--input-type=module|commonjs`.
    input_type: Option<InputType>,
    /// Options to report as `process.execArgv`.
    exec_argv: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputType {
    Module,
    CommonJs,
}

fn parse_node_args(args: &[String]) -> Result<NodeOptions, String> {
    let mut compat = None;
    let mut input_type = None;
    let mut exec_argv = Vec::new();
    let mut index = 0;
    let mut eval: Option<(String, bool)> = None;
    let mut check: Option<PathBuf> = None;
    let mut script: Option<(PathBuf, Vec<String>)> = None;
    let mut help = false;
    let mut version = false;

    // A `--node-version` given before the personality word still counts, and the
    // version manager passes it in this exact form.
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--" => {
                // Everything after `--` is positional: the script, then its
                // arguments.
                index += 1;
                if script.is_none() {
                    if let Some(next) = args.get(index) {
                        script = Some((PathBuf::from(next), args[index + 1..].to_vec()));
                    }
                }
                break;
            }
            "-e" | "--eval" => {
                index += 1;
                let code = args
                    .get(index)
                    .ok_or_else(|| "node: option -e requires an argument".to_string())?;
                eval = Some((code.clone(), false));
            }
            "-p" | "--print" => {
                index += 1;
                let code = args
                    .get(index)
                    .ok_or_else(|| "node: option -p requires an argument".to_string())?;
                eval = Some((code.clone(), true));
            }
            "-c" | "--check" => {
                index += 1;
                let path = args
                    .get(index)
                    .ok_or_else(|| "node: option -c requires an argument".to_string())?;
                check = Some(PathBuf::from(path));
            }
            "-i" | "--interactive" => {
                // Accepted and ignored: with no script, the REPL is what runs
                // anyway, which is the only thing `-i` asks for.
            }
            "-v" | "--version" => version = true,
            "-h" | "--help" => help = true,
            "-r" | "--require" | "--loader" | "--import" | "--experimental-loader" => {
                index += 1;
                let value = args.get(index).cloned().unwrap_or_default();
                eprintln!(
                    "codets: warning: {arg} is accepted but not applied yet (module preloading is not implemented); continuing without it"
                );
                exec_argv.push(arg.to_string());
                if !value.is_empty() {
                    exec_argv.push(value);
                }
            }
            other if other.starts_with("--node-version") || other.starts_with("--compat") => {
                let value = split_option_value(other)
                    .map(|value| value.to_string())
                    .or_else(|| {
                        index += 1;
                        args.get(index).cloned()
                    })
                    .ok_or_else(|| format!("node: {other} requires a version"))?;
                compat = Some(NodeCompat::request(&value).ok_or_else(|| {
                    format!(
                        "node: `{value}` is not a Node version; supported majors: {:?}",
                        NodeCompat::supported_majors()
                    )
                })?);
                exec_argv.push(other.to_string());
            }
            other if other.starts_with("--input-type") => {
                let value = split_option_value(other)
                    .ok_or_else(|| "node: --input-type requires a value".to_string())?;
                input_type = Some(match value {
                    "module" => InputType::Module,
                    "commonjs" => InputType::CommonJs,
                    other => {
                        return Err(format!(
                            "node: --input-type must be `module` or `commonjs`, got `{other}`"
                        ))
                    }
                });
                exec_argv.push(other.to_string());
            }
            other if other.starts_with("--") => {
                // Benign or merely informational flags are accepted and recorded.
                exec_argv.push(other.to_string());
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("node: bad option: {other}"));
            }
            path => {
                script = Some((PathBuf::from(path), args[index + 1..].to_vec()));
                break;
            }
        }
        index += 1;
    }

    let mode = if help {
        NodeMode::Help
    } else if version {
        NodeMode::Version
    } else if let Some(path) = check {
        NodeMode::Check { path }
    } else if let Some((code, print)) = eval {
        NodeMode::Eval { code, print }
    } else if let Some((path, rest)) = script {
        NodeMode::Script { path, args: rest }
    } else {
        // No script at all: read from standard input. Node does the same whether
        // or not the input is a terminal, so this is not two cases.
        NodeMode::Repl
    };

    Ok(NodeOptions {
        mode,
        compat,
        input_type,
        exec_argv,
    })
}

/// `--flag=value` → `Some("value")`.
fn split_option_value(option: &str) -> Option<&str> {
    option.split_once('=').map(|(_, value)| value)
}

const NODE_HELP: &str = "\
Usage: node [options] [script.js] [arguments]
       node -e \"code\"
       node -p \"code\"

Options:
  -e, --eval <code>              evaluate JavaScript or TypeScript source
  -p, --print <code>            evaluate and print the result
  -c, --check <file>            check the syntax of a file
  -i, --interactive             start a REPL
  --input-type=module|commonjs  how to read `-e` input
  --node-version <version>      claim a Node release (18, 20, 21, 22, 23, 24)
  -v, --version                 print the Node version
  -h, --help                    print this help

This is the `codets` toolchain speaking Node's language. TypeScript runs
directly, with no build step.
";

fn run_node(args: Vec<String>) -> ExitCode {
    let options = match parse_node_args(&args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(BAD_OPTION);
        }
    };
    let base = base_compat(options.compat);
    match options.mode {
        NodeMode::Help => {
            print!("{NODE_HELP}");
            ExitCode::SUCCESS
        }
        NodeMode::Version => {
            // Exactly what Node prints, because scripts parse this.
            println!("{}", base.v_string());
            ExitCode::SUCCESS
        }
        NodeMode::Check { path } => check_file(&path),
        NodeMode::Eval { code, print } => eval_inline(&code, print, base, options.input_type),
        NodeMode::Script { path, args } => run_script(&path, &args, base, options.exec_argv),
        NodeMode::Repl => repl(base),
    }
}

/// The compat level to use: an explicit request, else the version manager's
/// answer, else the built-in default.
fn base_compat(explicit: Option<NodeCompat>) -> NodeCompat {
    if let Some(compat) = explicit {
        return compat;
    }
    let store = VersionStore::discover();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    store.resolve(&cwd).node
}

/// `node -c`: parse without running.
fn check_file(path: &Path) -> ExitCode {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("node: cannot read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match crate::parser::parse_script(&source, Language::from_path(path)) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// `node -e` / `node -p`.
fn eval_inline(code: &str, print: bool, base: NodeCompat, input_type: Option<InputType>) -> ExitCode {
    let wants_module = matches!(input_type, Some(InputType::Module))
        || (input_type.is_none() && looks_like_a_module(code));
    if wants_module {
        // A module needs a real file so that relative specifiers resolve the way
        // Node resolves them: against the working directory. The file is a `.mts`
        // so TypeScript is accepted too.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let file = cwd.join(format!(".codets-eval-{}.mts", std::process::id()));
        if let Err(error) = fs::write(&file, code) {
            eprintln!("node: cannot stage the evaluation file: {error}");
            return ExitCode::FAILURE;
        }
        let mut runtime = Runtime::with_compat_and_root(base, cwd);
        runtime.echo_to_stdout();
        let outcome = runtime.run_module(&file);
        let _ = fs::remove_file(&file);
        return finish(runtime, outcome, print);
    }

    let mut runtime = Runtime::with_compat(base);
    runtime.echo_to_stdout();
    let outcome = runtime.eval(code, Language::Ts);
    finish(runtime, outcome, print)
}

/// True when the snippet uses module syntax, so `-e` should treat it as ESM.
fn looks_like_a_module(code: &str) -> bool {
    code.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("import ") || line.starts_with("import{") || line.starts_with("export ")
    })
}

/// Run a script file, choosing the module system the way Node does.
fn run_script(path: &Path, args: &[String], base: NodeCompat, exec_argv: Vec<String>) -> ExitCode {
    if !path.exists() {
        eprintln!("node: cannot find module {}", path.display());
        return ExitCode::FAILURE;
    }
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut runtime = Runtime::with_compat_and_root(base, root);
    runtime.echo_to_stdout();
    runtime.set_script(path, args.to_vec());
    *runtime.node.exec_argv.borrow_mut() = exec_argv;

    if module_system(path) == ModuleSystem::CommonJs && !crate::commonjs::is_available() {
        // Honest failure instead of a misleading `require is not defined`.
        eprintln!(
            "node: {} is CommonJS; the CommonJS loader is not available in this build",
            path.display()
        );
        return ExitCode::FAILURE;
    }

    let outcome = match module_system(path) {
        ModuleSystem::EsModule => runtime.run_module(path),
        ModuleSystem::CommonJs => crate::commonjs::run_entry(&mut runtime, path),
    };
    finish(runtime, outcome, false)
}

/// Which module system a file belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleSystem {
    EsModule,
    CommonJs,
}

/// Node's rule: the extension decides first, then the nearest `package.json`.
pub fn module_system(path: &Path) -> ModuleSystem {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "mjs" | "mts" => ModuleSystem::EsModule,
        "cjs" | "cts" => ModuleSystem::CommonJs,
        _ => match nearest_package_type(path) {
            Some(kind) if kind == "module" => ModuleSystem::EsModule,
            Some(_) => ModuleSystem::CommonJs,
            // A bare `.js` with no package.json is CommonJS in Node.
            None => ModuleSystem::CommonJs,
        },
    }
}

/// The `type` field of the nearest `package.json`, walking up from `path`.
pub fn nearest_package_type(path: &Path) -> Option<String> {
    let mut directory = path.parent()?.to_path_buf();
    loop {
        let candidate = directory.join("package.json");
        if let Ok(contents) = fs::read_to_string(&candidate) {
            if let Some(value) = package_type(&contents) {
                return Some(value);
            }
        }
        if !directory.pop() {
            return None;
        }
    }
}

/// Pull `"type": "..."` out of a `package.json` without a JSON dependency.
fn package_type(contents: &str) -> Option<String> {
    let key = contents.find("\"type\"")?;
    let rest = &contents[key + "\"type\"".len()..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let start = rest.find('"')?;
    let rest = &rest[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Shared teardown: uncaught exceptions, `exit` listeners, exit code.
fn finish(mut runtime: Runtime, outcome: Result<crate::value::Value, Error>, print: bool) -> ExitCode {
    let mut code = runtime.node.exit_code.get();
    match outcome {
        Ok(value) => {
            if print && !matches!(value, crate::value::Value::Undefined) {
                println!("{}", inspect(&value));
            }
        }
        Err(Error::Exit(explicit)) => {
            // `process.exit` already ran the listeners it is going to run.
            run_exit_listeners(&mut runtime, explicit);
            return ExitCode::from(explicit as u8);
        }
        Err(Error::Uncaught(thrown)) => {
            if runtime.node.listener_count("uncaughtException") > 0 {
                let value = thrown.value.clone();
                if let Err(secondary) =
                    runtime.node.emit(&mut runtime.interp, "uncaughtException", &[value])
                {
                    eprintln!("{}", secondary.render());
                    code = 1;
                }
            } else {
                eprintln!("{}", Error::Uncaught(thrown));
                code = 1;
            }
        }
        Err(other) => {
            eprintln!("{other}");
            code = 1;
        }
    }
    run_exit_listeners(&mut runtime, code);
    let final_code = runtime.node.exit_code.get();
    if final_code != 0 {
        code = final_code;
    }
    ExitCode::from(code as u8)
}

fn run_exit_listeners(runtime: &mut Runtime, code: i32) {
    if runtime.node.listener_count("exit") == 0 {
        return;
    }
    let code = crate::value::Value::Num(code as f64);
    if let Err(error) = runtime.node.emit(&mut runtime.interp, "exit", &[code]) {
        eprintln!("{}", error.render());
    }
}

/// The interactive shell.
fn repl(base: NodeCompat) -> ExitCode {
    let mut runtime = Runtime::with_compat(base);
    runtime.echo_to_stdout();
    let interactive = std::io::stdin().is_terminal();
    if interactive {
        println!(
            "codets {} (Node {} compatible) — TypeScript and JavaScript, Ctrl-D to exit",
            env!("CARGO_PKG_VERSION"),
            base.version_string()
        );
    }
    let stdin = std::io::stdin();
    let mut pending = String::new();
    loop {
        if interactive {
            let prompt = if pending.is_empty() { "> " } else { "... " };
            print!("{prompt}");
            let _ = std::io::stdout().flush();
        }
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("codets: cannot read standard input: {error}");
                return ExitCode::FAILURE;
            }
        }
        let trimmed = line.trim();
        if pending.is_empty() && (trimmed == ".exit" || trimmed == "exit") {
            break;
        }
        pending.push_str(&line);
        if pending.trim().is_empty() {
            pending.clear();
            continue;
        }
        match runtime.eval(&pending, Language::Ts) {
            Ok(value) => {
                pending.clear();
                if !matches!(value, crate::value::Value::Undefined) {
                    println!("{}", inspect(&value));
                }
            }
            Err(Error::Exit(code)) => return ExitCode::from(code as u8),
            Err(Error::Parse(_)) => {
                // An incomplete construct keeps the buffer open for the next line.
            }
            Err(error) => {
                pending.clear();
                eprintln!("{error}");
            }
        }
    }
    ExitCode::SUCCESS
}

// -------------------------------------------------------------- codets surface

const CODETS_HELP: &str = "\
codets — a pure-Rust JavaScript & TypeScript toolchain that replaces Node.js and npm

USAGE:
    codets <file.ts|file.js> [args]     Run a file
    codets run <file> [args]            Run a file
    codets eval|-e <code>               Evaluate source
    codets -p <code>                    Evaluate and print the result
    codets repl                         Start the interactive shell
    codets node [node options]          Speak Node's language
    codets npm <command>                Package management

VERSION MANAGEMENT:
    codets versions                     List installed toolchains and what is available
    codets install <version>            Install a toolchain (18, 20, 22, 24, or a full version)
    codets use <version> [--local]      Switch the active version (--local writes .codets-version)
    codets default <version>            Set the version used when nothing else applies
    codets current                      Print the active version and why it is active
    codets which                        Print the shim directory
    codets env                          Print shell setup for PATH
    codets features                     List the standard-library features the active version has
    codets upgrade [--from <path>]      Replace the toolchain with another codets build

GLOBAL OPTIONS:
    --node-version <version>            Claim a Node release for this run
    -v, --version                       Print the toolchain version
    -h, --help                          Print this help
";

fn run_codets(args: Vec<String>) -> ExitCode {
    let mut compat = None;
    let mut index = 0;
    // Global options may precede the subcommand.
    while index < args.len() {
        let arg = args[index].as_str();
        if arg.starts_with("--node-version") || arg.starts_with("--compat") {
            let value = match split_option_value(arg) {
                Some(value) => value.to_string(),
                None => {
                    index += 1;
                    match args.get(index) {
                        Some(value) => value.clone(),
                        None => {
                            eprintln!("codets: {arg} requires a version");
                            return ExitCode::from(BAD_OPTION);
                        }
                    }
                }
            };
            match NodeCompat::request(&value) {
                Some(parsed) => compat = Some(parsed),
                None => {
                    eprintln!("codets: `{value}` is not a Node version");
                    return ExitCode::from(BAD_OPTION);
                }
            }
            index += 1;
            continue;
        }
        break;
    }

    let rest = &args[index..];
    let Some(command) = rest.first().map(|value| value.as_str()) else {
        print!("{CODETS_HELP}");
        return ExitCode::SUCCESS;
    };
    let tail: Vec<String> = rest[1..].to_vec();

    match command {
        "-h" | "--help" | "help" => {
            print!("{CODETS_HELP}");
            ExitCode::SUCCESS
        }
        "-v" | "--version" => {
            let base = base_compat(compat);
            let store = VersionStore::discover();
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let source = match compat {
                Some(_) => "from --node-version".to_string(),
                None => format!("from {}", store.resolve(&cwd).source.describe()),
            };
            println!(
                "codets {} — {} ({}; {})",
                env!("CARGO_PKG_VERSION"),
                base.describe(),
                base.v_string(),
                source
            );
            ExitCode::SUCCESS
        }
        "run" => {
            let Some(path) = tail.first() else {
                eprintln!("codets: `run` needs a file path");
                return ExitCode::from(BAD_OPTION);
            };
            let mut node_args = Vec::new();
            if let Some(compat) = compat {
                node_args.push(format!("--node-version={}", compat.version()));
            }
            node_args.push(path.clone());
            node_args.extend(tail[1..].iter().cloned());
            run_node(node_args)
        }
        "eval" | "-e" | "--eval" => {
            let Some(code) = tail.first() else {
                eprintln!("codets: `eval` needs source code");
                return ExitCode::from(BAD_OPTION);
            };
            let mut node_args = Vec::new();
            if let Some(compat) = compat {
                node_args.push(format!("--node-version={}", compat.version()));
            }
            node_args.push("-e".to_string());
            node_args.push(code.clone());
            run_node(node_args)
        }
        "-p" | "--print" => {
            let Some(code) = tail.first() else {
                eprintln!("codets: `-p` needs source code");
                return ExitCode::from(BAD_OPTION);
            };
            run_node(vec!["-p".to_string(), code.clone()])
        }
        "repl" => repl(base_compat(compat)),
        "node" => run_node(tail),
        "npm" => crate::npm::run(tail),
        "versions" => list_versions(),
        "install" => install_version(tail),
        "use" => use_version(tail, compat),
        "default" => set_default_version(tail),
        "current" => print_current(),
        "which" => {
            let store = VersionStore::discover();
            println!("{}", store.bin_dir().display());
            println!("{}", store.root().display());
            ExitCode::SUCCESS
        }
        "env" => print_env(),
        "features" => list_features(compat),
        "upgrade" => upgrade(tail, compat),
        other if !other.starts_with('-') && Path::new(other).exists() => {
            // Backwards compatible: `codets plugin.ts`.
            let mut node_args = Vec::new();
            if let Some(compat) = compat {
                node_args.push(format!("--node-version={}", compat.version()));
            }
            node_args.push(other.to_string());
            node_args.extend(tail);
            run_node(node_args)
        }
        other => {
            eprintln!("codets: unknown command `{other}` (try `codets --help`)");
            ExitCode::from(BAD_OPTION)
        }
    }
}

fn list_versions() -> ExitCode {
    let store = VersionStore::discover();
    let current = store.current();
    let default = store.default_version();
    let installed = store.installed();

    println!("installed:");
    if installed.is_empty() {
        println!("  (none)");
    }
    for entry in &installed {
        let mut markers = Vec::new();
        if current == Some(entry.version) {
            markers.push("active");
        }
        if default == Some(entry.version) {
            markers.push("default");
        }
        let suffix = if markers.is_empty() {
            String::new()
        } else {
            format!("   [{}]", markers.join(", "))
        };
        println!("  {:<10} {}{suffix}", entry.version, entry.node.describe());
    }

    println!("\navailable to install:");
    for major in NodeCompat::supported_majors() {
        let compat = NodeCompat::for_major(*major);
        let installed_marker = installed
            .iter()
            .any(|entry| entry.version.major == *major);
        let marker = if installed_marker { "installed" } else { "" };
        println!("  {:<10} {}{marker}", compat.version(), compat.describe());
    }
    println!("\nstore: {}", store.root().display());
    ExitCode::SUCCESS
}

/// Accept `24`, `v24.0.0` or `24.0.0` as a version request. A bare major means
/// that release line's baseline, so `codets use 22` installs `22.12.0`.
fn parse_version_request(text: &str) -> Result<Version, String> {
    let compat = NodeCompat::request(text)
        .ok_or_else(|| format!("`{text}` is not a version; try one of {:?}", NodeCompat::supported_majors()))?;
    Ok(compat.version())
}

fn install_version(args: Vec<String>) -> ExitCode {
    let Some(requested) = args.first() else {
        eprintln!("codets: `install` needs a version, for example `codets install 24`");
        return ExitCode::from(BAD_OPTION);
    };
    let store = VersionStore::discover();
    // `codets install 24` should always do something useful, so an already
    // installed version is reported rather than silently accepted.
    match parse_version_request(requested) {
        Ok(version) => match store.install(version) {
            Ok(installed) => {
                println!("installed {} — {}", installed.version, installed.node.describe());
                println!("  {}", installed.path.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("codets: {error}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("codets: {message}");
            ExitCode::from(BAD_OPTION)
        }
    }
}

fn use_version(args: Vec<String>, explicit: Option<NodeCompat>) -> ExitCode {
    let local = args.iter().any(|arg| arg == "--local" || arg == "-l");
    let request = args.iter().find(|arg| !arg.starts_with('-'));
    let Some(request) = request else {
        eprintln!("codets: `use` needs a version, for example `codets use 22`");
        return ExitCode::from(BAD_OPTION);
    };
    let version = match parse_version_request(request) {
        Ok(version) => version,
        Err(message) => {
            eprintln!("codets: {message}");
            return ExitCode::from(BAD_OPTION);
        }
    };
    let store = VersionStore::discover();
    if !store.is_installed(&version) {
        match store.install(version) {
            Ok(installed) => println!("installed {} ({})", installed.version, installed.node.describe()),
            Err(error) => {
                eprintln!("codets: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    if local {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return match versions::write_project_pin(&cwd, &version) {
            Ok(path) => {
                println!("{VERSION_FILE} written: {} -> {version}", path.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("codets: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if let Err(error) = store.set_current(&version) {
        eprintln!("codets: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = versions::refresh_bin_shims(&store, &version) {
        eprintln!("codets: {error}");
        return ExitCode::FAILURE;
    }
    let compat = explicit.unwrap_or_else(|| NodeCompat::for_major(version.major));
    println!("now using {version} — {}", compat.describe());
    println!("shims: {}", store.bin_dir().display());
    ExitCode::SUCCESS
}

fn set_default_version(args: Vec<String>) -> ExitCode {
    let Some(request) = args.first() else {
        eprintln!("codets: `default` needs a version");
        return ExitCode::from(BAD_OPTION);
    };
    let version = match parse_version_request(request) {
        Ok(version) => version,
        Err(message) => {
            eprintln!("codets: {message}");
            return ExitCode::from(BAD_OPTION);
        }
    };
    let store = VersionStore::discover();
    if !store.is_installed(&version) {
        if let Err(error) = store.install(version) {
            eprintln!("codets: {error}");
            return ExitCode::FAILURE;
        }
    }
    match store.set_default(&version) {
        Ok(()) => {
            println!("default version is {version}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("codets: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_current() -> ExitCode {
    let store = VersionStore::discover();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolution = store.resolve(&cwd);
    println!(
        "{} ({})",
        resolution.node.v_string(),
        resolution.source.describe()
    );
    if resolution.source == VersionSource::Environment {
        println!("  set by {VERSION_ENV}");
    }
    ExitCode::SUCCESS
}

fn print_env() -> ExitCode {
    let store = VersionStore::discover();
    let bin = store.bin_dir();
    println!("# put the shims first on PATH so `node`, `npm` and `codets` are found");
    if cfg!(windows) {
        println!("set PATH={};%PATH%", bin.display());
    } else {
        println!("export PATH=\"{}:$PATH\"", bin.display());
    }
    println!("# the shim directory is regenerated by `codets use <version>`");
    ExitCode::SUCCESS
}

fn list_features(explicit: Option<NodeCompat>) -> ExitCode {
    let base = base_compat(explicit);
    println!("{} — {}", base.v_string(), base.describe());
    println!("\nenabled:");
    for feature in base.enabled_features() {
        println!("  {:<48} (Node >= {})", feature.name(), feature.minimum_major());
    }
    let disabled: Vec<_> = crate::node::Feature::ALL
        .into_iter()
        .filter(|feature| !base.supports(*feature))
        .collect();
    if !disabled.is_empty() {
        println!("\nnot in this release:");
        for feature in disabled {
            println!("  {:<48} (needs Node >= {})", feature.name(), feature.minimum_major());
        }
    }
    ExitCode::SUCCESS
}

fn upgrade(args: Vec<String>, explicit: Option<NodeCompat>) -> ExitCode {
    let store = VersionStore::discover();
    let mut from: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from" => {
                index += 1;
                match args.get(index) {
                    Some(path) => from = Some(PathBuf::from(path)),
                    None => {
                        eprintln!("codets: --from needs a path");
                        return ExitCode::from(BAD_OPTION);
                    }
                }
            }
            other if other.starts_with("--from=") => {
                from = Some(PathBuf::from(split_option_value(other).unwrap_or_default()));
            }
            _ => {}
        }
        index += 1;
    }

    let version = match explicit {
        Some(compat) => compat.version(),
        None => match store.current() {
            Some(version) => version,
            None => NodeCompat::default().version(),
        },
    };

    let Some(source) = from else {
        // Fetching a release needs a network client, which this build does not
        // carry yet. Say so instead of pretending the upgrade happened.
        eprintln!(
            "codets: upgrading from a release server is not implemented in this build.\n\
             \x20       Use `codets upgrade --from <path-to-codets-binary>` to replace the toolchain\n\
             \x20       with a build you already have, or `codets install <version>` / `codets use <version>`\n\
             \x20       to switch between Node compatibility levels."
        );
        return ExitCode::FAILURE;
    };

    if !source.is_file() {
        eprintln!("codets: {} is not a file", source.display());
        return ExitCode::FAILURE;
    }
    let installed = match store.install(version) {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("codets: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The shims in the version directory should call the new build, not the one
    // that is running right now, which is what makes this an upgrade.
    let target = installed.path.join(if cfg!(windows) { "codets.exe" } else { "codets" });
    if let Err(error) = fs::copy(&source, &target) {
        eprintln!("codets: cannot copy {} to {}: {error}", source.display(), target.display());
        return ExitCode::FAILURE;
    }
    if let Err(error) = rewrite_shims_for(&installed.path, &target, &installed.version) {
        eprintln!("codets: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = store.set_current(&installed.version) {
        eprintln!("codets: {error}");
        return ExitCode::FAILURE;
    }
    println!(
        "upgraded {} to the build at {}",
        installed.version,
        source.display()
    );
    ExitCode::SUCCESS
}

/// Point a version directory's shims at a specific executable.
fn rewrite_shims_for(directory: &Path, executable: &Path, version: &Version) -> Result<(), String> {
    let executable = executable.to_string_lossy().to_string();
    for command in ["codets", "node", "npm"] {
        let names = if cfg!(windows) {
            vec![format!("{command}.cmd")]
        } else {
            vec![command.to_string()]
        };
        let mut flags = vec![format!("--node-version={version}")];
        if command != "codets" {
            flags.push(format!("--codets-as={command}"));
        }
        for name in names {
            let path = directory.join(name);
            let quoted = flags
                .iter()
                .map(|flag| format!("\"{flag}\""))
                .collect::<Vec<_>>()
                .join(" ");
            let body = if cfg!(windows) {
                format!("@echo off\r\n\"{executable}\" {quoted} %*\r\n")
            } else {
                format!("#!/bin/sh\nexec \"{executable}\" {quoted} \"$@\"\n")
            };
            fs::write(&path, body).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        }
    }
    Ok(())
}
