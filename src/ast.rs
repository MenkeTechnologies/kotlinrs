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
    Char,
    String,
    Unit,
    /// A heap object — a class instance, `List`, `Map`, or `Pair`. Carries no
    /// class identity in the coarse type (that rides in the compiler's binding
    /// table, [`crate::compiler`]); it exists so `==` routes to structural
    /// equality and display routes through the object stringifier.
    Obj,
    Unknown,
}

impl Type {
    /// Integral kinds for op selection. `Char` is included because it is backed
    /// by an integer code unit at runtime, so `==`/comparisons on it use the
    /// numeric ops; only its *display* and add/sub result type differ.
    pub fn is_int(self) -> bool {
        matches!(self, Type::Int | Type::Long | Type::Char)
    }
    pub fn is_num(self) -> bool {
        matches!(
            self,
            Type::Int | Type::Long | Type::Double | Type::Char | Type::Unknown
        )
    }
    /// Parse a type annotation name (`Int`, `Double`, `String`, …).
    pub fn from_name(s: &str) -> Type {
        match s {
            "Int" => Type::Int,
            "Long" => Type::Long,
            "Double" | "Float" => Type::Double,
            "Boolean" => Type::Boolean,
            "Char" => Type::Char,
            "String" => Type::String,
            "Unit" => Type::Unit,
            _ => Type::Unknown,
        }
    }
}

/// A whole compilation unit: the top-level `class`/`object` declarations and
/// free `fun`s. Execution still enters `fun main`.
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub classes: Vec<ClassDecl>,
    pub funs: Vec<FunDecl>,
}

/// A function parameter, or a primary-constructor property parameter. `class`
/// carries the referenced class name when the annotation names a user class
/// (the coarse [`Type`] can't hold it), so the compiler can dispatch methods on
/// a parameter of class type.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    /// The class name when `ty == Type::Obj` and the annotation named a user
    /// class (e.g. `p: Person`).
    pub class: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FunDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
    /// The return class name when `ret == Type::Obj` and it named a user class.
    pub ret_class: Option<String>,
    pub body: Vec<Stmt>,
    pub line: u32,
}

/// Whether a primary-constructor parameter also declares a stored property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropKind {
    /// `val x` — a read-only stored property.
    Val,
    /// `var x` — a mutable stored property.
    Var,
    /// A plain constructor parameter (`x`) — not stored as a property.
    None,
}

/// A `class` / `data class` / `object` declaration.
///
/// A regular class's primary constructor lists [`Param`]s; those marked
/// `val`/`var` ([`PropKind`]) become stored properties. `data` classes
/// additionally get synthesized `equals`/`hashCode`/`toString`/`copy`/
/// `componentN`. An `object` has no constructor — its `props` carry
/// initializer expressions and are built once into a singleton.
#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
    /// Primary-constructor parameters (empty for an `object`).
    pub params: Vec<CtorProp>,
    /// `object` singleton properties with initializer expressions.
    pub obj_props: Vec<(String, Type, Option<String>, Expr)>,
    pub methods: Vec<FunDecl>,
    pub is_data: bool,
    pub is_object: bool,
    pub line: u32,
}

/// A primary-constructor parameter with its property kind.
#[derive(Debug, Clone)]
pub struct CtorProp {
    pub name: String,
    pub ty: Type,
    pub class: Option<String>,
    pub kind: PropKind,
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
    /// `receiver.field (op)= value` — a property write on an object.
    SetMember {
        recv: Expr,
        name: String,
        op: Option<BinOp>,
        value: Expr,
    },
    /// `receiver[index] (op)= value` — an indexed write on a list/map.
    SetIndex {
        recv: Expr,
        index: Expr,
        op: Option<BinOp>,
        value: Expr,
    },
    /// `val (a, b, …) = expr` — destructuring via `componentN`.
    Destructure {
        names: Vec<String>,
        init: Expr,
    },
    Return(Option<Expr>),
    While {
        cond: Expr,
        body: Vec<Stmt>,
        /// Optional loop label (`outer@ while (…)`) for `break@outer`.
        label: Option<String>,
    },
    /// `for (v in start..end)` / `until` / `downTo`, optional `step`.
    For {
        var: String,
        start: Expr,
        end: Expr,
        kind: RangeKind,
        step: Option<Expr>,
        body: Vec<Stmt>,
        /// Optional loop label (`outer@ for (…)`) for `break@outer`.
        label: Option<String>,
    },
    /// `break` / `break@label` — jump past the (labeled) enclosing loop.
    Break(Option<String>),
    /// `continue` / `continue@label` — jump to the (labeled) enclosing loop's
    /// next-iteration point.
    Continue(Option<String>),
    /// A bare `if` used as a statement (its value, if any, is discarded).
    If(IfExpr),
    /// A `when` used as a statement (its value, if any, is discarded).
    When(WhenExpr),
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

/// A `when` — subject form (`when (x) { … }`) or subjectless (`when { … }`).
/// Usable as an expression (its matched arm's value) or a statement (value
/// discarded). Arms are tested top-to-bottom; the first match wins.
#[derive(Debug, Clone)]
pub struct WhenExpr {
    /// The subject in `when (subject) { … }`; `None` for the subjectless form,
    /// where each arm guard is a standalone boolean expression.
    pub subject: Option<Box<Expr>>,
    pub arms: Vec<WhenArm>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct WhenArm {
    pub guard: WhenGuard,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum WhenGuard {
    /// `else -> …` — the fallthrough arm.
    Else,
    /// One or more comma-separated conditions; the arm matches if *any* holds.
    Conds(Vec<WhenCond>),
}

#[derive(Debug, Clone)]
pub enum WhenCond {
    /// Subject form: `subject == expr`. Subjectless form: a boolean `expr`.
    Expr(Expr),
    /// `in a..b` (or `!in …`) — subject-form range membership.
    InRange {
        negated: bool,
        start: Expr,
        end: Expr,
        kind: RangeKind,
    },
    /// `is Type` (or `!is …`) — subject-form runtime type check.
    Is { negated: bool, ty: String },
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
    /// A `Char` literal, carrying its UTF-16 code unit.
    Char(i64),
    /// The `null` literal.
    Null,
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
    /// `safe` marks a safe access `receiver?.property` (short-circuits to null).
    Member {
        recv: Box<Expr>,
        name: String,
        safe: bool,
        line: u32,
    },
    /// Method call on a receiver: `receiver.method(args)` (e.g.
    /// `"s".uppercase()`, `42.toString()`). Chainable via nested receivers.
    /// `safe` marks a safe call `receiver?.method(args)`.
    MethodCall {
        recv: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        safe: bool,
        line: u32,
    },
    /// Elvis `left ?: right` — `right` when `left` is null, else `left`.
    Elvis {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// Not-null assertion `expr!!` — throws NPE when `expr` is null.
    NotNull(Box<Expr>),
    /// Indexed access `receiver[index]` — list element or map value.
    Index {
        recv: Box<Expr>,
        index: Box<Expr>,
        line: u32,
    },
    /// `first to second` — a `Pair`.
    Pair {
        first: Box<Expr>,
        second: Box<Expr>,
    },
    /// A lambda `{ params -> body }`. First-class only inside the collection
    /// higher-order calls (`map`/`filter`/`forEach`), where the compiler inlines
    /// the body; used elsewhere it is a compile error.
    Lambda {
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    /// `if` used as an expression (each branch's last statement is its value).
    If(IfExpr),
    /// `when` used as an expression (the matched arm's value).
    When(WhenExpr),
}

#[derive(Debug, Clone)]
pub enum StrExpr {
    Text(String),
    Expr(Box<Expr>),
}
