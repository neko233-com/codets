//! End-to-end tests: every case runs real TypeScript source and checks the
//! console output, so a regression in any layer fails here.

use std::path::PathBuf;

use codets::value::Value;
use codets::{Error, Language, Runtime};

/// Run a TypeScript snippet and return what it printed.
fn run(source: &str) -> String {
    let mut runtime = Runtime::new();
    runtime
        .run_source(source, Language::Ts)
        .unwrap_or_else(|error| panic!("script failed: {error}\n  source: {source}"))
}

/// Run a snippet that is expected to fail, returning the rendered error.
fn run_error(source: &str) -> String {
    let mut runtime = Runtime::new();
    match runtime.run_source(source, Language::Ts) {
        Ok(output) => panic!("expected an error, but it printed: {output}"),
        Err(error) => error.to_string(),
    }
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

// ------------------------------------------------------------------ basics

#[test]
fn numbers_and_operators() {
    assert_eq!(
        run("console.log(1 + 2 * 3, 7 % 3, 2 ** 10, 7 / 2);"),
        "7 1 1024 3.5\n"
    );
    assert_eq!(run("console.log(1 < 2, 2 <= 2, 3 > 4, 1 === 1, 1 !== 1);"), "true true false true false\n");
    assert_eq!(run("console.log(10 & 6, 10 | 5, 1 << 4, 255 >> 4);"), "2 15 16 15\n");
    // `-0.1 + 0.2` really does equal `0.1` in IEEE-754; `0.1 + 0.2` does not.
    assert_eq!(
        run("console.log(-0.1 + 0.2 === 0.1, 0.1 + 0.2 === 0.3, typeof (1 / 0));"),
        "true false number\n"
    );
}

#[test]
fn strings_and_templates() {
    assert_eq!(
        run("console.log('a' + 'b', 'abc'.toUpperCase(), 'a,b,c'.split(',').length);"),
        "ab ABC 3\n"
    );
    assert_eq!(
        run("const n = 2; console.log(`n=${n} sq=${n * n}`);"),
        "n=2 sq=4\n"
    );
    assert_eq!(run("console.log('  pad  '.trim(), 'x'.repeat(3), 'héllo'.length);"), "pad xxx 5\n");
    assert_eq!(run("console.log('abcdef'.slice(1, 3), 'abc'.includes('b'), 'a-b'.replace('-', '+'));"), "bc true a+b\n");
}

#[test]
fn booleans_null_and_undefined() {
    assert_eq!(
        run("console.log(true && false, true || false, null ?? 'x', undefined ?? 'y');"),
        "false true x y\n"
    );
    assert_eq!(run("console.log(!0, !!'a', typeof null, typeof undefined);"), "true true object undefined\n");
}

#[test]
fn variables_and_closures() {
    assert_eq!(
        run(
            "function counter(): () => number {\n\
             let n: number = 0;\n\
             return () => ++n;\n\
             }\n\
             const next = counter();\n\
             console.log(next(), next(), next());"
        ),
        "1 2 3\n"
    );
    assert_eq!(
        run("let a = 1; { let a = 2; console.log(a); } console.log(a);"),
        "2\n1\n"
    );
    assert_eq!(
        run("var x = 1; for (var i = 0; i < 2; i++) { x += 1; } console.log(x, i);"),
        "3 2\n"
    );
}

// ------------------------------------------------------------ data structures

#[test]
fn destructuring_and_spread() {
    assert_eq!(
        run(
            "const [a, , b] = [1, 2, 3];\n\
             const { x, y: z = 5 } = { x: 10 };\n\
             console.log(a, b, x, z);"
        ),
        "1 3 10 5\n"
    );
    assert_eq!(
        run(
            "function sum(...ns: number[]): number { return ns.reduce((acc, n) => acc + n, 0); }\n\
             const parts = [1, 2];\n\
             console.log(sum(...parts, 3));"
        ),
        "6\n"
    );
    assert_eq!(
        run(
            "let a = 1, b = 2;\n\
             [a, b] = [b, a];\n\
             const merged = { ...{ p: 1 }, q: 2 };\n\
             console.log(a, b, Object.keys(merged).join(','));"
        ),
        "2 1 p,q\n"
    );
}

#[test]
fn arrays() {
    assert_eq!(
        run("const xs = [3, 1, 2]; console.log(xs.map((x) => x * 2).join('-'), xs.filter((x) => x > 1).length, xs.sort().join(','));"),
        "6-2-4 2 1,2,3\n"
    );
    assert_eq!(
        run("const a = [1]; a.push(2, 3); console.log(a.length, a.pop(), a.shift(), a.join('|'));"),
        "3 3 1 2\n"
    );
    assert_eq!(
        run("console.log([1, [2, [3]]].flat(2).join(','), [1, 2, 3].find((x) => x > 1), [1, 2].every((x) => x > 0));"),
        "1,2,3 2 true\n"
    );
    assert_eq!(
        run("const s = [1, 2, 3, 4]; console.log(s.slice(1, 3).join(','), s.indexOf(3), s.includes(9));"),
        "2,3 2 false\n"
    );
    // An insertion sort with a comparator, which forces a JavaScript callback.
    assert_eq!(
        run("console.log([3, 1, 2].sort((a, b) => a - b).join(','), [1, 2, 3].reverse().join(','));"),
        "1,2,3 3,2,1\n"
    );
}

#[test]
fn objects() {
    assert_eq!(
        run(
            "const o = { a: 1, m() { return this.a + 1; } };\n\
             o.b = 2;\n\
             console.log(Object.keys(o).join(','), o.m(), 'a' in o);"
        ),
        "a,m,b 2 true\n"
    );
    assert_eq!(
        run("console.log(Object.values({ a: 1, b: 2 }).join(','), Object.entries({ a: 1 }).length, Object.assign({}, { z: 9 }).z);"),
        "1,2 1 9\n"
    );
    assert_eq!(
        run("const o: any = { x: 1, y: 2 }; delete o.x; console.log(Object.keys(o).join(','), o.x);"),
        "y undefined\n"
    );
    assert_eq!(
        run("const base = { a: 1 }; const child = Object.create(base); console.log(child.a, base.isPrototypeOf(child));"),
        "1 true\n"
    );
}

// ---------------------------------------------------------------- functions

#[test]
fn function_forms() {
    assert_eq!(
        run("function add(a: number, b: number = 10): number { return a + b; } console.log(add(1), add(1, 2));"),
        "11 3\n"
    );
    assert_eq!(
        run("const mul = (a: number, b: number): number => a * b; console.log(mul(3, 4));"),
        "12\n"
    );
    assert_eq!(
        run(
            "function greet({ name }: { name: string }): string { return `hi ${name}`; }\n\
             console.log(greet({ name: 'ada' }));"
        ),
        "hi ada\n"
    );
    assert_eq!(
        run(
            "const obj = { value: 7, read() { const inner = () => this.value; return inner(); } };\n\
             console.log(obj.read());"
        ),
        "7\n"
    );
    // Function declarations are hoisted.
    assert_eq!(run("console.log(later()); function later(): number { return 5; }"), "5\n");
    assert_eq!(
        run("console.log([1, 2].map(function (x) { return x + 1; }).join(','));"),
        "2,3\n"
    );
}

// ------------------------------------------------------------------ classes

#[test]
fn classes_and_inheritance() {
    assert_eq!(
        run(
            "class Animal {\n\
             name: string;\n\
             constructor(name: string) { this.name = name; }\n\
             speak(): string { return `${this.name} makes a sound`; }\n\
             }\n\
             class Dog extends Animal {\n\
             speak(): string { return `${this.name} barks`; }\n\
             }\n\
             console.log(new Dog('Rex').speak(), new Animal('Cat').speak());"
        ),
        "Rex barks Cat makes a sound\n"
    );
    assert_eq!(
        run(
            "class Config {\n\
             static readonly VERSION = '1.0';\n\
             private items: string[] = [];\n\
             get count(): number { return this.items.length; }\n\
             add(item: string) { this.items.push(item); }\n\
             }\n\
             const c = new Config();\n\
             c.add('a'); c.add('b');\n\
             console.log(Config.VERSION, c.count);"
        ),
        "1.0 2\n"
    );
    assert_eq!(
        run(
            "class Base { constructor(public tag: string) {} describe(): string { return `base:${this.tag}`; } }\n\
             class Child extends Base {\n\
             constructor() { super('kid'); }\n\
             describe(): string { return `child of ${super.describe()}`; }\n\
             }\n\
             console.log(new Child().describe());"
        ),
        "child of base:kid\n"
    );
    assert_eq!(
        run("class A {} class B extends A {} console.log(new B() instanceof A, new B() instanceof B);"),
        "true true\n"
    );
}

// ------------------------------------------------------------ control flow

#[test]
fn loops_and_switch() {
    assert_eq!(
        run(
            "let total = 0;\n\
             for (let i = 0; i < 4; i++) total += i;\n\
             for (const ch of 'ab') total += ch.length;\n\
             const o = { p: 1, q: 2 };\n\
             for (const k in o) total += o[k];\n\
             let w = 0;\n\
             while (w < 3) w++;\n\
             do { w--; } while (w > 1);\n\
             console.log(total, w);"
        ),
        "11 1\n"
    );
    assert_eq!(
        run(
            "function classify(n: number): string {\n\
             switch (n) { case 0: return 'zero'; case 1: return 'one'; default: return 'many'; }\n\
             }\n\
             console.log(classify(0), classify(1), classify(9));"
        ),
        "zero one many\n"
    );
    assert_eq!(
        run(
            "let count = 0;\n\
             outer: for (let i = 0; i < 3; i++) {\n\
             for (let j = 0; j < 3; j++) {\n\
             if (j === 1) continue outer;\n\
             if (i === 2) break outer;\n\
             count++;\n\
             }\n\
             }\n\
             console.log(count);"
        ),
        "2\n"
    );
    assert_eq!(
        run("let s = 0; for (let i = 0; i < 10; i++) { if (i % 2 === 0) continue; if (i > 6) break; s += i; } console.log(s);"),
        "9\n"
    );
}

#[test]
fn exceptions() {
    assert_eq!(
        run(
            "try {\n\
             throw new Error('boom');\n\
             } catch (e) {\n\
             console.log('caught', (e as Error).message);\n\
             } finally {\n\
             console.log('done');\n\
             }"
        ),
        "caught boom\ndone\n"
    );
    assert_eq!(
        run(
            "function risky(): number { try { throw 1; } finally { console.log('cleanup'); } }\n\
             try { risky(); } catch (n) { console.log('rethrown', n); }"
        ),
        "cleanup\nrethrown 1\n"
    );
    assert_eq!(
        run("try { null as any; } catch { console.log('never'); } console.log('after');"),
        "after\n"
    );
    // A `return` inside `try` still runs `finally`.
    assert_eq!(
        run("function f(): number { try { return 1; } finally { console.log('finally'); } } console.log(f());"),
        "finally\n1\n"
    );
}

// ------------------------------------------------------------------ values

#[test]
fn typeof_instanceof_and_membership() {
    assert_eq!(
        run("class A {} console.log(typeof 1, typeof 's', typeof undefined, typeof (() => 1), new A() instanceof A);"),
        "number string undefined function true\n"
    );
    assert_eq!(
        run("const o: any = { a: { b: 1 } }; console.log(o?.a?.b, o?.c?.d, o.missing ?? 'fallback');"),
        "1 undefined fallback\n"
    );
    assert_eq!(
        run("const f: any = null; console.log(f?.());"),
        "undefined\n"
    );
    // Parentheses break an optional chain, as in JavaScript.
    assert_eq!(run("const a: any = { b: 1 }; console.log((a).b, a['b']);"), "1 1\n");
}

// ------------------------------------------------------------- TypeScript

#[test]
fn types_are_erased_at_runtime() {
    assert_eq!(
        run(
            "interface Point { x: number; y: number }\n\
             type Pair = [number, number];\n\
             const p: Point = { x: 1, y: 2 };\n\
             const pair: Pair = [p.x, p.y];\n\
             console.log(pair[0] + pair[1]);"
        ),
        "3\n"
    );
    assert_eq!(
        run(
            "function identity<T>(value: T): T { return value; }\n\
             const n = identity<number>(5);\n\
             console.log(n);"
        ),
        "5\n"
    );
    assert_eq!(
        run("const raw = 'text' as unknown as string; const len = raw!.length; console.log(len);"),
        "4\n"
    );
}

#[test]
fn enums_have_runtime_values() {
    assert_eq!(
        run(
            "enum Color { Red, Green = 5, Blue }\n\
             console.log(Color.Red, Color.Green, Color.Blue, Color[5], Color[0]);"
        ),
        "0 5 6 Green Red\n"
    );
    assert_eq!(
        run("const enum Flags { A = 1, B = 2 } console.log(Flags.A + Flags.B);"),
        "3\n"
    );
    assert_eq!(
        run("enum Label { X = 'x', Y = 'y' } console.log(Label.X, Label.Y);"),
        "x y\n"
    );
}

#[test]
fn constructor_parameter_properties_are_assigned() {
    assert_eq!(
        run(
            "class Person {\n\
             constructor(public name: string, private secret: string = 'sh') {}\n\
             reveal(): string { return this.secret; }\n\
             }\n\
             const p = new Person('ada');\n\
             console.log(p.name, p.reveal());"
        ),
        "ada sh\n"
    );
}

// ---------------------------------------------------------------- builtins

#[test]
fn json_round_trip() {
    assert_eq!(
        run("const data = JSON.parse('{\"a\":[1,2],\"b\":\"x\"}'); console.log(data.a.length, data.b, data.a[1]);"),
        "2 x 2\n"
    );
    assert_eq!(
        run("console.log(JSON.stringify({ n: 1, s: 't' }), JSON.stringify([1, 'a', null]));"),
        "{\"n\":1,\"s\":\"t\"} [1,\"a\",null]\n"
    );
    assert_eq!(
        run("console.log(JSON.stringify({ a: undefined, b: 1 }));"),
        "{\"b\":1}\n"
    );
}

#[test]
fn math_and_number_formatting() {
    assert_eq!(
        run("console.log(Math.max(1, 5, 3), Math.floor(2.7), Math.round(2.5), Math.abs(-4));"),
        "5 2 3 4\n"
    );
    assert_eq!(
        run("console.log((1.5).toFixed(2), Number.isInteger(3), Number.isInteger(3.5), parseInt('42px'), parseFloat('3.5rem'));"),
        "1.50 true false 42 3.5\n"
    );
    assert_eq!(run("console.log(Number('42') + 1, String(42) + '1');"), "43 421\n");
}

#[test]
fn regexp_support() {
    assert_eq!(run("console.log(/ab+/.test('abbb'), /z/.test('abc'));"), "true false\n")
    ;
    assert_eq!(
        run("const m = 'a1b22'.match(/[0-9]+/g); console.log(m.join(','));"),
        "1,22\n"
    );
    assert_eq!(run("console.log('a1b'.replace(/[0-9]/, 'X'));"), "aXb\n");
    assert_eq!(run("console.log(/ab+/i.source, /ab+/g.global);"), "ab+ true\n");
}

#[test]
fn map_and_set() {
    assert_eq!(
        run(
            "const m = new Map<string, number>();\n\
             m.set('a', 1); m.set('b', 2); m.set('a', 3);\n\
             const s = new Set<number>([1, 2, 2, 3]);\n\
             console.log(m.get('a'), m.size, m.has('z'), s.size, s.has(2));"
        ),
        "3 2 false 3 true\n"
    );
    assert_eq!(
        run("const s = new Set<string>(); s.add('x'); s.delete('x'); console.log(s.size);"),
        "0\n"
    );
}

#[test]
fn promise_and_async_ordering() {
    assert_eq!(
        run(
            "async function work(): Promise<number> { return 21 * 2; }\n\
             Promise.resolve(5).then((v) => console.log('then', v));\n\
             work().then((v) => console.log('async', v));\n\
             console.log('sync');"
        ),
        "sync\nthen 5\nasync 42\n"
    );
    assert_eq!(
        run(
            "function delay(ms: number): Promise<void> { return new Promise((resolve) => setTimeout(resolve, ms)); }\n\
             async function main(): Promise<void> { await delay(5); console.log('awake'); }\n\
             main();"
        ),
        "awake\n"
    );
    assert_eq!(
        run("Promise.reject(new Error('no')).catch((e) => console.log('caught', (e as Error).message));"),
        "caught no\n"
    );
    assert_eq!(
        run("Promise.all([Promise.resolve(1), Promise.resolve(2)]).then((vs) => console.log(vs.join(',')));"),
        "1,2\n"
    );
}

// ------------------------------------------------------------------ modules

#[test]
fn module_output_matches_expectations() {
    let mut runtime = Runtime::with_root(fixtures().join("modules"));
    let buffer = runtime.capture_output();
    let entry = fixtures().join("modules/main.ts");
    runtime
        .run_module(&entry)
        .unwrap_or_else(|error| panic!("module failed: {error}"));
    let output = buffer.borrow().clone();
    assert_eq!(
        output,
        "3.14159\n42\nmain\n[ 3, 6 ]\nkind=10 name=Square\n0 Square\n7.14159\n"
    );
}

#[test]
fn circular_imports_terminate() {
    // The fixture pair imports each other; the namespace exists before the body
    // runs, so this must not recurse forever.
    let mut runtime = Runtime::with_root(fixtures().join("modules"));
    let buffer = runtime.capture_output();
    let entry = fixtures().join("modules/cycle-a.ts");
    runtime
        .run_module(&entry)
        .unwrap_or_else(|error| panic!("cyclic module failed: {error}"));
    let output = buffer.borrow().clone();
    assert!(output.contains("cycle ok"), "unexpected output: {output}");
}

#[test]
fn bare_specifiers_are_rejected_with_a_clear_message() {
    let mut runtime = Runtime::with_root(fixtures().join("modules"));
    let entry = fixtures().join("modules/bare.ts");
    let error = runtime.run_module(&entry).expect_err("should be rejected");
    assert!(
        error.to_string().contains("node_modules"),
        "unexpected message: {error}"
    );
}

// -------------------------------------------------------------------- host

#[test]
fn rust_functions_can_be_exposed_to_scripts() {
    let mut runtime = Runtime::new();
    let buffer = runtime.capture_output();
    runtime.register_fn("hostAdd", |_interp, _this, args| {
        Ok(Value::Num(args[0].to_number() + args[1].to_number()))
    });
    runtime
        .eval("console.log(hostAdd(2, 3) * 2);", Language::Ts)
        .expect("host function should be callable");
    assert_eq!(buffer.borrow().clone(), "10\n");
}

// ------------------------------------------------------------------ errors

#[test]
fn uncaught_errors_are_reported() {
    let message = run_error("throw new Error('nope');");
    assert!(message.contains("nope"), "unexpected message: {message}");
}

#[test]
fn type_errors_are_reported() {
    let message = run_error("const o: any = null; o.x;");
    assert!(
        message.contains("cannot read property"),
        "unexpected message: {message}"
    );
}

#[test]
fn syntax_and_name_errors_are_distinguishable() {
    let mut runtime = Runtime::new();
    match runtime.eval("let =", Language::Ts) {
        Err(Error::Parse(error)) => assert!(error.line >= 1),
        other => panic!("expected a parse error, got {other:?}"),
    }
    match runtime.eval("notDefined(1);", Language::Ts) {
        Err(Error::Uncaught(error)) => {
            assert!(error.render().contains("not defined"), "got {}", error.render())
        }
        other => panic!("expected an uncaught error, got {other:?}"),
    }
}
