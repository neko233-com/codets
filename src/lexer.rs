//! Hand-written tokenizer shared by JavaScript and TypeScript.
//!
//! TypeScript adds no new tokens: every TS-only construct (type annotations,
//! generics, `interface`, `as`, `!`) is made of ordinary punctuators and
//! identifiers, so a single tokenizer serves both languages. The parser is
//! responsible for recognising and erasing the type-only parts.
//!
//! Two JavaScript ambiguities are resolved here:
//!
//! * **`/` as regex or division** — decided from the previous significant
//!   token: a `/` in operand position starts a regular expression.
//! * **`}` closing a block or an object literal** — decided by a small brace
//!   stack, which only matters for the following token (see above).

use std::fmt;

/// A single lexical token.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// Numeric literal, pre-parsed to `f64`, plus its raw source text.
    Num { value: f64, raw: String },
    /// BigInt literal; raw digits without the trailing `n` (may keep a `0x`/`0b`/`0o` prefix).
    BigInt(String),
    /// String literal with escapes resolved.
    Str(String),
    /// Template literal containing no substitution.
    TemplateNoSub(String),
    /// `` `head${ `` — the text before the first substitution.
    TemplateHead(String),
    /// `` }middle${ `` — the text between two substitutions.
    TemplateMiddle(String),
    /// `` }tail` `` — the text after the last substitution.
    TemplateTail(String),
    /// Regular expression literal.
    Regex { source: String, flags: String },
    /// Identifier or keyword (keywords are not distinguished lexically).
    Ident(String),
    /// Operator or punctuator, canonical text (`===`, `=>`, `...`, ...).
    Punct(String),
    /// End of input.
    Eof,
}

impl Tok {
    /// Human-readable description used in parser error messages.
    pub fn describe(&self) -> String {
        match self {
            Tok::Num { raw, .. } => format!("number `{raw}`"),
            Tok::BigInt(d) => format!("bigint `{d}n`"),
            Tok::Str(_) => "string literal".to_string(),
            Tok::TemplateNoSub(_)
            | Tok::TemplateHead(_)
            | Tok::TemplateMiddle(_)
            | Tok::TemplateTail(_) => "template literal".to_string(),
            Tok::Regex { source, flags } => format!("regular expression `/{source}/{flags}`"),
            Tok::Ident(name) => format!("`{name}`"),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Eof => "end of input".to_string(),
        }
    }

    /// True when this token is the punctuator `p`.
    pub fn is_punct(&self, p: &str) -> bool {
        matches!(self, Tok::Punct(x) if x == p)
    }

    /// True when this token is the identifier (or keyword) `name`.
    pub fn is_ident(&self, name: &str) -> bool {
        matches!(self, Tok::Ident(x) if x == name)
    }

    /// The identifier text, if this token is an identifier or keyword.
    pub fn ident_text(&self) -> Option<&str> {
        match self {
            Tok::Ident(name) => Some(name),
            _ => None,
        }
    }
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// A token together with its source location.
#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
    /// True when a line terminator appeared between the previous token and this one.
    /// Used for automatic semicolon insertion.
    pub nl_before: bool,
}

/// A lexical error, carrying the location where scanning failed.
#[derive(Clone, Debug, PartialEq)]
pub struct LexError {
    pub msg: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for LexError {}

/// What kind of construct a pending `{` opened, used only to decide whether a
/// following `/` is division (after an object literal) or a regex (after a block).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BraceCtx {
    Block,
    Object,
    Template,
}

/// Operand-position keywords: a `/` directly after one of these starts a regex.
fn word_allows_regex(word: &str) -> bool {
    matches!(
        word,
        "return"
            | "typeof"
            | "instanceof"
            | "in"
            | "of"
            | "new"
            | "delete"
            | "void"
            | "do"
            | "else"
            | "case"
            | "yield"
            | "await"
            | "throw"
            | "var"
            | "let"
            | "const"
            | "function"
            | "class"
            | "if"
            | "while"
            | "for"
            | "switch"
            | "catch"
            | "finally"
            | "try"
            | "export"
            | "import"
            | "default"
            | "extends"
    )
}

/// True when `tok` can be the last token of an expression. When it cannot, the
/// next `/` must start a regular expression rather than a division.
fn ends_expr(tok: &Tok) -> bool {
    match tok {
        Tok::Num { .. }
        | Tok::BigInt(_)
        | Tok::Str(_)
        | Tok::TemplateNoSub(_)
        | Tok::TemplateTail(_)
        | Tok::Regex { .. } => true,
        Tok::TemplateHead(_) | Tok::TemplateMiddle(_) => false,
        Tok::Ident(word) => !word_allows_regex(word),
        Tok::Punct(p) => matches!(p.as_str(), ")" | "]" | "}" | "++" | "--"),
        Tok::Eof => false,
    }
}

/// True for characters that may start an identifier.
pub fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic() || c == '\u{200C}' || c == '\u{200D}'
}

/// True for characters that may continue an identifier.
pub fn is_ident_part(c: char) -> bool {
    is_ident_start(c) || c.is_numeric()
}

/// Punctuators and operators, ordered longest-first so that scanning can take
/// the first match.
const PUNCTS: &[&str] = &[
    ">>>=", "===", "!==", "**=", "<<=", ">>=", "&&=", "||=", "??=", "...", ">>>", "=>", "==", "!=",
    "<=", ">=", "<<", ">>", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=",
    "|=", "^=", "**", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&",
    "|", "^", "!", "~", "?", ":", "=", ".", "@", "#",
];

/// The tokenizer.
pub struct Lexer {
    src: Vec<char>,
    pos: usize,
    line: u32,
    col_start: usize,
    braces: Vec<BraceCtx>,
    /// True when an operand is expected next, which is what makes `/` a regex.
    expect_operand: bool,
    /// Set when the last `}` scanned closed an object literal rather than a block.
    last_close_object: bool,
}

impl Lexer {
    /// Create a tokenizer over `src`. A leading `#!` hashbang line is skipped.
    pub fn new(src: &str) -> Self {
        let chars: Vec<char> = src.chars().collect();
        let mut pos = 0;
        if chars.first() == Some(&'#') && chars.get(1) == Some(&'!') {
            while pos < chars.len() && chars[pos] != '\n' && chars[pos] != '\r' {
                pos += 1;
            }
        }
        Self {
            src: chars,
            pos,
            line: 1,
            col_start: 0,
            braces: Vec::new(),
            expect_operand: true,
            last_close_object: false,
        }
    }

    /// Tokenize the whole input, ending with a [`Tok::Eof`] token.
    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut out = Vec::new();
        loop {
            let token = self.next_token()?;
            let done = token.tok == Tok::Eof;
            out.push(token);
            if done {
                return Ok(out);
            }
        }
    }

    /// Produce the next token.
    pub fn next_token(&mut self) -> Result<Token, LexError> {
        let nl_before = self.skip_trivia()?;
        let line = self.line;
        let col = self.col();
        let tok = self.scan_token()?;
        let mut ends = ends_expr(&tok);
        if matches!(&tok, Tok::Punct(p) if p == "}") {
            ends = self.last_close_object;
        }
        self.expect_operand = !ends;
        Ok(Token {
            tok,
            line,
            col,
            nl_before,
        })
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.src.get(self.pos + offset).copied()
    }

    fn col(&self) -> u32 {
        (self.pos - self.col_start) as u32 + 1
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, LexError> {
        Err(LexError {
            msg: msg.into(),
            line: self.line,
            col: self.col(),
        })
    }

    fn error_at(&self, line: u32, col: u32, msg: impl Into<String>) -> LexError {
        LexError {
            msg: msg.into(),
            line,
            col,
        }
    }

    /// Consume one character, keeping line/column tracking in sync.
    /// A `\r\n` pair is consumed as a single line break.
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        match c {
            '\n' => {
                self.line += 1;
                self.col_start = self.pos;
            }
            '\r' => {
                if self.peek() == Some('\n') {
                    self.pos += 1;
                }
                self.line += 1;
                self.col_start = self.pos;
            }
            _ => {}
        }
        Some(c)
    }

    /// Skip whitespace and comments, reporting whether a line break was seen.
    fn skip_trivia(&mut self) -> Result<bool, LexError> {
        let mut saw_newline = false;
        loop {
            match self.peek() {
                Some('\n') | Some('\r') => {
                    saw_newline = true;
                    self.bump();
                }
                Some(c) if c.is_whitespace() || c == '\u{FEFF}' => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' || c == '\r' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    let line = self.line;
                    let col = self.col();
                    self.bump();
                    self.bump();
                    let mut closed = false;
                    while let Some(c) = self.bump() {
                        if c == '\n' || c == '\r' {
                            saw_newline = true;
                        }
                        if c == '*' && self.peek() == Some('/') {
                            self.bump();
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err(self.error_at(line, col, "unterminated block comment"));
                    }
                }
                _ => return Ok(saw_newline),
            }
        }
    }

    fn scan_token(&mut self) -> Result<Tok, LexError> {
        let c = match self.peek() {
            Some(c) => c,
            None => return Ok(Tok::Eof),
        };
        match c {
            '`' => self.scan_template_start(),
            '}' if self.braces.last() == Some(&BraceCtx::Template) => {
                self.braces.pop();
                self.bump();
                self.scan_template_continue()
            }
            '\'' | '"' => self.scan_string(c),
            _ if c.is_ascii_digit() => self.scan_number(),
            '.' if self.peek_at(1).is_some_and(|d| d.is_ascii_digit()) => self.scan_number(),
            '#' if self.peek_at(1).is_some_and(is_ident_start) => Ok(self.scan_ident()),
            _ if is_ident_start(c) => Ok(self.scan_ident()),
            '/' if self.expect_operand => self.scan_regex(),
            _ => self.scan_punct(),
        }
    }

    fn scan_ident(&mut self) -> Tok {
        let mut text = String::new();
        if self.peek() == Some('#') {
            text.push('#');
            self.bump();
        }
        while let Some(c) = self.peek() {
            if is_ident_part(c) {
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Tok::Ident(text)
    }

    fn scan_number(&mut self) -> Result<Tok, LexError> {
        let start = self.pos;
        let mut radix = 10u32;
        if self.peek() == Some('0') {
            match self.peek_at(1) {
                Some('x') | Some('X') => {
                    radix = 16;
                    self.bump();
                    self.bump();
                }
                Some('b') | Some('B') => {
                    radix = 2;
                    self.bump();
                    self.bump();
                }
                Some('o') | Some('O') => {
                    radix = 8;
                    self.bump();
                    self.bump();
                }
                _ => {}
            }
        }

        let mut seen_digit = false;
        if radix == 10 {
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    seen_digit = true;
                    self.bump();
                } else if c == '_' {
                    self.bump();
                } else {
                    break;
                }
            }
            if self.peek() == Some('.') {
                self.bump();
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        seen_digit = true;
                        self.bump();
                    } else if c == '_' {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            if matches!(self.peek(), Some('e') | Some('E')) {
                let line = self.line;
                let col = self.col();
                self.bump();
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.bump();
                }
                let mut exponent_digit = false;
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        exponent_digit = true;
                        self.bump();
                    } else if c == '_' {
                        self.bump();
                    } else {
                        break;
                    }
                }
                if !exponent_digit {
                    return Err(self.error_at(line, col, "missing digits in exponent"));
                }
            }
        } else {
            while let Some(c) = self.peek() {
                if c.is_digit(radix) {
                    seen_digit = true;
                    self.bump();
                } else if c == '_' {
                    self.bump();
                } else {
                    break;
                }
            }
        }

        let is_bigint = self.peek() == Some('n');
        if is_bigint {
            self.bump();
        }

        if !seen_digit {
            return self.err("malformed numeric literal");
        }

        let raw: String = self.src[start..self.pos]
            .iter()
            .copied()
            .filter(|c| *c != '_')
            .collect();

        if is_bigint {
            let digits = raw.trim_end_matches('n').to_string();
            return Ok(Tok::BigInt(digits));
        }

        let value = if radix == 10 {
            raw.parse::<f64>().ok()
        } else {
            u64::from_str_radix(&raw[2..], radix).ok().map(|v| v as f64)
        };
        match value {
            Some(value) => Ok(Tok::Num { value, raw }),
            None => self.err(format!("numeric literal out of range: `{raw}`")),
        }
    }

    fn scan_string(&mut self, quote: char) -> Result<Tok, LexError> {
        let line = self.line;
        let col = self.col();
        self.bump();
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some('\n') | Some('\r') => {
                    return Err(self.error_at(line, col, "unterminated string literal"))
                }
                Some(c) if c == quote => {
                    self.bump();
                    return Ok(Tok::Str(out));
                }
                Some('\\') => {
                    self.bump();
                    let escaped = self.read_escape()?;
                    out.push_str(&escaped);
                }
                Some(c) => {
                    self.bump();
                    out.push(c);
                }
            }
        }
    }

    /// Read the escape sequence following a backslash. Returns the text the
    /// escape denotes (empty for a line continuation).
    fn read_escape(&mut self) -> Result<String, LexError> {
        let c = match self.bump() {
            Some(c) => c,
            None => return self.err("unterminated escape sequence"),
        };
        let text = match c {
            'n' => "\n".to_string(),
            't' => "\t".to_string(),
            'r' => "\r".to_string(),
            'b' => "\u{8}".to_string(),
            'f' => "\u{c}".to_string(),
            'v' => "\u{b}".to_string(),
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => "\0".to_string(),
            '\n' | '\r' => String::new(),
            'x' => {
                let mut hex = String::new();
                for _ in 0..2 {
                    match self.bump() {
                        Some(h) if h.is_ascii_hexdigit() => hex.push(h),
                        _ => return self.err("invalid hexadecimal escape sequence"),
                    }
                }
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => ch.to_string(),
                    None => return self.err("invalid hexadecimal escape sequence"),
                }
            }
            'u' => {
                if self.peek() == Some('{') {
                    self.bump();
                    let mut hex = String::new();
                    while let Some(h) = self.peek() {
                        if h.is_ascii_hexdigit() {
                            hex.push(h);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    if self.peek() != Some('}') || hex.is_empty() {
                        return self.err("invalid unicode escape sequence");
                    }
                    self.bump();
                    match u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        Some(ch) => ch.to_string(),
                        None => return self.err("invalid unicode escape sequence"),
                    }
                } else {
                    let mut hex = String::new();
                    for _ in 0..4 {
                        match self.bump() {
                            Some(h) if h.is_ascii_hexdigit() => hex.push(h),
                            _ => return self.err("invalid unicode escape sequence"),
                        }
                    }
                    // Lone surrogates cannot be represented in a Rust `String`;
                    // they are replaced rather than rejected.
                    let code = u32::from_str_radix(&hex, 16).unwrap_or(0);
                    match char::from_u32(code) {
                        Some(ch) => ch.to_string(),
                        None => "\u{FFFD}".to_string(),
                    }
                }
            }
            '1'..='9' => return self.err("octal escape sequences are not allowed"),
            other => other.to_string(),
        };
        Ok(text)
    }

    fn scan_template_start(&mut self) -> Result<Tok, LexError> {
        let line = self.line;
        let col = self.col();
        self.bump();
        let mut cooked = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error_at(line, col, "unterminated template literal")),
                Some('`') => {
                    self.bump();
                    return Ok(Tok::TemplateNoSub(cooked));
                }
                Some('$') if self.peek_at(1) == Some('{') => {
                    self.bump();
                    self.bump();
                    self.braces.push(BraceCtx::Template);
                    return Ok(Tok::TemplateHead(cooked));
                }
                Some('\\') => {
                    self.bump();
                    let escaped = self.read_escape()?;
                    cooked.push_str(&escaped);
                }
                Some('\r') => {
                    self.bump();
                    cooked.push('\n');
                }
                Some(c) => {
                    self.bump();
                    cooked.push(c);
                }
            }
        }
    }

    /// Continue a template literal right after the `}` that closed a substitution.
    fn scan_template_continue(&mut self) -> Result<Tok, LexError> {
        let mut cooked = String::new();
        loop {
            match self.peek() {
                None => return self.err("unterminated template literal"),
                Some('`') => {
                    self.bump();
                    return Ok(Tok::TemplateTail(cooked));
                }
                Some('$') if self.peek_at(1) == Some('{') => {
                    self.bump();
                    self.bump();
                    self.braces.push(BraceCtx::Template);
                    return Ok(Tok::TemplateMiddle(cooked));
                }
                Some('\\') => {
                    self.bump();
                    let escaped = self.read_escape()?;
                    cooked.push_str(&escaped);
                }
                Some('\r') => {
                    self.bump();
                    cooked.push('\n');
                }
                Some(c) => {
                    self.bump();
                    cooked.push(c);
                }
            }
        }
    }

    fn scan_regex(&mut self) -> Result<Tok, LexError> {
        let line = self.line;
        let col = self.col();
        self.bump();
        let mut source = String::new();
        let mut in_class = false;
        loop {
            match self.peek() {
                None | Some('\n') | Some('\r') => {
                    return Err(self.error_at(line, col, "unterminated regular expression"))
                }
                Some('\\') => {
                    self.bump();
                    let escaped = match self.peek() {
                        Some(c) => c,
                        None => {
                            return Err(
                                self.error_at(line, col, "unterminated regular expression")
                            )
                        }
                    };
                    source.push('\\');
                    source.push(escaped);
                    self.bump();
                }
                Some('[') => {
                    in_class = true;
                    source.push('[');
                    self.bump();
                }
                Some(']') => {
                    in_class = false;
                    source.push(']');
                    self.bump();
                }
                Some('/') if !in_class => {
                    self.bump();
                    break;
                }
                Some(c) => {
                    source.push(c);
                    self.bump();
                }
            }
        }
        let mut flags = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() {
                flags.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Ok(Tok::Regex { source, flags })
    }

    fn scan_punct(&mut self) -> Result<Tok, LexError> {
        // `?.` is not optional chaining when followed by a digit: `a?.5:b`.
        if self.peek() == Some('?')
            && self.peek_at(1) == Some('.')
            && self.peek_at(2).is_some_and(|d| d.is_ascii_digit())
        {
            self.bump();
            return Ok(Tok::Punct("?".to_string()));
        }

        for punct in PUNCTS {
            let len = punct.chars().count();
            let matches_here = punct
                .chars()
                .enumerate()
                .all(|(i, pc)| self.peek_at(i) == Some(pc));
            if !matches_here {
                continue;
            }
            for _ in 0..len {
                self.bump();
            }
            match *punct {
                "{" => {
                    self.braces.push(if self.expect_operand {
                        BraceCtx::Object
                    } else {
                        BraceCtx::Block
                    });
                }
                "}" => {
                    self.last_close_object = self.braces.pop() == Some(BraceCtx::Object);
                }
                _ => {}
            }
            return Ok(Tok::Punct((*punct).to_string()));
        }

        self.err(format!("unexpected character `{}`", self.peek().unwrap_or(' ')))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        Lexer::new(src)
            .tokenize()
            .expect("lexing should succeed")
            .into_iter()
            .map(|t| t.tok)
            .collect()
    }

    fn only(src: &str) -> Tok {
        let mut t = toks(src);
        assert_eq!(t.len(), 2, "expected one token, got {t:?}");
        t.remove(0)
    }

    fn num(src: &str) -> f64 {
        match only(src) {
            Tok::Num { value, .. } => value,
            other => panic!("expected number, got {other:?}"),
        }
    }

    #[test]
    fn numeric_literals() {
        assert_eq!(num("0"), 0.0);
        assert_eq!(num("42"), 42.0);
        assert_eq!(num("0x1F"), 31.0);
        assert_eq!(num("0b1010"), 10.0);
        assert_eq!(num("0o17"), 15.0);
        assert_eq!(num("1_000_000"), 1_000_000.0);
        assert_eq!(num("1.5e3"), 1500.0);
        assert_eq!(num("2e-3"), 0.002);
        assert_eq!(num(".5"), 0.5);
    }

    #[test]
    fn bigint_literals() {
        assert_eq!(only("10n"), Tok::BigInt("10".into()));
        assert_eq!(only("0xFFn"), Tok::BigInt("0xFF".into()));
    }

    #[test]
    fn string_escapes() {
        assert_eq!(only(r#"'a\nb'"#), Tok::Str("a\nb".into()));
        assert_eq!(only(r#""\u0041""#), Tok::Str("A".into()));
        assert_eq!(only(r#"'\\'"#), Tok::Str("\\".into()));
        assert_eq!(only(r#"'a\
b'"#), Tok::Str("ab".into()));
    }

    #[test]
    fn template_literals() {
        assert_eq!(only("`plain`"), Tok::TemplateNoSub("plain".into()));
        assert_eq!(
            toks("`a${b}c`"),
            vec![
                Tok::TemplateHead("a".into()),
                Tok::Ident("b".into()),
                Tok::TemplateTail("c".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn nested_templates_keep_brace_depth() {
        let got = toks("`x${ f({a:`y${z}`}) }w`");
        assert_eq!(
            got,
            vec![
                Tok::TemplateHead("x".into()),
                Tok::Ident("f".into()),
                Tok::Punct("(".into()),
                Tok::Punct("{".into()),
                Tok::Ident("a".into()),
                Tok::Punct(":".into()),
                Tok::TemplateHead("y".into()),
                Tok::Ident("z".into()),
                Tok::TemplateTail(String::new()),
                Tok::Punct("}".into()),
                Tok::Punct(")".into()),
                Tok::TemplateTail("w".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn two_substitutions_in_one_template() {
        let got = toks("`${a}${b}`");
        assert_eq!(
            got,
            vec![
                Tok::TemplateHead(String::new()),
                Tok::Ident("a".into()),
                Tok::TemplateMiddle(String::new()),
                Tok::Ident("b".into()),
                Tok::TemplateTail(String::new()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn regex_versus_division() {
        // Division after an operand.
        assert_eq!(
            toks("a / b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Punct("/".into()),
                Tok::Ident("b".into()),
                Tok::Eof,
            ]
        );
        // Regex in operand position.
        assert_eq!(
            only("/ab+/gi"),
            Tok::Regex {
                source: "ab+".into(),
                flags: "gi".into()
            }
        );
        // Regex right after `=` and after `(`.
        assert!(toks("x = /re/").contains(&Tok::Regex {
            source: "re".into(),
            flags: String::new()
        }));
        assert!(toks("(/re/)").contains(&Tok::Regex {
            source: "re".into(),
            flags: String::new()
        }));
        // Regex after a keyword that cannot end an expression.
        assert!(toks("return /re/;").contains(&Tok::Regex {
            source: "re".into(),
            flags: String::new()
        }));
        // Division after an object literal `}`.
        assert_eq!(
            toks("x = {a:1} / 2")[7],
            Tok::Punct("/".into()),
            "expected division after object literal"
        );
        // Regex after a block `}`.
        assert!(toks("if (a) {} /re/.test(x)").contains(&Tok::Regex {
            source: "re".into(),
            flags: String::new()
        }));
        // A char class may contain `/`.
        assert_eq!(
            only("/[/]/"),
            Tok::Regex {
                source: "[/]".into(),
                flags: String::new()
            }
        );
    }

    #[test]
    fn longest_punctuator_match() {
        assert_eq!(toks("a>>>=b")[1], Tok::Punct(">>>=".into()));
        assert_eq!(toks("a===b")[1], Tok::Punct("===".into()));
        assert_eq!(toks("a!==b")[1], Tok::Punct("!==".into()));
        assert_eq!(toks("...a")[0], Tok::Punct("...".into()));
        assert_eq!(toks("a??=b")[1], Tok::Punct("??=".into()));
        assert_eq!(toks("a?.b")[1], Tok::Punct("?.".into()));
        // `?.` followed by a digit is a conditional, not optional chaining.
        assert_eq!(
            toks("a?.5:b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Punct("?".into()),
                Tok::Num {
                    value: 0.5,
                    raw: ".5".into()
                },
                Tok::Punct(":".into()),
                Tok::Ident("b".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn newline_flag_drives_asi() {
        let stream = Lexer::new("let a = 1\nlet b = 2").tokenize().unwrap();
        let lets: Vec<&Token> = stream
            .iter()
            .filter(|t| t.tok.is_ident("let"))
            .collect();
        assert_eq!(lets.len(), 2);
        assert!(!lets[0].nl_before);
        // The line break sits between `1` and the second `let`, which is exactly
        // where automatic semicolon insertion has to look.
        assert!(
            lets[1].nl_before,
            "the second `let` should be flagged as newline-preceded"
        );
        // `b` follows a space, not a line break.
        let b_token = stream.iter().find(|t| t.tok.is_ident("b")).unwrap();
        assert!(!b_token.nl_before);
    }

    #[test]
    fn comments_and_hashbang() {
        assert_eq!(
            toks("#!/usr/bin/env rust2ts\n1 // c\n/* d */ + 2"),
            vec![
                Tok::Num {
                    value: 1.0,
                    raw: "1".into()
                },
                Tok::Punct("+".into()),
                Tok::Num {
                    value: 2.0,
                    raw: "2".into()
                },
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn private_names_and_unicode_idents() {
        assert_eq!(toks("this.#x")[2], Tok::Ident("#x".into()));
        assert_eq!(only("变量"), Tok::Ident("变量".into()));
        assert_eq!(only("$x_1"), Tok::Ident("$x_1".into()));
    }

    #[test]
    fn errors_report_location() {
        let err = Lexer::new("let s = 'abc").tokenize().unwrap_err();
        assert_eq!(err.line, 1);
        assert_eq!(err.col, 9);
        assert!(err.msg.contains("unterminated string"));

        let err = Lexer::new("/* nope").tokenize().unwrap_err();
        assert!(err.msg.contains("unterminated block comment"));

        let err = Lexer::new("`abc").tokenize().unwrap_err();
        assert!(err.msg.contains("unterminated template"));
    }

    #[test]
    fn line_and_column_tracking() {
        let stream = Lexer::new("a\n  bc").tokenize().unwrap();
        let bc = stream.iter().find(|t| t.tok.is_ident("bc")).unwrap();
        assert_eq!((bc.line, bc.col), (2, 3));
    }

    #[test]
    fn type_annotation_tokens_are_ordinary_tokens() {
        // TypeScript syntax needs no special tokenizer support.
        assert_eq!(
            toks("let x: Array<number> = []"),
            vec![
                Tok::Ident("let".into()),
                Tok::Ident("x".into()),
                Tok::Punct(":".into()),
                Tok::Ident("Array".into()),
                Tok::Punct("<".into()),
                Tok::Ident("number".into()),
                Tok::Punct(">".into()),
                Tok::Punct("=".into()),
                Tok::Punct("[".into()),
                Tok::Punct("]".into()),
                Tok::Eof,
            ]
        );
    }
}
