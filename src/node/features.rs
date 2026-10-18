//! Standard-library features gated on the selected Node.js release.
//!
//! Every function here is installed only when [`NodeCompat::supports`] says the
//! chosen release has it, so `--node-version 18` genuinely lacks `Object.groupBy`
//! while `--node-version 22` has it. The gates are the ones a real package would
//! hit when it branches on `process.versions.node`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::interp::{Control, Interp};
use crate::value::{Kind, Object, ObjectRef, PromiseState, Value};

use super::compat::{Feature, NodeCompat};

/// Install every feature the selected release has.
pub fn install(interp: &mut Interp, compat: NodeCompat) {
    if compat.supports(Feature::ObjectHasOwn) {
        install_object_has_own(interp);
    }
    if compat.supports(Feature::StructuredClone) {
        install_structured_clone(interp);
    }
    if compat.supports(Feature::ChangeByCopy) {
        install_change_by_copy(interp);
    }
    if compat.supports(Feature::ObjectGroupBy) {
        install_object_group_by(interp);
    }
    if compat.supports(Feature::PromiseWithResolvers) {
        install_promise_with_resolvers(interp);
    }
    if compat.supports(Feature::SetMethods) {
        install_set_methods(interp);
    }
    if compat.supports(Feature::ErrorIsError) {
        install_error_is_error(interp);
    }
    if compat.supports(Feature::RegExpEscape) {
        install_regexp_escape(interp);
    }
    if compat.supports(Feature::MathSumPrecise) {
        install_math_sum_precise(interp);
    }
}

// --------------------------------------------------------------------- helpers

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

/// Install a method on an object or prototype, following the built-ins' rules:
/// the property is non-enumerable.
fn def<F>(interp: &Interp, target: &ObjectRef, name: &str, length: usize, body: F)
where
    F: Fn(&mut Interp, &Value, &[Value]) -> Result<Value, Control> + 'static,
{
    let value = interp.native(name, length, body);
    let mut borrowed = target.borrow_mut();
    borrowed.props.set(name, value);
    borrowed.hide_from_enumeration(name);
}

/// The global constructor or namespace object with this name.
fn global_object(interp: &Interp, name: &str) -> ObjectRef {
    match interp.global_object.borrow().props.get(name) {
        Some(Value::Object(object)) => object.clone(),
        _ => interp.prototypes.object.clone(),
    }
}

/// The items of a `Set`, or `None` when `value` is not one.
fn set_items(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Object(object) => match &object.borrow().kind {
            Kind::Set(items) => Some(items.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn contains(items: &[Value], needle: &Value) -> bool {
    items.iter().any(|item| item.strict_equals(needle))
}

// ---------------------------------------------------------------- Object.hasOwn

fn install_object_has_own(interp: &mut Interp) {
    let target = global_object(interp, "Object");
    def(interp, &target, "hasOwn", 2, |interp, _this, args| {
        let key = interp.key_to_string(&arg(args, 1))?;
        Ok(match arg(args, 0) {
            Value::Object(object) => {
                let borrowed = object.borrow();
                Value::Bool(
                    borrowed.props.contains(&key) || borrowed.accessors.contains_key(&key),
                )
            }
            _ => Value::Bool(false),
        })
    });
}

// ------------------------------------------------------------ structuredClone

fn install_structured_clone(interp: &mut Interp) {
    interp.define_global(
        "structuredClone",
        interp.native("structuredClone", 1, |interp, _this, args| {
            let mut seen = HashMap::new();
            clone_value(interp, &arg(args, 0), &mut seen)
        }),
    );
}

/// Deep-copy a value the way `structuredClone` does.
///
/// `seen` maps an already-visited object to its copy, which is what lets a cyclic
/// graph clone instead of recursing forever.
fn clone_value(
    interp: &mut Interp,
    value: &Value,
    seen: &mut HashMap<*const std::cell::RefCell<Object>, Value>,
) -> Result<Value, Control> {
    let object = match value {
        Value::Object(object) => object.clone(),
        primitive => return Ok(primitive.clone()),
    };
    let key = Rc::as_ptr(&object);
    if let Some(existing) = seen.get(&key) {
        return Ok(existing.clone());
    }
    if object.borrow().callable.is_some() {
        return interp.type_error("structuredClone cannot clone a function");
    }

    let kind = match &object.borrow().kind {
        Kind::Plain => None,
        Kind::Array => Some(Kind::Array),
        Kind::Map(entries) => {
            let mut cloned = Vec::new();
            for (entry_key, entry_value) in entries.clone() {
                cloned.push((
                    clone_value(interp, &entry_key, seen)?,
                    clone_value(interp, &entry_value, seen)?,
                ));
            }
            Some(Kind::Map(cloned))
        }
        Kind::Set(items) => {
            let mut cloned = Vec::new();
            for item in items.clone() {
                cloned.push(clone_value(interp, &item, seen)?);
            }
            Some(Kind::Set(cloned))
        }
        Kind::Date(ms) => Some(Kind::Date(*ms)),
        Kind::RegExp(regex) => Some(Kind::RegExp(regex.clone())),
        Kind::Error => Some(Kind::Error),
        Kind::Promise(_) => {
            return interp.type_error("structuredClone cannot clone a Promise");
        }
    };

    let prototype = object.borrow().prototype.clone();
    let copy = match kind {
        Some(kind) => Object::with_kind(kind, prototype),
        None => Object::new(prototype),
    };
    seen.insert(key, Value::Object(copy.clone()));

    let (properties, _accessor_names) = {
        let borrowed = object.borrow();
        let properties: Vec<(String, Value)> = borrowed
            .props
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        // Accessors are not cloned: evaluating a getter during a clone could run
        // arbitrary script, which `structuredClone` does not do either.
        let accessor_names: Vec<String> = borrowed.accessors.keys().cloned().collect();
        (properties, accessor_names)
    };
    for (name, property) in properties {
        // `length` on an array is derived from the elements, so it is rebuilt
        // below rather than copied.
        if copy.borrow().is_array() && name == "length" {
            continue;
        }
        let cloned = clone_value(interp, &property, seen)?;
        copy.borrow_mut().props.set(name, cloned);
    }
    if copy.borrow().is_array() {
        let length = object.borrow().array_length();
        copy.borrow_mut().props.set("length", Value::Num(length as f64));
    }
    Ok(Value::Object(copy))
}

// ------------------------------------------------------------- change by copy

fn install_change_by_copy(interp: &mut Interp) {
    let array = interp.prototypes.array.clone();
    def(interp, &array, "toSorted", 1, |interp, this, args| {
        let items = interp.array_items(this)?;
        let copy = interp.new_array(items);
        let sort = interp.get_prop(&copy, "sort")?;
        interp.call_value(&sort, copy.clone(), args.to_vec())?;
        Ok(copy)
    });
    def(interp, &array, "toReversed", 0, |interp, this, _args| {
        let items = interp.array_items(this)?;
        let copy = interp.new_array(items);
        let reverse = interp.get_prop(&copy, "reverse")?;
        interp.call_value(&reverse, copy.clone(), Vec::new())?;
        Ok(copy)
    });
    def(interp, &array, "toSpliced", 2, |interp, this, args| {
        let items = interp.array_items(this)?;
        let copy = interp.new_array(items);
        let splice = interp.get_prop(&copy, "splice")?;
        interp.call_value(&splice, copy.clone(), args.to_vec())?;
        Ok(copy)
    });
    def(interp, &array, "with", 2, |interp, this, args| {
        let mut items = interp.array_items(this)?;
        let length = items.len() as i64;
        let raw = arg(args, 0).to_number();
        if !raw.is_finite() {
            return interp.range_error("invalid index passed to Array.prototype.with");
        }
        let mut index = raw.trunc() as i64;
        if index < 0 {
            index += length;
        }
        if index < 0 || index >= length {
            return interp.range_error("index out of range in Array.prototype.with");
        }
        let replacement = arg(args, 1);
        items[index as usize] = replacement;
        Ok(interp.new_array(items))
    });

    let string = interp.prototypes.string.clone();
    // `codets` stores strings as Rust `String`, which cannot hold a lone
    // surrogate, so every string is well formed by construction.
    def(interp, &string, "isWellFormed", 0, |_interp, _this, _args| {
        Ok(Value::Bool(true))
    });
    def(interp, &string, "toWellFormed", 0, |interp, this, _args| {
        let text = interp.to_string(this)?;
        Ok(Value::str(text))
    });
}

// -------------------------------------------------------------- Object.groupBy

fn install_object_group_by(interp: &mut Interp) {
    let target = global_object(interp, "Object");
    def(interp, &target, "groupBy", 2, |interp, _this, args| {
        let items = interp.iterate(&arg(args, 0))?;
        let callback = arg(args, 1);
        if !matches!(&callback, Value::Object(object) if object.borrow().callable.is_some()) {
            return interp.type_error("Object.groupBy requires a callback function");
        }
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Value> = HashMap::new();
        for (index, item) in items.iter().enumerate() {
            let key = interp.call_value(
                &callback,
                Value::Undefined,
                vec![item.clone(), Value::Num(index as f64)],
            )?;
            let key = interp.key_to_string(&key)?;
            if !groups.contains_key(&key) {
                order.push(key.clone());
                groups.insert(key.clone(), interp.new_array(Vec::new()));
            }
            let bucket = groups.get(&key).cloned().unwrap_or(Value::Undefined);
            let mut contents = interp.array_items(&bucket)?;
            contents.push(item.clone());
            let replacement = interp.new_array(contents);
            groups.insert(key, replacement);
        }
        let result = interp.new_object();
        for key in order {
            if let Some(bucket) = groups.get(&key) {
                result.borrow_mut().props.set(key, bucket.clone());
            }
        }
        Ok(Value::Object(result))
    });
}

// --------------------------------------------------- Promise.withResolvers

fn install_promise_with_resolvers(interp: &mut Interp) {
    let target = global_object(interp, "Promise");
    def(interp, &target, "withResolvers", 0, |interp, _this, _args| {
        let promise = interp.new_promise(Rc::new(PromiseState::new()));
        let resolve = interp.native("resolve", 1, {
            let promise = promise.clone();
            move |interp, _this, args| {
                interp.fulfill(&promise, arg(args, 0));
                Ok(Value::Undefined)
            }
        });
        let reject = interp.native("reject", 1, {
            let promise = promise.clone();
            move |interp, _this, args| {
                interp.reject(&promise, arg(args, 0));
                Ok(Value::Undefined)
            }
        });
        let result = interp.new_object();
        {
            let mut borrowed = result.borrow_mut();
            borrowed.props.set("promise", promise);
            borrowed.props.set("resolve", resolve);
            borrowed.props.set("reject", reject);
        }
        Ok(Value::Object(result))
    });
}

// ------------------------------------------------------------------ Set methods

fn install_set_methods(interp: &mut Interp) {
    let set = interp.prototypes.set.clone();

    // Every method returns a fresh Set, so they share one builder.
    fn build(interp: &Interp, items: Vec<Value>) -> Value {
        Value::Object(Object::with_kind(
            Kind::Set(items),
            Some(interp.prototypes.set.clone()),
        ))
    }

    def(interp, &set, "union", 1, |interp, this, args| {
        let Some(mut items) = set_items(this) else {
            return interp.type_error("Set.prototype.union called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.union expects a Set");
        };
        for item in other {
            if !contains(&items, &item) {
                items.push(item);
            }
        }
        Ok(build(interp, items))
    });
    def(interp, &set, "intersection", 1, |interp, this, args| {
        let Some(items) = set_items(this) else {
            return interp.type_error("Set.prototype.intersection called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.intersection expects a Set");
        };
        let shared: Vec<Value> = items
            .into_iter()
            .filter(|item| contains(&other, item))
            .collect();
        Ok(build(interp, shared))
    });
    def(interp, &set, "difference", 1, |interp, this, args| {
        let Some(items) = set_items(this) else {
            return interp.type_error("Set.prototype.difference called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.difference expects a Set");
        };
        let rest: Vec<Value> = items
            .into_iter()
            .filter(|item| !contains(&other, item))
            .collect();
        Ok(build(interp, rest))
    });
    def(interp, &set, "symmetricDifference", 1, |interp, this, args| {
        let Some(items) = set_items(this) else {
            return interp.type_error("Set.prototype.symmetricDifference called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.symmetricDifference expects a Set");
        };
        let mut both: Vec<Value> = items
            .iter()
            .filter(|item| !contains(&other, item))
            .cloned()
            .collect();
        for item in other {
            if !contains(&items, &item) && !contains(&both, &item) {
                both.push(item);
            }
        }
        Ok(build(interp, both))
    });
    def(interp, &set, "isSubsetOf", 1, |interp, this, args| {
        let Some(items) = set_items(this) else {
            return interp.type_error("Set.prototype.isSubsetOf called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.isSubsetOf expects a Set");
        };
        Ok(Value::Bool(items.iter().all(|item| contains(&other, item))))
    });
    def(interp, &set, "isSupersetOf", 1, |interp, this, args| {
        let Some(items) = set_items(this) else {
            return interp.type_error("Set.prototype.isSupersetOf called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.isSupersetOf expects a Set");
        };
        Ok(Value::Bool(other.iter().all(|item| contains(&items, item))))
    });
    def(interp, &set, "isDisjointFrom", 1, |interp, this, args| {
        let Some(items) = set_items(this) else {
            return interp.type_error("Set.prototype.isDisjointFrom called on a non-Set");
        };
        let Some(other) = set_items(&arg(args, 0)) else {
            return interp.type_error("Set.prototype.isDisjointFrom expects a Set");
        };
        Ok(Value::Bool(!other.iter().any(|item| contains(&items, item))))
    });
}

// ----------------------------------------------------------------- Error.isError

fn install_error_is_error(interp: &mut Interp) {
    let target = global_object(interp, "Error");
    def(interp, &target, "isError", 1, |_interp, _this, args| {
        Ok(Value::Bool(match arg(args, 0) {
            Value::Object(object) => matches!(object.borrow().kind, Kind::Error),
            _ => false,
        }))
    });
}

// ---------------------------------------------------------------- RegExp.escape

fn install_regexp_escape(interp: &mut Interp) {
    let target = global_object(interp, "RegExp");
    def(interp, &target, "escape", 1, |interp, _this, args| {
        let text = interp.to_string(&arg(args, 0))?;
        let mut escaped = String::with_capacity(text.len());
        for c in text.chars() {
            // Only the characters that carry meaning inside a pattern need a
            // backslash; anything else stays literal, which is what Node does.
            if matches!(
                c,
                '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
            ) {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        Ok(Value::str(escaped))
    });
}

// ------------------------------------------------------------- Math.sumPrecise

fn install_math_sum_precise(interp: &mut Interp) {
    let target = global_object(interp, "Math");
    def(interp, &target, "sumPrecise", 1, |interp, _this, args| {
        let items = interp.iterate(&arg(args, 0))?;
        // Neumaier summation: keep the low-order bits that a plain running total
        // would drop, which is the whole point of the function.
        let mut sum = 0.0f64;
        let mut compensation = 0.0f64;
        for item in items {
            let value = item.to_number();
            let tentative = sum + value;
            if sum.abs() >= value.abs() {
                compensation += (sum - tentative) + value;
            } else {
                compensation += (value - tentative) + sum;
            }
            sum = tentative;
        }
        Ok(Value::Num(sum + compensation))
    });
}
