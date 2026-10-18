//! ES module resolution, linking and evaluation.
//!
//! Only relative and absolute specifiers are supported: there is no
//! `node_modules` lookup, by design (see the crate documentation). Resolution
//! tries the exact path first and then the TypeScript and JavaScript
//! extensions, plus `index.*` for directories.
//!
//! Evaluation follows the observable part of the ES module algorithm:
//! dependencies are loaded first, the exports namespace object is created
//! before the body runs (which is what makes import cycles terminate), and
//! `export` statements are recorded so that re-exports can be resolved from the
//! dependency namespaces afterwards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{ExportDecl, ImportSpecifier, Stmt};
use crate::interp::Interp;
use crate::parser::{self, Language};
use crate::value::{Binding, Env, Value};
use crate::Error;

/// Loads and caches modules, rooted at a directory.
pub struct ModuleLoader {
    root: PathBuf,
    cache: HashMap<PathBuf, CacheEntry>,
}

struct CacheEntry {
    namespace: Value,
}

impl ModuleLoader {
    /// Create a loader whose relative resolution starts at `root`.
    pub fn new(root: impl Into<PathBuf>) -> ModuleLoader {
        ModuleLoader {
            root: root.into(),
            cache: HashMap::new(),
        }
    }

    /// The directory relative specifiers are resolved against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Drop the cache, which is how a plugin host picks up edited files.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// The module environments loaded so far, for diagnostics.
    pub fn loaded_paths(&self) -> Vec<&PathBuf> {
        self.cache.keys().collect()
    }

    /// Load a module from an absolute path, evaluating it on first use.
    pub fn load(&mut self, interp: &mut Interp, path: &Path) -> Result<Value, Error> {
        let resolved = self.resolve_path(path)?;
        if let Some(entry) = self.cache.get(&resolved) {
            return Ok(entry.namespace.clone());
        }

        let source = std::fs::read_to_string(&resolved)
            .map_err(|error| Error::Io(format!("cannot read {}: {error}", resolved.display())))?;
        let parsed = parser::parse_script(&source, Language::from_path(&resolved))?;
        let directory = resolved
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| self.root.clone());

        let exports = interp.new_object();
        let namespace = Value::Object(exports.clone());
        let env = Env::new(Some(interp.globals.clone()), true);

        // `import.meta` for the module body.
        let meta = interp.new_object();
        interp.set_prop(
            &Value::Object(meta.clone()),
            "url",
            Value::str(format!("file://{}", resolved.to_string_lossy())),
        )?;
        interp.set_prop(
            &Value::Object(meta.clone()),
            "path",
            Value::str(resolved.to_string_lossy().to_string()),
        )?;
        interp.set_prop(
            &Value::Object(meta.clone()),
            "dir",
            Value::str(directory.to_string_lossy().to_string()),
        )?;
        env.declare(
            "$import_meta",
            Binding {
                value: Value::Object(meta),
                mutable: false,
                initialized: true,
            },
        );

        // Register before evaluating so that a cycle finds a partial namespace
        // instead of recursing forever.
        self.cache.insert(
            resolved.clone(),
            CacheEntry {
                namespace: namespace.clone(),
            },
        );

        // ---- link: load dependencies and bind their exports
        let mut re_exports: Vec<(String, Value)> = Vec::new();
        for stmt in &parsed.stmts {
            match stmt {
                Stmt::Import(decl) => {
                    let dependency = self.resolve_specifier(interp, &decl.source, &directory)?;
                    for specifier in &decl.specifiers {
                        match specifier {
                            ImportSpecifier::Default(local) => {
                                let value = interp.get_prop(&dependency, "default")?;
                                env.declare(
                                    local,
                                    Binding {
                                        value,
                                        mutable: false,
                                        initialized: true,
                                    },
                                );
                            }
                            ImportSpecifier::Namespace(local) => {
                                env.declare(
                                    local,
                                    Binding {
                                        value: dependency.clone(),
                                        mutable: false,
                                        initialized: true,
                                    },
                                );
                            }
                            ImportSpecifier::Named { imported, local } => {
                                let value = interp.get_prop(&dependency, imported)?;
                                env.declare(
                                    local,
                                    Binding {
                                        value,
                                        mutable: false,
                                        initialized: true,
                                    },
                                );
                            }
                        }
                    }
                }
                Stmt::Export(ExportDecl::Named {
                    source: Some(source),
                    ..
                })
                | Stmt::Export(ExportDecl::All { source, .. }) => {
                    let dependency = self.resolve_specifier(interp, source, &directory)?;
                    re_exports.push((source.clone(), dependency));
                }
                _ => {}
            }
        }

        // ---- evaluate the module body
        let saved_dir = interp.current_dir.clone();
        let saved_source = interp.current_source.clone();
        interp.current_dir = Some(directory);
        interp.current_source = Some(resolved.to_string_lossy().to_string());

        interp.hoist(&parsed.stmts, &env, &env)?;
        for stmt in &parsed.stmts {
            match stmt {
                Stmt::Import(_) => {}
                Stmt::Export(ExportDecl::Default(inner)) => {
                    let value = match inner.as_ref() {
                        Stmt::Class(class) => {
                            interp
                                .eval_stmt(&Stmt::Class(class.clone()), &env, None)
                                .map_err(Error::from)?;
                            match &class.name {
                                Some(name) => env.get_own(name).unwrap_or(Value::Undefined),
                                None => Value::Undefined,
                            }
                        }
                        Stmt::Func(func) => {
                            interp
                                .eval_stmt(&Stmt::Func(func.clone()), &env, None)
                                .map_err(Error::from)?;
                            match &func.name {
                                Some(name) => env.get_own(name).unwrap_or(Value::Undefined),
                                None => Value::Undefined,
                            }
                        }
                        Stmt::Expr(expr) => interp.eval_expr(expr, &env)?,
                        other => {
                            interp.eval_stmt(other, &env, None).map_err(Error::from)?;
                            Value::Undefined
                        }
                    };
                    interp.set_prop(&namespace, "default", value)?;
                }
                Stmt::Export(ExportDecl::Decl(inner)) => {
                    interp.eval_stmt(inner, &env, None).map_err(Error::from)?;
                    for name in declared_names(inner) {
                        let value = env.get_own(&name).unwrap_or(Value::Undefined);
                        interp.set_prop(&namespace, &name, value)?;
                    }
                }
                // Resolved after the body has run, because the local bindings
                // may be declared anywhere in the module.
                Stmt::Export(ExportDecl::Named { .. }) | Stmt::Export(ExportDecl::All { .. }) => {}
                other => {
                    interp.eval_stmt(other, &env, None).map_err(Error::from)?;
                }
            }
        }

        // ---- named exports and re-exports
        for stmt in &parsed.stmts {
            match stmt {
                Stmt::Export(ExportDecl::Named { specifiers, source }) => match source {
                    None => {
                        for specifier in specifiers {
                            let value = env
                                .get_own(&specifier.local)
                                .unwrap_or(Value::Undefined);
                            interp.set_prop(&namespace, &specifier.exported, value)?;
                        }
                    }
                    Some(source) => {
                        let dependency = find_dependency(&re_exports, source);
                        if let Some(dependency) = dependency {
                            for specifier in specifiers {
                                let value = interp.get_prop(&dependency, &specifier.local)?;
                                interp.set_prop(&namespace, &specifier.exported, value)?;
                            }
                        }
                    }
                },
                Stmt::Export(ExportDecl::All { source, alias }) => {
                    let dependency = find_dependency(&re_exports, source);
                    if let Some(dependency) = dependency {
                        match alias {
                            Some(name) => {
                                interp.set_prop(&namespace, name, dependency)?;
                            }
                            None => {
                                let keys: Vec<String> = match &dependency {
                                    Value::Object(object) => object
                                        .borrow()
                                        .props
                                        .keys()
                                        .into_iter()
                                        .map(|key| key.to_string())
                                        .collect(),
                                    _ => Vec::new(),
                                };
                                for key in keys {
                                    if key == "default" {
                                        continue;
                                    }
                                    let value = interp.get_prop(&dependency, &key)?;
                                    interp.set_prop(&namespace, &key, value)?;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        interp.current_dir = saved_dir;
        interp.current_source = saved_source;
        Ok(namespace)
    }

    /// Resolve a specifier written inside `from_dir` and load it.
    fn resolve_specifier(
        &mut self,
        interp: &mut Interp,
        specifier: &str,
        from_dir: &Path,
    ) -> Result<Value, Error> {
        let path = self.resolve_relative(specifier, from_dir)?;
        self.load(interp, &path)
    }

    /// Turn a specifier into a path, rejecting bare package names.
    pub fn resolve_relative(&self, specifier: &str, from_dir: &Path) -> Result<PathBuf, Error> {
        if !specifier.starts_with("./")
            && !specifier.starts_with("../")
            && !specifier.starts_with(".\\")
            && !specifier.starts_with("..\\")
            && !Path::new(specifier).is_absolute()
        {
            return Err(Error::Io(format!(
                "cannot resolve `{specifier}`: rust4ts has no node_modules resolution, use a relative or absolute path"
            )));
        }
        let base = if Path::new(specifier).is_absolute() {
            PathBuf::from(specifier)
        } else {
            from_dir.join(specifier)
        };
        self.resolve_path(&base)
    }

    /// Apply the extension and directory-index rules to a concrete path.
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, Error> {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        for extension in ["ts", "mts", "js", "mjs", "cts", "cjs"] {
            let candidate = path.with_extension(extension);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        if path.is_dir() {
            for index in ["index.ts", "index.mts", "index.js", "index.mjs"] {
                let candidate = path.join(index);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(Error::Io(format!(
            "cannot find module `{}`",
            path.display()
        )))
    }
}

/// Look up a previously loaded re-export dependency by its source text.
fn find_dependency(dependencies: &[(String, Value)], source: &str) -> Option<Value> {
    dependencies
        .iter()
        .find(|(name, _)| name == source)
        .map(|(_, value)| value.clone())
}

/// The names a declaration introduces, used to publish `export const ...`.
fn declared_names(stmt: &Stmt) -> Vec<String> {
    match stmt {
        Stmt::Var { decls, .. } => decls
            .iter()
            .flat_map(|(pattern, _)| crate::interp::pattern_names(pattern))
            .collect(),
        Stmt::Func(func) => func.name.clone().into_iter().collect(),
        Stmt::Class(class) => class.name.clone().into_iter().collect(),
        Stmt::Enum { name, .. } => vec![name.clone()],
        Stmt::Export(ExportDecl::Decl(inner)) => declared_names(inner),
        _ => Vec::new(),
    }
}
