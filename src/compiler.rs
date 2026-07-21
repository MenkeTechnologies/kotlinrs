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
use crate::host::{
    KT_CHR_STRING, KT_DBG_LINE, KT_FFI_CALL, KT_FFI_COMPILE, KT_IDIV, KT_IMOD, KT_IS, KT_ISNULL,
    KT_METHOD, KT_NOTNULL, KT_TO_STRING,
};
use fusevm::{Chunk, ChunkBuilder, Op, Value};
use std::collections::HashMap;

/// The desugar target a `rust { ... }` block lowers to (see [`crate::rust_ffi`]).
const RUST_COMPILE: &str = "__rust_compile";

/// A single name binding: its slot, coarse type, and whether it is a `var`
/// (reassignable) or a `val` (write-once).
#[derive(Clone)]
struct Binding {
    slot: u16,
    ty: Type,
    mutable: bool,
}

/// A checkpoint of scope state, taken on block entry and restored on block exit
/// so bindings declared inside a nested block drop when the block ends. See
/// [`Scope::enter`] / [`Scope::exit`].
struct ScopeMark {
    next_slot: u16,
    undo_len: usize,
}

/// Per-function lowering scope: lexical name → binding, with nested-block
/// entry/exit so inner declarations don't leak and shadowing is restored on
/// exit. Slots are freed (reused) when a block ends; the VM's slot frame is
/// sized to the high-water mark, so reuse is safe.
struct Scope {
    map: HashMap<String, Binding>,
    next_slot: u16,
    /// Undo log: each `declare` records the name and the binding it displaced
    /// (`None` if the name was previously unbound). [`Scope::exit`] replays it
    /// back to a mark, restoring shadowed outer bindings.
    undo: Vec<(String, Option<Binding>)>,
}

impl Scope {
    fn new() -> Self {
        Scope {
            map: HashMap::new(),
            next_slot: 0,
            undo: Vec::new(),
        }
    }
    /// Declare (or shadow) `name`. `mutable` is `true` for `var`, `false` for
    /// `val`. Returns the assigned slot.
    fn declare(&mut self, name: &str, ty: Type, mutable: bool) -> u16 {
        let slot = self.next_slot;
        self.next_slot += 1;
        let prev = self
            .map
            .insert(name.to_string(), Binding { slot, ty, mutable });
        self.undo.push((name.to_string(), prev));
        slot
    }
    /// A fresh anonymous slot (loop end/step temporaries). Reclaimed on the next
    /// enclosing [`Scope::exit`] via the `next_slot` restore.
    fn temp(&mut self) -> u16 {
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }
    /// Open a nested block. Pair with [`Scope::exit`].
    fn enter(&self) -> ScopeMark {
        ScopeMark {
            next_slot: self.next_slot,
            undo_len: self.undo.len(),
        }
    }
    /// Close a nested block: undo every declaration made since the matching
    /// [`Scope::enter`] (restoring any shadowed outer binding) and free the
    /// slots the block used.
    fn exit(&mut self, mark: ScopeMark) {
        while self.undo.len() > mark.undo_len {
            let (name, prev) = self.undo.pop().unwrap();
            match prev {
                Some(b) => {
                    self.map.insert(name, b);
                }
                None => {
                    self.map.remove(&name);
                }
            }
        }
        self.next_slot = mark.next_slot;
    }
    fn slot(&self, name: &str) -> Option<u16> {
        self.map.get(name).map(|b| b.slot)
    }
    fn ty(&self, name: &str) -> Type {
        self.map.get(name).map(|b| b.ty).unwrap_or(Type::Unknown)
    }
    /// Whether `name` is currently bound as a reassignable `var`.
    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.map.get(name).map(|b| b.mutable)
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
    /// Stack of enclosing loops, innermost last. Each records the (labeled) loop
    /// so `break`/`continue` can backpatch their jumps to the loop's exit /
    /// next-iteration point. See [`LoopCtx`].
    loops: Vec<LoopCtx>,
}

/// Backpatch bookkeeping for one enclosing loop. `break`/`continue` emit a
/// `Jump(0)` and stash its op index here; the loop patches them once its exit
/// and continue targets are known.
struct LoopCtx {
    label: Option<String>,
    /// Op indices of `break` jumps — patched to the loop's exit.
    breaks: Vec<usize>,
    /// Op indices of `continue` jumps — patched to the loop's next-iteration
    /// point (the `while` condition, or the `for` increment).
    continues: Vec<usize>,
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
        loops: Vec::new(),
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
        // Parameters occupy slots 0..n in declaration order. Kotlin function
        // parameters are read-only (`val`), so they are declared immutable.
        for (pname, pty) in &f.params {
            sc.declare(pname, *pty, false);
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
                mutable,
            } => {
                let it = self.compile_expr(sc, init)?;
                let vty = ty.unwrap_or(it);
                let slot = sc.declare(name, vty, *mutable);
                self.b.emit(Op::SetSlot(slot), 0);
            }
            StmtKind::Assign { name, op, value } => {
                // A `val` (write-once) binding cannot be reassigned — Kotlin
                // reports this at compile time.
                if sc.is_mutable(name) == Some(false) {
                    return Err(format!("val cannot be reassigned: {name}"));
                }
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
            StmtKind::While { cond, body, label } => {
                let start = self.b.current_pos();
                self.compile_expr(sc, cond)?;
                let jf = self.b.emit(Op::JumpIfFalse(0), 0);
                self.loops.push(LoopCtx {
                    label: label.clone(),
                    breaks: Vec::new(),
                    continues: Vec::new(),
                });
                let mark = sc.enter();
                for s in body {
                    self.compile_stmt(sc, s)?;
                }
                sc.exit(mark);
                let ctx = self.loops.pop().unwrap();
                // `continue` re-tests the condition, so it targets the loop top.
                for j in &ctx.continues {
                    self.b.patch_jump(*j, start);
                }
                self.b.emit(Op::Jump(start), 0);
                let end = self.b.current_pos();
                self.b.patch_jump(jf, end);
                for j in &ctx.breaks {
                    self.b.patch_jump(*j, end);
                }
            }
            StmtKind::For {
                var,
                start,
                end,
                kind,
                step,
                body,
                label,
            } => {
                self.compile_for(sc, var, start, end, *kind, step, body, label)?;
            }
            StmtKind::Break(label) => {
                let j = self.b.emit(Op::Jump(0), 0);
                self.loop_for_label(label, s.line)?.breaks.push(j);
            }
            StmtKind::Continue(label) => {
                let j = self.b.emit(Op::Jump(0), 0);
                self.loop_for_label(label, s.line)?.continues.push(j);
            }
            StmtKind::If(ie) => {
                self.compile_if(sc, ie)?;
                self.b.emit(Op::Pop, ie.line); // statement position discards value
            }
            StmtKind::When(w) => {
                self.compile_when(sc, w)?;
                self.b.emit(Op::Pop, w.line); // statement position discards value
            }
            StmtKind::Expr(e) => {
                self.compile_expr(sc, e)?;
                self.b.emit(Op::Pop, 0);
            }
        }
        Ok(())
    }

    /// Resolve the [`LoopCtx`] a `break`/`continue` targets: the innermost loop
    /// for a bare form, or the nearest enclosing loop carrying `label`. Errors if
    /// used outside a loop or the label is unknown (both Kotlin compile errors).
    fn loop_for_label(
        &mut self,
        label: &Option<String>,
        line: u32,
    ) -> Result<&mut LoopCtx, String> {
        let idx = match label {
            Some(l) => self
                .loops
                .iter()
                .rposition(|c| c.label.as_deref() == Some(l.as_str()))
                .ok_or_else(|| format!("unresolved label: {l} (line {line})"))?,
            None => self
                .loops
                .len()
                .checked_sub(1)
                .ok_or_else(|| format!("break/continue outside a loop (line {line})"))?,
        };
        Ok(&mut self.loops[idx])
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
        label: &Option<String>,
    ) -> Result<(), String> {
        // The loop variable and end/step temporaries live in the loop's own
        // scope: they drop when the loop ends and are invisible afterward.
        let mark = sc.enter();
        // Loop counter (Kotlin's `for` variable is read-only `val`; the
        // compiler-emitted increment writes the slot directly, bypassing the
        // user-facing `val` reassignment check).
        let vslot = sc.declare(var, Type::Int, false);
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

        self.loops.push(LoopCtx {
            label: label.clone(),
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        for s in body {
            self.compile_stmt(sc, s)?;
        }
        let ctx = self.loops.pop().unwrap();
        // `continue` skips the rest of the body but still advances the counter,
        // so it targets the increment section below.
        let cont_target = self.b.current_pos();
        for j in &ctx.continues {
            self.b.patch_jump(*j, cont_target);
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
        for j in &ctx.breaks {
            self.b.patch_jump(*j, done);
        }
        sc.exit(mark);
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
            // A `Char` lowers to its integer code unit; the static `Char` type
            // carries display and Char-arithmetic semantics.
            Expr::Char(c) => {
                self.b.emit(Op::LoadInt(*c), 0);
                Ok(Type::Char)
            }
            // Kotlin `null` is fusevm `Undef`.
            Expr::Null => {
                self.b.emit(Op::LoadUndef, 0);
                Ok(Type::Unknown)
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
            Expr::Member {
                recv,
                name,
                safe,
                line,
            } => self.compile_member(sc, recv, name, &[], *safe, *line),
            Expr::MethodCall {
                recv,
                name,
                args,
                safe,
                line,
            } => self.compile_member(sc, recv, name, args, *safe, *line),
            Expr::Elvis { left, right } => self.compile_elvis(sc, left, right),
            Expr::NotNull(inner) => {
                let t = self.compile_expr(sc, inner)?;
                self.b.emit(Op::Extended(KT_NOTNULL, 0), 0);
                Ok(t)
            }
            Expr::If(ie) => self.compile_if(sc, ie),
            Expr::When(w) => self.compile_when(sc, w),
        }
    }

    /// Emit the ops that turn the top-of-stack value of static type `t` into its
    /// Kotlin `toString()` display form. `String` is already displayable;
    /// `Char` uses the char-coercion op; `Unit` becomes the literal
    /// `kotlin.Unit`; everything else routes through the generic coercion.
    fn emit_display(&mut self, t: Type) {
        match t {
            Type::String => {}
            Type::Char => {
                self.b.emit(Op::Extended(KT_CHR_STRING, 0), 0);
            }
            Type::Unit => {
                self.b.emit(Op::Pop, 0);
                let idx = self.b.add_constant(Value::str("kotlin.Unit"));
                self.b.emit(Op::LoadConst(idx), 0);
            }
            _ => {
                self.b.emit(Op::Extended(KT_TO_STRING, 0), 0);
            }
        }
    }

    /// Elvis `left ?: right`: evaluate `left`; if it is `null`, discard it and
    /// yield `right`, otherwise keep `left`.
    fn compile_elvis(&mut self, sc: &mut Scope, left: &Expr, right: &Expr) -> Result<Type, String> {
        let lt = self.compile_expr(sc, left)?; // [L]
        self.b.emit(Op::Dup, 0); // [L, L]
        self.b.emit(Op::Extended(KT_ISNULL, 0), 0); // [L, isNull]
                                                    // Not null → jump to end keeping L; null → fall through and replace.
        let jf = self.b.emit(Op::JumpIfFalse(0), 0); // pops isNull → [L]
        self.b.emit(Op::Pop, 0); // drop the null L → []
        let rt = self.compile_expr(sc, right)?; // [R]
        let end = self.b.current_pos();
        self.b.patch_jump(jf, end);
        Ok(if lt == rt { lt } else { Type::Unknown })
    }

    /// Lower a member/method access to a `KT_METHOD` host dispatch. The receiver
    /// and arguments are pushed deepest-first, then the member name (a `Str`
    /// constant) on top; the extension `arg` carries the argument count. A bare
    /// property read (`recv.property`) passes `args = []` (arg count 0).
    #[allow(clippy::too_many_arguments)]
    fn compile_member(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        args: &[Expr],
        safe: bool,
        line: u32,
    ) -> Result<Type, String> {
        // A safe call `recv?.member` short-circuits to null when the receiver is
        // null: evaluate the receiver into a slot, branch on null, and only
        // dispatch the member on the non-null path.
        if safe {
            return self.compile_safe_member(sc, recv, name, args, line);
        }
        // `Char.toString()` must render the character, not its code. The runtime
        // value is an `Int`, so the host's generic `toString` can't tell it is a
        // Char — resolve it statically here from the receiver's coarse type.
        if name == "toString" && args.is_empty() && self.infer(sc, recv) == Type::Char {
            self.compile_expr(sc, recv)?;
            self.b.emit(Op::Extended(KT_CHR_STRING, 0), line);
            return Ok(Type::String);
        }
        self.compile_expr(sc, recv)?;
        for a in args {
            self.compile_expr(sc, a)?;
        }
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(Op::Extended(KT_METHOD, args.len() as u8), line);
        Ok(method_ret_type(name))
    }

    /// Lower a safe member/method access `recv?.member(args)`. Evaluates the
    /// receiver into a temp slot; if it is null the whole access is null,
    /// otherwise it dispatches the member as usual.
    fn compile_safe_member(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        let mark = sc.enter();
        self.compile_expr(sc, recv)?; // [recv]
        let rslot = sc.temp();
        self.b.emit(Op::SetSlot(rslot), 0); // []
        self.b.emit(Op::GetSlot(rslot), 0); // [recv]
        self.b.emit(Op::Extended(KT_ISNULL, 0), 0); // [isNull]
                                                    // Not null → jump to the call; null → fall through to the null result.
        let jf = self.b.emit(Op::JumpIfFalse(0), line);
        self.b.emit(Op::LoadUndef, 0); // [null]
        let jend = self.b.emit(Op::Jump(0), 0);
        let call_pos = self.b.current_pos();
        self.b.patch_jump(jf, call_pos);
        self.b.emit(Op::GetSlot(rslot), 0); // [recv]
        for a in args {
            self.compile_expr(sc, a)?;
        }
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(Op::Extended(KT_METHOD, args.len() as u8), line);
        let end = self.b.current_pos();
        self.b.patch_jump(jend, end);
        sc.exit(mark);
        Ok(method_ret_type(name))
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
                    self.emit_display(t);
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
            self.emit_display(lt);
            self.compile_expr(sc, r)?;
            self.emit_display(rt);
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
        // Kotlin `Char` arithmetic: `Char + Int` / `Char - Int` → `Char`,
        // `Char - Char` → `Int`. Backed by the same integer ops; only the
        // result type (hence display) differs.
        let char_involved = lt == Type::Char || rt == Type::Char;
        let add_ty = if char_involved { Type::Char } else { num_ty };
        let sub_ty = if lt == Type::Char && rt == Type::Char {
            Type::Int
        } else if char_involved {
            Type::Char
        } else {
            num_ty
        };

        let ty = match op {
            BinOp::Add => {
                self.b.emit(Op::Add, 0);
                add_ty
            }
            BinOp::Sub => {
                self.b.emit(Op::Sub, 0);
                sub_ty
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
                    self.emit_display(t);
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
                        self.b
                            .emit(Op::Extended(KT_FFI_CALL, args.len() as u8), line);
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

    /// Lower a `when` to a chain of guard tests. The subject (if any) is
    /// evaluated once into a temp slot; each arm's conditions are tested in
    /// order and the first match runs the arm body, whose value becomes the
    /// `when`'s value. With no matching arm and no `else`, the value is `null`
    /// (Unit in statement position, discarded by the caller).
    fn compile_when(&mut self, sc: &mut Scope, w: &WhenExpr) -> Result<Type, String> {
        let mark = sc.enter();
        // Evaluate the subject once; remember its static type for `==` op choice.
        let subj = if let Some(subject) = &w.subject {
            let t = self.compile_expr(sc, subject)?;
            let slot = sc.temp();
            self.b.emit(Op::SetSlot(slot), 0);
            Some((slot, t))
        } else {
            None
        };

        let mut end_jumps: Vec<usize> = Vec::new();
        let mut result_ty: Option<Type> = None;
        let mut has_else = false;

        for arm in &w.arms {
            match &arm.guard {
                WhenGuard::Else => {
                    has_else = true;
                    let t = self.compile_block_value(sc, &arm.body)?;
                    result_ty = Some(join_ty(result_ty, t));
                    // `else` is terminal — later arms are unreachable.
                    break;
                }
                WhenGuard::Conds(conds) => {
                    // The arm matches if any condition holds: test each, jumping
                    // to the body on the first true; if none match, skip the body.
                    let mut hit_jumps: Vec<usize> = Vec::new();
                    for cond in conds {
                        self.compile_when_cond(sc, subj, cond)?; // [bool]
                        hit_jumps.push(self.b.emit(Op::JumpIfTrue(0), 0)); // pops bool
                    }
                    let skip = self.b.emit(Op::Jump(0), 0);
                    let body_pos = self.b.current_pos();
                    for j in hit_jumps {
                        self.b.patch_jump(j, body_pos);
                    }
                    let t = self.compile_block_value(sc, &arm.body)?;
                    result_ty = Some(join_ty(result_ty, t));
                    end_jumps.push(self.b.emit(Op::Jump(0), 0));
                    let next = self.b.current_pos();
                    self.b.patch_jump(skip, next);
                }
            }
        }
        // Non-exhaustive fallthrough: the value is `null` (Undef).
        if !has_else {
            self.b.emit(Op::LoadUndef, 0);
            result_ty = Some(join_ty(result_ty, Type::Unit));
        }
        let end = self.b.current_pos();
        for j in end_jumps {
            self.b.patch_jump(j, end);
        }
        sc.exit(mark);
        Ok(result_ty.unwrap_or(Type::Unit))
    }

    /// Compile one `when` arm condition, leaving a `Bool` on the stack.
    ///
    /// Subject form (`subj` is `Some`): `Expr` is an equality against the
    /// subject, `InRange` a range-membership test, `Is` a runtime type check.
    /// Subjectless form (`subj` is `None`): `Expr` is a standalone boolean.
    fn compile_when_cond(
        &mut self,
        sc: &mut Scope,
        subj: Option<(u16, Type)>,
        cond: &WhenCond,
    ) -> Result<(), String> {
        match cond {
            WhenCond::Expr(e) => match subj {
                Some((slot, sty)) => {
                    self.b.emit(Op::GetSlot(slot), 0);
                    let et = self.compile_expr(sc, e)?;
                    let str_eq = sty == Type::String || et == Type::String;
                    self.b.emit(if str_eq { Op::StrEq } else { Op::NumEq }, 0);
                }
                None => {
                    self.compile_expr(sc, e)?;
                }
            },
            WhenCond::InRange {
                negated,
                start,
                end,
                kind,
            } => {
                let (slot, _) = subj.ok_or("`in` condition requires a `when` subject")?;
                // subject >= lo AND subject <= hi (orientation depends on `kind`).
                let (lo_cmp, hi_cmp) = match kind {
                    RangeKind::Inclusive => (Op::NumGe, Op::NumLe),
                    RangeKind::Until => (Op::NumGe, Op::NumLt),
                    RangeKind::DownTo => (Op::NumLe, Op::NumGe),
                };
                self.b.emit(Op::GetSlot(slot), 0);
                self.compile_expr(sc, start)?;
                self.b.emit(lo_cmp, 0);
                self.b.emit(Op::GetSlot(slot), 0);
                self.compile_expr(sc, end)?;
                self.b.emit(hi_cmp, 0);
                self.b.emit(Op::LogAnd, 0);
                if *negated {
                    self.b.emit(Op::LogNot, 0);
                }
            }
            WhenCond::Is { negated, ty } => {
                let (slot, _) = subj.ok_or("`is` condition requires a `when` subject")?;
                self.b.emit(Op::GetSlot(slot), 0);
                let nidx = self.b.add_constant(Value::str(ty.clone()));
                self.b.emit(Op::LoadConst(nidx), 0);
                self.b.emit(Op::Extended(KT_IS, 0), 0);
                if *negated {
                    self.b.emit(Op::LogNot, 0);
                }
            }
        }
        Ok(())
    }

    /// Compile a branch body leaving exactly one value: the last statement's
    /// expression value, or `Undef` (Unit). The body is its own lexical scope —
    /// bindings it declares drop at the block's end (see [`Scope::enter`]).
    fn compile_block_value(&mut self, sc: &mut Scope, body: &[Stmt]) -> Result<Type, String> {
        let mark = sc.enter();
        let res = self.compile_block_value_inner(sc, body);
        sc.exit(mark);
        res
    }

    fn compile_block_value_inner(&mut self, sc: &mut Scope, body: &[Stmt]) -> Result<Type, String> {
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
            StmtKind::When(w) => {
                mark(self);
                self.compile_when(sc, w)
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
            Expr::Char(_) => Type::Char,
            Expr::Null => Type::Unknown,
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
                    } else if lt == Type::Char || rt == Type::Char {
                        Type::Char // Char + Int → Char
                    } else if lt == Type::Double || rt == Type::Double {
                        Type::Double
                    } else {
                        Type::Int
                    }
                }
                BinOp::Sub => {
                    let lt = self.infer(sc, l);
                    let rt = self.infer(sc, r);
                    if lt == Type::Char && rt == Type::Char {
                        Type::Int // Char - Char → Int
                    } else if lt == Type::Char || rt == Type::Char {
                        Type::Char // Char - Int → Char
                    } else if lt == Type::Double || rt == Type::Double {
                        Type::Double
                    } else {
                        Type::Int
                    }
                }
                BinOp::Mul | BinOp::Div | BinOp::Mod => {
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
            Expr::Member { name, .. } | Expr::MethodCall { name, .. } => method_ret_type(name),
            Expr::Elvis { left, right } => {
                let lt = self.infer(sc, left);
                let rt = self.infer(sc, right);
                if lt == rt {
                    lt
                } else {
                    Type::Unknown
                }
            }
            Expr::NotNull(inner) => self.infer(sc, inner),
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
            // `when`'s result type isn't statically joined here (arms can be
            // heterogeneous); leave it `Unknown` so display routes through the
            // generic coercion, which is correct for the common Int/String cases.
            Expr::When(_) => Type::Unknown,
        }
    }

    fn infer_stmt(&self, sc: &Scope, s: &Stmt) -> Type {
        match &s.kind {
            StmtKind::Expr(e) => self.infer(sc, e),
            StmtKind::If(ie) => self.infer(sc, &Expr::If(ie.clone())),
            StmtKind::When(w) => self.infer(sc, &Expr::When(w.clone())),
            _ => Type::Unit,
        }
    }
}

/// Join two coarse branch types: identical types collapse to that type, an
/// absent prior type adopts the new one, and any mismatch widens to `Unknown`.
fn join_ty(prev: Option<Type>, next: Type) -> Type {
    match prev {
        None => next,
        Some(t) if t == next => t,
        Some(_) => Type::Unknown,
    }
}

/// Static return type of a Kotlin stdlib member/method, mirroring the runtime
/// dispatch in [`crate::host::kt_method`]. Members not modeled here fall back to
/// `Unknown` (they still dispatch; only static typing of the result is coarse).
fn method_ret_type(name: &str) -> Type {
    match name {
        "length" | "code" => Type::Int,
        "isEmpty" | "isNotEmpty" => Type::Boolean,
        "toChar" => Type::Char,
        "uppercase" | "toUpperCase" | "lowercase" | "toLowerCase" | "trim" | "toString" => {
            Type::String
        }
        _ => Type::Unknown,
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
        StmtKind::While { cond, body, .. } => expr_has_ffi(cond) || body_has_ffi(body),
        StmtKind::For {
            start, end, body, ..
        } => expr_has_ffi(start) || expr_has_ffi(end) || body_has_ffi(body),
        StmtKind::Break(_) | StmtKind::Continue(_) => false,
        StmtKind::If(ie) => if_has_ffi(ie),
        StmtKind::When(w) => when_has_ffi(w),
        StmtKind::Expr(e) => expr_has_ffi(e),
    })
}

fn if_has_ffi(ie: &IfExpr) -> bool {
    expr_has_ffi(&ie.cond) || body_has_ffi(&ie.then) || ie.els.as_deref().is_some_and(body_has_ffi)
}

fn when_has_ffi(w: &WhenExpr) -> bool {
    w.subject.as_deref().is_some_and(expr_has_ffi)
        || w.arms.iter().any(|arm| {
            body_has_ffi(&arm.body)
                || match &arm.guard {
                    WhenGuard::Else => false,
                    WhenGuard::Conds(conds) => conds.iter().any(|c| match c {
                        WhenCond::Expr(e) => expr_has_ffi(e),
                        WhenCond::InRange { start, end, .. } => {
                            expr_has_ffi(start) || expr_has_ffi(end)
                        }
                        WhenCond::Is { .. } => false,
                    }),
                }
        })
}

fn expr_has_ffi(e: &Expr) -> bool {
    match e {
        Expr::Call { name, args, .. } => name == RUST_COMPILE || args.iter().any(expr_has_ffi),
        Expr::Member { recv, .. } => expr_has_ffi(recv),
        Expr::MethodCall { recv, args, .. } => expr_has_ffi(recv) || args.iter().any(expr_has_ffi),
        Expr::Unary { expr, .. } => expr_has_ffi(expr),
        Expr::Binary { l, r, .. } => expr_has_ffi(l) || expr_has_ffi(r),
        Expr::Elvis { left, right } => expr_has_ffi(left) || expr_has_ffi(right),
        Expr::NotNull(inner) => expr_has_ffi(inner),
        Expr::If(ie) => if_has_ffi(ie),
        Expr::When(w) => when_has_ffi(w),
        Expr::Str(parts) => parts.iter().any(|p| match p {
            StrExpr::Expr(e) => expr_has_ffi(e),
            StrExpr::Text(_) => false,
        }),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Char(_)
        | Expr::Null
        | Expr::Var(_) => false,
    }
}
