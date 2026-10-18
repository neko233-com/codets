//! CommonJS and core-module tests, driven through the real loader.
//!
//! Every case writes a small project into a scratch directory and runs it, so the
//! resolution rules (`node_modules` walking, `package.json` `main`, extension
//! order, cycles) are exercised as a program would exercise them.

use std::path::{Path, PathBuf};

use codets::{Error, Language, Runtime};

mod support;

use support::Scratch;

/// The console output of running `entry` in a project built from `files`.
struct Outcome {
    output: String,
    error: Option<String>,
}

/// Write a small project into a scratch directory, which the caller keeps for as
/// long as it needs and which removes itself afterwards.
fn project(name: &str, files: &[(&str, &str)]) -> Scratch {
    let root = Scratch::new(name);
    for (relative, body) in files {
        root.write(relative, body);
    }
    root
}

fn run_commonjs(name: &str, files: &[(&str, &str)], entry: &str) -> Outcome {
    let root = project(name, files);
    let mut runtime = Runtime::with_root(root.path());
    let buffer = runtime.capture_output();
    let result = codets::commonjs::run_entry(&mut runtime, &root.join(entry));
    let _ = runtime.interp.run_event_loop();
    let output = buffer.borrow().clone();
    Outcome {
        output,
        error: result.as_ref().err().map(|error| error.to_string()),
    }
}

/// Run an ES module, for the cases that interop with CommonJS.
fn run_module(name: &str, files: &[(&str, &str)], entry: &str) -> Outcome {
    let root = project(name, files);
    let mut runtime = Runtime::with_root(root.path());
    let buffer = runtime.capture_output();
    let result = runtime.run_module(&root.join(entry));
    let _ = runtime.interp.run_event_loop();
    let output = buffer.borrow().clone();
    Outcome {
        output,
        error: result.as_ref().err().map(|error| error.to_string()),
    }
}

fn expect_ok(outcome: &Outcome) -> &str {
    if let Some(error) = &outcome.error {
        panic!("expected success, got: {error}");
    }
    outcome.output.trim()
}

#[test]
fn require_loads_a_relative_file() {
    let outcome = run_commonjs(
        "relative",
        &[
            ("main.js", "const { double } = require('./math');\nconsole.log(double(21));"),
            ("math.js", "exports.double = (n) => n * 2;"),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "42");
}

#[test]
fn require_loads_json() {
    let outcome = run_commonjs(
        "json",
        &[
            ("main.js", "const config = require('./config.json');\nconsole.log(config.name, config.items.length);"),
            ("config.json", "{\n  \"name\": \"demo\",\n  \"items\": [1, 2, 3]\n}\n"),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "demo 3");
}

#[test]
fn module_exports_reassignment_wins() {
    let outcome = run_commonjs(
        "reassign",
        &[
            ("main.js", "const greet = require('./greet');\nconsole.log(typeof greet, greet('world'));"),
            ("greet.js", "exports.ignored = true;\nmodule.exports = (name) => `hello ${name}`;"),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "function hello world");
}

#[test]
fn the_node_modules_walk_finds_a_hoisted_package() {
    // `deep/inner.js` requires a package that lives at the project root, which is
    // the layout an installer produces.
    let outcome = run_commonjs(
        "walk",
        &[
            (
                "main.js",
                "const value = require('./lib/deep/inner');\nconsole.log(value);",
            ),
            ("lib/deep/inner.js", "module.exports = require('hoisted');"),
            (
                "node_modules/hoisted/package.json",
                "{ \"name\": \"hoisted\", \"version\": \"1.0.0\", \"main\": \"lib/entry.js\" }\n",
            ),
            ("node_modules/hoisted/lib/entry.js", "module.exports = 'found';"),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "found");
}

#[test]
fn a_package_exports_map_is_honoured() {
    let outcome = run_commonjs(
        "exports-map",
        &[
            ("main.js", "console.log(require('mapped'));"),
            (
                "node_modules/mapped/package.json",
                "{\n  \"name\": \"mapped\",\n  \"version\": \"1.0.0\",\n  \"exports\": { \".\": { \"require\": \"./lib/cjs.js\", \"default\": \"./lib/esm.js\" } }\n}\n",
            ),
            ("node_modules/mapped/lib/cjs.js", "module.exports = 'commonjs entry';"),
            ("node_modules/mapped/lib/esm.js", "export default 'esm entry';"),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "commonjs entry");
}

#[test]
fn a_require_cycle_sees_partial_exports() {
    // `a` starts, requires `b`, and `b` requires `a` back before `a` finished.
    let outcome = run_commonjs(
        "cycle",
        &[
            (
                "main.js",
                "const a = require('./a');\nconsole.log(a.name, a.fromB);",
            ),
            (
                "a.js",
                "exports.name = 'a';\nconst b = require('./b');\nexports.fromB = b.name;",
            ),
            (
                "b.js",
                "exports.name = 'b';\nconst a = require('./a');\nexports.seenA = a.name;",
            ),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "a b");
}

#[test]
fn require_resolve_and_filename_are_available() {
    let outcome = run_commonjs(
        "resolve",
        &[
            (
                "main.js",
                "console.log(require.resolve('./lib.js').endsWith('lib.js'));\n\
                 console.log(__filename.endsWith('main.js'), __dirname.length > 0);",
            ),
            ("lib.js", "module.exports = 1;"),
        ],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "true\ntrue true");
}

#[test]
fn a_missing_package_explains_itself() {
    let outcome = run_commonjs(
        "missing",
        &[("main.js", "require('not-installed-anywhere');")],
        "main.js",
    );
    let error = outcome.error.expect("expected an error");
    assert!(error.contains("Cannot find module 'not-installed-anywhere'"), "{error}");
    assert!(error.contains("node_modules"), "{error}");
}

#[test]
fn native_addons_are_rejected_with_a_reason() {
    let outcome = run_commonjs(
        "native",
        &[
            ("main.js", "require('./binding.node');"),
            ("binding.node", "not really"),
        ],
        "main.js",
    );
    let error = outcome.error.expect("expected an error");
    assert!(error.contains("native addon"), "{error}");
    assert!(error.contains("V8"), "{error}");
}

#[test]
fn requiring_an_es_module_says_so() {
    let outcome = run_commonjs(
        "esm-via-cjs",
        &[
            ("main.js", "require('./esm.mjs');"),
            ("esm.mjs", "export default 1;"),
        ],
        "main.js",
    );
    let error = outcome.error.expect("expected an error");
    assert!(error.contains("ES module"), "{error}");
}

#[test]
fn core_modules_resolve_with_and_without_the_node_prefix() {
    let outcome = run_commonjs(
        "core",
        &[(
            "main.js",
            "const path = require('path');\n\
             const fs = require('node:fs');\n\
             const os = require('node:os');\n\
             console.log(path.basename('/a/b/c.txt'), typeof fs.readFileSync, typeof os.platform);",
        )],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "c.txt function function");
}

#[test]
fn the_path_module_follows_node_rules() {
    let outcome = run_commonjs(
        "path",
        &[(
            "main.js",
            "const path = require('path');\n\
             console.log([
               path.join('a', 'b', '..', 'c'),
               path.normalize('a//b/./c/../d'),
               path.dirname('a/b/c.js'),
               path.extname('.gitignore') === '' ? 'no-ext' : 'ext',
               path.extname('a.tar.gz'),
               path.relative('/a/b', '/a/c'),
               path.isAbsolute('/x'),
             ].join('|'));",
        )],
        "main.js",
    );
    // Separators follow the host, so the expectation is built the same way.
    let separator = std::path::MAIN_SEPARATOR;
    let expected = format!(
        "a{sep}c|a{sep}b{sep}d|a{sep}b|no-ext|.gz|..{sep}c|{absolute}",
        sep = separator,
        absolute = if cfg!(windows) { "false" } else { "true" }
    );
    assert_eq!(expect_ok(&outcome), expected);
}

#[test]
fn the_events_module_supports_extends_and_once() {
    let outcome = run_commonjs(
        "events",
        &[(
            "main.js",
            "const EventEmitter = require('events');\n\
             class Ticker extends EventEmitter {}\n\
             const ticker = new Ticker();\n\
             ticker.on('tick', (n) => console.log('tick', n, ticker instanceof EventEmitter));\n\
             ticker.emit('tick', 1);\n\
             ticker.emit('tick', 2);\n\
             let once = 0;\n\
             ticker.once('only', () => { once += 1; });\n\
             ticker.emit('only');\n\
             ticker.emit('only');\n\
             console.log('once', once, ticker.listenerCount('tick'));",
        )],
        "main.js",
    );
    assert_eq!(
        expect_ok(&outcome),
        "tick 1 true\ntick 2 true\nonce 1 1"
    );
}

#[test]
fn the_assert_module_checks_values() {
    let outcome = run_commonjs(
        "assert",
        &[(
            "main.js",
            "const assert = require('assert');\n\
             assert.strictEqual(1 + 1, 2);\n\
             assert.deepStrictEqual({ a: [1, 2] }, { a: [1, 2] });\n\
             assert.throws(() => { throw new Error('expected'); });\n\
             try { assert.strictEqual('a', 'b'); } catch (error) { console.log('caught', error.name); }\n\
             console.log('assertions ran');",
        )],
        "main.js",
    );
    let output = expect_ok(&outcome);
    // `assert.strictEqual` throws an AssertionError, which `error.name` reports.
    assert!(output.contains("caught AssertionError"), "{output}");
    assert!(output.contains("assertions ran"), "{output}");
}

#[test]
fn util_format_and_types_work() {
    let outcome = run_commonjs(
        "util",
        &[(
            "main.js",
            "const util = require('util');\n\
             console.log(util.format('%s=%d %%', 'count', 3));\n\
             console.log(util.types.isDate(new Date()), util.types.isRegExp(/x/), util.types.isString('s'));",
        )],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "count=3 %\ntrue true true");
}

#[test]
fn util_promisify_resolves_through_the_callback() {
    let outcome = run_commonjs(
        "promisify",
        &[(
            "main.js",
            "const util = require('util');\n\
             const read = (value, callback) => callback(null, value * 2);\n\
             const promisified = util.promisify(read);\n\
             promisified(21).then((value) => console.log('resolved', value));",
        )],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "resolved 42");
}

#[test]
fn querystring_and_buffer_work() {
    let outcome = run_commonjs(
        "misc",
        &[(
            "main.js",
            "const querystring = require('querystring');\n\
             const parsed = querystring.parse('a=1&b=two%20words');\n\
             console.log(querystring.stringify({ x: 'a b' }), parsed.a, parsed.b);\n\
             const buffer = require('buffer');\n\
             const data = buffer.Buffer.from('hi');\n\
             console.log(data.length, data[0], data.toString(), buffer.Buffer.isBuffer(data));",
        )],
        "main.js",
    );
    assert_eq!(expect_ok(&outcome), "x=a%20b 1 two words\n2 104 hi true");
}

#[test]
fn an_es_module_can_create_a_require() {
    let outcome = run_module(
        "create-require",
        &[
            (
                "main.mts",
                "import { createRequire } from 'module';\n\
                 const require = createRequire(import.meta.url);\n\
                 const math = require('./math.js');\n\
                 console.log('esm got', math.double(3));",
            ),
            ("math.js", "module.exports = { double: (n) => n * 2 };"),
        ],
        "main.mts",
    );
    assert_eq!(expect_ok(&outcome), "esm got 6");
}

#[test]
fn timers_run_after_the_body() {
    let outcome = run_commonjs(
        "timers",
        &[(
            "main.js",
            "const timers = require('timers');\n\
             console.log('start');\n\
             timers.setTimeout(() => console.log('later'), 5);\n\
             process.nextTick(() => console.log('tick'));",
        )],
        "main.js",
    );
    let output = expect_ok(&outcome);
    assert!(output.starts_with("start"), "{output}");
    assert!(output.contains("tick"), "{output}");
    assert!(output.trim_end().ends_with("later"), "{output}");
}

#[test]
fn a_syntax_error_names_the_file() {
    let outcome = run_commonjs(
        "syntax",
        &[("main.js", "const = ;")],
        "main.js",
    );
    let error = outcome.error.expect("expected an error");
    assert!(error.contains("main.js"), "{error}");
}

#[test]
fn the_runtime_api_still_works_for_embedding() {
    // The embedding surface from 0.1.0 must keep working unchanged.
    let mut runtime = Runtime::new();
    let output = runtime
        .run_source(
            "const greet = (who: string): string => `hi ${who}`;\nconsole.log(greet('world'));",
            Language::Ts,
        )
        .expect("run");
    assert_eq!(output.trim(), "hi world");
    assert!(matches!(runtime.eval("1 + 1", Language::Ts), Ok(_)));
    assert!(matches!(runtime.eval("throw new Error('x')", Language::Ts), Err(Error::Uncaught(_))));
}

#[test]
fn a_commonjs_file_found_by_extension_order() {
    // `require('./util')` must find `util/index.js` when there is no `util.js`.
    let root = project(
        "index-resolution",
        &[
            ("main.js", "console.log(require('./util'));"),
            ("util/index.js", "module.exports = 'from the directory index';"),
        ],
    );
    let mut runtime = Runtime::with_root(root.path());
    let buffer = runtime.capture_output();
    let _ = codets::commonjs::run_entry(&mut runtime, &root.join("main.js"));
    assert_eq!(buffer.borrow().trim(), "from the directory index");
}

/// A path helper used by the assertions above.
#[allow(dead_code)]
fn join(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}
