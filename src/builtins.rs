//! Built-in objects, prototypes and global functions.
//!
//! Everything a script can reach without importing anything lives here. Each
//! constructor's methods are installed on the shared prototype objects created
//! by [`create_prototypes`], so `Object` identity behaves as scripts expect.

use std::collections::HashMap;
use std::rc::Rc;

use crate::interp::{to_int32, Control, Interp};
use crate::value::*;

/// The shared prototypes, created before the global object exists.
pub struct Prototypes {
    pub object: ObjectRef,
    pub function: ObjectRef,
    pub array: ObjectRef,
    pub string: ObjectRef,
    pub number: ObjectRef,
    pub boolean: ObjectRef,
    pub regexp: ObjectRef,
    pub error: HashMap<String, ObjectRef>,
    pub map: ObjectRef,
    pub set: ObjectRef,
    pub date: ObjectRef,
    pub promise: ObjectRef,
}

/// Create every prototype object, wiring their inheritance.
pub fn create_prototypes() -> Prototypes {
    // Object.prototype is the root, so it has no prototype of its own.
    let object = Object::new(None);
    let function = Object::new(Some(object.clone()));
    let array = Object::new(Some(object.clone()));
    let string = Object::new(Some(object.clone()));
    let number = Object::new(Some(object.clone()));
    let boolean = Object::new(Some(object.clone()));
    let regexp = Object::new(Some(object.clone()));
    let map = Object::new(Some(object.clone()));
    let set = Object::new(Some(object.clone()));
    let date = Object::new(Some(object.clone()));
    let promise = Object::new(Some(object.clone()));

    let mut error = HashMap::new();
    let error_root = Object::new(Some(object.clone()));
    error.insert("Error".to_string(), error_root.clone());
    for kind in [
        "TypeError",
        "RangeError",
        "SyntaxError",
        "ReferenceError",
        "EvalError",
        "URIError",
    ] {
        error.insert(kind.to_string(), Object::new(Some(error_root.clone())));
    }

    Prototypes {
        object,
        function,
        array,
        string,
        number,
        boolean,
        regexp,
        error,
        map,
        set,
        date,
        promise,
    }
}

/// The argument at `index`, or `undefined`.
fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

/// Install a method on a prototype or object.
fn def<F>(interp: &Interp, target: &ObjectRef, name: &str, length: usize, body: F)
where
    F: Fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control> + 'static,
{
    let value = interp.native(name, length, body);
    let mut borrowed = target.borrow_mut();
    borrowed.props.set(name, value);
    // Built-in methods are non-enumerable, which is what keeps
    // `for (const k in someObject)` from walking into Object.prototype.
    borrowed.hide_from_enumeration(name);
}

/// Install an accessor pair on a prototype.
fn def_accessor(
    interp: &Interp,
    target: &ObjectRef,
    name: &str,
    getter: Option<fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control>>,
    setter: Option<fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control>>,
) {
    let mut borrowed = target.borrow_mut();
    let entry = borrowed.accessors.entry(name.to_string()).or_insert((None, None));
    if let Some(getter) = getter {
        entry.0 = Some(interp.native(name, 0, getter));
    }
    if let Some(setter) = setter {
        entry.1 = Some(interp.native(name, 1, setter));
    }
    borrowed.hide_from_enumeration(name);
}

/// Render a value for `console.log`, in the style of a real engine.
pub fn inspect(value: &Value) -> String {
    inspect_at(value, 0)
}

fn inspect_at(value: &Value, depth: usize) -> String {
    const MAX_DEPTH: usize = 4;
    match value {
        Value::Str(text) => {
            if depth == 0 {
                text.to_string()
            } else {
                format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
            }
        }
        Value::Num(n) => format_number(*n),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Undefined => "undefined".to_string(),
        Value::BigInt(n) => format!("{n}n"),
        Value::Object(object) => {
            if depth >= MAX_DEPTH {
                return "[Object]".to_string();
            }
            let borrowed = object.borrow();
            if let Some(_) = &borrowed.callable {
                let name = borrowed
                    .props
                    .get("name")
                    .map(|n| n.to_display_string())
                    .unwrap_or_default();
                return if name.is_empty() {
                    "[Function (anonymous)]".to_string()
                } else {
                    format!("[Function: {name}]")
                };
            }
            match &borrowed.kind {
                Kind::Array => {
                    let length = borrowed.array_length();
                    drop(borrowed);
                    let items: Vec<String> = (0..length)
                        .map(|index| match object.borrow().props.get(&index.to_string()) {
                            Some(value) => inspect_at(value, depth + 1),
                            None => "<1 empty item>".to_string(),
                        })
                        .collect();
                    if items.is_empty() {
                        "[]".to_string()
                    } else {
                        format!("[ {} ]", items.join(", "))
                    }
                }
                Kind::Map(entries) => {
                    let entries = entries.clone();
                    drop(borrowed);
                    let rendered: Vec<String> = entries
                        .iter()
                        .map(|(k, v)| {
                            format!("{} => {}", inspect_at(k, depth + 1), inspect_at(v, depth + 1))
                        })
                        .collect();
                    format!("Map({}) {{ {} }}", rendered.len(), rendered.join(", "))
                }
                Kind::Set(items) => {
                    let items = items.clone();
                    drop(borrowed);
                    let rendered: Vec<String> = items
                        .iter()
                        .map(|item| inspect_at(item, depth + 1))
                        .collect();
                    format!("Set({}) {{ {} }}", rendered.len(), rendered.join(", "))
                }
                Kind::RegExp(regex) => {
                    let text = format!("/{}/{}", regex.source, regex.flags);
                    drop(borrowed);
                    text
                }
                Kind::Date(ms) => {
                    let text = format!("{}", ms);
                    drop(borrowed);
                    format!("Date({text})")
                }
                Kind::Promise(_) => {
                    drop(borrowed);
                    "Promise { <state> }".to_string()
                }
                Kind::Error => {
                    let name = borrowed
                        .props
                        .get("name")
                        .map(|v| v.to_display_string())
                        .unwrap_or_else(|| "Error".to_string());
                    let message = borrowed
                        .props
                        .get("message")
                        .map(|v| v.to_display_string())
                        .unwrap_or_default();
                    drop(borrowed);
                    format!("{name}: {message}")
                }
                Kind::Plain => {
                    let keys: Vec<String> = borrowed.props.keys().iter().map(|k| k.to_string()).collect();
                    let entries: Vec<String> = keys
                        .iter()
                        .filter_map(|key| {
                            object
                                .borrow()
                                .props
                                .get(key)
                                .map(|value| format!("{key}: {}", inspect_at(value, depth + 1)))
                        })
                        .collect();
                    if entries.is_empty() {
                        "{}".to_string()
                    } else {
                        format!("{{ {} }}", entries.join(", "))
                    }
                }
            }
        }
    }
}

/// Install everything into a fresh realm.
pub fn install(interp: &mut Interp) {
    install_object(interp);
    install_function(interp);
    install_array(interp);
    install_string(interp);
    install_number_and_boolean(interp);
    install_math(interp);
    install_json(interp);
    install_console(interp);
    install_errors(interp);
    install_map_set(interp);
    install_date(interp);
    regexp::install(interp);
    install_promise(interp);
    install_globals(interp);
    hide_link_slots(interp);
}

/// Hide the `prototype` / `constructor` / `name` / `length` slots that
/// JavaScript keeps non-enumerable.
///
/// Doing this once, after every built-in exists, avoids repeating the same two
/// lines at each of the fifteen constructor definitions above.
fn hide_link_slots(interp: &mut Interp) {
    let mut prototypes = vec![
        interp.prototypes.object.clone(),
        interp.prototypes.function.clone(),
        interp.prototypes.array.clone(),
        interp.prototypes.string.clone(),
        interp.prototypes.number.clone(),
        interp.prototypes.boolean.clone(),
        interp.prototypes.regexp.clone(),
        interp.prototypes.map.clone(),
        interp.prototypes.set.clone(),
        interp.prototypes.date.clone(),
        interp.prototypes.promise.clone(),
    ];
    prototypes.extend(interp.prototypes.error.values().cloned());
    for prototype in prototypes {
        prototype.borrow_mut().hide_from_enumeration("constructor");
    }

    let globals: Vec<Value> = interp
        .globals
        .bindings
        .borrow()
        .values()
        .map(|binding| binding.value.clone())
        .collect();
    for value in globals {
        if let Value::Object(object) = &value {
            if object.borrow().is_callable() {
                let mut borrowed = object.borrow_mut();
                for slot in ["prototype", "name", "length"] {
                    borrowed.hide_from_enumeration(slot);
                }
            }
        }
    }
}

// ------------------------------------------------------------------- Object

fn install_object(interp: &mut Interp) {
    let proto = interp.prototypes.object.clone();
    def(&*interp, &proto, "hasOwnProperty", 1, |interp, this, args| {
        let key = interp.key_to_string(&arg(args, 0))?;
        Ok(match this {
            Value::Object(object) => Value::Bool(object.borrow().props.contains(&key)),
            _ => Value::Bool(false),
        })
    });
    def(&*interp, &proto, "toString", 0, |_interp, this, _args| {
        Ok(Value::str(format!("[object {}]", this.class_name())))
    });
    def(&*interp, &proto, "valueOf", 0, |_interp, this, _args| {
        Ok(this.clone())
    });
    def(&*interp, &proto, "isPrototypeOf", 1, |_interp, this, args| {
        let target = arg(args, 0);
        let wanted = match this {
            Value::Object(object) => object.clone(),
            _ => return Ok(Value::Bool(false)),
        };
        let mut current = match &target {
            Value::Object(object) => object.borrow().prototype.clone(),
            _ => None,
        };
        while let Some(node) = current {
            if Rc::ptr_eq(&node, &wanted) {
                return Ok(Value::Bool(true));
            }
            current = node.borrow().prototype.clone();
        }
        Ok(Value::Bool(false))
    });

    // Object constructor and its static helpers.
    let constructor = interp.native("Object", 1, |interp, _this, args| match arg(args, 0) {
        Value::Object(object) => Ok(Value::Object(object)),
        Value::Null | Value::Undefined => Ok(Value::Object(interp.new_object())),
        other => Ok(Value::Object(interp.wrap_primitive(&other))),
    });
    let object_fn = match &constructor {
        Value::Object(object) => object.clone(),
        _ => return,
    };
    object_fn
        .borrow_mut()
        .props
        .set("prototype", Value::Object(proto.clone()));
    proto
        .borrow_mut()
        .props
        .set("constructor", constructor.clone());

    def(&*interp, &object_fn, "keys", 1, |interp, _this, args| {
        let keys = own_keys(interp, &arg(args, 0))?;
        Ok(interp.new_string_array(keys))
    });
    def(&*interp, &object_fn, "values", 1, |interp, _this, args| {
        let target = arg(args, 0);
        let mut values = Vec::new();
        for key in own_keys(interp, &target)? {
            values.push(interp.get_prop(&target, &key)?);
        }
        Ok(interp.new_array(values))
    });
    def(&*interp, &object_fn, "entries", 1, |interp, _this, args| {
        let target = arg(args, 0);
        let mut entries = Vec::new();
        for key in own_keys(interp, &target)? {
            let value = interp.get_prop(&target, &key)?;
            entries.push(interp.new_array(vec![Value::str(&key), value]));
        }
        Ok(interp.new_array(entries))
    });
    def(&*interp, &object_fn, "assign", 2, |interp, _this, args| {
        let target = arg(args, 0);
        if target.is_nullish() {
            return interp.type_error("Object.assign cannot convert undefined or null");
        }
        for source in args.iter().skip(1) {
            if source.is_nullish() {
                continue;
            }
            for key in own_keys(interp, source)? {
                let value = interp.get_prop(source, &key)?;
                interp.set_prop(&target, &key, value)?;
            }
        }
        Ok(target)
    });
    def(&*interp, &object_fn, "fromEntries", 1, |interp, _this, args| {
        let entries = interp.iterate(&arg(args, 0))?;
        let object = interp.new_object();
        for entry in entries {
            let key = interp.get_prop(&entry, "0")?;
            let value = interp.get_prop(&entry, "1")?;
            let key = interp.key_to_string(&key)?;
            interp.set_prop(&Value::Object(object.clone()), &key, value)?;
        }
        Ok(Value::Object(object))
    });
    def(&*interp, &object_fn, "create", 1, |_interp, _this, args| {
        let prototype = match arg(args, 0) {
            Value::Object(object) => Some(object),
            _ => None,
        };
        Ok(Value::Object(Object::new(prototype)))
    });
    def(&*interp, &object_fn, "getPrototypeOf", 1, |_interp, _this, args| {
        Ok(match arg(args, 0) {
            Value::Object(object) => match object.borrow().prototype.clone() {
                Some(prototype) => Value::Object(prototype),
                None => Value::Null,
            },
            _ => Value::Null,
        })
    });
    def(&*interp, &object_fn, "defineProperty", 3, |interp, _this, args| {
        let target = arg(args, 0);
        let key = interp.key_to_string(&arg(args, 1))?;
        let descriptor = arg(args, 2);
        let value = interp.get_prop(&descriptor, "value")?;
        let getter = interp.get_prop(&descriptor, "get")?;
        let setter = interp.get_prop(&descriptor, "set")?;
        if let Value::Object(object) = &target {
            let mut borrowed = object.borrow_mut();
            if !matches!(value, Value::Undefined) {
                borrowed.props.set(key.clone(), value);
            }
            if !matches!(getter, Value::Undefined) || !matches!(setter, Value::Undefined) {
                borrowed.accessors.insert(
                    key,
                    (
                        if matches!(getter, Value::Undefined) {
                            None
                        } else {
                            Some(getter)
                        },
                        if matches!(setter, Value::Undefined) {
                            None
                        } else {
                            Some(setter)
                        },
                    ),
                );
            }
        }
        Ok(target)
    });
    // Freezing is not modelled: objects stay mutable so that the common
    // `Object.freeze` guard rails in plugin code keep working.
    def(&*interp, &object_fn, "freeze", 1, |_interp, _this, args| Ok(arg(args, 0)));
    def(&*interp, &object_fn, "seal", 1, |_interp, _this, args| Ok(arg(args, 0)));
    def(&*interp, &object_fn, "isFrozen", 1, |_interp, _this, _args| {
        Ok(Value::Bool(false))
    });

    interp.define_global("Object", constructor);
}

fn own_keys(interp: &mut Interp, target: &Value) -> Result<Vec<String>, Control> {
    match target {
        Value::Object(object) => Ok(object.borrow().enumerable_keys()),
        Value::Str(text) => Ok((0..text.chars().count()).map(|i| i.to_string()).collect()),
        Value::Null | Value::Undefined => {
            interp.type_error("cannot convert undefined or null to object")
        }
        _ => Ok(Vec::new()),
    }
}

// ----------------------------------------------------------------- Function

fn install_function(interp: &mut Interp) {
    let proto = interp.prototypes.function.clone();
    def(&*interp, &proto, "call", 1, |interp, this, args| {
        let this_arg = arg(args, 0);
        let rest = args.iter().skip(1).cloned().collect();
        interp.call_value(this, this_arg, rest)
    });
    def(&*interp, &proto, "apply", 2, |interp, this, args| {
        let this_arg = arg(args, 0);
        let list = arg(args, 1);
        let rest = if list.is_nullish() {
            Vec::new()
        } else {
            interp.iterate(&list)?
        };
        interp.call_value(this, this_arg, rest)
    });
    def(&*interp, &proto, "bind", 1, |interp, this, args| {
        let target = this.clone();
        let bound_this = arg(args, 0);
        let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
        Ok(interp.native("bound", 0, move |interp, _this, call_args| {
            let mut all = bound_args.clone();
            all.extend(call_args.iter().cloned());
            interp.call_value(&target, bound_this.clone(), all)
        }))
    });
    def(&*interp, &proto, "toString", 0, |_interp, this, _args| {
        Ok(Value::str(this.to_display_string()))
    });
}

// -------------------------------------------------------------------- Array

fn install_array(interp: &mut Interp) {
    let proto = interp.prototypes.array.clone();

    def(&*interp, &proto, "push", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let mut items = items;
        items.extend(args.iter().cloned());
        let length = items.len();
        write_array(interp, this, items)?;
        Ok(Value::Num(length as f64))
    });
    def(&*interp, &proto, "pop", 0, |interp, this, _args| {
        let mut items = interp.array_items(this)?;
        let popped = items.pop().unwrap_or(Value::Undefined);
        write_array(interp, this, items)?;
        Ok(popped)
    });
    def(&*interp, &proto, "shift", 0, |interp, this, _args| {
        let mut items = interp.array_items(this)?;
        if items.is_empty() {
            return Ok(Value::Undefined);
        }
        let first = items.remove(0);
        write_array(interp, this, items)?;
        Ok(first)
    });
    def(&*interp, &proto, "unshift", 1, |interp, this, args| {
        let mut items = interp.array_items(this)?;
        let mut prefix: Vec<Value> = args.to_vec();
        prefix.extend(items);
        items = prefix;
        let length = items.len();
        write_array(interp, this, items)?;
        Ok(Value::Num(length as f64))
    });
    def(&*interp, &proto, "slice", 2, |interp, this, args| {
        let items = interp.array_items(this)?;
        let length = items.len() as f64;
        let start = normalize_index(arg(args, 0).to_number(), length, 0.0);
        let end = match args.get(1) {
            Some(_) => normalize_index(arg(args, 1).to_number(), length, length),
            None => length as usize,
        };
        let slice = if start < end {
            items[start as usize..end as usize].to_vec()
        } else {
            Vec::new()
        };
        Ok(interp.new_array(slice))
    });
    def(&*interp, &proto, "splice", 2, |interp, this, args| {
        let mut items = interp.array_items(this)?;
        let length = items.len() as f64;
        let start = normalize_index(arg(args, 0).to_number(), length, 0.0) as usize;
        let delete_count = match args.get(1) {
            Some(_) => {
                let requested = arg(args, 1).to_number();
                if requested.is_nan() {
                    0
                } else {
                    requested.max(0.0).min(items.len() as f64 - start as f64) as usize
                }
            }
            None => items.len() - start.min(items.len()),
        };
        let removed: Vec<Value> = if start < items.len() {
            items.drain(start..(start + delete_count).min(items.len())).collect()
        } else {
            Vec::new()
        };
        let inserted: Vec<Value> = args.iter().skip(2).cloned().collect();
        for (offset, value) in inserted.into_iter().enumerate() {
            items.insert(start + offset, value);
        }
        write_array(interp, this, items)?;
        Ok(interp.new_array(removed))
    });
    def(&*interp, &proto, "join", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let separator = match args.first() {
            Some(Value::Undefined) | None => ",".to_string(),
            Some(value) => interp.to_string(value)?,
        };
        let mut parts = Vec::new();
        for item in items {
            parts.push(match item {
                Value::Undefined | Value::Null => String::new(),
                other => interp.to_string(&other)?,
            });
        }
        Ok(Value::str(parts.join(&separator)))
    });
    def(&*interp, &proto, "concat", 1, |interp, this, args| {
        let mut items = interp.array_items(this)?;
        for value in args {
            let is_array = matches!(value, Value::Object(object) if object.borrow().is_array());
            if is_array {
                items.extend(interp.array_items(value)?);
            } else {
                items.push(value.clone());
            }
        }
        Ok(interp.new_array(items))
    });
    def(&*interp, &proto, "indexOf", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let needle = arg(args, 0);
        Ok(Value::Num(
            items
                .iter()
                .position(|item| item.strict_equals(&needle))
                .map(|index| index as f64)
                .unwrap_or(-1.0),
        ))
    });
    def(&*interp, &proto, "includes", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let needle = arg(args, 0);
        Ok(Value::Bool(items.iter().any(|item| {
            item.strict_equals(&needle)
                || (matches!(item, Value::Num(n) if n.is_nan())
                    && matches!(needle, Value::Num(n) if n.is_nan()))
        })))
    });
    def(&*interp, &proto, "reverse", 0, |interp, this, _args| {
        let mut items = interp.array_items(this)?;
        items.reverse();
        write_array(interp, this, items)?;
        Ok(this.clone())
    });
    def(&*interp, &proto, "at", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let index = arg(args, 0).to_number();
        let length = items.len() as f64;
        let resolved = if index < 0.0 { length + index } else { index };
        if resolved < 0.0 || resolved >= length {
            return Ok(Value::Undefined);
        }
        Ok(items[resolved as usize].clone())
    });
    def(&*interp, &proto, "forEach", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        for (index, item) in items.iter().enumerate() {
            interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
        }
        Ok(Value::Undefined)
    });
    def(&*interp, &proto, "map", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        let mut mapped = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            mapped.push(interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?);
        }
        Ok(interp.new_array(mapped))
    });
    def(&*interp, &proto, "filter", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        let mut kept = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let verdict = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
            if verdict.truthy() {
                kept.push(item.clone());
            }
        }
        Ok(interp.new_array(kept))
    });
    def(&*interp, &proto, "find", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        for (index, item) in items.iter().enumerate() {
            let verdict = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
            if verdict.truthy() {
                return Ok(item.clone());
            }
        }
        Ok(Value::Undefined)
    });
    def(&*interp, &proto, "findIndex", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        for (index, item) in items.iter().enumerate() {
            let verdict = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
            if verdict.truthy() {
                return Ok(Value::Num(index as f64));
            }
        }
        Ok(Value::Num(-1.0))
    });
    def(&*interp, &proto, "some", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        for (index, item) in items.iter().enumerate() {
            let verdict = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
            if verdict.truthy() {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    });
    def(&*interp, &proto, "every", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        for (index, item) in items.iter().enumerate() {
            let verdict = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
            if !verdict.truthy() {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    });
    def(&*interp, &proto, "reduce", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let callback = arg(args, 0);
        let (mut accumulator, start) = if args.len() > 1 {
            (arg(args, 1), 0usize)
        } else if items.is_empty() {
            return interp.type_error("reduce of empty array with no initial value");
        } else {
            (items[0].clone(), 1usize)
        };
        for index in start..items.len() {
            accumulator = interp.call_value(
                &callback,
                Value::Undefined,
                vec![
                    accumulator,
                    items[index].clone(),
                    Value::Num(index as f64),
                    this.clone(),
                ],
            )?;
        }
        Ok(accumulator)
    });
    def(&*interp, &proto, "sort", 1, |interp, this, args| {
        let mut items = interp.array_items(this)?;
        let comparator = arg(args, 0);
        let is_callable = matches!(&comparator, Value::Object(object) if object.borrow().is_callable());
        if is_callable {
            // A simple insertion sort: the comparison runs JavaScript code, so
            // it cannot be delegated to a Rust sort.
            let length = items.len();
            for i in 1..length {
                let mut j = i;
                while j > 0 {
                    let order = interp.call_value(
                        &comparator,
                        Value::Undefined,
                        vec![items[j - 1].clone(), items[j].clone()],
                    )?;
                    if order.to_number() > 0.0 {
                        items.swap(j - 1, j);
                        j -= 1;
                    } else {
                        break;
                    }
                }
            }
        } else {
            let mut keyed: Vec<(String, Value)> = Vec::with_capacity(items.len());
            for item in items {
                keyed.push((interp.to_string(&item)?, item));
            }
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            items = keyed.into_iter().map(|(_, value)| value).collect();
        }
        write_array(interp, this, items)?;
        Ok(this.clone())
    });
    def(&*interp, &proto, "flat", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let depth = match args.first() {
            Some(Value::Undefined) | None => 1.0,
            Some(value) => value.to_number(),
        };
        let mut out = Vec::new();
        flatten(interp, items, depth, &mut out)?;
        Ok(interp.new_array(out))
    });
    def(&*interp, &proto, "flatMap", 1, |interp, this, args| {
        let callback = arg(args, 0);
        let items = interp.array_items(this)?;
        let mut out = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let mapped = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64), this.clone()],
            )?;
            match &mapped {
                Value::Object(object) if object.borrow().is_array() => {
                    out.extend(interp.array_items(&mapped)?)
                }
                other => out.push(other.clone()),
            }
        }
        Ok(interp.new_array(out))
    });
    def(&*interp, &proto, "toString", 0, |interp, this, args| {
        let items = interp.array_items(this)?;
        let mut parts = Vec::new();
        for item in items {
            parts.push(match item {
                Value::Undefined | Value::Null => String::new(),
                other => interp.to_string(&other)?,
            });
        }
        let _ = args;
        Ok(Value::str(parts.join(",")))
    });

    let constructor = interp.native("Array", 1, |interp, _this, args| {
        if args.len() == 1 {
            if let Value::Num(length) = args[0] {
                let length = length.max(0.0) as usize;
                let items = vec![Value::Undefined; length];
                return Ok(interp.new_array(items));
            }
        }
        Ok(interp.new_array(args.to_vec()))
    });
    let array_fn = match &constructor {
        Value::Object(object) => object.clone(),
        _ => return,
    };
    array_fn
        .borrow_mut()
        .props
        .set("prototype", Value::Object(proto.clone()));
    proto
        .borrow_mut()
        .props
        .set("constructor", constructor.clone());

    def(&*interp, &array_fn, "isArray", 1, |_interp, _this, args| {
        Ok(Value::Bool(matches!(&arg(args, 0), Value::Object(object) if object.borrow().is_array())))
    });
    def(&*interp, &array_fn, "from", 1, |interp, _this, args| {
        let source = arg(args, 0);
        let items = interp.iterate(&source)?;
        let mapped = arg(args, 1);
        let is_callable = matches!(&mapped, Value::Object(object) if object.borrow().is_callable());
        if is_callable {
            let mut out = Vec::new();
            for (index, item) in items.iter().enumerate() {
                out.push(interp.call_value(
                    &mapped,
                    Value::Undefined,
                    vec![item.clone(), Value::Num(index as f64)],
                )?);
            }
            Ok(interp.new_array(out))
        } else {
            Ok(interp.new_array(items))
        }
    });
    def(&*interp, &array_fn, "of", 0, |interp, _this, args| {
        Ok(interp.new_array(args.to_vec()))
    });

    interp.define_global("Array", constructor);
}

fn write_array(interp: &mut Interp, target: &Value, items: Vec<Value>) -> Result<(), Control> {
    let object = match target {
        Value::Object(object) => object.clone(),
        _ => return interp.type_error("array method called on a non-array"),
    };
    {
        let mut borrowed = object.borrow_mut();
        let stale: Vec<String> = borrowed
            .props
            .keys()
            .into_iter()
            .filter(|key| key.chars().all(|c| c.is_ascii_digit()) || *key == "length")
            .map(|k| k.to_string())
            .collect();
        for key in stale {
            borrowed.props.remove(&key);
        }
        for (index, item) in items.iter().enumerate() {
            borrowed.props.set(index.to_string(), item.clone());
        }
        borrowed.props.set("length", Value::Num(items.len() as f64));
        borrowed.hide_from_enumeration("length");
    }
    Ok(())
}

fn flatten(
    interp: &mut Interp,
    items: Vec<Value>,
    depth: f64,
    out: &mut Vec<Value>,
) -> Result<(), Control> {
    for item in items {
        let is_array = matches!(&item, Value::Object(object) if object.borrow().is_array());
        if is_array && depth > 0.0 {
            let nested = interp.array_items(&item)?;
            flatten(interp, nested, depth - 1.0, out)?;
        } else {
            out.push(item);
        }
    }
    Ok(())
}

/// Resolve a possibly negative, fractional index against a length.
fn normalize_index(value: f64, length: f64, fallback: f64) -> usize {
    if value.is_nan() {
        return fallback as usize;
    }
    let resolved = if value < 0.0 { length + value } else { value };
    resolved.max(0.0).min(length) as usize
}

// ------------------------------------------------------------------- String

fn install_string(interp: &mut Interp) {
    let proto = interp.prototypes.string.clone();

    def(&*interp, &proto, "split", 2, |interp, this, args| {
        let text = interp.to_string(this)?;
        let separator = arg(args, 0);
        let parts: Vec<Value> = match separator {
            Value::Undefined => vec![Value::str(&text)],
            Value::Str(sep) if sep.is_empty() => text
                .chars()
                .map(|c| Value::str(c.to_string()))
                .collect(),
            other => {
                let separator = interp.to_string(&other)?;
                text.split(&separator as &str)
                    .map(Value::str)
                    .collect()
            }
        };
        Ok(interp.new_array(parts))
    });
    def(&*interp, &proto, "slice", 2, |interp, this, args| {
        let chars: Vec<char> = interp.to_string(this)?.chars().collect();
        let length = chars.len() as f64;
        let start = normalize_index(arg(args, 0).to_number(), length, 0.0);
        let end = match args.get(1) {
            Some(_) => normalize_index(arg(args, 1).to_number(), length, length),
            None => length as usize,
        };
        if start >= end {
            return Ok(Value::str(""));
        }
        Ok(Value::str(chars[start..end.min(chars.len())].iter().collect::<String>()))
    });
    def(&*interp, &proto, "substring", 2, |interp, this, args| {
        let chars: Vec<char> = interp.to_string(this)?.chars().collect();
        let length = chars.len() as f64;
        let mut start = arg(args, 0).to_number().max(0.0).min(length) as usize;
        let mut end = match args.get(1) {
            Some(Value::Undefined) | None => chars.len(),
            Some(_) => arg(args, 1).to_number().max(0.0).min(length) as usize,
        };
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        Ok(Value::str(chars[start..end].iter().collect::<String>()))
    });
    def(&*interp, &proto, "charAt", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let index = arg(args, 0).to_number();
        Ok(match text.chars().nth(index.max(0.0) as usize) {
            Some(c) => Value::str(c.to_string()),
            None => Value::str(""),
        })
    });
    def(&*interp, &proto, "charCodeAt", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let index = arg(args, 0).to_number();
        Ok(match text.chars().nth(index.max(0.0) as usize) {
            Some(c) => Value::Num(c as u32 as f64),
            None => Value::Num(f64::NAN),
        })
    });
    def(&*interp, &proto, "indexOf", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let needle = interp.to_string(&arg(args, 0))?;
        Ok(Value::Num(match text.find(&needle) {
            Some(byte_index) => text[..byte_index].chars().count() as f64,
            None => -1.0,
        }))
    });
    def(&*interp, &proto, "lastIndexOf", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let needle = interp.to_string(&arg(args, 0))?;
        Ok(Value::Num(match text.rfind(&needle) {
            Some(byte_index) => text[..byte_index].chars().count() as f64,
            None => -1.0,
        }))
    });
    def(&*interp, &proto, "includes", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let needle = interp.to_string(&arg(args, 0))?;
        Ok(Value::Bool(text.contains(&needle)))
    });
    def(&*interp, &proto, "startsWith", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let needle = interp.to_string(&arg(args, 0))?;
        Ok(Value::Bool(text.starts_with(&needle)))
    });
    def(&*interp, &proto, "endsWith", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let needle = interp.to_string(&arg(args, 0))?;
        Ok(Value::Bool(text.ends_with(&needle)))
    });
    def(&*interp, &proto, "trim", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?.trim()))
    });
    def(&*interp, &proto, "trimStart", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?.trim_start()))
    });
    def(&*interp, &proto, "trimEnd", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?.trim_end()))
    });
    def(&*interp, &proto, "toUpperCase", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?.to_uppercase()))
    });
    def(&*interp, &proto, "toLowerCase", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?.to_lowercase()))
    });
    def(&*interp, &proto, "repeat", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        let count = arg(args, 0).to_number().max(0.0) as usize;
        Ok(Value::str(text.repeat(count.min(1_000_000))))
    });
    def(&*interp, &proto, "padStart", 2, |interp, this, args| {
        let text = interp.to_string(this)?;
        let width = arg(args, 0).to_number().max(0.0) as usize;
        let filler = match args.get(1) {
            Some(value) => interp.to_string(value)?,
            None => " ".to_string(),
        };
        Ok(Value::str(pad(&text, width, &filler, true)))
    });
    def(&*interp, &proto, "padEnd", 2, |interp, this, args| {
        let text = interp.to_string(this)?;
        let width = arg(args, 0).to_number().max(0.0) as usize;
        let filler = match args.get(1) {
            Some(value) => interp.to_string(value)?,
            None => " ".to_string(),
        };
        Ok(Value::str(pad(&text, width, &filler, false)))
    });
    def(&*interp, &proto, "replace", 2, |interp, this, args| {
        let text = interp.to_string(this)?;
        let pattern = arg(args, 0);
        let replacement = arg(args, 1);
        let is_regex = matches!(&pattern, Value::Object(object) if regexp::regex_of(object).is_some());
        if is_regex {
            let regex = match &pattern {
                Value::Object(object) => regexp::regex_of(object).unwrap(),
                _ => unreachable!(),
            };
            let replacement_text = if matches!(&replacement, Value::Object(o) if o.borrow().is_callable()) {
                None
            } else {
                Some(interp.to_string(&replacement)?)
            };
            return regexp::replace(interp, &regex, &text, replacement_text, &replacement, false);
        }
        let needle = interp.to_string(&pattern)?;
        if matches!(&replacement, Value::Object(o) if o.borrow().is_callable()) {
            if let Some(byte_index) = text.find(&needle) {
                let matched = &text[byte_index..byte_index + needle.len()];
                let result =
                    interp.call_value(&replacement, Value::Undefined, vec![Value::str(matched)])?;
                let mut out = text[..byte_index].to_string();
                out.push_str(&interp.to_string(&result)?);
                out.push_str(&text[byte_index + needle.len()..]);
                return Ok(Value::str(out));
            }
            return Ok(Value::str(text));
        }
        let replacement_text = interp.to_string(&replacement)?;
        Ok(Value::str(text.replacen(&needle as &str, &replacement_text, 1)))
    });
    def(&*interp, &proto, "replaceAll", 2, |interp, this, args| {
        let text = interp.to_string(this)?;
        let needle = interp.to_string(&arg(args, 0))?;
        let replacement = interp.to_string(&arg(args, 1))?;
        Ok(Value::str(text.replace(&needle as &str, &replacement)))
    });
    def(&*interp, &proto, "concat", 1, |interp, this, args| {
        let mut text = interp.to_string(this)?;
        for value in args {
            text.push_str(&interp.to_string(value)?);
        }
        Ok(Value::str(text))
    });
    def(&*interp, &proto, "toString", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?))
    });
    def(&*interp, &proto, "valueOf", 0, |interp, this, _args| {
        Ok(Value::str(interp.to_string(this)?))
    });
    def(&*interp, &proto, "match", 1, |interp, this, args| {
        let text = interp.to_string(this)?;
        match arg(args, 0) {
            Value::Object(object) => match regexp::regex_of(&object) {
                Some(regex) => regexp::match_values(interp, &regex, &text),
                None => Ok(Value::Null),
            },
            pattern => {
                let pattern = interp.to_string(&pattern)?;
                let regex = match regexp::compile(&pattern, "") {
                    Some(regex) => regex,
                    None => return Ok(Value::Null),
                };
                regexp::match_values(interp, &regex, &text)
            }
        }
    });

    let constructor = interp.native("String", 1, |interp, _this, args| {
        if args.is_empty() {
            return Ok(Value::str(""));
        }
        Ok(Value::str(interp.to_string(&arg(args, 0))?))
    });
    let string_fn = match &constructor {
        Value::Object(object) => object.clone(),
        _ => return,
    };
    string_fn
        .borrow_mut()
        .props
        .set("prototype", Value::Object(proto.clone()));
    proto
        .borrow_mut()
        .props
        .set("constructor", constructor.clone());
    def(&*interp, &string_fn, "fromCharCode", 1, |_interp, _this, args| {
        let mut text = String::new();
        for value in args {
            if let Some(c) = char::from_u32(value.to_number().max(0.0) as u32) {
                text.push(c);
            }
        }
        Ok(Value::str(text))
    });

    interp.define_global("String", constructor);
}

fn pad(text: &str, width: usize, filler: &str, at_start: bool) -> String {
    let current = text.chars().count();
    if current >= width || filler.is_empty() {
        return text.to_string();
    }
    let mut padding = String::new();
    while padding.chars().count() < width - current {
        padding.push_str(filler);
    }
    let padding: String = padding.chars().take(width - current).collect();
    if at_start {
        format!("{padding}{text}")
    } else {
        format!("{text}{padding}")
    }
}

// ---------------------------------------------------------- Number/Boolean

fn install_number_and_boolean(interp: &mut Interp) {
    let number_proto = interp.prototypes.number.clone();
    def(&*interp, &number_proto, "toFixed", 1, |_interp, this, args| {
        let digits = arg(args, 0).to_number().max(0.0).min(100.0) as usize;
        let value = this.to_number();
        if value.is_nan() {
            return Ok(Value::str("NaN"));
        }
        Ok(Value::str(format!("{value:.digits$}")))
    });
    def(&*interp, &number_proto, "toString", 1, |_interp, this, args| {
        let radix = match args.first() {
            Some(Value::Undefined) | None => 10.0,
            Some(value) => value.to_number(),
        };
        let value = this.to_number();
        if radix == 10.0 {
            return Ok(Value::str(format_number(value)));
        }
        let radix = radix as u32;
        if !(2..=36).contains(&radix) {
            return Ok(Value::str(format_number(value)));
        }
        let mut n = value.trunc().abs() as u64;
        if n == 0 {
            return Ok(Value::str("0"));
        }
        const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut out = Vec::new();
        while n > 0 {
            out.push(DIGITS[(n % radix as u64) as usize] as char);
            n /= radix as u64;
        }
        if value < 0.0 {
            out.push('-');
        }
        out.reverse();
        Ok(Value::str(out.into_iter().collect::<String>()))
    });
    def(&*interp, &number_proto, "valueOf", 0, |_interp, this, _args| {
        Ok(Value::Num(this.to_number()))
    });

    let number_constructor = interp.native("Number", 1, |interp, _this, args| {
        if args.is_empty() {
            return Ok(Value::Num(0.0));
        }
        Ok(Value::Num(interp.to_number_value(&arg(args, 0))?))
    });
    if let Value::Object(number_fn) = &number_constructor {
        number_fn
            .borrow_mut()
            .props
            .set("prototype", Value::Object(number_proto.clone()));
        number_proto
            .borrow_mut()
            .props
            .set("constructor", number_constructor.clone());
        def(&*interp, number_fn, "isInteger", 1, |_interp, _this, args| {
            Ok(Value::Bool(match arg(args, 0) {
                Value::Num(n) => n.is_finite() && n.fract() == 0.0,
                _ => false,
            }))
        });
        def(&*interp, number_fn, "isFinite", 1, |_interp, _this, args| {
            Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite())))
        });
        def(&*interp, number_fn, "isNaN", 1, |_interp, _this, args| {
            Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_nan())))
        });
        number_fn
            .borrow_mut()
            .props
            .set("MAX_SAFE_INTEGER", Value::Num(9007199254740991.0));
        number_fn
            .borrow_mut()
            .props
            .set("MIN_SAFE_INTEGER", Value::Num(-9007199254740991.0));
        number_fn
            .borrow_mut()
            .props
            .set("EPSILON", Value::Num(f64::EPSILON));
    }
    interp.define_global("Number", number_constructor);
    interp.define_global("NaN", Value::Num(f64::NAN));
    interp.define_global("Infinity", Value::Num(f64::INFINITY));

    let boolean_proto = interp.prototypes.boolean.clone();
    def(&*interp, &boolean_proto, "toString", 0, |_interp, this, _args| {
        Ok(Value::str(if this.truthy() { "true" } else { "false" }))
    });
    def(&*interp, &boolean_proto, "valueOf", 0, |_interp, this, _args| {
        Ok(Value::Bool(this.truthy()))
    });
    let boolean_constructor = interp.native("Boolean", 1, |_interp, _this, args| {
        Ok(Value::Bool(arg(args, 0).truthy()))
    });
    if let Value::Object(boolean_fn) = &boolean_constructor {
        boolean_fn
            .borrow_mut()
            .props
            .set("prototype", Value::Object(boolean_proto.clone()));
        boolean_proto
            .borrow_mut()
            .props
            .set("constructor", boolean_constructor.clone());
    }
    interp.define_global("Boolean", boolean_constructor);
}

// --------------------------------------------------------------------- Math

fn install_math(interp: &mut Interp) {
    let math = interp.new_object();
    let object = math.clone();

    let unary: Vec<(&str, fn(f64) -> f64)> = vec![
        ("abs", f64::abs),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("round", |n| (n + 0.5).floor()),
        ("trunc", f64::trunc),
        ("sqrt", f64::sqrt),
        ("cbrt", f64::cbrt),
        ("sign", f64::signum),
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("tan", f64::tan),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("atan", f64::atan),
        ("log", f64::ln),
        ("log2", f64::log2),
        ("log10", f64::log10),
        ("exp", f64::exp),
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
        ("tanh", f64::tanh),
    ];
    for (name, function) in unary {
        def(&*interp, &object, name, 1, move |_interp, _this, args| {
            Ok(Value::Num(function(arg(args, 0).to_number())))
        });
    }
    def(&*interp, &object, "pow", 2, |_interp, _this, args| {
        Ok(Value::Num(
            arg(args, 0).to_number().powf(arg(args, 1).to_number()),
        ))
    });
    def(&*interp, &object, "atan2", 2, |_interp, _this, args| {
        Ok(Value::Num(
            arg(args, 0).to_number().atan2(arg(args, 1).to_number()),
        ))
    });
    def(&*interp, &object, "hypot", 2, |_interp, _this, args| {
        let sum: f64 = args
            .iter()
            .map(|value| value.to_number().powi(2))
            .sum();
        Ok(Value::Num(sum.sqrt()))
    });
    def(&*interp, &object, "min", 2, |_interp, _this, args| {
        let mut result = f64::INFINITY;
        for value in args {
            let n = value.to_number();
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if n < result {
                result = n;
            }
        }
        Ok(Value::Num(result))
    });
    def(&*interp, &object, "max", 2, |_interp, _this, args| {
        let mut result = f64::NEG_INFINITY;
        for value in args {
            let n = value.to_number();
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if n > result {
                result = n;
            }
        }
        Ok(Value::Num(result))
    });
    def(&*interp, &object, "random", 0, |_interp, _this, _args| {
        // A deterministic counter would be worse than a real random source
        // here, because plugin code seeds shuffles and ids with it.
        Ok(Value::Num(pseudo_random()))
    });
    def(&*interp, &object, "imul", 2, |_interp, _this, args| {
        let a = to_int32(arg(args, 0).to_number());
        let b = to_int32(arg(args, 1).to_number());
        Ok(Value::Num(a.wrapping_mul(b) as f64))
    });

    object
        .borrow_mut()
        .props
        .set("PI", Value::Num(std::f64::consts::PI));
    object
        .borrow_mut()
        .props
        .set("E", Value::Num(std::f64::consts::E));
    object
        .borrow_mut()
        .props
        .set("LN2", Value::Num(std::f64::consts::LN_2));
    object
        .borrow_mut()
        .props
        .set("SQRT2", Value::Num(std::f64::consts::SQRT_2));

    interp.define_global("Math", Value::Object(math));
}

/// A small xorshift generator, so `Math.random` needs no dependency.
fn pseudo_random() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0x2545F4914F6CDD1D) };
    }
    STATE.with(|state| {
        let mut x = state.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        state.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

// --------------------------------------------------------------------- JSON

fn install_json(interp: &mut Interp) {
    let json = interp.new_object();
    let object = json.clone();
    def(&*interp, &object, "parse", 1, |interp, _this, args| {
        let text = interp.to_string(&arg(args, 0))?;
        let raw = match json::parse(&text) {
            Some(raw) => raw,
            None => return Err(interp.throw_error("SyntaxError", "invalid JSON input")),
        };
        materialise(interp, raw)
    });
    def(&*interp, &object, "stringify", 3, |interp, _this, args| {
        let value = arg(args, 0);
        let indent = match args.get(2) {
            Some(Value::Num(n)) => Some(*n as usize),
            Some(Value::Str(s)) => Some(s.len()),
            _ => None,
        };
        Ok(match json::stringify(interp, &value, indent)? {
            Some(text) => Value::str(text),
            None => Value::Undefined,
        })
    });
    interp.define_global("JSON", Value::Object(json));
}

/// Convert the neutral JSON tree into interpreter values.
fn materialise(interp: &mut Interp, value: json::Raw) -> Result<Value, Control> {
    Ok(match value {
        json::Raw::Null => Value::Null,
        json::Raw::Bool(b) => Value::Bool(b),
        json::Raw::Num(n) => Value::Num(n),
        json::Raw::Str(s) => Value::str(s),
        json::Raw::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                out.push(materialise(interp, item)?);
            }
            interp.new_array(out)
        }
        json::Raw::Object(entries) => {
            let object = interp.new_object();
            for (key, item) in entries {
                let item = materialise(interp, item)?;
                interp.set_prop(&Value::Object(object.clone()), &key, item)?;
            }
            Value::Object(object)
        }
    })
}

// ------------------------------------------------------------------ console

fn install_console(interp: &mut Interp) {
    let console = interp.new_object();
    let object = console.clone();
    for name in ["log", "info", "debug", "warn", "error", "trace"] {
        def(&*interp, &object, name, 0, move |interp, _this, args| {
            let line: Vec<String> = args.iter().map(inspect).collect();
            interp.print_line(&line.join(" "));
            Ok(Value::Undefined)
        });
    }
    def(&*interp, &object, "dir", 0, |interp, _this, args| {
        interp.print_line(&inspect(&arg(args, 0)));
        Ok(Value::Undefined)
    });
    def(&*interp, &object, "assert", 1, |interp, _this, args| {
        if !arg(args, 0).truthy() {
            let message = match args.get(1) {
                Some(value) => inspect(value),
                None => "Assertion failed".to_string(),
            };
            return Err(interp.throw_error("Error", message));
        }
        Ok(Value::Undefined)
    });
    interp.define_global("console", Value::Object(console));
}

// ------------------------------------------------------------------- errors

fn install_errors(interp: &mut Interp) {
    let kinds = [
        "Error",
        "TypeError",
        "RangeError",
        "SyntaxError",
        "ReferenceError",
        "EvalError",
        "URIError",
    ];
    let mut constructors = Vec::new();
    for kind in kinds {
        let prototype = interp
            .prototypes
            .error
            .get(kind)
            .cloned()
            .unwrap_or_else(|| interp.prototypes.object.clone());
        let name = kind.to_string();
        let constructor = interp.native(kind, 1, move |interp, _this, args| {
            let message = match args.first() {
                Some(Value::Undefined) | None => String::new(),
                Some(value) => interp.to_string(value)?,
            };
            let error = interp.make_error(&name, &message);
            Ok(error)
        });
        if let Value::Object(function) = &constructor {
            function
                .borrow_mut()
                .props
                .set("prototype", Value::Object(prototype.clone()));
            prototype
                .borrow_mut()
                .props
                .set("constructor", constructor.clone());
        }
        prototype
            .borrow_mut()
            .props
            .set("name", Value::str(kind));
        prototype
            .borrow_mut()
            .props
            .set("message", Value::str(""));
        constructors.push((kind, constructor));
    }
    let error_proto = interp.prototypes.error["Error"].clone();
    def(&*interp, &error_proto, "toString", 0, |interp, this, _args| {
        let name = interp.get_prop(this, "name")?;
        let message = interp.get_prop(this, "message")?;
        let name = if matches!(name, Value::Undefined) {
            "Error".to_string()
        } else {
            interp.to_string(&name)?
        };
        let message = if matches!(message, Value::Undefined) {
            String::new()
        } else {
            interp.to_string(&message)?
        };
        Ok(Value::str(if message.is_empty() {
            name
        } else {
            format!("{name}: {message}")
        }))
    });

    for (kind, constructor) in constructors {
        interp.define_global(kind, constructor);
    }
}

// ---------------------------------------------------------------- Map / Set

fn install_map_set(interp: &mut Interp) {
    // Map
    let map_proto = interp.prototypes.map.clone();
    def(&*interp, &map_proto, "set", 2, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Map.prototype.set called on a non-Map"),
        };
        let key = arg(args, 0);
        let value = arg(args, 1);
        {
            let mut borrowed = object.borrow_mut();
            match &mut borrowed.kind {
                Kind::Map(entries) => {
                    match entries
                        .iter_mut()
                        .find(|(existing, _)| same_value_zero(existing, &key))
                    {
                        Some(slot) => slot.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                _ => return interp.type_error("Map.prototype.set called on a non-Map"),
            }
        }
        Ok(this.clone())
    });
    def(&*interp, &map_proto, "get", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Map.prototype.get called on a non-Map"),
        };
        let key = arg(args, 0);
        let found = match &object.borrow().kind {
            Kind::Map(entries) => entries
                .iter()
                .find(|(existing, _)| same_value_zero(existing, &key))
                .map(|(_, value)| value.clone()),
            _ => None,
        };
        Ok(found.unwrap_or(Value::Undefined))
    });
    def(&*interp, &map_proto, "has", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Map.prototype.has called on a non-Map"),
        };
        let key = arg(args, 0);
        let found = match &object.borrow().kind {
            Kind::Map(entries) => entries
                .iter()
                .any(|(existing, _)| same_value_zero(existing, &key)),
            _ => false,
        };
        Ok(Value::Bool(found))
    });
    def(&*interp, &map_proto, "delete", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Map.prototype.delete called on a non-Map"),
        };
        let key = arg(args, 0);
        let removed = {
            let mut borrowed = object.borrow_mut();
            match &mut borrowed.kind {
                Kind::Map(entries) => {
                    let before = entries.len();
                    entries.retain(|(existing, _)| !same_value_zero(existing, &key));
                    before != entries.len()
                }
                _ => return interp.type_error("Map.prototype.delete called on a non-Map"),
            }
        };
        Ok(Value::Bool(removed))
    });
    def(&*interp, &map_proto, "clear", 0, |interp, this, _args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Map.prototype.clear called on a non-Map"),
        };
        {
            let mut borrowed = object.borrow_mut();
            match &mut borrowed.kind {
                Kind::Map(entries) => entries.clear(),
                _ => return interp.type_error("Map.prototype.clear called on a non-Map"),
            }
        }
        Ok(Value::Undefined)
    });
    def(&*interp, &map_proto, "forEach", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Map.prototype.forEach called on a non-Map"),
        };
        let callback = arg(args, 0);
        let entries: Vec<(Value, Value)> = match &object.borrow().kind {
            Kind::Map(entries) => entries.clone(),
            _ => Vec::new(),
        };
        for (key, value) in entries {
            interp.call_value(
                &callback,
                Value::Undefined,
                vec![value, key, this.clone()],
            )?;
        }
        Ok(Value::Undefined)
    });
    def(&*interp, &map_proto, "keys", 0, |interp, this, _args| {
        let entries = map_entries(interp, this)?;
        Ok(interp.new_array(entries.into_iter().map(|(k, _)| k).collect()))
    });
    def(&*interp, &map_proto, "values", 0, |interp, this, _args| {
        let entries = map_entries(interp, this)?;
        Ok(interp.new_array(entries.into_iter().map(|(_, v)| v).collect()))
    });
    def(&*interp, &map_proto, "entries", 0, |interp, this, _args| {
        let entries = map_entries(interp, this)?;
        let pairs: Vec<Value> = entries
            .into_iter()
            .map(|(key, value)| interp.new_array(vec![key, value]))
            .collect();
        Ok(interp.new_array(pairs))
    });
    def_accessor(
        &*interp,
        &map_proto,
        "size",
        Some(|interp, this, _args| {
            Ok(Value::Num(map_entries(interp, this)?.len() as f64))
        }),
        None,
    );

    let map_constructor = interp.native("Map", 0, |interp, _this, args| {
        let object = Object::with_kind(Kind::Map(Vec::new()), Some(interp.prototypes.map.clone()));
        let initial = arg(args, 0);
        if !initial.is_nullish() {
            let entries = interp.iterate(&initial)?;
            let mut borrowed = object.borrow_mut();
            if let Kind::Map(slots) = &mut borrowed.kind {
                for entry in entries {
                    let key = interp.get_prop(&entry, "0")?;
                    let value = interp.get_prop(&entry, "1")?;
                    slots.push((key, value));
                }
            }
        }
        Ok(Value::Object(object))
    });
    if let Value::Object(map_fn) = &map_constructor {
        map_fn
            .borrow_mut()
            .props
            .set("prototype", Value::Object(map_proto.clone()));
        map_proto
            .borrow_mut()
            .props
            .set("constructor", map_constructor.clone());
    }
    interp.define_global("Map", map_constructor);

    // Set
    let set_proto = interp.prototypes.set.clone();
    def(&*interp, &set_proto, "add", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Set.prototype.add called on a non-Set"),
        };
        let value = arg(args, 0);
        {
            let mut borrowed = object.borrow_mut();
            match &mut borrowed.kind {
                Kind::Set(items) => {
                    if !items.iter().any(|existing| same_value_zero(existing, &value)) {
                        items.push(value);
                    }
                }
                _ => return interp.type_error("Set.prototype.add called on a non-Set"),
            }
        }
        Ok(this.clone())
    });
    def(&*interp, &set_proto, "has", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Set.prototype.has called on a non-Set"),
        };
        let value = arg(args, 0);
        let found = match &object.borrow().kind {
            Kind::Set(items) => items.iter().any(|existing| same_value_zero(existing, &value)),
            _ => false,
        };
        Ok(Value::Bool(found))
    });
    def(&*interp, &set_proto, "delete", 1, |interp, this, args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Set.prototype.delete called on a non-Set"),
        };
        let value = arg(args, 0);
        let removed = {
            let mut borrowed = object.borrow_mut();
            match &mut borrowed.kind {
                Kind::Set(items) => {
                    let before = items.len();
                    items.retain(|existing| !same_value_zero(existing, &value));
                    before != items.len()
                }
                _ => return interp.type_error("Set.prototype.delete called on a non-Set"),
            }
        };
        Ok(Value::Bool(removed))
    });
    def(&*interp, &set_proto, "clear", 0, |interp, this, _args| {
        let object = match this {
            Value::Object(object) => object.clone(),
            _ => return interp.type_error("Set.prototype.clear called on a non-Set"),
        };
        {
            let mut borrowed = object.borrow_mut();
            match &mut borrowed.kind {
                Kind::Set(items) => items.clear(),
                _ => return interp.type_error("Set.prototype.clear called on a non-Set"),
            }
        }
        Ok(Value::Undefined)
    });
    def(&*interp, &set_proto, "forEach", 1, |interp, this, args| {
        let callback = arg(args, 0);
        let items = set_items(interp, this)?;
        for item in items {
            interp.call_value(&callback, Value::Undefined, vec![item.clone(), item, this.clone()])?;
        }
        Ok(Value::Undefined)
    });
    def(&*interp, &set_proto, "values", 0, |interp, this, _args| {
        let items = set_items(interp, this)?;
        Ok(interp.new_array(items))
    });
    def_accessor(
        &*interp,
        &set_proto,
        "size",
        Some(|interp, this, _args| Ok(Value::Num(set_items(interp, this)?.len() as f64))),
        None,
    );

    let set_constructor = interp.native("Set", 0, |interp, _this, args| {
        let object = Object::with_kind(Kind::Set(Vec::new()), Some(interp.prototypes.set.clone()));
        let initial = arg(args, 0);
        if !initial.is_nullish() {
            let items = interp.iterate(&initial)?;
            if let Kind::Set(slots) = &mut object.borrow_mut().kind {
                for item in items {
                    if !slots.iter().any(|existing| same_value_zero(existing, &item)) {
                        slots.push(item);
                    }
                }
            }
        }
        Ok(Value::Object(object))
    });
    if let Value::Object(set_fn) = &set_constructor {
        set_fn
            .borrow_mut()
            .props
            .set("prototype", Value::Object(set_proto.clone()));
        set_proto
            .borrow_mut()
            .props
            .set("constructor", set_constructor.clone());
    }
    interp.define_global("Set", set_constructor);
}

fn map_entries(interp: &mut Interp, this: &Value) -> Result<Vec<(Value, Value)>, Control> {
    match this {
        Value::Object(object) => match &object.borrow().kind {
            Kind::Map(entries) => Ok(entries.clone()),
            _ => interp.type_error("value is not a Map"),
        },
        _ => interp.type_error("value is not a Map"),
    }
}

fn set_items(interp: &mut Interp, this: &Value) -> Result<Vec<Value>, Control> {
    match this {
        Value::Object(object) => match &object.borrow().kind {
            Kind::Set(items) => Ok(items.clone()),
            _ => interp.type_error("value is not a Set"),
        },
        _ => interp.type_error("value is not a Set"),
    }
}

/// SameValueZero, the equality `Map` and `Set` use.
pub fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => a.strict_equals(b),
    }
}

// --------------------------------------------------------------------- Date

fn install_date(interp: &mut Interp) {
    let proto = interp.prototypes.date.clone();
    def(&*interp, &proto, "getTime", 0, |_interp, this, _args| {
        Ok(match this {
            Value::Object(object) => match object.borrow().kind {
                Kind::Date(ms) => Value::Num(ms),
                _ => Value::Num(f64::NAN),
            },
            _ => Value::Num(f64::NAN),
        })
    });
    def(&*interp, &proto, "toISOString", 0, |_interp, this, _args| {
        Ok(match this {
            Value::Object(object) => match object.borrow().kind {
                Kind::Date(ms) => Value::str(format_iso(ms)),
                _ => Value::str("Invalid Date"),
            },
            _ => Value::str("Invalid Date"),
        })
    });
    def(&*interp, &proto, "toString", 0, |_interp, this, _args| {
        Ok(match this {
            Value::Object(object) => match object.borrow().kind {
                Kind::Date(ms) => Value::str(format_iso(ms)),
                _ => Value::str("Invalid Date"),
            },
            _ => Value::str("Invalid Date"),
        })
    });
    def(&*interp, &proto, "valueOf", 0, |_interp, this, _args| {
        Ok(match this {
            Value::Object(object) => match object.borrow().kind {
                Kind::Date(ms) => Value::Num(ms),
                _ => Value::Num(f64::NAN),
            },
            _ => Value::Num(f64::NAN),
        })
    });

    let constructor = interp.native("Date", 7, |interp, _this, args| {
        let ms = if args.is_empty() {
            now_ms()
        } else {
            match &args[0] {
                Value::Num(n) => *n,
                Value::Str(text) => parse_iso(text).unwrap_or(f64::NAN),
                _ => now_ms(),
            }
        };
        Ok(Value::Object(Object::with_kind(
            Kind::Date(ms),
            Some(interp.prototypes.date.clone()),
        )))
    });
    if let Value::Object(date_fn) = &constructor {
        date_fn
            .borrow_mut()
            .props
            .set("prototype", Value::Object(proto.clone()));
        proto
            .borrow_mut()
            .props
            .set("constructor", constructor.clone());
        def(&*interp, date_fn, "now", 0, |_interp, _this, _args| {
            Ok(Value::Num(now_ms()))
        });
    }
    interp.define_global("Date", constructor);
}

fn now_ms() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// A minimal ISO-8601 renderer, enough for logging timestamps.
fn format_iso(ms: f64) -> String {
    if !ms.is_finite() {
        return "Invalid Date".to_string();
    }
    let seconds = (ms / 1000.0).floor() as i64;
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

/// Convert days since the Unix epoch to a civil date (Howard Hinnant's
/// `civil_from_days` algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse the date form accepted by `Date.parse`: ISO-8601.
fn parse_iso(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    let date_part = trimmed.split('T').next()?;
    let mut fields = date_part.split('-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: i64 = fields.next()?.parse().ok()?;
    let day: i64 = fields.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some((days * 86_400) as f64 * 1000.0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ------------------------------------------------------------------ Promise

fn install_promise(interp: &mut Interp) {
    let proto = interp.prototypes.promise.clone();

    def(&*interp, &proto, "then", 2, |interp, this, args| {
        let on_fulfilled = arg(args, 0);
        let on_rejected = arg(args, 1);
        let next = interp.new_promise(Rc::new(PromiseState::new()));
        let fulfilled = reaction_for(&*interp, next.clone(), on_fulfilled, false);
        let rejected = reaction_for(&*interp, next.clone(), on_rejected, true);
        interp.add_reaction(this, fulfilled, rejected);
        Ok(next)
    });
    def(&*interp, &proto, "catch", 1, |interp, this, args| {
        let on_rejected = arg(args, 0);
        let next = interp.new_promise(Rc::new(PromiseState::new()));
        let fulfilled = reaction_for(&*interp, next.clone(), Value::Undefined, false);
        let rejected = reaction_for(&*interp, next.clone(), on_rejected, true);
        interp.add_reaction(this, fulfilled, rejected);
        Ok(next)
    });
    def(&*interp, &proto, "finally", 1, |interp, this, args| {
        let callback = arg(args, 0);
        let next = interp.new_promise(Rc::new(PromiseState::new()));
        // `finally` runs the callback and then forwards the original outcome,
        // not whatever the callback returned.
        let run = |interp: &Interp, reject: bool| -> Value {
            let target = next.clone();
            let callback = callback.clone();
            interp.native("<finally>", 1, move |interp, _this, args| {
                let value = arg(args, 0);
                let callable =
                    matches!(&callback, Value::Object(object) if object.borrow().is_callable());
                if callable {
                    interp.call_value(&callback, Value::Undefined, Vec::new())?;
                }
                if reject {
                    interp.reject(&target, value);
                } else {
                    interp.fulfill(&target, value);
                }
                Ok(Value::Undefined)
            })
        };
        let fulfilled = run(&*interp, false);
        let rejected = run(&*interp, true);
        interp.add_reaction(this, fulfilled, rejected);
        Ok(next)
    });

    let constructor = interp.native("Promise", 1, |interp, _this, args| {
        let executor = arg(args, 0);
        let state = Rc::new(PromiseState::new());
        let promise = interp.new_promise(state);
        let resolve_target = promise.clone();
        let reject_target = promise.clone();
        let resolve = interp.native("resolve", 1, move |interp, _this, args| {
            interp.fulfill(&resolve_target, arg(args, 0));
            Ok(Value::Undefined)
        });
        let reject = interp.native("reject", 1, move |interp, _this, args| {
            interp.reject(&reject_target, arg(args, 0));
            Ok(Value::Undefined)
        });
        match interp.call_value(&executor, Value::Undefined, vec![resolve, reject]) {
            Ok(_) => {}
            Err(Control::Throw(error)) => interp.reject(&promise, error.value),
            Err(other) => return Err(other),
        }
        Ok(promise)
    });
    if let Value::Object(promise_fn) = &constructor {
        promise_fn
            .borrow_mut()
            .props
            .set("prototype", Value::Object(proto.clone()));
        proto
            .borrow_mut()
            .props
            .set("constructor", constructor.clone());
        def(&*interp, promise_fn, "resolve", 1, |interp, _this, args| {
            let value = arg(args, 0);
            if interp.promise_state(&value).is_some() {
                return Ok(value);
            }
            Ok(interp.resolved_promise(value))
        });
        def(&*interp, promise_fn, "reject", 1, |interp, _this, args| {
            Ok(interp.rejected_promise(arg(args, 0)))
        });
        def(&*interp, promise_fn, "all", 1, |interp, _this, args| {
            let items = interp.iterate(&arg(args, 0))?;
            let mut results = Vec::with_capacity(items.len());
            // `await` drives the queues, so collecting sequentially yields the
            // same observable order as `Promise.all`.
            for item in items {
                match interp.await_value(item) {
                    Ok(value) => results.push(value),
                    Err(control) => return Err(control),
                }
            }
            Ok(interp.resolved_promise(interp.new_array(results)))
        });
        def(&*interp, promise_fn, "race", 1, |interp, _this, args| {
            let items = interp.iterate(&arg(args, 0))?;
            for item in items {
                match interp.await_value(item) {
                    Ok(value) => return Ok(interp.resolved_promise(value)),
                    Err(control) => return Err(control),
                }
            }
            Ok(interp.new_promise(Rc::new(PromiseState::new())))
        });
        def(&*interp, promise_fn, "allSettled", 1, |interp, _this, args| {
            let items = interp.iterate(&arg(args, 0))?;
            let mut results = Vec::new();
            for item in items {
                let entry = interp.new_object();
                let entry_value = Value::Object(entry.clone());
                match interp.await_value(item) {
                    Ok(value) => {
                        interp.set_prop(&entry_value, "status", Value::str("fulfilled"))?;
                        interp.set_prop(&entry_value, "value", value)?;
                    }
                    Err(Control::Throw(error)) => {
                        interp.set_prop(&entry_value, "status", Value::str("rejected"))?;
                        interp.set_prop(&entry_value, "reason", error.value)?;
                    }
                    Err(other) => return Err(other),
                }
                results.push(entry_value);
            }
            Ok(interp.resolved_promise(interp.new_array(results)))
        });
    }
    interp.define_global("Promise", constructor);
}

/// Build the reaction function attached to a promise.
fn reaction_for(interp: &Interp, next: Value, handler: Value, reject_path: bool) -> Value {
    interp.native("<reaction>", 1, move |interp, _this, args| {
        let value = arg(args, 0);
        let callable = matches!(&handler, Value::Object(object) if object.borrow().is_callable());
        if !callable {
            if reject_path {
                interp.reject(&next, value);
            } else {
                interp.fulfill(&next, value);
            }
            return Ok(Value::Undefined);
        }
        match interp.call_value(&handler, Value::Undefined, vec![value]) {
            Ok(result) => {
                // A handler returning a promise adopts its state.
                if interp.promise_state(&result).is_some() {
                    let fulfill = adopt(&*interp, next.clone(), false);
                    let reject = adopt(&*interp, next.clone(), true);
                    interp.add_reaction(&result, fulfill, reject);
                } else {
                    interp.fulfill(&next, result);
                }
            }
            Err(Control::Throw(error)) => interp.reject(&next, error.value),
            Err(other) => return Err(other),
        }
        Ok(Value::Undefined)
    })
}

fn adopt(interp: &Interp, next: Value, reject: bool) -> Value {
    interp.native("<adopt>", 1, move |interp, _this, args| {
        let value = arg(args, 0);
        if reject {
            interp.reject(&next, value);
        } else {
            interp.fulfill(&next, value);
        }
        Ok(Value::Undefined)
    })
}

// ------------------------------------------------------------ global scope

fn install_globals(interp: &mut Interp) {
    interp.define_global("undefined", Value::Undefined);
    interp.define_global("globalThis", Value::Object(interp.global_object.clone()));

    interp.define_global(
        "parseInt",
        interp.native("parseInt", 2, |interp, _this, args| {
            let text = interp.to_string(&arg(args, 0))?.trim().to_string();
            let radix = match args.get(1) {
                Some(Value::Undefined) | None => 0,
                Some(value) => value.to_number() as u32,
            };
            let (sign, digits) = match text.strip_prefix('-') {
                Some(rest) => (-1.0, rest.to_string()),
                None => (1.0, text.trim_start_matches('+').to_string()),
            };
            let radix = if radix == 0 {
                if digits.starts_with("0x") || digits.starts_with("0X") {
                    16
                } else {
                    10
                }
            } else {
                radix
            };
            let digits = if radix == 16 {
                digits.trim_start_matches("0x").trim_start_matches("0X").to_string()
            } else {
                digits
            };
            let valid: String = digits
                .chars()
                .take_while(|c| c.to_digit(radix).is_some())
                .collect();
            if valid.is_empty() {
                return Ok(Value::Num(f64::NAN));
            }
            Ok(Value::Num(match i64::from_str_radix(&valid, radix) {
                Ok(value) => sign * value as f64,
                Err(_) => f64::NAN,
            }))
        }),
    );
    interp.define_global(
        "parseFloat",
        interp.native("parseFloat", 1, |interp, _this, args| {
            let text = interp.to_string(&arg(args, 0))?;
            let trimmed = text.trim_start();
            let mut candidate = String::new();
            for (index, c) in trimmed.chars().enumerate() {
                let acceptable = if index == 0 {
                    c.is_ascii_digit() || c == '-' || c == '+' || c == '.'
                } else {
                    c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-'
                };
                if acceptable {
                    candidate.push(c);
                } else {
                    break;
                }
            }
            Ok(Value::Num(candidate.parse::<f64>().unwrap_or(f64::NAN)))
        }),
    );
    interp.define_global(
        "isNaN",
        interp.native("isNaN", 1, |interp, _this, args| {
            Ok(Value::Bool(interp.to_number_value(&arg(args, 0))?.is_nan()))
        }),
    );
    interp.define_global(
        "isFinite",
        interp.native("isFinite", 1, |interp, _this, args| {
            Ok(Value::Bool(interp.to_number_value(&arg(args, 0))?.is_finite()))
        }),
    );

    interp.define_global(
        "setTimeout",
        interp.native("setTimeout", 2, |interp, _this, args| {
            let callback = arg(args, 0);
            let delay = arg(args, 1).to_number();
            let extra: Vec<Value> = args.iter().skip(2).cloned().collect();
            let id = interp.set_timer(callback, delay, extra, false);
            Ok(Value::Num(id as f64))
        }),
    );
    interp.define_global(
        "setInterval",
        interp.native("setInterval", 2, |interp, _this, args| {
            let callback = arg(args, 0);
            let delay = arg(args, 1).to_number();
            let extra: Vec<Value> = args.iter().skip(2).cloned().collect();
            let id = interp.set_timer(callback, delay, extra, true);
            Ok(Value::Num(id as f64))
        }),
    );
    interp.define_global(
        "clearTimeout",
        interp.native("clearTimeout", 1, |interp, _this, args| {
            let id = arg(args, 0).to_number();
            if id.is_finite() {
                interp.clear_timer(id as u64);
            }
            Ok(Value::Undefined)
        }),
    );
    interp.define_global(
        "clearInterval",
        interp.native("clearInterval", 1, |interp, _this, args| {
            let id = arg(args, 0).to_number();
            if id.is_finite() {
                interp.clear_timer(id as u64);
            }
            Ok(Value::Undefined)
        }),
    );
    interp.define_global(
        "queueMicrotask",
        interp.native("queueMicrotask", 1, |interp, _this, args| {
            let callback = arg(args, 0);
            interp.queue_microtask(Box::new(move |interp| {
                interp.call_value(&callback, Value::Undefined, Vec::new())?;
                Ok(())
            }));
            Ok(Value::Undefined)
        }),
    );

    // `print` writes a plain line, which is handy for examples.
    interp.define_global(
        "print",
        interp.native("print", 0, |interp, _this, args| {
            let line: Vec<String> = args.iter().map(inspect).collect();
            interp.print_line(&line.join(" "));
            Ok(Value::Undefined)
        }),
    );
}

/// Regular expressions, backed by the pure-Rust `regex` crate.
pub mod regexp {
    use super::*;

    /// A compiled pattern together with the flags it was built with.
    pub struct CompiledRegex {
        pub regex: regex::Regex,
        pub source: String,
        pub flags: String,
        pub global: bool,
    }

    /// Compile a pattern, translating the flags the `regex` crate understands.
    ///
    /// JavaScript-only constructs such as backreferences and lookaround are
    /// rejected by the engine, which surfaces as a `SyntaxError`.
    pub fn compile(source: &str, flags: &str) -> Option<Rc<CompiledRegex>> {
        let mut prefix = String::new();
        if flags.contains('i') {
            prefix.push_str("(?i)");
        }
        if flags.contains('m') {
            prefix.push_str("(?m)");
        }
        if flags.contains('s') {
            prefix.push_str("(?s)");
        }
        let pattern = format!("{prefix}{source}");
        let regex = regex::Regex::new(&pattern).ok()?;
        Some(Rc::new(CompiledRegex {
            regex,
            source: source.to_string(),
            flags: flags.to_string(),
            global: flags.contains('g'),
        }))
    }

    /// The compiled pattern behind a RegExp object.
    pub fn regex_of(object: &ObjectRef) -> Option<Rc<CompiledRegex>> {
        match &object.borrow().kind {
            Kind::RegExp(regex) => Some(regex.clone()),
            _ => None,
        }
    }

    /// Build a RegExp value, used by regex literals in the AST.
    pub fn create(interp: &mut Interp, source: &str, flags: &str) -> Result<Value, Control> {
        match compile(source, flags) {
            Some(regex) => {
                let object =
                    Object::with_kind(Kind::RegExp(regex), Some(interp.prototypes.regexp.clone()));
                Ok(Value::Object(object))
            }
            None => Err(interp.throw_error(
                "SyntaxError",
                format!("invalid regular expression: /{source}/{flags}"),
            )),
        }
    }

    /// `String.prototype.match` and friends.
    pub fn match_values(
        interp: &mut Interp,
        regex: &Rc<CompiledRegex>,
        text: &str,
    ) -> Result<Value, Control> {
        if regex.global {
            let mut collected = Vec::new();
            for found in regex.regex.find_iter(text) {
                collected.push(Value::str(found.as_str()));
            }
            if collected.is_empty() {
                return Ok(Value::Null);
            }
            return Ok(interp.new_array(collected));
        }
        match regex.regex.captures(text) {
            Some(captures) => {
                let mut items = Vec::new();
                for index in 0..captures.len() {
                    items.push(match captures.get(index) {
                        Some(m) => Value::str(m.as_str()),
                        None => Value::Undefined,
                    });
                }
                let array = interp.new_array(items);
                let offset = captures.get(0).map(|m| m.start()).unwrap_or(0);
                if let Value::Object(object) = &array {
                    object.borrow_mut().props.set("index", Value::Num(offset as f64));
                    object.borrow_mut().props.set("input", Value::str(text));
                }
                Ok(array)
            }
            None => Ok(Value::Null),
        }
    }

    /// `String.prototype.replace` with a regular expression.
    pub fn replace(
        interp: &mut Interp,
        regex: &Rc<CompiledRegex>,
        text: &str,
        replacement: Option<String>,
        replacement_value: &Value,
        all: bool,
    ) -> Result<Value, Control> {
        let is_callable_checked = matches!(replacement_value, Value::Object(object) if object.borrow().is_callable());
        if let Some(replacement) = replacement {
            let _ = is_callable_checked;
            let result = if all || regex.global {
                regex.regex.replace_all(text, replacement.as_str())
            } else {
                regex.regex.replace(text, replacement.as_str())
            };
            return Ok(Value::str(result.into_owned()));
        }
        // A function replacement receives the whole match.
        let mut out = String::new();
        let mut last = 0usize;
        let mut count = 0usize;
        for found in regex.regex.find_iter(text) {
            if count > 0 && !(all || regex.global) {
                break;
            }
            out.push_str(&text[last..found.start()]);
            let replacement =
                interp.call_value(replacement_value, Value::Undefined, vec![Value::str(found.as_str())])?;
            out.push_str(&interp.to_string(&replacement)?);
            last = found.end();
            count += 1;
        }
        if count == 0 {
            return Ok(Value::str(text));
        }
        out.push_str(&text[last..]);
        Ok(Value::str(out))
    }

    /// Install the `RegExp` constructor and its prototype methods.
    pub fn install(interp: &mut Interp) {
        let proto = interp.prototypes.regexp.clone();

        def(&*interp, &proto, "test", 1, |interp, this, args| {
            let regex = match this {
                Value::Object(object) => match regex_of(object) {
                    Some(regex) => regex,
                    None => return interp.type_error("not a regular expression"),
                },
                _ => return interp.type_error("not a regular expression"),
            };
            let text = interp.to_string(&arg(args, 0))?;
            Ok(Value::Bool(regex.regex.is_match(&text)))
        });
        def(&*interp, &proto, "exec", 1, |interp, this, args| {
            let regex = match this {
                Value::Object(object) => match regex_of(object) {
                    Some(regex) => regex,
                    None => return interp.type_error("not a regular expression"),
                },
                _ => return interp.type_error("not a regular expression"),
            };
            let text = interp.to_string(&arg(args, 0))?;
            match_values(interp, &regex, &text)
        });
        def(&*interp, &proto, "toString", 0, |interp, this, _args| {
            match this {
                Value::Object(object) => match regex_of(object) {
                    Some(regex) => Ok(Value::str(format!("/{}/{}", regex.source, regex.flags))),
                    None => interp.type_error("not a regular expression"),
                },
                _ => interp.type_error("not a regular expression"),
            }
        });
        def_accessor(
            &*interp,
            &proto,
            "source",
            Some(|interp, this, _args| match this {
                Value::Object(object) => match regex_of(object) {
                    Some(regex) => Ok(Value::str(&regex.source)),
                    None => interp.type_error("not a regular expression"),
                },
                _ => interp.type_error("not a regular expression"),
            }),
            None,
        );
        def_accessor(
            &*interp,
            &proto,
            "flags",
            Some(|interp, this, _args| match this {
                Value::Object(object) => match regex_of(object) {
                    Some(regex) => Ok(Value::str(&regex.flags)),
                    None => interp.type_error("not a regular expression"),
                },
                _ => interp.type_error("not a regular expression"),
            }),
            None,
        );
        def_accessor(
            &*interp,
            &proto,
            "global",
            Some(|interp, this, _args| match this {
                Value::Object(object) => match regex_of(object) {
                    Some(regex) => Ok(Value::Bool(regex.global)),
                    None => interp.type_error("not a regular expression"),
                },
                _ => interp.type_error("not a regular expression"),
            }),
            None,
        );

        let constructor = interp.native("RegExp", 2, |interp, _this, args| {
            let pattern = match arg(args, 0) {
                Value::Object(object) => match regex_of(&object) {
                    Some(regex) => regex.source.clone(),
                    None => String::new(),
                },
                value => interp.to_string(&value)?,
            };
            let flags = match args.get(1) {
                Some(Value::Undefined) | None => String::new(),
                Some(value) => interp.to_string(value)?,
            };
            create(interp, &pattern, &flags)
        });
        if let Value::Object(function) = &constructor {
            function
                .borrow_mut()
                .props
                .set("prototype", Value::Object(proto.clone()));
            proto
                .borrow_mut()
                .props
                .set("constructor", constructor.clone());
        }
        interp.define_global("RegExp", constructor);
    }
}

/// A JSON reader and writer.
pub mod json {
    use super::*;

    /// The neutral tree produced by [`parse`], before values are materialised.
    pub enum Raw {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Array(Vec<Raw>),
        Object(Vec<(String, Raw)>),
    }

    /// Parse JSON text. Returns `None` on malformed input.
    pub fn parse(text: &str) -> Option<Raw> {
        let chars: Vec<char> = text.chars().collect();
        let mut parser = JsonParser { chars, pos: 0 };
        parser.skip_whitespace();
        let value = parser.value()?;
        parser.skip_whitespace();
        if parser.pos == parser.chars.len() {
            Some(value)
        } else {
            None
        }
    }

    struct JsonParser {
        chars: Vec<char>,
        pos: usize,
    }

    impl JsonParser {
        fn peek(&self) -> Option<char> {
            self.chars.get(self.pos).copied()
        }

        fn bump(&mut self) -> Option<char> {
            let c = self.peek()?;
            self.pos += 1;
            Some(c)
        }

        fn skip_whitespace(&mut self) {
            while matches!(self.peek(), Some(' ') | Some('\t') | Some('\n') | Some('\r')) {
                self.pos += 1;
            }
        }

        fn value(&mut self) -> Option<Raw> {
            match self.peek()? {
                '{' => self.object(),
                '[' => self.array(),
                '"' => self.string().map(Raw::Str),
                't' => self.literal("true").map(|_| Raw::Bool(true)),
                'f' => self.literal("false").map(|_| Raw::Bool(false)),
                'n' => self.literal("null").map(|_| Raw::Null),
                _ => self.number(),
            }
        }

        fn literal(&mut self, word: &str) -> Option<()> {
            for expected in word.chars() {
                if self.bump()? != expected {
                    return None;
                }
            }
            Some(())
        }

        fn number(&mut self) -> Option<Raw> {
            let start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E')
            {
                self.pos += 1;
            }
            if start == self.pos {
                return None;
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            text.parse::<f64>().ok().map(Raw::Num)
        }

        fn string(&mut self) -> Option<String> {
            if self.bump()? != '"' {
                return None;
            }
            let mut out = String::new();
            loop {
                match self.bump()? {
                    '"' => return Some(out),
                    '\\' => match self.bump()? {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        '/' => out.push('/'),
                        '\\' => out.push('\\'),
                        '"' => out.push('"'),
                        'u' => {
                            let mut hex = String::new();
                            for _ in 0..4 {
                                hex.push(self.bump()?);
                            }
                            let code = u32::from_str_radix(&hex, 16).ok()?;
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => out.push(other),
                    },
                    c => out.push(c),
                }
            }
        }

        fn array(&mut self) -> Option<Raw> {
            self.bump(); // '['
            let mut items = Vec::new();
            self.skip_whitespace();
            if self.peek() == Some(']') {
                self.bump();
                return Some(Raw::Array(items));
            }
            loop {
                self.skip_whitespace();
                items.push(self.value()?);
                self.skip_whitespace();
                match self.bump()? {
                    ',' => continue,
                    ']' => return Some(Raw::Array(items)),
                    _ => return None,
                }
            }
        }

        fn object(&mut self) -> Option<Raw> {
            self.bump(); // '{'
            let mut entries = Vec::new();
            self.skip_whitespace();
            if self.peek() == Some('}') {
                self.bump();
                return Some(Raw::Object(entries));
            }
            loop {
                self.skip_whitespace();
                let key = self.string()?;
                self.skip_whitespace();
                if self.bump()? != ':' {
                    return None;
                }
                self.skip_whitespace();
                let value = self.value()?;
                entries.push((key, value));
                self.skip_whitespace();
                match self.bump()? {
                    ',' => continue,
                    '}' => return Some(Raw::Object(entries)),
                    _ => return None,
                }
            }
        }
    }

    /// Serialise a value. Returns `None` for `undefined`, like `JSON.stringify`.
    pub fn stringify(
        interp: &mut Interp,
        value: &Value,
        indent: Option<usize>,
    ) -> Result<Option<String>, Control> {
        write_value(interp, value, indent, 0)
    }

    fn write_value(
        interp: &mut Interp,
        value: &Value,
        indent: Option<usize>,
        depth: usize,
    ) -> Result<Option<String>, Control> {
        Ok(match value {
            Value::Undefined => None,
            Value::Null => Some("null".to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Num(n) => Some(if n.is_finite() {
                format_number(*n)
            } else {
                "null".to_string()
            }),
            Value::BigInt(n) => Some(n.to_string()),
            Value::Str(text) => Some(quote(text)),
            Value::Object(object) => {
                let borrowed = object.borrow();
                if borrowed.callable.is_some() {
                    return Ok(None);
                }
                // A `toJSON` method takes precedence, as in the spec.
                drop(borrowed);
                let to_json = interp.get_prop(value, "toJSON")?;
                if matches!(&to_json, Value::Object(o) if o.borrow().is_callable()) {
                    let replaced = interp.call_value(&to_json, value.clone(), Vec::new())?;
                    return write_value(interp, &replaced, indent, depth);
                }

                let is_array = matches!(&value, Value::Object(o) if o.borrow().is_array());
                if is_array {
                    let items = interp.array_items(value)?;
                    let mut parts = Vec::new();
                    for item in items {
                        parts.push(
                            write_value(interp, &item, indent, depth + 1)?
                                .unwrap_or_else(|| "null".to_string()),
                        );
                    }
                    Some(join_parts(&parts, indent, depth, "[", "]"))
                } else {
                    let keys: Vec<String> = match &value {
                        Value::Object(object) => object.borrow().enumerable_keys(),
                        _ => Vec::new(),
                    };
                    let mut parts = Vec::new();
                    for key in keys {
                        let item = interp.get_prop(value, &key)?;
                        if let Some(rendered) = write_value(interp, &item, indent, depth + 1)? {
                            parts.push(format!("{}:{}", quote(&key), rendered));
                        }
                    }
                    Some(join_parts(&parts, indent, depth, "{", "}"))
                }
            }
        })
    }

    fn join_parts(
        parts: &[String],
        indent: Option<usize>,
        depth: usize,
        open: &str,
        close: &str,
    ) -> String {
        if parts.is_empty() {
            return format!("{open}{close}");
        }
        match indent {
            Some(width) if width > 0 => {
                let inner_padding = " ".repeat(width * (depth + 1));
                let padding = " ".repeat(width * depth);
                let separator = format!(",\n{inner_padding}");
                format!(
                    "{open}\n{inner_padding}{}{close}",
                    parts.join(&separator)
                )
                .replace(&format!("{close}"), &format!("\n{padding}{close}"))
            }
            _ => format!("{open}{}{close}", parts.join(",")),
        }
    }

    fn quote(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

impl Interp {
    /// Wrap a primitive in an object, used by `Object(primitive)`.
    pub fn wrap_primitive(&self, value: &Value) -> ObjectRef {
        let prototype = match value {
            Value::Str(_) => self.prototypes.string.clone(),
            Value::Num(_) => self.prototypes.number.clone(),
            Value::Bool(_) => self.prototypes.boolean.clone(),
            _ => self.prototypes.object.clone(),
        };
        let object = Object::new(Some(prototype));
        // The wrapped value is stored under a hidden slot and read through
        // `valueOf` by the prototype methods.
        object.borrow_mut().props.set("$value", value.clone());
        object
    }
}
