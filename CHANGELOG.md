# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-10-19

Self-upgrade, a configurable registry, and the machinery to publish a release and
prove it works before doing so.

### Added

* **`codets upgrade` installs a published release.** It reads the newest tag from the
  GitHub releases API, downloads `<base>/v<version>/codets-<version>-<triple>.tar.gz`,
  checks it against the release's `codets-<version>-checksums.txt`, unpacks the binary
  and installs it as the active toolchain. `--to <version>` pins one,
  `CODETS_DOWNLOAD_BASE` and `CODETS_API_BASE` point it at a mirror, and
  `--from <path>` still installs a build you already have.
* **One-line installers**, `install.sh` and `install.ps1`: they detect the platform's
  target triple, download the published archive, verify its SHA-256, unpack it and
  hand the binary to `codets bootstrap`, which owns the store layout. A missing
  checksum file is a warning (an older release may not have one); a mismatch always
  stops the install before anything is written. `install.ps1` runs under PowerShell 7
  on Linux and macOS as well as under Windows PowerShell, and takes Windows' own
  `tar.exe` by absolute path — a Git shell puts its GNU `tar` first on `PATH`, and that
  one reads a `C:\...` argument as a remote host rather than a file.
* **The registry is configurable the way npm configures it**: `--registry`, then
  `NPM_CONFIG_REGISTRY` (or npm's lowercase `npm_config_registry`), then the project's
  `.npmrc`, then the user's `.npmrc` (or `NPM_CONFIG_USERCONFIG` / `CODETS_NPMRC`),
  then `https://registry.npmjs.org`. Mirrors and internal registries therefore need no
  flag on any command. A `.npmrc` is looked for from the working directory up to the
  project root — the nearest directory holding a `package.json` or a `node_modules` —
  and no further, so an unrelated file above the project cannot change where it
  installs from.
* **`npm config get registry`** prints the registry in use, and `--origin` prints
  which of those decided it, on stderr so that piping stdout still yields a bare URL.
* **A release pipeline** (`.github/workflows/release.yml`), triggered by `v*` tags
  only. It refuses a tag that disagrees with `Cargo.toml` or that is not an ancestor
  of the default branch, runs the test suite before spending six runners on the
  matrix, publishes `.tar.gz` archives for six platforms and one checksum file, and
  takes the release notes from this file.
* **`AGENTS.md`**: the release rule, the rules the test suite follows and the asset
  naming contract, for anyone changing this repository.
* **`scripts/pre-release.{sh,ps1}`**: the gate a release has to pass locally — clean
  tree, tests, release build, no leaked scratch directories, and a CHANGELOG section
  for the version being tagged. **`scripts/check-temp-clean.{sh,ps1}`**: the leak
  check, which can also clean up. **`scripts/verify-demo.{sh,ps1}`**: the online
  end-to-end run against the real npm registry.
* **`examples/vite8-demo`**: a Vite 8 project that verifies a real install and runs
  its own TypeScript, with the limits of that verification written down next to it.
* **Offline coverage for everything that reaches outside.** `tests/release_contract.rs`
  and `tests/install_scripts.rs` serve workflow-shaped assets from a local HTTP server
  and drive `codets upgrade` and both installers against it, so naming, checksums,
  unpacking and the shell scripts are exercised on every `cargo test`. `tests/net.rs`
  covers the transport layer's limits, `tests/idempotency.rs` runs every
  state-writing command twice and compares the result.

### Changed

* **A version directory is now a hard-linked copy of the toolchain rather than a
  directory of shell shims.** `bin/` links to the active version's binary, and the
  binary picks its command personality from `argv[0]`, so `node` and `npm` need no
  dispatch of their own. This also removes the `node.exe` versus `node.cmd`
  `PATHEXT` trap on Windows and the `chmod +x` a shell shim needs on Unix.
* **`codets install` no longer changes which version is active.** It used to rewrite
  `bin/`, which silently moved `node` to the newly installed level.
* **`npm install` prunes.** A package that the manifest no longer asks for is removed
  from `node_modules`, along with its shim in `node_modules/.bin`, which is what npm
  does and what makes installing twice leave the same tree. Pruning is skipped when
  the manifest could not be resolved in full, so a typo in a registry URL cannot
  delete a working `node_modules`.
* **`package-lock.json` records a real `resolved`**: the tarball URL for an HTTP
  registry, and a `file:` path for a directory registry, rather than the installer's
  own description of itself.
* **`node_modules/.bin` shims call the toolchain that installed them**, taken from the
  caller rather than from `std::env::current_exe()`, so a shim is reproducible and can
  be tested by running it.
* **The store's `toolchain.conf` records `source=bootstrap` when `bootstrap` installed
  the toolchain**, rather than the path the binary was read from. An installer unpacks
  into a temporary directory and deletes it moments later, so that path was one no
  reader could ever follow — and it made two identical installs differ.
* `codets env` says "bin directory" and describes what `codets use` rewrites, rather
  than calling the store's links "shims".
* `codets versions` marks an installed level in its "available to install" list as
  `[installed]`, where the word used to run into the description.

### Fixed

* **Any package whose registry metadata is larger than 10 MiB could not be installed.**
  ureq's default response-body limit is 10 MiB and `codets` never raised it, so
  `npm install vite` failed with "the response body is larger than request limit".
  The limit is now 512 MiB, which is a guard against a runaway server rather than a
  number real packages reach.
* **A `.npmrc` above the project root no longer decides where a project installs
  from**, which on a machine with a `~/.npmrc` meant the project's own settings never
  applied.

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

[0.3.0]: https://github.com/neko233-com/codets/releases/tag/v0.3.0
[0.2.0]: https://github.com/neko233-com/codets/releases/tag/v0.2.0
[0.1.0]: https://github.com/neko233-com/codets/releases/tag/v0.1.0
