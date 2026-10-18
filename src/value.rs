//! Runtime values, objects, functions and lexical environments.
//!
//! The value model follows JavaScript closely enough that observable behaviour
//! matches: primitives are copied, objects are shared through `Rc<RefCell<..>>`,
//! property lookup walks a prototype chain, and `var` / `let` binding rules are
//! represented explicitly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::ast::{FuncBody, Param};

/// A shared reference to an object.
pub type ObjectRef = Rc<RefCell<Object>>;

/// A lexical environment, shared between closures.
pub type EnvRef = Rc<Env>;

/// The signature of a Rust function exposed to scripts.
pub type NativeFn = Rc<dyn Fn(&mut crate::interp::Interp, &Value, &[Value]) -> Result<Value, crate::interp::Control>>;

/// A JavaScript value.
#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    /// BigInt is stored as an `i128`, which covers the range plugin code uses.
    BigInt(i128),
    Object(ObjectRef),
}

impl Value {
    /// Build a string value.
    pub fn str(text: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(text.as_ref()))
    }

    /// True for `undefined` and `null`, the values optional chaining skips.
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    /// The result of the `typeof` operator.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::BigInt(_) => "bigint",
            Value::Object(object) => match &object.borrow().callable {
                Some(_) => "function",
                None => "object",
            },
        }
    }

    /// ToBoolean coercion.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::BigInt(n) => *n != 0,
            Value::Object(_) => true,
        }
    }

    /// ToNumber coercion for primitives. Objects call `valueOf` / `toString`
    /// through the interpreter before reaching here.
    pub fn to_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(true) => 1.0,
            Value::Bool(false) => 0.0,
            Value::Num(n) => *n,
            Value::Str(s) => parse_number(s),
            Value::BigInt(n) => *n as f64,
            Value::Object(_) => f64::NAN,
        }
    }

    /// ToString coercion for primitives.
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Undefined => "undefined".to_string(),
            Value::Null => "null".to_string(),
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            Value::Num(n) => format_number(*n),
            Value::Str(s) => s.to_string(),
            Value::BigInt(n) => n.to_string(),
            Value::Object(object) => {
                let borrowed = object.borrow();
                match &borrowed.callable {
                    Some(_) => match &borrowed.function_name() {
                        Some(name) => format!("function {name}() {{ [native code] }}"),
                        None => "function () { [native code] }".to_string(),
                    },
                    None => format!("[object {}]", borrowed.class_name()),
                }
            }
        }
    }

    /// The value's own `[[Class]]`-ish name, used by `Object.prototype.toString`.
    pub fn class_name(&self) -> String {
        match self {
            Value::Object(object) => object.borrow().class_name(),
            other => other.type_of().to_string(),
        }
    }

    /// Structural equality used by `===`.
    pub fn strict_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::BigInt(a), Value::BigInt(b)) => a == b,
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// The `==` algorithm, including the string/number coercions.
    pub fn loose_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null | Value::Undefined, Value::Null | Value::Undefined) => true,
            (Value::Num(a), Value::Num(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(_), _) => Value::Num(self.to_number()).loose_equals(other),
            (_, Value::Bool(_)) => self.loose_equals(&Value::Num(other.to_number())),
            (Value::Num(_), Value::Str(_)) => {
                Value::Num(self.to_number()).loose_equals(&Value::Num(other.to_number()))
            }
            (Value::Str(_), Value::Num(_)) => {
                Value::Num(self.to_number()).loose_equals(&Value::Num(other.to_number()))
            }
            (Value::Object(_), Value::Undefined | Value::Null) => false,
            _ => self.strict_equals(other),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => f.write_str("undefined"),
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Num(n) => write!(f, "{n}"),
            Value::Str(s) => write!(f, "\"{s}\""),
            Value::BigInt(n) => write!(f, "{n}n"),
            Value::Object(object) => write!(f, "<{}>", object.borrow().class_name()),
        }
    }
}

/// Format a number the way JavaScript does.
///
/// Rust's default float formatting already produces the shortest round-trip
/// representation, so only the exponent thresholds differ from the language
/// spec (JavaScript switches to exponential notation at 1e21 and below 1e-6).
pub fn format_number(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    let magnitude = n.abs();
    if magnitude >= 1e21 || magnitude < 1e-6 {
        let formatted = format!("{n:e}");
        // `1.5e21` -> `1.5e+21`, and drop a trailing `.0`.
        return formatted.replace("e", "e+").replace("e+-", "e-").replace(".0e", "e");
    }
    format!("{n}")
}

/// Parse a string as a number the way `Number(...)` does.
pub fn parse_number(text: &str) -> f64 {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    match trimmed {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    if let Some(bin) = trimmed.strip_prefix("0b").or_else(|| trimmed.strip_prefix("0B")) {
        return u64::from_str_radix(bin, 2).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// Insertion-ordered property table.
///
/// JavaScript orders integer-like keys ascending before the remaining keys in
/// insertion order, which [`PropMap::keys`] reproduces so that `Object.keys` and
/// `for...in` agree with a real engine.
#[derive(Clone, Default)]
pub struct PropMap {
    entries: Vec<Option<(String, Value)>>,
    index: HashMap<String, usize>,
}

impl PropMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.index.get(key).and_then(|i| self.entries[*i].as_ref().map(|(_, v)| v))
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self.index.get(key) {
            Some(i) => self.entries[*i].as_mut().map(|(_, v)| v),
            None => None,
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(i) = self.index.get(&key) {
            self.entries[*i] = Some((key, value));
            return;
        }
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push(Some((key, value)));
    }

    pub fn remove(&mut self, key: &str) -> bool {
        match self.index.remove(key) {
            Some(i) => {
                self.entries[i] = None;
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    /// Live property names in JavaScript key order.
    pub fn keys(&self) -> Vec<&str> {
        let mut indices: Vec<&str> = Vec::new();
        let mut others: Vec<&str> = Vec::new();
        for entry in self.entries.iter().flatten() {
            if is_array_index(&entry.0) {
                indices.push(&entry.0);
            } else {
                others.push(&entry.0);
            }
        }
        indices.sort_by_key(|k| k.parse::<u64>().unwrap_or(0));
        indices.extend(others);
        indices
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().filter_map(|e| e.as_ref().map(|(k, v)| (k, v)))
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

/// True when `key` is a canonical array index (`"0"`, `"42"`, but not `"01"`).
pub fn is_array_index(key: &str) -> bool {
    if key.is_empty() || key.len() > 10 {
        return false;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if key.len() > 1 && key.starts_with('0') {
        return false;
    }
    key.parse::<u32>().is_ok()
}

/// A user-defined function body plus the environment it closed over.
pub struct UserFunction {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub body: FuncBody,
    pub closure: EnvRef,
    /// `[[HomeObject]]`, used to resolve `super`.
    pub home: Option<ObjectRef>,
    pub is_arrow: bool,
    pub is_async: bool,
    pub is_generator: bool,
}

/// What a callable object does when invoked.
pub enum Callable {
    User(Box<UserFunction>),
    Native(NativeFn),
    /// A class constructor. `ctor` is absent for a class without a constructor
    /// (or with only an implicit one).
    Class { ctor: Option<Box<UserFunction>> },
}

/// Extra slots that distinguish the built-in object flavours.
pub enum Kind {
    Plain,
    Array,
    /// `Map` / `Set` entries; short-lived so a `Vec` scan is the right structure.
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
    RegExp(Rc<crate::builtins::regexp::CompiledRegex>),
    Date(f64),
    Error,
    Promise(Rc<PromiseState>),
}

/// The state of a promise, shared between the promise and its reactions.
pub struct PromiseState {
    pub state: RefCell<PromiseStatus>,
    /// Reactions queued before the state settled: (on_fulfilled, on_rejected).
    pub reactions: RefCell<Vec<(Value, Value)>>,
}

#[derive(Clone)]
pub enum PromiseStatus {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

impl PromiseState {
    pub fn new() -> Self {
        Self {
            state: RefCell::new(PromiseStatus::Pending),
            reactions: RefCell::new(Vec::new()),
        }
    }
}

/// An object: an ordered property table, a prototype, and a flavour.
///
/// Accessor properties (getters and setters) are stored apart from data
/// properties so that `Object.getOwnPropertyDescriptor`-style lookups and
/// assignment behave correctly.
pub struct Object {
    pub props: PropMap,
    /// Getter and setter pairs, keyed by property name.
    pub accessors: HashMap<String, (Option<Value>, Option<Value>)>,
    /// Properties that `for...in`, `Object.keys` and `JSON.stringify` skip.
    ///
    /// Built-in and class methods are non-enumerable in JavaScript, and code in
    /// the wild relies on that: `for (const k in instance)` must not walk into
    /// `Object.prototype`'s methods.
    pub non_enumerable: std::collections::HashSet<String>,
    pub prototype: Option<ObjectRef>,
    pub kind: Kind,
    pub callable: Option<Callable>,
}

impl Object {
    pub fn new(prototype: Option<ObjectRef>) -> ObjectRef {
        Rc::new(RefCell::new(Object {
            props: PropMap::new(),
            accessors: HashMap::new(),
            non_enumerable: std::collections::HashSet::new(),
            prototype,
            kind: Kind::Plain,
            callable: None,
        }))
    }

    pub fn with_kind(kind: Kind, prototype: Option<ObjectRef>) -> ObjectRef {
        Rc::new(RefCell::new(Object {
            props: PropMap::new(),
            accessors: HashMap::new(),
            non_enumerable: std::collections::HashSet::new(),
            prototype,
            kind,
            callable: None,
        }))
    }

    /// True when `key` participates in enumeration.
    pub fn is_enumerable(&self, key: &str) -> bool {
        !self.non_enumerable.contains(key)
    }

    /// Mark a property non-enumerable, as the built-ins and class members are.
    pub fn hide_from_enumeration(&mut self, key: &str) {
        self.non_enumerable.insert(key.to_string());
    }

    /// Own property names in JavaScript order, excluding non-enumerable ones.
    pub fn enumerable_keys(&self) -> Vec<String> {
        self.props
            .keys()
            .into_iter()
            .filter(|key| self.is_enumerable(key))
            .map(|key| key.to_string())
            .collect()
    }

    /// The name of the callable, if this object is one.
    pub fn function_name(&self) -> Option<String> {
        self.props.get("name").map(|v| v.to_display_string())
    }

    pub fn is_array(&self) -> bool {
        matches!(self.kind, Kind::Array)
    }

    pub fn is_callable(&self) -> bool {
        self.callable.is_some()
    }

    pub fn class_name(&self) -> String {
        match &self.kind {
            Kind::Array => "Array".to_string(),
            Kind::Map(_) => "Map".to_string(),
            Kind::Set(_) => "Set".to_string(),
            Kind::RegExp(_) => "RegExp".to_string(),
            Kind::Date(_) => "Date".to_string(),
            Kind::Error => "Error".to_string(),
            Kind::Promise(_) => "Promise".to_string(),
            Kind::Plain => {
                if self.callable.is_some() {
                    "Function".to_string()
                } else {
                    "Object".to_string()
                }
            }
        }
    }

    /// The array length, maintained as a real property so that
    /// `arr.length = n` truncates exactly like JavaScript.
    pub fn array_length(&self) -> usize {
        match self.props.get("length") {
            Some(Value::Num(n)) if *n >= 0.0 => *n as usize,
            _ => 0,
        }
    }
}

/// A `var` / `let` / `const` binding.
pub struct Binding {
    pub value: Value,
    pub mutable: bool,
    /// False until the declaration has run, which is what makes the temporal
    /// dead zone observable.
    pub initialized: bool,
}

/// A lexical environment: one scope's bindings plus a link to its parent.
pub struct Env {
    pub parent: Option<EnvRef>,
    pub bindings: RefCell<HashMap<String, Binding>>,
    /// True for function (or module) scopes, which is where `var` binds.
    pub is_function_scope: bool,
    /// The `this` value inside a function body.
    pub this_value: RefCell<Value>,
}

impl Env {
    pub fn new(parent: Option<EnvRef>, is_function_scope: bool) -> EnvRef {
        Rc::new(Env {
            parent,
            bindings: RefCell::new(HashMap::new()),
            is_function_scope,
            this_value: RefCell::new(Value::Undefined),
        })
    }

    /// Redeclare-free binding creation (last write wins), used by declarations.
    pub fn declare(&self, name: &str, binding: Binding) {
        self.bindings.borrow_mut().insert(name.to_string(), binding);
    }

    /// True when the name is bound in this scope.
    pub fn has_own(&self, name: &str) -> bool {
        self.bindings.borrow().contains_key(name)
    }

    pub fn get_own(&self, name: &str) -> Option<Value> {
        self.bindings
            .borrow()
            .get(name)
            .map(|binding| binding.value.clone())
    }

    /// Look a name up the scope chain, returning the environment that owns it.
    pub fn resolve(self: &EnvRef, name: &str) -> Option<(EnvRef, Binding)> {
        let mut current = Some(self.clone());
        while let Some(env) = current {
            let found = env.bindings.borrow().get(name).map(|binding| Binding {
                value: binding.value.clone(),
                mutable: binding.mutable,
                initialized: binding.initialized,
            });
            if let Some(binding) = found {
                return Some((env.clone(), binding));
            }
            current = env.parent.clone();
        }
        None
    }

    /// The innermost function scope, where `var` belongs.
    pub fn function_scope(self: &EnvRef) -> EnvRef {
        let mut current = self.clone();
        loop {
            if current.is_function_scope {
                return current;
            }
            match &current.parent {
                Some(parent) => current = parent.clone(),
                None => return current,
            }
        }
    }
}
