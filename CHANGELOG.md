# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-10-18

`rust4ts` becomes `codets`: a Node.js and npm replacement rather than an embedded
scripting engine. The engine from 0.1.0 is unchanged and its embedding API still
works; everything below is new around it.

### Added

* **Three command personalities from one binary.** `argv[0]` — or the
  `--codets-as` marker the version shims pass — selects the `codets`, `node` or
  `npm` surface, so the installed commands are drop-in replacements.
* **Node-compatible command line**: a script path with arguments, `-e`/`--eval`,
  `-p`/`--print`, `-c`/`--check`, `-i` and a REPL, `--input-type`, `--` for
  end-of-options, `--node-version`, and Node's exit codes (1 for an uncaught
  throw, 9 for an unusable command line). Unknown long options are accepted and
  recorded in `process.execArgv`; unknown short options are an error, like Node.
* **The `process` global**: `argv`, `argv0`, `execPath`, `execArgv`, `version`,
  `versions`, `platform`, `arch`, `pid`, `env`, `cwd`, `chdir`, `stdout`, `stderr`,
  `nextTick`, `hrtime`, `uptime`, `memoryUsage`, `release`, `features`, an
  `exitCode` accessor, and an event emitter covering `exit` and
  `uncaughtException`. `process.exit` is an uncatchable non-local exit, so a
  script `try`/`catch` cannot swallow it.
* **Node compatibility levels.** A run claims a specific Node release, which
  decides `process.version` and gates nine standard-library features that really
  are installed or withheld: `Object.hasOwn`, `structuredClone`, the
  `toSorted`/`toReversed`/`toSpliced`/`with` and `isWellFormed` family,
  `Object.groupBy`, `Promise.withResolvers`, the `Set` operations, `Error.isError`,
  `RegExp.escape` and `Math.sumPrecise`.
* **A built-in version manager**: `codets install`, `use` (with `--local` project
  pins), `default`, `current`, `versions`, `which`, `env`, `features` and
  `upgrade --from`. Resolution walks from `$CODETS_NODE_VERSION` through a
  `.codets-version` file to the `current` pointer and the store's default, and it
  writes `node`/`npm`/`codets` shims so switching one version switches all three.
* **CommonJS**: `require`, `module`, `exports`, `__filename`, `__dirname`,
  `require.resolve`, the `node_modules` walk, `package.json` `exports` (with
  conditions) and `main`, directory indices, JSON modules, and cycles that see the
  partially built exports instead of recursing.
* **ES-to-CommonJS interop**: `import` of a package resolves through the same
  `node_modules` rules and presents `module.exports` as the default export with its
  properties as named exports. Core modules are importable with or without the
  `node:` prefix.
* **Core modules**: `path`, `fs` (synchronous subset), `os`, `events` (including
  `class X extends EventEmitter` and `events.once`), `util` (including `promisify`,
  `inherits`, `format` and `types`), `assert`, `querystring`, `buffer`, `timers`,
  `module` (including `createRequire`) and `process`.
* **Standard-library gaps closed** that real packages hit immediately:
  `Number.isSafeInteger`, `Number.parseInt`, `Number.parseFloat` and `String.raw`,
  plus a JSON reader that tolerates a leading byte-order mark the way Node's does.
* **A package manager**: semver ranges (`^`, `~`, `>=`, `<`, `x`, `||`,
  intersections, pre-release rules), the npm registry protocol over HTTPS, a
  directory registry for offline use and mirrors, `sha512` integrity verification,
  gunzip + untar with the archive prefix stripped and escaping paths rejected,
  `node_modules/.bin` shims, `package-lock.json` in lockfile version 3's shape, and
  `install`, `ci`, `run`, `exec`, `list` and `init`.
* **Multi-threaded installation.** Metadata resolution, then download, verification
  and unpacking run across `--jobs` workers. Measured at **5.34×** the
  single-worker throughput on 24 packages (41.8 MiB unpacked), with both runs
  verified to produce identical trees. `scripts/bench-npm-install.ps1` reproduces
  it.
* **`codets upgrade --from <path>`**, which replaces a toolchain with a build you
  already have.

### Changed

* The crate, library, repository and binaries are named `codets`.
* `Runtime` now carries a Node host, and `Runtime::set_script` / `set_module_root`
  let a host describe what is running. `Runtime::with_compat` selects the Node
  compatibility level.
* The default CLI no longer prints help for a bare `codets`; it delegates to the
  toolchain's command surface.

### Fixed

* `process.argv` and `argv0` are live rather than captured when the process object
  is built, so a script sees its own name and arguments.
* `process.cwd()` reports the directory the command was run from; relative imports
  resolve against the importing file, as in Node.
* One `RefCell` borrow held across a `borrow_mut` in the `once` listener path
  panicked at run time; the lookup now binds its result first.
* The TLS provider is configured explicitly. ureq's default provider is Rustls, so
  enabling only the `native-tls` feature panicked on the first HTTPS request; the
  platform root store is used as well, which is what makes a corporate proxy work.

### Not included

Native addons, `Symbol`/`Proxy`/`Reflect`/`WeakMap`, generators, dynamic
`import()`, a garbage collector, nested dependency trees, workspaces,
`audit`/`publish`, remote self-upgrade, and any interpreter optimisation. See the
*Limits* section of the README, which distinguishes all of these from bugs.

## [0.1.0] - 2026-10-18

The first release, published as `rust4ts`: a working engine, not a scaffold.

### Added

* Hand-written JavaScript / TypeScript tokenizer with regex-versus-division
  disambiguation, nested template literals, BigInt and numeric separators,
  private names, Unicode identifiers and automatic-semicolon-insertion newline
  tracking.
* Recursive-descent and Pratt parser producing an AST with **TypeScript erased at
  parse time**: annotations, generics, `interface`, `type`, `declare`,
  `import type`, `as`/`satisfies`/`!`, access modifiers, decorators (with a
  warning), and function overload signatures.
* TypeScript constructs with runtime meaning are lowered rather than dropped:
  `enum` / `const enum` become real objects with reverse mappings, and
  constructor parameter properties inject `this.x = x`.
* Tree-walking interpreter: closures and lexical scoping, `var`/`let`/`const`
  with hoisting, destructuring, spread/rest, template and tagged templates,
  optional chaining with correct short-circuiting, `try`/`catch`/`finally`,
  labelled `break`/`continue`, classes with `extends`/`super`/`static`/
  accessors/fields/static blocks, `instanceof`, `in`.
* ES module loader with dependency-first evaluation, extension resolution
  (`.ts`, `.js`, `.mts`, `.mjs`, `.cts`, `.cjs`, `index.*`), `import.meta`, and
  import cycles that terminate. Bare specifiers were rejected with an explanatory
  error rather than silently mis-resolved — a boundary `0.2.0` removed.
* Built-ins: `Object`, `Array`, `String`, `Number`, `Boolean`, `Math`, `JSON`,
  `Date`, `RegExp`, `Map`, `Set`, `Promise`, the `Error` family, `console`,
  `parseInt`/`parseFloat`, timers on a virtual clock, and microtask ordering.
* `Runtime` embedding API (`eval`, `run_module`, `register_fn`, `set_global`,
  output capture) and a command-line runner.

### Known limitations

See the *Limits* section of the README: no npm/`node_modules`, no CommonJS, no
generators, no dynamic `import()`, no garbage collection of reference cycles,
`regex`-crate regular expressions only, and documented async/field-ordering
deviations.

[0.2.0]: https://github.com/neko233-com/codets/releases/tag/v0.2.0
[0.1.0]: https://github.com/neko233-com/codets/releases/tag/v0.1.0
