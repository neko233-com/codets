//! # rust4ts
//!
//! A pure-Rust JavaScript & TypeScript engine with **native TypeScript support**.
//!
//! `rust4ts` is meant to be embedded in a Rust application (game engine, desktop
//! shell, plugin host) where you want to run user-supplied `.ts` / `.js` scripts
//! without shipping a C toolchain and without depending on Node.js.
//!
//! ## Why a from-scratch engine?
//!
//! Embedding options such as `rquickjs` or `deno_core` bind a C/C++ engine. That
//! forces a native toolchain on every consumer and complicates cross-compilation.
//! `rust4ts` is written in 100% Rust — the single dependency is the pure-Rust
//! `regex` crate — so `cargo add rust4ts` just works.
//!
//! ## Native TypeScript
//!
//! There is no transpile step and no separate declaration pipeline. The parser
//! understands TypeScript syntax directly and erases types while building the
//! AST: `interface`, `type`, generics, `as`, `!`, `readonly`, decorators and
//! `public`/`private` cost nothing at run time. `enum` is the only TS construct
//! with runtime semantics, so it is kept in the tree and evaluated.
//!
//! ## Scope
//!
//! The target is **your own plugin and application code**, not the npm
//! ecosystem:
//!
//! * ES modules only (`import` / `export`, including `import.meta`). There is no
//!   CommonJS `require` shim and no `node_modules` resolution.
//! * A tree-walking interpreter: correctness first, not speed.
//! * Reference-counted values; reference cycles are not collected.
//! * No type checking — types are erased, exactly like Node's type stripping.
//! * Generators, `Symbol` and object freezing are not implemented.
//!
//! ## Example
//!
//! ```no_run
//! use rust4ts::Runtime;
//!
//! let mut runtime = Runtime::new();
//! let output = runtime.run_source(
//!     "const greet = (who: string): string => `hi ${who}`;\nconsole.log(greet('world'));",
//!     rust4ts::Language::Ts,
//! )?;
//! assert_eq!(output.trim(), "hi world");
//! # Ok::<(), rust4ts::Error>(())
//! ```

pub mod ast;
pub mod builtins;
pub mod interp;
pub mod lexer;
pub mod modules;
pub mod parser;
pub mod value;

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
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(error) => write!(f, "{error}"),
            Error::Uncaught(error) => write!(f, "{}", error.render()),
            Error::Io(message) => write!(f, "{message}"),
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
        }
    }
}

/// A script runtime: an interpreter plus a module loader rooted at a directory.
pub struct Runtime {
    pub interp: Interp,
    pub loader: ModuleLoader,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Create a runtime whose module resolution starts in the current directory.
    pub fn new() -> Runtime {
        Runtime {
            interp: Interp::new(),
            loader: ModuleLoader::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        }
    }

    /// Create a runtime that resolves relative imports from `root`.
    pub fn with_root(root: impl Into<PathBuf>) -> Runtime {
        Runtime {
            interp: Interp::new(),
            loader: ModuleLoader::new(root),
        }
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
