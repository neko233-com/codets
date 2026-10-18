//! The CommonJS loader: `require`, `module`, `exports`, `__filename`,
//! `__dirname`, and the `node_modules` resolution algorithm.
//!
//! CommonJS is not optional if the goal is to run what npm ships: the majority of
//! published packages are CommonJS or dual-published, and `require` is how they
//! load each other.
//!
//! What this implements:
//!
//! * `require` of a relative path, an absolute path, a core module (with or
//!   without the `node:` prefix), or a package in `node_modules`.
//! * The `node_modules` walk: starting at the requiring file's directory and
//!   moving up, so nested and hoisted layouts both resolve.
//! * `package.json` `exports` (a string, or the `"."` entry, including the
//!   `require` / `default` conditions) and `main`, then `index.*`.
//! * Extensions `.js`, `.cjs`, `.json`, `.ts`, `.cts`; directory indices; and
//!   `require.resolve`.
//! * Circular requires, which see the partially built `exports` object exactly as
//!   Node's do, because the module is registered before its body runs.
//!
//! What it does not:
//!
//! * `.node` native addons, which bind V8's ABI. A package that needs one cannot
//!   run here, and the error says so.
//! * `require` of an ES module (`.mjs` / `.mts`), and `import` of CommonJS through
//!   the ESM loader.
//! * `require.cache` is a placeholder object, so cache eviction is not available.

use std::path::{Path, PathBuf};

use crate::interp::{Control, Interp};
use crate::value::{Binding, Env, Value};
use crate::{core_modules, Error, Language, Runtime};

/// Extensions tried, in Node's order.
const EXTENSIONS: [&str; 7] = ["", ".js", ".cjs", ".mjs", ".ts", ".cts", ".mts"];

/// The CommonJS loader is always compiled in; the CLI asks so that it can report
/// an honest error when a build lacks it.
pub fn is_available() -> bool {
    true
}

/// Run a CommonJS file as the process entry point.
pub fn run_entry(runtime: &mut Runtime, path: &Path) -> Result<Value, Error> {
    let resolved = runtime
        .interp
        .canonical_path(path)
        .map_err(Error::Io)?;
    install_globals(runtime, &resolved);
    load_file(&mut runtime.interp, &resolved, true).map_err(Error::from)
}

/// Load a CommonJS file by path, for the ES module loader's interop.
///
/// The module cache is shared, so a package `require`d and `import`ed is
/// evaluated once, exactly as Node evaluates it once.
pub fn load_path(interp: &mut Interp, path: &Path) -> Result<Value, Error> {
    let resolved = interp
        .canonical_path(path)
        .unwrap_or_else(|_| path.to_path_buf());
    load_file(interp, &resolved, false).map_err(Error::from)
}

/// Give script code the CommonJS globals, resolved from `file`'s directory.
///
/// `node -e` and the REPL run outside a module file, but `require` still has to
/// work; Node resolves from the working directory in that case.
pub fn install_globals(runtime: &mut Runtime, file: &Path) {
    let directory = file
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let module = runtime.interp.new_object();
    let exports = runtime.interp.new_object();
    {
        let mut borrowed = module.borrow_mut();
        borrowed.props.set("exports", Value::Object(exports.clone()));
        borrowed
            .props
            .set("filename", Value::str(file.to_string_lossy().to_string()));
        borrowed
            .props
            .set("id", Value::str(file.to_string_lossy().to_string()));
        borrowed.props.set("loaded", Value::Bool(false));
    }
    let require = make_require(&mut runtime.interp, directory.clone());
    runtime.interp.define_global("require", require);
    runtime
        .interp
        .define_global("module", Value::Object(module));
    runtime
        .interp
        .define_global("exports", Value::Object(exports));
    runtime.interp.define_global(
        "__filename",
        Value::str(file.to_string_lossy().to_string()),
    );
    runtime.interp.define_global(
        "__dirname",
        Value::str(directory.to_string_lossy().to_string()),
    );
}

/// Build a `require` bound to one directory.
pub fn make_require(interp: &mut Interp, from_dir: PathBuf) -> Value {
    let resolve_dir = from_dir.clone();
    let require = interp.native("require", 1, move |interp, _this, args| {
        let specifier = arg(interp, args, 0)?;
        require_from(interp, &specifier, &from_dir)
    });
    // `require.resolve` answers the same question without loading.
    let resolve = interp.native("resolve", 1, move |interp, _this, args| {
        let specifier = arg(interp, args, 0)?;
        if core_modules::is_core(&specifier) {
            return Ok(Value::str(specifier));
        }
        match resolve_specifier(&specifier, &resolve_dir) {
            Ok(path) => Ok(Value::str(path.to_string_lossy().to_string())),
            Err(message) => Err(interp.throw_error("Error", message)),
        }
    });
    if let Value::Object(object) = &require {
        object.borrow_mut().props.set("resolve", resolve);
        let cache = interp.new_object();
        object.borrow_mut().props.set("cache", Value::Object(cache));
    }
    require
}

/// The `require` call itself: core module, then the filesystem.
fn require_from(interp: &mut Interp, specifier: &str, from_dir: &Path) -> Result<Value, Control> {
    if core_modules::is_core(specifier) {
        if let Some(module) = core_modules::resolve(interp, specifier) {
            return Ok(module);
        }
    }
    let resolved = match resolve_specifier(specifier, from_dir) {
        Ok(path) => path,
        Err(message) => return Err(interp.throw_error("Error", message)),
    };
    load_file(interp, &resolved, false)
}

/// Load and evaluate a resolved CommonJS file, using the module cache.
fn load_file(interp: &mut Interp, path: &Path, is_entry: bool) -> Result<Value, Control> {
    if let Some(cached) = interp.cjs_cache.get(path) {
        return Ok(cached.clone());
    }
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "json" => {
            let text = std::fs::read_to_string(path).map_err(|error| {
                interp.throw_error("Error", format!("cannot read {}: {error}", path.display()))
            })?;
            let value = parse_json(interp, &text, path)?;
            interp.cjs_cache.insert(path.to_path_buf(), value.clone());
            Ok(value)
        }
        "node" => Err(interp.throw_error(
            "Error",
            format!(
                "{} is a native addon (.node). codets cannot load it: native addons bind V8's ABI, \
                 which a pure-Rust engine does not expose.",
                path.display()
            ),
        )),
        "mjs" | "mts" => Err(interp.throw_error(
            "Error",
            format!(
                "require() of {} failed: it is an ES module. Use `import` from an ES module, or \
                 Node's `await import(...)`.",
                path.display()
            ),
        )),
        _ => evaluate_module(interp, path, is_entry),
    }
}

/// Wrap a source file in the CommonJS module function and run it.
fn evaluate_module(interp: &mut Interp, path: &Path, is_entry: bool) -> Result<Value, Control> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        interp.throw_error("Error", format!("cannot read {}: {error}", path.display()))
    })?;
    let parsed = crate::parser::parse_script(&source, Language::from_path(path))
        .map_err(|error| interp.throw_error("SyntaxError", format!("{}: {error}", path.display())))?;

    let directory = path
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let module = interp.new_object();
    let exports = interp.new_object();
    module
        .borrow_mut()
        .props
        .set("exports", Value::Object(exports.clone()));
    module
        .borrow_mut()
        .props
        .set("filename", Value::str(path.to_string_lossy().to_string()));
    module
        .borrow_mut()
        .props
        .set("id", Value::str(path.to_string_lossy().to_string()));
    module.borrow_mut().props.set("loaded", Value::Bool(false));
    if is_entry {
        module.borrow_mut().props.set("main", Value::Bool(true));
    }

    // Register before running the body so a cycle finds the partial exports.
    interp
        .cjs_cache
        .insert(path.to_path_buf(), Value::Object(exports.clone()));

    let require = make_require(interp, directory.clone());
    let env = Env::new(Some(interp.globals.clone()), true);
    for (name, value) in [
        ("module", Value::Object(module.clone())),
        ("exports", Value::Object(exports.clone())),
        ("require", require),
        ("__filename", Value::str(path.to_string_lossy().to_string())),
        ("__dirname", Value::str(directory.to_string_lossy().to_string())),
    ] {
        env.declare(
            name,
            Binding {
                value,
                mutable: true,
                initialized: true,
            },
        );
    }

    let previous_dir = interp.current_dir.clone();
    let previous_source = interp.current_source.clone();
    interp.current_dir = Some(directory);
    interp.current_source = Some(path.to_string_lossy().to_string());
    interp.hoist(&parsed.stmts, &env, &env)?;
    let outcome = interp.eval_block(&parsed.stmts, &env);
    interp.current_dir = previous_dir;
    interp.current_source = previous_source;
    outcome?;

    module
        .borrow_mut()
        .props
        .set("loaded", Value::Bool(true));
    // `module.exports = x` wins over the initial object.
    let final_exports = module
        .borrow()
        .props
        .get("exports")
        .cloned()
        .unwrap_or(Value::Object(exports));
    interp
        .cjs_cache
        .insert(path.to_path_buf(), final_exports.clone());
    Ok(final_exports)
}

fn parse_json(interp: &mut Interp, text: &str, path: &Path) -> Result<Value, Control> {
    match crate::builtins::json::parse(text) {
        Some(raw) => crate::builtins::materialise(interp, raw),
        None => Err(interp.throw_error(
            "SyntaxError",
            format!("{} is not valid JSON", path.display()),
        )),
    }
}

/// Turn a specifier written in `from_dir` into a file path.
pub fn resolve_specifier(specifier: &str, from_dir: &Path) -> Result<PathBuf, String> {
    let looks_relative = specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with(".\\")
        || specifier.starts_with("..\\")
        || Path::new(specifier).is_absolute();
    if looks_relative {
        let base = if Path::new(specifier).is_absolute() {
            PathBuf::from(specifier)
        } else {
            from_dir.join(specifier)
        };
        return resolve_as_path(&base).ok_or_else(|| {
            format!("Cannot find module '{specifier}' from '{}'", from_dir.display())
        });
    }

    // A bare specifier is a package: walk `node_modules` upwards.
    let (package, subpath) = split_package(specifier);
    let mut directory = Some(from_dir.to_path_buf());
    while let Some(current) = directory {
        let candidate = current.join("node_modules").join(&package);
        if let Some(resolved) = resolve_package(&candidate, subpath.as_deref()) {
            return Ok(resolved);
        }
        directory = current.parent().map(|parent| parent.to_path_buf());
    }
    Err(format!(
        "Cannot find module '{specifier}' from '{}'. It is not a core module and no package with that \
         name is installed in a node_modules directory above it.",
        from_dir.display()
    ))
}

/// `@scope/name/sub/path` → (`@scope/name`, `Some("sub/path")`).
fn split_package(specifier: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = specifier.split('/').collect();
    if specifier.starts_with('@') && parts.len() >= 2 {
        let package = format!("{}/{}", parts[0], parts[1]);
        let subpath = if parts.len() > 2 {
            Some(parts[2..].join("/"))
        } else {
            None
        };
        (package, subpath)
    } else {
        let package = parts[0].to_string();
        let subpath = if parts.len() > 1 {
            Some(parts[1..].join("/"))
        } else {
            None
        };
        (package, subpath)
    }
}

/// Resolve inside one `node_modules/<package>` directory.
fn resolve_package(root: &Path, subpath: Option<&str>) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    if let Some(subpath) = subpath {
        // `exports` decides where a subpath lives when the package declares it.
        if let Some(mapped) = package_exports(root, Some(subpath)) {
            return resolve_as_path(&root.join(mapped));
        }
        return resolve_as_path(&root.join(subpath));
    }
    package_exports(root, None)
        .and_then(|mapped| resolve_as_path(&root.join(mapped)))
        .or_else(|| resolve_as_path(root))
}

/// The path a package's `exports` (or `main`) points at.
fn package_exports(root: &Path, subpath: Option<&str>) -> Option<String> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json = crate::builtins::json::parse(&text)?;
    let field = |name: &str| {
        json.as_object()?
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let exports = field("exports");
    let key = match subpath {
        Some(subpath) => format!("./{subpath}"),
        None => ".".to_string(),
    };
    if let Some(exports) = exports {
        match &exports {
            crate::builtins::json::Raw::Str(target) => return Some(target.clone()),
            crate::builtins::json::Raw::Object(entries) => {
                // A conditions object (`{"require": ..., "default": ...}`) is
                // distinguishable from a subpath map by its keys.
                let is_subpath_map = entries.iter().any(|(name, _)| name.starts_with('.'));
                if is_subpath_map {
                    if let Some((_, value)) = entries.iter().find(|(name, _)| *name == key) {
                        return pick_target(value);
                    }
                } else if key == "." {
                    return pick_target(&exports);
                }
            }
            _ => {}
        }
    }
    field("main").and_then(|main| match main {
        crate::builtins::json::Raw::Str(target) => Some(target),
        _ => None,
    })
}

/// Choose a target out of the `require` / `default` conditions.
fn pick_target(value: &crate::builtins::json::Raw) -> Option<String> {
    use crate::builtins::json::Raw;
    match value {
        Raw::Str(target) => Some(target.clone()),
        Raw::Object(entries) => {
            for condition in ["require", "node", "default"] {
                if let Some((_, nested)) = entries.iter().find(|(name, _)| name == condition) {
                    if let Some(target) = pick_target(nested) {
                        return Some(target);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Apply the extension list and the directory rules to a candidate path.
fn resolve_as_path(base: &Path) -> Option<PathBuf> {
    for extension in EXTENSIONS {
        let candidate = if extension.is_empty() {
            base.to_path_buf()
        } else {
            PathBuf::from(format!("{}{extension}", base.display()))
        };
        if candidate.is_file() {
            return Some(normalise(&candidate));
        }
    }
    if base.is_dir() {
        if let Some(mapped) = package_exports(base, None) {
            if let Some(resolved) = resolve_as_path(&base.join(mapped)) {
                return Some(resolved);
            }
        }
        for extension in [".js", ".cjs", ".ts", ".cts", ".json"] {
            let candidate = base.join(format!("index{extension}"));
            if candidate.is_file() {
                return Some(normalise(&candidate));
            }
        }
    }
    None
}

/// Make a path absolute and free of `..`, so the cache has one key per file.
fn normalise(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Read `require`'s first argument, which must be a string.
fn arg(interp: &mut Interp, args: &[Value], index: usize) -> Result<String, Control> {
    match args.get(index) {
        Some(value) => interp.to_string(value),
        None => interp.type_error("require needs a module specifier"),
    }
}
