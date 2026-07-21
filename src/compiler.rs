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
    KT_CHR_STRING, KT_DBG_LINE, KT_FFI_CALL, KT_FFI_COMPILE, KT_GETFIELD, KT_IDIV, KT_IMOD,
    KT_INDEX_GET, KT_INDEX_SET, KT_IS, KT_ISNULL, KT_LIST, KT_LISTPUSH, KT_MAP, KT_METHOD, KT_NEW,
    KT_NEWLIST, KT_NOTNULL, KT_OBJEQ, KT_PAIR, KT_SETFIELD, KT_TO_STRING,
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
    /// The class/container name when `ty == Type::Obj` (e.g. `Person`, `List`),
    /// so member access on this binding can dispatch to the right method sub.
    class: Option<String>,
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
        self.declare_obj(name, ty, mutable, None)
    }
    /// Declare a binding that may carry a class/container name (`Type::Obj`).
    fn declare_obj(&mut self, name: &str, ty: Type, mutable: bool, class: Option<String>) -> u16 {
        let slot = self.next_slot;
        self.next_slot += 1;
        let prev = self.map.insert(
            name.to_string(),
            Binding {
                slot,
                ty,
                mutable,
                class,
            },
        );
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
    /// The class/container name bound to `name`, if any.
    fn class_of(&self, name: &str) -> Option<String> {
        self.map.get(name).and_then(|b| b.class.clone())
    }
    /// Whether `name` is currently bound as a reassignable `var`.
    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.map.get(name).map(|b| b.mutable)
    }
}

/// A stored property of a class (or `object`).
#[derive(Clone)]
struct PropMeta {
    name: String,
    ty: Type,
    class: Option<String>,
    mutable: bool,
}

/// Static signature of a user function or class method.
#[derive(Clone)]
struct FnSig {
    ret: Type,
    ret_class: Option<String>,
    arity: usize,
}

/// Compile-time metadata for a `class` / `data class` / `object`, driving
/// constructor lowering, field access, method dispatch, and (for `data`)
/// synthesized-member routing.
#[derive(Clone)]
struct ClassMeta {
    name: String,
    is_data: bool,
    is_object: bool,
    props: Vec<PropMeta>,
    /// method name → its signature; the sub is named `Class#method`.
    methods: HashMap<String, FnSig>,
}

impl ClassMeta {
    fn prop(&self, name: &str) -> Option<&PropMeta> {
        self.props.iter().find(|p| p.name == name)
    }
    /// The `KT_NEW` metadata string: `"Name\x1f(d|c)\x1ffield0\x1f…"`.
    fn meta_string(&self) -> String {
        let mut s = self.name.clone();
        s.push('\u{1f}');
        s.push(if self.is_data { 'd' } else { 'c' });
        for p in &self.props {
            s.push('\u{1f}');
            s.push_str(&p.name);
        }
        s
    }
}

/// The mangled sub name for a class method (`Person#greet`).
fn method_sub_name(class: &str, method: &str) -> String {
    format!("{class}#{method}")
}

pub struct Compiler {
    b: ChunkBuilder,
    /// name → signature for user functions, filled before lowering.
    fun_sig: HashMap<String, FnSig>,
    /// class/object name → metadata, filled before lowering.
    classes: HashMap<String, ClassMeta>,
    /// The class whose method is currently being lowered (enables implicit
    /// `this` for member/method access). `None` at top level and in free funcs.
    cur_class: Option<String>,
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
pub fn compile(program: &Program) -> Result<Chunk, String> {
    compile_with(program, false)
}

/// Compile with per-statement DAP line markers enabled (`kotlin --dap`).
pub fn compile_debug(program: &Program) -> Result<Chunk, String> {
    compile_with(program, true)
}

/// Compile a program to a runnable chunk, optionally instrumented with debug
/// line markers. Requires a `fun main`.
pub fn compile_with(program: &Program, debug: bool) -> Result<Chunk, String> {
    let mut fun_sig = HashMap::new();
    for f in &program.funs {
        fun_sig.insert(
            f.name.clone(),
            FnSig {
                ret: f.ret,
                ret_class: f.ret_class.clone(),
                arity: f.params.len(),
            },
        );
    }

    // Build class/object metadata before lowering so calls, constructors, and
    // member access can resolve against it regardless of declaration order.
    let mut classes: HashMap<String, ClassMeta> = HashMap::new();
    for cd in &program.classes {
        let props: Vec<PropMeta> = if cd.is_object {
            cd.obj_props
                .iter()
                .map(|(n, ty, class, _)| PropMeta {
                    name: n.clone(),
                    ty: *ty,
                    class: class.clone(),
                    mutable: true,
                })
                .collect()
        } else {
            cd.params
                .iter()
                .filter(|p| p.kind != PropKind::None)
                .map(|p| PropMeta {
                    name: p.name.clone(),
                    ty: p.ty,
                    class: p.class.clone(),
                    mutable: p.kind == PropKind::Var,
                })
                .collect()
        };
        let methods = cd
            .methods
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    FnSig {
                        ret: m.ret,
                        ret_class: m.ret_class.clone(),
                        arity: m.params.len(),
                    },
                )
            })
            .collect();
        if classes
            .insert(
                cd.name.clone(),
                ClassMeta {
                    name: cd.name.clone(),
                    is_data: cd.is_data,
                    is_object: cd.is_object,
                    props,
                    methods,
                },
            )
            .is_some()
        {
            return Err(format!("conflicting declarations for class {}", cd.name));
        }
    }

    let main = program
        .funs
        .iter()
        .find(|f| f.name == "main")
        .ok_or("no `fun main` found")?;

    let has_ffi = program.funs.iter().any(|f| body_has_ffi(&f.body));

    let mut c = Compiler {
        b: ChunkBuilder::new(),
        fun_sig,
        classes,
        cur_class: None,
        debug,
        has_ffi,
        loops: Vec::new(),
    };

    // Preamble: build `object` singletons into globals, then bind main's args
    // (an empty Array per declared parameter — the program-args wiring is an M0
    // stub), call main, discard its Unit, skip the bodies.
    for cd in &program.classes {
        if cd.is_object {
            c.build_object(cd)?;
        }
    }
    let main_idx = c.b.add_name("main");
    for _ in &main.params {
        c.b.emit(Op::MakeArray(0), main.line);
    }
    c.b.emit(Op::Call(main_idx, main.params.len() as u8), main.line);
    c.b.emit(Op::Pop, main.line);
    let end_jump = c.b.emit(Op::Jump(0), main.line);

    for f in &program.funs {
        c.compile_fun(f, None)?;
    }
    // Class/object methods lower as subs named `Class#method`, with `this`
    // (slot 0) as an implicit first parameter of the enclosing class type.
    for cd in &program.classes {
        for m in &cd.methods {
            c.compile_fun(m, Some(&cd.name))?;
        }
    }

    let end = c.b.current_pos();
    c.b.patch_jump(end_jump, end);
    Ok(c.b.build())
}

impl Compiler {
    /// Evaluate an `object`'s property initializers and construct its singleton
    /// once, storing the handle in a global named after the object.
    fn build_object(&mut self, cd: &ClassDecl) -> Result<(), String> {
        let meta = self.classes[&cd.name].clone();
        let meta_idx = self.b.add_constant(Value::str(meta.meta_string()));
        self.b.emit(Op::LoadConst(meta_idx), cd.line);
        let mut sc = Scope::new();
        for (_, _, _, init) in &cd.obj_props {
            self.compile_expr(&mut sc, init)?;
        }
        self.b
            .emit(Op::Extended(KT_NEW, meta.props.len() as u8), cd.line);
        let g = self.b.add_name(&cd.name);
        self.b.emit(Op::SetVar(g), cd.line);
        Ok(())
    }

    /// Lower a free function (`class` = `None`) or a class method (`class` =
    /// `Some(name)`, adding an implicit `this` in slot 0).
    fn compile_fun(&mut self, f: &FunDecl, class: Option<&str>) -> Result<(), String> {
        let entry = self.b.current_pos();
        let sub_name = match class {
            Some(cls) => method_sub_name(cls, &f.name),
            None => f.name.clone(),
        };
        let name_idx = self.b.add_name(&sub_name);
        self.b.add_sub_entry(name_idx, entry);

        let mut sc = Scope::new();
        let mut nslots = f.params.len();
        // A method receives `this` (the instance handle) as arg 0.
        if let Some(cls) = class {
            sc.declare_obj("this", Type::Obj, false, Some(cls.to_string()));
            nslots += 1;
        }
        // Parameters occupy the following slots in declaration order. Kotlin
        // function parameters are read-only (`val`), so declared immutable.
        for p in &f.params {
            sc.declare_obj(&p.name, p.ty, false, p.class.clone());
        }
        // Bind args (stack top = last arg) into slots, deepest last.
        for i in (0..nslots).rev() {
            self.b.emit(Op::SetSlot(i as u16), f.line);
        }

        self.cur_class = class.map(|s| s.to_string());
        let res: Result<(), String> = (|| {
            for s in &f.body {
                self.compile_stmt(&mut sc, s)?;
            }
            Ok(())
        })();
        self.cur_class = None;
        res?;
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
                let class = self.infer_class(sc, init);
                let it = self.compile_expr(sc, init)?;
                let mut vty = ty.unwrap_or(it);
                // A binding with a known class/container is a heap object.
                if class.is_some() {
                    vty = Type::Obj;
                }
                let slot = sc.declare_obj(name, vty, *mutable, class);
                self.b.emit(Op::SetSlot(slot), 0);
            }
            StmtKind::Assign { name, op, value } => {
                // A `val` (write-once) binding cannot be reassigned — Kotlin
                // reports this at compile time.
                if sc.is_mutable(name) == Some(false) {
                    return Err(format!("val cannot be reassigned: {name}"));
                }
                // A bare `name = …` that is not a local but is a property of the
                // enclosing class is an implicit-`this` field write.
                if sc.slot(name).is_none() {
                    if let Some(cls) = self.cur_class.clone() {
                        if self
                            .classes
                            .get(&cls)
                            .is_some_and(|m| m.prop(name).is_some())
                        {
                            return self.compile_set_member(
                                sc,
                                &Expr::Var("this".into()),
                                name,
                                op,
                                value,
                            );
                        }
                    }
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
            StmtKind::SetMember {
                recv,
                name,
                op,
                value,
            } => self.compile_set_member(sc, recv, name, op, value)?,
            StmtKind::SetIndex {
                recv,
                index,
                op,
                value,
            } => self.compile_set_index(sc, recv, index, op, value)?,
            StmtKind::Destructure { names, init } => self.compile_destructure(sc, names, init)?,
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
                if let Some(slot) = sc.slot(name) {
                    self.b.emit(Op::GetSlot(slot), 0);
                    return Ok(sc.ty(name));
                }
                // Implicit `this`: a bare name that is a property of the class
                // whose method we're lowering resolves to `this.name`.
                if let Some(cls) = self.cur_class.clone() {
                    if self
                        .classes
                        .get(&cls)
                        .is_some_and(|m| m.prop(name).is_some())
                    {
                        return self.compile_member(
                            sc,
                            &Expr::Var("this".into()),
                            name,
                            &[],
                            false,
                            0,
                        );
                    }
                }
                // A bare reference to an `object` singleton loads its global.
                if self.classes.get(name).is_some_and(|m| m.is_object) {
                    let g = self.b.add_name(name);
                    self.b.emit(Op::GetVar(g), 0);
                    return Ok(Type::Obj);
                }
                Err(format!("unresolved reference: {name}"))
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
            Expr::Index { recv, index, line } => {
                self.compile_expr(sc, recv)?;
                self.compile_expr(sc, index)?;
                self.b.emit(Op::Extended(KT_INDEX_GET, 0), *line);
                Ok(Type::Unknown)
            }
            Expr::Pair { first, second } => {
                self.compile_expr(sc, first)?;
                self.compile_expr(sc, second)?;
                self.b.emit(Op::Extended(KT_PAIR, 0), 0);
                Ok(Type::Obj)
            }
            // A lambda is only meaningful inlined into a collection HOF (handled
            // in `compile_member`); standalone it has no first-class value here.
            Expr::Lambda { .. } => {
                Err("a lambda is only supported as a map/filter/forEach argument".into())
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
        // Collection higher-order functions — the compiler inlines the lambda.
        if let ["map" | "filter" | "forEach"] = [name] {
            if let [Expr::Lambda { params, body }] = args {
                return self.compile_hof(sc, recv, name, params, body, line);
            }
        }
        // A statically-known user class: dispatch a user method as a native
        // `Op::Call`, read a property directly, or route a `data` member.
        if let Some(cls) = self.infer_class(sc, recv) {
            if let Some(meta) = self.classes.get(&cls).cloned() {
                // A user-declared method → direct call on the `Class#method` sub,
                // pushing `this` (the receiver) as arg 0.
                if let Some(sig) = meta.methods.get(name) {
                    if args.len() != sig.arity {
                        return Err(format!(
                            "method {name} on {cls} expects {} argument(s), got {}",
                            sig.arity,
                            args.len()
                        ));
                    }
                    self.compile_expr(sc, recv)?;
                    for a in args {
                        self.compile_expr(sc, a)?;
                    }
                    let idx = self.b.add_name(&method_sub_name(&cls, name));
                    self.b.emit(Op::Call(idx, (sig.arity + 1) as u8), line);
                    return Ok(sig.ret);
                }
                // A stored property read.
                if args.is_empty() {
                    if let Some(p) = meta.prop(name) {
                        self.compile_expr(sc, recv)?;
                        let nidx = self.b.add_constant(Value::str(name.to_string()));
                        self.b.emit(Op::LoadConst(nidx), line);
                        self.b.emit(Op::Extended(KT_GETFIELD, 0), line);
                        return Ok(p.ty);
                    }
                }
                // `data class` synthesized `copy(...)` — clone with positional
                // overrides applied in declaration order.
                if meta.is_data && name == "copy" {
                    return self.compile_copy(sc, recv, &meta, args, line);
                }
                // Other members (`toString`/`equals`/`hashCode`/`componentN`)
                // fall through to the host dispatch below.
            }
        }
        self.emit_kt_method(sc, recv, name, args, line)
    }

    /// Emit a generic `KT_METHOD` host dispatch: push the receiver, then the
    /// arguments deepest-first, then the member name, and dispatch.
    fn emit_kt_method(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        self.compile_expr(sc, recv)?;
        for a in args {
            self.compile_expr(sc, a)?;
        }
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(Op::Extended(KT_METHOD, args.len() as u8), line);
        Ok(method_ret_type(name))
    }

    /// Lower a `data class` `copy(...)`: build a fresh instance whose fields are
    /// the receiver's, with the positional arguments overriding the leading
    /// properties (`p.copy(newX)` overrides the first property only).
    fn compile_copy(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        meta: &ClassMeta,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        if args.len() > meta.props.len() {
            return Err(format!(
                "{}.copy takes at most {} argument(s)",
                meta.name,
                meta.props.len()
            ));
        }
        let mark = sc.enter();
        self.compile_expr(sc, recv)?; // [recv]
        let rslot = sc.temp();
        self.b.emit(Op::SetSlot(rslot), 0);
        let meta_idx = self.b.add_constant(Value::str(meta.meta_string()));
        self.b.emit(Op::LoadConst(meta_idx), line);
        for (i, p) in meta.props.iter().enumerate() {
            match args.get(i) {
                Some(a) => {
                    self.compile_expr(sc, a)?;
                }
                None => {
                    // Keep the receiver's current value for this property.
                    self.b.emit(Op::GetSlot(rslot), 0);
                    let nidx = self.b.add_constant(Value::str(p.name.clone()));
                    self.b.emit(Op::LoadConst(nidx), 0);
                    self.b.emit(Op::Extended(KT_GETFIELD, 0), 0);
                }
            }
        }
        self.b
            .emit(Op::Extended(KT_NEW, meta.props.len() as u8), line);
        sc.exit(mark);
        Ok(Type::Obj)
    }

    /// Inline a collection higher-order call (`map`/`filter`/`forEach`) over
    /// `recv` with the given lambda. `map`/`filter` build a fresh accumulator
    /// list; `forEach` runs the body for effect and yields Unit.
    fn compile_hof(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        kind: &str,
        params: &[String],
        body: &[Stmt],
        _line: u32,
    ) -> Result<Type, String> {
        if params.len() > 1 {
            return Err(format!("{kind} lambda takes a single parameter"));
        }
        let pname = params.first().map(String::as_str).unwrap_or("it");
        let collects = kind != "forEach";
        let mark = sc.enter();

        // src, size, index (and accumulator for map/filter) in temp slots.
        self.compile_expr(sc, recv)?;
        let src = sc.temp();
        self.b.emit(Op::SetSlot(src), 0);
        self.b.emit(Op::GetSlot(src), 0);
        let sz_name = self.b.add_constant(Value::str("size"));
        self.b.emit(Op::LoadConst(sz_name), 0);
        self.b.emit(Op::Extended(KT_METHOD, 0), 0);
        let szslot = sc.temp();
        self.b.emit(Op::SetSlot(szslot), 0);
        let islot = sc.temp();
        self.b.emit(Op::LoadInt(0), 0);
        self.b.emit(Op::SetSlot(islot), 0);
        let accslot = if collects {
            let a = sc.temp();
            self.b.emit(Op::Extended(KT_NEWLIST, 0), 0);
            self.b.emit(Op::SetSlot(a), 0);
            Some(a)
        } else {
            None
        };
        // The lambda parameter gets a fixed slot, reassigned each iteration.
        let pslot = sc.declare(pname, Type::Unknown, false);

        let top = self.b.current_pos();
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::GetSlot(szslot), 0);
        self.b.emit(Op::NumLt, 0);
        let jf = self.b.emit(Op::JumpIfFalse(0), 0);

        // it = src[i]
        self.b.emit(Op::GetSlot(src), 0);
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::Extended(KT_INDEX_GET, 0), 0);
        self.b.emit(Op::SetSlot(pslot), 0);

        // Run the lambda body, leaving its value on the stack.
        self.compile_block_value(sc, body)?;
        match kind {
            "forEach" => {
                self.b.emit(Op::Pop, 0);
            }
            "map" => {
                // [v] → [acc, v] → append
                self.b.emit(Op::GetSlot(accslot.unwrap()), 0);
                self.b.emit(Op::Swap, 0);
                self.b.emit(Op::Extended(KT_LISTPUSH, 0), 0);
            }
            "filter" => {
                // [bool]: on true, append the current element to the accumulator.
                let skip = self.b.emit(Op::JumpIfFalse(0), 0);
                self.b.emit(Op::GetSlot(accslot.unwrap()), 0);
                self.b.emit(Op::GetSlot(pslot), 0);
                self.b.emit(Op::Extended(KT_LISTPUSH, 0), 0);
                let after = self.b.current_pos();
                self.b.patch_jump(skip, after);
            }
            _ => unreachable!(),
        }

        // i += 1
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::LoadInt(1), 0);
        self.b.emit(Op::Add, 0);
        self.b.emit(Op::SetSlot(islot), 0);
        self.b.emit(Op::Jump(top), 0);
        let end = self.b.current_pos();
        self.b.patch_jump(jf, end);

        let ty = match accslot {
            Some(a) => {
                self.b.emit(Op::GetSlot(a), 0);
                Type::Obj
            }
            None => {
                self.b.emit(Op::LoadUndef, 0);
                Type::Unit
            }
        };
        sc.exit(mark);
        Ok(ty)
    }

    /// `recv.field (op)= value` — an object property write.
    fn compile_set_member(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        op: &Option<BinOp>,
        value: &Expr,
    ) -> Result<(), String> {
        // A `val` property cannot be reassigned (Kotlin compile-time error).
        if let Some(cls) = self.infer_class(sc, recv) {
            if let Some(p) = self.classes.get(&cls).and_then(|m| m.prop(name)) {
                if !p.mutable {
                    return Err(format!("val cannot be reassigned: {name}"));
                }
            }
        }
        self.compile_expr(sc, recv)?; // [obj]
        let store = self.compound_value(recv, name, None, op, value);
        self.compile_expr(sc, &store)?; // [obj, newval]
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), 0);
        self.b.emit(Op::Extended(KT_SETFIELD, 0), 0);
        Ok(())
    }

    /// `recv[index] (op)= value` — an indexed write.
    fn compile_set_index(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        index: &Expr,
        op: &Option<BinOp>,
        value: &Expr,
    ) -> Result<(), String> {
        self.compile_expr(sc, recv)?; // [recv]
        self.compile_expr(sc, index)?; // [recv, index]
        let store = self.compound_value(recv, "", Some(index), op, value);
        self.compile_expr(sc, &store)?; // [recv, index, value]
        self.b.emit(Op::Extended(KT_INDEX_SET, 0), 0);
        Ok(())
    }

    /// Build the value expression for a (possibly compound) assignment. For a
    /// plain `=` it is just `value`; for `op=` it is `target op value`, where
    /// `target` is the member (`index == None`) or the indexed access.
    fn compound_value(
        &self,
        recv: &Expr,
        name: &str,
        index: Option<&Expr>,
        op: &Option<BinOp>,
        value: &Expr,
    ) -> Expr {
        match op {
            None => value.clone(),
            Some(binop) => {
                let target = match index {
                    Some(ix) => Expr::Index {
                        recv: Box::new(recv.clone()),
                        index: Box::new(ix.clone()),
                        line: 0,
                    },
                    None => Expr::Member {
                        recv: Box::new(recv.clone()),
                        name: name.to_string(),
                        safe: false,
                        line: 0,
                    },
                };
                Expr::Binary {
                    op: *binop,
                    l: Box::new(target),
                    r: Box::new(value.clone()),
                }
            }
        }
    }

    /// `val (a, b, …) = expr` — bind each name to `expr.componentN` (1-based).
    fn compile_destructure(
        &mut self,
        sc: &mut Scope,
        names: &[String],
        init: &Expr,
    ) -> Result<(), String> {
        self.compile_expr(sc, init)?; // [val]
        let tslot = sc.temp();
        self.b.emit(Op::SetSlot(tslot), 0);
        for (i, nm) in names.iter().enumerate() {
            if nm == "_" {
                continue; // `_` discards the component
            }
            self.b.emit(Op::GetSlot(tslot), 0);
            let cidx = self
                .b
                .add_constant(Value::str(format!("component{}", i + 1)));
            self.b.emit(Op::LoadConst(cidx), 0);
            self.b.emit(Op::Extended(KT_METHOD, 0), 0);
            let slot = sc.declare(nm, Type::Unknown, false);
            self.b.emit(Op::SetSlot(slot), 0);
        }
        Ok(())
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

        // `==`/`!=` on a heap object is structural equality (data-class,
        // List/Map/Pair), not the numeric/pointer compare of the native ops.
        if matches!(op, BinOp::Eq | BinOp::Ne) && (lt == Type::Obj || rt == Type::Obj) {
            self.compile_expr(sc, l)?;
            self.compile_expr(sc, r)?;
            self.b.emit(Op::Extended(KT_OBJEQ, 0), 0);
            if op == BinOp::Ne {
                self.b.emit(Op::LogNot, 0);
            }
            return Ok(Type::Boolean);
        }

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
            // Collection builders → heap objects.
            "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_LIST, args.len() as u8), line);
                Ok(Type::Obj)
            }
            "mapOf" | "mutableMapOf" | "hashMapOf" | "emptyMap" => {
                // Each argument is a `k to v` Pair.
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_MAP, args.len() as u8), line);
                Ok(Type::Obj)
            }
            _ => {
                // A constructor call `Class(args)`.
                if let Some(meta) = self.classes.get(name).cloned() {
                    return self.compile_construct(sc, &meta, args, line);
                }
                // A free user function.
                if let Some(sig) = self.fun_sig.get(name).cloned() {
                    if args.len() != sig.arity {
                        return Err(format!(
                            "function {name} expects {} argument(s), got {}",
                            sig.arity,
                            args.len()
                        ));
                    }
                    for a in args {
                        self.compile_expr(sc, a)?;
                    }
                    let idx = self.b.add_name(name);
                    self.b.emit(Op::Call(idx, sig.arity as u8), line);
                    return Ok(sig.ret);
                }
                // Implicit `this.method(args)` inside a class method.
                if let Some(cls) = self.cur_class.clone() {
                    if self
                        .classes
                        .get(&cls)
                        .is_some_and(|m| m.methods.contains_key(name))
                    {
                        return self.compile_member(
                            sc,
                            &Expr::Var("this".into()),
                            name,
                            args,
                            false,
                            line,
                        );
                    }
                }
                // Unknown name. With a `rust { ... }` block present it may be an
                // FFI export registered at runtime, so lower to a by-name FFI
                // dispatch; the args are pushed deepest-first, then the name.
                // Without any FFI block, it stays a compile-time error.
                if self.has_ffi {
                    for a in args {
                        self.compile_expr(sc, a)?;
                    }
                    let nidx = self.b.add_constant(Value::str(name.to_string()));
                    self.b.emit(Op::LoadConst(nidx), line);
                    self.b
                        .emit(Op::Extended(KT_FFI_CALL, args.len() as u8), line);
                    return Ok(Type::Unknown);
                }
                Err(format!("unresolved reference: {name}"))
            }
        }
    }

    /// Lower a constructor call `Class(args)`: push the class metadata string,
    /// then each stored-property value in declaration order, then `KT_NEW`. Only
    /// `val`/`var` primary-constructor params are stored; plain params are not
    /// modeled (they carry no property).
    fn compile_construct(
        &mut self,
        sc: &mut Scope,
        meta: &ClassMeta,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        if meta.is_object {
            return Err(format!("cannot construct object {}", meta.name));
        }
        if args.len() != meta.props.len() {
            return Err(format!(
                "constructor {} expects {} argument(s), got {}",
                meta.name,
                meta.props.len(),
                args.len()
            ));
        }
        let meta_idx = self.b.add_constant(Value::str(meta.meta_string()));
        self.b.emit(Op::LoadConst(meta_idx), line);
        for a in args {
            self.compile_expr(sc, a)?;
        }
        self.b
            .emit(Op::Extended(KT_NEW, meta.props.len() as u8), line);
        Ok(Type::Obj)
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
                "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" | "mapOf"
                | "mutableMapOf" | "hashMapOf" | "emptyMap" => Type::Obj,
                _ if self.classes.contains_key(name) => Type::Obj, // constructor
                _ => self
                    .fun_sig
                    .get(name)
                    .map(|s| s.ret)
                    .unwrap_or(Type::Unknown),
            },
            Expr::Index { .. } => Type::Unknown,
            Expr::Pair { .. } => Type::Obj,
            Expr::Lambda { .. } => Type::Unknown,
            Expr::Member { recv, name, .. } => {
                // A property read on a known class yields the property's type.
                if let Some(cls) = self.infer_class(sc, recv) {
                    if let Some(p) = self.classes.get(&cls).and_then(|m| m.prop(name)) {
                        return p.ty;
                    }
                }
                method_ret_type(name)
            }
            Expr::MethodCall { recv, name, .. } => {
                // `map`/`filter` yield a `List` (heap object).
                if matches!(name.as_str(), "map" | "filter") {
                    return Type::Obj;
                }
                if let Some(cls) = self.infer_class(sc, recv) {
                    if let Some(sig) = self.classes.get(&cls).and_then(|m| m.methods.get(name)) {
                        return sig.ret;
                    }
                }
                method_ret_type(name)
            }
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

    /// The class/container name of an expression's value, when statically known:
    /// a bound variable's class, a constructor call, `this`, a class-typed
    /// function return, or a class-typed property/method result. Drives method
    /// dispatch and property typing.
    fn infer_class(&self, sc: &Scope, e: &Expr) -> Option<String> {
        match e {
            Expr::Var(n) => {
                if n == "this" {
                    return self.cur_class.clone().or_else(|| sc.class_of(n));
                }
                if let Some(c) = sc.class_of(n) {
                    return Some(c);
                }
                // Unbound but a property of the enclosing class → implicit this.
                if let Some(cls) = &self.cur_class {
                    if let Some(p) = self.classes.get(cls).and_then(|m| m.prop(n)) {
                        return p.class.clone();
                    }
                }
                // An `object` singleton referenced by name.
                if self.classes.get(n).is_some_and(|m| m.is_object) {
                    return Some(n.clone());
                }
                None
            }
            Expr::Call { name, .. } => {
                if self.classes.contains_key(name) {
                    return Some(name.clone()); // constructor
                }
                self.fun_sig.get(name).and_then(|s| s.ret_class.clone())
            }
            Expr::Member { recv, name, .. } => {
                let cls = self.infer_class(sc, recv)?;
                self.classes
                    .get(&cls)
                    .and_then(|m| m.prop(name))
                    .and_then(|p| p.class.clone())
            }
            Expr::MethodCall { recv, name, .. } => {
                if matches!(name.as_str(), "map" | "filter") {
                    return Some("List".to_string());
                }
                let cls = self.infer_class(sc, recv)?;
                self.classes
                    .get(&cls)
                    .and_then(|m| m.methods.get(name))
                    .and_then(|s| s.ret_class.clone())
            }
            Expr::NotNull(inner) => self.infer_class(sc, inner),
            Expr::Elvis { left, .. } => self.infer_class(sc, left),
            Expr::Pair { .. } => Some("Pair".to_string()),
            _ => None,
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
        StmtKind::SetMember { recv, value, .. } => expr_has_ffi(recv) || expr_has_ffi(value),
        StmtKind::SetIndex {
            recv, index, value, ..
        } => expr_has_ffi(recv) || expr_has_ffi(index) || expr_has_ffi(value),
        StmtKind::Destructure { init, .. } => expr_has_ffi(init),
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
        Expr::Index { recv, index, .. } => expr_has_ffi(recv) || expr_has_ffi(index),
        Expr::Pair { first, second } => expr_has_ffi(first) || expr_has_ffi(second),
        Expr::Lambda { body, .. } => body_has_ffi(body),
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
