//! Abstract syntax tree for the supported Kotlin subset.
//!
//! A program is a list of top-level `fun` declarations; execution enters
//! `fun main`. Types are tracked coarsely (see [`Type`]) so the compiler can
//! pick `+` vs string concat and integer vs float division at lowering time.

/// Coarse static type. `Unknown` is the join for anything the frontend can't
/// resolve without a full type checker; it lowers conservatively (numeric ops
/// default to `Int` behavior, `+` to arithmetic unless a `String` is present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Long,
    Double,
    Boolean,
    String,
    Unit,
    Unknown,
}

impl Type {
    pub fn is_int(self) -> bool {
        matches!(self, Type::Int | Type::Long)
    }
    pub fn is_num(self) -> bool {
        matches!(self, Type::Int | Type::Long | Type::Double | Type::Unknown)
    }
    /// Parse a type annotation name (`Int`, `Double`, `String`, …).
    pub fn from_name(s: &str) -> Type {
        match s {
            "Int" => Type::Int,
            "Long" => Type::Long,
            "Double" | "Float" => Type::Double,
            "Boolean" => Type::Boolean,
            "String" => Type::String,
            "Unit" => Type::Unit,
            _ => Type::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FunDecl {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
    pub body: Vec<Stmt>,
    pub line: u32,
}

/// A statement plus the 1-based source line it started on. The line drives
/// `--dap` breakpoints/stepping (the compiler emits a per-statement debug marker
/// carrying it) and terse diagnostics.
#[derive(Debug, Clone)]
pub struct Stmt {
    pub line: u32,
    pub kind: StmtKind,
}

impl Stmt {
    pub fn new(line: u32, kind: StmtKind) -> Stmt {
        Stmt { line, kind }
    }
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    /// `val`/`var` binding. `mutable` distinguishes `var`.
    Let {
        name: String,
        ty: Option<Type>,
        init: Expr,
        mutable: bool,
    },
    /// `name (op)= value`, where `op` is `None` for a plain `=`.
    Assign {
        name: String,
        op: Option<BinOp>,
        value: Expr,
    },
    Return(Option<Expr>),
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// `for (v in start..end)` / `until` / `downTo`, optional `step`.
    For {
        var: String,
        start: Expr,
        end: Expr,
        kind: RangeKind,
        step: Option<Expr>,
        body: Vec<Stmt>,
    },
    /// A bare `if` used as a statement (its value, if any, is discarded).
    If(IfExpr),
    /// An expression evaluated for effect (e.g. a `println(...)` call).
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    /// `a..b` inclusive, ascending.
    Inclusive,
    /// `a until b`, ascending, excludes `b`.
    Until,
    /// `a downTo b`, descending, inclusive.
    DownTo,
}

#[derive(Debug, Clone)]
pub struct IfExpr {
    pub cond: Box<Expr>,
    pub then: Vec<Stmt>,
    pub els: Option<Vec<Stmt>>,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// String literal, split into literal runs and interpolated sub-expressions.
    Str(Vec<StrExpr>),
    Var(String),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    /// A call: `println(x)`, `print(x)`, or a user function `f(a, b)`.
    Call {
        name: String,
        args: Vec<Expr>,
        line: u32,
    },
    /// Property access on a receiver: `receiver.property` (e.g. `"s".length`).
    Member {
        recv: Box<Expr>,
        name: String,
        line: u32,
    },
    /// Method call on a receiver: `receiver.method(args)` (e.g.
    /// `"s".uppercase()`, `42.toString()`). Chainable via nested receivers.
    MethodCall {
        recv: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        line: u32,
    },
    /// `if` used as an expression (each branch's last statement is its value).
    If(IfExpr),
}

#[derive(Debug, Clone)]
pub enum StrExpr {
    Text(String),
    Expr(Box<Expr>),
}
