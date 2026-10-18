//! The tree-walking interpreter.
//!
//! Design notes that matter for anyone reading this:
//!
//! * Non-local exits (`return`, `break`, `continue`, `throw`) travel through
//!   `Result<_, Control>` rather than through a loop of flags.
//! * `await` drives the event loop instead of suspending a state machine. When
//!   the awaited promise is not settled yet, the interpreter drains microtasks
//!   and then runs timers until it settles, or reports a deadlock. That makes
//!   `await` work for the synchronous-and-timer workloads an embedded plugin
//!   runtime faces, at the cost of a deviation: an async function body starts
//!   eagerly rather than on a later microtask.
//! * Class field initialisers run when the instance is created, which is before
//!   the constructor body. In a derived class a real engine runs them after
//!   `super()`, so that ordering is a documented deviation.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::ast::*;
use crate::builtins::{self, Prototypes};
use crate::value::*;

/// A non-local exit produced while evaluating.
#[derive(Debug)]
pub enum Control {
    /// A JavaScript `throw`, carrying the thrown value and a script stack trace.
    Throw(JsError),
    Return(Value),
    Break(Option<String>),
    Continue(Option<String>),
    /// `process.exit(code)`: terminate the host process.
    ///
    /// This is deliberately not a [`Control::Throw`], so a `try` / `catch` in
    /// script code cannot swallow it — the same guarantee Node gives.
    Exit(i32),
}

/// A thrown JavaScript value together with the frames it passed through.
#[derive(Debug, Clone)]
pub struct JsError {
    pub value: Value,
    pub frames: Vec<String>,
}

impl Control {
    /// Render a non-local exit the way a command line should report it.
    ///
    /// Only [`Control::Throw`] is a real script error; the other variants are
    /// control flow that escaped its construct, which is a bug in the script.
    pub fn render(&self) -> String {
        match self {
            Control::Throw(error) => error.render(),
            Control::Exit(code) => format!("process exited with code {code}"),
            Control::Return(value) => {
                format!("unexpected `return` of {}", value.to_display_string())
            }
            Control::Break(_) => "unexpected `break` outside a loop".to_string(),
            Control::Continue(_) => "unexpected `continue` outside a loop".to_string(),
        }
    }
}

impl JsError {
    /// Render the error for a terminal, the way Node does.
    pub fn render(&self) -> String {
        let mut text = if let Value::Object(object) = &self.value {
            let borrowed = object.borrow();
            match borrowed.props.get("stack") {
                Some(Value::Str(s)) => s.to_string(),
                _ => borrowed
                    .props
                    .get("message")
                    .map(|m| m.to_display_string())
                    .unwrap_or_else(|| "Error".to_string()),
            }
        } else {
            self.value.to_display_string()
        };
        for frame in self.frames.iter().rev() {
            text.push_str("\n    at ");
            text.push_str(frame);
        }
        text
    }
}

/// A queued timer.
struct Timer {
    id: u64,
    /// Virtual time in milliseconds at which it fires.
    due: f64,
    callback: Value,
    args: Vec<Value>,
    interval: Option<f64>,
}

/// Where `console` writes.
#[derive(Clone)]
pub enum Output {
    /// Print to the real standard output.
    Stdout,
    /// Capture into a buffer, which is what the test suite uses.
    Buffer(Rc<RefCell<String>>),
}

impl Output {
    fn write_line(&self, line: &str) {
        match self {
            Output::Stdout => println!("{line}"),
            Output::Buffer(buffer) => {
                let mut buffer = buffer.borrow_mut();
                buffer.push_str(line);
                buffer.push('\n');
            }
        }
    }

    /// Write text with no trailing newline, flushing it in stream mode.
    fn write_raw(&self, text: &str) {
        match self {
            Output::Stdout => {
                use std::io::Write;
                let mut stdout = std::io::stdout();
                let _ = stdout.write_all(text.as_bytes());
                let _ = stdout.flush();
            }
            Output::Buffer(buffer) => buffer.borrow_mut().push_str(text),
        }
    }

    /// Write diagnostics, which go to standard error in stream mode.
    fn write_err(&self, text: &str) {
        match self {
            Output::Stdout => {
                use std::io::Write;
                let mut stderr = std::io::stderr();
                let _ = stderr.write_all(text.as_bytes());
                let _ = stderr.flush();
            }
            Output::Buffer(buffer) => buffer.borrow_mut().push_str(text),
        }
    }
}

/// A pending microtask: a reaction scheduled by promise resolution.
pub type Microtask = Box<dyn FnOnce(&mut Interp) -> Result<(), Control>>;

/// The interpreter: one JavaScript realm.
pub struct Interp {
    pub globals: EnvRef,
    pub global_object: ObjectRef,
    pub prototypes: Prototypes,
    pub microtasks: VecDeque<Microtask>,
    timers: Vec<Timer>,
    next_timer_id: u64,
    /// Virtual clock in milliseconds, advanced by the timer queue.
    pub clock_ms: f64,
    pub call_depth: usize,
    pub output: Output,
    /// Directory of the module currently executing, for resolving specifiers.
    pub current_dir: Option<std::path::PathBuf>,
    /// Source label of the module currently executing, for stack traces.
    pub current_source: Option<String>,
    /// Names of the frames currently on the JavaScript call stack.
    pub frames: Vec<String>,
    /// Guards against runaway recursion.
    pub max_depth: usize,
    /// Loaded CommonJS modules, keyed by canonical path. The entry is inserted
    /// before a module body runs, which is what makes a `require` cycle see the
    /// partial `exports` object instead of recursing forever.
    pub cjs_cache: std::collections::HashMap<std::path::PathBuf, Value>,
}

impl Interp {
    /// Create a fresh realm with all built-ins installed.
    pub fn new() -> Interp {
        let prototypes = builtins::create_prototypes();
        let global_object = Object::new(Some(prototypes.object.clone()));
        let globals = Env::new(None, true);
        let mut interp = Interp {
            globals,
            global_object,
            prototypes,
            microtasks: VecDeque::new(),
            timers: Vec::new(),
            next_timer_id: 1,
            clock_ms: 0.0,
            call_depth: 0,
            output: Output::Buffer(Rc::new(RefCell::new(String::new()))),
            current_dir: None,
            current_source: None,
            frames: Vec::new(),
            max_depth: 900,
            cjs_cache: std::collections::HashMap::new(),
        };
        builtins::install(&mut interp);
        interp
    }

    /// Send `console` output to the real terminal.
    pub fn echo_to_stdout(&mut self) {
        self.output = Output::Stdout;
    }

    /// Capture `console` output into a buffer.
    pub fn capture_output(&mut self) -> Rc<RefCell<String>> {
        let buffer = Rc::new(RefCell::new(String::new()));
        self.output = Output::Buffer(buffer.clone());
        buffer
    }

    /// Print one line through the configured output.
    pub fn print_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Write text with no trailing newline — `process.stdout.write`.
    pub fn print_raw(&self, text: &str) {
        self.output.write_raw(text);
    }

    /// Write text to the diagnostics stream — `process.stderr.write`.
    ///
    /// In stream mode this is the real standard error. When output is captured
    /// (the test suite) it lands in the same buffer, so a test sees writes in
    /// the order the program made them.
    pub fn print_error(&self, text: &str) {
        self.output.write_err(text);
    }

    // ----------------------------------------------------------- constructors

    /// A plain object with the given prototype.
    pub fn new_object(&self) -> ObjectRef {
        Object::new(Some(self.prototypes.object.clone()))
    }

    /// A fresh module scope, whose parent is the global scope.
    pub fn module_env(&self) -> EnvRef {
        Env::new(Some(self.globals.clone()), true)
    }

    /// The canonical path of a module file, so the cache has one key per file
    /// even when the same module is reached by different relative routes.
    pub fn canonical_path(&self, path: &std::path::Path) -> Result<std::path::PathBuf, String> {
        std::fs::canonicalize(path)
            .map_err(|error| format!("cannot resolve {}: {error}", path.display()))
    }

    /// An array holding `items`.
    pub fn new_array(&self, items: Vec<Value>) -> Value {
        let object = Object::with_kind(Kind::Array, Some(self.prototypes.array.clone()));
        {
            let mut borrowed = object.borrow_mut();
            for (index, item) in items.iter().enumerate() {
                borrowed.props.set(index.to_string(), item.clone());
            }
            borrowed.props.set("length", Value::Num(items.len() as f64));
        }
        object.borrow_mut().hide_from_enumeration("length");
        Value::Object(object)
    }

    /// An array of strings, used where a plain list is wanted.
    pub fn new_string_array(&self, items: Vec<String>) -> Value {
        self.new_array(items.into_iter().map(Value::str).collect())
    }

    /// Build a callable object from a Rust closure.
    pub fn native<F>(&self, name: &str, length: usize, body: F) -> Value
    where
        F: Fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control> + 'static,
    {
        self.native_with_proto(name, length, body, self.prototypes.function.clone())
    }

    fn native_with_proto<F>(
        &self,
        name: &str,
        length: usize,
        body: F,
        prototype: ObjectRef,
    ) -> Value
    where
        F: Fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control> + 'static,
    {
        let object = Object::new(Some(prototype));
        {
            let mut borrowed = object.borrow_mut();
            borrowed.callable = Some(Callable::Native(Rc::new(body)));
            borrowed.props.set("name", Value::str(name));
            borrowed.props.set("length", Value::Num(length as f64));
            borrowed.hide_from_enumeration("name");
            borrowed.hide_from_enumeration("length");
            borrowed.hide_from_enumeration("prototype");
        }
        Value::Object(object)
    }

    /// Build a user function object.
    pub fn new_user_function(&self, user: UserFunction) -> Value {
        let object = Object::new(Some(self.prototypes.function.clone()));
        let name = user.name.clone().unwrap_or_default();
        {
            let mut borrowed = object.borrow_mut();
            borrowed.props.set("name", Value::str(&name));
            borrowed
                .props
                .set("length", Value::Num(user.params.len() as f64));
            // A constructible function gets a fresh prototype object.
            if !user.is_arrow {
                let proto = Object::new(Some(self.prototypes.object.clone()));
                proto.borrow_mut().props.set("constructor", Value::Object(object.clone()));
                proto.borrow_mut().hide_from_enumeration("constructor");
                borrowed
                    .props
                    .set("prototype", Value::Object(proto));
            }
            for slot in ["name", "length", "prototype"] {
                borrowed.hide_from_enumeration(slot);
            }
            borrowed.callable = Some(Callable::User(Box::new(user)));
        }
        Value::Object(object)
    }

    /// Install a property on the global object and its global binding.
    pub fn define_global(&mut self, name: &str, value: Value) {
        self.global_object.borrow_mut().props.set(name, value.clone());
        self.globals.declare(
            name,
            Binding {
                value,
                mutable: true,
                initialized: true,
            },
        );
    }

    // --------------------------------------------------------------- errors

    /// Build an error object of the named kind.
    pub fn make_error(&self, kind: &str, message: &str) -> Value {
        let prototype = self
            .prototypes
            .error
            .get(kind)
            .or_else(|| self.prototypes.error.get("Error"))
            .cloned()
            .unwrap_or_else(|| self.prototypes.object.clone());
        let object = Object::with_kind(Kind::Error, Some(prototype));
        {
            let mut borrowed = object.borrow_mut();
            borrowed.props.set("name", Value::str(kind));
            borrowed.props.set("message", Value::str(message));
            borrowed
                .props
                .set("stack", Value::str(format!("{kind}: {message}")));
        }
        Value::Object(object)
    }

    pub fn throw_error(&self, kind: &str, message: impl AsRef<str>) -> Control {
        Control::Throw(JsError {
            value: self.make_error(kind, message.as_ref()),
            frames: self.frames.clone(),
        })
    }

    pub fn type_error<T>(&self, message: impl AsRef<str>) -> Result<T, Control> {
        Err(self.throw_error("TypeError", message))
    }

    pub fn range_error<T>(&self, message: impl AsRef<str>) -> Result<T, Control> {
        Err(self.throw_error("RangeError", message))
    }

    /// Throw an arbitrary JavaScript value.
    pub fn throw_value<T>(&self, value: Value) -> Result<T, Control> {
        Err(Control::Throw(JsError {
            value,
            frames: self.frames.clone(),
        }))
    }

    // -------------------------------------------------------- conversions

    /// ToPrimitive: try `valueOf` then `toString`, as the spec does.
    pub fn to_primitive(&mut self, value: &Value) -> Result<Value, Control> {
        match value {
            Value::Object(object) => {
                let is_callable = object.borrow().is_callable();
                if is_callable {
                    return Ok(Value::str(object.borrow().props.get("name").map(|n| {
                        format!("function {}() {{ [native code] }}", n.to_display_string())
                    })
                    .unwrap_or_else(|| "function () { [native code] }".to_string())));
                }
                for method in ["valueOf", "toString"] {
                    let function = self.get_prop(value, method)?;
                    if let Value::Object(ref callable) = function {
                        if callable.borrow().is_callable() {
                            let result = self.call_value(&function, value.clone(), Vec::new())?;
                            if !matches!(result, Value::Object(_)) {
                                return Ok(result);
                            }
                        }
                    }
                }
                Ok(Value::str("[object Object]"))
            }
            _ => Ok(value.clone()),
        }
    }

    /// ToString, going through ToPrimitive for objects.
    pub fn to_string(&mut self, value: &Value) -> Result<String, Control> {
        match value {
            Value::Object(_) => {
                let primitive = self.to_primitive(value)?;
                Ok(primitive.to_display_string())
            }
            _ => Ok(value.to_display_string()),
        }
    }

    /// ToNumber, going through ToPrimitive for objects.
    pub fn to_number_value(&mut self, value: &Value) -> Result<f64, Control> {
        match value {
            Value::Object(_) => {
                let primitive = self.to_primitive(value)?;
                Ok(primitive.to_number())
            }
            _ => Ok(value.to_number()),
        }
    }

    /// Convert a property key to its string form.
    pub fn key_to_string(&mut self, key: &Value) -> Result<String, Control> {
        match key {
            Value::Str(s) => Ok(s.to_string()),
            Value::Num(n) => Ok(format_number(*n)),
            other => self.to_string(other),
        }
    }

    // ------------------------------------------------------------ properties

    /// Look up `key`, walking the prototype chain and calling getters.
    pub fn get_prop(&mut self, target: &Value, key: &str) -> Result<Value, Control> {
        // String primitives expose `length` and their characters.
        if let Value::Str(text) = target {
            if key == "length" {
                return Ok(Value::Num(text.chars().count() as f64));
            }
            if let Some(index) = index_of(key) {
                return Ok(match text.chars().nth(index) {
                    Some(c) => Value::str(c.to_string()),
                    None => Value::Undefined,
                });
            }
            return self.get_from_chain(&Value::Object(self.prototypes.string.clone()), key);
        }

        let object = match target {
            Value::Object(object) => object.clone(),
            Value::Undefined | Value::Null => {
                return self.type_error(format!(
                    "cannot read property `{key}` of {}",
                    target.to_display_string()
                ))
            }
            _ => {
                // Numbers and booleans read from their prototypes.
                let prototype = match target {
                    Value::Num(_) => self.prototypes.number.clone(),
                    Value::Bool(_) => self.prototypes.boolean.clone(),
                    _ => self.prototypes.object.clone(),
                };
                return self.get_from_chain(&Value::Object(prototype), key);
            }
        };
        self.get_from_chain(&Value::Object(object), key)
    }

    fn get_from_chain(&mut self, start: &Value, key: &str) -> Result<Value, Control> {
        // The receiver for a getter is the object the property was read from,
        // not the prototype that happens to hold the accessor.
        let receiver = start.clone();
        let mut current = match start {
            Value::Object(object) => Some(object.clone()),
            _ => None,
        };
        while let Some(object) = current {
            let getter = object
                .borrow()
                .accessors
                .get(key)
                .and_then(|(get, _)| get.clone());
            if let Some(getter) = getter {
                return self.call_value(&getter, receiver, Vec::new());
            }
            if let Some(value) = object.borrow().props.get(key).cloned() {
                return Ok(value);
            }
            let prototype = object.borrow().prototype.clone();
            current = prototype;
        }
        Ok(Value::Undefined)
    }

    /// Assign `key`, honouring setters on the chain.
    pub fn set_prop(&mut self, target: &Value, key: &str, value: Value) -> Result<(), Control> {
        let object = match target {
            Value::Object(object) => object.clone(),
            Value::Undefined | Value::Null => {
                return self.type_error(format!(
                    "cannot set property `{key}` of {}",
                    target.to_display_string()
                ))
            }
            // Writing to a primitive silently fails, as in sloppy mode.
            _ => return Ok(()),
        };

        // A setter anywhere on the chain takes precedence, and it is called with
        // the object the assignment targeted.
        let mut current = Some(object.clone());
        while let Some(node) = current {
            let setter = node
                .borrow()
                .accessors
                .get(key)
                .and_then(|(_, set)| set.clone());
            if let Some(setter) = setter {
                self.call_value(&setter, target.clone(), vec![value])?;
                return Ok(());
            }
            if node.borrow().props.contains(key) {
                break;
            }
            let prototype = node.borrow().prototype.clone();
            current = prototype;
        }

        {
            let mut borrowed = object.borrow_mut();
            borrowed.props.set(key, value.clone());
            if borrowed.is_array() {
                if let Some(index) = index_of(key) {
                    // Keep `length` in sync with the highest index.
                    let length = borrowed.array_length();
                    if index + 1 > length {
                        borrowed.props.set("length", Value::Num((index + 1) as f64));
                    }
                } else if key == "length" {
                    if let Value::Num(n) = value {
                        let new_length = if n < 0.0 { 0 } else { n as usize };
                        let stale: Vec<String> = borrowed
                            .props
                            .keys()
                            .into_iter()
                            .filter(|k| index_of(k).is_some_and(|i| i >= new_length))
                            .map(|k| k.to_string())
                            .collect();
                        for k in stale {
                            borrowed.props.remove(&k);
                        }
                        borrowed.props.set("length", Value::Num(new_length as f64));
                    }
                }
            }
        }
        Ok(())
    }

    /// `delete target[key]`
    pub fn delete_prop(&mut self, target: &Value, key: &str) -> Result<bool, Control> {
        match target {
            Value::Object(object) => Ok(object.borrow_mut().props.remove(key)),
            _ => Ok(true),
        }
    }

    /// True when `key` exists on the object or its prototype chain.
    pub fn has_prop(&self, target: &Value, key: &str) -> bool {
        let mut current = match target {
            Value::Object(object) => Some(object.clone()),
            _ => None,
        };
        while let Some(object) = current {
            let borrowed = object.borrow();
            if borrowed.props.contains(key) || borrowed.accessors.contains_key(key) {
                return true;
            }
            current = borrowed.prototype.clone();
        }
        false
    }

    // ---------------------------------------------------------------- calls

    /// Call a callable value.
    pub fn call_value(
        &mut self,
        callee: &Value,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value, Control> {
        let object = match callee {
            Value::Object(object) => object.clone(),
            _ => {
                return self.type_error(format!(
                    "{} is not a function",
                    callee.to_display_string()
                ))
            }
        };
        let callable = object.borrow().callable.as_ref().map(clone_callable);
        let callable = match callable {
            Some(callable) => callable,
            None => {
                return self.type_error(format!(
                    "{} is not a function",
                    callee.to_display_string()
                ))
            }
        };

        self.call_depth += 1;
        if self.call_depth > self.max_depth {
            self.call_depth -= 1;
            return self.range_error("maximum call stack size exceeded");
        }
        let result = match callable {
            Callable::Native(function) => function(self, &this, &args),
            Callable::User(user) => self.call_user(&object, &user, this, args),
            Callable::Class { ctor } => match ctor {
                Some(ctor) => self.call_user(&object, &ctor, this, args),
                None => Ok(Value::Undefined),
            },
        };
        self.call_depth -= 1;
        result
    }

    fn call_user(
        &mut self,
        callee: &ObjectRef,
        user: &UserFunction,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value, Control> {
        let display_name = user.name.clone().unwrap_or_else(|| "<anonymous>".to_string());
        self.frames.push(display_name);
        let env = Env::new(Some(user.closure.clone()), true);
        let parent_env = env
            .parent
            .clone()
            .unwrap_or_else(|| env.clone());
        let this_value = if user.is_arrow {
            // An arrow keeps the `this` of the scope it was written in.
            self.current_this(&parent_env)
        } else {
            this
        };
        *env.this_value.borrow_mut() = this_value.clone();

        // `super` is resolved through the home object of the method: `super.x`
        // reads the parent prototype, while `super(...)` calls the parent
        // constructor stored as that prototype's `constructor`.
        if let Some(home) = &user.home {
            let parent_prototype = home.borrow().prototype.clone();
            let parent_constructor = parent_prototype
                .as_ref()
                .and_then(|proto| proto.borrow().props.get("constructor").cloned());
            let super_proto = match parent_prototype {
                Some(parent) => Value::Object(parent),
                None => Value::Object(self.new_object()),
            };
            env.declare(
                "$super_proto",
                Binding {
                    value: super_proto,
                    mutable: false,
                    initialized: true,
                },
            );
            if let Some(constructor) = parent_constructor {
                env.declare(
                    "$super_ctor",
                    Binding {
                        value: constructor,
                        mutable: false,
                        initialized: true,
                    },
                );
            }
        }

        // Bind parameters, then hoist and run the body.
        self.bind_params(&user.params, &args, &env)?;
        let outcome = match &user.body {
            FuncBody::Expr(expr) => self.eval_expr(expr, &env),
            FuncBody::Block(stmts) => {
                self.hoist(stmts, &env, &env)?;
                self.eval_block(stmts, &env)
            }
        };
        // A class constructor that returns an object overrides the instance.
        self.frames.pop();

        let unwrapped = match outcome {
            Ok(value) => Ok(value),
            Err(Control::Return(value)) => Ok(value),
            Err(other) => Err(other),
        };

        if user.is_async {
            // Async bodies run eagerly here and the result is wrapped, which
            // keeps `await` usable without a suspension state machine.
            return Ok(match unwrapped {
                Ok(value) => self.resolved_promise(value),
                Err(Control::Throw(error)) => self.rejected_promise(error.value),
                Err(other) => return Err(other),
            });
        }

        if matches!(callee.borrow().callable, Some(Callable::Class { .. })) {
            if let Ok(Value::Object(returned)) = &unwrapped {
                return Ok(Value::Object(returned.clone()));
            }
        }
        unwrapped
    }

    /// `new callee(args)`
    pub fn construct(&mut self, callee: &Value, args: Vec<Value>) -> Result<Value, Control> {
        let object = match callee {
            Value::Object(object) => object.clone(),
            _ => {
                return self.type_error(format!(
                    "{} is not a constructor",
                    callee.to_display_string()
                ))
            }
        };

        let is_arrow = matches!(
            object.borrow().callable,
            Some(Callable::User(ref user)) if user.is_arrow
        );
        if is_arrow {
            return self.type_error("an arrow function cannot be used as a constructor");
        }

        // Built-in constructors such as Array or Map handle `new` themselves.
        if let Some(Callable::Native(_)) = object.borrow().callable {
            return self.call_value(callee, Value::Undefined, args);
        }

        let prototype = match object.borrow().props.get("prototype") {
            Some(Value::Object(prototype)) => Some(prototype.clone()),
            _ => None,
        };
        let instance = Object::new(prototype.or_else(|| Some(self.prototypes.object.clone())));

        let instance_value = Value::Object(instance.clone());

        // Field initialisers run before the constructor body. See the module
        // documentation: a real engine runs a derived class's fields after
        // `super()` returns, which this implementation does not reproduce.
        let init_fields = object.borrow().props.get("$init_fields").cloned();
        if let Some(init_fields) = init_fields {
            self.call_value(&init_fields, instance_value.clone(), Vec::new())?;
        }

        // A class without a constructor of its own defers to its parent, which
        // is what makes `class B extends A {}` behave like `new A(...)`.
        let has_ctor = matches!(
            object.borrow().callable,
            Some(Callable::Class { ctor: Some(_) })
        );
        if !has_ctor {
            let parent = object.borrow().props.get("$super").cloned();
            if let Some(parent) = parent {
                let is_callable =
                    matches!(&parent, Value::Object(o) if o.borrow().is_callable());
                if is_callable {
                    self.call_value(&parent, instance_value.clone(), args.clone())?;
                }
            }
        }

        let result = self.call_value(callee, instance_value.clone(), args)?;
        match result {
            Value::Object(returned) => Ok(Value::Object(returned)),
            _ => Ok(instance_value),
        }
    }

    /// Bind parameters, applying defaults, destructuring and rest.
    fn bind_params(
        &mut self,
        params: &[Param],
        args: &[Value],
        env: &EnvRef,
    ) -> Result<(), Control> {
        for (index, param) in params.iter().enumerate() {
            let value = if param.rest {
                let rest: Vec<Value> = args.iter().skip(index).cloned().collect();
                self.new_array(rest)
            } else {
                match args.get(index) {
                    Some(value) => value.clone(),
                    None => Value::Undefined,
                }
            };
            self.bind_pattern(&param.pattern, value, env, true)?;
        }
        Ok(())
    }

    /// Bind a destructuring pattern to a value.
    ///
    /// `declare` selects between creating new mutable bindings (parameters and
    /// declarations) and assigning to existing ones.
    pub fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
        env: &EnvRef,
        declare: bool,
    ) -> Result<(), Control> {
        match pattern {
            Pattern::Ident(name) => {
                if declare {
                    env.declare(
                        name,
                        Binding {
                            value,
                            mutable: true,
                            initialized: true,
                        },
                    );
                } else {
                    self.assign_name(env, name, value)?;
                }
                Ok(())
            }
            Pattern::Default(inner, default) => {
                let resolved = if matches!(value, Value::Undefined) {
                    self.eval_expr(default, env)?
                } else {
                    value
                };
                self.bind_pattern(inner, resolved, env, declare)
            }
            Pattern::Array { elements, rest } => {
                let mut index = 0usize;
                for element in elements {
                    if let Some(element) = element {
                        let mut item = self.get_prop(&value, &index.to_string())?;
                        // A default only kicks in for a missing value.
                        if matches!(element, Pattern::Default(..)) && matches!(item, Value::Undefined)
                        {
                            item = Value::Undefined;
                        }
                        self.bind_pattern(element, item, env, declare)?;
                    }
                    index += 1;
                }
                if let Some(rest) = rest {
                    let items = self.array_items(&value)?;
                    let tail: Vec<Value> = if index < items.len() {
                        items[index..].to_vec()
                    } else {
                        Vec::new()
                    };
                    let tail = self.new_array(tail);
                    self.bind_pattern(rest, tail, env, declare)?;
                }
                Ok(())
            }
            Pattern::Object { props, rest } => {
                let mut taken: Vec<String> = Vec::new();
                for prop in props {
                    let key = self.prop_key_string(&prop.key, env)?;
                    let item = self.get_prop(&value, &key)?;
                    taken.push(key);
                    self.bind_pattern(&prop.value, item, env, declare)?;
                }
                if let Some(rest) = rest {
                    let target = Object::new(Some(self.prototypes.object.clone()));
                    if let Value::Object(source) = &value {
                        let names: Vec<String> = source
                            .borrow()
                            .props
                            .keys()
                            .into_iter()
                            .filter(|key| source.borrow().is_enumerable(key))
                            .map(|key| key.to_string())
                            .collect();
                        for name in names {
                            if taken.iter().any(|taken| taken == &name) {
                                continue;
                            }
                            let item = source
                                .borrow()
                                .props
                                .get(&name)
                                .cloned()
                                .unwrap_or(Value::Undefined);
                            target.borrow_mut().props.set(name, item);
                        }
                    }
                    self.bind_pattern(rest, Value::Object(target), env, declare)?;
                }
                Ok(())
            }
        }
    }

    /// Resolve a property key, which may be computed.
    pub fn prop_key_string(&mut self, key: &PropKey, env: &EnvRef) -> Result<String, Control> {
        match key {
            PropKey::Ident(name) => Ok(name.clone()),
            PropKey::Str(text) => Ok(text.clone()),
            PropKey::Num(n) => Ok(format_number(*n)),
            PropKey::Computed(expr) => {
                let value = self.eval_expr(expr, env)?;
                self.key_to_string(&value)
            }
        }
    }

    /// Assign to an existing binding, building a ReferenceError when missing.
    pub fn assign_name(&mut self, env: &EnvRef, name: &str, value: Value) -> Result<(), Control> {
        match env.resolve(name) {
            Some((owner, binding)) => {
                if !binding.initialized {
                    return self.type_error(format!(
                        "cannot access `{name}` before initialization"
                    ));
                }
                if !binding.mutable {
                    return self.type_error(format!("assignment to constant variable `{name}`"));
                }
                if let Some(slot) = owner.bindings.borrow_mut().get_mut(name) {
                    slot.value = value;
                }
                Ok(())
            }
            None => {
                // Sloppy-mode behaviour: a global is created.
                if env.function_scope().has_own(name) {
                    self.globals.declare(
                        name,
                        Binding {
                            value,
                            mutable: true,
                            initialized: true,
                        },
                    );
                    return Ok(());
                }
                self.globals.declare(
                    name,
                    Binding {
                        value,
                        mutable: true,
                        initialized: true,
                    },
                );
                Ok(())
            }
        }
    }

    /// The items of an array value.
    pub fn array_items(&mut self, value: &Value) -> Result<Vec<Value>, Control> {
        let object = match value {
            Value::Object(object) => object.clone(),
            _ => return self.type_error("value is not iterable"),
        };
        let length = object.borrow().array_length();
        let mut items = Vec::with_capacity(length);
        for index in 0..length {
            items.push(self.get_prop(value, &index.to_string())?);
        }
        Ok(items)
    }

    // ------------------------------------------------------------ evaluation

    /// Hoist `var` declarations and function declarations into scope.
    pub fn hoist(&mut self, stmts: &[Stmt], function_env: &EnvRef, block_env: &EnvRef) -> Result<(), Control> {
        for stmt in stmts {
            match stmt {
                Stmt::Var { kind, decls } => {
                    let target = if *kind == VarKind::Var {
                        function_env
                    } else {
                        block_env
                    };
                    for (pattern, _) in decls {
                        for name in pattern_names(pattern) {
                            if *kind == VarKind::Var {
                                if !target.has_own(&name) {
                                    target.declare(
                                        &name,
                                        Binding {
                                            value: Value::Undefined,
                                            mutable: true,
                                            initialized: true,
                                        },
                                    );
                                }
                            } else if !target.has_own(&name) {
                                target.declare(
                                    &name,
                                    Binding {
                                        value: Value::Undefined,
                                        mutable: true,
                                        initialized: false,
                                    },
                                );
                            }
                        }
                    }
                }
                Stmt::Func(func) => {
                    let value = self.eval_function(func, block_env, None)?;
                    if let Some(name) = &func.name {
                        block_env.declare(
                            name,
                            Binding {
                                value,
                                mutable: true,
                                initialized: true,
                            },
                        );
                    }
                }
                Stmt::Block(inner) => self.hoist(inner, function_env, block_env)?,
                Stmt::If { cons, alt, .. } => {
                    self.hoist(std::slice::from_ref(cons.as_ref()), function_env, block_env)?;
                    if let Some(alt) = alt {
                        self.hoist(std::slice::from_ref(alt.as_ref()), function_env, block_env)?;
                    }
                }
                Stmt::For { init, body, .. } => {
                    if let Some(ForInit::Var { kind, decls }) = init {
                        let target = if *kind == VarKind::Var {
                            function_env
                        } else {
                            block_env
                        };
                        for (pattern, _) in decls {
                            for name in pattern_names(pattern) {
                                if !target.has_own(&name) {
                                    target.declare(
                                        &name,
                                        Binding {
                                            value: Value::Undefined,
                                            mutable: true,
                                            initialized: *kind != VarKind::Var,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    self.hoist(std::slice::from_ref(body.as_ref()), function_env, block_env)?;
                }
                Stmt::ForIn { body, .. } | Stmt::ForOf { body, .. } | Stmt::While { body, .. } => {
                    self.hoist(std::slice::from_ref(body.as_ref()), function_env, block_env)?
                }
                Stmt::DoWhile { body, .. } => {
                    self.hoist(std::slice::from_ref(body.as_ref()), function_env, block_env)?
                }
                Stmt::Try {
                    block,
                    handler,
                    finalizer,
                } => {
                    self.hoist(block, function_env, block_env)?;
                    if let Some((_, body)) = handler {
                        self.hoist(body, function_env, block_env)?;
                    }
                    if let Some(body) = finalizer {
                        self.hoist(body, function_env, block_env)?;
                    }
                }
                Stmt::Switch { cases, .. } => {
                    for case in cases {
                        self.hoist(&case.body, function_env, block_env)?;
                    }
                }
                Stmt::Labeled { body, .. } => {
                    self.hoist(std::slice::from_ref(body.as_ref()), function_env, block_env)?
                }
                Stmt::Export(ExportDecl::Decl(inner)) => {
                    self.hoist(std::slice::from_ref(inner.as_ref()), function_env, block_env)?
                }
                Stmt::Export(ExportDecl::Default(inner)) => {
                    self.hoist(std::slice::from_ref(inner.as_ref()), function_env, block_env)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Evaluate a list of statements, returning the last expression value.
    pub fn eval_block(&mut self, stmts: &[Stmt], env: &EnvRef) -> Result<Value, Control> {
        let mut last = Value::Undefined;
        for stmt in stmts {
            last = self.eval_stmt(stmt, env, None)?;
        }
        Ok(last)
    }

    /// Evaluate one statement.
    pub fn eval_stmt(
        &mut self,
        stmt: &Stmt,
        env: &EnvRef,
        label: Option<&str>,
    ) -> Result<Value, Control> {
        match stmt {
            Stmt::Empty | Stmt::Debugger => Ok(Value::Undefined),
            Stmt::Expr(expr) => self.eval_expr(expr, env),
            Stmt::Block(stmts) => {
                let scope = Env::new(Some(env.clone()), false);
                self.hoist(stmts, env, &scope)?;
                self.eval_block(stmts, &scope)
            }
            Stmt::Var { kind, decls } => {
                for (pattern, init) in decls {
                    let value = match init {
                        Some(expr) => self.eval_expr(expr, env)?,
                        None => Value::Undefined,
                    };
                    if *kind == VarKind::Var {
                        // `var` binds in the function scope; the binding was
                        // created by the hoisting pass.
                        let target = env.function_scope();
                        self.bind_var_pattern(pattern, value, &target)?;
                    } else {
                        self.declare_pattern(pattern, value, env)?;
                    }
                }
                Ok(Value::Undefined)
            }
            Stmt::Func(_) => Ok(Value::Undefined), // hoisted already
            Stmt::Class(class) => {
                let value = self.eval_class(class, env, None)?;
                if let Some(name) = &class.name {
                    env.declare(
                        name,
                        Binding {
                            value,
                            mutable: false,
                            initialized: true,
                        },
                    );
                }
                Ok(Value::Undefined)
            }
            Stmt::Enum { name, members } => {
                let value = self.eval_enum(members, env)?;
                env.declare(
                    name,
                    Binding {
                        value,
                        mutable: false,
                        initialized: true,
                    },
                );
                Ok(Value::Undefined)
            }
            Stmt::Return(arg) => {
                let value = match arg {
                    Some(expr) => self.eval_expr(expr, env)?,
                    None => Value::Undefined,
                };
                Err(Control::Return(value))
            }
            Stmt::If { test, cons, alt } => {
                let condition = self.eval_expr(test, env)?;
                if condition.truthy() {
                    self.eval_stmt(cons, env, label)
                } else if let Some(alt) = alt {
                    self.eval_stmt(alt, env, label)
                } else {
                    Ok(Value::Undefined)
                }
            }
            Stmt::While { test, body } => {
                let mut last = Value::Undefined;
                loop {
                    let condition = self.eval_expr(test, env)?;
                    if !condition.truthy() {
                        break;
                    }
                    match self.eval_stmt(body, env, label) {
                        Ok(value) => last = value,
                        Err(Control::Break(target)) if breaks_here(&target, label) => break,
                        Err(Control::Continue(target)) if breaks_here(&target, label) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(last)
            }
            Stmt::DoWhile { body, test } => {
                let mut last = Value::Undefined;
                loop {
                    match self.eval_stmt(body, env, label) {
                        Ok(value) => last = value,
                        Err(Control::Break(target)) if breaks_here(&target, label) => break,
                        Err(Control::Continue(target)) if breaks_here(&target, label) => {}
                        Err(other) => return Err(other),
                    }
                    let condition = self.eval_expr(test, env)?;
                    if !condition.truthy() {
                        break;
                    }
                }
                Ok(last)
            }
            Stmt::For {
                init,
                test,
                update,
                body,
            } => self.eval_for(init, test, update, body, env, label),
            Stmt::ForOf {
                target,
                right,
                body,
                ..
            } => {
                let iterable = self.eval_expr(right, env)?;
                let items = self.iterate(&iterable)?;
                for item in items {
                    let scope = Env::new(Some(env.clone()), false);
                    self.assign_for_target(target, item, env, &scope)?;
                    match self.eval_stmt(body, &scope, label) {
                        Ok(_) => {}
                        Err(Control::Break(inner)) if breaks_here(&inner, label) => break,
                        Err(Control::Continue(inner)) if breaks_here(&inner, label) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(Value::Undefined)
            }
            Stmt::ForIn {
                target,
                right,
                body,
            } => {
                let subject = self.eval_expr(right, env)?;
                let keys = self.enumerable_keys(&subject);
                for key in keys {
                    let scope = Env::new(Some(env.clone()), false);
                    self.assign_for_target(target, Value::str(&key), env, &scope)?;
                    match self.eval_stmt(body, &scope, label) {
                        Ok(_) => {}
                        Err(Control::Break(inner)) if breaks_here(&inner, label) => break,
                        Err(Control::Continue(inner)) if breaks_here(&inner, label) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(Value::Undefined)
            }
            Stmt::Switch { disc, cases } => {
                let subject = self.eval_expr(disc, env)?;
                let mut start = None;
                for (index, case) in cases.iter().enumerate() {
                    match &case.test {
                        Some(test) => {
                            let candidate = self.eval_expr(test, env)?;
                            if candidate.strict_equals(&subject) {
                                start = Some(index);
                                break;
                            }
                        }
                        None => {
                            if start.is_none() {
                                start = Some(index);
                            }
                        }
                    }
                }
                if let Some(start) = start {
                    let scope = Env::new(Some(env.clone()), false);
                    for case in cases.iter().skip(start) {
                        match self.eval_block(&case.body, &scope) {
                            Ok(_) => {}
                            Err(Control::Break(target)) if breaks_here(&target, label) => break,
                            Err(other) => return Err(other),
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            Stmt::Try {
                block,
                handler,
                finalizer,
            } => {
                let outcome = match self.eval_block(block, env) {
                    Ok(value) => Ok(value),
                    Err(Control::Throw(error)) => match handler {
                        Some((pattern, body)) => {
                            let scope = Env::new(Some(env.clone()), false);
                            if let Some(pattern) = pattern {
                                self.declare_pattern(pattern, error.value.clone(), &scope)?;
                            }
                            self.eval_block(body, &scope)
                        }
                        None => Err(Control::Throw(error)),
                    },
                    Err(other) => Err(other),
                };
                if let Some(finalizer) = finalizer {
                    let finalized = self.eval_block(finalizer, env);
                    if let Err(control) = finalized {
                        return Err(control);
                    }
                }
                outcome
            }
            Stmt::Throw(expr) => {
                let value = self.eval_expr(expr, env)?;
                self.throw_value(value)
            }
            Stmt::Break(target) => Err(Control::Break(target.clone())),
            Stmt::Continue(target) => Err(Control::Continue(target.clone())),
            Stmt::Labeled { label: name, body } => self.eval_stmt(body, env, Some(name)),
            Stmt::Import(decl) => {
                let _ = decl;
                Ok(Value::Undefined)
            }
            Stmt::Export(decl) => match decl {
                ExportDecl::Default(inner) | ExportDecl::Decl(inner) => {
                    self.eval_stmt(inner, env, label)
                }
                ExportDecl::Named { .. } | ExportDecl::All { .. } => Ok(Value::Undefined),
            },
        }
    }

    fn eval_for(
        &mut self,
        init: &Option<ForInit>,
        test: &Option<Expr>,
        update: &Option<Expr>,
        body: &Stmt,
        env: &EnvRef,
        label: Option<&str>,
    ) -> Result<Value, Control> {
        let scope = Env::new(Some(env.clone()), false);
        if let Some(init) = init {
            match init {
                ForInit::Var { kind, decls } => {
                    for (pattern, value) in decls {
                        let value = match value {
                            Some(expr) => self.eval_expr(expr, &scope)?,
                            None => Value::Undefined,
                        };
                        if *kind == VarKind::Var {
                            let target = env.function_scope();
                            self.bind_var_pattern(pattern, value, &target)?;
                        } else {
                            self.declare_pattern(pattern, value, &scope)?;
                        }
                    }
                }
                ForInit::Expr(expr) => {
                    self.eval_expr(expr, &scope)?;
                }
            }
        }
        let mut last = Value::Undefined;
        loop {
            if let Some(test) = test {
                let condition = self.eval_expr(test, &scope)?;
                if !condition.truthy() {
                    break;
                }
            }
            match self.eval_stmt(body, &scope, label) {
                Ok(value) => last = value,
                Err(Control::Break(target)) if breaks_here(&target, label) => break,
                Err(Control::Continue(target)) if breaks_here(&target, label) => {}
                Err(other) => return Err(other),
            }
            if let Some(update) = update {
                self.eval_expr(update, &scope)?;
            }
        }
        Ok(last)
    }

    fn assign_for_target(
        &mut self,
        target: &ForTarget,
        value: Value,
        outer: &EnvRef,
        scope: &EnvRef,
    ) -> Result<(), Control> {
        match target {
            ForTarget::Var { kind, pattern } => {
                if *kind == VarKind::Var {
                    let function_scope = outer.function_scope();
                    self.bind_var_pattern(pattern, value, &function_scope)
                } else {
                    self.declare_pattern(pattern, value, scope)
                }
            }
            ForTarget::Pattern(pattern) => self.declare_pattern(pattern, value, scope),
            ForTarget::Expr(expr) => self.assign_to(expr, value, outer),
        }
    }

    /// Iterate a value with the for-of protocol.
    pub fn iterate(&mut self, value: &Value) -> Result<Vec<Value>, Control> {
        match value {
            Value::Object(object) => {
                let borrowed = object.borrow();
                match &borrowed.kind {
                    Kind::Array => {
                        drop(borrowed);
                        self.array_items(value)
                    }
                    Kind::Set(items) => Ok(items.clone()),
                    Kind::Map(entries) => Ok(entries
                        .iter()
                        .map(|(key, value)| self.new_array(vec![key.clone(), value.clone()]))
                        .collect()),
                    _ => {
                        let length = borrowed
                            .props
                            .get("length")
                            .map(|v| v.to_number())
                            .unwrap_or(0.0);
                        if length > 0.0 {
                            drop(borrowed);
                            let mut items = Vec::new();
                            for index in 0..(length as usize) {
                                items.push(self.get_prop(value, &index.to_string())?);
                            }
                            Ok(items)
                        } else {
                            Err(self.throw_error("TypeError", "value is not iterable"))
                        }
                    }
                }
            }
            Value::Str(text) => Ok(text
                .chars()
                .map(|c| Value::str(c.to_string()))
                .collect()),
            _ => self.type_error("value is not iterable"),
        }
    }

    /// The keys visited by `for...in`: own enumerable keys, then the chain.
    fn enumerable_keys(&self, value: &Value) -> Vec<String> {
        let mut keys = Vec::new();
        let mut current = match value {
            Value::Object(object) => Some(object.clone()),
            _ => None,
        };
        while let Some(object) = current {
            let borrowed = object.borrow();
            for key in borrowed.props.keys() {
                if !borrowed.is_enumerable(key) {
                    continue;
                }
                if !keys.iter().any(|existing| existing == key) {
                    keys.push(key.to_string());
                }
            }
            current = borrowed.prototype.clone();
        }
        keys
    }

    /// Declare a binding in the current scope.
    fn declare_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
        env: &EnvRef,
    ) -> Result<(), Control> {
        for name in pattern_names(pattern) {
            if !env.has_own(&name) {
                env.declare(
                    &name,
                    Binding {
                        value: Value::Undefined,
                        mutable: true,
                        initialized: false,
                    },
                );
            }
        }
        self.bind_pattern(pattern, value, env, true)
    }

    /// Assign to a `var` binding that the hoisting pass already created.
    fn bind_var_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
        target: &EnvRef,
    ) -> Result<(), Control> {
        self.bind_pattern(pattern, value, target, true)
    }

    /// Destructuring assignment to an arbitrary expression target.
    pub fn assign_to(
        &mut self,
        target: &Expr,
        value: Value,
        env: &EnvRef,
    ) -> Result<(), Control> {
        match target {
            Expr::Ident(name) => self.assign_name(env, name, value),
            Expr::Paren(inner) => self.assign_to(inner, value, env),
            Expr::Member { object, prop, .. } => {
                let base = self.eval_expr(object, env)?;
                let key = match prop {
                    MemberProp::Name(name) => name.clone(),
                    MemberProp::Index(index) => {
                        let key = self.eval_expr(index, env)?;
                        self.key_to_string(&key)?
                    }
                };
                self.set_prop(&base, &key, value)
            }
            Expr::Array(elements) => {
                let mut index = 0usize;
                for element in elements {
                    if let Some(element) = element {
                        if let Expr::Spread(inner) = element {
                            let items = self.array_items(&value)?;
                            let tail: Vec<Value> = if index < items.len() {
                                items[index..].to_vec()
                            } else {
                                Vec::new()
                            };
                            let tail = self.new_array(tail);
                            self.assign_to(inner, tail, env)?;
                        } else {
                            let item = self.get_prop(&value, &index.to_string())?;
                            self.assign_to(element, item, env)?;
                        }
                    }
                    index += 1;
                }
                Ok(())
            }
            Expr::Object(props) => {
                for prop in props {
                    match prop {
                        Prop::KeyValue { key, value: source, .. } => {
                            let name = self.prop_key_string(key, env)?;
                            let item = self.get_prop(&value, &name)?;
                            self.assign_to(source, item, env)?;
                        }
                        Prop::Spread(inner) => {
                            let rest = self.new_object();
                            if let Value::Object(source) = &value {
                                let entries: Vec<(String, Value)> = source
                                    .borrow()
                                    .props
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect();
                                for (key, item) in entries {
                                    rest.borrow_mut().props.set(key, item);
                                }
                            }
                            self.assign_to(inner, Value::Object(rest), env)?;
                        }
                        _ => {}
                    }
                }
                Ok(())
            }
            other => self.type_error(format!("cannot assign to {other:?}")),
        }
    }

    /// Build a function object from an AST function node.
    pub fn eval_function(
        &mut self,
        func: &Func,
        env: &EnvRef,
        home: Option<ObjectRef>,
    ) -> Result<Value, Control> {
        if func.is_generator {
            return self.type_error("generator functions are not supported by codets");
        }
        Ok(self.new_user_function(UserFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            body: func.body.clone(),
            closure: env.clone(),
            home,
            is_arrow: func.is_arrow,
            is_async: func.is_async,
            is_generator: false,
        }))
    }

    /// Evaluate a class declaration or expression into a constructor value.
    pub fn eval_class(
        &mut self,
        class: &Class,
        env: &EnvRef,
        name_hint: Option<&str>,
    ) -> Result<Value, Control> {
        let name = class
            .name
            .clone()
            .or_else(|| name_hint.map(|n| n.to_string()));

        let superclass = match &class.superclass {
            Some(expr) => Some(self.eval_expr(expr, env)?),
            None => None,
        };

        // The constructor function object itself.
        let constructor = Object::new(Some(self.prototypes.function.clone()));
        let prototype = Object::new(Some(match &superclass {
            Some(Value::Object(parent)) => match parent.borrow().props.get("prototype") {
                Some(Value::Object(parent_prototype)) => parent_prototype.clone(),
                _ => self.prototypes.object.clone(),
            },
            _ => self.prototypes.object.clone(),
        }));

        let mut ctor: Option<Box<UserFunction>> = None;
        let mut fields: Vec<(String, Option<Expr>)> = Vec::new();

        for member in &class.members {
            match member {
                ClassMember::Method {
                    key,
                    func,
                    is_static,
                    kind,
                } => {
                    let key_string = self.prop_key_string(key, env)?;
                    if *kind == MethodKind::Constructor && !*is_static {
                        ctor = Some(Box::new(UserFunction {
                            name: name.clone(),
                            params: func.params.clone(),
                            body: func.body.clone(),
                            closure: env.clone(),
                            home: Some(prototype.clone()),
                            is_arrow: false,
                            is_async: false,
                            is_generator: false,
                        }));
                        continue;
                    }
                    let home = if *is_static {
                        constructor.clone()
                    } else {
                        prototype.clone()
                    };
                    let value = self.eval_function(func, env, Some(home.clone()))?;
                    match kind {
                        MethodKind::Get => {
                            home.borrow_mut()
                                .accessors
                                .entry(key_string.clone())
                                .or_insert((None, None))
                                .0 = Some(value);
                        }
                        MethodKind::Set => {
                            home.borrow_mut()
                                .accessors
                                .entry(key_string.clone())
                                .or_insert((None, None))
                                .1 = Some(value);
                        }
                        _ => {
                            home.borrow_mut().props.set(key_string.clone(), value);
                        }
                    }
                    // Class members are non-enumerable, like the built-ins.
                    home.borrow_mut().hide_from_enumeration(&key_string);
                }
                ClassMember::Field {
                    key,
                    value,
                    is_static,
                } => {
                    let key_string = self.prop_key_string(key, env)?;
                    if *is_static {
                        let evaluated = match value {
                            Some(expr) => self.eval_expr(expr, env)?,
                            None => Value::Undefined,
                        };
                        constructor.borrow_mut().props.set(key_string.clone(), evaluated);
                        constructor.borrow_mut().hide_from_enumeration(&key_string);
                    } else {
                        fields.push((key_string, value.clone()));
                    }
                }
                ClassMember::StaticBlock(stmts) => {
                    let scope = Env::new(Some(env.clone()), true);
                    *scope.this_value.borrow_mut() = Value::Object(constructor.clone());
                    self.hoist(stmts, &scope, &scope)?;
                    self.eval_block(stmts, &scope)?;
                }
            }
        }

        // Instance field initialisers are stored as a callable, because a
        // runtime `Value` cannot hold AST nodes. `construct` invokes it once per
        // instance with `this` bound.
        if !fields.is_empty() {
            let class_env = env.clone();
            let initialisers = fields.clone();
            let init = self.native("$initFields", 0, move |interp, this, _args| {
                let scope = Env::new(Some(class_env.clone()), true);
                *scope.this_value.borrow_mut() = this.clone();
                for (key, value) in &initialisers {
                    let evaluated = match value {
                        Some(expr) => interp.eval_expr(expr, &scope)?,
                        None => Value::Undefined,
                    };
                    interp.set_prop(this, key, evaluated)?;
                }
                Ok(Value::Undefined)
            });
            constructor.borrow_mut().props.set("$init_fields", init);
        }

        {
            let mut borrowed = constructor.borrow_mut();
            borrowed.callable = Some(Callable::Class { ctor });
            borrowed
                .props
                .set("name", Value::str(name.clone().unwrap_or_default()));
            borrowed.props.set("prototype", Value::Object(prototype.clone()));
            for slot in ["name", "prototype", "length"] {
                borrowed.hide_from_enumeration(slot);
            }
        }
        prototype
            .borrow_mut()
            .props
            .set("constructor", Value::Object(constructor.clone()));
        prototype.borrow_mut().hide_from_enumeration("constructor");

        // A derived class without its own constructor inherits the parent's.
        if let Some(superclass) = &superclass {
            constructor
                .borrow_mut()
                .props
                .set("$super", superclass.clone());
            if let Value::Object(parent) = superclass {
                let parent_prototype = parent.borrow().props.get("prototype").cloned();
                if let Some(parent_prototype) = parent_prototype {
                    prototype.borrow_mut().prototype = match parent_prototype {
                        Value::Object(proto) => Some(proto),
                        _ => None,
                    };
                }
            }
        }

        Ok(Value::Object(constructor))
    }

    /// Evaluate an enum into its runtime object, including reverse mappings.
    fn eval_enum(&mut self, members: &[(String, Option<Expr>)], env: &EnvRef) -> Result<Value, Control> {
        let object = Object::new(Some(self.prototypes.object.clone()));
        let mut auto = 0f64;
        for (name, value) in members {
            let evaluated = match value {
                Some(expr) => {
                    let value = self.eval_expr(expr, env)?;
                    if let Value::Num(n) = value {
                        auto = n + 1.0;
                    }
                    value
                }
                None => {
                    let n = auto;
                    auto += 1.0;
                    Value::Num(n)
                }
            };
            {
                let mut borrowed = object.borrow_mut();
                borrowed.props.set(name.clone(), evaluated.clone());
                if let Value::Num(n) = evaluated {
                    let reverse = format_number(n);
                    borrowed.props.set(reverse, Value::str(name));
                }
            }
        }
        Ok(Value::Object(object))
    }

    /// Evaluate an expression.
    pub fn eval_expr(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, Control> {
        match expr {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::str(s)),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Hole => Ok(Value::Undefined),
            Expr::BigInt(text) => {
                let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                    i128::from_str_radix(hex, 16).ok()
                } else if let Some(bin) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
                    i128::from_str_radix(bin, 2).ok()
                } else if let Some(oct) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
                    i128::from_str_radix(oct, 8).ok()
                } else {
                    text.parse::<i128>().ok()
                };
                match parsed {
                    Some(value) => Ok(Value::BigInt(value)),
                    None => self.range_error(format!("invalid BigInt literal `{text}`")),
                }
            }
            Expr::Ident(name) => {
                if let Some((_, binding)) = env.resolve(name) {
                    if !binding.initialized {
                        return self.type_error(format!(
                            "cannot access `{name}` before initialization"
                        ));
                    }
                    return Ok(binding.value);
                }
                match self.globals.resolve(name) {
                    Some((_, binding)) => Ok(binding.value),
                    None => Err(self.throw_error(
                        "ReferenceError",
                        format!("{name} is not defined"),
                    )),
                }
            }
            Expr::This => Ok(self.current_this(env)),
            Expr::Super => match env.resolve("$super_proto") {
                Some((_, binding)) => Ok(binding.value),
                None => self.type_error("`super` is only valid inside a class method"),
            },
            Expr::Paren(inner) => self.eval_expr(inner, env),
            Expr::Template { quasis, exprs } => {
                let mut text = String::new();
                for (index, quasi) in quasis.iter().enumerate() {
                    text.push_str(quasi);
                    if let Some(expr) = exprs.get(index) {
                        let value = self.eval_expr(expr, env)?;
                        text.push_str(&self.to_string(&value)?);
                    }
                }
                Ok(Value::str(text))
            }
            Expr::TaggedTemplate { tag, quasis, exprs } => {
                let callable = self.eval_expr(tag, env)?;
                let strings = self.new_array(quasis.iter().map(Value::str).collect());
                let raw = self.new_array(quasis.iter().map(Value::str).collect());
                if let Value::Object(strings_object) = &strings {
                    strings_object.borrow_mut().props.set("raw", raw);
                }
                let mut args = vec![strings];
                for expr in exprs {
                    args.push(self.eval_expr(expr, env)?);
                }
                self.call_value(&callable, Value::Undefined, args)
            }
            Expr::Array(elements) => {
                let mut items = Vec::new();
                let mut spread_from: Option<usize> = None;
                for element in elements {
                    match element {
                        Some(Expr::Spread(inner)) => {
                            spread_from = Some(items.len());
                            let inner = self.eval_expr(inner, env)?;
                            items.extend(self.iterate(&inner)?);
                        }
                        Some(expr) => items.push(self.eval_expr(expr, env)?),
                        None => items.push(Value::Undefined),
                    }
                }
                if spread_from.is_some() {
                    // Holes are materialised as undefined; that is what the
                    // iteration protocol does anyway.
                }
                Ok(self.new_array(items))
            }
            Expr::Object(props) => {
                let object = Object::new(Some(self.prototypes.object.clone()));
                for prop in props {
                    match prop {
                        Prop::KeyValue { key, value, .. } => {
                            let name = self.prop_key_string(key, env)?;
                            let evaluated = self.eval_expr(value, env)?;
                            object.borrow_mut().props.set(name, evaluated);
                        }
                        Prop::Method { key, func } => {
                            let name = self.prop_key_string(key, env)?;
                            let value =
                                self.eval_function_with_home(func, env, object.clone())?;
                            object.borrow_mut().props.set(name, value);
                        }
                        Prop::Accessor {
                            key,
                            func,
                            is_getter,
                        } => {
                            let name = self.prop_key_string(key, env)?;
                            let value =
                                self.eval_function_with_home(func, env, object.clone())?;
                            let mut borrowed = object.borrow_mut();
                            let entry = borrowed.accessors.entry(name).or_insert((None, None));
                            if *is_getter {
                                entry.0 = Some(value);
                            } else {
                                entry.1 = Some(value);
                            }
                        }
                        Prop::Spread(source) => {
                            let source_value = self.eval_expr(source, env)?;
                            if let Value::Object(source_object) = &source_value {
                                let entries: Vec<(String, Value)> = {
                                    let borrowed = source_object.borrow();
                                    borrowed
                                        .props
                                        .iter()
                                        .filter(|(key, _)| borrowed.is_enumerable(key))
                                        .map(|(k, v)| (k.clone(), v.clone()))
                                        .collect()
                                };
                                for (key, value) in entries {
                                    object.borrow_mut().props.set(key, value);
                                }
                            }
                        }
                    }
                }
                Ok(Value::Object(object))
            }
            Expr::Func(func) => self.eval_function_with_home(func, env, self.new_object()),
            Expr::Class(class) => self.eval_class(class, env, None),
            Expr::Regex { source, flags } => builtins::regexp::create(self, source, flags),
            Expr::ImportMeta => Ok(env
                .resolve("$import_meta")
                .map(|(_, binding)| binding.value)
                .unwrap_or(Value::Undefined)),
            Expr::ImportCall(specifier) => {
                let request = self.eval_expr(specifier, env)?;
                let request = self.to_string(&request)?;
                let namespace = self.dynamic_import(&request)?;
                Ok(self.resolved_promise(namespace))
            }
            Expr::Unary { op, arg } => self.eval_unary(op, arg, env),
            Expr::Update { op, prefix, arg } => self.eval_update(op, *prefix, arg, env),
            Expr::Binary { op, left, right } => {
                let left = self.eval_expr(left, env)?;
                let right = self.eval_expr(right, env)?;
                self.binary(*op, left, right)
            }
            Expr::Logical { op, left, right } => {
                let left_value = self.eval_expr(left, env)?;
                let short_circuit = match op {
                    LogOp::And => !left_value.truthy(),
                    LogOp::Or => left_value.truthy(),
                    LogOp::Nullish => !left_value.is_nullish(),
                };
                if short_circuit {
                    return Ok(left_value);
                }
                self.eval_expr(right, env)
            }
            Expr::Assign { op, target, value } => self.eval_assign(*op, target, value, env),
            Expr::Cond { test, cons, alt } => {
                let condition = self.eval_expr(test, env)?;
                if condition.truthy() {
                    self.eval_expr(cons, env)
                } else {
                    self.eval_expr(alt, env)
                }
            }
            Expr::Sequence(items) => {
                let mut last = Value::Undefined;
                for item in items {
                    last = self.eval_expr(item, env)?;
                }
                Ok(last)
            }
            Expr::Spread(inner) => self.eval_expr(inner, env),
            Expr::Member { .. } | Expr::Call { .. } => self.eval_chain(expr, env),
            Expr::New { callee, args } => {
                let target = self.eval_expr(callee, env)?;
                let mut evaluated = Vec::new();
                for arg in args {
                    match arg {
                        Expr::Spread(inner) => {
                            let inner = self.eval_expr(inner, env)?;
                            evaluated.extend(self.iterate(&inner)?);
                        }
                        arg => evaluated.push(self.eval_expr(arg, env)?),
                    }
                }
                self.construct(&target, evaluated)
            }
            Expr::Await(inner) => {
                let value = self.eval_expr(inner, env)?;
                self.await_value(value)
            }
            Expr::Yield { .. } => {
                self.type_error("generators are not supported by codets")
            }
        }
    }

    fn eval_function_with_home(
        &mut self,
        func: &Func,
        env: &EnvRef,
        home: ObjectRef,
    ) -> Result<Value, Control> {
        self.eval_function(func, env, Some(home))
    }

    /// Read `this` from the nearest function scope.
    fn current_this(&self, env: &EnvRef) -> Value {
        let mut current = Some(env.clone());
        while let Some(node) = current {
            if node.is_function_scope {
                return node.this_value.borrow().clone();
            }
            current = node.parent.clone();
        }
        Value::Undefined
    }

    /// Evaluate a member/call chain, honouring optional short-circuiting.
    ///
    /// The chain is flattened so that `a?.b.c` stops at the first nullish link
    /// rather than merely replacing that link with `undefined`.
    fn eval_chain(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, Control> {
        // `super(...)` targets the parent constructor rather than the parent
        // prototype, so it cannot go through the generic chain below.
        if let Expr::Call { callee, args, .. } = expr {
            if matches!(&**callee, Expr::Super) {
                let ctor = match env.resolve("$super_ctor") {
                    Some((_, binding)) => binding.value,
                    None => {
                        return self
                            .type_error("`super()` is only valid in a derived constructor")
                    }
                };
                let mut evaluated = Vec::new();
                for arg in args {
                    match arg {
                        Expr::Spread(inner) => {
                            let inner = self.eval_expr(inner, env)?;
                            evaluated.extend(self.iterate(&inner)?);
                        }
                        arg => evaluated.push(self.eval_expr(arg, env)?),
                    }
                }
                let this = self.current_this(env);
                return self.call_value(&ctor, this, evaluated);
            }
        }

        let mut links: Vec<(&Expr, bool)> = Vec::new();
        let mut base = expr;
        loop {
            match base {
                Expr::Member { object, optional, .. } => {
                    links.push((base, *optional));
                    base = object;
                }
                Expr::Call { callee, optional, .. } => {
                    links.push((base, *optional));
                    base = callee;
                }
                _ => break,
            }
        }
        links.reverse();

        let mut current = self.eval_expr(base, env)?;
        // `super.method()` must run with the current `this`, not with the
        // prototype object that holds the method.
        let super_base = matches!(base, Expr::Super);
        let mut this_for_call = if super_base {
            Some(self.current_this(env))
        } else {
            None
        };
        let mut first_link = true;
        for (link, optional) in links {
            if optional && current.is_nullish() {
                return Ok(Value::Undefined);
            }
            match link {
                Expr::Member { prop, .. } => {
                    let key = match prop {
                        MemberProp::Name(name) => name.clone(),
                        MemberProp::Index(index) => {
                            let key = self.eval_expr(index, env)?;
                            self.key_to_string(&key)?
                        }
                    };
                    if !(super_base && first_link) {
                        this_for_call = Some(current.clone());
                    }
                    current = self.get_prop(&current, &key)?;
                }
                Expr::Call { args, .. } => {
                    let mut evaluated = Vec::new();
                    for arg in args {
                        match arg {
                            Expr::Spread(inner) => {
                                let inner = self.eval_expr(inner, env)?;
                                evaluated.extend(self.iterate(&inner)?);
                            }
                            arg => evaluated.push(self.eval_expr(arg, env)?),
                        }
                    }
                    let this = this_for_call.take().unwrap_or(Value::Undefined);
                    current = self.call_value(&current, this, evaluated)?;
                }
                _ => unreachable!("only member and call links are collected"),
            }
            first_link = false;
        }
        Ok(current)
    }

    fn eval_unary(&mut self, op: &UnOp, arg: &Expr, env: &EnvRef) -> Result<Value, Control> {
        match op {
            UnOp::Typeof => {
                // `typeof undefinedName` must not throw.
                if let Expr::Ident(name) = arg {
                    if env.resolve(name).is_none() {
                        return Ok(Value::str("undefined"));
                    }
                }
                let value = self.eval_expr(arg, env)?;
                Ok(Value::str(value.type_of()))
            }
            UnOp::Delete => match arg {
                Expr::Member { object, prop, .. } => {
                    let base = self.eval_expr(object, env)?;
                    let key = match prop {
                        MemberProp::Name(name) => name.clone(),
                        MemberProp::Index(index) => {
                            let key = self.eval_expr(index, env)?;
                            self.key_to_string(&key)?
                        }
                    };
                    Ok(Value::Bool(self.delete_prop(&base, &key)?))
                }
                _ => Ok(Value::Bool(true)),
            },
            UnOp::Void => {
                self.eval_expr(arg, env)?;
                Ok(Value::Undefined)
            }
            UnOp::Not => {
                let value = self.eval_expr(arg, env)?;
                Ok(Value::Bool(!value.truthy()))
            }
            UnOp::Minus => {
                let value = self.eval_expr(arg, env)?;
                if let Value::BigInt(n) = value {
                    return Ok(Value::BigInt(-n));
                }
                let number = self.to_number_value(&value)?;
                Ok(Value::Num(-number))
            }
            UnOp::Plus => {
                let value = self.eval_expr(arg, env)?;
                let number = self.to_number_value(&value)?;
                Ok(Value::Num(number))
            }
            UnOp::BitNot => {
                let value = self.eval_expr(arg, env)?;
                let number = self.to_number_value(&value)?;
                Ok(Value::Num(!to_int32(number) as f64))
            }
        }
    }

    fn eval_update(
        &mut self,
        op: &UpdateOp,
        prefix: bool,
        arg: &Expr,
        env: &EnvRef,
    ) -> Result<Value, Control> {
        let old = self.eval_expr(arg, env)?;
        let old_number = self.to_number_value(&old)?;
        let new_number = match op {
            UpdateOp::Inc => old_number + 1.0,
            UpdateOp::Dec => old_number - 1.0,
        };
        self.assign_to(arg, Value::Num(new_number), env)?;
        Ok(Value::Num(if prefix { new_number } else { old_number }))
    }

    fn eval_assign(
        &mut self,
        op: AssignOp,
        target: &Expr,
        value: &Expr,
        env: &EnvRef,
    ) -> Result<Value, Control> {
        // Short-circuiting assignments evaluate the right side only when needed.
        if matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Nullish) {
            let current = self.eval_expr(target, env)?;
            let skip = match op {
                AssignOp::And => !current.truthy(),
                AssignOp::Or => current.truthy(),
                _ => !current.is_nullish(),
            };
            if skip {
                return Ok(current);
            }
            let evaluated = self.eval_expr(value, env)?;
            self.assign_to(target, evaluated.clone(), env)?;
            return Ok(evaluated);
        }

        let evaluated = self.eval_expr(value, env)?;
        let result = if op == AssignOp::Assign {
            evaluated
        } else {
            let current = self.eval_expr(target, env)?;
            let binary = match op {
                AssignOp::Add => BinOp::Add,
                AssignOp::Sub => BinOp::Sub,
                AssignOp::Mul => BinOp::Mul,
                AssignOp::Div => BinOp::Div,
                AssignOp::Mod => BinOp::Mod,
                AssignOp::Exp => BinOp::Exp,
                AssignOp::BitAnd => BinOp::BitAnd,
                AssignOp::BitOr => BinOp::BitOr,
                AssignOp::BitXor => BinOp::BitXor,
                AssignOp::Shl => BinOp::Shl,
                AssignOp::Shr => BinOp::Shr,
                AssignOp::UShr => BinOp::UShr,
                _ => unreachable!("logical assignment handled above"),
            };
            self.binary(binary, current, evaluated)?
        };
        self.assign_to(target, result.clone(), env)?;
        Ok(result)
    }

    /// The binary operator implementations, including the `+` overload.
    pub fn binary(&mut self, op: BinOp, left: Value, right: Value) -> Result<Value, Control> {
        if op == BinOp::Add {
            let left_primitive = self.to_primitive(&left)?;
            let right_primitive = self.to_primitive(&right)?;
            if matches!(left_primitive, Value::Str(_)) || matches!(right_primitive, Value::Str(_)) {
                let mut text = self.to_string(&left_primitive)?;
                text.push_str(&self.to_string(&right_primitive)?);
                return Ok(Value::str(text));
            }
            if let (Value::BigInt(a), Value::BigInt(b)) = (&left_primitive, &right_primitive) {
                return Ok(Value::BigInt(a.wrapping_add(*b)));
            }
            let a = left_primitive.to_number();
            let b = right_primitive.to_number();
            return Ok(Value::Num(a + b));
        }

        if let (Value::BigInt(a), Value::BigInt(b)) = (&left, &right) {
            return match op {
                BinOp::Sub => Ok(Value::BigInt(a.wrapping_sub(*b))),
                BinOp::Mul => Ok(Value::BigInt(a.wrapping_mul(*b))),
                BinOp::Div => {
                    if *b == 0 {
                        self.range_error("division by zero")
                    } else {
                        Ok(Value::BigInt(a / b))
                    }
                }
                _ => {
                    let a = *a as f64;
                    let b = *b as f64;
                    self.binary(op, Value::Num(a), Value::Num(b))
                }
            };
        }

        match op {
            BinOp::Eq => Ok(Value::Bool(left.loose_equals(&right))),
            BinOp::Ne => Ok(Value::Bool(!left.loose_equals(&right))),
            BinOp::StrictEq => Ok(Value::Bool(left.strict_equals(&right))),
            BinOp::StrictNe => Ok(Value::Bool(!left.strict_equals(&right))),
            BinOp::In => {
                let key = self.key_to_string(&left)?;
                if !matches!(right, Value::Object(_)) {
                    return self.type_error("the right operand of `in` must be an object");
                }
                Ok(Value::Bool(self.has_prop(&right, &key)))
            }
            BinOp::Instanceof => {
                let prototype = self.get_prop(&right, "prototype")?;
                let prototype = match prototype {
                    Value::Object(prototype) => prototype,
                    _ => {
                        return self
                            .type_error("the right operand of `instanceof` must be a function")
                    }
                };
                let mut current = match &left {
                    Value::Object(object) => object.borrow().prototype.clone(),
                    _ => None,
                };
                while let Some(node) = current {
                    if Rc::ptr_eq(&node, &prototype) {
                        return Ok(Value::Bool(true));
                    }
                    current = node.borrow().prototype.clone();
                }
                Ok(Value::Bool(false))
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                if let (Value::Str(a), Value::Str(b)) = (&left, &right) {
                    let ordering = a.cmp(b);
                    return Ok(Value::Bool(match op {
                        BinOp::Lt => ordering.is_lt(),
                        BinOp::Gt => ordering.is_gt(),
                        BinOp::Le => !ordering.is_gt(),
                        _ => !ordering.is_lt(),
                    }));
                }
                let a = self.to_number_value(&left)?;
                let b = self.to_number_value(&right)?;
                if a.is_nan() || b.is_nan() {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(match op {
                    BinOp::Lt => a < b,
                    BinOp::Gt => a > b,
                    BinOp::Le => a <= b,
                    _ => a >= b,
                }))
            }
            _ => {
                let a = self.to_number_value(&left)?;
                let b = self.to_number_value(&right)?;
                let result = match op {
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Mod => a % b,
                    BinOp::Exp => a.powf(b),
                    BinOp::BitAnd => (to_int32(a) & to_int32(b)) as f64,
                    BinOp::BitOr => (to_int32(a) | to_int32(b)) as f64,
                    BinOp::BitXor => (to_int32(a) ^ to_int32(b)) as f64,
                    BinOp::Shl => (to_int32(a) << (to_uint32(b) & 31)) as f64,
                    BinOp::Shr => (to_int32(a) >> (to_uint32(b) & 31)) as f64,
                    BinOp::UShr => (to_uint32(a) >> (to_uint32(b) & 31)) as f64,
                    _ => unreachable!("comparison and equality handled above"),
                };
                Ok(Value::Num(result))
            }
        }
    }

    // ------------------------------------------------------------- promises

    /// A promise that is already fulfilled.
    pub fn resolved_promise(&self, value: Value) -> Value {
        let state = Rc::new(PromiseState::new());
        *state.state.borrow_mut() = PromiseStatus::Fulfilled(value);
        self.new_promise(state)
    }

    /// A promise that is already rejected.
    pub fn rejected_promise(&self, reason: Value) -> Value {
        let state = Rc::new(PromiseState::new());
        *state.state.borrow_mut() = PromiseStatus::Rejected(reason);
        self.new_promise(state)
    }

    /// Wrap promise state in a promise object.
    pub fn new_promise(&self, state: Rc<PromiseState>) -> Value {
        let object = Object::with_kind(Kind::Promise(state), Some(self.prototypes.promise.clone()));
        Value::Object(object)
    }

    /// Extract the shared state of a promise value.
    pub fn promise_state(&self, value: &Value) -> Option<Rc<PromiseState>> {
        match value {
            Value::Object(object) => match &object.borrow().kind {
                Kind::Promise(state) => Some(state.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Queue a microtask.
    pub fn queue_microtask(&mut self, task: Microtask) {
        self.microtasks.push_back(task);
    }

    /// Fulfil a promise, scheduling its reactions.
    pub fn fulfill(&mut self, promise: &Value, value: Value) {
        let state = match self.promise_state(promise) {
            Some(state) => state,
            None => return,
        };
        {
            let mut current = state.state.borrow_mut();
            if !matches!(*current, PromiseStatus::Pending) {
                return;
            }
            *current = PromiseStatus::Fulfilled(value.clone());
        }
        let reactions: Vec<(Value, Value)> = state.reactions.borrow_mut().drain(..).collect();
        for (on_fulfilled, _) in reactions {
            self.schedule_reaction(on_fulfilled, value.clone());
        }
    }

    /// Reject a promise, scheduling its reactions.
    pub fn reject(&mut self, promise: &Value, reason: Value) {
        let state = match self.promise_state(promise) {
            Some(state) => state,
            None => return,
        };
        {
            let mut current = state.state.borrow_mut();
            if !matches!(*current, PromiseStatus::Pending) {
                return;
            }
            *current = PromiseStatus::Rejected(reason.clone());
        }
        let reactions: Vec<(Value, Value)> = state.reactions.borrow_mut().drain(..).collect();
        for (_, on_rejected) in reactions {
            self.schedule_reaction(on_rejected, reason.clone());
        }
    }

    fn schedule_reaction(&mut self, handler: Value, argument: Value) {
        let is_callable = match &handler {
            Value::Object(object) => object.borrow().is_callable(),
            _ => false,
        };
        if !is_callable {
            return;
        }
        let handler: Value = handler;
        self.queue_microtask(Box::new(move |interp| {
            interp.call_value(&handler, Value::Undefined, vec![argument])?;
            Ok(())
        }));
    }

    /// Register reactions on a promise.
    pub fn add_reaction(&mut self, promise: &Value, on_fulfilled: Value, on_rejected: Value) {
        let state = match self.promise_state(promise) {
            Some(state) => state,
            None => return,
        };
        let settled = state.state.borrow().clone();
        match settled {
            PromiseStatus::Pending => {
                state.reactions.borrow_mut().push((on_fulfilled, on_rejected));
            }
            PromiseStatus::Fulfilled(value) => self.schedule_reaction(on_fulfilled, value),
            PromiseStatus::Rejected(reason) => self.schedule_reaction(on_rejected, reason),
        }
    }

    /// `await`: drive the queues until the promise settles.
    pub fn await_value(&mut self, value: Value) -> Result<Value, Control> {
        let state = match self.promise_state(&value) {
            Some(state) => state,
            // `await` on a non-promise yields it unchanged.
            None => return Ok(value),
        };
        loop {
            match state.state.borrow().clone() {
                PromiseStatus::Fulfilled(value) => return Ok(value),
                PromiseStatus::Rejected(reason) => {
                    return self.throw_value(reason);
                }
                PromiseStatus::Pending => {}
            }
            if !self.run_one_step()? {
                return Err(self.throw_error(
                    "Error",
                    "await: the promise never settles and no further events are pending",
                ));
            }
        }
    }

    /// Run one microtask or timer. Returns false when nothing is left to do.
    pub fn run_one_step(&mut self) -> Result<bool, Control> {
        if let Some(task) = self.microtasks.pop_front() {
            task(self)?;
            return Ok(true);
        }
        if self.timers.is_empty() {
            return Ok(false);
        }
        // Run every timer due at the earliest time, advancing the virtual clock.
        let earliest = self
            .timers
            .iter()
            .map(|timer| timer.due)
            .fold(f64::INFINITY, f64::min);
        self.clock_ms = self.clock_ms.max(earliest);
        let mut due: Vec<Timer> = Vec::new();
        let mut index = 0;
        while index < self.timers.len() {
            if self.timers[index].due <= self.clock_ms {
                due.push(self.timers.remove(index));
            } else {
                index += 1;
            }
        }
        for timer in due {
            self.call_value(&timer.callback, Value::Undefined, timer.args.clone())?;
            if let Some(interval) = timer.interval {
                self.timers.push(Timer {
                    id: timer.id,
                    due: self.clock_ms + interval,
                    callback: timer.callback,
                    args: timer.args,
                    interval: Some(interval),
                });
            }
        }
        Ok(true)
    }

    /// Drain microtasks and then timers, up to the given virtual time.
    pub fn run_event_loop(&mut self) -> Result<(), Control> {
        while self.run_one_step()? {}
        Ok(())
    }

    /// Schedule a timer.
    pub fn set_timer(&mut self, callback: Value, delay_ms: f64, args: Vec<Value>, interval: bool) -> u64 {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        let interval = if interval {
            Some(delay_ms.max(1.0))
        } else {
            None
        };
        self.timers.push(Timer {
            id,
            due: self.clock_ms + delay_ms.max(0.0),
            callback,
            args,
            interval,
        });
        id
    }

    /// Cancel a timer by id.
    pub fn clear_timer(&mut self, id: u64) {
        self.timers.retain(|timer| timer.id != id);
    }

    /// Load a module by specifier, used by dynamic `import()`.
    ///
    /// Dynamic import needs the loader, which lives outside the interpreter, so
    /// it is reported as unsupported rather than silently resolving the wrong
    /// path. Use [`crate::Runtime::run_module`] instead.
    pub fn dynamic_import(&mut self, specifier: &str) -> Result<Value, Control> {
        Err(self.throw_error(
            "Error",
            format!(
                "dynamic import of `{specifier}` is not supported; use the static syntax `import ... from` instead"
            ),
        ))
    }
}

/// The callable of an object, cloned out so the borrow can be released.
fn clone_callable(callable: &Callable) -> Callable {
    match callable {
        Callable::Native(function) => Callable::Native(function.clone()),
        Callable::User(user) => Callable::User(Box::new(UserFunction {
            name: user.name.clone(),
            params: user.params.clone(),
            body: user.body.clone(),
            closure: user.closure.clone(),
            home: user.home.clone(),
            is_arrow: user.is_arrow,
            is_async: user.is_async,
            is_generator: user.is_generator,
        })),
        Callable::Class { ctor } => Callable::Class {
            ctor: ctor.as_ref().map(|ctor| {
                Box::new(UserFunction {
                    name: ctor.name.clone(),
                    params: ctor.params.clone(),
                    body: ctor.body.clone(),
                    closure: ctor.closure.clone(),
                    home: ctor.home.clone(),
                    is_arrow: ctor.is_arrow,
                    is_async: ctor.is_async,
                    is_generator: ctor.is_generator,
                })
            }),
        },
    }
}

/// True when a `break` / `continue` target applies to the enclosing construct.
fn breaks_here(target: &Option<String>, label: Option<&str>) -> bool {
    match (target, label) {
        (None, _) => true,
        (Some(target), Some(label)) => target == label,
        (Some(_), None) => false,
    }
}

/// The integer value of a property key, when it is an array index.
fn index_of(key: &str) -> Option<usize> {
    if is_array_index(key) {
        key.parse::<usize>().ok()
    } else {
        None
    }
}

/// ToInt32.
pub fn to_int32(n: f64) -> i32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    let truncated = n.trunc();
    let modulo = truncated.rem_euclid(4294967296.0);
    if modulo >= 2147483648.0 {
        (modulo - 4294967296.0) as i32
    } else {
        modulo as i32
    }
}

/// ToUint32.
pub fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    n.trunc().rem_euclid(4294967296.0) as u32
}

/// Every name bound by a pattern.
pub fn pattern_names(pattern: &Pattern) -> Vec<String> {
    let mut names = Vec::new();
    collect_names(pattern, &mut names);
    names
}

fn collect_names(pattern: &Pattern, out: &mut Vec<String>) {
    match pattern {
        Pattern::Ident(name) => out.push(name.clone()),
        Pattern::Default(inner, _) => collect_names(inner, out),
        Pattern::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                collect_names(element, out);
            }
            if let Some(rest) = rest {
                collect_names(rest, out);
            }
        }
        Pattern::Object { props, rest } => {
            for prop in props {
                collect_names(&prop.value, out);
            }
            if let Some(rest) = rest {
                collect_names(rest, out);
            }
        }
    }
}
