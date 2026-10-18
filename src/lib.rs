//! # codets
//!
//! A pure-Rust JavaScript & TypeScript toolchain that replaces Node.js and npm,
//! with **native TypeScript support**.
//!
//! The crate is both a command line and a library:
//!
//! * Installing it gives you three commands — `codets`, `node` and `npm` — that
//!   are the same program, chosen by `argv[0]`. See [`cli`].
//! * The library is the engine plus the Node surface area: [`Runtime`] for
//!   embedding, [`commonjs`] for `require`, [`core_modules`] for the standard
//!   library, [`node`] for `process` and the compatibility levels, and [`npm`]
//!   for the package manager.
//!
//! ## Native TypeScript
//!
//! There is no transpile step and no separate declaration pipeline. The parser
//! understands TypeScript syntax directly and erases types while building the
//! AST: `interface`, `type`, generics, `as`, `!`, `readonly`, decorators and
//! `public`/`private` cost nothing at run time. `enum` and constructor parameter
//! properties are the only TS constructs with runtime semantics, so they are the
//! only ones kept in the tree.
//!
//! ## Dependencies
//!
//! The engine needs only the pure-Rust `regex` crate. The package manager adds
//! `flate2` (with its Rust backend), `tar`, `sha2` and `ureq` — all pure Rust, with
//! TLS coming from the platform rather than a bundled engine. Building `codets`
//! therefore needs no C toolchain.
//!
//! ## Example
//!
//! ```no_run
//! use codets::Runtime;
//!
//! let mut runtime = Runtime::new();
//! let output = runtime.run_source(
//!     "const greet = (who: string): string => `hi ${who}`;\nconsole.log(greet('world'));",
//!     codets::Language::Ts,
//! )?;
//! assert_eq!(output.trim(), "hi world");
//! # Ok::<(), codets::Error>(())
//! ```
//!
//! ## What it is not
//!
//! A tree-walking interpreter with reference-counted values: reference cycles are
//! never collected and it is far slower than a JIT. Native addons (`.node`) cannot
//! load, `Symbol`/`Proxy`/`Reflect` and generators are not implemented, and
//! dependency resolution is flat. The README's *Limits* section is the complete
//! list; treat anything absent there as a bug and anything present as deliberate.

pub mod ast;
pub mod builtins;
pub mod cli;
pub mod commonjs;
pub mod core_modules;
pub mod interp;
pub mod lexer;
pub mod modules;
pub mod net;
pub mod node;
pub mod npm;
pub mod parser;
pub mod value;
pub mod versions;

pub use interp::{Control, Interp, JsError};
pub use modules::ModuleLoader;
pub use parser::{Language, ParseError};

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Everything that can go wrong when running a script.
#[derive(Debug)]
pub enum Error {
    /// A syntax error in the source.
    Parse(ParseError),
    /// A JavaScript value was thrown and not caught.
    Uncaught(JsError),
    /// Reading a module from disk failed.
    Io(String),
    /// The script called `process.exit(code)`.
    Exit(i32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(error) => write!(f, "{error}"),
            Error::Uncaught(error) => write!(f, "{}", error.render()),
            Error::Io(message) => write!(f, "{message}"),
            Error::Exit(code) => write!(f, "process exited with code {code}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ParseError> for Error {
    fn from(error: ParseError) -> Self {
        Error::Parse(error)
    }
}

impl From<Control> for Error {
    fn from(control: Control) -> Self {
        match control {
            Control::Throw(error) => Error::Uncaught(error),
            Control::Return(_) => {
                Error::Io("unexpected `return` outside a function".to_string())
            }
            Control::Break(_) => Error::Io("unexpected `break` outside a loop".to_string()),
            Control::Continue(_) => {
                Error::Io("unexpected `continue` outside a loop".to_string())
            }
            Control::Exit(code) => Error::Exit(code),
        }
    }
}

/// A script runtime: an interpreter, a Node.js host, and a module loader rooted
/// at a directory.
pub struct Runtime {
    pub interp: Interp,
    pub loader: ModuleLoader,
    /// The Node.js process state behind `process`.
    pub node: Rc<node::NodeHost>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Create a runtime whose module resolution starts in the current directory,
    /// speaking the newest supported Node.js release.
    pub fn new() -> Runtime {
        Runtime::with_compat(node::NodeCompat::default())
    }

    /// Create a runtime that resolves relative imports from `root`.
    pub fn with_root(root: impl Into<PathBuf>) -> Runtime {
        Runtime::with_compat_and_root(node::NodeCompat::default(), root)
    }

    /// Create a runtime that claims to be a specific Node.js release.
    pub fn with_compat(compat: node::NodeCompat) -> Runtime {
        Runtime::with_compat_and_root(
            compat,
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        )
    }

    /// Both at once: a Node.js release and a module root.
    pub fn with_compat_and_root(compat: node::NodeCompat, root: impl Into<PathBuf>) -> Runtime {
        let mut interp = Interp::new();
        let host = Rc::new(node::NodeHost::new(compat));
        host.install(&mut interp);
        Runtime {
            interp,
            loader: ModuleLoader::new(root),
            node: host,
        }
    }

    /// Record which script is running, so `process.argv` and `process.argv0` are
    /// what Node would report.
    ///
    /// `process.cwd()` deliberately keeps the directory the command was run from:
    /// Node resolves relative *imports* against the importing file, which the
    /// module loader already does, while `cwd()` reports the shell's directory.
    pub fn set_script(&mut self, path: impl AsRef<Path>, args: Vec<String>) {
        let path = path.as_ref();
        *self.node.script_path.borrow_mut() = Some(path.to_string_lossy().to_string());
        if let Some(name) = path.file_name() {
            *self.node.argv0.borrow_mut() = name.to_string_lossy().to_string();
        }
        *self.node.script_args.borrow_mut() = args;
    }

    /// Resolve relative imports against this directory from now on.
    pub fn set_module_root(&mut self, root: impl Into<PathBuf>) {
        self.loader = ModuleLoader::new(root);
    }

    /// Print `console` output to the process's standard output.
    pub fn echo_to_stdout(&mut self) {
        self.interp.echo_to_stdout();
    }

    /// Capture `console` output instead of printing it. Useful in tests.
    pub fn capture_output(&mut self) -> Rc<RefCell<String>> {
        self.interp.capture_output()
    }

    /// Run a source string, returning the console output it produced.
    pub fn run_source(&mut self, source: &str, language: Language) -> Result<String, Error> {
        let buffer = self.interp.capture_output();
        self.eval(source, language)?;
        let text = buffer.borrow().clone();
        Ok(text)
    }

    /// Evaluate a source string in the global scope, as a script.
    pub fn eval(&mut self, source: &str, language: Language) -> Result<value::Value, Error> {
        let parsed = parser::parse_script(source, language)?;
        let env = interp::Interp::module_env(&mut self.interp);
        self.interp.hoist(&parsed.stmts, &env, &env)?;
        let result = self.interp.eval_block(&parsed.stmts, &env)?;
        self.interp.run_event_loop()?;
        Ok(result)
    }

    /// Load and execute an ES module from disk, returning its exports.
    pub fn run_module(&mut self, path: &Path) -> Result<value::Value, Error> {
        let namespace = self.loader.load(&mut self.interp, path)?;
        self.interp.run_event_loop()?;
        Ok(namespace)
    }

    /// Load a module and return its exports as a value.
    pub fn import(&mut self, path: &Path) -> Result<value::Value, Error> {
        self.run_module(path)
    }

    /// Expose a Rust closure to scripts as a global function.
    pub fn register_fn<F>(&mut self, name: &str, body: F)
    where
        F: Fn(&mut Interp, &value::Value, &[value::Value]) -> Result<value::Value, Control>
            + 'static,
    {
        let function = self.interp.native(name, 0, body);
        self.interp.define_global(name, function);
    }

    /// Set a global value.
    pub fn set_global(&mut self, name: &str, value: value::Value) {
        self.interp.define_global(name, value);
    }

    /// Read a global value.
    pub fn get_global(&self, name: &str) -> value::Value {
        self.interp
            .globals
            .resolve(name)
            .map(|(_, binding)| binding.value)
            .unwrap_or(value::Value::Undefined)
    }
}
