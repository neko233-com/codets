//! The `rust4ts` command line runner.
//!
//! ```text
//! rust4ts <file.ts>        run a module (imports resolve relative to the file)
//! rust4ts eval "<code>"    evaluate a snippet in the global scope
//! rust4ts --version
//! ```

use std::path::Path;
use std::process::ExitCode;

use rust4ts::{Language, Runtime};

const HELP: &str = "\
rust4ts - a pure-Rust JavaScript & TypeScript engine

USAGE:
    rust4ts <file.ts|file.js>     Run a module
    rust4ts run <file.ts>         Run a module
    rust4ts eval \"<code>\"         Evaluate TypeScript or JavaScript source
    rust4ts --version, -V         Print the version
    rust4ts --help, -h            Print this help

NOTES:
    Modules are ES modules; there is no CommonJS `require`.
    Relative imports resolve with the .ts / .js extensions added automatically.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    let first = match args.first() {
        Some(first) => first.as_str(),
        None => {
            print!("{HELP}");
            return Ok(ExitCode::SUCCESS);
        }
    };

    match first {
        "--help" | "-h" => {
            print!("{HELP}");
            Ok(ExitCode::SUCCESS)
        }
        "--version" | "-V" => {
            println!("rust4ts {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        "eval" | "-e" | "--eval" => {
            let code = args
                .get(1)
                .ok_or_else(|| "rust4ts eval needs a source string".to_string())?;
            let mut runtime = Runtime::new();
            runtime.echo_to_stdout();
            runtime
                .eval(code, Language::Ts)
                .map_err(|error| error.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            let path = args
                .get(1)
                .ok_or_else(|| "rust4ts run needs a file path".to_string())?;
            run_file(path)
        }
        // A bare path is the common case: `rust4ts plugin.ts`.
        path if !path.starts_with('-') => run_file(path),
        other => Err(format!("unknown option `{other}` (try `rust4ts --help`)")),
    }
}

fn run_file(path: &str) -> Result<ExitCode, String> {
    let path = Path::new(path);
    if !path.exists() {
        return Err(format!("no such file: {}", path.display()));
    }
    // Resolve relative imports against the script's own directory, which is what
    // any other runtime does.
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let mut runtime = Runtime::with_root(root);
    runtime.echo_to_stdout();
    runtime
        .run_module(path)
        .map_err(|error| error.to_string())?;
    Ok(ExitCode::SUCCESS)
}
