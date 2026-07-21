//! Recursive-descent parser for the Kotlin subset.
//!
//! Grammar (informal):
//! ```text
//! program  := funDecl*
//! funDecl  := 'fun' IDENT '(' params? ')' (':' TYPE)? (block | '=' expr)
//! block    := '{' stmt* '}'
//! stmt     := letDecl | 'return' expr? | 'while' '(' expr ')' block
//!           | 'for' '(' IDENT 'in' range ')' block | ifStmt | assign | exprStmt
//! expr     := or
//! or       := and ('||' and)*
//! and      := eq  ('&&' eq)*
//! eq       := cmp (('=='|'!=') cmp)*
//! cmp      := add (('<'|'>'|'<='|'>=') add)*
//! add      := mul (('+'|'-') mul)*
//! mul      := unary (('*'|'/'|'%') unary)*
//! unary    := ('-'|'!') unary | postfix
//! postfix  := primary ('.' IDENT ('(' args? ')')?)*
//! primary  := INT | FLOAT | STRING | BOOL | ifExpr | call | IDENT | '(' expr ')'
//! ```

use crate::ast::*;
use crate::lexer::Lexer;
use crate::token::{Spanned, StrPart, Tok};

pub struct Parser {
    toks: Vec<Spanned>,
    pos: usize,
}

/// Parse a full program (top-level `fun` declarations).
pub fn parse_program(src: &str) -> Result<Vec<FunDecl>, String> {
    let toks = Lexer::new(src).tokenize()?;
    let mut p = Parser { toks, pos: 0 };
    let mut funs = Vec::new();
    while !p.at(&Tok::Eof) {
        funs.push(p.fun_decl()?);
    }
    Ok(funs)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    /// The token `n` positions ahead (clamped to `Eof` past the end).
    fn peek_at(&self, n: usize) -> &Tok {
        self.toks
            .get(self.pos + n)
            .map(|s| &s.tok)
            .unwrap_or(&Tok::Eof)
    }
    fn line(&self) -> u32 {
        self.toks[self.pos].line
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> Result<(), String> {
        if self.at(t) {
            self.bump();
            Ok(())
        } else {
            Err(format!(
                "expected {:?}, found {:?} (line {})",
                t,
                self.peek(),
                self.line()
            ))
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("expected identifier, found {:?}", other)),
        }
    }

    // ── Declarations ───────────────────────────────────────────────

    fn fun_decl(&mut self) -> Result<FunDecl, String> {
        let line = self.line();
        self.eat(&Tok::Fun)?;
        let name = self.ident()?;
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            let pname = self.ident()?;
            let ty = if self.at(&Tok::Colon) {
                self.bump();
                self.type_name()?
            } else {
                Type::Unknown
            };
            params.push((pname, ty));
            if self.at(&Tok::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.eat(&Tok::RParen)?;
        let ret_annot = if self.at(&Tok::Colon) {
            self.bump();
            Some(self.type_name()?)
        } else {
            None
        };
        // Body is either a block `{ … }` or a single-expression body `= expr`
        // (Kotlin `fun f(...) = expr`), which desugars to `{ return expr }`.
        let (body, is_expr_body) = if self.at(&Tok::Assign) {
            self.bump();
            let e = self.expr()?;
            (vec![Stmt::new(line, StmtKind::Return(Some(e)))], true)
        } else {
            (self.block()?, false)
        };
        // With no explicit return annotation, a block body defaults to `Unit`
        // (its value is discarded); an `= expr` body's type is the expression's,
        // which the frontend doesn't fully infer — leave it `Unknown` so callers
        // lower conservatively rather than being forced to `Unit`.
        let ret = match ret_annot {
            Some(t) => t,
            None if is_expr_body => Type::Unknown,
            None => Type::Unit,
        };
        Ok(FunDecl {
            name,
            params,
            ret,
            body,
            line,
        })
    }

    /// A type reference — `Int`, `String`, `Array<String>`, `Int?`, … Generic
    /// args are consumed but ignored (coarse typing), and a trailing `?`
    /// (nullable) is accepted and discarded — nullability is tracked at the
    /// value/flow level, not in the coarse static type.
    fn type_name(&mut self) -> Result<Type, String> {
        let name = self.ident()?;
        if self.at(&Tok::Lt) {
            let mut depth = 0;
            loop {
                match self.bump() {
                    Tok::Lt => depth += 1,
                    Tok::Gt => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Eof => return Err("unterminated type argument list".into()),
                    _ => {}
                }
            }
        }
        if self.at(&Tok::Question) {
            self.bump(); // nullable marker `T?`
        }
        Ok(Type::from_name(&name))
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::Semi) {
                self.bump();
                continue;
            }
            stmts.push(self.stmt()?);
        }
        self.eat(&Tok::RBrace)?;
        Ok(stmts)
    }

    // ── Statements ─────────────────────────────────────────────────

    fn stmt(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        // A loop label: `outer@ for (…)` / `outer@ while (…)`.
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::At) {
            let label = self.ident()?;
            self.eat(&Tok::At)?;
            let kind = match self.peek() {
                Tok::While => self.while_stmt(Some(label))?,
                Tok::For => self.for_stmt(Some(label))?,
                other => {
                    return Err(format!(
                        "a label must precede a loop (`for`/`while`), found {other:?}"
                    ))
                }
            };
            return Ok(Stmt::new(line, kind));
        }
        let kind = match self.peek() {
            Tok::Val | Tok::Var => self.let_decl()?,
            Tok::Return => {
                self.bump();
                // A `return` with no expression (Unit) — the next token starts a
                // new statement or closes the block.
                if matches!(self.peek(), Tok::RBrace | Tok::Semi | Tok::Eof) {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.expr()?))
                }
            }
            Tok::While => self.while_stmt(None)?,
            Tok::For => self.for_stmt(None)?,
            Tok::If => StmtKind::If(self.if_expr()?),
            Tok::When => StmtKind::When(self.when_expr()?),
            Tok::Break => {
                self.bump();
                StmtKind::Break(self.opt_label()?)
            }
            Tok::Continue => {
                self.bump();
                StmtKind::Continue(self.opt_label()?)
            }
            _ => self.assign_or_expr()?,
        };
        Ok(Stmt::new(line, kind))
    }

    /// An optional `@label` after `break`/`continue`.
    fn opt_label(&mut self) -> Result<Option<String>, String> {
        if self.at(&Tok::At) {
            self.bump();
            Ok(Some(self.ident()?))
        } else {
            Ok(None)
        }
    }

    fn let_decl(&mut self) -> Result<StmtKind, String> {
        let mutable = matches!(self.bump(), Tok::Var);
        let name = self.ident()?;
        let ty = if self.at(&Tok::Colon) {
            self.bump();
            Some(self.type_name()?)
        } else {
            None
        };
        self.eat(&Tok::Assign)?;
        let init = self.expr()?;
        Ok(StmtKind::Let {
            name,
            ty,
            init,
            mutable,
        })
    }

    fn while_stmt(&mut self, label: Option<String>) -> Result<StmtKind, String> {
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.block()?;
        Ok(StmtKind::While { cond, body, label })
    }

    fn for_stmt(&mut self, label: Option<String>) -> Result<StmtKind, String> {
        self.eat(&Tok::For)?;
        self.eat(&Tok::LParen)?;
        let var = self.ident()?;
        self.eat(&Tok::In)?;
        let start = self.range_bound()?;
        let (kind, end) = match self.peek() {
            Tok::DotDot => {
                self.bump();
                (RangeKind::Inclusive, self.range_bound()?)
            }
            Tok::Until => {
                self.bump();
                (RangeKind::Until, self.range_bound()?)
            }
            Tok::DownTo => {
                self.bump();
                (RangeKind::DownTo, self.range_bound()?)
            }
            other => {
                return Err(format!(
                    "for-loop needs a range (`a..b`, `a until b`, `a downTo b`), found {:?}",
                    other
                ))
            }
        };
        let step = if self.at(&Tok::Step) {
            self.bump();
            Some(self.range_bound()?)
        } else {
            None
        };
        self.eat(&Tok::RParen)?;
        let body = self.block()?;
        Ok(StmtKind::For {
            var,
            start,
            end,
            kind,
            step,
            body,
            label,
        })
    }

    /// A range endpoint — additive precedence, so `1..n-1` parses `n-1` as the
    /// end without swallowing the `..`.
    fn range_bound(&mut self) -> Result<Expr, String> {
        self.additive()
    }

    fn assign_or_expr(&mut self) -> Result<StmtKind, String> {
        // Look for `IDENT (op)=` — an assignment — before falling back to an
        // expression statement.
        if let Tok::Ident(name) = self.peek().clone() {
            let op = match self.toks.get(self.pos + 1).map(|s| &s.tok) {
                Some(Tok::Assign) => Some(None),
                Some(Tok::PlusEq) => Some(Some(BinOp::Add)),
                Some(Tok::MinusEq) => Some(Some(BinOp::Sub)),
                Some(Tok::StarEq) => Some(Some(BinOp::Mul)),
                Some(Tok::SlashEq) => Some(Some(BinOp::Div)),
                Some(Tok::PercentEq) => Some(Some(BinOp::Mod)),
                _ => None,
            };
            if let Some(binop) = op {
                self.bump(); // ident
                self.bump(); // assign token
                let value = self.expr()?;
                return Ok(StmtKind::Assign {
                    name,
                    op: binop,
                    value,
                });
            }
        }
        Ok(StmtKind::Expr(self.expr()?))
    }

    // ── Expressions ────────────────────────────────────────────────

    pub fn expr(&mut self) -> Result<Expr, String> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.and_expr()?;
        while self.at(&Tok::OrOr) {
            self.bump();
            let r = self.and_expr()?;
            l = Expr::Binary {
                op: BinOp::Or,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.eq_expr()?;
        while self.at(&Tok::AndAnd) {
            self.bump();
            let r = self.eq_expr()?;
            l = Expr::Binary {
                op: BinOp::And,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn eq_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.cmp_expr()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::Ne,
                _ => break,
            };
            self.bump();
            let r = self.cmp_expr()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn cmp_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.elvis_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Gt => BinOp::Gt,
                Tok::Le => BinOp::Le,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.bump();
            let r = self.elvis_expr()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    /// Elvis `a ?: b`, right-associative, binding tighter than comparison and
    /// looser than additive (matching Kotlin, which places `?:` above named
    /// checks and comparisons). `?:` is `Question` immediately followed by
    /// `Colon`; a `?` followed by `.` is a safe call and stays in `postfix`.
    fn elvis_expr(&mut self) -> Result<Expr, String> {
        let l = self.additive()?;
        if self.at(&Tok::Question) && matches!(self.peek_at(1), Tok::Colon) {
            self.bump(); // ?
            self.bump(); // :
            let r = self.elvis_expr()?;
            Ok(Expr::Elvis {
                left: Box::new(l),
                right: Box::new(r),
            })
        } else {
            Ok(l)
        }
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut l = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let r = self.multiplicative()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let r = self.unary()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Minus => {
                self.bump();
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(self.unary()?),
                })
            }
            Tok::Not => {
                self.bump();
                Ok(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(self.unary()?),
                })
            }
            _ => self.postfix(),
        }
    }

    /// Postfix `.` chains: `recv.property` and `recv.method(args)`, left-
    /// associative and chainable (`a.b.c()`). Binds tighter than the prefix
    /// unary operators, so `-a.b` is `-(a.b)`, matching Kotlin.
    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            // Not-null assertion `expr!!` — two consecutive `!` tokens (`!=`
            // lexes as a single `NotEq`, so this only fires on a literal `!!`).
            if self.at(&Tok::Not) && matches!(self.peek_at(1), Tok::Not) {
                self.bump();
                self.bump();
                e = Expr::NotNull(Box::new(e));
                continue;
            }
            // Plain member/method `.` or safe-call `?.`.
            let safe = if self.at(&Tok::Dot) {
                false
            } else if self.at(&Tok::Question) && matches!(self.peek_at(1), Tok::Dot) {
                self.bump(); // `?`
                true
            } else {
                break;
            };
            let line = self.line();
            self.bump(); // `.`
            let name = self.ident()?;
            if self.at(&Tok::LParen) {
                self.bump();
                let mut args = Vec::new();
                while !self.at(&Tok::RParen) {
                    args.push(self.expr()?);
                    if self.at(&Tok::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.eat(&Tok::RParen)?;
                e = Expr::MethodCall {
                    recv: Box::new(e),
                    name,
                    args,
                    safe,
                    line,
                };
            } else {
                e = Expr::Member {
                    recv: Box::new(e),
                    name,
                    safe,
                    line,
                };
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let line = self.line();
        match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                Ok(Expr::Int(n))
            }
            Tok::Float(f) => {
                self.bump();
                Ok(Expr::Float(f))
            }
            Tok::Bool(b) => {
                self.bump();
                Ok(Expr::Bool(b))
            }
            Tok::Char(c) => {
                self.bump();
                Ok(Expr::Char(c))
            }
            Tok::Null => {
                self.bump();
                Ok(Expr::Null)
            }
            Tok::Str(parts) => {
                self.bump();
                Ok(Expr::Str(self.str_parts(&parts)?))
            }
            Tok::If => Ok(Expr::If(self.if_expr()?)),
            Tok::When => Ok(Expr::When(self.when_expr()?)),
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(name) => {
                self.bump();
                if self.at(&Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(&Tok::RParen) {
                        args.push(self.expr()?);
                        if self.at(&Tok::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.eat(&Tok::RParen)?;
                    Ok(Expr::Call { name, args, line })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("unexpected token {:?} (line {})", other, line)),
        }
    }

    fn if_expr(&mut self) -> Result<IfExpr, String> {
        let line = self.line();
        self.eat(&Tok::If)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let then = self.branch_body()?;
        let els = if self.at(&Tok::Else) {
            self.bump();
            if self.at(&Tok::If) {
                // `else if` chains as an else-branch holding a single if-stmt.
                let l = self.line();
                Some(vec![Stmt::new(l, StmtKind::If(self.if_expr()?))])
            } else {
                Some(self.branch_body()?)
            }
        } else {
            None
        };
        Ok(IfExpr {
            cond: Box::new(cond),
            then,
            els,
            line,
        })
    }

    /// A `when` — subject form `when (x) { … }` or subjectless `when { … }`.
    /// Arms are `guard -> body`, with `guard` either `else`, or one or more
    /// comma-separated conditions.
    fn when_expr(&mut self) -> Result<WhenExpr, String> {
        let line = self.line();
        self.eat(&Tok::When)?;
        let subject = if self.at(&Tok::LParen) {
            self.bump();
            let e = self.expr()?;
            self.eat(&Tok::RParen)?;
            Some(Box::new(e))
        } else {
            None
        };
        let has_subject = subject.is_some();
        self.eat(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::Semi) {
                self.bump();
                continue;
            }
            let guard = if self.at(&Tok::Else) {
                self.bump();
                WhenGuard::Else
            } else {
                let mut conds = vec![self.when_cond(has_subject)?];
                while self.at(&Tok::Comma) {
                    self.bump();
                    conds.push(self.when_cond(has_subject)?);
                }
                WhenGuard::Conds(conds)
            };
            self.eat(&Tok::Arrow)?;
            let body = self.branch_body()?;
            arms.push(WhenArm { guard, body });
        }
        self.eat(&Tok::RBrace)?;
        Ok(WhenExpr {
            subject,
            arms,
            line,
        })
    }

    /// A single `when` arm condition. In subject form it may be `in range`,
    /// `!in range`, `is Type`, `!is Type`, or an expression (equality against
    /// the subject). In subjectless form it is a boolean expression.
    fn when_cond(&mut self, has_subject: bool) -> Result<WhenCond, String> {
        if has_subject {
            match self.peek() {
                Tok::In => {
                    self.bump();
                    return self.when_range(false);
                }
                Tok::Is => {
                    self.bump();
                    let ty = self.ident()?;
                    return Ok(WhenCond::Is { negated: false, ty });
                }
                // `!in` / `!is` — a `!` immediately followed by `in`/`is`.
                Tok::Not if matches!(self.peek_at(1), Tok::In) => {
                    self.bump();
                    self.bump();
                    return self.when_range(true);
                }
                Tok::Not if matches!(self.peek_at(1), Tok::Is) => {
                    self.bump();
                    self.bump();
                    let ty = self.ident()?;
                    return Ok(WhenCond::Is { negated: true, ty });
                }
                _ => {}
            }
        }
        Ok(WhenCond::Expr(self.expr()?))
    }

    /// The range after `in`/`!in` in a `when` arm — `a..b`, `a until b`, or
    /// `a downTo b`.
    fn when_range(&mut self, negated: bool) -> Result<WhenCond, String> {
        let start = self.range_bound()?;
        let (kind, end) = match self.peek() {
            Tok::DotDot => {
                self.bump();
                (RangeKind::Inclusive, self.range_bound()?)
            }
            Tok::Until => {
                self.bump();
                (RangeKind::Until, self.range_bound()?)
            }
            Tok::DownTo => {
                self.bump();
                (RangeKind::DownTo, self.range_bound()?)
            }
            other => return Err(format!(
                "`in` condition needs a range (`a..b`, `a until b`, `a downTo b`), found {other:?}"
            )),
        };
        Ok(WhenCond::InRange {
            negated,
            start,
            end,
            kind,
        })
    }

    /// An `if`/`else`/`when` branch body: either a `{ … }` block or a single
    /// statement. The single form covers a value expression (`if (c) e1 else e2`)
    /// as well as the control-flow forms Kotlin permits there — `break`,
    /// `continue`, `return`, and a nested `when` — which are statements, not
    /// expressions.
    fn branch_body(&mut self) -> Result<Vec<Stmt>, String> {
        if self.at(&Tok::LBrace) {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    /// Turn lexed [`StrPart`]s into [`StrExpr`]s, sub-parsing each interpolation
    /// fragment as its own expression.
    fn str_parts(&self, parts: &[StrPart]) -> Result<Vec<StrExpr>, String> {
        let mut out = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                StrPart::Text(t) => out.push(StrExpr::Text(t.clone())),
                StrPart::Expr(src) => {
                    let toks = Lexer::new(src).tokenize()?;
                    let mut sub = Parser { toks, pos: 0 };
                    let e = sub.expr()?;
                    out.push(StrExpr::Expr(Box::new(e)));
                }
            }
        }
        Ok(out)
    }
}
