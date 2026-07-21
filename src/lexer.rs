//! Hand-written Kotlin lexer.
//!
//! Produces [`Spanned`] tokens. Comments (`//`, `/* … */`, nested) are skipped.
//! String literals are scanned into [`StrPart`]s (see [`token`](crate::token)):
//! `$ident` and `${expr}` interpolations are split out here so the parser stays
//! a pure token consumer.

use crate::token::{Spanned, StrPart, Tok};

/// Byte length of the UTF-8 sequence whose leading byte is `b`.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
        }
    }

    /// Tokenize the whole source. Returns `Err(msg)` on an unterminated string
    /// or an unexpected byte.
    pub fn tokenize(mut self) -> Result<Vec<Spanned>, String> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.pos >= self.src.len() {
                out.push(Spanned {
                    tok: Tok::Eof,
                    line: self.line,
                });
                return Ok(out);
            }
            let line = self.line;
            let tok = self.next_token()?;
            out.push(Spanned { tok, line });
        }
    }

    fn peek(&self) -> u8 {
        *self.src.get(self.pos).unwrap_or(&0)
    }
    fn peek2(&self) -> u8 {
        *self.src.get(self.pos + 1).unwrap_or(&0)
    }
    fn bump(&mut self) -> u8 {
        let c = self.peek();
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
        }
        c
    }

    fn skip_trivia(&mut self) -> Result<(), String> {
        loop {
            match self.peek() {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    self.bump();
                }
                b'/' if self.peek2() == b'/' => {
                    while self.pos < self.src.len() && self.peek() != b'\n' {
                        self.bump();
                    }
                }
                b'/' if self.peek2() == b'*' => {
                    self.bump();
                    self.bump();
                    let mut depth = 1;
                    while depth > 0 {
                        if self.pos >= self.src.len() {
                            return Err("unterminated block comment".into());
                        }
                        if self.peek() == b'/' && self.peek2() == b'*' {
                            self.bump();
                            self.bump();
                            depth += 1;
                        } else if self.peek() == b'*' && self.peek2() == b'/' {
                            self.bump();
                            self.bump();
                            depth -= 1;
                        } else {
                            self.bump();
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<Tok, String> {
        let c = self.peek();
        match c {
            b'0'..=b'9' => Ok(self.number()),
            b'"' => self.string(),
            b'\'' => self.char_literal(),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => Ok(self.ident_or_keyword()),
            _ => self.operator(),
        }
    }

    /// A `Char` literal `'A'` — a single UTF-16 unit (or escape). Kotlin `Char`
    /// is integral; the code unit is carried in [`Tok::Char`] and lowers to a
    /// plain integer at runtime, statically typed `Char` so display and `Char`
    /// arithmetic stay faithful.
    fn char_literal(&mut self) -> Result<Tok, String> {
        self.bump(); // opening '
        if self.pos >= self.src.len() {
            return Err("unterminated char literal".into());
        }
        let ch = if self.peek() == b'\\' {
            self.bump();
            let e = self.bump();
            match e {
                b'n' => '\n',
                b't' => '\t',
                b'r' => '\r',
                b'\\' => '\\',
                b'\'' => '\'',
                b'"' => '"',
                b'$' => '$',
                b'0' => '\0',
                b'b' => '\u{8}',
                b'u' => {
                    // `\uXXXX` — exactly four hex digits.
                    let mut code: u32 = 0;
                    for _ in 0..4 {
                        let h = self.bump();
                        let d = (h as char)
                            .to_digit(16)
                            .ok_or("invalid `\\u` escape in char literal")?;
                        code = code * 16 + d;
                    }
                    char::from_u32(code).ok_or("invalid unicode scalar in char literal")?
                }
                other => other as char,
            }
        } else {
            // A UTF-8 encoded scalar. Decode from the raw bytes so multi-byte
            // characters (`'é'`) lex to a single Char.
            let start = self.pos;
            let first = self.peek();
            let len = utf8_len(first);
            for _ in 0..len {
                self.bump();
            }
            std::str::from_utf8(&self.src[start..self.pos])
                .ok()
                .and_then(|s| s.chars().next())
                .ok_or("invalid char literal")?
        };
        if self.peek() != b'\'' {
            return Err("unterminated char literal (expected closing `'`)".into());
        }
        self.bump(); // closing '
        Ok(Tok::Char(ch as i64))
    }

    fn number(&mut self) -> Tok {
        let start = self.pos;
        while self.peek().is_ascii_digit() || self.peek() == b'_' {
            self.bump();
        }
        let mut is_float = false;
        // A `.` is a decimal point only when followed by a digit — otherwise it
        // is the range operator `..` (e.g. `1..10`).
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_float = true;
            self.bump();
            while self.peek().is_ascii_digit() || self.peek() == b'_' {
                self.bump();
            }
        }
        if matches!(self.peek(), b'e' | b'E') {
            is_float = true;
            self.bump();
            if matches!(self.peek(), b'+' | b'-') {
                self.bump();
            }
            while self.peek().is_ascii_digit() {
                self.bump();
            }
        }
        let raw: String = std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .chars()
            .filter(|&ch| ch != '_')
            .collect();
        // A trailing `L` (Long) or `f`/`F` (Float) suffix.
        let suffix = self.peek();
        if suffix == b'L' {
            self.bump();
            return Tok::Int(raw.parse().unwrap_or(0));
        }
        if matches!(suffix, b'f' | b'F') {
            self.bump();
            return Tok::Float(raw.parse().unwrap_or(0.0));
        }
        if is_float {
            Tok::Float(raw.parse().unwrap_or(0.0))
        } else {
            Tok::Int(raw.parse().unwrap_or(0))
        }
    }

    fn ident_or_keyword(&mut self) -> Tok {
        let start = self.pos;
        while matches!(self.peek(), b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_') {
            self.bump();
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
        match s {
            "fun" => Tok::Fun,
            "val" => Tok::Val,
            "var" => Tok::Var,
            "if" => Tok::If,
            "else" => Tok::Else,
            "while" => Tok::While,
            "for" => Tok::For,
            "in" => Tok::In,
            "return" => Tok::Return,
            "until" => Tok::Until,
            "downTo" => Tok::DownTo,
            "step" => Tok::Step,
            "when" => Tok::When,
            "is" => Tok::Is,
            "break" => Tok::Break,
            "continue" => Tok::Continue,
            "null" => Tok::Null,
            "true" => Tok::Bool(true),
            "false" => Tok::Bool(false),
            _ => Tok::Ident(s.to_string()),
        }
    }

    fn string(&mut self) -> Result<Tok, String> {
        self.bump(); // opening "
        let mut parts: Vec<StrPart> = Vec::new();
        let mut cur = String::new();
        loop {
            if self.pos >= self.src.len() {
                return Err("unterminated string literal".into());
            }
            let c = self.peek();
            match c {
                b'"' => {
                    self.bump();
                    break;
                }
                b'\\' => {
                    self.bump();
                    let e = self.bump();
                    cur.push(match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'\\' => '\\',
                        b'"' => '"',
                        b'$' => '$',
                        b'0' => '\0',
                        other => other as char,
                    });
                }
                b'$' => {
                    // Flush the pending literal run before the interpolation.
                    if !cur.is_empty() {
                        parts.push(StrPart::Text(std::mem::take(&mut cur)));
                    }
                    self.bump();
                    if self.peek() == b'{' {
                        self.bump();
                        let mut depth = 1;
                        let estart = self.pos;
                        while depth > 0 {
                            if self.pos >= self.src.len() {
                                return Err("unterminated `${` in string".into());
                            }
                            match self.peek() {
                                b'{' => depth += 1,
                                b'}' => depth -= 1,
                                _ => {}
                            }
                            if depth == 0 {
                                break;
                            }
                            self.bump();
                        }
                        let expr = std::str::from_utf8(&self.src[estart..self.pos])
                            .unwrap()
                            .to_string();
                        self.bump(); // closing }
                        parts.push(StrPart::Expr(expr));
                    } else {
                        // Bare `$name` — a simple identifier reference.
                        let estart = self.pos;
                        while matches!(self.peek(), b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
                        {
                            self.bump();
                        }
                        let expr = std::str::from_utf8(&self.src[estart..self.pos])
                            .unwrap()
                            .to_string();
                        if expr.is_empty() {
                            cur.push('$');
                        } else {
                            parts.push(StrPart::Expr(expr));
                        }
                    }
                }
                _ => {
                    cur.push(self.bump() as char);
                }
            }
        }
        if !cur.is_empty() || parts.is_empty() {
            parts.push(StrPart::Text(cur));
        }
        Ok(Tok::Str(parts))
    }

    fn operator(&mut self) -> Result<Tok, String> {
        let c = self.bump();
        let d = self.peek();
        let two = |lx: &mut Self, t: Tok| {
            lx.bump();
            t
        };
        Ok(match (c, d) {
            (b'+', b'=') => two(self, Tok::PlusEq),
            (b'-', b'=') => two(self, Tok::MinusEq),
            (b'*', b'=') => two(self, Tok::StarEq),
            (b'/', b'=') => two(self, Tok::SlashEq),
            (b'%', b'=') => two(self, Tok::PercentEq),
            (b'=', b'=') => two(self, Tok::EqEq),
            (b'!', b'=') => two(self, Tok::NotEq),
            (b'<', b'=') => two(self, Tok::Le),
            (b'>', b'=') => two(self, Tok::Ge),
            (b'&', b'&') => two(self, Tok::AndAnd),
            (b'|', b'|') => two(self, Tok::OrOr),
            (b'-', b'>') => two(self, Tok::Arrow),
            (b'.', b'.') => two(self, Tok::DotDot),
            (b'+', _) => Tok::Plus,
            (b'-', _) => Tok::Minus,
            (b'*', _) => Tok::Star,
            (b'/', _) => Tok::Slash,
            (b'%', _) => Tok::Percent,
            (b'=', _) => Tok::Assign,
            (b'<', _) => Tok::Lt,
            (b'>', _) => Tok::Gt,
            (b'!', _) => Tok::Not,
            (b'(', _) => Tok::LParen,
            (b')', _) => Tok::RParen,
            (b'{', _) => Tok::LBrace,
            (b'}', _) => Tok::RBrace,
            (b',', _) => Tok::Comma,
            (b':', _) => Tok::Colon,
            (b';', _) => Tok::Semi,
            (b'.', _) => Tok::Dot,
            (b'@', _) => Tok::At,
            (b'?', _) => Tok::Question,
            (other, _) => {
                return Err(format!("unexpected character '{}'", other as char));
            }
        })
    }
}
