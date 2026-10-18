//! The Node.js core modules: `path`, `fs`, `os`, `events`, `util`, `assert`,
//! `querystring`, `buffer`, `timers`, `module` and `process`.
//!
//! These are built on demand and handed to `require` / `import`. They are a
//! deliberate subset: every implemented function behaves like Node's, and every
//! gap is listed in the crate documentation rather than silently returning
//! something wrong. Two structural gaps are worth knowing here:
//!
//! * There are no typed arrays, so `Buffer` is a plain object with numeric
//!   properties. Indexing, `length`, `toString`, `slice` and `equals` work;
//!   `readUInt32LE`-style binary codecs do not exist yet.
//! * `vm`, `child_process`, `worker_threads`, `net`, `http`, `zlib` and `crypto`
//!   are not implemented.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::interp::{Control, Interp};
use crate::value::{Kind, Object, ObjectRef, PromiseState, Value};

/// The core module names `require` and `import` accept, with or without the
/// `node:` prefix.
pub const NAMES: [&str; 13] = [
    "assert",
    "buffer",
    "events",
    "fs",
    "module",
    "os",
    "path",
    "process",
    "querystring",
    "timers",
    "url",
    "util",
    "worker_threads",
];

/// True when `name` (with or without the `node:` prefix) is a core module.
pub fn is_core(name: &str) -> bool {
    let bare = name.strip_prefix("node:").unwrap_or(name);
    NAMES.contains(&bare)
}

/// Build the core module `name`, or `None` when it is not one.
///
/// `worker_threads` is recognised but not implemented: it resolves so that a
/// package probing for it gets a Node-shaped failure instead of "module not
/// found", and any use of it throws.
pub fn resolve(interp: &mut Interp, name: &str) -> Option<Value> {
    let bare = name.strip_prefix("node:").unwrap_or(name);
    Some(match bare {
        "path" => path_module(interp),
        "fs" => fs_module(interp),
        "os" => os_module(interp),
        "events" => events_module(interp),
        "util" => util_module(interp),
        "assert" => assert_module(interp),
        "querystring" => querystring_module(interp),
        "url" => url_module(interp),
        "buffer" => buffer_module(interp),
        "timers" => timers_module(interp),
        "module" => module_module(interp),
        "process" => process_module(interp),
        "worker_threads" => unimplemented_module(interp, "worker_threads"),
        _ => return None,
    })
}

fn unimplemented_module(interp: &mut Interp, name: &str) -> Value {
    let module = interp.new_object();
    let message = format!("the `{name}` core module is not implemented by codets");
    set(
        &module,
        "__codets_unimplemented",
        Value::str(message.clone()),
    );
    Value::Object(module)
}

// ------------------------------------------------------------------- helpers

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn set(object: &ObjectRef, key: &str, value: Value) {
    object.borrow_mut().props.set(key, value);
}

/// Install a method, non-enumerable like every other built-in.
fn def<F>(interp: &Interp, target: &ObjectRef, name: &str, length: usize, body: F)
where
    F: Fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control> + 'static,
{
    let value = interp.native(name, length, body);
    let mut borrowed = target.borrow_mut();
    borrowed.props.set(name, value);
    borrowed.hide_from_enumeration(name);
}

/// Read a global that Node defines, such as `process` or `setTimeout`.
fn global(interp: &mut Interp, name: &str) -> Value {
    interp
        .globals
        .resolve(name)
        .map(|(_, binding)| binding.value)
        .unwrap_or(Value::Undefined)
}

/// Turn an `io::Error` into the shape Node uses: `CODE: message, syscall 'path'`.
fn io_error(interp: &Interp, error: &std::io::Error, syscall: &str, path: &str) -> Control {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        std::io::ErrorKind::InvalidInput => "EINVAL",
        _ => "EIO",
    };
    interp.throw_error("Error", format!("{code}: {error}, {syscall} '{path}'"))
}

// ---------------------------------------------------------------------- path

/// Node's path separator for the host platform.
const SEP: char = std::path::MAIN_SEPARATOR;

fn is_absolute_text(text: &str) -> bool {
    if cfg!(windows) {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
            return true;
        }
        text.starts_with("\\\\") || text.starts_with("//")
    } else {
        text.starts_with('/')
    }
}

/// Split a Windows drive prefix off, so normalisation can keep it.
fn split_drive(text: &str) -> (String, String) {
    if cfg!(windows) {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
            let rest: String = chars[2..].iter().collect();
            return (format!("{}:", chars[0]), rest);
        }
    }
    (String::new(), text.to_string())
}

/// `path.normalize`, including `..` collapsing, which `std::path` does not do.
fn normalize_text(text: &str) -> String {
    if text.is_empty() {
        return ".".to_string();
    }
    let (drive, rest) = split_drive(text);
    let absolute = is_absolute_text(text) || (!drive.is_empty() && rest.starts_with(['/', '\\']));
    let mut stack: Vec<&str> = Vec::new();
    for part in rest.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => match stack.last() {
                Some(last) if *last != ".." => {
                    stack.pop();
                }
                _ => {
                    if !absolute {
                        stack.push("..");
                    }
                }
            },
            other => stack.push(other),
        }
    }
    let joined = stack.join(&SEP.to_string());
    let mut result = drive;
    if absolute {
        result.push(SEP);
    }
    result.push_str(&joined);
    if result.is_empty() {
        ".".to_string()
    } else {
        result
    }
}

/// The path segments of a normalised path, for `relative`.
fn segments(text: &str) -> Vec<String> {
    text.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| part.to_string())
        .collect()
}

fn relative_text(from: &str, to: &str) -> String {
    let from = normalize_text(from);
    let to = normalize_text(to);
    if from == to {
        return String::new();
    }
    let from_parts = segments(&from);
    let to_parts = segments(&to);
    let mut common = 0;
    while common < from_parts.len() && common < to_parts.len() && from_parts[common] == to_parts[common] {
        common += 1;
    }
    let mut out: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        out.push("..".to_string());
    }
    out.extend(to_parts[common..].iter().cloned());
    out.join(&SEP.to_string())
}

fn path_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();
    let text = |args: &[Value], index: usize| -> String {
        match args.get(index) {
            Some(Value::Str(value)) => value.to_string(),
            Some(Value::Undefined) | None => String::new(),
            Some(other) => other.to_display_string(),
        }
    };

    def(interp, &module, "join", 0, move |_interp, _this, args| {
        let separator = SEP.to_string();
        let combined: Vec<String> = args
            .iter()
            .filter_map(|value| match value {
                Value::Str(text) if !text.is_empty() => Some(text.to_string()),
                _ => None,
            })
            .collect();
        Ok(Value::str(normalize_text(&combined.join(&separator))))
    });
    def(interp, &module, "normalize", 1, move |_interp, _this, args| {
        Ok(Value::str(normalize_text(&text(args, 0))))
    });
    def(interp, &module, "resolve", 0, move |_interp, _this, args| {
        let separator = SEP.to_string();
        let mut resolved = String::new();
        for value in args.iter().rev() {
            let part = match value {
                Value::Str(text) => text.to_string(),
                _ => continue,
            };
            if part.is_empty() {
                continue;
            }
            resolved = if resolved.is_empty() {
                part.clone()
            } else {
                format!("{part}{separator}{resolved}")
            };
            if is_absolute_text(&part) {
                break;
            }
        }
        if !is_absolute_text(&resolved) {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            resolved = format!("{}{separator}{resolved}", cwd.to_string_lossy());
        }
        Ok(Value::str(normalize_text(&resolved)))
    });
    def(interp, &module, "isAbsolute", 1, move |_interp, _this, args| {
        Ok(Value::Bool(is_absolute_text(&text(args, 0))))
    });
    def(interp, &module, "dirname", 1, move |_interp, _this, args| {
        let value = normalize_text(&text(args, 0));
        let parent = Path::new(&value)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(Value::str(if parent.is_empty() && !is_absolute_text(&value) {
            ".".to_string()
        } else {
            parent
        }))
    });
    def(interp, &module, "basename", 2, move |_interp, _this, args| {
        let value = normalize_text(&text(args, 0));
        let mut base = Path::new(&value)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if base.is_empty() {
            base = value.clone();
        }
        if let Some(Value::Str(strip)) = args.get(1) {
            if !strip.is_empty() && base.ends_with(strip.as_ref()) && base != strip.as_ref() {
                base.truncate(base.len() - strip.len());
            }
        }
        Ok(Value::str(base))
    });
    def(interp, &module, "extname", 1, move |_interp, _this, args| {
        let value = text(args, 0);
        let base = Path::new(&value)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        // A leading dot starts a hidden file, not an extension, which is why
        // `.gitignore` has no extension.
        let trimmed_start = if let Some(rest) = base.strip_prefix('.') {
            rest.to_string()
        } else {
            base.clone()
        };
        Ok(Value::str(match trimmed_start.rfind('.') {
            Some(index) => trimmed_start[index..].to_string(),
            None => String::new(),
        }))
    });
    def(interp, &module, "relative", 2, move |_interp, _this, args| {
        Ok(Value::str(relative_text(&text(args, 0), &text(args, 1))))
    });
    def(interp, &module, "parse", 1, move |interp, _this, args| {
        let value = text(args, 0);
        let normalized = normalize_text(&value);
        let base = Path::new(&normalized)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let extension = Path::new(&base)
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        let name = if extension.is_empty() {
            base.clone()
        } else {
            base[..base.len() - extension.len()].to_string()
        };
        let directory = Path::new(&normalized)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default();
        let parsed = interp.new_object();
        set(
            &parsed,
            "root",
            Value::str(if is_absolute_text(&normalized) {
                SEP.to_string()
            } else {
                String::new()
            }),
        );
        set(&parsed, "dir", Value::str(directory));
        set(&parsed, "base", Value::str(base));
        set(&parsed, "ext", Value::str(extension));
        set(&parsed, "name", Value::str(name));
        Ok(Value::Object(parsed))
    });
    def(interp, &module, "format", 1, move |_interp, _this, args| {
        let Some(Value::Object(path)) = args.first().cloned() else {
            return Ok(Value::str(""));
        };
        let read = |key: &str| {
            path.borrow()
                .props
                .get(key)
                .map(|value| value.to_display_string())
                .unwrap_or_default()
        };
        let directory = read("dir");
        let base = read("base");
        let mut text = String::new();
        if !directory.is_empty() {
            text.push_str(&directory);
            if !base.is_empty() {
                text.push(SEP);
            }
        }
        text.push_str(&base);
        Ok(Value::str(text))
    });
    def(interp, &module, "toNamespacedPath", 1, move |_interp, _this, args| {
        Ok(Value::str(text(args, 0)))
    });

    set(&module, "sep", Value::str(SEP.to_string()));
    set(
        &module,
        "delimiter",
        Value::str(if cfg!(windows) { ";" } else { ":" }),
    );
    // `path.posix` and `path.win32` are the same object here, because the module
    // follows the host's separator rules rather than emulating the other family.
    set(&module, "posix", Value::Object(module.clone()));
    set(&module, "win32", Value::Object(module.clone()));
    Value::Object(module)
}

// ------------------------------------------------------------------------ fs

fn fs_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();

    def(interp, &module, "readFileSync", 2, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Value::str(String::from_utf8_lossy(&bytes).to_string())),
            Err(error) => Err(io_error(interp, &error, "open", &path)),
        }
    });
    def(interp, &module, "writeFileSync", 3, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        let data = interp.to_string(&arg(args, 1))?;
        match std::fs::write(&path, data) {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "open", &path)),
        }
    });
    def(interp, &module, "appendFileSync", 3, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        let data = interp.to_string(&arg(args, 1))?;
        let outcome = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(data.as_bytes()));
        match outcome {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "open", &path)),
        }
    });
    def(interp, &module, "existsSync", 1, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        Ok(Value::Bool(Path::new(&path).exists()))
    });
    def(interp, &module, "mkdirSync", 2, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        let recursive = match args.get(1) {
            Some(Value::Object(options)) => options
                .borrow()
                .props
                .get("recursive")
                .map(|value| value.truthy())
                .unwrap_or(false),
            Some(value) => value.truthy(),
            None => false,
        };
        let outcome = if recursive {
            std::fs::create_dir_all(&path)
        } else {
            std::fs::create_dir(&path)
        };
        match outcome {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "mkdir", &path)),
        }
    });
    def(interp, &module, "readdirSync", 2, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect();
                names.sort();
                Ok(interp.new_string_array(names))
            }
            Err(error) => Err(io_error(interp, &error, "scandir", &path)),
        }
    });
    def(interp, &module, "statSync", 2, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                let stat = interp.new_object();
                let is_directory = metadata.is_dir();
                set(&stat, "size", Value::Num(metadata.len() as f64));
                let methods = [
                    ("isFile", !is_directory),
                    ("isDirectory", is_directory),
                    ("isSymbolicLink", metadata.file_type().is_symlink()),
                ];
                for (name, outcome) in methods {
                    let method = interp.native(name, 0, move |_interp, _this, _args| {
                        Ok(Value::Bool(outcome))
                    });
                    let mut borrowed = stat.borrow_mut();
                    borrowed.props.set(name, method);
                    borrowed.hide_from_enumeration(name);
                }
                Ok(Value::Object(stat))
            }
            Err(error) => Err(io_error(interp, &error, "stat", &path)),
        }
    });
    def(interp, &module, "lstatSync", 2, |interp, this, args| {
        // `lstatSync` differs only for symlinks; the metadata above is close
        // enough that delegating keeps one implementation.
        let read = interp.get_prop(this, "statSync")?;
        interp.call_value(&read, this.clone(), args.to_vec())
    });
    def(interp, &module, "unlinkSync", 1, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "unlink", &path)),
        }
    });
    def(interp, &module, "rmSync", 2, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        let target = Path::new(&path);
        let outcome = if target.is_dir() {
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        match outcome {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "rm", &path)),
        }
    });
    def(interp, &module, "rmdirSync", 1, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        match std::fs::remove_dir(&path) {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "rmdir", &path)),
        }
    });
    def(interp, &module, "renameSync", 2, |interp, _this, args| {
        let from = interp.to_string(&arg(args, 0))?;
        let to = interp.to_string(&arg(args, 1))?;
        match std::fs::rename(&from, &to) {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "rename", &from)),
        }
    });
    def(interp, &module, "copyFileSync", 3, |interp, _this, args| {
        let from = interp.to_string(&arg(args, 0))?;
        let to = interp.to_string(&arg(args, 1))?;
        match std::fs::copy(&from, &to) {
            Ok(_) => Ok(Value::Undefined),
            Err(error) => Err(io_error(interp, &error, "copyfile", &from)),
        }
    });
    def(interp, &module, "realpathSync", 2, |interp, _this, args| {
        let path = interp.to_string(&arg(args, 0))?;
        match std::fs::canonicalize(&path) {
            Ok(resolved) => Ok(Value::str(resolved.to_string_lossy().to_string())),
            Err(error) => Err(io_error(interp, &error, "realpath", &path)),
        }
    });

    let constants = interp.new_object();
    for (name, value) in [
        ("F_OK", 0.0),
        ("R_OK", 4.0),
        ("W_OK", 2.0),
        ("X_OK", 1.0),
    ] {
        set(&constants, name, Value::Num(value));
    }
    set(&module, "constants", Value::Object(constants));
    Value::Object(module)
}

// ------------------------------------------------------------------------ os

fn os_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();
    def(interp, &module, "platform", 0, |_interp, _this, _args| {
        Ok(Value::str(match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            other => other,
        }))
    });
    def(interp, &module, "arch", 0, |_interp, _this, _args| {
        Ok(Value::str(match std::env::consts::ARCH {
            "x86_64" => "x64",
            "x86" => "ia32",
            "aarch64" => "arm64",
            other => other,
        }))
    });
    def(interp, &module, "homedir", 0, |_interp, _this, _args| {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        Ok(Value::str(home))
    });
    def(interp, &module, "tmpdir", 0, |_interp, _this, _args| {
        Ok(Value::str(
            std::env::temp_dir().to_string_lossy().to_string(),
        ))
    });
    def(interp, &module, "hostname", 0, |_interp, _this, _args| {
        Ok(Value::str(
            std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "localhost".to_string()),
        ))
    });
    def(interp, &module, "type", 0, |_interp, _this, _args| {
        Ok(Value::str(match std::env::consts::OS {
            "windows" => "Windows_NT",
            "macos" => "Darwin",
            "linux" => "Linux",
            other => other,
        }))
    });
    def(interp, &module, "release", 0, |_interp, _this, _args| {
        Ok(Value::str(""))
    });
    def(interp, &module, "endianness", 0, |_interp, _this, _args| {
        Ok(Value::str(if cfg!(target_endian = "little") { "LE" } else { "BE" }))
    });
    def(interp, &module, "cpus", 0, |interp, _this, _args| {
        let count = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        let mut cpus = Vec::new();
        for _ in 0..count {
            let cpu = interp.new_object();
            set(&cpu, "model", Value::str(""));
            set(&cpu, "speed", Value::Num(0.0));
            cpus.push(Value::Object(cpu));
        }
        Ok(interp.new_array(cpus))
    });
    // Memory reporting is not implemented, which is documented rather than
    // guessed at.
    def(interp, &module, "totalmem", 0, |_interp, _this, _args| {
        Ok(Value::Num(0.0))
    });
    def(interp, &module, "freemem", 0, |_interp, _this, _args| {
        Ok(Value::Num(0.0))
    });
    def(interp, &module, "loadavg", 0, |interp, _this, _args| {
        Ok(interp.new_array(vec![
            Value::Num(0.0),
            Value::Num(0.0),
            Value::Num(0.0),
        ]))
    });
    set(
        &module,
        "EOL",
        Value::str(if cfg!(windows) { "\r\n" } else { "\n" }),
    );
    set(&module, "devNull", Value::str(if cfg!(windows) { "\\\\.\\nul" } else { "/dev/null" }));
    Value::Object(module)
}

// -------------------------------------------------------------------- events

/// The `$listeners` map on an emitter instance: event name -> listener array.
fn listeners_of(interp: &mut Interp, this: &Value) -> Result<ObjectRef, Control> {
    let Value::Object(object) = this else {
        return interp.type_error("EventEmitter method called on a non-object");
    };
    let existing = object.borrow().props.get("$listeners").cloned();
    match existing {
        Some(Value::Object(map)) => Ok(map),
        _ => {
            let map = interp.new_object();
            object
                .borrow_mut()
                .props
                .set("$listeners", Value::Object(map.clone()));
            Ok(map)
        }
    }
}

fn events_module(interp: &mut Interp) -> Value {
    let prototype = interp.new_object();

    def(interp, &prototype, "on", 2, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let listener = arg(args, 1);
        let map = listeners_of(interp, this)?;
        let existing = map.borrow().props.get(&event).cloned();
        let mut entries = match existing {
            Some(Value::Object(list)) => interp.array_items(&Value::Object(list))?,
            _ => Vec::new(),
        };
        entries.push(listener);
        map.borrow_mut()
            .props
            .set(event, interp.new_array(entries));
        Ok(this.clone())
    });
    def(interp, &prototype, "addListener", 2, |interp, this, args| {
        let on = interp.get_prop(this, "on")?;
        interp.call_value(&on, this.clone(), args.to_vec())
    });
    def(interp, &prototype, "prependListener", 2, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let listener = arg(args, 1);
        let map = listeners_of(interp, this)?;
        let existing = map.borrow().props.get(&event).cloned();
        let mut entries = match existing {
            Some(Value::Object(list)) => interp.array_items(&Value::Object(list))?,
            _ => Vec::new(),
        };
        entries.insert(0, listener);
        map.borrow_mut().props.set(event, interp.new_array(entries));
        Ok(this.clone())
    });
    def(interp, &prototype, "once", 2, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let listener = arg(args, 1);
        // The wrapper removes itself before delegating, which is what makes a
        // `once` listener fire exactly once even when `emit` re-enters.
        //
        // Known deviation: because `$listeners` holds the wrapper rather than the
        // function passed in, `removeListener(event, original)` cannot find it and
        // `listeners()` returns the wrapper. Removing by the wrapper works.
        let slot: std::rc::Rc<std::cell::RefCell<Value>> =
            std::rc::Rc::new(std::cell::RefCell::new(Value::Undefined));
        let wrapper = interp.native("onceListener", 0, {
            let this = this.clone();
            let event = event.clone();
            let listener = listener.clone();
            let slot = slot.clone();
            move |interp, _this, call_args| {
                let itself = slot.borrow().clone();
                let map = listeners_of(interp, &this)?;
                // The lookup result is bound first: holding a `borrow()` across
                // the `borrow_mut()` below would panic at run time.
                let existing = map.borrow().props.get(&event).cloned();
                if let Some(Value::Object(list)) = existing {
                    let entries = interp.array_items(&Value::Object(list))?;
                    let kept: Vec<Value> = entries
                        .into_iter()
                        .filter(|entry| !entry.strict_equals(&itself))
                        .collect();
                    map.borrow_mut().props.set(event.clone(), interp.new_array(kept));
                }
                interp.call_value(&listener, Value::Undefined, call_args.to_vec())
            }
        });
        *slot.borrow_mut() = wrapper.clone();
        let on = interp.get_prop(this, "on")?;
        interp.call_value(&on, this.clone(), vec![Value::str(&event), wrapper])
    });
    def(interp, &prototype, "emit", 1, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let rest: Vec<Value> = args.iter().skip(1).cloned().collect();
        let map = listeners_of(interp, this)?;
        let listeners = match map.borrow().props.get(&event).cloned() {
            Some(Value::Object(list)) => interp.array_items(&Value::Object(list))?,
            _ => Vec::new(),
        };
        if listeners.is_empty() {
            // Node only throws for `error`, and only when nothing listens.
            if event == "error" {
                let message = rest
                    .first()
                    .map(|value| value.to_display_string())
                    .unwrap_or_else(|| "Unhandled error".to_string());
                return Err(interp.throw_error(
                    "Error",
                    format!("Unhandled 'error' event: {message}"),
                ));
            }
            return Ok(Value::Bool(false));
        }
        for listener in listeners {
            interp.call_value(&listener, this.clone(), rest.clone())?;
        }
        Ok(Value::Bool(true))
    });
    def(interp, &prototype, "removeListener", 2, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let listener = arg(args, 1);
        let map = listeners_of(interp, this)?;
        let existing = map.borrow().props.get(&event).cloned();
        if let Some(Value::Object(list)) = existing {
            let entries = interp.array_items(&Value::Object(list))?;
            let kept: Vec<Value> = entries
                .into_iter()
                .filter(|entry| !entry.strict_equals(&listener))
                .collect();
            map.borrow_mut().props.set(event, interp.new_array(kept));
        }
        Ok(this.clone())
    });
    def(interp, &prototype, "off", 2, |interp, this, args| {
        let remove = interp.get_prop(this, "removeListener")?;
        interp.call_value(&remove, this.clone(), args.to_vec())
    });
    def(interp, &prototype, "removeAllListeners", 1, |interp, this, args| {
        let map = listeners_of(interp, this)?;
        match args.first() {
            None | Some(Value::Undefined) => {
                map.borrow_mut().props.clear();
            }
            Some(value) => {
                let event = interp.to_string(value)?;
                map.borrow_mut().props.remove(&event);
            }
        }
        Ok(this.clone())
    });
    def(interp, &prototype, "listeners", 1, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let map = listeners_of(interp, this)?;
        let existing = map.borrow().props.get(&event).cloned();
        Ok(match existing {
            Some(Value::Object(list)) => Value::Object(list),
            _ => interp.new_array(Vec::new()),
        })
    });
    def(interp, &prototype, "listenerCount", 1, |interp, this, args| {
        let event = interp.to_string(&arg(args, 0))?;
        let map = listeners_of(interp, this)?;
        let count = match map.borrow().props.get(&event).cloned() {
            Some(Value::Object(list)) => interp.array_items(&Value::Object(list))?.len(),
            _ => 0,
        };
        Ok(Value::Num(count as f64))
    });
    def(interp, &prototype, "eventNames", 0, |interp, this, _args| {
        let map = listeners_of(interp, this)?;
        let names: Vec<String> = map
            .borrow()
            .props
            .keys()
            .into_iter()
            .map(|name| name.to_string())
            .collect();
        Ok(interp.new_string_array(names))
    });
    def(interp, &prototype, "setMaxListeners", 1, |_interp, this, _args| {
        Ok(this.clone())
    });
    def(interp, &prototype, "getMaxListeners", 0, |_interp, _this, _args| {
        Ok(Value::Num(10.0))
    });

    let ctor = interp.native("EventEmitter", 0, {
        let prototype = prototype.clone();
        move |interp, this, _args| {
            let target = match this {
                Value::Object(object) => object.clone(),
                _ => Object::new(Some(prototype.clone())),
            };
            target
                .borrow_mut()
                .props
                .set("$listeners", Value::Object(interp.new_object()));
            Ok(Value::Object(target))
        }
    });
    if let Value::Object(object) = &ctor {
        set(object, "prototype", Value::Object(prototype.clone()));
        object.borrow_mut().hide_from_enumeration("prototype");
    }

    // `events` *is* the EventEmitter constructor, with the module-level helpers
    // hung off it.
    let module = match &ctor {
        Value::Object(object) => object.clone(),
        _ => interp.new_object(),
    };
    // `events.once(emitter, name)` resolves with the event's arguments, which is
    // the promise form of "wait for this once".
    def(interp, &module, "once", 2, |interp, _this, args| {
        let emitter = arg(args, 0);
        let event = interp.to_string(&arg(args, 1))?;
        let promise = interp.new_promise(std::rc::Rc::new(PromiseState::new()));
        let listener = interp.native("onceListener", 0, {
            let promise = promise.clone();
            move |interp, _this, call_args| {
                let value = match call_args.len() {
                    0 => Value::Undefined,
                    1 => call_args[0].clone(),
                    _ => interp.new_array(call_args.to_vec()),
                };
                interp.fulfill(&promise, value);
                Ok(Value::Undefined)
            }
        });
        let on = interp.get_prop(&emitter, "on")?;
        interp.call_value(&on, emitter.clone(), vec![Value::str(&event), listener])?;
        Ok(promise)
    });
    Value::Object(module)
}

// ---------------------------------------------------------------------- util

fn util_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();

    def(interp, &module, "format", 1, |interp, _this, args| {
        let template = interp.to_string(&arg(args, 0))?;
        let mut out = String::new();
        let mut next = 1;
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('s') => {
                    if let Some(value) = args.get(next) {
                        out.push_str(&value.to_display_string());
                        next += 1;
                    }
                }
                Some('d') | Some('i') => {
                    if let Some(value) = args.get(next) {
                        out.push_str(&crate::value::format_number(value.to_number()));
                        next += 1;
                    }
                }
                Some('f') => {
                    if let Some(value) = args.get(next) {
                        out.push_str(&crate::value::format_number(value.to_number()));
                        next += 1;
                    }
                }
                Some('j') => {
                    if let Some(value) = args.get(next) {
                        out.push_str(&crate::builtins::inspect(value));
                        next += 1;
                    }
                }
                Some('o') | Some('O') => {
                    if let Some(value) = args.get(next) {
                        out.push_str(&crate::builtins::inspect(value));
                        next += 1;
                    }
                }
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        for value in args.iter().skip(next) {
            out.push(' ');
            out.push_str(&crate::builtins::inspect(value));
        }
        Ok(Value::str(out))
    });
    def(interp, &module, "inspect", 1, |_interp, _this, args| {
        Ok(Value::str(crate::builtins::inspect(&arg(args, 0))))
    });
    def(interp, &module, "formatWithOptions", 1, |interp, this, args| {
        let format = interp.get_prop(this, "format")?;
        let rest: Vec<Value> = args.iter().skip(1).cloned().collect();
        interp.call_value(&format, Value::Undefined, rest)
    });
    def(interp, &module, "deprecate", 2, |_interp, _this, args| {
        // The function is returned unchanged: warning plumbing needs a
        // `process.emitWarning` stream that codets does not implement.
        Ok(arg(args, 0))
    });
    def(interp, &module, "promisify", 1, |interp, _this, args| {
        let target = arg(args, 0);
        Ok(interp.native("promisified", 0, move |interp, _this, call_args| {
            let promise = interp.new_promise(std::rc::Rc::new(PromiseState::new()));
            // Node calls back as `(error, value)`.
            let callback = interp.native("callback", 2, {
                let promise = promise.clone();
                move |interp, _this, args| {
                    match args.first().cloned().unwrap_or(Value::Null) {
                        Value::Null | Value::Undefined => {
                            interp.fulfill(&promise, arg(args, 1));
                        }
                        error => interp.reject(&promise, error),
                    }
                    Ok(Value::Undefined)
                }
            });
            let mut forwarded = call_args.to_vec();
            forwarded.push(callback);
            if let Err(Control::Throw(thrown)) =
                interp.call_value(&target, Value::Undefined, forwarded)
            {
                // A synchronous throw still rejects, which is what Node does.
                let reason = thrown.value.clone();
                interp.reject(&promise, reason);
            }
            Ok(promise)
        }))
    });
    def(interp, &module, "inherits", 2, |interp, _this, args| {
        let (Value::Object(child), Value::Object(parent)) = (arg(args, 0), arg(args, 1)) else {
            return interp.type_error("util.inherits expects two constructors");
        };
        let parent_prototype = match parent.borrow().props.get("prototype") {
            Some(Value::Object(prototype)) => prototype.clone(),
            _ => interp.prototypes.object.clone(),
        };
        let child_prototype = Object::new(Some(parent_prototype));
        child_prototype
            .borrow_mut()
            .props
            .set("constructor", Value::Object(child.clone()));
        child
            .borrow_mut()
            .props
            .set("prototype", Value::Object(child_prototype));
        Ok(Value::Undefined)
    });

    let types = interp.new_object();
    let checks: [(&str, fn(&Value) -> bool); 11] = [
        ("isDate", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::Date(_)))),
        ("isRegExp", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::RegExp(_)))),
        ("isArray", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::Array))),
        ("isMap", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::Map(_)))),
        ("isSet", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::Set(_)))),
        ("isPromise", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::Promise(_)))),
        ("isError", |value| matches!(value, Value::Object(o) if matches!(o.borrow().kind, Kind::Error))),
        ("isFunction", |value| matches!(value, Value::Object(o) if o.borrow().callable.is_some())),
        ("isString", |value| matches!(value, Value::Str(_))),
        ("isNumber", |value| matches!(value, Value::Num(_))),
        ("isBoolean", |value| matches!(value, Value::Bool(_))),
    ];
    for (name, check) in checks {
        def(interp, &types, name, 1, move |_interp, _this, args| {
            Ok(Value::Bool(check(&arg(args, 0))))
        });
    }
    def(interp, &types, "isNull", 1, |_interp, _this, args| {
        Ok(Value::Bool(matches!(arg(args, 0), Value::Null)))
    });
    def(interp, &types, "isUndefined", 1, |_interp, _this, args| {
        Ok(Value::Bool(matches!(arg(args, 0), Value::Undefined)))
    });
    def(interp, &types, "isObject", 1, |_interp, _this, args| {
        Ok(Value::Bool(matches!(arg(args, 0), Value::Object(_))))
    });
    set(&module, "types", Value::Object(types));
    Value::Object(module)
}

// -------------------------------------------------------------------- assert

/// Structural equality, which `assert.deepStrictEqual` needs.
fn deep_equal(left: &Value, right: &Value, depth: usize) -> bool {
    if depth > 64 {
        return true;
    }
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let (kind_a, kind_b) = (a.borrow().kind_name(), b.borrow().kind_name());
            if kind_a != kind_b {
                return false;
            }
            let (left_items, right_items) = {
                let a = a.borrow();
                let b = b.borrow();
                (
                    a.props
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<Vec<_>>(),
                    b.props
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<Vec<_>>(),
                )
            };
            if left_items.len() != right_items.len() {
                return false;
            }
            left_items.iter().all(|(key, value)| {
                right_items
                    .iter()
                    .find(|(other, _)| other == key)
                    .map(|(_, other)| deep_equal(value, other, depth + 1))
                    .unwrap_or(false)
            })
        }
        _ => left.strict_equals(right),
    }
}

fn assert_module(interp: &mut Interp) -> Value {
    let module = interp.native("assert", 2, |interp, _this, args| {
        let value = arg(args, 0);
        if !value.truthy() {
            let message = match args.get(1) {
                Some(value) => value.to_display_string(),
                None => format!("{} == true", crate::builtins::inspect(&value)),
            };
            return Err(interp.throw_error("AssertionError", message));
        }
        Ok(Value::Undefined)
    });

    let target = match &module {
        Value::Object(object) => object.clone(),
        _ => interp.new_object(),
    };

    def(interp, &target, "ok", 2, |interp, _this, args| {
        let value = arg(args, 0);
        if !value.truthy() {
            return Err(interp.throw_error(
                "AssertionError",
                format!("The expression evaluated to a falsy value: {} == true", crate::builtins::inspect(&value)),
            ));
        }
        Ok(Value::Undefined)
    });
    def(interp, &target, "equal", 3, |interp, _this, args| {
        let (left, right) = (arg(args, 0), arg(args, 1));
        if !left.loose_equals(&right) {
            return Err(interp.throw_error(
                "AssertionError",
                format!(
                    "{} == {}",
                    crate::builtins::inspect(&left),
                    crate::builtins::inspect(&right)
                ),
            ));
        }
        Ok(Value::Undefined)
    });
    def(interp, &target, "strictEqual", 3, |interp, _this, args| {
        let (left, right) = (arg(args, 0), arg(args, 1));
        if !left.strict_equals(&right) {
            return Err(interp.throw_error(
                "AssertionError",
                format!(
                    "{} === {}",
                    crate::builtins::inspect(&left),
                    crate::builtins::inspect(&right)
                ),
            ));
        }
        Ok(Value::Undefined)
    });
    def(interp, &target, "notStrictEqual", 3, |interp, _this, args| {
        let (left, right) = (arg(args, 0), arg(args, 1));
        if left.strict_equals(&right) {
            return Err(interp.throw_error(
                "AssertionError",
                "expected the two values to differ",
            ));
        }
        Ok(Value::Undefined)
    });
    def(interp, &target, "deepStrictEqual", 3, |interp, _this, args| {
        let (left, right) = (arg(args, 0), arg(args, 1));
        if !deep_equal(&left, &right, 0) {
            return Err(interp.throw_error(
                "AssertionError",
                format!(
                    "{} !== {}",
                    crate::builtins::inspect(&left),
                    crate::builtins::inspect(&right)
                ),
            ));
        }
        Ok(Value::Undefined)
    });
    def(interp, &target, "deepEqual", 3, |interp, this, args| {
        let compare = interp.get_prop(this, "deepStrictEqual")?;
        let outcome = interp.call_value(&compare, this.clone(), args.to_vec());
        match outcome {
            // A loose comparison falls back to `==` for the top level.
            Err(_) => {
                let equal = interp.get_prop(this, "equal")?;
                interp.call_value(&equal, this.clone(), args.to_vec())
            }
            Ok(value) => Ok(value),
        }
    });
    def(interp, &target, "throws", 2, |interp, _this, args| {
        let callback = arg(args, 0);
        match interp.call_value(&callback, Value::Undefined, Vec::new()) {
            Ok(_) => Err(interp.throw_error("AssertionError", "Missing expected exception")),
            Err(Control::Throw(thrown)) => Ok(thrown.value.clone()),
            Err(other) => Err(other),
        }
    });
    def(interp, &target, "doesNotThrow", 2, |interp, _this, args| {
        let callback = arg(args, 0);
        match interp.call_value(&callback, Value::Undefined, Vec::new()) {
            Ok(value) => Ok(value),
            Err(Control::Throw(thrown)) => Err(interp.throw_error(
                "AssertionError",
                format!("Got unwanted exception: {}", thrown.render()),
            )),
            Err(other) => Err(other),
        }
    });
    def(interp, &target, "fail", 1, |interp, _this, args| {
        let message = match args.first() {
            Some(value) if !value.is_nullish() => value.to_display_string(),
            _ => "Failed".to_string(),
        };
        Err(interp.throw_error("AssertionError", message))
    });
    def(interp, &target, "ifError", 1, |interp, _this, args| {
        let value = arg(args, 0);
        if !value.is_nullish() {
            if let Value::Object(object) = &value {
                if matches!(object.borrow().kind, Kind::Error) {
                    return Err(Control::Throw(crate::interp::JsError {
                        value,
                        frames: Vec::new(),
                    }));
                }
            }
            return Err(interp.throw_error("AssertionError", "ifError got unwanted exception"));
        }
        Ok(Value::Undefined)
    });
    set(
        &target,
        "AssertionError",
        interp.make_error("Error", "AssertionError"),
    );
    module
}

// ---------------------------------------------------------------- querystring

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*'
            | b'\'' | b'(' | b')' => out.push(byte as char),
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn querystring_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();
    def(interp, &module, "escape", 1, |interp, _this, args| {
        Ok(Value::str(percent_encode(&interp.to_string(&arg(args, 0))?)))
    });
    def(interp, &module, "unescape", 1, |interp, _this, args| {
        Ok(Value::str(percent_decode(&interp.to_string(&arg(args, 0))?)))
    });
    def(interp, &module, "stringify", 4, |interp, _this, args| {
        let Some(Value::Object(source)) = args.first().cloned() else {
            return Ok(Value::str(""));
        };
        let entries: Vec<(String, Value)> = source
            .borrow()
            .props
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut parts: Vec<String> = Vec::new();
        for (key, value) in entries {
            let key = percent_encode(&key);
            match value {
                Value::Object(list) if list.borrow().is_array() => {
                    for item in interp.array_items(&Value::Object(list))? {
                        parts.push(format!("{key}={}", percent_encode(&item.to_display_string())));
                    }
                }
                other => parts.push(format!("{key}={}", percent_encode(&other.to_display_string()))),
            }
        }
        Ok(Value::str(parts.join("&")))
    });
    def(interp, &module, "parse", 1, |interp, _this, args| {
        let text = interp.to_string(&arg(args, 0))?;
        let result = interp.new_object();
        for pair in text.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (percent_decode(key), percent_decode(value)),
                None => (percent_decode(pair), String::new()),
            };
            let existing = result.borrow().props.get(&key).cloned();
            match existing {
                Some(Value::Object(list)) if list.borrow().is_array() => {
                    let mut items = interp.array_items(&Value::Object(list))?;
                    items.push(Value::str(value));
                    let replacement = interp.new_array(items);
                    result.borrow_mut().props.set(key, replacement);
                }
                Some(previous) => {
                    let replacement = interp
                        .new_array(vec![previous, Value::str(value)]);
                    result.borrow_mut().props.set(key, replacement);
                }
                None => {
                    result.borrow_mut().props.set(key, Value::str(value));
                }
            }
        }
        Ok(Value::Object(result))
    });
    def(interp, &module, "decode", 1, |interp, this, args| {
        let parse = interp.get_prop(this, "parse")?;
        interp.call_value(&parse, this.clone(), args.to_vec())
    });
    def(interp, &module, "encode", 4, |interp, this, args| {
        let stringify = interp.get_prop(this, "stringify")?;
        interp.call_value(&stringify, this.clone(), args.to_vec())
    });
    Value::Object(module)
}

// ----------------------------------------------------------------------- url

fn url_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();
    // A URL here is a plain object with the parts spelled out, plus
    // `searchParams`. It is enough for code that reads `url.pathname`, and
    // documented as not implementing the WHATWG parser.
    def(interp, &module, "parse", 1, |interp, _this, args| {
        let text = interp.to_string(&arg(args, 0))?;
        let parsed = interp.new_object();
        let (scheme, rest) = match text.split_once("://") {
            Some((scheme, rest)) => (scheme.to_string(), rest.to_string()),
            None => (String::new(), text.clone()),
        };
        let (host, path_and_query) = match rest.find('/') {
            Some(index) => (rest[..index].to_string(), rest[index..].to_string()),
            None => (rest.clone(), "/".to_string()),
        };
        let (pathname, search) = match path_and_query.split_once('?') {
            Some((path, query)) => (path.to_string(), format!("?{query}")),
            None => (path_and_query.clone(), String::new()),
        };
        set(&parsed, "href", Value::str(text));
        set(&parsed, "protocol", Value::str(if scheme.is_empty() { String::new() } else { format!("{scheme}:") }));
        set(&parsed, "host", Value::str(host.clone()));
        set(&parsed, "hostname", Value::str(host));
        set(&parsed, "pathname", Value::str(pathname));
        set(&parsed, "search", Value::str(search));
        set(&parsed, "hash", Value::str(""));
        Ok(Value::Object(parsed))
    });
    set(
        &module,
        "__codets_note",
        Value::str("url.URL and url.URLSearchParams are not implemented; url.parse returns a plain object"),
    );
    Value::Object(module)
}

// -------------------------------------------------------------------- buffer

/// The marker that identifies a `codets` Buffer.
const BUFFER_MARKER: &str = "$codets_buffer";

fn new_buffer(_interp: &Interp, prototype: &ObjectRef, bytes: Vec<u8>) -> Value {
    let object = Object::new(Some(prototype.clone()));
    {
        let mut borrowed = object.borrow_mut();
        for (index, byte) in bytes.iter().enumerate() {
            borrowed.props.set(index.to_string(), Value::Num(*byte as f64));
        }
        borrowed.props.set("length", Value::Num(bytes.len() as f64));
        borrowed.props.set(BUFFER_MARKER, Value::Bool(true));
        borrowed.hide_from_enumeration("length");
        borrowed.hide_from_enumeration(BUFFER_MARKER);
    }
    Value::Object(object)
}

fn buffer_bytes(interp: &mut Interp, value: &Value) -> Result<Vec<u8>, Control> {
    match value {
        Value::Object(object) => {
            let is_buffer = object
                .borrow()
                .props
                .get(BUFFER_MARKER)
                .map(|marker| marker.truthy())
                .unwrap_or(false);
            if is_buffer {
                let length = object.borrow().array_length();
                let mut bytes = Vec::with_capacity(length);
                for index in 0..length {
                    let item = object
                        .borrow()
                        .props
                        .get(&index.to_string())
                        .cloned()
                        .unwrap_or(Value::Num(0.0));
                    bytes.push(item.to_number() as u8);
                }
                return Ok(bytes);
            }
            if object.borrow().is_array() {
                let items = interp.array_items(value)?;
                return Ok(items.iter().map(|item| item.to_number() as u8).collect());
            }
            interp.type_error("Buffer.from expects a string, array or Buffer")
        }
        Value::Str(text) => Ok(text.as_bytes().to_vec()),
        _ => interp.type_error("Buffer.from expects a string, array or Buffer"),
    }
}

fn buffer_module(interp: &mut Interp) -> Value {
    let prototype = interp.new_object();

    def(interp, &prototype, "toString", 3, |interp, this, _args| {
        let bytes = buffer_bytes(interp, this)?;
        Ok(Value::str(String::from_utf8_lossy(&bytes).to_string()))
    });
    def(interp, &prototype, "slice", 2, |interp, this, args| {
        let bytes = buffer_bytes(interp, this)?;
        let length = bytes.len() as i64;
        let mut start = arg(args, 0).to_number() as i64;
        let mut end = match args.get(1) {
            Some(value) if !value.is_nullish() => value.to_number() as i64,
            _ => length,
        };
        if start < 0 {
            start += length;
        }
        if end < 0 {
            end += length;
        }
        let start = start.clamp(0, length) as usize;
        let end = end.clamp(0, length) as usize;
        let prototype = this.object_prototype(interp);
        Ok(new_buffer(
            interp,
            &prototype,
            bytes[start.min(end)..end].to_vec(),
        ))
    });
    def(interp, &prototype, "subarray", 2, |interp, this, args| {
        let slice = interp.get_prop(this, "slice")?;
        interp.call_value(&slice, this.clone(), args.to_vec())
    });
    def(interp, &prototype, "equals", 1, |interp, this, args| {
        let left = buffer_bytes(interp, this)?;
        let right = buffer_bytes(interp, &arg(args, 0))?;
        Ok(Value::Bool(left == right))
    });
    def(interp, &prototype, "toJSON", 0, |interp, this, _args| {
        let bytes = buffer_bytes(interp, this)?;
        let data = interp.new_array(bytes.iter().map(|byte| Value::Num(*byte as f64)).collect());
        let result = interp.new_object();
        set(&result, "type", Value::str("Buffer"));
        set(&result, "data", data);
        Ok(Value::Object(result))
    });

    let ctor = interp.new_object();
    def(interp, &ctor, "from", 2, {
        let prototype = prototype.clone();
        move |interp, _this, args| {
            let bytes = match arg(args, 0) {
                Value::Str(text) => text.as_bytes().to_vec(),
                other => buffer_bytes(interp, &other)?,
            };
            Ok(new_buffer(interp, &prototype, bytes))
        }
    });
    def(interp, &ctor, "alloc", 3, {
        let prototype = prototype.clone();
        move |interp, _this, args| {
            let length = arg(args, 0).to_number().max(0.0) as usize;
            let fill = match args.get(1) {
                Some(value) => value.to_number() as u8,
                None => 0,
            };
            Ok(new_buffer(interp, &prototype, vec![fill; length]))
        }
    });
    def(interp, &ctor, "allocUnsafe", 1, {
        let prototype = prototype.clone();
        move |interp, _this, args| {
            let length = arg(args, 0).to_number().max(0.0) as usize;
            Ok(new_buffer(interp, &prototype, vec![0; length]))
        }
    });
    def(interp, &ctor, "concat", 2, {
        let prototype = prototype.clone();
        move |interp, _this, args| {
            let items = interp.array_items(&arg(args, 0))?;
            let mut bytes = Vec::new();
            for item in items {
                bytes.extend(buffer_bytes(interp, &item)?);
            }
            Ok(new_buffer(interp, &prototype, bytes))
        }
    });
    def(interp, &ctor, "isBuffer", 1, |_interp, _this, args| {
        Ok(Value::Bool(match arg(args, 0) {
            Value::Object(object) => object
                .borrow()
                .props
                .get(BUFFER_MARKER)
                .map(|marker| marker.truthy())
                .unwrap_or(false),
            _ => false,
        }))
    });
    def(interp, &ctor, "byteLength", 2, |interp, _this, args| {
        let bytes = match arg(args, 0) {
            Value::Str(text) => text.as_bytes().len(),
            other => buffer_bytes(interp, &other)?.len(),
        };
        Ok(Value::Num(bytes as f64))
    });
    set(&ctor, "prototype", Value::Object(prototype.clone()));
    set(&prototype, "constructor", Value::Object(ctor.clone()));
    // Node's `buffer` module is an object with `Buffer` on it, so both
    // `require('buffer').Buffer` and `const { Buffer } = require('buffer')` work.
    let module = interp.new_object();
    set(&module, "Buffer", Value::Object(ctor));
    Value::Object(module)
}

/// `this`'s prototype, for building another buffer of the same flavour.
trait ObjectPrototype {
    fn object_prototype(&self, interp: &Interp) -> ObjectRef;
}

impl ObjectPrototype for Value {
    fn object_prototype(&self, interp: &Interp) -> ObjectRef {
        match self {
            Value::Object(object) => object
                .borrow()
                .prototype
                .clone()
                .unwrap_or_else(|| interp.prototypes.object.clone()),
            _ => interp.prototypes.object.clone(),
        }
    }
}

// -------------------------------------------------------------------- timers

fn timers_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();
    for name in ["setTimeout", "setInterval", "clearTimeout", "clearInterval"] {
        let function = global(interp, name);
        set(&module, name, function);
    }
    // `setImmediate` runs on the timer queue with zero delay, which is as close
    // as a virtual clock gets; it returns a number rather than an `Immediate`.
    set(&module, "setImmediate", global(interp, "setTimeout"));
    set(&module, "clearImmediate", global(interp, "clearTimeout"));
    Value::Object(module)
}

// -------------------------------------------------------------------- module

fn module_module(interp: &mut Interp) -> Value {
    let module = interp.new_object();
    let names = interp.new_string_array(NAMES.iter().map(|name| name.to_string()).collect());
    set(&module, "builtinModules", names);
    // `createRequire` returns a `require` bound to a file's directory, which is
    // how an ES module gets at the CommonJS loader.
    def(interp, &module, "createRequire", 1, |interp, _this, args| {
        let from = interp.to_string(&arg(args, 0))?;
        let from = from.strip_prefix("file://").unwrap_or(&from).to_string();
        let directory = Path::new(&from)
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(crate::commonjs::make_require(interp, directory))
    });
    Value::Object(module)
}

fn process_module(interp: &mut Interp) -> Value {
    global(interp, "process")
}

/// Add a `kind_name` helper used by the assert deep comparison.
trait KindName {
    fn kind_name(&self) -> &'static str;
}

impl KindName for Object {
    fn kind_name(&self) -> &'static str {
        match self.kind {
            Kind::Plain => "object",
            Kind::Array => "array",
            Kind::Map(_) => "map",
            Kind::Set(_) => "set",
            Kind::RegExp(_) => "regexp",
            Kind::Date(_) => "date",
            Kind::Error => "error",
            Kind::Promise(_) => "promise",
        }
    }
}
