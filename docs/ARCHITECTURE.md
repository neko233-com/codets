# Architecture

```
source text
    │
    ▼
lexer.rs      hand-written tokenizer (shared by JS and TS)
    │  Tok::Num / Str / Template* / Regex / Ident / Punct, plus `nl_before`
    ▼
parser.rs     recursive descent + Pratt expressions
    │  erases TypeScript while building the tree
    ▼
ast.rs        Stmt / Expr / Pattern / Class / Func
    │
    ▼
interp.rs     tree-walking evaluator
    ├── value.rs     Value, Object, PropMap, Function, Env
    ├── builtins.rs  prototypes, standard library, regex, JSON, Promise
    └── modules.rs   ES module resolution, linking and evaluation
```

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

* `src/lexer.rs` and `src/parser.rs` carry unit tests next to the code.
* The parser suite's key assertion is **AST equality**: each TypeScript snippet is
  compared against the equivalent JavaScript snippet, so "the types were erased"
  is verified structurally instead of assumed.
* `tests/runtime.rs` runs real TypeScript source end to end and checks console
  output, including a multi-file plugin fixture, an import cycle, and a rejected
  bare specifier.
* The crate documentation example is a doc test.

## Where a different VM would plug in

`Runtime` is a thin facade over `Interp`. The public surface — `eval`,
`run_module`, `register_fn`, `set_global`, `capture_output`, plus the `Value`
object model — is what a host depends on. A bytecode VM or a bound C engine could
implement the same surface and swap in behind `Interp` without touching host
code.
