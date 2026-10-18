# Architecture

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

The engine half of that picture is unchanged since 0.1.0. What 0.2.0 added is
everything above it: the command line, the Node surface area, the two module
systems, the core modules and the package manager.

## Design decisions

**One tokenizer for two languages.** TypeScript adds no tokens; every TS-only
construct is made of ordinary punctuators and identifiers. Keeping the lexer
language-agnostic means the regex-versus-division and template-brace logic exists
once.

**Two ambiguities are resolved in the lexer**, because both need state that only
the tokenizer has cheaply:

* `/` starts a regular expression when the previous token cannot end an
  expression (a small table of operators, punctuators and keywords).
* `}` closes a block or an object literal, tracked with a brace stack, which
  decides whether the *following* `/` is division or a regex.

**Type erasure happens in the parser, not in a transform pass.** A type scanner
walks over type syntax with bracket-depth tracking and context-sensitive stop
sets, and simply produces no nodes. It handles the awkward cases:

* `Array<Array<number>>` — the tokenizer emits `>>` as one token, which the
  scanner splits back into two closers. `>=` is split the same way.
* `let a: string` on its own line — a newline at bracket depth zero ends an
  annotation, unless the next token continues the type (`| A`, `& B`).
* `: () => void` — a function-type arrow directly follows the `)` of its
  parameter list, while the arrow that starts a *body* does not, which is how the
  two are told apart.
* `type X = a | b` spanning lines — a type alias ends at `;` or at a keyword that
  begins a statement, so multi-line unions survive while the following statement
  does not get swallowed.
* `f<T>(x)` and `new C<T>()` — call-site type arguments are tried speculatively
  and only committed to when the list is followed by `(` or a template literal.
  That is the same resolution TypeScript itself uses, so `a < b > (c)` is a call
  in `.ts` and a comparison in `.js`.

**Only two TypeScript constructs survive into the tree.** `enum` keeps its
members so the interpreter can build the object and the numeric reverse mapping;
constructor parameter properties are turned into `this.x = x` statements at parse
time. Everything else is erased.

**Non-local exits use `Result`.** `return`, `break`, `continue` and `throw`
travel as `Control` values, so `try`/`finally` and labelled loops are ordinary
control flow instead of flag bookkeeping.

**Optional chaining is evaluated as a flattened chain.** `a?.b.c` must skip the
rest of the chain when `a` is nullish, which is not the same as returning
`undefined` for `a?.b` and then reading `.c`. The evaluator walks the links
iteratively and keeps a `Paren` node in the AST so that `(a?.b).c` still throws.

**Property lookup carries the receiver.** Getters are invoked with the object the
property was *read from*, not the prototype that stores the accessor. This is the
difference between `get name()` working and silently reading `undefined` on the
prototype — a bug the test suite caught.

**Enumeration mirrors JavaScript.** Built-in methods, class members, function
`name`/`length`/`prototype` and the constructor/prototype links are marked
non-enumerable, because `for (const k in instance)` must not walk into
`Object.prototype`.

**`await` drives the event loop.** Rather than compiling functions into a
resumable state machine, `await` drains microtasks and then timers until the
promise settles, and reports a deadlock when nothing is left to run. This makes
`await delay(10)` work with a synchronous evaluator. The cost is that an async
body starts eagerly rather than on a later microtask; both the deviation and the
reason are documented in `interp.rs` and the README.

**Module evaluation follows the observable ES algorithm.** Dependencies load
first, the exports namespace object exists before the body runs (so cycles
terminate), and `export` statements are resolved after the body so that bindings
declared later in the module are still exported.

## Testing strategy

* `src/lexer.rs`, `src/parser.rs` and each new module carry unit tests next to the
  code: semver ordering and ranges, base64 digests, packument parsing, version
  store resolution, `package.json` editing, and `parallel_map`'s order and
  thread use.
* The parser suite's key assertion is **AST equality**: each TypeScript snippet is
  compared against the equivalent JavaScript snippet, so "the types were erased"
  is verified structurally instead of assumed.
* `tests/runtime.rs` runs real TypeScript source end to end and checks console
  output, including a multi-file plugin fixture, an import cycle, and a rejected
  bare specifier.
* `tests/cli.rs` executes the three real binaries, because the compatibility claim
  is about the commands on `PATH`: version output, feature gating per Node level,
  `-e`/`-p`/`--check`, exit codes, `process` events at exit, and the version
  manager installing, switching and writing shims.
* `tests/commonjs.rs` writes small projects into scratch directories and runs
  them, so `node_modules` walking, `exports` maps, cycles, JSON modules, the core
  modules and ES-to-CommonJS interop are exercised as a program would exercise
  them.
* `tests/npm_install.rs` generates real gzipped tarballs, so the installer's
  integrity check, unpacking, bin links and lockfile are tested against the real
  formats rather than a stub. It also asserts that one worker and eight workers
  produce the same packages, which is what makes the benchmark's timing comparison
  meaningful.
* `scripts/bench-npm-install.ps1` is the performance evidence: it builds a local
  registry, installs it serial and parallel, checks the trees match, and reports
  the speed-up.

## The Node layer

**One binary, three personalities.** `argv[0]` decides whether the process is
`codets`, `node` or `npm`, and `--codets-as=<name>` overrides it. The override
exists because the version manager's shims are copies of the same executable: the
operating system reports whatever filename it was invoked through, which is how a
`node.cmd` shim ends up asking for Node's command line.

**Compatibility is a value, not a build flag.** `node::NodeCompat` is a Node
release, and it is the single input to two decisions: what `process.version`
reports, and which standard-library features `node::features` installs. A feature
is in the engine only when the selected release has it, so `--node-version=18`
genuinely lacks `Object.groupBy` — it is not hidden behind a check that any script
could see through. `Feature` lists only features that are really implemented;
advertising one that is not would make `codets features` a lie.

**`process` reads live state.** `argv`, `argv0` and `execArgv` are accessors rather
than snapshots, because the command line sets them after the process object is
built. `process.exit` is a distinct `Control` variant rather than a thrown value,
so it travels through `try`/`catch` untouched — the same guarantee Node gives, and
one that a thrown sentinel would not provide.

**Two module systems, one resolution rule.** `commonjs.rs` owns the `node_modules`
walk and `package.json` `exports` handling, and the ES module loader calls into it
for bare specifiers instead of growing a second implementation. What differs is
only what happens to the file once it is found: an `.mjs`/`.mts` file is evaluated
as a module, and anything else is evaluated as CommonJS and then presented as a
namespace (`module.exports` as `default`, its own enumerable properties as named
exports). Core modules are handed over directly, with themselves as `default`, so
both `import fs from 'node:fs'` and `import { readFileSync } from 'node:fs'` bind.

**The CommonJS cache is per realm and keyed by canonical path**, and the entry is
written *before* the module body runs. That ordering is what makes a `require`
cycle return the partially built `exports` object instead of recursing until the
stack runs out.

**Core modules are built on demand.** They are ordinary objects with native
functions on them, which is why `class Ticker extends EventEmitter {}` works: the
`EventEmitter` constructor is a native function carrying a `prototype` object, and
the class machinery looks no further than that.

## The package manager

**Resolution is flat and says so.** Each package name is resolved once, against
every range requested for it, and the highest published version satisfying all of
them wins. When no version can satisfy them all, the installer takes the best for
the first request and records a warning naming the nested copy that would have been
needed. The alternative — a nested tree — is a much larger piece of work, and
pretending to do it silently would be worse than reporting the boundary.

**Parallelism lives here, and only here.** JavaScript execution is single-threaded
by specification, so the only place concurrency can pay is the installer.
`install::parallel_map` runs a shared work queue on `--jobs` threads and preserves
input order, so a slow package cannot idle the other workers and the result does
not depend on scheduling. Each step of a package's installation — fetch, verify,
unpack — happens on whichever worker picks it up, which means the phases overlap
across packages even though they are sequential within one.

**Nothing is written before it is verified.** A `sha512` digest from the registry
is checked against the bytes received, and untar rejects any entry whose path would
escape the destination directory. A mismatch is a warning against that one package,
not a failed install: one bad package in a tree should not stop the other
nineteen.

**Registries are an interface with two implementations.** The HTTP one speaks the
npm protocol; the directory one reads the same document shape off disk. The second
is not a test double — it is how the tests run offline and how an internal mirror
would be used — which is why the installer has no test-only code path.

## The version manager

**A toolchain directory is a Node compatibility level.** `codets install 22`
materialises `versions/22.12.0/` with a manifest and its own shims, and `use` points
the store's `current` file at it. The shims pass `--node-version` and
`--codets-as`, so one executable serves all three command names at whichever level
is active. None of this needs the network; only replacing the *binary* does, which
is what `codets upgrade --from` is for.

**Resolution is a chain with a defined order** — environment variable, project
pin, current pointer, configured default, built-in default — because a version
manager that cannot explain *why* it chose a version is worse than useless. Each
step records a `VersionSource`, and `codets current` prints it.


## Where a different VM would plug in

`Runtime` is a thin facade over `Interp`. The public surface — `eval`,
`run_module`, `register_fn`, `set_global`, `capture_output`, plus the `Value`
object model — is what a host depends on. A bytecode VM or a bound C engine could
implement the same surface and swap in behind `Interp` without touching host
code.
