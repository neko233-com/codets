# rust4ts

A **pure-Rust JavaScript & TypeScript engine with native TypeScript support**.
Embed a JS/TS runtime in your Rust application — no C toolchain, no V8, no
Node.js.

```bash
cargo add rust4ts
```

```rust
use rust4ts::{Language, Runtime};

let mut runtime = Runtime::new();
runtime.echo_to_stdout();
runtime.eval(
    "enum Color { Red, Green = 5 }\n\
     const label = (c: Color): string => `color ${c}`;\n\
     console.log(Color[5], label(Color.Green));",
    Language::Ts,
)?;
// prints: Green color 5
```

Or from the command line:

```bash
rust4ts plugin.ts        # run a module, resolving relative imports
rust4ts eval "console.log([1,2,3].map(n => n * 2).join(','))"
```

## Why another JavaScript engine?

Because every existing embedding option costs you something a Rust crate should
not cost:

| | `rust4ts` | `rquickjs` / QuickJS | `deno_core` / V8 |
|---|---|---|---|
| C/C++ toolchain required | **no** | yes | yes |
| Cross-compilation | **plain `cargo build`** | needs a C cross-compiler | needs a V8 toolchain |
| Binary size | **small** | ~1 MB | tens of MB |
| Language | **native TypeScript** | transpile first | transpile first |
| Executes npm packages | no | no | mostly yes |
| Speed | slow | fast | fastest |

`rust4ts` is a from-scratch tree-walking interpreter written in 100% Rust. Its
only dependency is the pure-Rust [`regex`](https://crates.io/crates/regex)
crate. It targets **your own plugin and application code**, not the npm
ecosystem — see [Limits](#limits-read-this-first).

## Native TypeScript

There is no transpile step and no `.d.ts` pipeline. The parser understands
TypeScript directly and erases types while building the AST, so nothing about
types survives into the runtime.

| Construct | What happens |
|---|---|
| `: T` annotations, `as`, `satisfies`, `!` | erased |
| `<T>` generics on functions, classes, arrows | erased |
| `interface`, `type X = ...` | produce nothing |
| `declare ...`, `import type`, inline `type` specifiers | produce nothing |
| `public` / `private` / `protected` / `readonly` / `abstract` / `override` | erased |
| `@decorator` | parsed and dropped, **with a warning** |
| Function overload signatures | dropped; the implementation wins |
| `enum` / `const enum` | **kept**: real object plus reverse mapping for numbers |
| `constructor(private x: number)` | **kept**: injects `this.x = x` |

`enum` and parameter properties are the only two TypeScript constructs with
runtime meaning, so they are the only two that survive.

Type-only imports and exports produce no code, so a module that exports only
types still loads.

## Embedding it in a host application

```rust
use rust4ts::value::Value;
use rust4ts::{Language, Runtime};

let mut runtime = Runtime::with_root("./plugins");

// Expose Rust functions to scripts.
runtime.register_fn("log", |runtime, _this, args| {
    let line: Vec<String> = args.iter().map(|v| v.to_display_string()).collect();
    runtime.print_line(&line.join(" "));
    Ok(Value::Undefined)
});

// Run a plugin and inspect its exports.
let exports = runtime.run_module(std::path::Path::new("./plugins/hello.ts"))?;
let _default = runtime.interp.get_prop(&exports, "default")?;
```

`Runtime::eval` runs a script in the global scope, `Runtime::run_module` loads an
ES module from disk and returns its namespace object, `ModuleLoader::clear_cache`
lets a host pick up edited plugin files.

## CLI

```
rust4ts <file.ts|file.js>     Run a module
rust4ts eval "<code>"         Evaluate a snippet
rust4ts --version | --help
```

Uncaught errors go to stderr and set exit code 1.

## What works

* **Language**: closures and lexical scoping, `var`/`let`/`const`, destructuring
  (nested, defaults, rest), spread, template literals, tagged templates,
  optional chaining and `??`, `typeof`/`instanceof`/`in`/`delete`, labelled
  `break`/`continue`, `switch`, `try`/`catch`/`finally`, `for`/`for-in`/`for-of`,
  classes with `extends`/`super`/`static`/getters/setters/fields/static blocks.
* **Modules**: ES modules only, with correct dependency-first evaluation and
  import cycles that terminate. `import.meta` is available.
* **Built-ins**: `Object`, `Array`, `String`, `Number`, `Boolean`, `Math`,
  `JSON`, `Date` (basic), `RegExp`, `Map`, `Set`, `Promise`, the `Error` family,
  `console`, `parseInt`/`parseFloat`, `setTimeout`/`setInterval` on a virtual
  clock.
* **Async**: `async`/`await` and promise reactions, microtasks before timers.

## Limits (read this first)

These are deliberate, documented boundaries rather than bugs:

* **No npm.** Bare specifiers such as `import x from "lodash"` are rejected with
  an explanatory error; only relative and absolute paths resolve. There is no
  `node_modules` lookup and no CommonJS `require`.
* **No generators.** `function*` and `yield` parse but throw at run time.
* **No dynamic `import()`.** Use static imports.
* **Not fast.** A tree-walking interpreter, roughly 10-100x slower than a JIT.
* **No garbage collector.** Values are reference counted, so reference cycles
  are never freed. Long-running hosts should avoid building cyclic graphs.
* **No type checking.** Types are erased, exactly like Node's type stripping.
* `Object.freeze` and friends are accepted but do not actually freeze.
* **Regular expressions** are backed by the `regex` crate, so backreferences and
  lookaround (`/(?=x)/`, `/(a)\1/`) are rejected. `g`/`i`/`m`/`s` work.
* `String.prototype.length` counts Unicode code points, not UTF-16 code units,
  so it differs from JavaScript for non-BMP characters (emoji).
* **Not implemented**: `Symbol`, `Proxy`, `Reflect`, `WeakMap`/`WeakSet`,
  `Intl`, `encodeURIComponent`, JSX, `.tsx`.
* TypeScript `namespace` / `module` blocks are rejected with a clear error.

### Documented behaviour differences

* An `async` function body runs **eagerly** and its result is wrapped in a
  promise, instead of being deferred to a later microtask. Promise ordering for
  `then` chains is unaffected.
* `await` **drives the event loop** until the promise settles, then throws a
  deadlock error if no further events are pending. That makes `await sleep(10)`
  work without a suspension state machine.
* Instance field initialisers run **before** the constructor body. A real engine
  runs a derived class's fields after `super()` returns.
* Module exports are snapshotted when the module body finishes, so a module in an
  import cycle sees `undefined` for bindings that are declared later in the
  cycle's other half.
* No JSX, and no explicit type arguments in a *value* position beyond the call
  syntax `f<T>(x)` / `new C<T>()`, which is supported.

## Architecture

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). In short: `lexer` → `parser`
(which erases TypeScript) → `ast` → `interp` (tree walking) on top of the
`value` object model, with `builtins` supplying the standard library and
`modules` doing ES module resolution. The engine sits behind a narrow interface
so a different VM could be swapped in later without changing the public API.

## Development

```bash
cargo test        # 73 tests: lexer, parser, runtime semantics, modules, doc example
cargo run -- tests/fixtures/modules/main.ts
```

On Windows with a Visual Studio installation that only ships the `onecore`
toolset libraries, linking fails with `LNK1104: cannot open file 'msvcrt.lib'`.
Point `LIB` at the MSVC `onecore\x64` folder plus the Windows SDK's `ucrt\x64`
and `um\x64` folders before running cargo.

## 中文说明

`rust4ts` 是一个**纯 Rust** 的 JavaScript / TypeScript 引擎，**原生支持
TypeScript**：解析器直接吃 TS 语法并在构建 AST 时擦除类型，没有独立的编译步骤。
它适合把 JS/TS 插件嵌入 Rust 宿主程序（游戏、桌面外壳、插件系统），不需要 C 工具
链，也不依赖 Node.js。唯一的第三方依赖是纯 Rust 的 `regex`。

crate 名称是 `rust4ts`，因为 crates.io 上 `rustts` 已被他人占用（其描述为
`DEPRECATED: renamed to manivelle`），而 `rust-ts` 去掉连字符后与 `rustts` 归一化
相同，会被 crates.io 判为重复。

**能力边界（重要）**：不支持 npm/`node_modules`、CommonJS、生成器、动态
`import()`；性能不追求（树遍历解释器）；引用计数不做循环回收；`enum` 与构造器参数
属性是仅有的两个具有运行时语义的 TS 构造。完整清单见上文 *Limits*。

## License

MIT.
