//! Token stream for the Kotlin frontend.
//!
//! String literals carry their interpolation structure pre-split into
//! [`StrPart`]s so the parser never re-scans the raw source: `"a${x}b"`
//! lexes to a single [`Tok::Str`] holding `[Text("a"), Expr("x"), Text("b")]`,
//! and the parser sub-parses each `Expr` source fragment on demand.

/// One piece of a string literal: a literal run or an interpolated expression
/// (the raw Kotlin source between `${…}` or after a bare `$`).
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Text(String),
    Expr(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // Literals
    Int(i64),
    Float(f64),
    Str(Vec<StrPart>),
    Bool(bool),
    /// A `Char` literal (`'A'`), carrying its UTF-16 code unit as an integer.
    Char(i64),
    Ident(String),

    // Keywords
    Fun,
    Val,
    Var,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Until,
    DownTo,
    Step,
    When,
    Is,
    Break,
    Continue,
    Null,
    Class,
    Object,
    Data,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket, // [
    RBracket, // ]
    Comma,
    Colon,
    Semi,
    Dot,
    DotDot,   // ..
    Arrow,    // ->
    At,       // @  (loop labels: `outer@ for (...)`, `break@outer`)
    Question, // ?  (safe-call `?.`, elvis `?:`, nullable type `T?`)

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,    // =
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    EqEq,      // ==
    NotEq,     // !=
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd, // &&
    OrOr,   // ||
    Not,    // !

    Eof,
}

/// A token plus the 1-based source line it started on, for `kotlin: <reason>`
/// diagnostics and fusevm line attribution.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned {
    pub tok: Tok,
    pub line: u32,
}
