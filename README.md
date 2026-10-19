# codets

A **pure-Rust JavaScript & TypeScript toolchain** that replaces Node.js and npm.

One build produces three commands, so `node`, `npm` and `codets` on your `PATH` are
the same program wearing three hats:

| Command | Stands in for | What it does |
|---|---|---|
| `codets` | a new toolchain | Runs code, manages the installed Node compatibility levels |
| `node` | `node` | Runs scripts with Node's flags, globals and exit codes |
| `npm` | `npm` | Resolves, downloads, verifies and unpacks packages |

TypeScript runs directly — there is no build step, no `ts-node`, no transpiler in
the way. It is not Node.js under the hood: the engine, the module loaders, the
standard library and the package manager are all original Rust, and the only
native code involved is the platform's own TLS stack.

```bash
codets app.ts                    # run TypeScript
node app.js                      # Node-compatible entry point
npm install                      # install dependencies
codets use 22                    # switch the Node compatibility level
```

## Install

Windows:

```powershell
irm https://raw.githubusercontent.com/neko233-com/codets/main/install.ps1 | iex
```

Linux and macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/neko233-com/codets/main/install.sh | sh
```

Both pick the archive for your platform from the
[releases](https://github.com/neko233-com/codets/releases), check it against the
release's `codets-<version>-checksums.txt`, and hand the binary to `codets
bootstrap`, which writes the store and puts `codets`, `node` and `npm` on `PATH`.
Arguments go to `bootstrap`, so `... | sh -s -- --level 22` claims Node 22 rather
than the built-in default. `CODETS_VERSION` pins a release, `CODETS_HOME` moves
the store, and `CODETS_DOWNLOAD_BASE` / `CODETS_API_BASE` point the download at a
mirror.

Published builds cover Windows, Linux and macOS on both x64 and ARM64, except
macOS, which is Apple Silicon only: an Intel Mac is told there is no published build
rather than given one it cannot run.

Or build it yourself and install that build:

```bash
cargo build --release
./target/release/codets bootstrap
```

## Status

This is an early but real implementation, and the numbers below were produced by
the commands in this repository rather than estimated:

| | |
|---|---|
| Tests | **208 tests, all passing** (`cargo test`), plus 5 that need the network |
| Real npm registry install | verified: `npm install` fetched `is-odd@3.0.1` and its transitive `is-number` from `registry.npmjs.org`, and `node index.js` ran the program |
| Parallel install throughput | **5.34×** faster with 8 workers than 1 (24 packages, 41.8 MiB unpacked, identical trees) — `scripts/bench-npm-install.ps1` |
| JavaScript throughput baseline | ~2.2M simple loop iterations/second (`bench-npm-install.ps1`, debug build) |

Read [Limits](#limits-read-this-before-trusting-it) before trusting it with
anything that matters. In particular: native addons do not work, and the
JavaScript engine is a tree-walking interpreter, so it is far slower than V8.

## Native TypeScript

No transpile step, no `.d.ts` pipeline. The parser understands TypeScript directly
and erases types while building the AST, so nothing about types survives into the
runtime — the same strategy Node itself uses for type stripping.

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

`enum` and parameter properties are the only two TypeScript constructs with runtime
meaning, so they are the only two that survive.

## Node compatibility levels

`codets` claims to be a specific Node.js release. That release decides what
`process.version` reports and which standard-library features exist, so a package
that branches on the Node version takes the branch it would take on real Node.

```console
$ node --node-version=18 -p "typeof Object.groupBy"
undefined
$ node --node-version=24 -p "typeof Object.groupBy"
function
```

Supported lines: **18, 20, 21, 22, 23, 24** (the default is 24 LTS, `Krypton`).
`codets features` lists what the active level provides and, for the ones it does
not, the Node release that would.

Each gated feature is really implemented, not merely advertised:

| Feature | Node | What it installs |
|---|---|---|
| `Object.hasOwn` | 16 | the static method |
| `structuredClone` | 17 | a deep clone, with cycle support |
| `toSorted` / `toReversed` / `toSpliced` / `with`, `isWellFormed` / `toWellFormed` | 20 | the copy-on-write array methods |
| `Object.groupBy` | 21 | grouping into arrays |
| `Promise.withResolvers` | 22 | promise plus its resolvers |
| `Set.prototype.union` and friends | 22 | all seven set operations |
| `Error.isError` | 24 | the static check |
| `RegExp.escape` | 24 | pattern escaping |
| `Math.sumPrecise` | 24 | Neumaier summation |

## The version manager

`codets` manages its own toolchains the way `rustup` and `fnm` do, so a project can
pin the Node release it targets and switch in one command.

```console
$ codets install 22                 # materialise the 22 LTS toolchain
$ codets use 22                     # make it active for this machine
$ codets use 22 --local             # ...or pin it for this project only
$ codets current                    # what is active, and why
$ codets versions                   # installed and available
$ codets env                        # PATH setup
$ codets upgrade                    # replace this toolchain with a published build
```

Resolution order for the active version, first match wins:

1. `$CODETS_NODE_VERSION`
2. the nearest `.codets-version` file, walking up from the working directory
3. the `current` pointer (`codets use`)
4. `default` in the store's settings (`codets default`)
5. the built-in default

The store lives in `~/.codets` (or `$CODETS_HOME`):

```text
~/.codets/
  bin/                     what goes on PATH: codets, node, npm
  versions/22.12.0/        one toolchain: its own binary, its `node` and `npm`, a manifest
  current                  the active version
  settings.conf            `default=<version>`
```

`bin/` holds links to the active version's binary, not shell shims, and a version
directory holds its own copy — so `versions/22.12.0/node app.ts` pins Node 22
whatever `current` says, and `node.exe` never loses to a `node.cmd` on `PATH`.

Installing a version copies the running binary into a version directory, which
needs no network. `codets upgrade` is the only step that does: it fetches a
published release, checks it against the release's checksum file, and installs it.
`codets upgrade --from <path>` does the same from a build you already have.

## The package manager

`npm install` implements npm's own file formats, so a project installed by npm can
be installed by `codets` and the other way round.

* **Resolution** — `^`, `~`, `>=`, `<`, `x`, `||` and intersections, with
  pre-release rules. Each package name is resolved once against every range
  requested for it.
* **Registries** — the npm registry protocol over HTTPS, or a directory on disk
  that holds packuments and tarballs (which is how the tests run offline).
* **Integrity** — `sha512` digests are verified before anything is written.
* **Unpacking** — gunzip and untar, with the archive's leading `package/`
  directory stripped and escaping paths rejected.
* **Binaries** — every declared `bin` gets a shim in `node_modules/.bin`.
* **Lockfile** — `package-lock.json` is written in version 3's shape, with the
  `resolved` URL each package came from.
* **Scripts** — `npm run <script>` runs with `node_modules/.bin` on `PATH` and
  npm's lifecycle variables set. `npm exec`, `npm list` and `npm init` work too.

### Where packages come from

The registry is resolved the way npm resolves it, highest priority first:

1. `--registry <url|directory>`
2. `NPM_CONFIG_REGISTRY`, or npm's lowercase `npm_config_registry`
3. the project's `.npmrc`, found by walking up to the project root
4. the user's `.npmrc` — `NPM_CONFIG_USERCONFIG`, `CODETS_NPMRC`, or `~/.npmrc`
5. `https://registry.npmjs.org`

So a mirror, a proxy or an internal registry needs no flag on any command:

```console
$ cat .npmrc
registry=https://registry.npmmirror.com/

$ npm config get registry                 # https://registry.npmmirror.com/
$ npm config get registry --origin        # ...and which of the five decided it
$ npm install                             # from the mirror, on every command
```

A `.npmrc` is not searched above the project root, which is the nearest directory
holding a `package.json` or a `node_modules`. Walking further up would let an
unrelated file — a stray `~/.npmrc`, say — change where a project installs from.

### Where the parallelism lives

JavaScript execution is single-threaded, because the language is. The parallelism
is in the installer, where it actually pays: metadata resolution, then download,
verification and unpacking of every package at once, on as many workers as you ask
for.

```console
$ npm install --jobs 8
added 24 packages in 0.04s (transfer 0.03s on 8 threads, 41.8 MiB)
$ npm install --jobs 1
added 24 packages in 0.24s (transfer 0.23s on 1 threads, 41.8 MiB)
```

Reproduce that with `scripts/bench-npm-install.ps1`, which builds a local registry
of real gzipped tarballs and reports both runs plus the speed-up. Both runs are
checked to produce identical trees, so the comparison is meaningful.

### The Vite 8 demo

`examples/vite8-demo` is a real Vite 8 project, installed from the public registry
rather than from a fixture. `scripts/verify-demo.ps1` (or `.sh`) drives it end to
end and fails on the first thing that is wrong:

```console
$ scripts/verify-demo.ps1
== 3/6 npm install
added 40 packages in 6.23s (transfer 2.11s on 8 threads, 366.7 MiB)
warnings: 0
== 4/6 what the install produced
vite 8.3.4, 40 packages in the lockfile, vite shim present
== 5/6 the demo's own TypeScript
vite@8
codets@0.3
vite + codets: 2 releases described
```

That run is what found the 10 MiB response limit described in the CHANGELOG: the
packument for `vite` is larger, so before that fix the package could not be installed
at all.

What the demo does **not** do is run Vite's own command line, and it says so on every
run: `vite dev` and `vite build` need `child_process` and native modules. See
[`examples/vite8-demo/README.md`](examples/vite8-demo/README.md).

## What works

* **Language** — closures and lexical scoping, `var`/`let`/`const`, destructuring
  (nested, defaults, rest), spread, template literals and tagged templates,
  optional chaining and `??`, `typeof`/`instanceof`/`in`/`delete`, labelled
  `break`/`continue`, `switch`, `try`/`catch`/`finally`, `for`/`for-in`/`for-of`,
  classes with `extends`/`super`/`static`/getters/setters/fields/static blocks.
* **Modules** — ES modules with dependency-first evaluation, import cycles,
  `import.meta`, and **CommonJS**: `require`, `module`, `exports`, `__filename`,
  `__dirname`, the `node_modules` walk, `package.json` `exports` and `main`,
  `require.resolve`, JSON modules. `import` of a CommonJS package works through
  Node-style interop (`module.exports` as `default`, its properties as named
  exports).
* **Core modules** — `path`, `fs` (synchronous subset), `os`, `events`
  (including `class X extends EventEmitter`), `util` (including `promisify`),
  `assert`, `querystring`, `buffer`, `timers`, `module` (including
  `createRequire`), `process`, all with or without the `node:` prefix.
* **`process`** — `argv`, `argv0`, `execPath`, `execArgv`, `version`, `versions`,
  `platform`, `arch`, `pid`, `env`, `cwd`, `chdir`, `exit`, `exitCode`, `stdout`,
  `stderr`, `nextTick`, `hrtime`, `uptime`, `memoryUsage`, `release`, `features`,
  and enough of the event emitter for `process.on('exit')` and
  `process.on('uncaughtException')` to work.
* **Built-ins** — `Object`, `Array`, `String`, `Number`, `Boolean`, `Math`,
  `JSON`, `Date`, `RegExp`, `Map`, `Set`, `Promise`, the `Error` family,
  `console`, `parseInt`/`parseFloat`, `structuredClone`, `setTimeout` and friends
  on a virtual clock, with microtasks before timers.
* **Async** — `async`/`await` and promise reactions.

## Limits (read this before trusting it)

These are boundaries, not bugs, and each one is a deliberate stopping point rather
than an oversight:

**Nothing that needs V8.**

* **Native addons do not work.** A `.node` file binds V8's ABI, which a pure-Rust
  engine does not expose. `require('binding.node')` fails with an explanation.
  Any package that ships or depends on one cannot run here.
* **`Symbol`, `Proxy`, `Reflect`, `WeakMap`/`WeakSet`, `Intl` are not
  implemented**, so packages that build on them — many ORMs, polyfills and
  transpiled decorator stacks — will not run.
* **No generators.** `function*` and `yield` parse but throw at run time.
* **No dynamic `import()`.** Use static imports.
* **No garbage collector.** Values are reference counted, so reference cycles are
  never freed. A long-running process that builds cyclic graphs will grow.
* **Not fast.** A tree-walking interpreter, roughly 10-100x slower than a JIT.
  This phase did not attempt interpreter optimisation; that needs a bytecode VM.

**Deliberate gaps in the toolchain.**

* **Dependency resolution is flat.** Each package name resolves to one version. If
  two ranges cannot be satisfied by the same version, the installer picks the best
  for the first and warns; it does not build a nested tree. No `overrides`,
  `resolutions` or workspaces.
* **No `audit`, `publish`, `update`, `link` or `dedupe`.** No `npx` shorthand
  beyond `npm exec`.
* **`require.cache` is a placeholder**, so cache eviction is not available.
* **Importing a *relative* CommonJS file from an ES module is not interop-wrapped**;
  only bare package specifiers are. Reach relative CommonJS through `require`.
* **`codets upgrade` installs unverified when a release has no checksum file.** A
  mismatch always stops it, and it never loads native code, but an archive served
  without a checksum is trusted.
* **The Vite command line cannot run.** `vite dev` and `vite build` spawn esbuild
  through `child_process` and load Rolldown and lightningcss as native `.node`
  modules; neither is implemented here. The Vite 8 demo installs its dependency tree
  and runs its own TypeScript, which is what it claims to do.
* **Scoped registries are not honoured.** `@scope:registry=` is read as an ordinary
  key and ignored, so a package that needs its own registry has to be fetched with
  `--registry` instead.
* **A single response may not exceed 512 MiB.** That is a guard against a runaway
  server rather than a limit real packages reach; the largest registry documents seen
  are tens of megabytes.
* **TLS is the platform's** (`native-tls`), so a Linux build needs the system
  OpenSSL headers at build time. This keeps `codets` itself free of a bundled TLS
  engine.

**Engine details worth knowing.**

* `Object.freeze` and friends are accepted but do not actually freeze.
* Regular expressions use the `regex` crate, so backreferences and lookaround
  (`/(?=x)/`, `/(a)\1/`) are rejected. `g`/`i`/`m`/`s` work.
* `String.prototype.length` counts Unicode code points, not UTF-16 code units, so
  it differs from JavaScript for non-BMP characters (emoji).
* `process.env` is a snapshot object; mutating it does not reach child processes
  (there is no `child_process` yet).
* `process.memoryUsage()` reports zeros: heap accounting is not implemented.
* `fs.readFileSync` returns a string even without an encoding, because there are no
  typed arrays for a `Buffer` to be backed by. `Buffer` is a plain object with
  numeric properties, so indexing, `length`, `toString`, `slice` and `equals` work
  but the binary codecs (`readUInt32LE` and friends) do not exist.
* `EventEmitter`'s `once` stores a self-removing wrapper, so `listeners()` returns
  the wrapper and `removeListener(event, original)` cannot find it. Removing by the
  wrapper works.
* `vm`, `child_process`, `worker_threads`, `net`, `http`, `zlib` and `crypto` are
  not implemented.
* TypeScript `namespace` / `module` blocks are rejected with a clear error.

### Documented behaviour differences

* An `async` function body runs **eagerly** and its result is wrapped in a promise,
  instead of being deferred to a later microtask.
* `await` **drives the event loop** until the promise settles, then throws a
  deadlock error if no further events are pending.
* Instance field initialisers run **before** the constructor body, in a derived
  class too.
* Module exports are snapshotted when the module body finishes, so a module in an
  import cycle sees `undefined` for bindings declared later in the cycle's other
  half.
* `path.posix` and `path.win32` are the same object: the module follows the host's
  separator rules rather than emulating the other family.
* `codets install` creates the toolchain directory locally. Version levels *are*
  the Node compatibility level of the running binary; a level that needs different
  engine code needs a new `codets` build.
* `--node-version` accepts a major and uses that line's baseline release
  (`18` → `18.20.4`); a full version is taken exactly as written.

## Architecture

```text
                 codets                 node                  npm
                    │                    │                     │
                    └──────── cli.rs ────┴─────────────────────┘
                                 │
              ┌──────────────────┼───────────────────┐
              ▼                  ▼                   ▼
        node/ (process,    commonjs.rs +       npm/ (semver, registry,
        compat, features)  core_modules.rs     install, lockfile)
              │                  │                   │
              └──────────────────┼───────────────────┘
                                 ▼
   lexer.rs → parser.rs (erases TypeScript) → ast.rs → interp.rs
                                                          │
                                    value.rs │ builtins.rs │ modules.rs
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design decisions behind
each layer.

## Building

```bash
cargo build --release      # three binaries in target/release
cargo test                 # 208 tests, offline
```

On Windows with a Visual Studio installation that ships only the `onecore` toolset
libraries, linking fails with `LNK1104: cannot open file 'msvcrt.lib'`. Point `LIB`
at the MSVC `onecore\x64` folder plus the Windows SDK's `ucrt\x64` and `um\x64`
folders before running cargo.

Three scripts check what `cargo test` cannot, and all three need the network:
`scripts/pre-release` is the gate a release has to pass before its tag is pushed,
`scripts/verify-demo` installs the Vite 8 demo from the real npm registry, and
`scripts/verify-release` downloads and checks every asset of a published release
after it exists. `AGENTS.md` describes when each runs.

## Repository layout

```text
src/
  lexer.rs  parser.rs  ast.rs          the front end, TypeScript-aware
  interp.rs  value.rs  builtins.rs     the engine and its standard library
  modules.rs                           the ES module loader
  cli.rs                               the three command personalities
  node/                                process, compatibility levels, features
  commonjs.rs  core_modules.rs         require and the Node standard library
  npm/                                 semver, registries, the installer
  versions.rs                          the built-in version manager
  release.rs                           where its own releases live, for `upgrade`
  net.rs                               the single place that uses the network
tests/
  cli.rs  commonjs.rs  npm_install.rs  runtime.rs   fixtures/
  release_contract.rs  install_scripts.rs          what a published release is
  idempotency.rs  net.rs  live_registry.rs         twice, limits, the real thing
  support/                           scratch directories and a fake release server
examples/vite8-demo/                 a real Vite project, installed by codets
docs/ARCHITECTURE.md
AGENTS.md                            the release rule and the testing rules
scripts/bench-npm-install.ps1
scripts/pre-release.*                the gate a release has to pass locally
scripts/check-temp-clean.*           the leak check
scripts/verify-demo.*                the online end-to-end run
install.sh  install.ps1              the one-line installers
.github/workflows/release.yml        builds the assets both of them expect
```

## History

This project was published as **`rust4ts`** (`0.1.0`), an embeddable JS/TS engine
for Rust hosts. `0.2.0` turns it into `codets`: the toolchain, the Node-compatible
command line, CommonJS, the core modules and the package manager are all new, and
the embedding API from `0.1.0` still works unchanged.

The work after `0.2.0` — self-upgrade, the one-line installers, the configurable
registry, the release pipeline, and the test suite that proves each of them — is
`0.3.0`; see [`CHANGELOG.md`](CHANGELOG.md).

## 中文说明

`codets` 是一个**纯 Rust** 的 JavaScript / TypeScript 工具链，用来替代
Node.js 与 npm。一次构建产出三个命令：`codets`、`node`、`npm`，放到 `PATH` 上
即可直接顶替原来的 `node` 与 `npm`。**原生支持 TypeScript**：解析器直接吃 TS
语法并在构建 AST 时擦除类型，没有独立的编译步骤。

已经真正跑通的（都有自动化测试或实测记录）：CommonJS 的 `require` 与
`node_modules` 解析、`package.json` 的 `exports`/`main`、核心模块
（`path`/`fs`/`os`/`events`/`util`/`assert`/`querystring`/`buffer`/`timers`/`module`/`process`）、
内置版本管理器（`codets install/use/default/current/versions`）、以及
**多线程并行安装**——24 个包用 8 线程比 1 线程快 **5.34 倍**（`scripts/bench-npm-install.ps1`
可复现，两次安装产物完全一致）。也已实测从 npm 官方源安装真实包
（`is-odd` + 传递依赖 `is-number`）并成功运行。

安装：Windows 用 `install.ps1`、Linux/macOS 用 `install.sh`（一行命令，见上文
*Install*），两者都会核对发布产物的 SHA-256 后再交给 `codets bootstrap`；
已经发布的版本可以用 `codets upgrade` 原地替换当前工具链。registry 的解析顺序
与 npm 一致（`--registry` → `NPM_CONFIG_REGISTRY` → 项目 `.npmrc` → 用户 `.npmrc`
→ 官方源），因此换镜像不需要给每条命令加参数；用
`examples/vite8-demo`（真实 Vite 8 项目）验证过完整依赖树安装与自带 TypeScript
直跑。

**能力边界（重要）**：JS 执行语义按语言规范保持单线程，多线程只存在于 npm
安装层；不支持原生插件（`.node`，它绑定 V8 的 ABI）、`Symbol`/`Proxy`/`Reflect`、
生成器、动态 `import()`；依赖解析是扁平的，不做嵌套树、workspaces、`overrides`；
解释器是树遍历实现，比 V8 慢一到两个数量级，本期没有做解释器层面的优化（那需要
字节码 VM）；`fs.readFileSync` 总是返回字符串，因为没有 TypedArray。完整清单见上文
*Limits*。

## License

MIT.
