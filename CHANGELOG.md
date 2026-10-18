# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-18

The first release: a working engine, not a scaffold.

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
  import cycles that terminate. Bare specifiers are rejected with an explanatory
  error rather than silently mis-resolved.
* Built-ins: `Object`, `Array`, `String`, `Number`, `Boolean`, `Math`, `JSON`,
  `Date`, `RegExp`, `Map`, `Set`, `Promise`, the `Error` family, `console`,
  `parseInt`/`parseFloat`, timers on a virtual clock, and microtask ordering.
* `Runtime` embedding API (`eval`, `run_module`, `register_fn`, `set_global`,
  output capture) and a `rust4ts` command-line runner.
* 73 automated tests: 45 unit tests across the lexer and parser, 27 end-to-end
  runtime tests including a real multi-file TypeScript plugin, and the crate
  documentation example.

### Known limitations

See the *Limits* section of the README: no npm/`node_modules`, no CommonJS, no
generators, no dynamic `import()`, no garbage collection of reference cycles,
`regex`-crate regular expressions only, and documented async/field-ordering
deviations.

[0.1.0]: https://github.com/neko233-com/rust4ts/releases/tag/v0.1.0
