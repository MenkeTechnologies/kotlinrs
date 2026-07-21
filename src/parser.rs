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

    /// A type reference — `Int`, `String`, `Array<String>`, … Generic args are
    /// consumed but ignored (coarse typing).
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
            Tok::While => self.while_stmt()?,
            Tok::For => self.for_stmt()?,
            Tok::If => StmtKind::If(self.if_expr()?),
            _ => self.assign_or_expr()?,
        };
        Ok(Stmt::new(line, kind))
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

    fn while_stmt(&mut self) -> Result<StmtKind, String> {
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.block()?;
        Ok(StmtKind::While { cond, body })
    }

    fn for_stmt(&mut self) -> Result<StmtKind, String> {
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
        let mut l = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Gt => BinOp::Gt,
                Tok::Le => BinOp::Le,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.bump();
            let r = self.additive()?;
            l = Expr::Binary {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
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
        while self.at(&Tok::Dot) {
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
                    line,
                };
            } else {
                e = Expr::Member {
                    recv: Box::new(e),
                    name,
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
            Tok::Str(parts) => {
                self.bump();
                Ok(Expr::Str(self.str_parts(&parts)?))
            }
            Tok::If => Ok(Expr::If(self.if_expr()?)),
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

    /// An `if`/`else` branch body: either a `{ … }` block or a single
    /// expression (Kotlin allows `if (c) e1 else e2`).
    fn branch_body(&mut self) -> Result<Vec<Stmt>, String> {
        if self.at(&Tok::LBrace) {
            self.block()
        } else {
            let line = self.line();
            Ok(vec![Stmt::new(line, StmtKind::Expr(self.expr()?))])
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
