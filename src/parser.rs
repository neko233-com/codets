//! Recursive-descent parser producing an AST with TypeScript syntax erased.
//!
//! # How TypeScript support works
//!
//! There is no separate transpiler. The parser recognises TypeScript-only
//! syntax and simply does not emit nodes for it:
//!
//! * type annotations (`: T`), type parameters (`<T>`), `as` / `satisfies`
//!   casts and `!` non-null assertions are scanned over and dropped;
//! * `interface` and `type` declarations produce nothing;
//! * `import type` / `export type` and inline `type` specifiers produce nothing;
//! * `declare` blocks produce nothing;
//! * decorators are parsed and dropped, with a warning, because dropping them
//!   would silently change behaviour if they were meant to do something.
//!
//! Two constructs do have runtime meaning and are therefore lowered:
//!
//! * `enum` becomes an object whose members map name to value, plus the reverse
//!   mapping for numeric members;
//! * parameter properties (`constructor(private x: number)`) inject
//!   `this.x = x` at the top of the constructor.
//!
//! Deliberately unsupported: TypeScript `namespace` / `module` blocks, `.tsx`
//! JSX, and explicit type arguments at call sites such as `f<number>(x)`.
//! All three produce a clear parse error rather than silent misbehaviour.

use std::fmt;

use crate::ast::*;
use crate::lexer::{LexError, Lexer, Tok, Token};

/// Which language dialect to accept.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    /// Plain JavaScript: TypeScript-only syntax is a syntax error.
    Js,
    /// TypeScript: type syntax is accepted and erased.
    Ts,
}

impl Language {
    /// Pick a dialect from a file extension, defaulting to JavaScript.
    pub fn from_path(path: &std::path::Path) -> Language {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("ts") | Some("mts") | Some("cts") => Language::Ts,
            _ => Language::Js,
        }
    }
}

/// A syntax error with its source location.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    pub msg: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            msg: e.msg,
            line: e.line,
            col: e.col,
        }
    }
}

/// The result of parsing one script: statements plus any warnings raised while
/// erasing TypeScript.
#[derive(Clone, Debug)]
pub struct ParsedScript {
    pub stmts: Stmts,
    pub warnings: Vec<String>,
}

/// Parse a whole script.
pub fn parse_script(src: &str, language: Language) -> Result<ParsedScript, ParseError> {
    let toks = Lexer::new(src).tokenize()?;
    let mut parser = Parser {
        toks,
        pos: 0,
        ts: language == Language::Ts,
        warnings: Vec::new(),
        no_in: false,
        generator_depth: 0,
        property_parameters: Vec::new(),
    };
    let stmts = parser.parse_program()?;
    Ok(ParsedScript {
        stmts,
        warnings: parser.warnings,
    })
}

/// Keywords that may not be used as a plain binding identifier.
fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
    )
}

/// Keywords that always begin a statement.
fn starts_statement(word: &str) -> bool {
    matches!(
        word,
        "var"
            | "let"
            | "const"
            | "function"
            | "class"
            | "if"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "try"
            | "return"
            | "throw"
            | "break"
            | "continue"
            | "debugger"
            | "import"
            | "export"
            | "interface"
            | "type"
            | "enum"
            | "declare"
            | "namespace"
            | "abstract"
            | "async"
    )
}

/// Binary and logical operator precedence. Higher binds tighter.
fn binary_op(punct: &str) -> Option<(Operator, u8)> {
    let (op, prec) = match punct {
        "??" => (Operator::Log(LogOp::Nullish), 2),
        "||" => (Operator::Log(LogOp::Or), 3),
        "&&" => (Operator::Log(LogOp::And), 4),
        "|" => (Operator::Bin(BinOp::BitOr), 5),
        "^" => (Operator::Bin(BinOp::BitXor), 6),
        "&" => (Operator::Bin(BinOp::BitAnd), 7),
        "==" => (Operator::Bin(BinOp::Eq), 8),
        "!=" => (Operator::Bin(BinOp::Ne), 8),
        "===" => (Operator::Bin(BinOp::StrictEq), 8),
        "!==" => (Operator::Bin(BinOp::StrictNe), 8),
        "<" => (Operator::Bin(BinOp::Lt), 9),
        ">" => (Operator::Bin(BinOp::Gt), 9),
        "<=" => (Operator::Bin(BinOp::Le), 9),
        ">=" => (Operator::Bin(BinOp::Ge), 9),
        "in" => (Operator::Bin(BinOp::In), 9),
        "instanceof" => (Operator::Bin(BinOp::Instanceof), 9),
        "<<" => (Operator::Bin(BinOp::Shl), 10),
        ">>" => (Operator::Bin(BinOp::Shr), 10),
        ">>>" => (Operator::Bin(BinOp::UShr), 10),
        "+" => (Operator::Bin(BinOp::Add), 11),
        "-" => (Operator::Bin(BinOp::Sub), 11),
        "*" => (Operator::Bin(BinOp::Mul), 12),
        "/" => (Operator::Bin(BinOp::Div), 12),
        "%" => (Operator::Bin(BinOp::Mod), 12),
        "**" => (Operator::Bin(BinOp::Exp), 13),
        _ => return None,
    };
    Some((op, prec))
}

/// A binary operator, which is either strict-associative or short-circuiting.
#[derive(Clone, Copy, Debug)]
enum Operator {
    Bin(BinOp),
    Log(LogOp),
}

/// Which kind of type is being scanned over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TypeContext {
    /// After `:` on a binding, parameter, field or `catch` clause.
    Annotation,
    /// After the parameter list of a function: ends at the body (`{` or `=>`).
    Return,
}

/// The parser.
struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// True when parsing TypeScript, which enables type syntax.
    ts: bool,
    warnings: Vec<String>,
    /// Set while parsing a `for` header, where `in` must not be a binary operator.
    no_in: bool,
    /// Non-zero while inside a generator body, which enables `yield`.
    generator_depth: u32,
    /// Parameter properties collected for the constructor currently being parsed.
    property_parameters: Vec<String>,
}

impl Parser {
    // ---------------------------------------------------------------- helpers

    fn cur(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn cur_token(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn peek(&self, n: usize) -> &Tok {
        let idx = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[idx].tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.cur(), Tok::Eof)
    }

    fn at_punct(&self, punct: &str) -> bool {
        self.cur().is_punct(punct)
    }

    fn at_ident(&self, word: &str) -> bool {
        self.cur().is_ident(word)
    }

    fn nl_before(&self) -> bool {
        self.cur_token().nl_before
    }

    fn bump(&mut self) -> Tok {
        let tok = self.toks[self.pos].tok.clone();
        if !matches!(tok, Tok::Eof) {
            self.pos += 1;
        }
        tok
    }

    fn eat_punct(&mut self, punct: &str) -> bool {
        if self.at_punct(punct) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_ident(&mut self, word: &str) -> bool {
        if self.at_ident(word) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, punct: &str) -> Result<(), ParseError> {
        if self.eat_punct(punct) {
            Ok(())
        } else {
            self.err(format!(
                "expected `{}` but found {}",
                punct,
                self.cur().describe()
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.cur().clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(name)
            }
            other => self.err(format!("expected an identifier but found {}", other.describe())),
        }
    }

    /// Expect a specific keyword, which is not bound to anything.
    fn expect_word(&mut self, word: &str) -> Result<(), ParseError> {
        if self.eat_ident(word) {
            Ok(())
        } else {
            self.err(format!(
                "expected `{}` but found {}",
                word,
                self.cur().describe()
            ))
        }
    }

    fn expect_string(&mut self) -> Result<String, ParseError> {
        match self.cur().clone() {
            Tok::Str(s) => {
                self.bump();
                Ok(s)
            }
            other => self.err(format!(
                "expected a string literal but found {}",
                other.describe()
            )),
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        let t = self.cur_token();
        Err(ParseError {
            msg: msg.into(),
            line: t.line,
            col: t.col,
        })
    }

    fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    /// Consume a statement terminator, applying automatic semicolon insertion.
    fn consume_semicolon(&mut self) -> Result<(), ParseError> {
        if self.eat_punct(";") {
            return Ok(());
        }
        if self.at_eof() || self.at_punct("}") || self.nl_before() {
            return Ok(());
        }
        self.err(format!(
            "expected `;` but found {}",
            self.cur().describe()
        ))
    }

    // ------------------------------------------------------------- TypeScript

    /// True when the current token starts a `type X = ...` alias rather than
    /// the identifier `type`.
    fn is_type_alias_start(&self) -> bool {
        self.at_ident("type")
            && matches!(self.peek(1), Tok::Ident(_))
            && (self.peek(2).is_punct("=") || self.peek(2).is_punct("<"))
    }

    /// Consume `interface Name ... { ... }`.
    fn skip_interface(&mut self) -> Result<(), ParseError> {
        self.bump();
        // Name and any `extends` clause, up to the body.
        while !self.at_eof() && !self.at_punct("{") {
            self.bump();
        }
        self.skip_braced_block()
    }

    /// Consume `type X<T> = ...` up to the end of the statement, tolerating
    /// multi-line union types.
    fn skip_type_alias(&mut self) -> Result<(), ParseError> {
        self.bump();
        let mut depth = 0i32;
        loop {
            if self.at_eof() {
                return Ok(());
            }
            let tok = self.cur().clone();
            match &tok {
                Tok::Punct(p) => match p.as_str() {
                    "(" | "[" | "{" | "<" => {
                        depth += 1;
                        self.bump();
                    }
                    ")" | "]" | "}" | ">" => {
                        if depth == 0 && p == "}" {
                            return Ok(());
                        }
                        depth -= 1;
                        self.bump();
                    }
                    ";" if depth == 0 => {
                        self.bump();
                        return Ok(());
                    }
                    _ => {
                        self.bump();
                    }
                },
                Tok::Ident(word) => {
                    // A new statement keyword at depth zero ends the alias. This
                    // is what makes multi-line `| A | B` unions work while still
                    // stopping before the next statement.
                    if depth == 0 && self.nl_before() && starts_statement(word) {
                        return Ok(());
                    }
                    self.bump();
                }
                _ => {
                    if depth == 0 && self.nl_before() {
                        return Ok(());
                    }
                    self.bump();
                }
            }
        }
    }

    /// Consume a `declare ...` statement, which has no runtime effect.
    fn skip_declare(&mut self) -> Result<(), ParseError> {
        self.bump();
        // `declare module "x" {` and `declare global {` own a body.
        let mut depth = 0i32;
        loop {
            if self.at_eof() {
                return Ok(());
            }
            if self.at_punct("{") {
                return self.skip_braced_block();
            }
            let tok = self.cur().clone();
            match &tok {
                Tok::Punct(p) => match p.as_str() {
                    "(" | "[" | "<" => {
                        depth += 1;
                        self.bump();
                    }
                    ")" | "]" | ">" => {
                        depth -= 1;
                        self.bump();
                    }
                    ";" if depth == 0 => {
                        self.bump();
                        return Ok(());
                    }
                    _ => {
                        self.bump();
                    }
                },
                _ => {
                    if depth == 0 && self.nl_before() {
                        return Ok(());
                    }
                    self.bump();
                }
            }
        }
    }

    /// Consume a balanced `{ ... }` block, assuming the current token is `{`.
    fn skip_braced_block(&mut self) -> Result<(), ParseError> {
        self.expect_punct("{")?;
        let mut depth = 1i32;
        while depth > 0 {
            if self.at_eof() {
                return self.err("unexpected end of input inside a `{ ... }` block");
            }
            match self.cur().clone() {
                Tok::Punct(p) => {
                    match p.as_str() {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        _ => {}
                    }
                    self.bump();
                }
                Tok::TemplateHead(_) | Tok::TemplateMiddle(_) => {
                    // Keep the `}` of a substitution from closing the block.
                    depth += 1;
                    self.bump();
                }
                Tok::TemplateTail(_) | Tok::TemplateNoSub(_) => {
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
        Ok(())
    }

    /// Skip a decorator: `@name`, `@a.b`, `@a.b(args)`, `@a<T>()`.
    ///
    /// A second bare identifier is never part of the decorator, so
    /// `@log m() {}` leaves `m() {}` for the class member parser.
    fn skip_decorator(&mut self) -> Result<(), ParseError> {
        self.bump(); // '@'
        if !matches!(self.cur(), Tok::Ident(_)) {
            return Ok(());
        }
        self.bump();
        loop {
            if self.at_punct(".") || self.at_punct("?.") {
                self.bump();
                if matches!(self.cur(), Tok::Ident(_)) {
                    self.bump();
                } else {
                    return Ok(());
                }
            } else if self.at_punct("(") || self.at_punct("[") {
                let open = if self.at_punct("(") { "(" } else { "[" };
                let close = if open == "(" { ")" } else { "]" };
                let mut depth = 0i32;
                loop {
                    if self.at_eof() {
                        return Ok(());
                    }
                    let tok = self.cur().clone();
                    match &tok {
                        Tok::Punct(q) if q.as_str() == open => {
                            depth += 1;
                            self.bump();
                        }
                        Tok::Punct(q) if q.as_str() == close => {
                            depth -= 1;
                            self.bump();
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {
                            self.bump();
                        }
                    }
                }
            } else if self.ts && self.at_punct("<") {
                if self.skip_angle_group().is_err() {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
    }

    /// Consume `( ... )` type parameters or type arguments, honouring the fact
    /// that the lexer may have produced `>>`, `>>>` or `>=` as single tokens.
    ///
    /// `emit` controls whether the collected type is turned into `this.x = x`
    /// assignments; it is only used for constructor parameter properties.
    fn skip_angle_group(&mut self) -> Result<(), ParseError> {
        self.expect_punct("<")?;
        let mut depth = 1i32;
        while depth > 0 {
            if self.at_eof() {
                return self.err("unexpected end of input inside `< ... >` type arguments");
            }
            let tok = self.cur().clone();
            match &tok {
                Tok::Punct(p) => {
                    if let Some(rest) = consume_leading_angle(p, &mut depth) {
                        if rest.is_empty() {
                            self.bump();
                        } else {
                            // The token was only partly consumed (`>>` closing two
                            // levels, or `>=`); keep the remainder in place.
                            self.toks[self.pos].tok = Tok::Punct(rest);
                        }
                        if depth <= 0 {
                            return Ok(());
                        }
                    } else {
                        self.bump();
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
        Ok(())
    }

    /// Scan over a type annotation, stopping at the first token that cannot
    /// belong to the type in this context.
    ///
    /// The scanner tracks bracket depth so that nested types survive, and it
    /// splits multi-`>` tokens (`>>`, `>>>`, `>=`) because the tokenizer emits
    /// them as single tokens while `Array<Array<number>>` needs two closers.
    fn skip_type(&mut self, ctx: TypeContext) -> Result<(), ParseError> {
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut brace = 0i32;
        let mut angle = 0i32;
        let mut consumed_any = false;
        // A function type's `=>` directly follows the `)` of its parameter list,
        // so that arrow belongs to the type (`: () => void`). The arrow that
        // starts a function body follows the completed type instead.
        let mut prev_close_paren = false;
        loop {
            if self.at_eof() {
                return Ok(());
            }
            let tok = self.cur().clone();
            let at_top = depth_is_zero(paren, bracket, brace, angle);
            match &tok {
                Tok::Punct(p) => {
                    let p = p.as_str();
                    if at_top {
                        match p {
                            "," | ";" | ")" | "]" | "}" | "=" => return Ok(()),
                            "{" if ctx == TypeContext::Return => return Ok(()),
                            "=>" if ctx == TypeContext::Return && !prev_close_paren => {
                                return Ok(())
                            }
                            _ => {}
                        }
                        // A type that ran onto the next line ends there, unless
                        // the new line continues it (`| A` / `& B`).
                        if consumed_any && self.nl_before() && p != "|" && p != "&" {
                            return Ok(());
                        }
                    }
                    if p.starts_with('>') && angle > 0 {
                        if let Some(rest) = consume_leading_angle(p, &mut angle) {
                            if rest.is_empty() {
                                self.bump();
                            } else {
                                self.toks[self.pos].tok = Tok::Punct(rest);
                            }
                        } else {
                            self.bump();
                        }
                    } else {
                        match p {
                            "(" => paren += 1,
                            ")" => paren -= 1,
                            "[" => bracket += 1,
                            "]" => bracket -= 1,
                            "{" => brace += 1,
                            "}" => brace -= 1,
                            "<" => angle += 1,
                            _ => {}
                        }
                        self.bump();
                    }
                    prev_close_paren = p == ")";
                    consumed_any = true;
                }
                Tok::Ident(word) => {
                    // A statement keyword at the start of a new line ends the
                    // annotation, which is what makes ASI-terminated annotations
                    // such as `let a: string` on one line work.
                    if at_top && consumed_any && self.nl_before() && starts_statement(word) {
                        return Ok(());
                    }
                    prev_close_paren = false;
                    consumed_any = true;
                    self.bump();
                }
                _ => {
                    if at_top && consumed_any && self.nl_before() {
                        return Ok(());
                    }
                    prev_close_paren = false;
                    consumed_any = true;
                    self.bump();
                }
            }
        }
    }

    /// If the current token is `?` or `!` in a binding position, consume it.
    fn eat_optional_marker(&mut self) {
        if self.at_punct("?") || self.at_punct("!") {
            self.bump();
        }
    }

    /// Consume `enum Name { ... }`.
    ///
    /// Unlike the other TypeScript declarations an enum has runtime semantics,
    /// so it is kept in the tree: the interpreter builds the object, including
    /// the reverse mapping for numeric members.
    fn parse_enum(&mut self) -> Result<Stmt, ParseError> {
        self.eat_ident("const");
        self.bump(); // 'enum'
        let name = self.expect_ident()?;
        self.expect_punct("{")?;

        let mut members: Vec<(String, Option<Expr>)> = Vec::new();
        while !self.at_punct("}") {
            if self.at_eof() {
                return self.err("unexpected end of input inside an `enum` body");
            }
            let key = match self.cur().clone() {
                Tok::Ident(word) => {
                    self.bump();
                    word
                }
                Tok::Str(s) => {
                    self.bump();
                    s
                }
                Tok::Num { value, .. } => {
                    self.bump();
                    value.to_string()
                }
                other => {
                    return self.err(format!(
                        "expected an enum member name but found {}",
                        other.describe()
                    ))
                }
            };
            let value = if self.eat_punct("=") {
                Some(self.parse_assignment()?)
            } else {
                None
            };
            members.push((key, value));
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        Ok(Stmt::Enum { name, members })
    }

    // ------------------------------------------------------------- statements

    fn parse_program(&mut self) -> Result<Stmts, ParseError> {
        let mut out = Vec::new();
        while !self.at_eof() {
            out.push(self.parse_stmt()?);
        }
        Ok(out)
    }

    /// Parse a block body: `{ ... }`, used for function and statement bodies.
    fn parse_block(&mut self) -> Result<Stmts, ParseError> {
        self.expect_punct("{")?;
        let mut out = Vec::new();
        while !self.at_punct("}") {
            if self.at_eof() {
                return self.err("unexpected end of input, expected `}`");
            }
            out.push(self.parse_stmt()?);
        }
        self.expect_punct("}")?;
        Ok(out)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        if self.ts {
            if self.at_punct("@") {
                while self.at_punct("@") {
                    self.warn("decorator ignored: rust4ts does not run decorators");
                    self.skip_decorator()?;
                }
            }
            if self.at_ident("interface") {
                self.skip_interface()?;
                return Ok(Stmt::Empty);
            }
            if self.is_type_alias_start() {
                self.skip_type_alias()?;
                return Ok(Stmt::Empty);
            }
            if self.at_ident("enum")
                || (self.at_ident("const") && self.peek(1).is_ident("enum"))
            {
                return self.parse_enum();
            }
            if self.at_ident("declare") && self.declare_is_statement() {
                self.skip_declare()?;
                return Ok(Stmt::Empty);
            }
            if (self.at_ident("namespace") || self.at_ident("module"))
                && matches!(self.peek(1), Tok::Ident(_) | Tok::Str(_))
            {
                return self.err(
                    "TypeScript `namespace` / `module` blocks are not supported by rust4ts",
                );
            }
            if self.at_ident("abstract") && self.peek(1).is_ident("class") {
                self.bump();
                return self.parse_class(true);
            }
        }

        if self.at_punct("{") {
            return Ok(Stmt::Block(self.parse_block()?));
        }
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        if self.at_ident("debugger") {
            self.bump();
            self.consume_semicolon()?;
            return Ok(Stmt::Debugger);
        }
        if self.at_ident("var") || self.at_ident("let") || self.at_ident("const") {
            // `let` as an identifier (`let = 1`) is legal outside strict mode,
            // but treating it as a declaration is the useful interpretation.
            if self.at_ident("let") && matches!(self.peek(1), Tok::Punct(p) if p == "=") {
                let expr = self.parse_expression()?;
                self.consume_semicolon()?;
                return Ok(Stmt::Expr(expr));
            }
            let stmt = self.parse_var_stmt()?;
            self.consume_semicolon()?;
            return Ok(stmt);
        }
        if self.at_ident("function") {
            let func = self.parse_function(true, false)?;
            return Ok(Stmt::Func(Box::new(func)));
        }
        if self.at_ident("async") && self.peek(1).is_ident("function") {
            self.bump();
            let func = self.parse_function(true, true)?;
            return Ok(Stmt::Func(Box::new(func)));
        }
        if self.at_ident("class") {
            return self.parse_class(false);
        }
        if self.at_ident("if") {
            return self.parse_if();
        }
        if self.at_ident("for") {
            return self.parse_for();
        }
        if self.at_ident("while") {
            self.bump();
            self.expect_punct("(")?;
            let test = self.parse_expression()?;
            self.expect_punct(")")?;
            let body = self.parse_stmt()?;
            return Ok(Stmt::While {
                test,
                body: Box::new(body),
            });
        }
        if self.at_ident("do") {
            self.bump();
            let body = self.parse_stmt()?;
            if !self.eat_ident("while") {
                return self.err("expected `while` after a `do` body");
            }
            self.expect_punct("(")?;
            let test = self.parse_expression()?;
            self.expect_punct(")")?;
            self.consume_semicolon()?;
            return Ok(Stmt::DoWhile {
                body: Box::new(body),
                test,
            });
        }
        if self.at_ident("switch") {
            return self.parse_switch();
        }
        if self.at_ident("try") {
            return self.parse_try();
        }
        if self.at_ident("return") {
            self.bump();
            let arg = if self.at_punct(";") || self.at_punct("}") || self.at_eof() || self.nl_before()
            {
                None
            } else {
                Some(self.parse_expression()?)
            };
            self.consume_semicolon()?;
            return Ok(Stmt::Return(arg));
        }
        if self.at_ident("throw") {
            self.bump();
            let arg = self.parse_expression()?;
            self.consume_semicolon()?;
            return Ok(Stmt::Throw(arg));
        }
        if self.at_ident("break") || self.at_ident("continue") {
            let is_break = self.at_ident("break");
            self.bump();
            let label = match self.cur().clone() {
                Tok::Ident(name) if !self.nl_before() && !is_reserved(&name) => {
                    self.bump();
                    Some(name)
                }
                _ => None,
            };
            self.consume_semicolon()?;
            return Ok(if is_break {
                Stmt::Break(label)
            } else {
                Stmt::Continue(label)
            });
        }
        if self.at_ident("import") && !self.peek(1).is_punct("(") && !self.peek(1).is_punct(".") {
            return self.parse_import();
        }
        if self.at_ident("export") {
            return self.parse_export();
        }
        if self.at_ident("with") {
            return self.err("`with` statements are not supported");
        }

        // A labelled statement, or a plain expression statement.
        if let Tok::Ident(name) = self.cur().clone() {
            if self.peek(1).is_punct(":") && !is_reserved(&name) && name != "default" {
                self.bump();
                self.bump();
                let body = self.parse_stmt()?;
                return Ok(Stmt::Labeled {
                    label: name,
                    body: Box::new(body),
                });
            }
        }

        let expr = self.parse_expression()?;
        self.consume_semicolon()?;
        Ok(Stmt::Expr(expr))
    }

    /// True when `declare` begins a declaration rather than being an identifier.
    fn declare_is_statement(&self) -> bool {
        match self.peek(1) {
            Tok::Ident(word) => matches!(
                word.as_str(),
                "var"
                    | "let"
                    | "const"
                    | "function"
                    | "class"
                    | "interface"
                    | "type"
                    | "enum"
                    | "namespace"
                    | "module"
                    | "global"
                    | "abstract"
            ),
            _ => false,
        }
    }

    fn parse_var_stmt(&mut self) -> Result<Stmt, ParseError> {
        let kind = if self.at_ident("var") {
            VarKind::Var
        } else if self.at_ident("let") {
            VarKind::Let
        } else {
            VarKind::Const
        };
        self.bump();
        let decls = self.parse_var_declarators()?;
        Ok(Stmt::Var { kind, decls })
    }

    fn parse_var_declarators(&mut self) -> Result<Vec<(Pattern, Option<Expr>)>, ParseError> {
        let mut decls = Vec::new();
        loop {
            let pattern = self.parse_binding_target()?;
            let init = if self.eat_punct("=") {
                Some(self.parse_assignment()?)
            } else {
                None
            };
            decls.push((pattern, init));
            if !self.eat_punct(",") {
                break;
            }
        }
        Ok(decls)
    }

    /// Parse a binding target that may carry a default value (`x = 1`). Used for
    /// parameters and for the elements of a destructuring pattern, where a
    /// top-level `=` really is a default.
    fn parse_binding_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.parse_binding_pattern_inner(true)
    }

    /// Parse a binding target where a top-level `=` is *not* a default: in a
    /// declaration or a `for` header the `=` introduces the initialiser, and in
    /// a `catch` clause a default is not allowed at all.
    fn parse_binding_target(&mut self) -> Result<Pattern, ParseError> {
        self.parse_binding_pattern_inner(false)
    }

    /// Parse a binding target: an identifier, destructuring pattern, optionally
    /// with a default value and a TypeScript annotation.
    fn parse_binding_pattern_inner(&mut self, allow_default: bool) -> Result<Pattern, ParseError> {
        let mut pattern = match self.cur().clone() {
            Tok::Punct(p) if p == "[" => self.parse_array_pattern()?,
            Tok::Punct(p) if p == "{" => self.parse_object_pattern()?,
            _ => {
                let name = self.expect_ident()?;
                Pattern::Ident(name)
            }
        };
        if self.ts {
            self.eat_optional_marker();
            if self.at_punct(":") {
                self.bump();
                self.skip_type(TypeContext::Annotation)?;
            }
        }
        if allow_default && self.at_punct("=") {
            self.bump();
            let default = self.parse_assignment()?;
            pattern = Pattern::Default(Box::new(pattern), Box::new(default));
        }
        Ok(pattern)
    }

    fn parse_array_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.expect_punct("[")?;
        let mut elements = Vec::new();
        let mut rest = None;
        while !self.at_punct("]") {
            if self.at_eof() {
                return self.err("unexpected end of input inside an array pattern");
            }
            if self.eat_punct(",") {
                elements.push(None);
                continue;
            }
            if self.at_punct("...") {
                self.bump();
                rest = Some(Box::new(self.parse_binding_pattern()?));
                break;
            }
            elements.push(Some(self.parse_binding_pattern()?));
            if !self.at_punct("]") {
                self.expect_punct(",")?;
            }
        }
        self.expect_punct("]")?;
        Ok(Pattern::Array { elements, rest })
    }

    fn parse_object_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.expect_punct("{")?;
        let mut props = Vec::new();
        let mut rest = None;
        while !self.at_punct("}") {
            if self.at_eof() {
                return self.err("unexpected end of input inside an object pattern");
            }
            if self.at_punct("...") {
                self.bump();
                rest = Some(Box::new(self.parse_binding_pattern()?));
                break;
            }
            let key = self.parse_prop_key()?;
            let value = if self.eat_punct(":") {
                self.parse_binding_pattern()?
            } else {
                // Shorthand, possibly with a default: `{ a = 1 }`.
                let name = match &key {
                    PropKey::Ident(name) => name.clone(),
                    _ => {
                        return self
                            .err("a destructuring property needs a value after `:`")
                    }
                };
                let mut pat = Pattern::Ident(name);
                if self.ts {
                    self.eat_optional_marker();
                    if self.at_punct(":") {
                        self.bump();
                        self.skip_type(TypeContext::Annotation)?;
                    }
                }
                if self.eat_punct("=") {
                    let default = self.parse_assignment()?;
                    pat = Pattern::Default(Box::new(pat), Box::new(default));
                }
                pat
            };
            props.push(ObjPatProp { key, value });
            if !self.at_punct("}") {
                self.expect_punct(",")?;
            }
        }
        self.expect_punct("}")?;
        Ok(Pattern::Object { props, rest })
    }

    /// Parse an object literal property key.
    fn parse_prop_key(&mut self) -> Result<PropKey, ParseError> {
        if self.eat_punct("[") {
            let expr = self.parse_assignment()?;
            self.expect_punct("]")?;
            return Ok(PropKey::Computed(Box::new(expr)));
        }
        match self.cur().clone() {
            Tok::Str(s) => {
                self.bump();
                Ok(PropKey::Str(s))
            }
            Tok::Num { value, .. } => {
                self.bump();
                Ok(PropKey::Num(value))
            }
            Tok::BigInt(d) => {
                self.bump();
                Ok(PropKey::Str(format!("{d}n")))
            }
            Tok::Ident(name) => {
                self.bump();
                Ok(PropKey::Ident(name))
            }
            other => self.err(format!(
                "expected a property name but found {}",
                other.describe()
            )),
        }
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        self.bump();
        self.expect_punct("(")?;
        let test = self.parse_expression()?;
        self.expect_punct(")")?;
        let cons = self.parse_stmt()?;
        let alt = if self.eat_ident("else") {
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        Ok(Stmt::If {
            test,
            cons: Box::new(cons),
            alt,
        })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        self.bump();
        let is_await = self.eat_ident("await");
        self.expect_punct("(")?;

        // `for (;;)`
        if self.eat_punct(";") {
            let test = if self.at_punct(";") {
                None
            } else {
                Some(self.parse_expression()?)
            };
            self.expect_punct(";")?;
            let update = if self.at_punct(")") {
                None
            } else {
                Some(self.parse_expression()?)
            };
            self.expect_punct(")")?;
            let body = self.parse_stmt()?;
            return Ok(Stmt::For {
                init: None,
                test,
                update,
                body: Box::new(body),
            });
        }

        // Declaration header: `for (let x of y)` or `for (let i = 0; ...)`.
        if self.at_ident("var") || self.at_ident("let") || self.at_ident("const") {
            let kind = if self.at_ident("var") {
                VarKind::Var
            } else if self.at_ident("let") {
                VarKind::Let
            } else {
                VarKind::Const
            };
            self.bump();
            let first = self.parse_binding_target()?;
            if self.at_ident("of") || self.at_ident("in") {
                let is_of = self.at_ident("of");
                self.bump();
                let right = if is_of {
                    self.parse_assignment()?
                } else {
                    let saved = self.no_in;
                    self.no_in = true;
                    let e = self.parse_expression()?;
                    self.no_in = saved;
                    e
                };
                self.expect_punct(")")?;
                let body = self.parse_stmt()?;
                let target = ForTarget::Var { kind, pattern: first };
                return Ok(if is_of {
                    Stmt::ForOf {
                        target,
                        right,
                        body: Box::new(body),
                        is_await,
                    }
                } else {
                    Stmt::ForIn {
                        target,
                        right,
                        body: Box::new(body),
                    }
                });
            }

            // Classic `for` with declarations.
            let mut decls = Vec::new();
            let init = if self.eat_punct("=") {
                Some(self.parse_assignment()?)
            } else {
                None
            };
            decls.push((first, init));
            while self.eat_punct(",") {
                let pattern = self.parse_binding_target()?;
                let init = if self.eat_punct("=") {
                    Some(self.parse_assignment()?)
                } else {
                    None
                };
                decls.push((pattern, init));
            }
            self.expect_punct(";")?;
            let test = if self.at_punct(";") {
                None
            } else {
                Some(self.parse_expression()?)
            };
            self.expect_punct(";")?;
            let update = if self.at_punct(")") {
                None
            } else {
                Some(self.parse_expression()?)
            };
            self.expect_punct(")")?;
            let body = self.parse_stmt()?;
            return Ok(Stmt::For {
                init: Some(ForInit::Var { kind, decls }),
                test,
                update,
                body: Box::new(body),
            });
        }

        // Expression header.
        let saved = self.no_in;
        self.no_in = true;
        let init_expr = self.parse_expression()?;
        self.no_in = saved;

        if self.at_ident("of") || self.at_ident("in") {
            let is_of = self.at_ident("of");
            self.bump();
            let right = if is_of {
                self.parse_assignment()?
            } else {
                let saved = self.no_in;
                self.no_in = true;
                let e = self.parse_expression()?;
                self.no_in = saved;
                e
            };
            self.expect_punct(")")?;
            let body = self.parse_stmt()?;
            let target = ForTarget::Expr(init_expr);
            return Ok(if is_of {
                Stmt::ForOf {
                    target,
                    right,
                    body: Box::new(body),
                    is_await,
                }
            } else {
                Stmt::ForIn {
                    target,
                    right,
                    body: Box::new(body),
                }
            });
        }

        self.expect_punct(";")?;
        let test = if self.at_punct(";") {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect_punct(";")?;
        let update = if self.at_punct(")") {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect_punct(")")?;
        let body = self.parse_stmt()?;
        Ok(Stmt::For {
            init: Some(ForInit::Expr(init_expr)),
            test,
            update,
            body: Box::new(body),
        })
    }

    fn parse_switch(&mut self) -> Result<Stmt, ParseError> {
        self.bump();
        self.expect_punct("(")?;
        let disc = self.parse_expression()?;
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        let mut cases = Vec::new();
        while !self.at_punct("}") {
            if self.at_eof() {
                return self.err("unexpected end of input inside a `switch`");
            }
            let test = if self.eat_ident("case") {
                let e = self.parse_expression()?;
                self.expect_punct(":")?;
                Some(e)
            } else if self.eat_ident("default") {
                self.expect_punct(":")?;
                None
            } else {
                return self.err("expected `case` or `default` inside a `switch`");
            };
            let mut body = Vec::new();
            while !self.at_punct("}") && !self.at_ident("case") && !self.at_ident("default") {
                if self.at_eof() {
                    return self.err("unexpected end of input inside a `switch` case");
                }
                body.push(self.parse_stmt()?);
            }
            cases.push(SwitchCase { test, body });
        }
        self.expect_punct("}")?;
        Ok(Stmt::Switch { disc, cases })
    }

    fn parse_try(&mut self) -> Result<Stmt, ParseError> {
        self.bump();
        let block = self.parse_block()?;
        let mut handler = None;
        let mut finalizer = None;
        if self.eat_ident("catch") {
            let binding = if self.eat_punct("(") {
                let pattern = self.parse_binding_target()?;
                self.expect_punct(")")?;
                Some(pattern)
            } else {
                None
            };
            let body = self.parse_block()?;
            handler = Some((binding, body));
        }
        if self.eat_ident("finally") {
            finalizer = Some(self.parse_block()?);
        }
        if handler.is_none() && finalizer.is_none() {
            return self.err("`try` must be followed by `catch` or `finally`");
        }
        Ok(Stmt::Try {
            block,
            handler,
            finalizer,
        })
    }

    fn parse_import(&mut self) -> Result<Stmt, ParseError> {
        self.bump(); // 'import'

        // `import type ...` is erased.
        let type_only = self.at_ident("type")
            && !self.peek(1).is_punct(",")
            && !self.peek(1).is_ident("from");
        if type_only {
            self.skip_statement_to_end();
            return Ok(Stmt::Empty);
        }

        // `import "module"` — side-effect only.
        if let Tok::Str(source) = self.cur().clone() {
            self.bump();
            self.consume_semicolon()?;
            return Ok(Stmt::Import(ImportDecl {
                specifiers: Vec::new(),
                source,
            }));
        }

        let mut specifiers = Vec::new();
        if let Tok::Ident(name) = self.cur().clone() {
            self.bump();
            specifiers.push(ImportSpecifier::Default(name));
            if !self.eat_punct(",") {
                self.expect_word("from")?;
                let source = self.expect_string()?;
                self.consume_semicolon()?;
                return Ok(Stmt::Import(ImportDecl {
                    specifiers,
                    source,
                }));
            }
        }

        if self.at_punct("*") {
            self.bump();
            self.expect_word("as")?;
            let local = self.expect_ident()?;
            specifiers.push(ImportSpecifier::Namespace(local));
        } else if self.eat_punct("{") {
            while !self.at_punct("}") {
                if self.at_eof() {
                    return self.err("unexpected end of input in an import list");
                }
                // Inline `type` specifiers are erased.
                if self.ts
                    && self.at_ident("type")
                    && !self.peek(1).is_punct(",")
                    && !self.peek(1).is_punct("}")
                    && !self.peek(1).is_ident("as")
                {
                    self.bump();
                    let _ = self.expect_ident()?;
                    if self.eat_ident("as") {
                        let _ = self.expect_ident()?;
                    }
                } else {
                    let imported = self.import_export_name()?;
                    let local = if self.eat_ident("as") {
                        self.expect_ident()?
                    } else {
                        imported.clone()
                    };
                    specifiers.push(ImportSpecifier::Named { imported, local });
                }
                if !self.at_punct("}") {
                    self.expect_punct(",")?;
                }
            }
            self.expect_punct("}")?;
        }

        if !self.eat_ident("from") {
            return self.err("expected `from` in an import declaration");
        }
        let source = self.expect_string()?;
        self.consume_semicolon()?;
        Ok(Stmt::Import(ImportDecl {
            specifiers,
            source,
        }))
    }

    /// A module export/import name, which may be a keyword or a string.
    fn import_export_name(&mut self) -> Result<String, ParseError> {
        match self.cur().clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(name)
            }
            Tok::Str(name) => {
                self.bump();
                Ok(name)
            }
            Tok::Num { raw, .. } => {
                self.bump();
                Ok(raw)
            }
            other => self.err(format!(
                "expected a module name but found {}",
                other.describe()
            )),
        }
    }

    fn parse_export(&mut self) -> Result<Stmt, ParseError> {
        self.bump(); // 'export'

        if self.ts {
            // After `export`, the word `type` always begins a type-only export
            // (`export type { A }`, `export type X = ...`), never a value.
            if self.at_ident("type") {
                self.skip_statement_to_end();
                return Ok(Stmt::Empty);
            }
            if self.at_ident("interface") {
                self.skip_interface()?;
                return Ok(Stmt::Empty);
            }
            if self.is_type_alias_start() {
                self.skip_type_alias()?;
                return Ok(Stmt::Empty);
            }
            if self.at_ident("declare") {
                self.skip_declare()?;
                return Ok(Stmt::Empty);
            }
        }

        if self.at_ident("default") {
            self.bump();
            let inner = if self.at_ident("function")
                || (self.at_ident("async") && self.peek(1).is_ident("function"))
            {
                let is_async = self.eat_ident("async");
                let func = self.parse_function(false, is_async)?;
                Stmt::Func(Box::new(func))
            } else if self.at_ident("class") || (self.ts && self.at_ident("abstract")) {
                if self.ts {
                    self.eat_ident("abstract");
                }
                self.parse_class(false)?
            } else {
                let expr = self.parse_assignment()?;
                self.consume_semicolon()?;
                Stmt::Expr(expr)
            };
            return Ok(Stmt::Export(ExportDecl::Default(Box::new(inner))));
        }

        if self.at_punct("*") {
            self.bump();
            let alias = if self.eat_ident("as") {
                Some(self.import_export_name()?)
            } else {
                None
            };
            if !self.eat_ident("from") {
                return self.err("expected `from` after `export *`");
            }
            let source = self.expect_string()?;
            self.consume_semicolon()?;
            return Ok(Stmt::Export(ExportDecl::All { source, alias }));
        }

        if self.eat_punct("{") {
            let mut specifiers = Vec::new();
            while !self.at_punct("}") {
                if self.at_eof() {
                    return self.err("unexpected end of input in an export list");
                }
                if self.ts
                    && self.at_ident("type")
                    && !self.peek(1).is_punct(",")
                    && !self.peek(1).is_punct("}")
                    && !self.peek(1).is_ident("as")
                {
                    self.bump();
                    let _ = self.import_export_name()?;
                    if self.eat_ident("as") {
                        let _ = self.import_export_name()?;
                    }
                } else {
                    let local = self.import_export_name()?;
                    let exported = if self.eat_ident("as") {
                        self.import_export_name()?
                    } else {
                        local.clone()
                    };
                    specifiers.push(ExportSpecifier { local, exported });
                }
                if !self.at_punct("}") {
                    self.expect_punct(",")?;
                }
            }
            self.expect_punct("}")?;
            let source = if self.eat_ident("from") {
                Some(self.expect_string()?)
            } else {
                None
            };
            self.consume_semicolon()?;
            return Ok(Stmt::Export(ExportDecl::Named {
                specifiers,
                source,
            }));
        }

        if self.at_ident("enum") || (self.at_ident("const") && self.peek(1).is_ident("enum")) {
            let decl = self.parse_enum()?;
            return Ok(Stmt::Export(ExportDecl::Decl(Box::new(decl))));
        }

        if self.at_ident("abstract") && self.peek(1).is_ident("class") {
            self.bump();
            let class = self.parse_class(true)?;
            return Ok(Stmt::Export(ExportDecl::Decl(Box::new(class))));
        }

        let inner = self.parse_stmt()?;
        Ok(Stmt::Export(ExportDecl::Decl(Box::new(inner))))
    }

    /// Skip the remainder of the current statement, used for erased type-only
    /// declarations.
    fn skip_statement_to_end(&mut self) {
        let mut depth = 0i32;
        loop {
            if self.at_eof() {
                return;
            }
            let tok = self.cur().clone();
            match &tok {
                Tok::Punct(p) => match p.as_str() {
                    "(" | "[" | "{" | "<" => {
                        depth += 1;
                        self.bump();
                    }
                    ")" | "]" | "}" | ">" => {
                        if p == "}" && depth == 0 {
                            return;
                        }
                        depth -= 1;
                        self.bump();
                    }
                    ";" if depth == 0 => {
                        self.bump();
                        return;
                    }
                    _ => {
                        self.bump();
                    }
                },
                _ => {
                    if depth == 0 && self.nl_before() {
                        return;
                    }
                    self.bump();
                }
            }
        }
    }

    // ------------------------------------------------------------- functions

    fn parse_function(&mut self, is_decl: bool, is_async: bool) -> Result<Func, ParseError> {
        self.expect_word("function")?;
        let is_generator = self.eat_punct("*");
        let name = if let Tok::Ident(name) = self.cur().clone() {
            self.bump();
            Some(name)
        } else if is_decl {
            return self.err("a function declaration needs a name");
        } else {
            None
        };
        if self.ts && self.at_punct("<") {
            self.skip_angle_group()?;
        }
        let params = self.parse_param_list(true)?;
        if self.ts && self.at_punct(":") {
            self.bump();
            self.skip_type(TypeContext::Return)?;
        }
        if self.at_punct(";") {
            // A TypeScript overload signature: it has no body, so it has no
            // runtime effect and becomes an empty function. A later declaration
            // of the same name supplies the real implementation.
            self.bump();
            return Ok(Func {
                name,
                params: Vec::new(),
                body: FuncBody::Block(Vec::new()),
                is_async,
                is_generator,
                is_arrow: false,
                is_expression_body: false,
            });
        }
        if is_generator {
            self.generator_depth += 1;
        }
        let body = self.parse_block()?;
        if is_generator {
            self.generator_depth -= 1;
        }
        Ok(Func {
            name,
            params,
            body: FuncBody::Block(body),
            is_async,
            is_generator,
            is_arrow: false,
            is_expression_body: false,
        })
    }

    /// Parse `( ... )` parameters.
    fn parse_param_list(&mut self, allow_property_parameters: bool) -> Result<Vec<Param>, ParseError> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        while !self.at_punct(")") {
            if self.at_eof() {
                return self.err("unexpected end of input in a parameter list");
            }
            // TypeScript parameter modifiers, including parameter properties.
            let mut is_property = false;
            if self.ts {
                loop {
                    let modifier = match self.cur().ident_text() {
                        Some(
                            word @ ("public" | "private" | "protected" | "readonly" | "override"),
                        ) => word.to_string(),
                        _ => break,
                    };
                    // `readonly` alone does not create a property, but any of
                    // these modifiers on a constructor parameter does.
                    if modifier != "override" {
                        is_property = true;
                    }
                    self.bump();
                }
            }
            let rest = self.eat_punct("...");
            let pattern = self.parse_binding_pattern()?;
            if is_property && allow_property_parameters {
                // A modifier on a constructor parameter declares a property, so
                // `private x: number = 1` becomes `this.x = x`. The pattern may
                // be wrapped in a default, hence the name extraction.
                for name in crate::interp::pattern_names(&pattern) {
                    self.property_parameters.push(name);
                }
            }
            params.push(Param { pattern, rest });
            if !self.at_punct(")") {
                self.expect_punct(",")?;
            }
        }
        self.expect_punct(")")?;
        Ok(params)
    }

    /// Parse the body of an arrow function after `=>`.
    fn parse_arrow_body(&mut self) -> Result<FuncBody, ParseError> {
        if self.at_punct("{") {
            let body = self.parse_block()?;
            Ok(FuncBody::Block(body))
        } else {
            let expr = self.parse_assignment()?;
            Ok(FuncBody::Expr(Box::new(expr)))
        }
    }

    /// After seeing `(`, decide whether this is an arrow's parameter list or a
    /// parenthesised expression. Restores the position when it is not an arrow.
    fn try_parse_paren_arrow(&mut self, is_async: bool) -> Result<Option<Expr>, ParseError> {
        let saved = self.pos;
        let saved_props = self.property_parameters.len();
        let attempt = (|| -> Result<Option<Expr>, ParseError> {
            let params = self.parse_param_list(false)?;
            if self.ts && self.at_punct(":") {
                self.bump();
                self.skip_type(TypeContext::Return)?;
            }
            if !self.at_punct("=>") {
                return Ok(None);
            }
            self.bump();
            let body = self.parse_arrow_body()?;
            let is_expression_body = matches!(body, FuncBody::Expr(_));
            Ok(Some(Expr::Func(Box::new(Func {
                name: None,
                params,
                body,
                is_async,
                is_generator: false,
                is_arrow: true,
                is_expression_body,
            }))))
        })();
        match attempt {
            Ok(Some(expr)) => Ok(Some(expr)),
            Ok(None) | Err(_) => {
                self.pos = saved;
                self.property_parameters.truncate(saved_props);
                Ok(None)
            }
        }
    }

    /// Try to parse a TypeScript generic arrow: `<T>(x: T) => x`.
    fn try_parse_generic_arrow(&mut self, is_async: bool) -> Result<Option<Expr>, ParseError> {
        if !self.ts || !self.at_punct("<") {
            return Ok(None);
        }
        let saved = self.pos;
        let attempt = (|| -> Result<Option<Expr>, ParseError> {
            self.skip_angle_group()?;
            if !self.at_punct("(") {
                return Ok(None);
            }
            let params = self.parse_param_list(false)?;
            if self.at_punct(":") {
                self.bump();
                self.skip_type(TypeContext::Return)?;
            }
            if !self.at_punct("=>") {
                return Ok(None);
            }
            self.bump();
            let body = self.parse_arrow_body()?;
            let is_expression_body = matches!(body, FuncBody::Expr(_));
            Ok(Some(Expr::Func(Box::new(Func {
                name: None,
                params,
                body,
                is_async,
                is_generator: false,
                is_arrow: true,
                is_expression_body,
            }))))
        })();
        match attempt {
            Ok(Some(expr)) => Ok(Some(expr)),
            Ok(None) | Err(_) => {
                self.pos = saved;
                Ok(None)
            }
        }
    }

    // ------------------------------------------------------------- classes

    fn parse_class(&mut self, is_decl: bool) -> Result<Stmt, ParseError> {
        let class = self.parse_class_body(is_decl)?;
        Ok(Stmt::Class(Box::new(class)))
    }

    fn parse_class_body(&mut self, is_decl: bool) -> Result<Class, ParseError> {
        self.expect_word("class")?;
        let name = if let Tok::Ident(name) = self.cur().clone() {
            if is_reserved(&name) {
                return self.err(format!("`{name}` cannot be used as a class name"));
            }
            self.bump();
            Some(name)
        } else if is_decl {
            return self.err("a class declaration needs a name");
        } else {
            None
        };
        if self.ts && self.at_punct("<") {
            self.skip_angle_group()?;
        }

        let superclass = if self.eat_ident("extends") {
            let expr = self.parse_member_expression_only()?;
            if self.ts && self.at_punct("<") {
                self.skip_angle_group()?;
            }
            Some(Box::new(expr))
        } else {
            None
        };

        if self.ts && self.at_ident("implements") {
            self.bump();
            // A comma-separated list of types, terminated by the class body.
            // Angle depth is tracked so that a `{` inside type arguments is not
            // mistaken for the start of the body.
            let mut angle = 0i32;
            loop {
                if self.at_eof() {
                    return self.err("unexpected end of input in an `implements` clause");
                }
                match self.cur().clone() {
                    Tok::Punct(p) => {
                        if p == "{" && angle <= 0 {
                            break;
                        }
                        if p.starts_with('>') && angle > 0 {
                            if let Some(rest) = consume_leading_angle(&p, &mut angle) {
                                if rest.is_empty() {
                                    self.bump();
                                } else {
                                    self.toks[self.pos].tok = Tok::Punct(rest);
                                }
                                continue;
                            }
                        }
                        match p.as_str() {
                            "<" | "{" => angle += 1,
                            "}" => angle -= 1,
                            _ => {}
                        }
                        self.bump();
                    }
                    _ => {
                        self.bump();
                    }
                }
            }
        }

        self.expect_punct("{")?;
        let mut members = Vec::new();
        while !self.at_punct("}") {
            if self.at_eof() {
                return self.err("unexpected end of input inside a class body");
            }
            if self.eat_punct(";") {
                continue;
            }
            if self.at_punct("@") {
                while self.at_punct("@") {
                    self.warn("decorator ignored: rust4ts does not run decorators");
                    self.skip_decorator()?;
                }
            }
            if let Some(member) = self.parse_class_member()? {
                members.push(member);
            }
        }
        self.expect_punct("}")?;
        Ok(Class {
            name,
            superclass,
            members,
        })
    }

    fn parse_class_member(&mut self) -> Result<Option<ClassMember>, ParseError> {
        let mut is_static = false;
        let mut is_declared = false;

        // Modifiers. `static` is the only one with runtime meaning.
        loop {
            let word = match self.cur().ident_text() {
                Some(w) => w.to_string(),
                None => break,
            };
            match word.as_str() {
                "static" => {
                    // `static` is a modifier only when a member name follows.
                    let next = self.peek(1).clone();
                    let is_modifier = matches!(next, Tok::Ident(_) | Tok::Str(_) | Tok::Num { .. })
                        || next.is_punct("[")
                        || next.is_punct("{");
                    if !is_modifier {
                        break;
                    }
                    self.bump();
                    is_static = true;
                    if self.at_punct("{") {
                        let body = self.parse_block()?;
                        return Ok(Some(ClassMember::StaticBlock(body)));
                    }
                }
                "public" | "private" | "protected" | "readonly" | "override" | "abstract"
                | "accessor" => {
                    // Kept only for parsing; these have no runtime effect. An
                    // `abstract` member without a body is skipped below.
                    self.bump();
                }
                "declare" => {
                    self.bump();
                    is_declared = true;
                }
                "async" if !matches!(self.peek(1), Tok::Punct(p) if p == "(" || p == "=" || p == ";" || p == ":") =>
                {
                    break;
                }
                "get" | "set" if !matches!(self.peek(1), Tok::Punct(p) if p == "(" || p == "=" || p == ";" || p == ":") =>
                {
                    break;
                }
                _ => break,
            }
        }

        // `declare` members exist only for the type checker.
        if is_declared {
            self.skip_statement_to_end();
            return Ok(None);
        }

        // Accessors and async methods.
        let mut accessor: Option<bool> = None;
        if self.at_ident("get") || self.at_ident("set") {
            let is_getter = self.at_ident("get");
            self.bump();
            accessor = Some(is_getter);
        }
        let mut is_async = false;
        if self.at_ident("async") {
            self.bump();
            is_async = true;
        }
        let is_generator = self.eat_punct("*");

        let key = self.parse_prop_key()?;
        if self.ts {
            self.eat_optional_marker();
        }

        // A field with a type annotation but no initialiser.
        if self.ts && self.at_punct(":") {
            self.bump();
            self.skip_type(TypeContext::Annotation)?;
        }

        if self.at_punct("(") {
            let is_constructor = !is_static
                && accessor.is_none()
                && matches!(&key, PropKey::Ident(name) if name == "constructor");
            // Parameter properties are only meaningful in a constructor.
            self.property_parameters.clear();
            let params = self.parse_param_list(is_constructor)?;
            if self.ts && self.at_punct(":") {
                self.bump();
                self.skip_type(TypeContext::Return)?;
            }
            if self.at_punct(";") {
                // A method overload signature: drop it.
                self.bump();
                self.property_parameters.clear();
                return Ok(None);
            }
            if is_generator {
                self.generator_depth += 1;
            }
            let mut body = self.parse_block()?;
            if is_generator {
                self.generator_depth -= 1;
            }
            let injected = std::mem::take(&mut self.property_parameters);
            if !injected.is_empty() {
                let mut prefix: Stmts = injected
                    .iter()
                    .map(|name| {
                        Stmt::Expr(Expr::Assign {
                            op: AssignOp::Assign,
                            target: Box::new(Expr::Member {
                                object: Box::new(Expr::This),
                                prop: MemberProp::Name(name.clone()),
                                optional: false,
                            }),
                            value: Box::new(Expr::Ident(name.clone())),
                        })
                    })
                    .collect();
                prefix.append(&mut body);
                body = prefix;
            }
            let kind = match accessor {
                Some(true) => MethodKind::Get,
                Some(false) => MethodKind::Set,
                None if is_constructor => MethodKind::Constructor,
                None => MethodKind::Method,
            };
            return Ok(Some(ClassMember::Method {
                key,
                func: Box::new(Func {
                    name: None,
                    params,
                    body: FuncBody::Block(body),
                    is_async,
                    is_generator,
                    is_arrow: false,
                    is_expression_body: false,
                }),
                is_static,
                kind,
            }));
        }

        // Otherwise a field.
        let value = if self.eat_punct("=") {
            Some(self.parse_assignment()?)
        } else {
            None
        };
        self.consume_semicolon()?;
        Ok(Some(ClassMember::Field {
            key,
            value,
            is_static,
        }))
    }

    /// Parse an expression that is a primary plus member accesses, used for
    /// `extends` clauses where a `<` must not be read as a comparison.
    fn parse_member_expression_only(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at_punct(".") {
                self.bump();
                let name = match self.cur().clone() {
                    Tok::Ident(name) => {
                        self.bump();
                        name
                    }
                    other => {
                        return self.err(format!(
                            "expected a property name but found {}",
                            other.describe()
                        ))
                    }
                };
                expr = Expr::Member {
                    object: Box::new(expr),
                    prop: MemberProp::Name(name),
                    optional: false,
                };
            } else if self.at_punct("[") {
                self.bump();
                let index = self.parse_expression()?;
                self.expect_punct("]")?;
                expr = Expr::Member {
                    object: Box::new(expr),
                    prop: MemberProp::Index(Box::new(index)),
                    optional: false,
                };
            } else {
                return Ok(expr);
            }
        }
    }

    // ------------------------------------------------------------- expressions

    fn parse_expression(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_assignment()?;
        if !self.at_punct(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_punct(",") {
            items.push(self.parse_assignment()?);
        }
        Ok(Expr::Sequence(items))
    }

    fn parse_assignment(&mut self) -> Result<Expr, ParseError> {
        // `x => ...`
        if let Tok::Ident(name) = self.cur().clone() {
            if !is_reserved(&name) && self.peek(1).is_punct("=>") {
                self.bump();
                self.bump();
                let body = self.parse_arrow_body()?;
                let is_expression_body = matches!(body, FuncBody::Expr(_));
                return Ok(Expr::Func(Box::new(Func {
                    name: None,
                    params: vec![Param {
                        pattern: Pattern::Ident(name),
                        rest: false,
                    }],
                    body,
                    is_async: false,
                    is_generator: false,
                    is_arrow: true,
                    is_expression_body,
                })));
            }
        }
        // `(a, b) => ...` and `async (a) => ...`
        if self.at_punct("(") {
            if let Some(arrow) = self.try_parse_paren_arrow(false)? {
                return Ok(arrow);
            }
        }
        if self.at_ident("async") {
            let next = self.peek(1).clone();
            let arrowish = match &next {
                Tok::Punct(p) => p == "(",
                Tok::Ident(name) => !is_reserved(name),
                _ => false,
            };
            if arrowish && !self.peek_token_nl(1) {
                let saved = self.pos;
                self.bump(); // 'async'
                let mut arrow = None;
                if self.at_punct("(") {
                    arrow = self.try_parse_paren_arrow(true)?;
                }
                if arrow.is_none() {
                    if let Tok::Ident(name) = self.cur().clone() {
                        if !is_reserved(&name) && self.peek(1).is_punct("=>") {
                            self.bump();
                            self.bump();
                            let body = self.parse_arrow_body()?;
                            let is_expression_body = matches!(body, FuncBody::Expr(_));
                            arrow = Some(Expr::Func(Box::new(Func {
                                name: None,
                                params: vec![Param {
                                    pattern: Pattern::Ident(name),
                                    rest: false,
                                }],
                                body,
                                is_async: true,
                                is_generator: false,
                                is_arrow: true,
                                is_expression_body,
                            })));
                        }
                    }
                }
                if let Some(expr) = arrow {
                    return Ok(expr);
                }
                self.pos = saved;
            }
        }
        // `<T>(x: T) => x`
        if let Some(arrow) = self.try_parse_generic_arrow(false)? {
            return Ok(arrow);
        }

        let left = self.parse_conditional()?;
        if let Some(op) = self.assignment_operator() {
            self.bump();
            let value = self.parse_assignment()?;
            return Ok(Expr::Assign {
                op,
                target: Box::new(left),
                value: Box::new(value),
            });
        }
        Ok(left)
    }

    fn peek_token_nl(&self, n: usize) -> bool {
        let idx = (self.pos + n).min(self.toks.len() - 1);
        self.toks[idx].nl_before
    }

    fn assignment_operator(&self) -> Option<AssignOp> {
        let punct = match self.cur() {
            Tok::Punct(p) => p.as_str(),
            _ => return None,
        };
        let op = match punct {
            "=" => AssignOp::Assign,
            "+=" => AssignOp::Add,
            "-=" => AssignOp::Sub,
            "*=" => AssignOp::Mul,
            "/=" => AssignOp::Div,
            "%=" => AssignOp::Mod,
            "**=" => AssignOp::Exp,
            "&=" => AssignOp::BitAnd,
            "|=" => AssignOp::BitOr,
            "^=" => AssignOp::BitXor,
            "<<=" => AssignOp::Shl,
            ">>=" => AssignOp::Shr,
            ">>>=" => AssignOp::UShr,
            "&&=" => AssignOp::And,
            "||=" => AssignOp::Or,
            "??=" => AssignOp::Nullish,
            _ => return None,
        };
        Some(op)
    }

    fn parse_conditional(&mut self) -> Result<Expr, ParseError> {
        let test = self.parse_binary(1)?;
        if self.at_punct("?") {
            self.bump();
            let saved = self.no_in;
            self.no_in = false;
            let cons = self.parse_assignment()?;
            self.no_in = saved;
            self.expect_punct(":")?;
            let alt = self.parse_assignment()?;
            return Ok(Expr::Cond {
                test: Box::new(test),
                cons: Box::new(cons),
                alt: Box::new(alt),
            });
        }
        Ok(test)
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            // `as T` / `satisfies T` cast.
            if self.ts && (self.at_ident("as") || self.at_ident("satisfies")) {
                if self.at_ident("as") && self.peek(1).is_ident("const") {
                    self.bump();
                    self.bump();
                    continue;
                }
                self.bump();
                self.skip_type(TypeContext::Annotation)?;
                continue;
            }

            let (op, prec) = match self.current_operator() {
                Some(pair) => pair,
                None => break,
            };
            if prec < min_prec {
                break;
            }
            self.bump();
            // `**` is right-associative; everything else is left-associative.
            let next_min = if matches!(op, Operator::Bin(BinOp::Exp)) {
                prec
            } else {
                prec + 1
            };
            let right = self.parse_binary(next_min)?;
            left = match op {
                Operator::Bin(op) => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                Operator::Log(op) => Expr::Logical {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    fn current_operator(&self) -> Option<(Operator, u8)> {
        match self.cur() {
            Tok::Punct(p) => binary_op(p),
            Tok::Ident(word) => {
                if word == "in" && self.no_in {
                    return None;
                }
                if word == "in" || word == "instanceof" {
                    binary_op(word)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.cur() {
            Tok::Punct(p) => match p.as_str() {
                "!" => Some(UnOp::Not),
                "-" => Some(UnOp::Minus),
                "+" => Some(UnOp::Plus),
                "~" => Some(UnOp::BitNot),
                _ => None,
            },
            Tok::Ident(word) => match word.as_str() {
                "typeof" => Some(UnOp::Typeof),
                "void" => Some(UnOp::Void),
                "delete" => Some(UnOp::Delete),
                _ => None,
            },
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let arg = self.parse_unary()?;
            return Ok(Expr::Unary {
                op,
                arg: Box::new(arg),
            });
        }

        if self.at_ident("await") {
            self.bump();
            let arg = self.parse_unary()?;
            return Ok(Expr::Await(Box::new(arg)));
        }

        if self.at_punct("++") || self.at_punct("--") {
            let op = if self.at_punct("++") {
                UpdateOp::Inc
            } else {
                UpdateOp::Dec
            };
            self.bump();
            let arg = self.parse_unary()?;
            return Ok(Expr::Update {
                op,
                prefix: true,
                arg: Box::new(arg),
            });
        }

        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let expr = self.parse_call_member()?;

        if !self.nl_before() && (self.at_punct("++") || self.at_punct("--")) {
            let op = if self.at_punct("++") {
                UpdateOp::Inc
            } else {
                UpdateOp::Dec
            };
            self.bump();
            return Ok(Expr::Update {
                op,
                prefix: false,
                arg: Box::new(expr),
            });
        }
        Ok(expr)
    }

    fn parse_call_member(&mut self) -> Result<Expr, ParseError> {
        let expr = self.parse_primary()?;
        self.parse_call_member_after(expr)
    }

    fn parse_call_member_after(&mut self, mut expr: Expr) -> Result<Expr, ParseError> {
        loop {
            // TypeScript non-null assertion inside a chain: `v!.w!()`. It is
            // only absorbed when no line break separates it, so `foo()\n!bar()`
            // is never glued together.
            if self.ts && self.at_punct("!") && !self.nl_before() {
                self.bump();
                continue;
            }
            if self.at_punct(".") {
                self.bump();
                let name = match self.cur().clone() {
                    Tok::Ident(name) => {
                        self.bump();
                        name
                    }
                    other => {
                        return self.err(format!(
                            "expected a property name but found {}",
                            other.describe()
                        ))
                    }
                };
                expr = Expr::Member {
                    object: Box::new(expr),
                    prop: MemberProp::Name(name),
                    optional: false,
                };
            } else if self.at_punct("?.") {
                self.bump();
                if self.at_punct("(") {
                    let args = self.parse_arguments()?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        optional: true,
                    };
                } else if self.at_punct("[") {
                    self.bump();
                    let index = self.parse_expression()?;
                    self.expect_punct("]")?;
                    expr = Expr::Member {
                        object: Box::new(expr),
                        prop: MemberProp::Index(Box::new(index)),
                        optional: true,
                    };
                } else {
                    let name = match self.cur().clone() {
                        Tok::Ident(name) => {
                            self.bump();
                            name
                        }
                        other => {
                            return self.err(format!(
                                "expected a property name after `?.` but found {}",
                                other.describe()
                            ))
                        }
                    };
                    expr = Expr::Member {
                        object: Box::new(expr),
                        prop: MemberProp::Name(name),
                        optional: true,
                    };
                }
            } else if self.at_punct("[") {
                self.bump();
                let index = self.parse_expression()?;
                self.expect_punct("]")?;
                expr = Expr::Member {
                    object: Box::new(expr),
                    prop: MemberProp::Index(Box::new(index)),
                    optional: false,
                };
            } else if self.at_punct("(") {
                let args = self.parse_arguments()?;
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                    optional: false,
                };
            } else if matches!(
                self.cur(),
                Tok::TemplateNoSub(_) | Tok::TemplateHead(_)
            ) {
                let (quasis, exprs) = self.parse_template_parts()?;
                expr = Expr::TaggedTemplate {
                    tag: Box::new(expr),
                    quasis,
                    exprs,
                };
            } else if self.ts && self.at_punct("<") {
                // Explicit type arguments at a call site: `f<number>(x)`. Only
                // committed to when the type list is followed by `(` or a
                // template literal, which keeps `a < b > (c)` a comparison.
                if !self.try_skip_call_type_arguments()? {
                    return Ok(expr);
                }
            } else {
                return Ok(expr);
            }
        }
    }

    /// Attempt to consume `<...>` type arguments in a call position. Returns
    /// false (and restores the position) when this does not look like a call.
    fn try_skip_call_type_arguments(&mut self) -> Result<bool, ParseError> {
        let saved = self.pos;
        let looks_like_call = match self.skip_angle_group() {
            Ok(()) => {
                self.at_punct("(")
                    || matches!(self.cur(), Tok::TemplateHead(_) | Tok::TemplateNoSub(_))
            }
            Err(_) => false,
        };
        if looks_like_call {
            Ok(true)
        } else {
            self.pos = saved;
            Ok(false)
        }
    }

    fn parse_arguments(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        while !self.at_punct(")") {
            if self.at_eof() {
                return self.err("unexpected end of input in an argument list");
            }
            if self.at_punct("...") {
                self.bump();
                let arg = self.parse_assignment()?;
                args.push(Expr::Spread(Box::new(arg)));
            } else {
                args.push(self.parse_assignment()?);
            }
            if !self.at_punct(")") {
                self.expect_punct(",")?;
            }
        }
        self.expect_punct(")")?;
        Ok(args)
    }

    /// Read the parts of a template literal, continuing after the head.
    fn parse_template_parts(&mut self) -> Result<(Vec<String>, Vec<Expr>), ParseError> {
        let mut quasis = Vec::new();
        let mut exprs = Vec::new();
        match self.bump() {
            Tok::TemplateNoSub(text) => {
                quasis.push(text);
                return Ok((quasis, exprs));
            }
            Tok::TemplateHead(text) => quasis.push(text),
            other => {
                return self.err(format!(
                    "expected a template literal but found {}",
                    other.describe()
                ))
            }
        }
        loop {
            let expr = self.parse_expression()?;
            exprs.push(expr);
            match self.bump() {
                Tok::TemplateMiddle(text) => quasis.push(text),
                Tok::TemplateTail(text) => {
                    quasis.push(text);
                    return Ok((quasis, exprs));
                }
                other => {
                    return self.err(format!(
                        "expected a template continuation but found {}",
                        other.describe()
                    ))
                }
            }
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.cur().clone() {
            Tok::Num { value, .. } => {
                self.bump();
                Ok(Expr::Num(value))
            }
            Tok::BigInt(digits) => {
                self.bump();
                Ok(Expr::BigInt(digits))
            }
            Tok::Str(text) => {
                self.bump();
                Ok(Expr::Str(text))
            }
            Tok::Regex { source, flags } => {
                self.bump();
                Ok(Expr::Regex { source, flags })
            }
            Tok::TemplateNoSub(_) | Tok::TemplateHead(_) => {
                let (quasis, exprs) = self.parse_template_parts()?;
                Ok(Expr::Template { quasis, exprs })
            }
            Tok::Ident(word) => match word.as_str() {
                "true" => {
                    self.bump();
                    Ok(Expr::Bool(true))
                }
                "false" => {
                    self.bump();
                    Ok(Expr::Bool(false))
                }
                "null" => {
                    self.bump();
                    Ok(Expr::Null)
                }
                "this" => {
                    self.bump();
                    Ok(Expr::This)
                }
                "super" => {
                    self.bump();
                    Ok(Expr::Super)
                }
                "new" => self.parse_new(),
                "function" => {
                    let func = self.parse_function(false, false)?;
                    Ok(Expr::Func(Box::new(func)))
                }
                "class" => {
                    let class = self.parse_class_body(false)?;
                    Ok(Expr::Class(Box::new(class)))
                }
                "async" if self.peek(1).is_ident("function") => {
                    self.bump();
                    let func = self.parse_function(false, true)?;
                    Ok(Expr::Func(Box::new(func)))
                }
                "import" if self.peek(1).is_punct(".") => {
                    self.bump();
                    self.expect_punct(".")?;
                    self.expect_word("meta")?;
                    Ok(Expr::ImportMeta)
                }
                "import" if self.peek(1).is_punct("(") => {
                    self.bump();
                    let args = self.parse_arguments()?;
                    let mut args = args;
                    match args.len() {
                        1 => Ok(Expr::ImportCall(Box::new(args.remove(0)))),
                        _ => self.err("`import()` takes exactly one argument"),
                    }
                }
                "yield" if self.generator_depth > 0 => {
                    self.bump();
                    let delegate = self.eat_punct("*");
                    let arg = if self.at_punct(";")
                        || self.at_punct(")")
                        || self.at_punct("]")
                        || self.at_punct("}")
                        || self.at_punct(",")
                        || self.at_eof()
                        || self.nl_before()
                    {
                        None
                    } else {
                        Some(Box::new(self.parse_assignment()?))
                    };
                    Ok(Expr::Yield { arg, delegate })
                }
                "await" => {
                    self.bump();
                    let arg = self.parse_unary()?;
                    Ok(Expr::Await(Box::new(arg)))
                }
                _ => {
                    if is_reserved(&word) {
                        return self.err(format!("unexpected keyword `{word}`"));
                    }
                    self.bump();
                    Ok(Expr::Ident(word))
                }
            },
            Tok::Punct(p) => match p.as_str() {
                "(" => {
                    self.bump();
                    let inner = self.parse_expression()?;
                    self.expect_punct(")")?;
                    Ok(Expr::Paren(Box::new(inner)))
                }
                "[" => self.parse_array_literal(),
                "{" => self.parse_object_literal(),
                _ => self.err(format!("unexpected token `{p}`")),
            },
            other => self.err(format!(
                "unexpected {} in expression position",
                other.describe()
            )),
        }
    }

    fn parse_new(&mut self) -> Result<Expr, ParseError> {
        self.bump(); // 'new'
        let callee = if self.at_ident("new") {
            self.parse_new()?
        } else {
            self.parse_primary()?
        };
        // Member accesses bind to the callee; a call would be the constructor
        // invocation, so stop before `(`.
        let mut callee = callee;
        loop {
            if self.at_punct(".") {
                self.bump();
                let name = match self.cur().clone() {
                    Tok::Ident(name) => {
                        self.bump();
                        name
                    }
                    other => {
                        return self.err(format!(
                            "expected a property name but found {}",
                            other.describe()
                        ))
                    }
                };
                callee = Expr::Member {
                    object: Box::new(callee),
                    prop: MemberProp::Name(name),
                    optional: false,
                };
            } else if self.at_punct("[") {
                self.bump();
                let index = self.parse_expression()?;
                self.expect_punct("]")?;
                callee = Expr::Member {
                    object: Box::new(callee),
                    prop: MemberProp::Index(Box::new(index)),
                    optional: false,
                };
            } else if self.ts && self.at_punct("<") {
                // `new Foo<T>()`
                if !self.try_skip_call_type_arguments()? {
                    break;
                }
            } else {
                break;
            }
        }
        let args = if self.at_punct("(") {
            self.parse_arguments()?
        } else {
            Vec::new()
        };
        Ok(Expr::New {
            callee: Box::new(callee),
            args,
        })
    }

    fn parse_array_literal(&mut self) -> Result<Expr, ParseError> {
        self.expect_punct("[")?;
        let mut elements = Vec::new();
        loop {
            if self.at_punct("]") {
                break;
            }
            if self.at_eof() {
                return self.err("unexpected end of input inside an array literal");
            }
            if self.at_punct(",") {
                self.bump();
                elements.push(None);
                continue;
            }
            if self.at_punct("...") {
                self.bump();
                let expr = self.parse_assignment()?;
                elements.push(Some(Expr::Spread(Box::new(expr))));
            } else {
                elements.push(Some(self.parse_assignment()?));
            }
            if self.at_punct("]") {
                break;
            }
            self.expect_punct(",")?;
            if self.at_punct("]") {
                break;
            }
        }
        self.expect_punct("]")?;
        Ok(Expr::Array(elements))
    }

    fn parse_object_literal(&mut self) -> Result<Expr, ParseError> {
        self.expect_punct("{")?;
        let mut props = Vec::new();
        while !self.at_punct("}") {
            if self.at_eof() {
                return self.err("unexpected end of input inside an object literal");
            }
            if self.at_punct("...") {
                self.bump();
                let expr = self.parse_assignment()?;
                props.push(Prop::Spread(expr));
            } else {
                let prop = self.parse_object_property()?;
                props.push(prop);
            }
            if !self.at_punct("}") {
                self.expect_punct(",")?;
            }
        }
        self.expect_punct("}")?;
        Ok(Expr::Object(props))
    }

    fn parse_object_property(&mut self) -> Result<Prop, ParseError> {
        let mut is_async = false;
        let mut accessor: Option<bool> = None;

        if self.at_ident("async") {
            let next = self.peek(1).clone();
            let methodish = match &next {
                Tok::Ident(_) | Tok::Str(_) | Tok::Num { .. } => true,
                Tok::Punct(p) => p == "[" || p == "*",
                _ => false,
            };
            if methodish {
                self.bump();
                is_async = true;
            }
        } else if self.at_ident("get") || self.at_ident("set") {
            let is_getter = self.at_ident("get");
            let next = self.peek(1).clone();
            let accessorish = match &next {
                Tok::Ident(_) | Tok::Str(_) | Tok::Num { .. } => true,
                Tok::Punct(p) => p == "[",
                _ => false,
            };
            if accessorish {
                self.bump();
                accessor = Some(is_getter);
            }
        }

        let is_generator = self.eat_punct("*");
        let key = self.parse_prop_key()?;

        if self.at_punct("(") {
            let params = self.parse_param_list(false)?;
            if self.ts && self.at_punct(":") {
                self.bump();
                self.skip_type(TypeContext::Return)?;
            }
            if is_generator {
                self.generator_depth += 1;
            }
            let body = self.parse_block()?;
            if is_generator {
                self.generator_depth -= 1;
            }
            let func = Box::new(Func {
                name: None,
                params,
                body: FuncBody::Block(body),
                is_async,
                is_generator,
                is_arrow: false,
                is_expression_body: false,
            });
            return Ok(match accessor {
                Some(is_getter) => Prop::Accessor {
                    key,
                    func,
                    is_getter,
                },
                None => Prop::Method { key, func },
            });
        }

        if accessor.is_some() {
            return self.err("`get` / `set` must be followed by a parameter list");
        }

        if self.eat_punct(":") {
            let value = self.parse_assignment()?;
            return Ok(Prop::KeyValue {
                key,
                value,
                shorthand: false,
            });
        }

        // Shorthand `{ a }` or `{ a = 1 }` (the latter only in patterns, but it
        // is accepted here and treated as a defaulted shorthand).
        let name = match &key {
            PropKey::Ident(name) => name.clone(),
            _ => return self.err("a property needs a value after `:`"),
        };
        if self.eat_punct("=") {
            let default = self.parse_assignment()?;
            return Ok(Prop::KeyValue {
                key,
                value: Expr::Assign {
                    op: AssignOp::Assign,
                    target: Box::new(Expr::Ident(name)),
                    value: Box::new(default),
                },
                shorthand: true,
            });
        }
        Ok(Prop::KeyValue {
            key,
            value: Expr::Ident(name),
            shorthand: true,
        })
    }
}

/// Consume leading `>` characters from a punctuator, updating the angle depth.
/// Returns the leftover text, which is `None` when nothing was consumed.
fn consume_leading_angle(punct: &str, depth: &mut i32) -> Option<String> {
    if !punct.starts_with('>') {
        return None;
    }
    let mut consumed = 0;
    for ch in punct.chars() {
        if ch == '>' && *depth > 0 {
            *depth -= 1;
            consumed += 1;
        } else {
            break;
        }
    }
    if consumed == 0 {
        return None;
    }
    Some(punct.chars().skip(consumed).collect())
}

fn depth_is_zero(paren: i32, bracket: i32, brace: i32, angle: i32) -> bool {
    paren == 0 && bracket == 0 && brace == 0 && angle == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(src: &str) -> Stmts {
        parse_script(src, Language::Ts)
            .unwrap_or_else(|e| panic!("TypeScript parse failed: {e}\n  source: {src}"))
            .stmts
    }

    fn js(src: &str) -> Stmts {
        parse_script(src, Language::Js)
            .unwrap_or_else(|e| panic!("JavaScript parse failed: {e}\n  source: {src}"))
            .stmts
    }

    fn ts_error(src: &str) -> ParseError {
        match parse_script(src, Language::Ts) {
            Ok(_) => panic!("expected a parse error, but this parsed fine: {src}"),
            Err(e) => e,
        }
    }

    fn dump(stmts: &Stmts) -> String {
        format!("{stmts:#?}")
    }

    /// The initialiser of a single-declarator `let` / `const` statement.
    fn init_of(stmt: &Stmt) -> &Expr {
        match stmt {
            Stmt::Var { decls, .. } => match &decls[0].1 {
                Some(init) => init,
                None => panic!("the declarator has no initialiser"),
            },
            other => panic!("expected a declaration, got {other:?}"),
        }
    }

    /// The central guarantee of native TypeScript support: a `.ts` program and
    /// the equivalent `.js` program must produce identical ASTs, because the
    /// types are erased rather than transformed.
    fn assert_same_as_js(ts_src: &str, js_src: &str) {
        assert_eq!(
            dump(&ts(ts_src)),
            dump(&js(js_src)),
            "\n  TypeScript: {ts_src}\n  JavaScript: {js_src}"
        );
    }

    // ------------------------------------------------------------- TS erasure

    #[test]
    fn type_annotations_are_erased_on_bindings() {
        assert_same_as_js("let x: number = 1;", "let x = 1;");
        assert_same_as_js("const s: string = 'a';", "const s = 'a';");
        assert_same_as_js("let arr: Array<number> = [];", "let arr = [];");
        assert_same_as_js("let t: [number, string] = q;", "let t = q;");
        assert_same_as_js("let u: string | null = null;", "let u = null;");
        assert_same_as_js("let o: { a: number; b: string } = p;", "let o = p;");
        assert_same_as_js("let v: (a: number) => void = f;", "let v = f;");
        // Nested generics end in a `>>` token, which has to be split.
        assert_same_as_js(
            "let m: Map<string, Array<number>> = new Map();",
            "let m = new Map();",
        );
        assert_same_as_js("let w: Array<Array<number>> = n;", "let w = n;");
        // No space before `=` after the closing `>`.
        assert_same_as_js("let z: Array<number>= [];", "let z = [];");
    }

    #[test]
    fn types_are_erased_in_parameters_and_return_positions() {
        assert_same_as_js(
            "function f(a: string, b?: number): void {}",
            "function f(a, b) {}",
        );
        assert_same_as_js(
            "function g(a: string = 'x'): number { return 1; }",
            "function g(a = 'x') { return 1; }",
        );
        assert_same_as_js(
            "function h({ a, b }: Opts = {}): void {}",
            "function h({ a, b } = {}) {}",
        );
        // A return type may itself be a function type.
        assert_same_as_js(
            "function pick(cb: () => number): () => void { return cb; }",
            "function pick(cb) { return cb; }",
        );
    }

    #[test]
    fn arrow_functions_keep_their_shape() {
        assert_same_as_js("const f = (): void => {};", "const f = () => {};");
        assert_same_as_js("const g = (a: number): number => a;", "const g = (a) => a;");
        assert_same_as_js("const h = (a: number = 1) => a;", "const h = (a = 1) => a;");
        assert_same_as_js(
            "const i = async (a: number): Promise<number> => a;",
            "const i = async (a) => a;",
        );
        assert_same_as_js("const j = (a: number, ...rest: string[]) => a;", "const j = (a, ...rest) => a;");
    }

    #[test]
    fn generic_declarations_are_erased() {
        assert_same_as_js(
            "function id<T>(x: T): T { return x; }",
            "function id(x) { return x; }",
        );
        assert_same_as_js("class Box<T> { value!: T; }", "class Box { value; }");
        assert_same_as_js("const id = <T,>(x: T) => x;", "const id = (x) => x;");
        assert_same_as_js("const id = <T extends object>(x: T) => x;", "const id = (x) => x;");
    }

    #[test]
    fn casts_and_assertions_are_erased() {
        assert_same_as_js("let a = v as string;", "let a = v;");
        assert_same_as_js("let a = v as unknown as T;", "let a = v;");
        assert_same_as_js("let a = arr as const;", "let a = arr;");
        assert_same_as_js("let a = v satisfies T;", "let a = v;");
        assert_same_as_js("let a = v!.w;", "let a = v.w;");
        assert_same_as_js("let a = v!.w!();", "let a = v.w();");
    }

    #[test]
    fn interfaces_and_type_aliases_produce_nothing() {
        let stmts = ts("interface A { x: number }\nlet y = 1;");
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Empty));
        assert!(matches!(&stmts[1], Stmt::Var { .. }));

        let stmts = ts("type B = { a: string } | number;\nlet z = 2;");
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Empty));
    }

    #[test]
    fn multiline_type_alias_is_fully_skipped() {
        let stmts = ts("type Shape =\n  | { kind: 'a' }\n  | { kind: 'b' };\nlet s = 1;");
        assert_eq!(stmts.len(), 2);
        assert!(matches!(&stmts[0], Stmt::Empty));
        assert!(matches!(&stmts[1], Stmt::Var { .. }));
    }

    #[test]
    fn type_only_imports_and_exports_are_erased() {
        let stmts = ts(
            "import type { X } from 'm';\nexport type { Y };\nexport type Z = number;\nlet a = 1;",
        );
        assert_eq!(stmts.len(), 4);
        for index in 0..3 {
            assert!(
                matches!(&stmts[index], Stmt::Empty),
                "statement {index} should have been erased"
            );
        }
        assert!(matches!(&stmts[3], Stmt::Var { .. }));
    }

    #[test]
    fn inline_type_import_specifiers_are_erased() {
        let stmts = ts("import { type A, b, type C as D } from 'm';");
        match &stmts[0] {
            Stmt::Import(decl) => {
                assert_eq!(decl.specifiers.len(), 1, "only the value import survives");
                match &decl.specifiers[0] {
                    ImportSpecifier::Named { imported, local } => {
                        assert_eq!(imported, "b");
                        assert_eq!(local, "b");
                    }
                    other => panic!("unexpected specifier: {other:?}"),
                }
            }
            other => panic!("expected an import, got {other:?}"),
        }
    }

    #[test]
    fn declare_statements_produce_nothing() {
        let stmts = ts(
            "declare const x: number;\ndeclare module 'm' { export const a: number; }\ndeclare global { interface W {} }\nlet r = 1;",
        );
        assert_eq!(stmts.len(), 4);
        for index in 0..3 {
            assert!(matches!(&stmts[index], Stmt::Empty));
        }
        assert!(matches!(&stmts[3], Stmt::Var { .. }));
    }

    // ------------------------------------------------------ TS runtime meaning

    #[test]
    fn enum_is_kept_because_it_has_runtime_meaning() {
        let stmts = ts("enum E { A, B = 3, C = 'x' }");
        match &stmts[0] {
            Stmt::Enum { name, members } => {
                assert_eq!(name, "E");
                assert_eq!(members.len(), 3);
                assert_eq!(members[0].0, "A");
                assert!(members[0].1.is_none());
                assert_eq!(members[1].0, "B");
                assert!(matches!(&members[1].1, Some(Expr::Num(n)) if *n == 3.0));
                assert_eq!(members[2].0, "C");
                assert!(matches!(&members[2].1, Some(Expr::Str(s)) if s == "x"));
            }
            other => panic!("expected an enum statement, got {other:?}"),
        }
    }

    #[test]
    fn const_and_exported_enums_are_accepted() {
        let stmts = ts("const enum F { X }");
        assert_eq!(stmts.len(), 1);
        assert!(matches!(&stmts[0], Stmt::Enum { .. }));

        let stmts = ts("export enum G { A }");
        match &stmts[0] {
            Stmt::Export(ExportDecl::Decl(inner)) => {
                assert!(matches!(&**inner, Stmt::Enum { .. }))
            }
            other => panic!("expected an exported enum, got {other:?}"),
        }
    }

    #[test]
    fn constructor_parameter_properties_become_assignments() {
        let stmts = ts(
            "class C { constructor(private readonly x: number, public y: string) { this.z = 1; } }",
        );
        let class = match &stmts[0] {
            Stmt::Class(class) => class,
            other => panic!("expected a class, got {other:?}"),
        };
        let ctor = match &class.members[0] {
            ClassMember::Method { func, kind, .. } => {
                assert_eq!(*kind, MethodKind::Constructor);
                func
            }
            other => panic!("expected a constructor, got {other:?}"),
        };
        let body = match &ctor.body {
            FuncBody::Block(body) => body,
            other => panic!("expected a block body, got {other:?}"),
        };
        assert_eq!(
            body.len(),
            3,
            "two injected property assignments plus the original statement"
        );
        for (index, name) in ["x", "y"].into_iter().enumerate() {
            match &body[index] {
                Stmt::Expr(Expr::Assign { target, value, .. }) => {
                    match (&**target, &**value) {
                        (
                            Expr::Member {
                                object,
                                prop: MemberProp::Name(field),
                                ..
                            },
                            Expr::Ident(source),
                        ) => {
                            assert!(matches!(&**object, Expr::This));
                            assert_eq!(field.as_str(), name);
                            assert_eq!(source.as_str(), name);
                        }
                        other => panic!("unexpected assignment shape: {other:?}"),
                    }
                }
                other => panic!("expected an injected assignment, got {other:?}"),
            }
        }
    }

    #[test]
    fn class_modifiers_and_static_members() {
        let stmts = ts(
            "class A { private x: number = 1; static y = 2; public m(): void {} readonly z!: string; }",
        );
        let class = match &stmts[0] {
            Stmt::Class(class) => class,
            other => panic!("expected a class, got {other:?}"),
        };
        assert_eq!(class.members.len(), 4);
        assert!(matches!(
            &class.members[0],
            ClassMember::Field {
                is_static: false,
                ..
            }
        ));
        assert!(matches!(
            &class.members[1],
            ClassMember::Field { is_static: true, .. }
        ));
        assert!(matches!(
            &class.members[2],
            ClassMember::Method {
                is_static: false,
                ..
            }
        ));
        assert!(matches!(&class.members[3], ClassMember::Field { .. }));
    }

    #[test]
    fn static_blocks_are_kept() {
        let stmts = ts("class A { static { setup(); } }");
        match &stmts[0] {
            Stmt::Class(class) => {
                assert!(matches!(&class.members[0], ClassMember::StaticBlock(_)))
            }
            other => panic!("expected a class, got {other:?}"),
        }
    }

    #[test]
    fn overload_signatures_do_not_break_declarations() {
        let stmts = ts(
            "function f(a: number): number;\nfunction f(a: string): string;\nfunction f(a: any): any { return a; }",
        );
        assert_eq!(stmts.len(), 3);
        for index in 0..2 {
            match &stmts[index] {
                Stmt::Func(func) => {
                    assert!(matches!(&func.body, FuncBody::Block(body) if body.is_empty()))
                }
                other => panic!("expected a function, got {other:?}"),
            }
        }
        match &stmts[2] {
            Stmt::Func(func) => {
                assert!(matches!(&func.body, FuncBody::Block(body) if !body.is_empty()))
            }
            other => panic!("expected the implementation, got {other:?}"),
        }
    }

    #[test]
    fn decorators_are_dropped_with_a_warning() {
        let parsed = parse_script("@sealed\nclass A { @log m() {} }", Language::Ts)
            .expect("decorated code should still parse");
        assert!(
            !parsed.warnings.is_empty(),
            "silently dropping decorators would be a trap, so a warning is required"
        );
        assert!(parsed.warnings.iter().all(|w| w.contains("decorator")));
        match &parsed.stmts[0] {
            Stmt::Class(class) => assert_eq!(class.members.len(), 1),
            other => panic!("expected a class, got {other:?}"),
        }
    }

    #[test]
    fn class_generics_and_implements_are_erased() {
        assert_same_as_js(
            "class A<T> extends B<T> implements C<D>, E { m(): T { return this.v as T; } }",
            "class A extends B { m() { return this.v; } }",
        );
    }

    // ------------------------------------------------------- general JS shapes

    #[test]
    fn call_site_type_arguments_match_typescript() {
        let stmts = ts("f<number>(1);");
        match &stmts[0] {
            Stmt::Expr(Expr::Call { callee, args, .. }) => {
                assert!(matches!(&**callee, Expr::Ident(name) if name == "f"));
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected a call, got {other:?}"),
        }

        // TypeScript reads `a < b > (c)` as a call with a type argument, and so
        // does rust4ts. JavaScript mode keeps it a comparison.
        assert!(matches!(
            &ts("a < b > (c);")[0],
            Stmt::Expr(Expr::Call { .. })
        ));
        assert!(matches!(
            &js("a < b > (c);")[0],
            Stmt::Expr(Expr::Binary {
                op: BinOp::Gt,
                ..
            })
        ));
    }

    #[test]
    fn optional_chaining_respects_parentheses() {
        match &ts("a?.b.c;")[0] {
            Stmt::Expr(Expr::Member {
                object,
                prop: MemberProp::Name(name),
                optional: false,
            }) => {
                assert_eq!(name, "c");
                assert!(matches!(
                    &**object,
                    Expr::Member {
                        optional: true,
                        ..
                    }
                ));
            }
            other => panic!("expected a member access, got {other:?}"),
        }

        // Parentheses break the chain: the outer access must not short-circuit.
        match &ts("(a?.b).c;")[0] {
            Stmt::Expr(Expr::Member { object, .. }) => {
                assert!(matches!(&**object, Expr::Paren(inner)
                    if matches!(&**inner, Expr::Member { optional: true, .. })));
            }
            other => panic!("expected a member access, got {other:?}"),
        }
    }

    #[test]
    fn destructuring_patterns() {
        let stmts = ts("let { a, b: c = 2, ...rest } = o;\nlet [x, , y, ...tail] = arr;");
        match &stmts[0] {
            Stmt::Var { decls, .. } => match &decls[0].0 {
                Pattern::Object { props, rest } => {
                    assert_eq!(props.len(), 2);
                    assert!(matches!(&props[0].value, Pattern::Ident(n) if n == "a"));
                    assert!(matches!(&props[1].value, Pattern::Default(..)));
                    assert!(matches!(rest.as_deref(), Some(Pattern::Ident(n)) if n == "rest"));
                }
                other => panic!("expected an object pattern, got {other:?}"),
            },
            other => panic!("expected a declaration, got {other:?}"),
        }
        match &stmts[1] {
            Stmt::Var { decls, .. } => match &decls[0].0 {
                Pattern::Array { elements, rest } => {
                    assert_eq!(elements.len(), 3);
                    assert!(elements[1].is_none(), "the elision is a hole");
                    assert!(matches!(rest.as_deref(), Some(Pattern::Ident(n)) if n == "tail"));
                }
                other => panic!("expected an array pattern, got {other:?}"),
            },
            other => panic!("expected a declaration, got {other:?}"),
        }
    }

    #[test]
    fn control_flow_statements() {
        let stmts = ts(
            "for (let i = 0; i < 3; i++) { }\nfor (const [k, v] of pairs) { }\nfor (const k in obj) { }\nwhile (a) { }\ndo { } while (b);\nswitch (x) { case 1: break; default: break; }\ntry { } catch (e) { } finally { }\nlabel: for (;;) { break label; }",
        );
        assert_eq!(stmts.len(), 8);
        assert!(matches!(&stmts[0], Stmt::For { .. }));
        assert!(matches!(&stmts[1], Stmt::ForOf { .. }));
        assert!(matches!(&stmts[2], Stmt::ForIn { .. }));
        assert!(matches!(&stmts[3], Stmt::While { .. }));
        assert!(matches!(&stmts[4], Stmt::DoWhile { .. }));
        assert!(matches!(&stmts[5], Stmt::Switch { cases, .. } if cases.len() == 2));
        assert!(matches!(&stmts[6], Stmt::Try { handler: Some(_), finalizer: Some(_), .. }));
        assert!(matches!(&stmts[7], Stmt::Labeled { .. }));
    }

    #[test]
    fn module_declarations() {
        let stmts = ts(
            "import a, { b as c } from \"m\";\nimport * as ns from \"n\";\nimport \"side\";\nexport { x as y };\nexport * from \"m\";\nexport * as z from \"m\";\nexport default 42;\nexport const q = 1;",
        );
        assert_eq!(stmts.len(), 8);
        match &stmts[0] {
            Stmt::Import(decl) => {
                assert_eq!(decl.source, "m");
                assert_eq!(decl.specifiers.len(), 2);
                assert!(matches!(&decl.specifiers[0], ImportSpecifier::Default(n) if n == "a"));
                assert!(matches!(
                    &decl.specifiers[1],
                    ImportSpecifier::Named { imported, local } if imported == "b" && local == "c"
                ));
            }
            other => panic!("expected an import, got {other:?}"),
        }
        assert!(matches!(&stmts[2], Stmt::Import(decl) if decl.specifiers.is_empty()));
        assert!(matches!(
            &stmts[3],
            Stmt::Export(ExportDecl::Named { specifiers, source: None })
                if specifiers.len() == 1 && specifiers[0].exported == "y"
        ));
        assert!(matches!(
            &stmts[4],
            Stmt::Export(ExportDecl::All { alias: None, .. })
        ));
        assert!(matches!(
            &stmts[5],
            Stmt::Export(ExportDecl::All { alias: Some(a), .. }) if a == "z"
        ));
        assert!(matches!(&stmts[6], Stmt::Export(ExportDecl::Default(_))));
        assert!(matches!(&stmts[7], Stmt::Export(ExportDecl::Decl(_))));
    }

    #[test]
    fn automatic_semicolon_insertion() {
        assert_eq!(ts("let a = 1\nlet b = 2").len(), 2);
        assert_eq!(ts("a()\nb()").len(), 2);
        assert_eq!(ts("x = 1\ny = 2").len(), 2);

        // `return` followed by a line break returns nothing.
        let stmts = ts("function f() { return\n1 }");
        match &stmts[0] {
            Stmt::Func(func) => match &func.body {
                FuncBody::Block(body) => {
                    assert!(matches!(&body[0], Stmt::Return(None)));
                    assert!(matches!(&body[1], Stmt::Expr(Expr::Num(_))));
                }
                other => panic!("expected a block body, got {other:?}"),
            },
            other => panic!("expected a function, got {other:?}"),
        }
    }

    #[test]
    fn template_literals_and_regex_survive_parsing() {
        let stmts = ts("let t = `a${b}${c + 1}d`;\nlet r = /ab+/gi;\nlet d = a / b;");
        assert_eq!(stmts.len(), 3);
        match init_of(&stmts[0]) {
            Expr::Template { quasis, exprs } => {
                assert_eq!(
                    quasis,
                    &vec!["a".to_string(), "".to_string(), "d".to_string()]
                );
                assert_eq!(exprs.len(), 2);
            }
            other => panic!("expected a template, got {other:?}"),
        }
        match init_of(&stmts[1]) {
            Expr::Regex { source, flags } => {
                assert_eq!(source, "ab+");
                assert_eq!(flags, "gi");
            }
            other => panic!("expected a regex literal, got {other:?}"),
        }
        match init_of(&stmts[2]) {
            Expr::Binary { op: BinOp::Div, .. } => {}
            other => panic!("expected a division, got {other:?}"),
        }
    }

    #[test]
    fn declarations_keep_initialisers_out_of_the_pattern() {
        // `=` in a declaration introduces the initialiser, not a destructuring
        // default; conflating the two silently changes the AST.
        match &ts("let x = 1;")[0] {
            Stmt::Var { decls, .. } => {
                assert!(matches!(&decls[0].0, Pattern::Ident(n) if n == "x"));
                assert!(matches!(&decls[0].1, Some(Expr::Num(n)) if *n == 1.0));
            }
            other => panic!("expected a declaration, got {other:?}"),
        }
        // Inside a pattern, `=` still is a default.
        match &ts("let { a = 1 } = o;")[0] {
            Stmt::Var { decls, .. } => match &decls[0].0 {
                Pattern::Object { props, .. } => {
                    assert!(matches!(&props[0].value, Pattern::Default(..)))
                }
                other => panic!("expected an object pattern, got {other:?}"),
            },
            other => panic!("expected a declaration, got {other:?}"),
        }
    }

    #[test]
    fn a_realistic_plugin_file_parses() {
        let source = r#"
import { Logger } from "./log.ts";
export interface Config { name: string; retries?: number }
type Outcome = "ok" | "fail";

const DEFAULTS: Config = { name: "plugin", retries: 3 };

export enum Status { Idle, Running = 10, Done }

export default class Plugin {
  private readonly log: Logger;
  private status: Status = Status.Idle;

  constructor(private config: Config = DEFAULTS) {
    this.log = new Logger(config.name);
  }

  get current(): Status { return this.status; }

  async run(times: number = this.config.retries ?? 1): Promise<Outcome> {
    for (let i = 0; i < times; i++) {
      try {
        const items: number[] = [1, 2, 3].map((n) => n * i);
        this.log.info(`tick ${i}: ${items.join(", ")}`);
      } catch (e) {
        return "fail";
      } finally {
        this.status = Status.Running;
      }
    }
    return "ok";
  }
}
"#;
        let stmts = ts(source);
        assert_eq!(
            stmts.len(),
            6,
            "import, the erased interface, the erased type alias, the const, the exported enum and the exported class"
        );
        assert!(matches!(&stmts[0], Stmt::Import(_)));
        assert!(matches!(&stmts[1], Stmt::Empty), "the interface is erased");
        assert!(matches!(&stmts[2], Stmt::Empty), "the type alias is erased");
        assert!(matches!(&stmts[3], Stmt::Var { .. }), "the const survives");
        assert!(matches!(
            &stmts[4],
            Stmt::Export(ExportDecl::Decl(inner)) if matches!(&**inner, Stmt::Enum { .. })
        ));
        assert!(matches!(
            &stmts[5],
            Stmt::Export(ExportDecl::Default(inner)) if matches!(&**inner, Stmt::Class(_))
        ));
    }

    // ------------------------------------------------------------------ errors

    #[test]
    fn javascript_mode_rejects_type_syntax() {
        let err = parse_script("let x: number = 1;", Language::Js).unwrap_err();
        assert!(
            err.msg.contains("expected `;`"),
            "unexpected message: {}",
            err.msg
        );
    }

    #[test]
    fn namespaces_are_reported_as_unsupported() {
        let err = ts_error("namespace N { }");
        assert!(err.msg.contains("namespace"), "unexpected message: {}", err.msg);
    }

    #[test]
    fn errors_carry_a_line_and_column() {
        let err = ts_error("let x = 1;\nlet y = ;");
        assert_eq!(err.line, 2);
        assert!(err.col > 1);
    }

    #[test]
    fn unbalanced_delimiters_are_reported() {
        let err = ts_error("function f( {");
        assert!(err.line >= 1);
        assert!(!err.msg.is_empty());
    }
}
