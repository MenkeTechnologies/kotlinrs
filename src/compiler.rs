//! Lower the Kotlin AST to a `fusevm::Chunk`.
//!
//! Design: kotlinrs carries no VM or JIT. Arithmetic, comparison, control flow,
//! locals, and calls lower to *native* fusevm ops (`Add`, `NumLt`, `JumpIfFalse`,
//! `GetSlot`, `Op::Call`, `PrintLn`, …) so fusevm's Cranelift JIT can trace hot
//! loops. Only the three Kotlin-specific behaviors that the universal ops can't
//! express go through the extension handler (see [`crate::host`]).
//!
//! ## Invariant
//! Every `compile_expr` leaves **exactly one** value on the stack (Unit is a
//! pushed `Undef`), and every `compile_stmt` is stack-neutral. This keeps the
//! stack balanced across `if`/`while`/`for` without a separate analysis pass.
//!
//! ## Layout of the emitted chunk
//! ```text
//! [preamble]  push main's args · Call(main) · Pop · Jump(END)
//! [bodies]    each `fun` as a sub (add_sub_entry): prologue binds params to
//!             slots, then the compiled body, then a fallthrough Unit return
//! END:        one past the last op — the VM halts here
//! ```

use crate::ast::*;
use crate::host::{KT_DBG_LINE, KT_FFI_CALL, KT_FFI_COMPILE, KT_IDIV, KT_IMOD, KT_TO_STRING};
use fusevm::{Chunk, ChunkBuilder, Op, Value};
use std::collections::HashMap;

/// The desugar target a `rust { ... }` block lowers to (see [`crate::rust_ffi`]).
const RUST_COMPILE: &str = "__rust_compile";

/// Per-function lowering scope: slot assignments and coarse types.
struct Scope {
    slots: HashMap<String, u16>,
    types: HashMap<String, Type>,
    next_slot: u16,
}

impl Scope {
    fn new() -> Self {
        Scope {
            slots: HashMap::new(),
            types: HashMap::new(),
            next_slot: 0,
        }
    }
    fn declare(&mut self, name: &str, ty: Type) -> u16 {
        let slot = self.next_slot;
        self.next_slot += 1;
        self.slots.insert(name.to_string(), slot);
        self.types.insert(name.to_string(), ty);
        slot
    }
    /// A fresh anonymous slot (loop end/step temporaries).
    fn temp(&mut self) -> u16 {
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }
    fn slot(&self, name: &str) -> Option<u16> {
        self.slots.get(name).copied()
    }
    fn ty(&self, name: &str) -> Type {
        self.types.get(name).copied().unwrap_or(Type::Unknown)
    }
}

pub struct Compiler {
    b: ChunkBuilder,
    /// name → (return type, arity) for user functions, filled before lowering.
    fun_sig: HashMap<String, (Type, usize)>,
    /// When true, emit a per-statement `Op::Extended(KT_DBG_LINE, 0)` marker
    /// (carrying the statement's source line) before each statement, so the
    /// `--dap` debugger can stop at breakpoints and step. Off for normal runs —
    /// they carry zero extra ops.
    debug: bool,
    /// True when the program contains a `rust { ... }` FFI block (a
    /// `__rust_compile` call). Only then does an unresolved call name lower to a
    /// runtime FFI dispatch instead of a compile error — so non-FFI programs keep
    /// their exact "unresolved reference" compile-time diagnostic.
    has_ffi: bool,
}

/// Compile a program to a runnable chunk. Requires a `fun main`.
pub fn compile(program: &[FunDecl]) -> Result<Chunk, String> {
    compile_with(program, false)
}

/// Compile with per-statement DAP line markers enabled (`kotlin --dap`).
pub fn compile_debug(program: &[FunDecl]) -> Result<Chunk, String> {
    compile_with(program, true)
}

/// Compile a program to a runnable chunk, optionally instrumented with debug
/// line markers. Requires a `fun main`.
pub fn compile_with(program: &[FunDecl], debug: bool) -> Result<Chunk, String> {
    let mut fun_sig = HashMap::new();
    for f in program {
        fun_sig.insert(f.name.clone(), (f.ret, f.params.len()));
    }
    let main = program
        .iter()
        .find(|f| f.name == "main")
        .ok_or("no `fun main` found")?;

    let has_ffi = program.iter().any(|f| body_has_ffi(&f.body));

    let mut c = Compiler {
        b: ChunkBuilder::new(),
        fun_sig,
        debug,
        has_ffi,
    };

    // Preamble: bind main's args (an empty Array per declared parameter — the
    // program-args wiring is an M0 stub), call main, discard its Unit, skip the
    // function bodies.
    let main_idx = c.b.add_name("main");
    for _ in &main.params {
        c.b.emit(Op::MakeArray(0), main.line);
    }
    c.b.emit(Op::Call(main_idx, main.params.len() as u8), main.line);
    c.b.emit(Op::Pop, main.line);
    let end_jump = c.b.emit(Op::Jump(0), main.line);

    for f in program {
        c.compile_fun(f)?;
    }

    let end = c.b.current_pos();
    c.b.patch_jump(end_jump, end);
    Ok(c.b.build())
}

impl Compiler {
    fn compile_fun(&mut self, f: &FunDecl) -> Result<(), String> {
        let entry = self.b.current_pos();
        let name_idx = self.b.add_name(&f.name);
        self.b.add_sub_entry(name_idx, entry);

        let mut sc = Scope::new();
        // Parameters occupy slots 0..n in declaration order.
        for (pname, pty) in &f.params {
            sc.declare(pname, *pty);
        }
        // Bind args (stack top = last arg) into slots, deepest last.
        for i in (0..f.params.len()).rev() {
            self.b.emit(Op::SetSlot(i as u16), f.line);
        }

        for s in &f.body {
            self.compile_stmt(&mut sc, s)?;
        }
        // Fallthrough Unit return for `Unit` functions / a missing `return`.
        self.b.emit(Op::LoadUndef, f.line);
        self.b.emit(Op::ReturnValue, f.line);
        Ok(())
    }

    // ── Statements (stack-neutral) ─────────────────────────────────

    fn compile_stmt(&mut self, sc: &mut Scope, s: &Stmt) -> Result<(), String> {
        // In debug mode, a stack-neutral marker carrying this statement's source
        // line precedes the statement — the `--dap` hook reads it to decide
        // whether to stop here. `Op::Extended(KT_DBG_LINE, 0)` pushes nothing (the
        // host handler is a no-op on the value stack), so the balance invariant
        // holds.
        if self.debug && s.line != 0 {
            self.b.emit(Op::Extended(KT_DBG_LINE, 0), s.line);
        }
        match &s.kind {
            StmtKind::Let {
                name,
                ty,
                init,
                mutable: _,
            } => {
                let it = self.compile_expr(sc, init)?;
                let vty = ty.unwrap_or(it);
                let slot = sc.declare(name, vty);
                self.b.emit(Op::SetSlot(slot), 0);
            }
            StmtKind::Assign { name, op, value } => {
                let slot = sc
                    .slot(name)
                    .ok_or_else(|| format!("unresolved reference: {name}"))?;
                match op {
                    None => {
                        self.compile_expr(sc, value)?;
                    }
                    Some(binop) => {
                        // `x op= v` == `x = x op v`.
                        let lhs = Expr::Var(name.clone());
                        let expr = Expr::Binary {
                            op: *binop,
                            l: Box::new(lhs),
                            r: Box::new(value.clone()),
                        };
                        self.compile_expr(sc, &expr)?;
                    }
                }
                self.b.emit(Op::SetSlot(slot), 0);
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) => {
                        self.compile_expr(sc, e)?;
                    }
                    None => {
                        self.b.emit(Op::LoadUndef, 0);
                    }
                }
                self.b.emit(Op::ReturnValue, 0);
            }
            StmtKind::While { cond, body } => {
                let start = self.b.current_pos();
                self.compile_expr(sc, cond)?;
                let jf = self.b.emit(Op::JumpIfFalse(0), 0);
                for s in body {
                    self.compile_stmt(sc, s)?;
                }
                self.b.emit(Op::Jump(start), 0);
                let end = self.b.current_pos();
                self.b.patch_jump(jf, end);
            }
            StmtKind::For {
                var,
                start,
                end,
                kind,
                step,
                body,
            } => {
                self.compile_for(sc, var, start, end, *kind, step, body)?;
            }
            StmtKind::If(ie) => {
                self.compile_if(sc, ie)?;
                self.b.emit(Op::Pop, ie.line); // statement position discards value
            }
            StmtKind::Expr(e) => {
                self.compile_expr(sc, e)?;
                self.b.emit(Op::Pop, 0);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compile_for(
        &mut self,
        sc: &mut Scope,
        var: &str,
        start: &Expr,
        end: &Expr,
        kind: RangeKind,
        step: &Option<Expr>,
        body: &[Stmt],
    ) -> Result<(), String> {
        // Loop counter and one-shot end/step temporaries.
        let vslot = sc.declare(var, Type::Int);
        self.compile_expr(sc, start)?;
        self.b.emit(Op::SetSlot(vslot), 0);
        let eslot = sc.temp();
        self.compile_expr(sc, end)?;
        self.b.emit(Op::SetSlot(eslot), 0);
        let sslot = if let Some(st) = step {
            let s = sc.temp();
            self.compile_expr(sc, st)?;
            self.b.emit(Op::SetSlot(s), 0);
            Some(s)
        } else {
            None
        };

        let top = self.b.current_pos();
        self.b.emit(Op::GetSlot(vslot), 0);
        self.b.emit(Op::GetSlot(eslot), 0);
        self.b.emit(
            match kind {
                RangeKind::Inclusive => Op::NumLe,
                RangeKind::Until => Op::NumLt,
                RangeKind::DownTo => Op::NumGe,
            },
            0,
        );
        let jf = self.b.emit(Op::JumpIfFalse(0), 0);

        for s in body {
            self.compile_stmt(sc, s)?;
        }

        // counter += step (or -= step for downTo).
        self.b.emit(Op::GetSlot(vslot), 0);
        match sslot {
            Some(s) => self.b.emit(Op::GetSlot(s), 0),
            None => self.b.emit(Op::LoadInt(1), 0),
        };
        self.b.emit(
            if kind == RangeKind::DownTo {
                Op::Sub
            } else {
                Op::Add
            },
            0,
        );
        self.b.emit(Op::SetSlot(vslot), 0);
        self.b.emit(Op::Jump(top), 0);
        let done = self.b.current_pos();
        self.b.patch_jump(jf, done);
        Ok(())
    }

    // ── Expressions (leave exactly one value) ──────────────────────

    fn compile_expr(&mut self, sc: &mut Scope, e: &Expr) -> Result<Type, String> {
        match e {
            Expr::Int(n) => {
                self.b.emit(Op::LoadInt(*n), 0);
                Ok(Type::Int)
            }
            Expr::Float(f) => {
                self.b.emit(Op::LoadFloat(*f), 0);
                Ok(Type::Double)
            }
            Expr::Bool(b) => {
                self.b
                    .emit(if *b { Op::LoadTrue } else { Op::LoadFalse }, 0);
                Ok(Type::Boolean)
            }
            Expr::Str(parts) => {
                self.compile_str(sc, parts)?;
                Ok(Type::String)
            }
            Expr::Var(name) => {
                let slot = sc
                    .slot(name)
                    .ok_or_else(|| format!("unresolved reference: {name}"))?;
                self.b.emit(Op::GetSlot(slot), 0);
                Ok(sc.ty(name))
            }
            Expr::Unary { op, expr } => {
                let t = self.compile_expr(sc, expr)?;
                match op {
                    UnOp::Neg => {
                        self.b.emit(Op::Negate, 0);
                        Ok(if t == Type::Double {
                            Type::Double
                        } else {
                            Type::Int
                        })
                    }
                    UnOp::Not => {
                        self.b.emit(Op::LogNot, 0);
                        Ok(Type::Boolean)
                    }
                }
            }
            Expr::Binary { op, l, r } => self.compile_binary(sc, *op, l, r),
            Expr::Call { name, args, line } => self.compile_call(sc, name, args, *line),
            Expr::If(ie) => self.compile_if(sc, ie),
        }
    }

    fn compile_str(&mut self, sc: &mut Scope, parts: &[StrExpr]) -> Result<(), String> {
        if parts.is_empty() {
            let idx = self.b.add_constant(Value::str(""));
            self.b.emit(Op::LoadConst(idx), 0);
            return Ok(());
        }
        for (i, part) in parts.iter().enumerate() {
            match part {
                StrExpr::Text(t) => {
                    let idx = self.b.add_constant(Value::str(t.clone()));
                    self.b.emit(Op::LoadConst(idx), 0);
                }
                StrExpr::Expr(e) => {
                    let t = self.compile_expr(sc, e)?;
                    if t != Type::String {
                        self.b.emit(Op::Extended(KT_TO_STRING, 0), 0);
                    }
                }
            }
            if i > 0 {
                self.b.emit(Op::Concat, 0);
            }
        }
        Ok(())
    }

    fn compile_binary(
        &mut self,
        sc: &mut Scope,
        op: BinOp,
        l: &Expr,
        r: &Expr,
    ) -> Result<Type, String> {
        match op {
            BinOp::And => {
                self.compile_expr(sc, l)?;
                let j = self.b.emit(Op::JumpIfFalseKeep(0), 0);
                self.b.emit(Op::Pop, 0);
                self.compile_expr(sc, r)?;
                let end = self.b.current_pos();
                self.b.patch_jump(j, end);
                return Ok(Type::Boolean);
            }
            BinOp::Or => {
                self.compile_expr(sc, l)?;
                let j = self.b.emit(Op::JumpIfTrueKeep(0), 0);
                self.b.emit(Op::Pop, 0);
                self.compile_expr(sc, r)?;
                let end = self.b.current_pos();
                self.b.patch_jump(j, end);
                return Ok(Type::Boolean);
            }
            _ => {}
        }

        let lt = self.infer(sc, l);
        let rt = self.infer(sc, r);

        // `+` is string concatenation when either side is a String.
        if op == BinOp::Add && (lt == Type::String || rt == Type::String) {
            self.compile_expr(sc, l)?;
            if lt != Type::String {
                self.b.emit(Op::Extended(KT_TO_STRING, 0), 0);
            }
            self.compile_expr(sc, r)?;
            if rt != Type::String {
                self.b.emit(Op::Extended(KT_TO_STRING, 0), 0);
            }
            self.b.emit(Op::Concat, 0);
            return Ok(Type::String);
        }

        self.compile_expr(sc, l)?;
        self.compile_expr(sc, r)?;

        let both_int = lt.is_int() && rt.is_int();
        let both_str = lt == Type::String && rt == Type::String;
        let num_ty = if lt == Type::Double || rt == Type::Double {
            Type::Double
        } else {
            Type::Int
        };

        let ty = match op {
            BinOp::Add => {
                self.b.emit(Op::Add, 0);
                num_ty
            }
            BinOp::Sub => {
                self.b.emit(Op::Sub, 0);
                num_ty
            }
            BinOp::Mul => {
                self.b.emit(Op::Mul, 0);
                num_ty
            }
            BinOp::Div => {
                if both_int {
                    self.b.emit(Op::Extended(KT_IDIV, 0), 0);
                    Type::Int
                } else {
                    self.b.emit(Op::Div, 0);
                    Type::Double
                }
            }
            BinOp::Mod => {
                self.b.emit(Op::Extended(KT_IMOD, 0), 0);
                if both_int {
                    Type::Int
                } else {
                    Type::Double
                }
            }
            BinOp::Eq => {
                self.b.emit(if both_str { Op::StrEq } else { Op::NumEq }, 0);
                Type::Boolean
            }
            BinOp::Ne => {
                self.b.emit(if both_str { Op::StrNe } else { Op::NumNe }, 0);
                Type::Boolean
            }
            BinOp::Lt => {
                self.b.emit(if both_str { Op::StrLt } else { Op::NumLt }, 0);
                Type::Boolean
            }
            BinOp::Gt => {
                self.b.emit(if both_str { Op::StrGt } else { Op::NumGt }, 0);
                Type::Boolean
            }
            BinOp::Le => {
                self.b.emit(if both_str { Op::StrLe } else { Op::NumLe }, 0);
                Type::Boolean
            }
            BinOp::Ge => {
                self.b.emit(if both_str { Op::StrGe } else { Op::NumGe }, 0);
                Type::Boolean
            }
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        };
        Ok(ty)
    }

    fn compile_call(
        &mut self,
        sc: &mut Scope,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        // `__rust_compile("<base64>", line)` — the desugar target of a
        // `rust { ... }` block. Compile the base64 body string and hand it to the
        // FFI-compile extension op; the call evaluates to Unit.
        if name == RUST_COMPILE {
            if let Some(body) = args.first() {
                self.compile_expr(sc, body)?;
                self.b.emit(Op::Extended(KT_FFI_COMPILE, 0), line);
            }
            self.b.emit(Op::LoadUndef, line);
            return Ok(Type::Unit);
        }
        match name {
            "println" | "print" => {
                if args.len() > 1 {
                    return Err(format!("{name} takes at most one argument in M0"));
                }
                if let Some(a) = args.first() {
                    let t = self.compile_expr(sc, a)?;
                    if t != Type::String {
                        self.b.emit(Op::Extended(KT_TO_STRING, 0), 0);
                    }
                    self.b.emit(
                        if name == "println" {
                            Op::PrintLn(1)
                        } else {
                            Op::Print(1)
                        },
                        line,
                    );
                } else {
                    self.b.emit(
                        if name == "println" {
                            Op::PrintLn(0)
                        } else {
                            Op::Print(0)
                        },
                        line,
                    );
                }
                self.b.emit(Op::LoadUndef, line); // println returns Unit
                Ok(Type::Unit)
            }
            _ => {
                match self.fun_sig.get(name) {
                    Some(&(ret, arity)) => {
                        if args.len() != arity {
                            return Err(format!(
                                "function {name} expects {arity} argument(s), got {}",
                                args.len()
                            ));
                        }
                        for a in args {
                            self.compile_expr(sc, a)?;
                        }
                        let idx = self.b.add_name(name);
                        self.b.emit(Op::Call(idx, arity as u8), line);
                        Ok(ret)
                    }
                    // Unknown name. With a `rust { ... }` block present it may be an
                    // FFI export registered at runtime, so lower to a by-name FFI
                    // dispatch; the args are pushed deepest-first, then the name.
                    // Without any FFI block, it stays a compile-time error.
                    None if self.has_ffi => {
                        for a in args {
                            self.compile_expr(sc, a)?;
                        }
                        let nidx = self.b.add_constant(Value::str(name.to_string()));
                        self.b.emit(Op::LoadConst(nidx), line);
                        self.b.emit(Op::Extended(KT_FFI_CALL, args.len() as u8), line);
                        Ok(Type::Unknown)
                    }
                    None => Err(format!("unresolved reference: {name}")),
                }
            }
        }
    }

    fn compile_if(&mut self, sc: &mut Scope, ie: &IfExpr) -> Result<Type, String> {
        self.compile_expr(sc, &ie.cond)?;
        let jf = self.b.emit(Op::JumpIfFalse(0), ie.line);
        let tt = self.compile_block_value(sc, &ie.then)?;
        let jmp = self.b.emit(Op::Jump(0), ie.line);
        let else_pos = self.b.current_pos();
        self.b.patch_jump(jf, else_pos);
        let et = match &ie.els {
            Some(els) => self.compile_block_value(sc, els)?,
            None => {
                self.b.emit(Op::LoadUndef, ie.line);
                Type::Unit
            }
        };
        let end = self.b.current_pos();
        self.b.patch_jump(jmp, end);
        Ok(if tt == et { tt } else { Type::Unknown })
    }

    /// Compile a branch body leaving exactly one value: the last statement's
    /// expression value, or `Undef` (Unit).
    fn compile_block_value(&mut self, sc: &mut Scope, body: &[Stmt]) -> Result<Type, String> {
        if body.is_empty() {
            self.b.emit(Op::LoadUndef, 0);
            return Ok(Type::Unit);
        }
        let (last, init) = body.split_last().unwrap();
        for s in init {
            self.compile_stmt(sc, s)?;
        }
        // The last statement's value is the block's value. Its debug marker
        // precedes it so a breakpoint on the tail line fires. The `Expr`/`If`
        // arms compile the value directly (not via `compile_stmt`), so the marker
        // is emitted here; the fallback arm defers to `compile_stmt`, which emits
        // its own marker.
        let mark = |c: &mut Self| {
            if c.debug && last.line != 0 {
                c.b.emit(Op::Extended(KT_DBG_LINE, 0), last.line);
            }
        };
        match &last.kind {
            StmtKind::Expr(e) => {
                mark(self);
                self.compile_expr(sc, e)
            }
            StmtKind::If(ie) => {
                mark(self);
                self.compile_if(sc, ie)
            }
            _ => {
                self.compile_stmt(sc, last)?;
                self.b.emit(Op::LoadUndef, 0);
                Ok(Type::Unit)
            }
        }
    }

    // ── Coarse type inference (no code emitted) ────────────────────

    fn infer(&self, sc: &Scope, e: &Expr) -> Type {
        match e {
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Double,
            Expr::Bool(_) => Type::Boolean,
            Expr::Str(_) => Type::String,
            Expr::Var(n) => sc.ty(n),
            Expr::Unary { op, expr } => match op {
                UnOp::Not => Type::Boolean,
                UnOp::Neg => {
                    if self.infer(sc, expr) == Type::Double {
                        Type::Double
                    } else {
                        Type::Int
                    }
                }
            },
            Expr::Binary { op, l, r } => match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or => Type::Boolean,
                BinOp::Add => {
                    let lt = self.infer(sc, l);
                    let rt = self.infer(sc, r);
                    if lt == Type::String || rt == Type::String {
                        Type::String
                    } else if lt == Type::Double || rt == Type::Double {
                        Type::Double
                    } else {
                        Type::Int
                    }
                }
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    if self.infer(sc, l) == Type::Double || self.infer(sc, r) == Type::Double {
                        Type::Double
                    } else {
                        Type::Int
                    }
                }
            },
            Expr::Call { name, .. } => match name.as_str() {
                "println" | "print" => Type::Unit,
                _ => self
                    .fun_sig
                    .get(name)
                    .map(|(t, _)| *t)
                    .unwrap_or(Type::Unknown),
            },
            Expr::If(ie) => {
                let tt = ie
                    .then
                    .last()
                    .map(|s| self.infer_stmt(sc, s))
                    .unwrap_or(Type::Unit);
                match &ie.els {
                    Some(els) => {
                        let et = els
                            .last()
                            .map(|s| self.infer_stmt(sc, s))
                            .unwrap_or(Type::Unit);
                        if tt == et {
                            tt
                        } else {
                            Type::Unknown
                        }
                    }
                    None => Type::Unit,
                }
            }
        }
    }

    fn infer_stmt(&self, sc: &Scope, s: &Stmt) -> Type {
        match &s.kind {
            StmtKind::Expr(e) => self.infer(sc, e),
            StmtKind::If(ie) => self.infer(sc, &Expr::If(ie.clone())),
            _ => Type::Unit,
        }
    }
}

// ── FFI detection (does the program contain a `rust { ... }` block?) ────────

/// True if any statement in `body` (recursively) evaluates a `__rust_compile`
/// call — the desugar target of a `rust { ... }` block.
fn body_has_ffi(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Let { init, .. } => expr_has_ffi(init),
        StmtKind::Assign { value, .. } => expr_has_ffi(value),
        StmtKind::Return(Some(e)) => expr_has_ffi(e),
        StmtKind::Return(None) => false,
        StmtKind::While { cond, body } => expr_has_ffi(cond) || body_has_ffi(body),
        StmtKind::For {
            start, end, body, ..
        } => expr_has_ffi(start) || expr_has_ffi(end) || body_has_ffi(body),
        StmtKind::If(ie) => if_has_ffi(ie),
        StmtKind::Expr(e) => expr_has_ffi(e),
    })
}

fn if_has_ffi(ie: &IfExpr) -> bool {
    expr_has_ffi(&ie.cond)
        || body_has_ffi(&ie.then)
        || ie.els.as_deref().is_some_and(body_has_ffi)
}

fn expr_has_ffi(e: &Expr) -> bool {
    match e {
        Expr::Call { name, args, .. } => {
            name == RUST_COMPILE || args.iter().any(expr_has_ffi)
        }
        Expr::Unary { expr, .. } => expr_has_ffi(expr),
        Expr::Binary { l, r, .. } => expr_has_ffi(l) || expr_has_ffi(r),
        Expr::If(ie) => if_has_ffi(ie),
        Expr::Str(parts) => parts.iter().any(|p| match p {
            StrExpr::Expr(e) => expr_has_ffi(e),
            StrExpr::Text(_) => false,
        }),
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Var(_) => false,
    }
}
