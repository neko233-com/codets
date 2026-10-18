//! The `process` global.
//!
//! The shape follows Node: `argv`, `argv0`, `execPath`, `execArgv`, `version`,
//! `versions`, `platform`, `arch`, `pid`, `env`, the `stdout` / `stderr`
//! streams, `exit` / `exitCode`, the timers, and enough of the event emitter for
//! `process.on('exit', ...)` to work.
//!
//! Deliberate gaps, so nobody debugs a phantom: heap accounting is not
//! implemented, so `memoryUsage()` reports zeros; `process.env` is a snapshot
//! that child processes would not see (there is no `child_process` yet); and
//! `process.kill` is not supported.

use std::path::PathBuf;
use std::rc::Rc;

use crate::interp::{Control, Interp};
use crate::value::{ObjectRef, Value};

use super::NodeHost;

/// Install one property on a built object.
fn set(object: &ObjectRef, key: &str, value: Value) {
    object.borrow_mut().props.set(key, value);
}

/// `process.platform` spelling for the running target.
fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

/// `process.arch` spelling for the running target.
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        "arm" => "arm",
        other => other,
    }
}

/// Build the `process` object for `host`.
pub fn build(host: &Rc<NodeHost>, interp: &mut Interp) -> Value {
    let process = interp.new_object();

    // ------------------------------------------------------------ plain data
    set(&process, "execPath", Value::str(&host.exec_path));
    set(&process, "version", Value::str(host.compat.v_string()));
    set(&process, "platform", Value::str(platform()));
    set(&process, "arch", Value::str(arch()));
    set(&process, "pid", Value::Num(std::process::id() as f64));

    // `argv`, `argv0` and `execArgv` are accessors rather than snapshots, because
    // the command line sets them after this object is built and Node's values are
    // live for the whole run.
    let argv_getter = interp.native("get argv", 0, {
        let host = host.clone();
        move |interp, _this, _args| {
            Ok(interp.new_array(host.argv().into_iter().map(Value::str).collect()))
        }
    });
    process
        .borrow_mut()
        .accessors
        .insert("argv".to_string(), (Some(argv_getter), None));
    let argv0_getter = interp.native("get argv0", 0, {
        let host = host.clone();
        move |_interp, _this, _args| Ok(Value::str(host.argv0.borrow().as_str()))
    });
    process
        .borrow_mut()
        .accessors
        .insert("argv0".to_string(), (Some(argv0_getter), None));
    let exec_argv_getter = interp.native("get execArgv", 0, {
        let host = host.clone();
        move |interp, _this, _args| {
            Ok(interp.new_array(
                host.exec_argv
                    .borrow()
                    .iter()
                    .cloned()
                    .map(Value::str)
                    .collect(),
            ))
        }
    });
    process
        .borrow_mut()
        .accessors
        .insert("execArgv".to_string(), (Some(exec_argv_getter), None));

    let versions = interp.new_object();
    set(&versions, "node", Value::str(host.compat.version_string()));
    set(&versions, "codets", Value::str(host.codets_version));
    set(&process, "versions", Value::Object(versions));

    let release = interp.new_object();
    set(&release, "name", Value::str("node"));
    set(
        &release,
        "lts",
        match host.compat.lts_name() {
            Some(name) => Value::str(name),
            None => Value::Bool(false),
        },
    );
    set(&process, "release", Value::Object(release));

    // `process.features` describes the runtime, so it reports what `codets`
    // actually implements rather than mirroring Node's V8/build flags.
    let features = interp.new_object();
    set(&features, "esm", Value::Bool(true));
    set(&features, "typescript", Value::Bool(true));
    set(&features, "inspector", Value::Bool(false));
    set(&process, "features", Value::Object(features));

    let env = interp.new_object();
    {
        let variables = host.env.borrow().clone();
        let mut borrowed = env.borrow_mut();
        for (name, value) in variables {
            borrowed.props.set(name, Value::str(value));
        }
    }
    set(&process, "env", Value::Object(env));

    // ------------------------------------------------------------- streams
    let stdout = interp.new_object();
    install_stream(&stdout, interp, host, false);
    set(&process, "stdout", Value::Object(stdout));
    let stderr = interp.new_object();
    install_stream(&stderr, interp, host, true);
    set(&process, "stderr", Value::Object(stderr));

    // ----------------------------------------------------------- exit code
    let getter = interp.native("get exitCode", 0, {
        let host = host.clone();
        move |_interp, _this, _args| Ok(Value::Num(host.exit_code.get() as f64))
    });
    let setter = interp.native("set exitCode", 1, {
        let host = host.clone();
        move |interp, _this, args| {
            let code = match args.first() {
                None | Some(Value::Undefined) | Some(Value::Null) => 0,
                Some(value) => {
                    let number = value.to_number();
                    if !number.is_finite() {
                        return interp.type_error("process.exitCode must be a number");
                    }
                    number as i32
                }
            };
            host.exit_code.set(code);
            Ok(Value::Undefined)
        }
    });
    process
        .borrow_mut()
        .accessors
        .insert("exitCode".to_string(), (Some(getter), Some(setter)));

    // -------------------------------------------------------- process.exit
    set(
        &process,
        "exit",
        interp.native("exit", 1, {
            let host = host.clone();
            move |_interp, _this, args| {
                let code = args.first().map(|value| value.to_number()).unwrap_or(0.0);
                let code = if code.is_finite() { code as i32 } else { 0 };
                host.exit_code.set(code);
                host.exited.set(true);
                Err(Control::Exit(code))
            }
        }),
    );
    set(
        &process,
        "abort",
        interp.native("abort", 0, {
            let host = host.clone();
            move |_interp, _this, _args| {
                // 134 is what a genuine abort reports: 128 + SIGABRT.
                host.exit_code.set(134);
                host.exited.set(true);
                Err(Control::Exit(134))
            }
        }),
    );

    // ----------------------------------------------------------- directory
    set(
        &process,
        "cwd",
        interp.native("cwd", 0, {
            let host = host.clone();
            move |_interp, _this, _args| {
                Ok(Value::str(host.cwd.borrow().to_string_lossy().to_string()))
            }
        }),
    );
    set(
        &process,
        "chdir",
        interp.native("chdir", 1, {
            let host = host.clone();
            move |interp, _this, args| {
                let target = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                let path = PathBuf::from(&target);
                if let Err(error) = std::env::set_current_dir(&path) {
                    return Err(interp.throw_error(
                        "Error",
                        format!("ENOENT: cannot chdir to `{target}`: {error}"),
                    ));
                }
                *host.cwd.borrow_mut() = path;
                Ok(Value::Undefined)
            }
        }),
    );

    // ------------------------------------------------------------- timers
    set(
        &process,
        "nextTick",
        interp.native("nextTick", 1, |interp, _this, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            let extra: Vec<Value> = args.iter().skip(1).cloned().collect();
            interp.queue_microtask(Box::new(move |interp| {
                interp.call_value(&callback, Value::Undefined, extra.clone())?;
                Ok(())
            }));
            Ok(Value::Undefined)
        }),
    );
    set(
        &process,
        "uptime",
        interp.native("uptime", 0, {
            let host = host.clone();
            move |_interp, _this, _args| Ok(Value::Num(host.started.elapsed().as_secs_f64()))
        }),
    );
    let hrtime = build_hrtime(interp, &host);
    set(&process, "hrtime", hrtime);
    set(
        &process,
        "memoryUsage",
        interp.native("memoryUsage", 0, |interp, _this, _args| {
            // `codets` has no heap accounting; the fields exist so that code
            // that reads them keeps working, and they are documented as zeros.
            let usage = interp.new_object();
            for field in ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"] {
                set(&usage, field, Value::Num(0.0));
            }
            Ok(Value::Object(usage))
        }),
    );

    // ------------------------------------------------------------- events
    set(
        &process,
        "on",
        interp.native("on", 2, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                host.on(&event, args.get(1).cloned().unwrap_or(Value::Undefined), false);
                Ok(Value::Undefined)
            }
        }),
    );
    set(
        &process,
        "addListener",
        interp.native("addListener", 2, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                host.on(&event, args.get(1).cloned().unwrap_or(Value::Undefined), false);
                Ok(Value::Undefined)
            }
        }),
    );
    set(
        &process,
        "once",
        interp.native("once", 2, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                host.on(&event, args.get(1).cloned().unwrap_or(Value::Undefined), true);
                Ok(Value::Undefined)
            }
        }),
    );
    set(
        &process,
        "off",
        interp.native("off", 2, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
                host.off(&event, &listener);
                Ok(Value::Undefined)
            }
        }),
    );
    set(
        &process,
        "removeListener",
        interp.native("removeListener", 2, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
                host.off(&event, &listener);
                Ok(Value::Undefined)
            }
        }),
    );
    set(
        &process,
        "removeAllListeners",
        interp.native("removeAllListeners", 1, {
            let host = host.clone();
            move |interp, _this, args| {
                match args.first() {
                    None | Some(Value::Undefined) => host.remove_all_listeners(None),
                    Some(value) => {
                        let event = interp.to_string(value)?;
                        host.remove_all_listeners(Some(&event));
                    }
                }
                Ok(Value::Undefined)
            }
        }),
    );
    set(
        &process,
        "emit",
        interp.native("emit", 1, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                let rest: Vec<Value> = args.iter().skip(1).cloned().collect();
                Ok(Value::Bool(host.emit(interp, &event, &rest)?))
            }
        }),
    );
    set(
        &process,
        "listeners",
        interp.native("listeners", 1, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                Ok(interp.new_array(host.listeners_for(&event)))
            }
        }),
    );
    set(
        &process,
        "listenerCount",
        interp.native("listenerCount", 1, {
            let host = host.clone();
            move |interp, _this, args| {
                let event = interp.to_string(args.first().unwrap_or(&Value::Undefined))?;
                Ok(Value::Num(host.listener_count(&event) as f64))
            }
        }),
    );

    Value::Object(process)
}

/// `process.hrtime`, plus the `hrtime.bigint` variant.
fn build_hrtime(interp: &mut Interp, host: &Rc<NodeHost>) -> Value {
    let hrtime = interp.native("hrtime", 1, {
        let host = host.clone();
        move |interp, _this, _args| {
            let elapsed = host.started.elapsed();
            let seconds = elapsed.as_secs();
            let nanos = elapsed.subsec_nanos();
            Ok(interp.new_array(vec![
                Value::Num(seconds as f64),
                Value::Num(nanos as f64),
            ]))
        }
    });
    let bigint = interp.native("bigint", 0, {
        let host = host.clone();
        move |_interp, _this, _args| {
            let elapsed = host.started.elapsed();
            Ok(Value::BigInt(elapsed.as_nanos() as i128))
        }
    });
    if let Value::Object(object) = &hrtime {
        object.borrow_mut().props.set("bigint", bigint);
    }
    hrtime
}

/// Give an object the `write` / `end` / `isTTY` surface of a writable stream.
fn install_stream(object: &ObjectRef, interp: &mut Interp, host: &Rc<NodeHost>, is_stderr: bool) {
    let _ = host;
    set(
        object,
        "write",
        interp.native("write", 1, move |interp, _this, args| {
            let chunk = args.first().cloned().unwrap_or(Value::Undefined);
            let text = interp.to_string(&chunk)?;
            if is_stderr {
                interp.print_error(&text);
            } else {
                interp.print_raw(&text);
            }
            Ok(Value::Bool(true))
        }),
    );
    set(
        object,
        "end",
        interp.native("end", 1, move |interp, _this, args| {
            if let Some(chunk) = args.first() {
                if !chunk.is_nullish() {
                    let text = interp.to_string(chunk)?;
                    if is_stderr {
                        interp.print_error(&text);
                    } else {
                        interp.print_raw(&text);
                    }
                }
            }
            Ok(Value::Undefined)
        }),
    );
    let is_tty = if is_stderr {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    } else {
        use std::io::IsTerminal;
        std::io::stdout().is_terminal()
    };
    set(object, "isTTY", Value::Bool(is_tty));
}
