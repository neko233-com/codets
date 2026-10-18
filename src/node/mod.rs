//! Node.js surface area: the `process` global, Node-version compatibility, and
//! (in later modules) CommonJS and the core modules.
//!
//! Everything here is what turns the `codets` engine into something that can run
//! a Node program: `process`, `globalThis.process`, `require` and the standard
//! library modules are supplied by this module tree rather than by the language
//! core in `builtins`.

pub mod compat;
pub mod features;
pub mod process;

pub use compat::{Feature, NodeCompat, Version, DEFAULT_MAJOR};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use crate::interp::{Control, Interp};
use crate::value::Value;

/// One Node.js process: the state `process` reads and writes.
///
/// The host is shared (`Rc`) because every `process` accessor closes over it, and
/// because the command line needs to read the exit code after execution.
pub struct NodeHost {
    /// The Node release this realm claims to be.
    pub compat: NodeCompat,
    /// The `codets` release doing the work, reported as `process.versions.codets`.
    pub codets_version: &'static str,
    /// How the toolchain was invoked, for `process.argv0`.
    pub argv0: RefCell<String>,
    /// Path to the running executable, for `process.execPath`.
    pub exec_path: String,
    /// Path of the script being run, if any.
    pub script_path: RefCell<Option<String>>,
    /// Arguments after the script path.
    pub script_args: RefCell<Vec<String>>,
    /// Node-level options that were accepted, for `process.execArgv`.
    pub exec_argv: RefCell<Vec<String>>,
    /// `process.exitCode`, also written by `process.exit`.
    pub exit_code: Rc<Cell<i32>>,
    /// True once `process.exit` has been called.
    pub exited: Cell<bool>,
    /// The working directory, maintained by `process.chdir`.
    pub cwd: RefCell<PathBuf>,
    /// The environment snapshot behind `process.env`.
    pub env: RefCell<IndexMapish>,
    /// `process.on(...)` listeners, keyed by event name.
    pub listeners: RefCell<HashMap<String, Vec<(Value, bool)>>>,
    /// Monotonic origin for `process.hrtime` and `process.uptime`.
    pub started: Instant,
}

/// A tiny ordered string map for the environment, so `process.env` keeps the
/// order the variables were created in.
pub type IndexMapish = Vec<(String, String)>;

impl NodeHost {
    /// A host with the environment inherited from the real process.
    pub fn new(compat: NodeCompat) -> NodeHost {
        let args: Vec<String> = std::env::args().collect();
        let argv0 = args
            .first()
            .map(|first| {
                Path::new(first)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| first.clone())
            })
            .unwrap_or_else(|| "codets".to_string());
        let exec_path = std::env::current_exe()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| args.first().cloned().unwrap_or_default());
        NodeHost {
            compat,
            codets_version: env!("CARGO_PKG_VERSION"),
            argv0: RefCell::new(argv0),
            exec_path,
            script_path: RefCell::new(None),
            script_args: RefCell::new(Vec::new()),
            exec_argv: RefCell::new(Vec::new()),
            exit_code: Rc::new(Cell::new(0)),
            exited: Cell::new(false),
            cwd: RefCell::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            env: RefCell::new(std::env::vars().collect()),
            listeners: RefCell::new(HashMap::new()),
            started: Instant::now(),
        }
    }

    /// `process.argv`: the executable, the script, then the script's arguments.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.exec_path.clone()];
        if let Some(script) = self.script_path.borrow().clone() {
            argv.push(script);
        }
        argv.extend(self.script_args.borrow().iter().cloned());
        argv
    }

    /// Read an environment variable.
    pub fn get_env(&self, name: &str) -> Option<String> {
        self.env
            .borrow()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    /// Create or update an environment variable.
    pub fn set_env(&self, name: &str, value: &str) {
        let mut env = self.env.borrow_mut();
        match env.iter_mut().find(|(key, _)| key == name) {
            Some(entry) => entry.1 = value.to_string(),
            None => env.push((name.to_string(), value.to_string())),
        }
    }

    /// Register a listener, as `process.on` does.
    pub fn on(&self, event: &str, listener: Value, once: bool) {
        self.listeners
            .borrow_mut()
            .entry(event.to_string())
            .or_default()
            .push((listener, once));
    }

    /// Remove a listener by identity; returns whether one was removed.
    pub fn off(&self, event: &str, listener: &Value) -> bool {
        let mut listeners = self.listeners.borrow_mut();
        let Some(entries) = listeners.get_mut(event) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|(stored, _)| !stored.strict_equals(listener));
        before != entries.len()
    }

    /// Drop every listener for one event, or for all events.
    pub fn remove_all_listeners(&self, event: Option<&str>) {
        let mut listeners = self.listeners.borrow_mut();
        match event {
            Some(name) => {
                listeners.remove(name);
            }
            None => listeners.clear(),
        }
    }

    /// The registered listeners for an event, for `process.listeners`.
    pub fn listeners_for(&self, event: &str) -> Vec<Value> {
        self.listeners
            .borrow()
            .get(event)
            .map(|entries| entries.iter().map(|(value, _)| value.clone()).collect())
            .unwrap_or_default()
    }

    pub fn listener_count(&self, event: &str) -> usize {
        self.listeners
            .borrow()
            .get(event)
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    /// Call every listener for `event`. Returns whether any ran.
    ///
    /// `once` listeners are removed before they are called, matching Node: a
    /// listener that re-enters `emit` must not run twice.
    pub fn emit(
        &self,
        interp: &mut Interp,
        event: &str,
        args: &[Value],
    ) -> Result<bool, Control> {
        let listeners = self.listeners_for(event);
        if listeners.is_empty() {
            return Ok(false);
        }
        {
            let mut stored = self.listeners.borrow_mut();
            if let Some(entries) = stored.get_mut(event) {
                entries.retain(|(_, once)| !*once);
            }
        }
        for listener in listeners {
            interp.call_value(&listener, Value::Undefined, args.to_vec())?;
        }
        Ok(true)
    }

    /// Install the Node globals this module tree provides.
    pub fn install(self: &Rc<NodeHost>, interp: &mut Interp) {
        let process = process::build(self, interp);
        interp.define_global("process", process);
        features::install(interp, self.compat);
    }
}
