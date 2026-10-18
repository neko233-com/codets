//! Abstract syntax tree for JavaScript with TypeScript syntax already erased.
//!
//! Nothing in this tree describes a type: type annotations, `interface`, `type`
//! aliases, generics, `as` casts, `!` assertions and access modifiers are
//! consumed by the parser and never reach these structures. The two exceptions
//! are `enum`, which has runtime semantics and is lowered to real statements,
//! and `import`/`export` declarations that only carry types, which are dropped
//! entirely.

/// A list of statements, used for every statement body.
pub type Stmts = Vec<Stmt>;

/// How a variable was declared. `var` is function-scoped, the others are
/// block-scoped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

/// Key of an object literal member, class member, or destructuring property.
#[derive(Clone, Debug)]
pub enum PropKey {
    /// A plain identifier key: `{ a: 1 }`.
    Ident(String),
    /// A string key: `{ "a-b": 1 }`.
    Str(String),
    /// A numeric key: `{ 1: "a" }`.
    Num(f64),
    /// A computed key: `{ [expr]: 1 }`.
    Computed(Box<Expr>),
}

/// How a value is bound, in `let`/`const` declarations, parameters and
/// destructuring.
#[derive(Clone, Debug)]
pub enum Pattern {
    /// A single binding: `x`.
    Ident(String),
    /// Array destructuring: `[a, , b]`, with a trailing rest element.
    Array {
        elements: Vec<Option<Pattern>>,
        rest: Option<Box<Pattern>>,
    },
    /// Object destructuring: `{ a, b: c, ...rest }`.
    Object {
        props: Vec<ObjPatProp>,
        rest: Option<Box<Pattern>>,
    },
    /// A default value: `x = 1`.
    Default(Box<Pattern>, Box<Expr>),
}

/// One `key: pattern` entry of an object destructuring pattern.
#[derive(Clone, Debug)]
pub struct ObjPatProp {
    pub key: PropKey,
    pub value: Pattern,
}

/// Function parameters, shared by declarations, expressions and arrows.
#[derive(Clone, Debug)]
pub struct Param {
    pub pattern: Pattern,
    /// True for a trailing rest parameter (`...args`).
    pub rest: bool,
}

/// Body of a function: a block, or a single expression for concise arrow bodies.
#[derive(Clone, Debug)]
pub enum FuncBody {
    Block(Stmts),
    Expr(Box<Expr>),
}

/// A function of any flavour: declaration, expression, method or arrow.
#[derive(Clone, Debug)]
pub struct Func {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub body: FuncBody,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_arrow: bool,
    /// For arrows: true when the arrow has exactly one bare parameter (`x => x`)
    /// or is written as a concise body — used only for source fidelity.
    pub is_expression_body: bool,
}

/// Kind of a class member that is a function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MethodKind {
    Method,
    Get,
    Set,
    Constructor,
}

/// A member of a class body.
#[derive(Clone, Debug)]
pub enum ClassMember {
    Method {
        key: PropKey,
        func: Box<Func>,
        is_static: bool,
        kind: MethodKind,
    },
    /// A field declaration: `count = 0`.
    Field {
        key: PropKey,
        value: Option<Expr>,
        is_static: bool,
    },
    /// A `static { ... }` initialisation block.
    StaticBlock(Stmts),
}

/// A class expression or declaration.
#[derive(Clone, Debug)]
pub struct Class {
    pub name: Option<String>,
    pub superclass: Option<Box<Expr>>,
    pub members: Vec<ClassMember>,
}

/// One entry of an object literal.
#[derive(Clone, Debug)]
pub enum Prop {
    /// `key: value`, or the shorthand `{ key }`.
    KeyValue {
        key: PropKey,
        value: Expr,
        shorthand: bool,
    },
    /// A method: `{ m() {} }`.
    Method { key: PropKey, func: Box<Func> },
    /// `{ get x() {} }` / `{ set x(v) {} }`.
    Accessor {
        key: PropKey,
        func: Box<Func>,
        is_getter: bool,
    },
    /// `{ ...other }`.
    Spread(Expr),
}

/// Unary operators.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Minus,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

/// Binary operators with no short-circuit behaviour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    Instanceof,
}

/// Short-circuiting logical operators.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogOp {
    And,
    Or,
    Nullish,
}

/// Assignment operators; `Assign` is plain `=`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    And,
    Or,
    Nullish,
}

/// `++` / `--`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpdateOp {
    Inc,
    Dec,
}

/// A property access target.
#[derive(Clone, Debug)]
pub enum MemberProp {
    /// `a.b`
    Name(String),
    /// `a["b"]` or `a[i]`
    Index(Box<Expr>),
}

/// An expression.
#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    BigInt(String),
    Str(String),
    Bool(bool),
    Null,
    /// A template literal, already split into literal parts and substitutions.
    Template {
        quasis: Vec<String>,
        exprs: Vec<Expr>,
    },
    /// A tagged template: `` tag`a${b}` ``.
    TaggedTemplate {
        tag: Box<Expr>,
        quasis: Vec<String>,
        exprs: Vec<Expr>,
    },
    Ident(String),
    This,
    /// `super.x` or `super(...)`; the interpreter resolves it from the
    /// enclosing method's home object.
    Super,
    Array(Vec<Option<Expr>>),
    Object(Vec<Prop>),
    Func(Box<Func>),
    Class(Box<Class>),
    Unary {
        op: UnOp,
        arg: Box<Expr>,
    },
    Update {
        op: UpdateOp,
        prefix: bool,
        arg: Box<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Logical {
        op: LogOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Assign {
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Cond {
        test: Box<Expr>,
        cons: Box<Expr>,
        alt: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        /// `f?.()` — short-circuits to `undefined` when the callee is nullish.
        optional: bool,
    },
    New {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `import("x")`
    ImportCall(Box<Expr>),
    /// `import.meta`
    ImportMeta,
    Member {
        object: Box<Expr>,
        prop: MemberProp,
        /// `a?.b` — short-circuits to `undefined` when the object is nullish.
        optional: bool,
    },
    /// Parenthesised sequence: `(a, b, c)`.
    Sequence(Vec<Expr>),
    /// `...expr` inside a call or array literal.
    Spread(Box<Expr>),
    /// `await expr`
    Await(Box<Expr>),
    /// `yield expr` / `yield* expr`
    Yield {
        arg: Option<Box<Expr>>,
        delegate: bool,
    },
    Regex {
        source: String,
        flags: String,
    },
    /// A parenthesised expression. Kept in the tree because parentheses change
    /// semantics for optional chaining: `(a?.b).c` must *not* short-circuit.
    Paren(Box<Expr>),
    /// The hole in an array literal / elision is represented by `Option::None`
    /// inside [`Expr::Array`], so no dedicated variant is needed.
    Hole,
}

/// Initialiser of a `for` loop header.
#[derive(Clone, Debug)]
pub enum ForInit {
    Var {
        kind: VarKind,
        decls: Vec<(Pattern, Option<Expr>)>,
    },
    Expr(Expr),
}

/// Declaration target of `for-in` / `for-of`.
#[derive(Clone, Debug)]
pub enum ForTarget {
    Var {
        kind: VarKind,
        pattern: Pattern,
    },
    Pattern(Pattern),
    Expr(Expr),
}

/// One `case` of a `switch`.
#[derive(Clone, Debug)]
pub struct SwitchCase {
    /// `None` for `default`.
    pub test: Option<Expr>,
    pub body: Stmts,
}

/// A named import clause entry.
#[derive(Clone, Debug)]
pub enum ImportSpecifier {
    /// `import def from "m"`
    Default(String),
    /// `import * as ns from "m"`
    Namespace(String),
    /// `import { a as b } from "m"`
    Named { imported: String, local: String },
}

/// `import ... from "source"`.
#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub specifiers: Vec<ImportSpecifier>,
    pub source: String,
}

/// A named export entry.
#[derive(Clone, Debug)]
pub struct ExportSpecifier {
    pub local: String,
    pub exported: String,
}

/// What an `export` statement exports.
#[derive(Clone, Debug)]
pub enum ExportDecl {
    /// `export { a, b as c }` with an optional `from "m"`.
    Named {
        specifiers: Vec<ExportSpecifier>,
        source: Option<String>,
    },
    /// `export default <expr | function | class>`
    Default(Box<Stmt>),
    /// `export const x = 1`, `export function f() {}`, `export class C {}`
    Decl(Box<Stmt>),
    /// `export * from "m"`, or `export * as ns from "m"` when `alias` is set.
    All {
        source: String,
        alias: Option<String>,
    },
}

/// A statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    /// `var` / `let` / `const`, possibly several declarators.
    Var {
        kind: VarKind,
        decls: Vec<(Pattern, Option<Expr>)>,
    },
    Func(Box<Func>),
    Class(Box<Class>),
    Return(Option<Expr>),
    If {
        test: Expr,
        cons: Box<Stmt>,
        alt: Option<Box<Stmt>>,
    },
    Block(Stmts),
    For {
        init: Option<ForInit>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    ForIn {
        target: ForTarget,
        right: Expr,
        body: Box<Stmt>,
    },
    ForOf {
        target: ForTarget,
        right: Expr,
        body: Box<Stmt>,
        is_await: bool,
    },
    While {
        test: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        test: Expr,
    },
    Switch {
        disc: Expr,
        cases: Vec<SwitchCase>,
    },
    Try {
        block: Stmts,
        /// `catch (e) { }`; the binding is `None` for `catch { }`.
        handler: Option<(Option<Pattern>, Stmts)>,
        finalizer: Option<Stmts>,
    },
    Throw(Expr),
    Break(Option<String>),
    Continue(Option<String>),
    /// `label: stmt`
    Labeled {
        label: String,
        body: Box<Stmt>,
    },
    Import(ImportDecl),
    Export(ExportDecl),
    Empty,
    Debugger,
    /// A TypeScript `enum`. Unlike the other TypeScript declarations this has
    /// runtime semantics, so it is kept in the tree rather than erased.
    Enum {
        name: String,
        /// Member name and optional explicit initialiser.
        members: Vec<(String, Option<Expr>)>,
    },
}
