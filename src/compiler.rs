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
    KT_ARRAY, KT_ARRAY_INIT, KT_ARRAY_NEW, KT_CHR_STRING, KT_CLASSOF, KT_CLOSURE_CALL, KT_COLL_HOF,
    KT_DBG_LINE, KT_DDIV, KT_EXC_ABORT, KT_EXC_CUT, KT_EXC_DEPTH, KT_EXC_MATCH, KT_EXC_NEW,
    KT_EXC_PENDING, KT_EXC_STASH, KT_EXC_TAKE, KT_EXC_THROW, KT_EXC_UNSTASH, KT_EXTEND,
    KT_FFI_CALL, KT_FFI_COMPILE, KT_GETFIELD, KT_IDIV, KT_IMOD, KT_IN, KT_INDEX_GET, KT_INDEX_SET,
    KT_IS, KT_ISNULL, KT_ITER_GET, KT_ITER_SIZE, KT_LIST, KT_MAKE_CLOSURE, KT_MAP, KT_MATH,
    KT_DISPLAY, KT_JOIN, KT_METHOD, KT_NEW, KT_NOTNULL, KT_OBJEQ, KT_PAIR, KT_PRINT, KT_PRINTLN, KT_RANGE,
    KT_RANGE_STEP, KT_SCOPE_FN, KT_SET, KT_SETFIELD, KT_TOSTRING_REG, KT_TO_STRING, KT_TYPE_REG,
};
use fusevm::{Chunk, ChunkBuilder, Op, Value};
use std::collections::{HashMap, HashSet};

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
    /// Every currently-visible binding as `(name, slot, ty, class)`, ordered by
    /// slot for deterministic capture layout. This is the lexical environment a
    /// lambda closes over — capturing the whole visible set (by value) is always
    /// correct for reads and avoids a separate free-variable pass; unreferenced
    /// captures only cost an unused slot.
    fn visible(&self) -> Vec<(String, u16, Type, Option<String>)> {
        let mut out: Vec<(String, u16, Type, Option<String>)> = self
            .map
            .iter()
            .map(|(n, b)| (n.clone(), b.slot, b.ty, b.class.clone()))
            .collect();
        out.sort_by_key(|(_, slot, _, _)| *slot);
        out
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

/// Compile-time metadata for a `class` / `data class` / `object` / `interface`,
/// driving constructor lowering, field access, method dispatch, and (for `data`)
/// synthesized-member routing.
#[derive(Clone)]
struct ClassMeta {
    name: String,
    is_data: bool,
    is_object: bool,
    is_interface: bool,
    /// `abstract class` / `sealed class` — has a constructor its subclasses call
    /// but cannot itself be instantiated.
    is_abstract: bool,
    /// Every stored field an instance carries, base-most first: the ancestors'
    /// fields, then this class's own. Drives property lookup and `data` display.
    props: Vec<PropMeta>,
    /// This class's own stored fields only — what its constructor contributes on
    /// top of the base instance. Equal to `props` for a class with no superclass.
    own_props: Vec<PropMeta>,
    /// The primary-constructor parameters in declaration order, including the
    /// ones that are *not* stored properties (`class Dog(name: String, …)`,
    /// whose `name` is forwarded to the superclass rather than kept).
    ctor_params: Vec<CtorProp>,
    /// method name → its signature; own methods first, then inherited ones.
    methods: HashMap<String, FnSig>,
    /// The method names this class *implements* itself (a non-`abstract` body),
    /// which is what `super.m()` resolves against.
    own_methods: HashSet<String>,
    /// Self first, then every user-declared ancestor (superclass and
    /// interfaces), nearest first. See [`linearize`].
    mro: Vec<String>,
    /// The direct supertypes as written.
    parents: Vec<String>,
    /// The superclass whose constructor this class's constructor calls — the
    /// first parent that is a user `class` (an `interface` has no constructor).
    base: Option<String>,
    /// The superclass constructor arguments of `: Super(a, b)`.
    super_args: Vec<Expr>,
    /// The built-in JVM throwable this class ultimately extends, if any
    /// (`class MyError(m: String) : Exception(m)` → `Some("Exception")`). Such a
    /// class carries a synthetic `message` field and displays / is caught like
    /// the built-in throwables.
    throwable_base: Option<String>,
}

impl ClassMeta {
    fn prop(&self, name: &str) -> Option<&PropMeta> {
        self.props.iter().find(|p| p.name == name)
    }
    /// The `KT_NEW` / `KT_EXTEND` metadata string for this class's OWN fields:
    /// `"Name\x1f(d|c)\x1ffield0\x1f…"`. A subclass's base fields ride on the
    /// base instance `KT_EXTEND` builds from, so they are not repeated here.
    fn meta_string(&self) -> String {
        let mut s = self.name.clone();
        s.push('\u{1f}');
        s.push(if self.is_data { 'd' } else { 'c' });
        for p in &self.own_props {
            s.push('\u{1f}');
            s.push_str(&p.name);
        }
        s
    }
    /// Whether this type can be written as a constructor call.
    fn instantiable(&self) -> bool {
        !self.is_object && !self.is_interface && !self.is_abstract
    }
}

/// The mangled sub name for a class method (`Person#greet`).
fn method_sub_name(class: &str, method: &str) -> String {
    format!("{class}#{method}")
}

/// The mangled sub name for a class constructor (`Person#$init`). `$` cannot
/// appear in a Kotlin identifier, so the name can never collide with a method.
fn ctor_sub_name(class: &str) -> String {
    format!("{class}#$init")
}

/// The synthetic field a class extending a built-in throwable stores its
/// `Exception(message)` argument in, so `e.message` and the `Class: message`
/// display form both work.
const MESSAGE_FIELD: &str = "message";

/// Depth-first supertype order for `name`: the type itself, then each direct
/// parent's own order, keeping the first occurrence of a repeat. This is the
/// order an override resolves in — the class before what it inherits from, and
/// an earlier-listed supertype before a later one, which is how Kotlin resolves
/// an unqualified `super` call when only one supertype implements the member.
///
/// The depth cap makes a cyclic `: A`/`: B` hierarchy terminate rather than
/// recurse forever; the cycle is reported separately.
fn linearize(name: &str, parents: &HashMap<String, Vec<String>>) -> Vec<String> {
    fn go(name: &str, parents: &HashMap<String, Vec<String>>, out: &mut Vec<String>, depth: u32) {
        if depth > 64 || out.iter().any(|x| x == name) {
            return;
        }
        out.push(name.to_string());
        if let Some(ps) = parents.get(name) {
            for p in ps {
                go(p, parents, out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    go(name, parents, &mut out, 0);
    out
}

pub struct Compiler {
    b: ChunkBuilder,
    /// name → signature for user functions, filled before lowering.
    fun_sig: HashMap<String, FnSig>,
    /// class/object name → metadata, filled before lowering.
    classes: HashMap<String, ClassMeta>,
    /// method name → the `(runtime class tag, owning type)` pairs a call on that
    /// name may land in. Backs virtual dispatch; see [`build_method_index`].
    method_index: HashMap<String, Vec<(String, String)>>,
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
    /// Lambda bodies discovered while lowering, awaiting emission as subroutine
    /// regions after all functions/methods (a queue because emitting one lambda
    /// may enqueue further nested lambdas). See [`Compiler::compile_lambda`].
    pending_lambdas: Vec<PendingLambda>,
    /// Monotonic id for synthetic lambda sub names (`$lambda$0`, `$lambda$1`, …).
    lambdas_seen: u32,
    /// The `kotlin.math` names the program's imports brought into scope, as
    /// *visible spelling* → *runtime name*. Kotlin does NOT auto-import
    /// `kotlin.math`, so `abs`/`sqrt`/`PI` are compile errors without an import,
    /// a single-name import brings in only that name, and `as` renames it — all
    /// three are reproduced by resolving through this table.
    /// (`java.lang.Math` is auto-imported, so `Math.abs` never needs an entry.)
    math_scope: HashMap<String, String>,
    /// Set by `import kotlin.math.*`, which puts every math name in scope.
    math_star: bool,
    /// True when the program contains a `try`/`throw` anywhere. Only then are the
    /// per-statement unwind checks (and the suppressible print builtins) emitted,
    /// so an exception-free program keeps byte-identical bytecode — and its
    /// speed. See the “Exception unwinding” section in [`crate::host`].
    has_try: bool,
    /// Where a pending exception unwinds to from the statement being compiled —
    /// innermost last. Empty means “leave this frame”.
    unwind: Vec<UnwindFrame>,
    /// The enclosing `try`s that own a `finally`, innermost last. A `return`
    /// inside one cannot jump straight out — the finalizer has to run first — so
    /// it parks its value here and jumps to that `try`'s return path instead.
    /// Cleared on entry to a `fun`/lambda body: a `return` belongs to its own
    /// frame.
    finally_returns: Vec<FinallyReturn>,
}

/// One enclosing `try`-with-`finally`'s return path: the slot a pending return
/// value waits in, and the jumps from each `return` statement inside it.
struct FinallyReturn {
    slot: u16,
    jumps: Vec<usize>,
}

/// Where the unwind check emitted after a statement jumps when an exception is
/// in flight. The variants compose: a `throw` inside a loop inside a `fun`
/// inside a `try` breaks the loop, returns from the frame, then lands in the
/// `catch` dispatch.
#[derive(Clone, Copy, PartialEq)]
enum UnwindKind {
    /// Into the enclosing `try`'s `catch` dispatch (or, from a handler body,
    /// past the handlers into its `finally`).
    Try,
    /// Out of the enclosing loop; the check after the loop statement continues
    /// the walk outward.
    Loop,
    /// Out of the enclosing `fun`/method/lambda frame, returning a placeholder;
    /// the caller's own check resumes the walk there.
    Frame,
}

/// One entry of [`Compiler::unwind`]: the target kind plus the forward jumps
/// awaiting patching to it. `Frame` needs no jump list (it returns inline), but
/// carrying one uniformly keeps the push/pop protocol simple.
struct UnwindFrame {
    kind: UnwindKind,
    jumps: Vec<usize>,
}

/// Build the visible-name → runtime-name table for the `kotlin.math` imports.
fn math_scope(imports: &[ImportDecl]) -> (HashMap<String, String>, bool) {
    let mut scope = HashMap::new();
    let mut star = false;
    for imp in imports.iter().filter(|i| i.package() == "kotlin.math") {
        let name = imp.tail();
        if name == "*" {
            star = true;
        } else if is_math_fn(name) || is_math_const(name) {
            let visible = imp.alias.clone().unwrap_or_else(|| name.to_string());
            scope.insert(visible, name.to_string());
        }
    }
    (scope, star)
}

/// A lambda body queued for emission as a subroutine. `params` already has the
/// implicit `it` injected when the literal had no explicit parameters. `captures`
/// are the enclosing-frame bindings the lambda closes over (its upvalues), in the
/// same order the make-closure site pushed their values and the body's prologue
/// pops them into slots. `class` carries the enclosing class context so a lambda
/// defined inside a method can still reach `this`/fields.
struct PendingLambda {
    name_idx: u16,
    params: Vec<(String, Type)>,
    captures: Vec<(String, Type, Option<String>)>,
    body: Vec<Stmt>,
    class: Option<String>,
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

/// The `kotlin.Any` members every class already inherits. They are `open`
/// there, and overriding one needs no *user* supertype to declare it — so the
/// "overrides nothing" check below must let them through.
const ANY_MEMBERS: &[&str] = &["toString", "equals", "hashCode"];

/// Enforce Kotlin's inheritance modifiers on `cd`.
///
/// Four rules, each rejecting a program `kotlinc` also rejects:
///
/// * a class may only extend a `class` marked `open` / `abstract` / `sealed`
///   (an `interface` is always implementable, and a built-in throwable parent
///   has no declaration here to check);
/// * `override` must have something to override;
/// * a member that *does* redeclare a supertype's must say `override`;
/// * and what it overrides must be overridable — `open`, `abstract`, or itself
///   an `override` (which Kotlin leaves open unless marked `final`). Every
///   member of an `interface` is implicitly open.
///
/// A member is matched by name **and** arity, because a same-named member at a
/// different arity is an overload, not an override.
fn check_modifiers(
    cd: &ClassDecl,
    mro: &[String],
    by_name: &HashMap<&str, &ClassDecl>,
) -> Result<(), String> {
    for p in &cd.parents {
        if let Some(d) = by_name.get(p.as_str()) {
            if !d.is_interface && !(d.is_open || d.is_abstract || d.is_sealed) {
                return Err(format!(
                    "class {}: {p} is final, so it cannot be inherited from (line {})",
                    cd.name, cd.line
                ));
            }
        }
    }
    // Where each inherited member is declared, nearest supertype first.
    let inherited = |name: &str, arity: usize| -> Option<(&ClassDecl, &FunDecl)> {
        mro[1..]
            .iter()
            .filter_map(|a| by_name.get(a.as_str()))
            .find_map(|d| {
                d.methods
                    .iter()
                    .find(|m| m.name == name && m.params.len() == arity)
                    .map(|m| (*d, m))
            })
    };
    for m in &cd.methods {
        let found = inherited(&m.name, m.params.len());
        match (m.is_override, found) {
            (true, None) if !ANY_MEMBERS.contains(&m.name.as_str()) => {
                return Err(format!(
                    "class {}: `{}` overrides nothing (line {})",
                    cd.name, m.name, m.line
                ));
            }
            (false, Some((owner, _))) if !m.is_abstract => {
                return Err(format!(
                    "class {}: `{}` hides a member of supertype {} and needs an `override` \
                     modifier (line {})",
                    cd.name, m.name, owner.name, m.line
                ));
            }
            (true, Some((owner, base))) => {
                let overridable =
                    owner.is_interface || base.is_abstract || base.is_open || base.is_override;
                if !overridable {
                    return Err(format!(
                        "class {}: `{}` in {} is final and cannot be overridden (line {})",
                        cd.name, m.name, owner.name, m.line
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Index every declared `class`/`object`/`interface`: its linearized ancestry,
/// its flattened field record, and the method set an instance responds to.
fn build_class_meta(program: &Program) -> Result<HashMap<String, ClassMeta>, String> {
    let mut by_name: HashMap<&str, &ClassDecl> = HashMap::new();
    for cd in &program.classes {
        if by_name.insert(cd.name.as_str(), cd).is_some() {
            return Err(format!("conflicting declarations for class {}", cd.name));
        }
    }
    // Only USER supertypes take part in the linearization; a built-in throwable
    // parent (`: Exception(msg)`) has no declaration to inherit fields or method
    // bodies from and is tracked separately as `throwable_base`.
    let parent_map: HashMap<String, Vec<String>> = program
        .classes
        .iter()
        .map(|cd| {
            let ps = cd
                .parents
                .iter()
                .filter(|p| by_name.contains_key(p.as_str()))
                .cloned()
                .collect();
            (cd.name.clone(), ps)
        })
        .collect();

    let mut classes: HashMap<String, ClassMeta> = HashMap::new();
    for cd in &program.classes {
        let mro = linearize(&cd.name, &parent_map);
        // A supertype that is neither declared nor a built-in throwable would
        // silently vanish from dispatch, so it is rejected up front.
        for p in &cd.parents {
            if !by_name.contains_key(p.as_str()) && crate::host::throwable_fqn(p).is_none() {
                return Err(format!("unresolved supertype {p} of class {}", cd.name));
            }
        }
        if cd.parents.iter().any(|p| p == &cd.name) || mro[1..].iter().any(|a| a == &cd.name) {
            return Err(format!("class {} is its own supertype", cd.name));
        }
        check_modifiers(cd, &mro, &by_name)?;

        let own_field = |p: &CtorProp| PropMeta {
            name: p.name.clone(),
            ty: p.ty,
            class: p.class.clone(),
            mutable: p.kind == PropKind::Var,
        };
        // The superclass whose constructor this one chains to (interfaces have
        // none), and the built-in throwable the ancestry ultimately reaches.
        let base = cd
            .parents
            .iter()
            .find(|p| by_name.get(p.as_str()).is_some_and(|d| !d.is_interface))
            .cloned();
        let throwable_base = mro
            .iter()
            .filter_map(|a| by_name.get(a.as_str()))
            .flat_map(|d| d.parents.iter())
            .find(|p| crate::host::throwable_fqn(p).is_some())
            .cloned();

        let mut own_props: Vec<PropMeta> = Vec::new();
        // A throwable subclass stores its `Exception(message)` argument in a
        // synthetic leading field, which is what `e.message` reads and what the
        // `Class: message` display form prints.
        if throwable_base.is_some() && cd.parents.iter().any(|p| Some(p) == throwable_base.as_ref())
        {
            own_props.push(PropMeta {
                name: MESSAGE_FIELD.to_string(),
                ty: Type::NullableString,
                class: None,
                mutable: false,
            });
        }
        if cd.is_object {
            own_props.extend(cd.obj_props.iter().map(|(n, ty, class, _)| PropMeta {
                name: n.clone(),
                ty: *ty,
                class: class.clone(),
                mutable: true,
            }));
        } else {
            own_props.extend(
                cd.params
                    .iter()
                    .filter(|p| p.kind != PropKind::None)
                    .map(own_field),
            );
        }

        // The full field record, base-most first — the order `KT_EXTEND` builds
        // an instance in, so property lookup and `data` display agree with it.
        let mut props: Vec<PropMeta> = Vec::new();
        for anc in mro.iter().skip(1).rev() {
            let Some(d) = by_name.get(anc.as_str()) else {
                continue;
            };
            for p in d.params.iter().filter(|p| p.kind != PropKind::None) {
                if !props.iter().any(|x| x.name == p.name) {
                    props.push(own_field(p));
                }
            }
        }
        for p in &own_props {
            if !props.iter().any(|x| x.name == p.name) {
                props.push(p.clone());
            }
        }
        // Kotlin's `data` members are derived from the primary constructor
        // alone. The flat field record keeps the inherited fields too (property
        // lookup needs them), so the *instance* records where this class's own
        // fields begin and the derived members read only from there — see
        // `HeapObj::Instance::data_from`.

        // Methods, own first: the class's own declaration shadows an inherited
        // one, and an earlier-listed supertype shadows a later one.
        let mut methods: HashMap<String, FnSig> = HashMap::new();
        for anc in &mro {
            let Some(d) = by_name.get(anc.as_str()) else {
                continue;
            };
            for m in &d.methods {
                methods.entry(m.name.clone()).or_insert(FnSig {
                    ret: m.ret,
                    ret_class: m.ret_class.clone(),
                    arity: m.params.len(),
                });
            }
        }
        let own_methods = cd
            .methods
            .iter()
            .filter(|m| !m.is_abstract)
            .map(|m| m.name.clone())
            .collect();

        classes.insert(
            cd.name.clone(),
            ClassMeta {
                name: cd.name.clone(),
                is_data: cd.is_data,
                is_object: cd.is_object,
                is_interface: cd.is_interface,
                is_abstract: cd.is_abstract || cd.is_sealed,
                props,
                own_props,
                ctor_params: cd.params.clone(),
                methods,
                own_methods,
                mro,
                parents: cd.parents.clone(),
                base,
                super_args: cd.super_args.clone(),
                throwable_base,
            },
        );
    }
    Ok(classes)
}

/// The runtime dispatch table: for every method name, the concrete class tags
/// that respond to it paired with the type owning the implementation. A call on
/// a receiver whose static class is a supertype tests the tag at runtime and
/// lands in the right `Owner#method` sub — virtual dispatch, without a vtable
/// the VM has no notion of.
fn build_method_index(
    program: &Program,
    classes: &HashMap<String, ClassMeta>,
) -> HashMap<String, Vec<(String, String)>> {
    let by_name: HashMap<&str, &ClassDecl> =
        program.classes.iter().map(|c| (c.name.as_str(), c)).collect();
    let mut index: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (tag, meta) in classes {
        // Only a type that can exist at runtime is a dispatch tag: an interface
        // or an abstract class is never a receiver's own class.
        if meta.is_interface || meta.is_abstract {
            continue;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for owner in &meta.mro {
            let Some(d) = by_name.get(owner.as_str()) else {
                continue;
            };
            for m in &d.methods {
                // An `abstract` declaration reserves the name so a later
                // supertype's stale implementation cannot shadow the override
                // that a nearer type supplied.
                if seen.insert(&m.name) && !m.is_abstract {
                    index
                        .entry(m.name.clone())
                        .or_default()
                        .push((tag.clone(), owner.clone()));
                }
            }
        }
    }
    // A stable order keeps the emitted dispatch chain reproducible.
    for v in index.values_mut() {
        v.sort();
    }
    index
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

    let classes = build_class_meta(program)?;
    let method_index = build_method_index(program, &classes);

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
        method_index,
        cur_class: None,
        debug,
        has_ffi,
        loops: Vec::new(),
        pending_lambdas: Vec::new(),
        lambdas_seen: 0,
        math_scope: HashMap::new(),
        math_star: false,
        has_try: uses_exceptions(program),
        unwind: Vec::new(),
        finally_returns: Vec::new(),
    };
    (c.math_scope, c.math_star) = math_scope(&program.imports);

    // Preamble: publish each declared type's supertypes to the runtime (which
    // `is` checks, `catch` matching, and the throwable display form all consult),
    // build `object` singletons into globals, then bind main's args (an empty
    // Array per declared parameter — the program-args wiring is an M0 stub),
    // call main, discard its Unit, skip the bodies.
    for cd in &program.classes {
        let meta = &c.classes[&cd.name];
        let supers = c.runtime_supers(meta).join(",");
        let (name, line) = (cd.name.clone(), cd.line);
        let n = c.b.add_constant(Value::str(name));
        c.b.emit(Op::LoadConst(n), line);
        let s = c.b.add_constant(Value::str(supers));
        c.b.emit(Op::LoadConst(s), line);
        c.b.emit(Op::Extended(KT_TYPE_REG, 0), line);
    }
    c.emit_tostring_registry();
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
    // An exception that walked out of `main` reached the top: report it the way
    // the JVM does and stop with a non-zero status.
    if c.has_try {
        c.b.emit(Op::CallBuiltin(KT_EXC_PENDING, 0), main.line);
        let ok = c.b.emit(Op::JumpIfFalse(0), main.line);
        c.b.emit(Op::CallBuiltin(KT_EXC_ABORT, 0), main.line);
        c.b.emit(Op::Pop, main.line);
        let after = c.b.current_pos();
        c.b.patch_jump(ok, after);
    }
    let end_jump = c.b.emit(Op::Jump(0), main.line);

    for f in &program.funs {
        c.compile_fun(f, None)?;
    }
    // Class/object methods lower as subs named `Class#method`, with `this`
    // (slot 0) as an implicit first parameter of the enclosing class type. An
    // `abstract` declaration owns no body — it only reserves the name so a
    // subtype's override is reachable through the dispatch chain.
    for cd in &program.classes {
        if !cd.is_object && !cd.is_interface {
            c.compile_ctor(cd)?;
        }
        for m in &cd.methods {
            if !m.is_abstract {
                c.compile_fun(m, Some(&cd.name))?;
            }
        }
    }
    // Lambda bodies emit as subroutine regions last. Draining may enqueue further
    // lambdas (one nested inside another), so loop until the queue is empty.
    while let Some(pl) = c.pending_lambdas.pop() {
        c.compile_lambda_body(pl)?;
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
            .emit(Op::Extended(KT_NEW, meta.own_props.len() as u8), cd.line);
        let g = self.b.add_name(&cd.name);
        self.b.emit(Op::SetVar(g), cd.line);
        Ok(())
    }

    /// The supertype names an instance of `meta` answers `is` / `catch` with:
    /// its user-declared ancestors, then the built-in throwable chain it reaches
    /// (so `class MyError : Exception(…)` is caught by `catch (e: Exception)` and
    /// `catch (e: Throwable)` alike).
    fn runtime_supers(&self, meta: &ClassMeta) -> Vec<String> {
        let mut out: Vec<String> = meta.mro[1..].to_vec();
        if let Some(t) = &meta.throwable_base {
            for name in crate::host::throwable_ancestry(t) {
                if !out.iter().any(|x| x == name) {
                    out.push(name.to_string());
                }
            }
        }
        out
    }

    /// Emit a class's constructor subroutine `Class#$init`.
    ///
    /// The subroutine exists (rather than an inline `KT_NEW` at every `C(...)`
    /// site) because a subclass's superclass arguments are written in terms of
    /// its own constructor parameters — `class Dog(name: String) : Animal(name)`
    /// — which only a frame that has bound those parameters can evaluate. The
    /// body is one of three shapes:
    ///
    /// * **superclass** — evaluate the `: Super(args)`, call `Super#$init` to get
    ///   the base instance, then `KT_EXTEND` it with this class's own fields
    ///   under this class's runtime tag. Nesting takes care of deeper ancestries.
    /// * **built-in throwable superclass** — no base instance exists, so the
    ///   `Exception(message)` argument lands in the synthetic `message` field.
    /// * **no superclass** — a plain `KT_NEW` over the class's own fields.
    fn compile_ctor(&mut self, cd: &ClassDecl) -> Result<(), String> {
        let meta = self.classes[&cd.name].clone();
        let entry = self.b.current_pos();
        let name_idx = self.b.add_name(&ctor_sub_name(&cd.name));
        self.b.add_sub_entry(name_idx, entry);

        // Bind the primary-constructor parameters to slots, deepest last.
        let mut sc = Scope::new();
        for p in &meta.ctor_params {
            sc.declare_obj(&p.name, p.ty, false, p.class.clone());
        }
        for i in (0..meta.ctor_params.len()).rev() {
            self.b.emit(Op::SetSlot(i as u16), cd.line);
        }

        // The base instance, when there is a superclass to chain to.
        if let Some(base) = &meta.base {
            let bmeta = self.classes[base].clone();
            if meta.super_args.len() != bmeta.ctor_params.len() {
                return Err(format!(
                    "superclass constructor {base} expects {} argument(s), got {}",
                    bmeta.ctor_params.len(),
                    meta.super_args.len()
                ));
            }
            for a in &meta.super_args {
                self.compile_expr(&mut sc, a)?;
            }
            let idx = self.b.add_name(&ctor_sub_name(base));
            self.b
                .emit(Op::Call(idx, meta.super_args.len() as u8), cd.line);
        }

        let midx = self.b.add_constant(Value::str(meta.meta_string()));
        self.b.emit(Op::LoadConst(midx), cd.line);
        // A throwable subclass's first own field is the synthetic `message`.
        let direct_throwable = meta
            .throwable_base
            .as_ref()
            .is_some_and(|t| meta.parents.iter().any(|p| p == t));
        if direct_throwable {
            match meta.super_args.first() {
                Some(a) => {
                    let t = self.compile_expr(&mut sc, a)?;
                    self.emit_display(t);
                }
                // `class E : Exception()` — `E().message` is Kotlin `null`.
                None => {
                    self.b.emit(Op::LoadUndef, cd.line);
                }
            }
        }
        for p in meta.own_props.iter().filter(|p| p.name != MESSAGE_FIELD) {
            let slot = sc
                .slot(&p.name)
                .ok_or_else(|| format!("class {}: unbound property {}", cd.name, p.name))?;
            self.b.emit(Op::GetSlot(slot), cd.line);
        }
        let n = meta.own_props.len() as u8;
        let op = if meta.base.is_some() {
            Op::Extended(KT_EXTEND, n)
        } else {
            Op::Extended(KT_NEW, n)
        };
        self.b.emit(op, cd.line);
        self.b.emit(Op::ReturnValue, cd.line);
        Ok(())
    }

    /// Whether the program declares a `toString()` override, which is what makes
    /// display route through the re-entrant [`KT_DISPLAY`] builtin instead of the
    /// VM-less `KT_TO_STRING` extension op.
    fn has_tostring_override(&self) -> bool {
        self.method_index
            .get("toString")
            .is_some_and(|t| !t.is_empty())
    }

    /// Publish each overriding class's `toString()` subroutine to the runtime, so
    /// [`KT_DISPLAY`] can invoke it for a receiver whose class is only known
    /// there. Emitted once, before `main`, and only when an override exists.
    fn emit_tostring_registry(&mut self) {
        if !self.has_tostring_override() {
            return;
        }
        for (tag, owner) in self.method_index["toString"].clone() {
            let t = self.b.add_constant(Value::str(tag));
            self.b.emit(Op::LoadConst(t), 0);
            let sub = self.b.add_name(&method_sub_name(&owner, "toString"));
            self.b.emit(Op::LoadInt(sub as i64), 0);
            self.b.emit(Op::Extended(KT_TOSTRING_REG, 0), 0);
        }
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
        // The frame is its own unwind boundary: an exception with no handler
        // inside it leaves the frame, and the caller's check resumes the walk.
        // A `return` likewise belongs to this frame, so an enclosing `try`'s
        // return path (from a lambda's definition site) must not capture it.
        self.push_unwind(UnwindKind::Frame);
        let outer_returns = std::mem::take(&mut self.finally_returns);
        let res: Result<(), String> = (|| {
            for s in &f.body {
                self.compile_stmt(&mut sc, s)?;
            }
            Ok(())
        })();
        self.cur_class = None;
        self.finally_returns = outer_returns;
        let here = self.b.current_pos();
        self.pop_unwind_to(here);
        res?;
        // Fallthrough Unit return for `Unit` functions / a missing `return`.
        self.b.emit(Op::LoadUndef, f.line);
        self.b.emit(Op::ReturnValue, f.line);
        Ok(())
    }

    // ── Exception unwinding (compile-time half) ────────────────────
    //
    // See the “Exception unwinding” section in [`crate::host`] for the protocol.
    // A statement boundary is the only point where the operand stack is known to
    // be balanced, so that is where the check goes; the handler's `KT_EXC_CUT`
    // mops up anything a *nested* abandoned expression left behind.

    /// Emit the post-statement pending test and the jump the innermost enclosing
    /// construct wants for it. A no-op in a program without a `try`.
    fn unwind_check(&mut self) {
        self.unwind_check_dropping(0);
    }

    /// [`Compiler::unwind_check`] at a point where `drop` values sit on the
    /// operand stack: they are popped on the unwind path so the jump leaves the
    /// stack balanced.
    ///
    /// The `drop == 1` form guards a binding store. Without it a raise while
    /// computing an initializer would still commit the resulting garbage to the
    /// `val`/`var` before control reached the handler, so a handler reading that
    /// binding would see `null` instead of its previous value.
    fn unwind_check_dropping(&mut self, drop: usize) {
        if !self.has_try {
            return;
        }
        self.b.emit(Op::CallBuiltin(KT_EXC_PENDING, 0), 0);
        let ok = self.b.emit(Op::JumpIfFalse(0), 0);
        for _ in 0..drop {
            self.b.emit(Op::Pop, 0);
        }
        match self.unwind.last().map(|f| f.kind) {
            // A `try` body or a loop body: jump forward to the construct's
            // exceptional exit, patched by whoever pushed the frame.
            Some(UnwindKind::Try) | Some(UnwindKind::Loop) => {
                let j = self.b.emit(Op::Jump(0), 0);
                self.unwind
                    .last_mut()
                    .expect("just matched a frame")
                    .jumps
                    .push(j);
            }
            // A `fun`/method/lambda body: return a placeholder immediately so the
            // frame is popped (`Op::ReturnValue` truncates the value stack to the
            // frame base, so the abandoned operands cost nothing). The caller's
            // own check resumes the walk.
            Some(UnwindKind::Frame) | None => {
                self.b.emit(Op::LoadUndef, 0);
                self.b.emit(Op::ReturnValue, 0);
            }
        }
        let at = self.b.current_pos();
        self.b.patch_jump(ok, at);
    }

    fn push_unwind(&mut self, kind: UnwindKind) {
        self.unwind.push(UnwindFrame {
            kind,
            jumps: Vec::new(),
        });
    }

    /// Pop the innermost unwind frame and patch its collected jumps to `target`.
    fn pop_unwind_to(&mut self, target: usize) {
        if let Some(f) = self.unwind.pop() {
            for j in f.jumps {
                self.b.patch_jump(j, target);
            }
        }
    }

    // ── Statements (stack-neutral) ─────────────────────────────────

    /// Compile one statement, then — in a program that contains a `try` — the
    /// unwind check that carries an in-flight exception outward.
    fn compile_stmt(&mut self, sc: &mut Scope, s: &Stmt) -> Result<(), String> {
        self.compile_stmt_inner(sc, s)?;
        self.unwind_check();
        Ok(())
    }

    fn compile_stmt_inner(&mut self, sc: &mut Scope, s: &Stmt) -> Result<(), String> {
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
                // A raise inside the initializer must not commit its garbage
                // result to the new binding.
                self.unwind_check_dropping(1);
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
                // As with a `val` initializer: a raise mid-value must leave the
                // variable's previous value intact (`acc += 10 / 0`).
                self.unwind_check_dropping(1);
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
                // Inside a `try` that owns a `finally`, the finalizer must run
                // before the frame is left: park the value and jump to that
                // `try`'s return path, which runs the `finally` (and any outer
                // one) and then returns.
                match self.finally_returns.last().map(|f| f.slot) {
                    Some(slot) => {
                        self.b.emit(Op::SetSlot(slot), 0);
                        let j = self.b.emit(Op::Jump(0), 0);
                        self.finally_returns
                            .last_mut()
                            .expect("just matched a frame")
                            .jumps
                            .push(j);
                    }
                    None => {
                        self.b.emit(Op::ReturnValue, 0);
                    }
                }
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
                // A raise inside the body (or in the condition just evaluated)
                // leaves the loop; the check after the loop statement carries it
                // further out.
                self.push_unwind(UnwindKind::Loop);
                self.unwind_check();
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
                self.pop_unwind_to(end);
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
            StmtKind::ForIn {
                var,
                iter,
                body,
                label,
            } => {
                self.compile_for_in(sc, var, iter, body, label)?;
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
        self.push_unwind(UnwindKind::Loop);
        self.unwind_check();
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
        self.pop_unwind_to(done);
        for j in &ctx.breaks {
            self.b.patch_jump(*j, done);
        }
        sc.exit(mark);
        Ok(())
    }

    /// `for (v in iterable)` over a value — a `List`, an array, or a range held
    /// in a variable. The iterable is evaluated ONCE into a slot (Kotlin
    /// evaluates the header expression once), its length read once, and the loop
    /// then walks indices with native ops, fetching each element through
    /// `KT_ITER_GET`. Counted ranges never reach here; the parser routes those to
    /// [`Compiler::compile_for`], which needs no host call per iteration.
    fn compile_for_in(
        &mut self,
        sc: &mut Scope,
        var: &str,
        iter: &Expr,
        body: &[Stmt],
        label: &Option<String>,
    ) -> Result<(), String> {
        let mark = sc.enter();
        // Iterating a `String` yields `Char`s. The runtime element is an integer
        // code unit like every other kotlinrs `Char`, so the element type has to
        // be decided here — otherwise `println(c)` would print the code, not the
        // character.
        let elem_ty = if self.infer(sc, iter).is_str() {
            Type::Char
        } else {
            Type::Unknown
        };
        // The iterable and the loop's index/length temporaries.
        let cslot = sc.temp();
        self.compile_expr(sc, iter)?;
        self.b.emit(Op::SetSlot(cslot), 0);
        let nslot = sc.temp();
        self.b.emit(Op::GetSlot(cslot), 0);
        self.b.emit(Op::Extended(KT_ITER_SIZE, 0), 0);
        self.b.emit(Op::SetSlot(nslot), 0);
        let islot = sc.temp();
        self.b.emit(Op::LoadInt(0), 0);
        self.b.emit(Op::SetSlot(islot), 0);
        // The element binding is a `val` (Kotlin's `for` variable is read-only).
        let vslot = sc.declare(var, elem_ty, false);

        let top = self.b.current_pos();
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::GetSlot(nslot), 0);
        self.b.emit(Op::NumLt, 0);
        let jf = self.b.emit(Op::JumpIfFalse(0), 0);
        self.b.emit(Op::GetSlot(cslot), 0);
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::Extended(KT_ITER_GET, 0), 0);
        self.b.emit(Op::SetSlot(vslot), 0);

        self.loops.push(LoopCtx {
            label: label.clone(),
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.push_unwind(UnwindKind::Loop);
        self.unwind_check();
        for s in body {
            self.compile_stmt(sc, s)?;
        }
        let ctx = self.loops.pop().unwrap();
        let cont_target = self.b.current_pos();
        for j in &ctx.continues {
            self.b.patch_jump(*j, cont_target);
        }
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::LoadInt(1), 0);
        self.b.emit(Op::Add, 0);
        self.b.emit(Op::SetSlot(islot), 0);
        self.b.emit(Op::Jump(top), 0);
        let done = self.b.current_pos();
        self.b.patch_jump(jf, done);
        self.pop_unwind_to(done);
        for j in &ctx.breaks {
            self.b.patch_jump(*j, done);
        }
        sc.exit(mark);
        Ok(())
    }

    // ── Expressions (leave exactly one value) ──────────────────────

    fn compile_expr(&mut self, sc: &mut Scope, e: &Expr) -> Result<Type, String> {
        match e {
            // `super` is a receiver, never a value — `compile_member` takes it
            // before it can be compiled on its own, so reaching here means the
            // program wrote it somewhere Kotlin does not allow it either.
            Expr::Super { .. } => Err("`super` is not an expression".to_string()),
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
            // A `Char` is a runtime type of its own (`crate::host::CHAR_TAG`),
            // not an `Int` the static type happens to call a character, so the
            // literal loads the char value itself.
            Expr::Char(c) => {
                let idx = self.b.add_constant(crate::host::char_of(*c));
                self.b.emit(Op::LoadConst(idx), 0);
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
                // `kotlin.math.PI` / `.E`, in scope only under the import.
                if let Some(rt) = self.resolve_math_const(name) {
                    return self.compile_math_const(&rt, 0);
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
                let rt = self.infer(sc, recv);
                self.compile_expr(sc, recv)?;
                self.compile_expr(sc, index)?;
                self.b.emit(Op::Extended(KT_INDEX_GET, 0), *line);
                // `s[i]` on a String is a `Char`; every other receiver's element
                // type is beyond the coarse inference.
                Ok(if rt.is_str() {
                    Type::Char
                } else {
                    Type::Unknown
                })
            }
            Expr::Pair { first, second } => {
                self.compile_expr(sc, first)?;
                self.compile_expr(sc, second)?;
                self.b.emit(Op::Extended(KT_PAIR, 0), 0);
                Ok(Type::Obj)
            }
            Expr::Range { start, end, kind } => {
                self.compile_expr(sc, start)?;
                self.compile_expr(sc, end)?;
                self.b.emit(Op::Extended(KT_RANGE, range_form(*kind)), 0);
                Ok(Type::Obj)
            }
            Expr::Step { recv, by } => {
                self.compile_expr(sc, recv)?;
                self.compile_expr(sc, by)?;
                self.b.emit(Op::Extended(KT_RANGE_STEP, 0), 0);
                Ok(Type::Obj)
            }
            Expr::In {
                value,
                container,
                negated,
            } => {
                self.compile_expr(sc, value)?;
                self.compile_expr(sc, container)?;
                self.b.emit(Op::Extended(KT_IN, 0), 0);
                if *negated {
                    self.b.emit(Op::LogNot, 0);
                }
                Ok(Type::Boolean)
            }
            Expr::Is { value, ty, negated } => {
                self.compile_expr(sc, value)?;
                let nidx = self.b.add_constant(Value::str(ty.clone()));
                self.b.emit(Op::LoadConst(nidx), 0);
                self.b.emit(Op::Extended(KT_IS, 0), 0);
                if *negated {
                    self.b.emit(Op::LogNot, 0);
                }
                Ok(Type::Boolean)
            }
            Expr::IncDec {
                target,
                inc,
                prefix,
            } => self.compile_incdec(sc, target, *inc, *prefix),
            Expr::Lambda { params, body } => self.compile_lambda(sc, params, body),
            Expr::If(ie) => self.compile_if(sc, ie),
            Expr::When(w) => self.compile_when(sc, w),
            Expr::Try(t) => self.compile_try(sc, t),
            Expr::Throw(e) => {
                self.compile_expr(sc, e)?;
                self.b.emit(Op::CallBuiltin(KT_EXC_THROW, 1), 0);
                // The builtin leaves a placeholder so the expression is stack-
                // balanced; nothing observes it, because the enclosing
                // statement's unwind check jumps before the value is used.
                Ok(Type::Unknown)
            }
        }
    }

    /// Lower `try { … } catch (e: T) { … }* [finally { … }]` as a value.
    ///
    /// The shape, in order:
    ///
    /// ```text
    ///   depth = KT_EXC_DEPTH        ; operand-stack depth at entry
    ///   <body>              -> res  ; unwind checks inside jump to `dispatch`
    ///   Jump fin
    /// dispatch:                     ; exceptional exit — `res` is null
    ///   KT_EXC_CUT(depth)           ; drop what the abandoned statement pushed
    ///   <catch arms>        -> res  ; each arm: KT_EXC_MATCH, then KT_EXC_TAKE
    /// fin:
    ///   KT_EXC_STASH                ; park any still-in-flight exception …
    ///   <finally>                   ; … so the finalizer runs to completion …
    ///   KT_EXC_UNSTASH              ; … then resume unwinding it
    ///   load res
    /// ```
    ///
    /// The result travels in a synthetic local rather than on the operand stack
    /// because the exceptional path enters at `dispatch` from an arbitrary
    /// statement boundary inside the body, where nothing has been pushed.
    ///
    /// The handler arms run under their own unwind frame targeting `fin`, so an
    /// exception thrown *by* a handler still runs the `finally` before
    /// propagating — the JVM's ordering. The `finally` body is emitted once, on
    /// the single path both exits converge to.
    fn compile_try(&mut self, sc: &mut Scope, t: &TryExpr) -> Result<Type, String> {
        // A `return` out of a `try` that owns a `finally` is honoured (it routes
        // through the return path below), but a `break`/`continue` is not: it
        // would leave the loop without running the finalizer, and silently
        // skipping a cleanup block is worse than not accepting the program.
        let has_finally = !t.finally_body.is_empty();
        if has_finally
            && (body_breaks(&t.body) || t.catches.iter().any(|c| body_breaks(&c.body)))
        {
            return Err(format!(
                "`break`/`continue` out of a `try` with a `finally` is not supported (line {})",
                t.line
            ));
        }
        let mark = sc.enter();
        let res = sc.temp();
        let depth = sc.temp();
        if has_finally {
            let slot = sc.temp();
            self.finally_returns.push(FinallyReturn {
                slot,
                jumps: Vec::new(),
            });
        }
        self.b.emit(Op::CallBuiltin(KT_EXC_DEPTH, 0), t.line);
        self.b.emit(Op::SetSlot(depth), t.line);

        // ── guarded body ──
        self.push_unwind(UnwindKind::Try);
        let body_ty = self.compile_block_value(sc, &t.body)?;
        self.b.emit(Op::SetSlot(res), 0);
        // The body's tail expression has no statement after it, so its own check
        // goes here — a raise in the last expression must still dispatch.
        self.unwind_check_dropping(0);
        let to_fin = self.b.emit(Op::Jump(0), 0);

        // ── handler dispatch ──
        let dispatch = self.b.current_pos();
        self.pop_unwind_to(dispatch);
        self.b.emit(Op::GetSlot(depth), 0);
        self.b.emit(Op::CallBuiltin(KT_EXC_CUT, 1), 0);
        self.b.emit(Op::Pop, 0);
        self.b.emit(Op::LoadUndef, 0);
        self.b.emit(Op::SetSlot(res), 0);

        self.push_unwind(UnwindKind::Try);
        let mut arm_ty: Option<Type> = None;
        let mut handled = Vec::new();
        for arm in &t.catches {
            let tidx = self.b.add_constant(Value::str(arm.ty.clone()));
            self.b.emit(Op::LoadConst(tidx), 0);
            self.b.emit(Op::CallBuiltin(KT_EXC_MATCH, 1), 0);
            let next_arm = self.b.emit(Op::JumpIfFalse(0), 0);
            // Matched: claim the exception (so the handler body runs with
            // side-effecting builtins live again) and bind it.
            let amark = sc.enter();
            let slot = sc.declare_obj(&arm.name, Type::Obj, false, Some(arm.ty.clone()));
            self.b.emit(Op::CallBuiltin(KT_EXC_TAKE, 0), 0);
            self.b.emit(Op::SetSlot(slot), 0);
            let t_arm = self.compile_block_value(sc, &arm.body)?;
            arm_ty = Some(join_ty(arm_ty, t_arm));
            self.b.emit(Op::SetSlot(res), 0);
            self.unwind_check_dropping(0);
            sc.exit(amark);
            handled.push(self.b.emit(Op::Jump(0), 0));
            let next = self.b.current_pos();
            self.b.patch_jump(next_arm, next);
        }
        // Falling off the last arm leaves the exception in flight: unhandled
        // here, it keeps unwinding once the `finally` has run.

        // ── finally (both paths converge here) ──
        let fin = self.b.current_pos();
        self.pop_unwind_to(fin);
        self.b.patch_jump(to_fin, fin);
        for j in handled {
            self.b.patch_jump(j, fin);
        }
        let pending_ret = if has_finally {
            self.emit_finally(sc, &t.finally_body)?;
            self.finally_returns.pop()
        } else {
            None
        };
        self.b.emit(Op::GetSlot(res), 0);

        // ── return path ──
        // A `return` inside the body or a handler parked its value and jumped
        // here, so the finalizer runs on that path too. The `finally` body is
        // emitted a second time rather than shared through a subroutine: fusevm's
        // frames are for calls, not for local jumps, so a shared copy would need
        // a return address. Duplication is what `javac` itself does since `jsr`
        // was dropped.
        if let Some(ret) = pending_ret.filter(|r| !r.jumps.is_empty()) {
            let over = self.b.emit(Op::Jump(0), 0);
            let ret_path = self.b.current_pos();
            for j in ret.jumps {
                self.b.patch_jump(j, ret_path);
            }
            self.emit_finally(sc, &t.finally_body)?;
            self.b.emit(Op::GetSlot(ret.slot), 0);
            // An enclosing `try` with its own `finally` must run that one too, so
            // the value hops to its return path instead of leaving the frame.
            match self.finally_returns.last().map(|f| f.slot) {
                Some(outer) => {
                    self.b.emit(Op::SetSlot(outer), 0);
                    let j = self.b.emit(Op::Jump(0), 0);
                    self.finally_returns
                        .last_mut()
                        .expect("just matched a frame")
                        .jumps
                        .push(j);
                }
                None => {
                    self.b.emit(Op::ReturnValue, 0);
                }
            }
            let after = self.b.current_pos();
            self.b.patch_jump(over, after);
        }
        sc.exit(mark);
        Ok(join_ty(arm_ty, body_ty))
    }

    /// Emit one copy of a `finally` body, bracketed by the stash/unstash pair
    /// that parks any in-flight exception across it — otherwise the finalizer's
    /// own statements would be suppressed (and immediately unwound) by the very
    /// exception it is cleaning up after. A raise inside the finalizer jumps
    /// straight to the unstash, which keeps the NEW exception and discards the
    /// parked one: the JVM's rule.
    fn emit_finally(&mut self, sc: &mut Scope, body: &[Stmt]) -> Result<(), String> {
        self.b.emit(Op::CallBuiltin(KT_EXC_STASH, 0), 0);
        self.b.emit(Op::Pop, 0);
        self.push_unwind(UnwindKind::Try);
        for s in body {
            self.compile_stmt(sc, s)?;
        }
        let unstash = self.b.current_pos();
        self.pop_unwind_to(unstash);
        self.b.emit(Op::CallBuiltin(KT_EXC_UNSTASH, 0), 0);
        self.b.emit(Op::Pop, 0);
        Ok(())
    }

    /// `x++` / `x--` / `++x` / `--x` in either statement or expression position.
    ///
    /// The update reuses the ordinary assignment lowering, so the increment
    /// covers a variable, a property, and an indexed element identically — and,
    /// as with `x += 1`, a non-variable target has its receiver evaluated twice.
    /// The distinction between the two forms is only WHICH value is left on the
    /// stack: a postfix increment saves the pre-update value into a temp and
    /// pushes that afterwards, a prefix one re-reads the target.
    fn compile_incdec(
        &mut self,
        sc: &mut Scope,
        target: &Expr,
        inc: bool,
        prefix: bool,
    ) -> Result<Type, String> {
        let ty = self.infer(sc, target);
        let op = if inc { BinOp::Add } else { BinOp::Sub };
        let mark = sc.enter();
        // Postfix yields the value from BEFORE the update, so capture it first.
        let saved = if prefix {
            None
        } else {
            self.compile_expr(sc, target)?;
            let slot = sc.temp();
            self.b.emit(Op::SetSlot(slot), 0);
            Some(slot)
        };
        let update = match target {
            Expr::Var(name) => StmtKind::Assign {
                name: name.clone(),
                op: Some(op),
                value: Expr::Int(1),
            },
            Expr::Member {
                recv,
                name,
                safe: false,
                ..
            } => StmtKind::SetMember {
                recv: (**recv).clone(),
                name: name.clone(),
                op: Some(op),
                value: Expr::Int(1),
            },
            Expr::Index { recv, index, .. } => StmtKind::SetIndex {
                recv: (**recv).clone(),
                index: (**index).clone(),
                op: Some(op),
                value: Expr::Int(1),
            },
            _ => {
                return Err("the operand of ++/-- must be a variable, property, or element".into())
            }
        };
        // `compile_stmt_inner`, not `compile_stmt`: this synthetic statement sits
        // mid-expression, where an unwind jump would strand the operands the
        // enclosing expression has already pushed. The enclosing statement's own
        // check picks up any exception the update raised.
        self.compile_stmt_inner(sc, &Stmt::new(0, update))?;
        match saved {
            Some(slot) => {
                self.b.emit(Op::GetSlot(slot), 0);
            }
            // Prefix: the value is the target AFTER the update, so re-read it.
            None => {
                self.compile_expr(sc, target)?;
            }
        }
        sc.exit(mark);
        Ok(ty)
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
            // A value that may be a class instance goes through the re-entrant
            // display builtin when the program declares a `toString()` override,
            // so the override is honoured for a receiver whose class is only
            // known at runtime — and for one nested inside a printed
            // collection. Every other program emits the single host op it did.
            Type::Obj | Type::Unknown if self.has_tostring_override() => {
                self.b.emit(Op::CallBuiltin(KT_DISPLAY, 1), 0);
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
        // `super.m(args)` — the *statically* resolved supertype implementation,
        // never the virtual one, which is the whole point of the keyword.
        if let Expr::Super { qualifier } = recv {
            return self.compile_super_call(sc, qualifier.as_deref(), name, args, line);
        }
        // `java.lang.Math` statics. Kotlin auto-imports `java.lang.*` on the JVM,
        // so `Math.abs(-3)` compiles with no import — unlike the `kotlin.math`
        // top-level spellings. `Math.round` is NOT `kotlin.math.round`: it is
        // half-up and returns a `Long`, so it dispatches under its own name.
        if self.is_java_math(sc, recv) {
            match name {
                "PI" | "E" => return self.compile_math_const(name, line),
                "round" => return self.compile_math(sc, "jround", args, line),
                _ if is_math_fn(name) => return self.compile_math(sc, name, args, line),
                _ => return Err(format!("unresolved reference: Math.{name}")),
            }
        }
        // `Char.toString()` must render the character, not its code. The runtime
        // value is an `Int`, so the host's generic `toString` can't tell it is a
        // Char — resolve it statically here from the receiver's coarse type.
        if name == "toString" && args.is_empty() && self.infer(sc, recv) == Type::Char {
            self.compile_expr(sc, recv)?;
            self.b.emit(Op::Extended(KT_CHR_STRING, 0), line);
            return Ok(Type::String);
        }
        // In a program with a `toString()` override, the two members that render
        // a value as a string route through the re-entrant display builtin so
        // the override is used — the host's own stringifier has no VM to run it
        // with. `Class#toString` itself must NOT be rerouted, or it would
        // recurse; a receiver whose static class implements the override is
        // dispatched directly below.
        if self.has_tostring_override() {
            let own_override = self
                .infer_class(sc, recv)
                .is_some_and(|c| self.classes.get(&c).is_some_and(|m| m.methods.contains_key("toString")));
            if name == "toString" && args.is_empty() && !own_override {
                self.compile_expr(sc, recv)?;
                self.b.emit(Op::CallBuiltin(KT_DISPLAY, 1), line);
                return Ok(Type::String);
            }
            if name == "joinToString" && args.len() <= 1 {
                self.compile_expr(sc, recv)?;
                for a in args {
                    let t = self.compile_expr(sc, a)?;
                    self.emit_display(t);
                }
                self.b.emit(Op::CallBuiltin(KT_JOIN, args.len() as u8), line);
                return Ok(Type::String);
            }
        }
        // Collection higher-order functions take a first-class lambda VALUE (a
        // trailing-lambda literal or a passed closure) and invoke it per element
        // at runtime via the `KT_COLL_HOF` builtin.
        if is_coll_hof(name) && !args.is_empty() {
            return self.compile_coll_hof(sc, recv, name, args, line);
        }
        // `it`-form scope functions on any receiver: `x.let { … }` /
        // `x.also { … }` / `x.takeIf { … }`.
        if matches!(name, "let" | "also" | "takeIf") && args.len() == 1 {
            self.compile_expr(sc, recv)?;
            self.compile_expr(sc, &args[0])?; // the lambda → closure value
            let nidx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(nidx), line);
            self.b.emit(Op::CallBuiltin(KT_SCOPE_FN, 0), line);
            return Ok(Type::Unknown);
        }
        // A statically-known user class: dispatch a user method, read a property
        // directly, or route a `data` member.
        let static_cls = self.infer_class(sc, recv);
        if let Some(cls) = &static_cls {
            if let Some(meta) = self.classes.get(cls).cloned() {
                // A user-declared method. The receiver's *runtime* class may be
                // any subtype of its static one, so the call resolves against
                // every candidate implementation (see [`Compiler::candidates`]).
                if let Some(sig) = meta.methods.get(name) {
                    if args.len() != sig.arity {
                        return Err(format!(
                            "method {name} on {cls} expects {} argument(s), got {}",
                            sig.arity,
                            args.len()
                        ));
                    }
                    let cands = self.candidates(Some(cls), name, args.len());
                    if !cands.is_empty() {
                        self.emit_virtual_call(sc, recv, name, args, &cands, line)?;
                        return Ok(sig.ret);
                    }
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
        // An untyped receiver (a lambda parameter, a `List` element, a `when`
        // subject) that names a user method: decide by runtime class tag, with
        // the host stdlib dispatch as the fallback arm.
        if static_cls.is_none() {
            let cands = self.candidates(None, name, args.len());
            if !cands.is_empty() {
                self.emit_virtual_call(sc, recv, name, args, &cands, line)?;
                return Ok(self.virtual_ret_type(&cands, name));
            }
        }
        self.emit_kt_method(sc, recv, name, args, line)
    }

    /// The `(runtime tag, owning type)` pairs a call to `name` on a receiver of
    /// static class `cls` may land in: every instantiable type that is `cls` or
    /// a subtype of it and implements `name` at the requested arity. `cls` of
    /// `None` means "receiver type unknown" — every implementor is a candidate.
    fn candidates(&self, cls: Option<&str>, name: &str, argc: usize) -> Vec<(String, String)> {
        let Some(all) = self.method_index.get(name) else {
            return Vec::new();
        };
        all.iter()
            .filter(|(tag, _)| match cls {
                Some(c) => self.classes.get(tag).is_some_and(|m| m.mro.iter().any(|a| a == c)),
                None => true,
            })
            .filter(|(tag, _)| {
                self.classes
                    .get(tag)
                    .and_then(|m| m.methods.get(name))
                    .is_some_and(|s| s.arity == argc)
            })
            .cloned()
            .collect()
    }

    /// The declared return type shared by every candidate implementation, or
    /// `Unknown` when they disagree (nothing then relies on it statically).
    fn virtual_ret_type(&self, cands: &[(String, String)], name: &str) -> Type {
        let mut ty: Option<Type> = None;
        for (tag, _) in cands {
            let Some(sig) = self.classes.get(tag).and_then(|m| m.methods.get(name)) else {
                return Type::Unknown;
            };
            match ty {
                Some(t) if t != sig.ret => return Type::Unknown,
                _ => ty = Some(sig.ret),
            }
        }
        ty.unwrap_or(Type::Unknown)
    }

    /// Emit a call to `recv.name(args)` against a candidate set.
    ///
    /// A single candidate owner needs no test — every instance that reaches this
    /// site runs the same body, so the call is a direct `Op::Call` and the
    /// program pays nothing for the hierarchy. Otherwise the receiver is
    /// evaluated once into a slot, its runtime class tag read with
    /// [`KT_CLASSOF`], and each candidate's tag compared in turn; a receiver
    /// matching none falls through to the host stdlib dispatch, which is what
    /// keeps `x.length` working when `x` is a `String` rather than an instance.
    fn emit_virtual_call(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        args: &[Expr],
        cands: &[(String, String)],
        line: u32,
    ) -> Result<(), String> {
        let owners: HashSet<&str> = cands.iter().map(|(_, o)| o.as_str()).collect();
        if owners.len() == 1 {
            let owner = cands[0].1.clone();
            self.compile_expr(sc, recv)?;
            for a in args {
                self.compile_expr(sc, a)?;
            }
            let idx = self.b.add_name(&method_sub_name(&owner, name));
            self.b.emit(Op::Call(idx, (args.len() + 1) as u8), line);
            return Ok(());
        }
        let mark = sc.enter();
        self.compile_expr(sc, recv)?;
        let rslot = sc.temp();
        self.b.emit(Op::SetSlot(rslot), line);
        self.b.emit(Op::GetSlot(rslot), line);
        self.b.emit(Op::Extended(KT_CLASSOF, 0), line);
        let cslot = sc.temp();
        self.b.emit(Op::SetSlot(cslot), line);

        let mut done: Vec<usize> = Vec::new();
        for (tag, owner) in cands {
            self.b.emit(Op::GetSlot(cslot), line);
            let c = self.b.add_constant(Value::str(tag.clone()));
            self.b.emit(Op::LoadConst(c), line);
            self.b.emit(Op::StrEq, line);
            let miss = self.b.emit(Op::JumpIfFalse(0), line);
            self.b.emit(Op::GetSlot(rslot), line);
            for a in args {
                self.compile_expr(sc, a)?;
            }
            let idx = self.b.add_name(&method_sub_name(owner, name));
            self.b.emit(Op::Call(idx, (args.len() + 1) as u8), line);
            done.push(self.b.emit(Op::Jump(0), line));
            let next = self.b.current_pos();
            self.b.patch_jump(miss, next);
        }
        // Fallback: the universal host member dispatch on the stored receiver.
        self.b.emit(Op::GetSlot(rslot), line);
        for a in args {
            self.compile_expr(sc, a)?;
        }
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(Op::Extended(KT_METHOD, args.len() as u8), line);
        let end = self.b.current_pos();
        for j in done {
            self.b.patch_jump(j, end);
        }
        sc.exit(mark);
        Ok(())
    }

    /// Lower `super.m(args)` inside a method: resolve `m` against the enclosing
    /// class's ancestry — the nearest supertype that *implements* it — and call
    /// that body directly with the current `this`.
    fn compile_super_call(
        &mut self,
        sc: &mut Scope,
        qualifier: Option<&str>,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        let cls = self
            .cur_class
            .clone()
            .ok_or_else(|| format!("`super` outside a class (line {line})"))?;
        let meta = self.classes[&cls].clone();
        let implements = |a: &str| {
            self.classes
                .get(a)
                .is_some_and(|m| m.own_methods.contains(name))
        };
        // `super<T>.m()` names the supertype to run — the reason to write it is
        // that more than one supertype implements `m`, so resolving it by
        // supertype-list order (what the unqualified form does) would pick the
        // wrong body. `T` must be a *direct* supertype, as in Kotlin.
        let owner = match qualifier {
            Some(t) => {
                if !meta.parents.iter().any(|p| p == t) {
                    return Err(format!(
                        "{t} is not a direct supertype of {cls} (line {line})"
                    ));
                }
                if !implements(t) {
                    return Err(format!("{t} does not implement `{name}` (line {line})"));
                }
                t.to_string()
            }
            None => meta.mro[1..]
                .iter()
                .find(|a| implements(a))
                .cloned()
                .ok_or_else(|| {
                    format!("no supertype of {cls} implements `{name}` (line {line})")
                })?,
        };
        self.compile_expr(sc, &Expr::Var("this".into()))?;
        for a in args {
            self.compile_expr(sc, a)?;
        }
        let idx = self.b.add_name(&method_sub_name(&owner, name));
        self.b.emit(Op::Call(idx, (args.len() + 1) as u8), line);
        Ok(self
            .classes
            .get(&owner)
            .and_then(|m| m.methods.get(name))
            .map(|s| s.ret)
            .unwrap_or(Type::Unknown))
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
        if args.len() > meta.ctor_params.len() {
            return Err(format!(
                "{}.copy takes at most {} argument(s)",
                meta.name,
                meta.ctor_params.len()
            ));
        }
        let mark = sc.enter();
        self.compile_expr(sc, recv)?; // [recv]
        let rslot = sc.temp();
        self.b.emit(Op::SetSlot(rslot), 0);
        for (i, p) in meta.ctor_params.iter().enumerate() {
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
        // Kotlin's generated `copy` *calls the primary constructor*, so a data
        // class under a superclass re-runs its `: Super(args)` header rather
        // than carrying the receiver's base fields over — which matters as soon
        // as those arguments are written in terms of the constructor
        // parameters.
        let idx = self.b.add_name(&ctor_sub_name(&meta.name));
        self.b
            .emit(Op::Call(idx, meta.ctor_params.len() as u8), line);
        sc.exit(mark);
        Ok(Type::Obj)
    }

    /// Build a first-class lambda value at its literal site. The body is queued
    /// for emission as a subroutine (`compile_lambda_body`); here we push each
    /// captured upvalue (read from the enclosing frame), then the body's name
    /// index, parameter count, and capture count, and register the closure via
    /// the `KT_MAKE_CLOSURE` builtin (which returns a `Value::Obj` handle). An
    /// implicit-`it` lambda (no explicit params) takes one parameter, `it`.
    fn compile_lambda(
        &mut self,
        sc: &mut Scope,
        params: &[(String, Type)],
        body: &[Stmt],
    ) -> Result<Type, String> {
        // An unparameterized lambda has the single implicit parameter `it`.
        let effective: Vec<(String, Type)> = if params.is_empty() {
            vec![("it".to_string(), Type::Unknown)]
        } else {
            params.to_vec()
        };
        // Capture the whole visible enclosing environment (by value), minus names
        // the lambda's own parameters shadow. Captures use slots after the params
        // in the body; the push order here matches the body prologue's pop order.
        let caps: Vec<(String, u16, Type, Option<String>)> = sc
            .visible()
            .into_iter()
            .filter(|(n, _, _, _)| !effective.iter().any(|(p, _)| p == n))
            .collect();
        for (_, slot, _, _) in &caps {
            self.b.emit(Op::GetSlot(*slot), 0);
        }
        let id = self.lambdas_seen;
        self.lambdas_seen += 1;
        let name_idx = self.b.add_name(&format!("$lambda${id}"));
        self.pending_lambdas.push(PendingLambda {
            name_idx,
            params: effective.clone(),
            captures: caps
                .iter()
                .map(|(n, _, ty, cl)| (n.clone(), *ty, cl.clone()))
                .collect(),
            body: body.to_vec(),
            class: self.cur_class.clone(),
        });
        self.b.emit(Op::LoadInt(name_idx as i64), 0);
        self.b.emit(Op::LoadInt(effective.len() as i64), 0);
        self.b.emit(Op::LoadInt(caps.len() as i64), 0);
        self.b.emit(Op::CallBuiltin(KT_MAKE_CLOSURE, 0), 0);
        Ok(Type::Unknown)
    }

    /// Emit a queued lambda body as a subroutine region. Slots hold the
    /// parameters first (`0..n`), then the captured upvalues (`n..n+k`); the
    /// prologue pops the pushed params + captures top-down into those slots. The
    /// body lowers as a block-value (its last expression is the lambda's result),
    /// then a `ReturnValue` hands that value back to the invoking builtin.
    fn compile_lambda_body(&mut self, pl: PendingLambda) -> Result<(), String> {
        let entry = self.b.current_pos();
        self.b.add_sub_entry(pl.name_idx, entry);

        let mut sc = Scope::new();
        for (p, ty) in &pl.params {
            sc.declare(p, *ty, false);
        }
        for (n, ty, cl) in &pl.captures {
            sc.declare_obj(n, *ty, false, cl.clone());
        }
        let total = pl.params.len() + pl.captures.len();
        for i in (0..total).rev() {
            self.b.emit(Op::SetSlot(i as u16), 0);
        }

        let saved = self.cur_class.take();
        self.cur_class = pl.class.clone();
        // A lambda body is invoked through a nested `vm.run()`, so it is its own
        // frame for unwinding too: a raise inside it returns out, and the host
        // suppresses any further invocation while the exception is in flight.
        self.push_unwind(UnwindKind::Frame);
        let outer_returns = std::mem::take(&mut self.finally_returns);
        let res = self.compile_block_value(&mut sc, &pl.body);
        self.cur_class = saved;
        self.finally_returns = outer_returns;
        let here = self.b.current_pos();
        self.pop_unwind_to(here);
        res?;
        self.b.emit(Op::ReturnValue, 0);
        Ok(())
    }

    /// Lower a higher-order collection call `recv.name(extra…) { lambda }` to the
    /// `KT_COLL_HOF` builtin. The receiver, any leading non-closure args (e.g.
    /// `fold`'s initial), the closure (the last argument), and the method-name
    /// string are pushed; the builtin iterates and invokes the lambda per element.
    fn compile_coll_hof(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        // The lambda is always the last argument (trailing-lambda syntax, or a
        // passed closure value); anything before it is a leading value arg.
        let (closure, extras) = args.split_last().unwrap();
        self.compile_expr(sc, recv)?;
        for e in extras {
            self.compile_expr(sc, e)?;
        }
        self.compile_expr(sc, closure)?;
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b
            .emit(Op::CallBuiltin(KT_COLL_HOF, extras.len() as u8), line);
        Ok(hof_ret_type(name))
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

        // `x == null` / `x != null` is a null test, not a value comparison: the
        // native numeric/string ops would compare an absent value's coerced form
        // (`0` / `""`) and answer `true` for a non-null `0`/`""` receiver.
        if matches!(op, BinOp::Eq | BinOp::Ne)
            && (matches!(l, Expr::Null) || matches!(r, Expr::Null))
        {
            let value = if matches!(l, Expr::Null) { r } else { l };
            self.compile_expr(sc, value)?;
            self.b.emit(Op::Extended(KT_ISNULL, 0), 0);
            if op == BinOp::Ne {
                self.b.emit(Op::LogNot, 0);
            }
            return Ok(Type::Boolean);
        }

        // `+` is string concatenation when either side is a String.
        if op == BinOp::Add && (lt.is_str() || rt.is_str()) {
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
        let both_str = lt.is_str() && rt.is_str();
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
                    // IEEE division, not the native op: Kotlin's `x / 0.0` is a
                    // signed infinity and `0.0 / 0.0` is NaN, where `Op::Div`
                    // yields `Undef` (which printed as `null`).
                    self.b.emit(Op::Extended(KT_DDIV, 0), 0);
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
        // A call on a local binding invokes a first-class lambda value `f(args)`
        // — the local holds a closure handle. Locals win over same-named free
        // functions (lexical shadowing), matching Kotlin.
        if let Some(slot) = sc.slot(name) {
            self.b.emit(Op::GetSlot(slot), line);
            for a in args {
                self.compile_expr(sc, a)?;
            }
            self.b
                .emit(Op::CallBuiltin(KT_CLOSURE_CALL, args.len() as u8), line);
            return Ok(Type::Unknown);
        }
        match name {
            "println" | "print" => {
                if args.len() > 1 {
                    return Err(format!("{name} takes at most one argument in M0"));
                }
                let argc = args.len() as u8;
                if let Some(a) = args.first() {
                    let t = self.compile_expr(sc, a)?;
                    self.emit_display(t);
                }
                // In a program that uses exceptions the print goes through a
                // host builtin that is suppressed while an exception unwinds, so
                // nothing is emitted between a `throw` and its handler. Without a
                // `try` the native op stays (and the builtin's Unit result stands
                // in for the `LoadUndef` the native form needs).
                if self.has_try {
                    let id = if name == "println" {
                        KT_PRINTLN
                    } else {
                        KT_PRINT
                    };
                    self.b.emit(Op::CallBuiltin(id, argc), line);
                } else {
                    self.b.emit(
                        match (name, argc) {
                            ("println", 0) => Op::PrintLn(0),
                            ("println", _) => Op::PrintLn(1),
                            (_, 0) => Op::Print(0),
                            _ => Op::Print(1),
                        },
                        line,
                    );
                    self.b.emit(Op::LoadUndef, line); // println returns Unit
                }
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
            // Array builders → a heap array. The element values decide the JVM
            // descriptor at runtime (see `crate::host::array_desc`), so the
            // typed builders differ from `arrayOf` only in the primitive case.
            "arrayOf" | "intArrayOf" | "doubleArrayOf" | "booleanArrayOf" | "charArrayOf" => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_ARRAY, args.len() as u8), line);
                Ok(Type::Obj)
            }
            // `IntArray(n)` / `DoubleArray(n)` / `BooleanArray(n)` — a zero-filled
            // primitive array — and the initializer form `IntArray(n) { it * 2 }`,
            // which fills each slot with the lambda applied to its index. The
            // generic `Array(n) { … }` exists only in the initializer form
            // (Kotlin has no zero-filled `Array(n)`), so its descriptor is
            // inferred from the elements the lambda produced.
            "IntArray" | "DoubleArray" | "BooleanArray" | "CharArray" | "Array" => {
                let desc = match name {
                    "DoubleArray" => "[D",
                    "BooleanArray" => "[Z",
                    "CharArray" => "[C",
                    "Array" => "",
                    _ => "[I",
                };
                match args.len() {
                    2 => {
                        self.compile_expr(sc, &args[0])?; // size
                        let didx = self.b.add_constant(Value::str(desc));
                        self.b.emit(Op::LoadConst(didx), line);
                        self.compile_expr(sc, &args[1])?; // the init lambda
                        self.b.emit(Op::CallBuiltin(KT_ARRAY_INIT, 0), line);
                    }
                    1 if name != "Array" => {
                        self.compile_expr(sc, &args[0])?;
                        let didx = self.b.add_constant(Value::str(desc));
                        self.b.emit(Op::LoadConst(didx), line);
                        self.b.emit(Op::Extended(KT_ARRAY_NEW, 0), line);
                    }
                    _ => {
                        return Err(format!(
                            "{name} takes a size and (for `Array`, requires) an \
                             `{name}(n) {{ … }}` initializer lambda"
                        ))
                    }
                }
                Ok(Type::Obj)
            }
            // A built-in throwable constructor: `RuntimeException("boom")`,
            // `IllegalStateException()`. Only reached when no user class or local
            // shadows the name (the constructor/user-function arms run first).
            _ if self.is_throwable_ctor(name) && args.len() <= 1 => {
                let fqn = crate::host::throwable_fqn(name).unwrap();
                let fidx = self.b.add_constant(Value::str(fqn));
                self.b.emit(Op::LoadConst(fidx), line);
                match args.first() {
                    Some(a) => {
                        let t = self.compile_expr(sc, a)?;
                        self.emit_display(t);
                    }
                    // No message: `Throwable.message` is null.
                    None => {
                        self.b.emit(Op::LoadUndef, line);
                    }
                }
                self.b.emit(Op::CallBuiltin(KT_EXC_NEW, 0), line);
                Ok(Type::Obj)
            }
            // `kotlin.math` — resolvable only under `import kotlin.math.…`, which
            // is what Kotlin itself requires. `maxOf`/`minOf` live in the
            // auto-imported `kotlin` package and need no import.
            _ if self.resolve_math_fn(name).is_some() => {
                let rt = self.resolve_math_fn(name).unwrap();
                self.compile_math(sc, &rt, args, line)
            }
            // `Set` builders → an insertion-ordered distinct heap collection,
            // which is what Kotlin's `LinkedHashSet`-backed `setOf` is.
            "setOf" | "mutableSetOf" | "hashSetOf" | "linkedSetOf" | "sortedSetOf"
            | "emptySet" => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_SET, args.len() as u8), line);
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

    /// Whether `name` is a built-in throwable constructor (`RuntimeException`,
    /// `IllegalStateException`, …). A user `class`/`fun` of the same name
    /// shadows it, exactly as a user declaration shadows the imported JDK type.
    fn is_throwable_ctor(&self, name: &str) -> bool {
        crate::host::throwable_fqn(name).is_some()
            && !self.classes.contains_key(name)
            && !self.fun_sig.contains_key(name)
    }

    /// Resolve a bare name to the math function it dispatches to, honouring the
    /// import rules: an auto-imported `kotlin` name always resolves, a star
    /// import opens every `kotlin.math` function, and a single-name import opens
    /// exactly the name (or alias) it declared.
    fn resolve_math_fn(&self, name: &str) -> Option<String> {
        if let Some(rt) = auto_math_fn(name) {
            return Some(rt.to_string());
        }
        if self.math_star && is_math_fn(name) {
            return Some(name.to_string());
        }
        self.math_scope
            .get(name)
            .filter(|rt| is_math_fn(rt))
            .cloned()
    }

    /// As [`Compiler::resolve_math_fn`], for the `kotlin.math` constants.
    fn resolve_math_const(&self, name: &str) -> Option<String> {
        if self.math_star && is_math_const(name) {
            return Some(name.to_string());
        }
        self.math_scope
            .get(name)
            .filter(|rt| is_math_const(rt))
            .cloned()
    }

    /// Whether `recv` is the `java.lang.Math` class reference rather than a
    /// value. A local binding or a user class named `Math` shadows it, so the
    /// name is only treated as the JVM class when nothing else claims it.
    fn is_java_math(&self, sc: &Scope, recv: &Expr) -> bool {
        matches!(recv, Expr::Var(n) if n == "Math")
            && sc.slot("Math").is_none()
            && !self.classes.contains_key("Math")
    }

    /// `Math.PI` / `kotlin.math.PI` — a literal `Double` constant, so it folds at
    /// compile time rather than paying a host dispatch.
    fn compile_math_const(&mut self, name: &str, line: u32) -> Result<Type, String> {
        let v = if name == "PI" {
            std::f64::consts::PI
        } else {
            std::f64::consts::E
        };
        self.b.emit(Op::LoadFloat(v), line);
        Ok(Type::Double)
    }

    /// Lower a math call to the `KT_MATH` host dispatch: arguments deepest-first,
    /// then the runtime function name, with the argument count in the extension
    /// payload — the same shape `KT_METHOD` uses.
    fn compile_math(
        &mut self,
        sc: &mut Scope,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        let mut tys = Vec::with_capacity(args.len());
        for a in args {
            tys.push(self.compile_expr(sc, a)?);
        }
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(Op::Extended(KT_MATH, args.len() as u8), line);
        Ok(math_ret_type(name, &tys))
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
        if !meta.instantiable() {
            let what = if meta.is_object {
                "object"
            } else if meta.is_interface {
                "interface"
            } else {
                "abstract class"
            };
            return Err(format!("cannot construct {what} {}", meta.name));
        }
        if args.len() != meta.ctor_params.len() {
            return Err(format!(
                "constructor {} expects {} argument(s), got {}",
                meta.name,
                meta.ctor_params.len(),
                args.len()
            ));
        }
        for a in args {
            self.compile_expr(sc, a)?;
        }
        let idx = self.b.add_name(&ctor_sub_name(&meta.name));
        self.b.emit(Op::Call(idx, args.len() as u8), line);
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
                    let str_eq = sty.is_str() || et.is_str();
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
            Expr::Super { .. } => Type::Unknown,
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Double,
            Expr::Bool(_) => Type::Boolean,
            Expr::Char(_) => Type::Char,
            Expr::Null => Type::Unknown,
            Expr::Str(_) => Type::String,
            Expr::Var(n) => {
                if sc.slot(n).is_none() && self.resolve_math_const(n).is_some() {
                    Type::Double
                } else {
                    sc.ty(n)
                }
            }
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
                    if lt.is_str() || rt.is_str() {
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
            Expr::Call { name, args, .. } => match name.as_str() {
                "println" | "print" => Type::Unit,
                "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" | "mapOf"
                | "mutableMapOf" | "hashMapOf" | "emptyMap" | "setOf" | "mutableSetOf"
                | "hashSetOf" | "linkedSetOf" | "sortedSetOf" | "emptySet" | "arrayOf"
                | "intArrayOf" | "doubleArrayOf" | "booleanArrayOf" | "charArrayOf"
                | "IntArray" | "DoubleArray" | "BooleanArray" | "CharArray" | "Array" => Type::Obj,
                _ if self.classes.contains_key(name) => Type::Obj, // constructor
                // A math call keeps its `Int` overload for integral arguments —
                // this is what makes `abs(-7) / 2` truncate rather than divide.
                _ if self.resolve_math_fn(name).is_some() => {
                    let tys: Vec<Type> = args.iter().map(|a| self.infer(sc, a)).collect();
                    math_ret_type(&self.resolve_math_fn(name).unwrap(), &tys)
                }
                _ => self
                    .fun_sig
                    .get(name)
                    .map(|s| s.ret)
                    .unwrap_or(Type::Unknown),
            },
            // `s[i]` yields a `Char`; other element types are past the coarse
            // inference and stay `Unknown`.
            Expr::Index { recv, .. } => {
                if self.infer(sc, recv).is_str() {
                    Type::Char
                } else {
                    Type::Unknown
                }
            }
            Expr::Pair { .. } => Type::Obj,
            // A range and an array are heap objects; `in` is a predicate.
            Expr::Range { .. } | Expr::Step { .. } => Type::Obj,
            Expr::In { .. } | Expr::Is { .. } => Type::Boolean,
            // `++`/`--` keep the target's type, so `d++` on a `Double` stays a
            // `Double` for display and `/` dispatch.
            Expr::IncDec { target, .. } => self.infer(sc, target),
            Expr::Lambda { .. } => Type::Unknown,
            Expr::Member { recv, name, .. } => {
                if self.is_java_math(sc, recv) {
                    return Type::Double; // `Math.PI` / `Math.E`
                }
                // A property read on a known class yields the property's type.
                if let Some(cls) = self.infer_class(sc, recv) {
                    if let Some(p) = self.classes.get(&cls).and_then(|m| m.prop(name)) {
                        return p.ty;
                    }
                }
                method_ret_type(name)
            }
            Expr::MethodCall {
                recv, name, args, ..
            } => {
                // `Math.round` returns a `Long`; the rest follow the shared
                // math overload rule.
                if self.is_java_math(sc, recv) {
                    if name == "round" {
                        return Type::Long;
                    }
                    let tys: Vec<Type> = args.iter().map(|a| self.infer(sc, a)).collect();
                    return math_ret_type(name, &tys);
                }
                // A higher-order collection method's result type is fixed.
                if is_coll_hof(name) {
                    return hof_ret_type(name);
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
            // A `try`'s value is the body's or a handler's; joining them keeps
            // `val n = try { f() } catch (e: Exception) { 0 }` integral, which is
            // what decides whether a later `/` truncates.
            Expr::Try(t) => {
                let bt = t
                    .body
                    .last()
                    .map(|s| self.infer_stmt(sc, s))
                    .unwrap_or(Type::Unit);
                let mut joined = Some(bt);
                for arm in &t.catches {
                    let at = arm
                        .body
                        .last()
                        .map(|s| self.infer_stmt(sc, s))
                        .unwrap_or(Type::Unit);
                    joined = Some(join_ty(joined, at));
                }
                joined.unwrap_or(Type::Unit)
            }
            // `throw` is Kotlin's `Nothing`: it has no value to type.
            Expr::Throw(_) => Type::Unknown,
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
                match name.as_str() {
                    "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" => {
                        return Some("List".to_string())
                    }
                    "setOf" | "mutableSetOf" | "hashSetOf" | "linkedSetOf" | "sortedSetOf"
                    | "emptySet" => return Some("Set".to_string()),
                    "mapOf" | "mutableMapOf" | "hashMapOf" | "emptyMap" => {
                        return Some("Map".to_string())
                    }
                    _ => {}
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
                match name.as_str() {
                    "map" | "mapIndexed" | "flatMap" | "filter" | "filterNot" | "sortedBy"
                    | "sortedByDescending" | "toList" | "distinct" | "sorted"
                    | "sortedDescending" | "take" | "drop" => return Some("List".to_string()),
                    "toSet" | "toMutableSet" | "union" | "intersect" | "subtract" => {
                        return Some("Set".to_string())
                    }
                    "associate" | "associateBy" | "associateWith" | "groupBy" => {
                        return Some("Map".to_string())
                    }
                    _ => {}
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
            // Container names for dispatch. None of these is a user class, so
            // member access on them falls through to the host method table.
            Expr::Range { .. } | Expr::Step { .. } => Some("IntRange".to_string()),
            _ => None,
        }
    }
}

/// The math functions kotlinrs resolves. `abs`/`max`/`min`/`sqrt`/`floor`/
/// `ceil`/`round` are `kotlin.math` members (import-gated); `maxOf`/`minOf` are
/// `kotlin` package members and always in scope.
fn is_math_fn(name: &str) -> bool {
    matches!(
        name,
        "abs" | "max" | "min" | "sqrt" | "floor" | "ceil" | "round" | "maxOf" | "minOf"
    )
}

/// The `kotlin.math` constants.
fn is_math_const(name: &str) -> bool {
    matches!(name, "PI" | "E")
}

/// The math functions Kotlin auto-imports (the `kotlin` package), usable with no
/// `import` line. `maxOf`/`minOf` are the `kotlin` package spellings of the same
/// operation `kotlin.math.max`/`min` performs, so they share one implementation.
fn auto_math_fn(name: &str) -> Option<&'static str> {
    match name {
        "maxOf" => Some("max"),
        "minOf" => Some("min"),
        _ => None,
    }
}

/// Static result type of a math call, given its argument types. `sqrt` and the
/// rounding family are `Double`-only; `abs`/`max`/`min` are overloaded and keep
/// an `Int` result for integral arguments — which matters because it decides
/// whether a following `/` lowers to truncating integer or IEEE division.
fn math_ret_type(name: &str, args: &[Type]) -> Type {
    match name {
        "sqrt" | "floor" | "ceil" | "round" => Type::Double,
        // `java.lang.Math.round` is the odd one out: `Long`, not `Double`.
        "jround" => Type::Long,
        _ if args.contains(&Type::Double) => Type::Double,
        _ if args.iter().all(|t| t.is_int()) => Type::Int,
        _ => Type::Unknown,
    }
}

/// The [`crate::host::RangeForm`] discriminant the `KT_RANGE` op carries.
fn range_form(kind: RangeKind) -> u8 {
    match kind {
        RangeKind::Inclusive => 0,
        RangeKind::Until => 1,
        RangeKind::DownTo => 2,
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

/// The higher-order collection methods that take a first-class lambda value and
/// route to the `KT_COLL_HOF` builtin (see [`crate::host::coll_hof`]).
fn is_coll_hof(name: &str) -> bool {
    matches!(
        name,
        "map"
            | "mapIndexed"
            | "flatMap"
            | "filter"
            | "filterNot"
            | "forEach"
            | "fold"
            | "reduce"
            | "any"
            | "all"
            | "none"
            | "count"
            | "sumOf"
            | "maxByOrNull"
            | "minByOrNull"
            | "sortedBy"
            | "sortedByDescending"
            | "associate"
            | "associateBy"
            | "associateWith"
            | "groupBy"
    )
}

/// Static result type of a higher-order collection method, for display/`==` op
/// selection. Collection-returning methods yield a heap `Obj`; the rest are
/// coarse (`Unknown` display routes through the generic Kotlin stringifier).
fn hof_ret_type(name: &str) -> Type {
    match name {
        "map" | "mapIndexed" | "flatMap" | "filter" | "filterNot" | "sortedBy"
        | "sortedByDescending" | "associate" | "associateBy" | "associateWith" | "groupBy" => {
            Type::Obj
        }
        "forEach" => Type::Unit,
        "any" | "all" | "none" => Type::Boolean,
        "count" => Type::Int,
        _ => Type::Unknown,
    }
}

// ── Whole-body predicates (FFI detection, exception detection) ─────────────
//
// Both questions the compiler asks up front — “does this program contain a
// `rust { … }` block?” and “does it contain a `try`/`throw`?” — are the same
// recursive walk over every expression a body can reach, so they share one
// visitor. Adding an AST node therefore only has to teach [`expr_any`] about it.

/// True when any expression reachable from `body` satisfies `f`.
fn body_any(body: &[Stmt], f: &dyn Fn(&Expr) -> bool) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Let { init, .. } => expr_any(init, f),
        StmtKind::Assign { value, .. } => expr_any(value, f),
        StmtKind::SetMember { recv, value, .. } => expr_any(recv, f) || expr_any(value, f),
        StmtKind::SetIndex {
            recv, index, value, ..
        } => expr_any(recv, f) || expr_any(index, f) || expr_any(value, f),
        StmtKind::Destructure { init, .. } => expr_any(init, f),
        StmtKind::Return(Some(e)) => expr_any(e, f),
        StmtKind::Return(None) => false,
        StmtKind::While { cond, body, .. } => expr_any(cond, f) || body_any(body, f),
        StmtKind::For {
            start,
            end,
            step,
            body,
            ..
        } => {
            expr_any(start, f)
                || expr_any(end, f)
                || step.as_ref().is_some_and(|e| expr_any(e, f))
                || body_any(body, f)
        }
        StmtKind::ForIn { iter, body, .. } => expr_any(iter, f) || body_any(body, f),
        StmtKind::Break(_) | StmtKind::Continue(_) => false,
        StmtKind::If(ie) => if_any(ie, f),
        StmtKind::When(w) => when_any(w, f),
        StmtKind::Expr(e) => expr_any(e, f),
    })
}

fn if_any(ie: &IfExpr, f: &dyn Fn(&Expr) -> bool) -> bool {
    expr_any(&ie.cond, f)
        || body_any(&ie.then, f)
        || ie.els.as_deref().is_some_and(|b| body_any(b, f))
}

fn when_any(w: &WhenExpr, f: &dyn Fn(&Expr) -> bool) -> bool {
    w.subject.as_deref().is_some_and(|e| expr_any(e, f))
        || w.arms.iter().any(|arm| {
            body_any(&arm.body, f)
                || match &arm.guard {
                    WhenGuard::Else => false,
                    WhenGuard::Conds(conds) => conds.iter().any(|c| match c {
                        WhenCond::Expr(e) => expr_any(e, f),
                        WhenCond::InRange { start, end, .. } => {
                            expr_any(start, f) || expr_any(end, f)
                        }
                        WhenCond::Is { .. } => false,
                    }),
                }
        })
}

fn expr_any(e: &Expr, f: &dyn Fn(&Expr) -> bool) -> bool {
    if f(e) {
        return true;
    }
    match e {
        Expr::Call { args, .. } => args.iter().any(|a| expr_any(a, f)),
        Expr::Member { recv, .. } => expr_any(recv, f),
        Expr::MethodCall { recv, args, .. } => {
            expr_any(recv, f) || args.iter().any(|a| expr_any(a, f))
        }
        Expr::Unary { expr, .. } => expr_any(expr, f),
        Expr::Binary { l, r, .. } => expr_any(l, f) || expr_any(r, f),
        Expr::Elvis { left, right } => expr_any(left, f) || expr_any(right, f),
        Expr::NotNull(inner) => expr_any(inner, f),
        Expr::Index { recv, index, .. } => expr_any(recv, f) || expr_any(index, f),
        Expr::Pair { first, second } => expr_any(first, f) || expr_any(second, f),
        Expr::Range { start, end, .. } => expr_any(start, f) || expr_any(end, f),
        Expr::Step { recv, by } => expr_any(recv, f) || expr_any(by, f),
        Expr::In {
            value, container, ..
        } => expr_any(value, f) || expr_any(container, f),
        Expr::Is { value, .. } => expr_any(value, f),
        Expr::IncDec { target, .. } => expr_any(target, f),
        Expr::Lambda { body, .. } => body_any(body, f),
        Expr::If(ie) => if_any(ie, f),
        Expr::When(w) => when_any(w, f),
        Expr::Try(t) => {
            body_any(&t.body, f)
                || t.catches.iter().any(|c| body_any(&c.body, f))
                || body_any(&t.finally_body, f)
        }
        Expr::Throw(inner) => expr_any(inner, f),
        Expr::Str(parts) => parts.iter().any(|p| match p {
            StrExpr::Expr(e) => expr_any(e, f),
            StrExpr::Text(_) => false,
        }),
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Char(_)
        | Expr::Null
        | Expr::Super { .. }
        | Expr::Var(_) => false,
    }
}

/// True if any statement in `body` (recursively) evaluates a `__rust_compile`
/// call — the desugar target of a `rust { ... }` block.
fn body_has_ffi(body: &[Stmt]) -> bool {
    body_any(body, &|e| {
        matches!(e, Expr::Call { name, .. } if name == RUST_COMPILE)
    })
}

/// True when the program contains a `try` or a `throw` anywhere — in `main`, a
/// free `fun`, a method, or a lambda body. Only such a program pays for the
/// per-statement unwind checks and the suppressible print builtins.
pub fn uses_exceptions(program: &Program) -> bool {
    let has = |body: &[Stmt]| body_any(body, &|e| matches!(e, Expr::Try(_) | Expr::Throw(_)));
    program.funs.iter().any(|f| has(&f.body))
        || program
            .classes
            .iter()
            .any(|c| c.methods.iter().any(|m| has(&m.body)))
}

/// True when `body` can leave its enclosing loop by a `break`/`continue` — the
/// one shape a `try` with a `finally` cannot honour, because the jump would
/// bypass the finalizer. A `break`/`continue` inside a *nested* loop is absorbed
/// by that loop and does not count. (A `return` is honoured: it routes through
/// the `try`'s return path, which runs the finalizer first.)
fn body_breaks(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Break(_) | StmtKind::Continue(_) => true,
        StmtKind::If(ie) => body_breaks(&ie.then) || ie.els.as_deref().is_some_and(body_breaks),
        StmtKind::When(w) => w.arms.iter().any(|a| body_breaks(&a.body)),
        StmtKind::Expr(e) => expr_breaks(e),
        // A nested loop absorbs its own `break`/`continue`.
        _ => false,
    })
}

/// An `if`/`when` used as an *expression* can still hold a `break` in a branch
/// (`val x = if (c) break else 1`), so the scan follows those too.
fn expr_breaks(e: &Expr) -> bool {
    match e {
        Expr::If(ie) => body_breaks(&ie.then) || ie.els.as_deref().is_some_and(body_breaks),
        Expr::When(w) => w.arms.iter().any(|a| body_breaks(&a.body)),
        _ => false,
    }
}
