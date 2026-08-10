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
    COLL_COPY, COLL_DEFAULT_CAP, COLL_HASH, COLL_SORTED, KT_ARRAY, KT_ARRAY_INIT, KT_ARRAY_NEW,
    KT_AS, KT_BUILDER, KT_CHR_STRING, KT_CLASSOF, KT_CLOSURE_CALL, KT_COLL_HOF, KT_COMPARATOR,
    KT_DBG_LINE, KT_DDIV, KT_DISPLAY, KT_ENUM_REG, KT_EQUALS_REG, KT_EXC_ABORT, KT_EXC_CUT,
    KT_EXC_DEPTH, KT_EXC_MATCH, KT_EXC_NEW, KT_EXC_PENDING, KT_EXC_STASH, KT_EXC_TAKE,
    KT_EXC_THROW, KT_EXC_UNSTASH, KT_EXTEND, KT_FFI_CALL, KT_FFI_COMPILE, KT_GENSEQ, KT_GETFIELD,
    KT_HASH_REG, KT_IDENTITY, KT_IDIV, KT_IMOD, KT_INDEX_GET_VM, KT_INDEX_SET_VM, KT_IN_VM, KT_IS,
    KT_ISNULL, KT_ITER_GET, KT_ITER_SIZE, KT_JOIN, KT_LAZY_GET, KT_LAZY_NEW, KT_LIST,
    KT_MAKE_CLOSURE, KT_MAP_VM, KT_MATH, KT_METHOD_VM, KT_NEW, KT_NOTNULL, KT_OBJEQ_VM, KT_OPER_VM,
    KT_PAIR, KT_PRECOND, KT_PRINT, KT_PRINTLN, KT_RANGE, KT_RANGE_STEP, KT_RESULT_HOF,
    KT_RUN_CATCHING, KT_SCOPE_FN, KT_SETFIELD, KT_SET_VM, KT_TOSTRING_REG, KT_TO_STRING,
    KT_TYPE_REG,
};
use fusevm::{Chunk, ChunkBuilder, Op, Value};
use std::cell::RefCell;
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
    /// The ELEMENT type when this binding holds a sequence — the type of what a
    /// `for` variable or a lambda parameter over it receives. `Unknown` for
    /// everything else, and for a sequence whose elements do not agree.
    ///
    /// It exists so `listOf(1, 2).map { it * it }` can know `it` is an `Int` and
    /// wrap the product at 32 bits: Kotlin's integer width is a property of the
    /// STATIC TYPE, so an untyped lambda parameter is a place the frontend gave
    /// up, not a place the information was absent.
    elem: Type,
    /// The RESULT type when this binding holds a function value declared with a
    /// function type (`val f: (Int) -> Int`). `Unknown` for everything else,
    /// including a lambda whose type was inferred rather than written down.
    ///
    /// A call through the binding has this width, and Kotlin's integer width is
    /// a property of the STATIC type: without it `f(7) / f(2)` took the IEEE
    /// division path and answered `3.5` where the reference toolchain truncates
    /// to `3`, and `f(a) + f(b)` skipped the 32-bit wrap.
    fn_ret: Type,
    /// The TYPE ARGUMENTS of this binding's class, when it holds an instance of
    /// a generic class — `val b = Box(65536)` records `[Int]`. Empty for
    /// everything else, including a generic class whose argument the frontend
    /// could not resolve.
    ///
    /// A read of a `T`-typed property through the binding answers the argument
    /// at `T`'s index, which is what gives `b.v * b.v` its 32-bit wrap.
    type_args: Vec<TypeArg>,
    /// This `var` lives in a one-element heap cell rather than directly in its
    /// slot, because a lambda in the same frame ASSIGNS to it. A closure
    /// captures by value, so the only way a write inside the lambda can be seen
    /// by the frame that declared the variable is for both to hold the same heap
    /// handle and read/write through it — which is what the JVM backend does
    /// with its `Ref.IntRef` wrappers.
    boxed: bool,
    /// This `val` was declared `by lazy`, so the slot holds an unforced cell
    /// and every read forces it (see [`crate::host::KT_LAZY_GET`]).
    lazy: bool,
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
        self.declare_full(name, ty, mutable, class, Type::Unknown)
    }
    /// Declare a sequence-valued binding, recording its ELEMENT type so a
    /// lambda or `for` over it can type its variable (see [`Binding::elem`]).
    fn declare_elem(&mut self, name: &str, ty: Type, mutable: bool, elem: Type) -> u16 {
        self.declare_full(name, ty, mutable, None, elem)
    }
    fn declare_full(
        &mut self,
        name: &str,
        ty: Type,
        mutable: bool,
        class: Option<String>,
        elem: Type,
    ) -> u16 {
        let slot = self.next_slot;
        self.next_slot += 1;
        let prev = self.map.insert(
            name.to_string(),
            Binding {
                slot,
                ty,
                mutable,
                class,
                elem,
                fn_ret: Type::Unknown,
                type_args: Vec::new(),
                boxed: false,
                lazy: false,
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
    /// The element type recorded for a sequence-valued binding.
    fn elem_of(&self, name: &str) -> Type {
        self.map.get(name).map(|b| b.elem).unwrap_or(Type::Unknown)
    }
    /// Record the type ARGUMENTS of a generic-class binding — see
    /// [`Binding::type_args`].
    fn set_type_args(&mut self, name: &str, args: Vec<TypeArg>) {
        if let Some(b) = self.map.get_mut(name) {
            b.type_args = args;
        }
    }
    /// The type arguments recorded for `name`, empty when it holds no instance
    /// of a generic class or the arguments could not be resolved.
    fn type_args_of(&self, name: &str) -> Vec<TypeArg> {
        self.map
            .get(name)
            .map(|b| b.type_args.clone())
            .unwrap_or_default()
    }
    /// Record the declared RESULT type of a function-typed binding — see
    /// [`Binding::fn_ret`].
    fn set_fn_ret(&mut self, name: &str, ret: Type) {
        if let Some(b) = self.map.get_mut(name) {
            b.fn_ret = ret;
        }
    }
    /// The declared result type of a call through `name`, `Unknown` when the
    /// binding is not a function value with a written-down type.
    fn fn_ret_of(&self, name: &str) -> Type {
        self.map
            .get(name)
            .map(|b| b.fn_ret)
            .unwrap_or(Type::Unknown)
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
    fn visible(&self) -> Vec<Captured> {
        let mut out: Vec<Captured> = self
            .map
            .iter()
            .map(|(n, b)| Captured {
                name: n.clone(),
                slot: b.slot,
                ty: b.ty,
                class: b.class.clone(),
                elem: b.elem,
                boxed: b.boxed,
            })
            .collect();
        out.sort_by_key(|c| c.slot);
        out
    }
    /// Move `name`'s binding into a heap cell (see [`Binding::boxed`]).
    fn box_binding(&mut self, name: &str) {
        if let Some(b) = self.map.get_mut(name) {
            b.boxed = true;
        }
    }
    fn is_boxed(&self, name: &str) -> bool {
        self.map.get(name).is_some_and(|b| b.boxed)
    }
    fn mark_lazy(&mut self, name: &str) {
        if let Some(b) = self.map.get_mut(name) {
            b.lazy = true;
        }
    }
    fn is_lazy(&self, name: &str) -> bool {
        self.map.get(name).is_some_and(|b| b.lazy)
    }
}

/// A stored property of a class (or `object`).
#[derive(Clone)]
struct PropMeta {
    name: String,
    ty: Type,
    class: Option<String>,
    mutable: bool,
    /// Declared `by lazy` — the stored value is a cell, so every read forces it
    /// (see [`crate::host::KT_LAZY_GET`]).
    lazy: bool,
    /// Declared `by <delegate>` — the field stores the DELEGATE, and the class
    /// whose `getValue`/`setValue` every access routes through.
    delegate: Option<String>,
    /// See [`crate::ast::CtorProp::type_param_of`] — the index into the OWNING
    /// class's type-parameter list this property's declared type named, so a
    /// read resolves against the receiver's type argument.
    ///
    /// Only a class's OWN properties carry it. An inherited one is dropped to
    /// `None` in the flattened record below: the index is positional against the
    /// ANCESTOR's list, and the subclass's own list need not agree with it —
    /// `class Sub(x: Int) : Box<Int>(x)` has no type parameters at all.
    ///
    /// …unless the subclass WROTE the argument, which is exactly what
    /// `: Box<Int>` does. That case is resolved when the record is flattened:
    /// the index is substituted away and the concrete type lands in `ty` /
    /// `class` / `type_args` below, so what survives here is only the
    /// still-positional case.
    type_param_of: Option<usize>,
    /// The type arguments this property's declared type WROTE — the `[Int]` of
    /// `val b: Box<Int>`. Concrete rather than positional, so unlike
    /// `type_param_of` it survives inheritance untouched. See
    /// [`crate::ast::TypeArg`].
    type_args: Vec<TypeArg>,
}

/// Static signature of a user function or class method.
#[derive(Clone)]
struct FnSig {
    ret: Type,
    ret_class: Option<String>,
    /// See [`crate::ast::FunDecl::ret_type_param_of`] — the argument index a
    /// type-variable result reads its width from.
    ret_type_param_of: Option<usize>,
    /// See [`crate::ast::FunDecl::ret_class_type_param_of`] — the index into the
    /// owning class's type-parameter list a type-variable result names, read off
    /// the RECEIVER's type argument.
    ret_class_type_param_of: Option<usize>,
    /// See [`crate::ast::FunDecl::ret_type_args`] — the arguments the return
    /// annotation wrote, which is the only source of a generic result's width
    /// when neither an argument nor the receiver carries one.
    ret_type_args: Vec<TypeArg>,
    arity: usize,
    /// The parameters in declaration order. Names are what a named argument
    /// (`f(count = 3)`) binds against; the defaults and the `vararg` marker are
    /// what [`Compiler::expand_args`] fills an under-supplied call from.
    params: Vec<Param>,
}

impl FnSig {
    fn of(f: &FunDecl) -> FnSig {
        FnSig {
            ret: f.ret,
            ret_class: f.ret_class.clone(),
            ret_type_param_of: f.ret_type_param_of,
            ret_class_type_param_of: f.ret_class_type_param_of,
            ret_type_args: f.ret_type_args.clone(),
            arity: f.params.len(),
            params: f.params.clone(),
        }
    }
}

/// The mangled sub name an extension function's body is emitted under. `$`
/// cannot appear in a Kotlin identifier, so it can never collide with a free
/// function, a method, or another receiver's extension of the same name.
fn ext_sub_name(recv: &str, name: &str) -> String {
    format!("{recv}$ext${name}")
}

/// Compile-time metadata for a `class` / `data class` / `object` / `interface`,
/// driving constructor lowering, field access, method dispatch, and (for `data`)
/// synthesized-member routing.
#[derive(Clone)]
struct ClassMeta {
    name: String,
    /// How many type parameters the class declares — the length of the type
    /// ARGUMENT vector an instantiation of it produces. See [`TypeArg`].
    type_param_count: usize,
    is_data: bool,
    is_object: bool,
    /// `enum class` — its constants are singletons on its companion, it displays
    /// as its `name` and it orders by `ordinal`.
    is_enum: bool,
    is_interface: bool,
    /// `abstract class` / `sealed class` — has a constructor its subclasses call
    /// but cannot itself be instantiated.
    is_abstract: bool,
    /// Every stored field an instance carries, base-most first: the ancestors'
    /// fields, then this class's own. Drives property lookup and `data` display.
    props: Vec<PropMeta>,
    /// This class's own stored fields only — what its constructor contributes on
    /// top of the base instance. Equal to `props` for a class with no superclass.
    /// Primary-constructor properties first, then the body ones.
    own_props: Vec<PropMeta>,
    /// How many leading entries of `own_props` came from the primary
    /// constructor. A `data class`'s `toString`/`equals`/`hashCode`/`componentN`
    /// read only those — `data class D(val a: Int) { val b = 2 }` prints
    /// `D(a=1)` — so the count travels to the runtime in the meta string.
    data_len: usize,
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
    /// Whether a primary constructor was written. Without one, a `C()` call
    /// selects a no-argument SECONDARY rather than the implicit primary.
    has_primary: bool,
    /// The parameter counts of the secondary constructors, in declaration
    /// order. Constructor selection at a `C(args)` site is by arity first, then
    /// by argument type where more than one candidate takes that many.
    sec_arities: Vec<usize>,
    /// The secondary constructors' parameters, parallel to `sec_arities`, for
    /// binding named and defaulted arguments.
    sec_params: Vec<Vec<Param>>,
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
    /// `"Name\x1f(d|c)\x1fdataLen\x1fwidths\x1ffield0\x1f…"`. A subclass's base
    /// fields ride on the base instance `KT_EXTEND` builds from, so they are not
    /// repeated here.
    ///
    /// `dataLen` is how many of the own fields the primary constructor
    /// contributed — the ones a `data class`'s derived members read (see
    /// [`ClassMeta::data_len`]).
    ///
    /// `widths` is one character per own property, `'l'` for a declared `Long`
    /// and `'.'` otherwise. A `data class`'s generated `hashCode` needs it: the
    /// `Long` fold differs from the `Int` one and the two types share a runtime
    /// representation, so the declared width has to travel with the class (see
    /// [`crate::host`]'s `LONG_FIELDS`).
    fn meta_string(&self) -> String {
        let mut s = self.name.clone();
        s.push('\u{1f}');
        s.push(if self.is_data { 'd' } else { 'c' });
        s.push('\u{1f}');
        s.push_str(&self.data_len.to_string());
        s.push('\u{1f}');
        for p in &self.own_props {
            s.push(if p.ty == Type::Long { 'l' } else { '.' });
        }
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

/// The class an unannotated body property's initializer produces, where the
/// initializer names it syntactically: a constructor call for a declared class,
/// or one of the collection builders. `None` when the initializer is anything
/// else, which leaves the property's declared type alone.
///
/// This runs in the pre-pass that BUILDS the class table, so it cannot consult
/// [`Compiler::infer`] — that needs the very table being built. Only the two
/// forms whose class is visible in the syntax are answered; anything subtler
/// keeps the `Unknown` it had.
fn body_prop_class(p: &BodyProp, by_name: &HashMap<&str, &ClassDecl>) -> Option<String> {
    // A `by lazy`/delegated property stores a cell or a delegate, not the value.
    if p.lazy || p.delegate {
        return None;
    }
    match &p.init {
        Expr::Call { name, .. } if by_name.contains_key(name.as_str()) => Some(name.clone()),
        Expr::Call { name, .. } => match name.as_str() {
            "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" => Some("List".to_string()),
            "setOf" | "mutableSetOf" | "hashSetOf" | "linkedSetOf" | "sortedSetOf" | "emptySet" => {
                Some("Set".to_string())
            }
            "mapOf" | "mutableMapOf" | "hashMapOf" | "emptyMap" => Some("Map".to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The class of a property's delegate, read off the `by <expr>` initializer.
/// Only a direct constructor call names it — anything else leaves the delegate
/// unresolvable at compile time, which is reported where the access is emitted
/// rather than here.
fn delegate_class_of(init: &Expr) -> Option<String> {
    match init {
        Expr::Call { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// The `KProperty` argument a delegated access passes: a one-field data
/// instance carrying the property's name, which is what `property.name` reads.
fn kproperty_meta() -> String {
    "KProperty\u{1f}d\u{1f}1\u{1f}.\u{1f}name".to_string()
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

/// The mangled sub name for a class's Nth secondary constructor
/// (`Person#$ctor0`), numbered in declaration order.
fn sec_ctor_sub_name(class: &str, idx: usize) -> String {
    format!("{class}#$ctor{idx}")
}

/// The synthetic field a class extending a built-in throwable stores its
/// `Exception(message)` argument in, so `e.message` and the `Class: message`
/// display form both work.
const MESSAGE_FIELD: &str = "message";

/// The binding [`Compiler::compile_safe_member`] parks a `?.` receiver in so the
/// not-null path can re-enter the ordinary member lowering with a slot standing
/// in for the receiver. The `$` cannot appear in a lexed identifier, so this can
/// never shadow a name the program itself declares.
const SAFE_RECV: &str = "$safe";

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
    /// `(receiver type name, function name)` → signature, for the extension
    /// functions the program declares (`fun Int.dbl()`). Keyed on the receiver
    /// because the same name may be extended onto several types.
    extensions: HashMap<(String, String), FnSig>,
    /// class/object name → metadata, filled before lowering.
    classes: HashMap<String, ClassMeta>,
    /// Top-level `val`/`var` properties by name. They live in chunk globals, so
    /// every function sees them; a local of the same name shadows one, which is
    /// why the slot lookup always runs first.
    globals: HashMap<String, PropMeta>,
    /// method name → the `(runtime class tag, owning type)` pairs a call on that
    /// name may land in. Backs virtual dispatch; see [`build_method_index`].
    method_index: HashMap<String, Vec<(String, String)>>,
    /// The class whose method is currently being lowered (enables implicit
    /// `this` for member/method access). `None` at top level and in free funcs.
    cur_class: Option<String>,
    /// The `object` whose property initializers are being lowered right now.
    ///
    /// [`Compiler::build_object`] evaluates every initializer into a local slot
    /// and only publishes the singleton to its global once they are all done, so
    /// while it runs the global still holds nothing. A later initializer naming
    /// an earlier property through the QUALIFIED form — `val entries =
    /// listOf(Dir.NORTH)` inside `Dir`'s own companion, which is legal Kotlin and
    /// what the `enum` lowering emits — would therefore read an unset global and
    /// fail at runtime. Recording the object being built lets that read resolve
    /// to the slot instead, which is what the unqualified `NORTH` already does.
    building_object: Option<String>,
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
    /// Local `fun`s discovered while lowering, awaiting emission as ordinary
    /// subroutines once the enclosing body is finished (emitting one mid-body
    /// would splice it into the caller's instruction stream).
    pending_local_funs: Vec<PendingLocalFun>,
    /// Visible local-`fun` name → the unique sub its body was emitted under.
    /// Saved and restored around each `fun` body, so a local name is visible for
    /// the rest of its enclosing function and never leaks past it.
    local_funs: HashMap<String, String>,
    /// The signatures of those same local `fun`s. Kept apart from `fun_sig` so a
    /// local declaration cannot leak into another function's name resolution;
    /// it shadows a top-level function of the same name while in scope.
    local_sigs: HashMap<String, FnSig>,
    /// Monotonic id for the mangled local-`fun` sub names.
    local_funs_seen: u32,
    /// The names a lambda somewhere in the CURRENT frame's body assigns to. A
    /// `var` declared in this frame under one of them is stored boxed, so the
    /// lambda's write is visible to the frame (see [`Binding::boxed`]).
    /// Recomputed on entry to each `fun`/lambda body.
    boxed_vars: HashSet<String>,
    /// Parameter types for the NEXT lambda literal to be lowered, published by
    /// the call site that is about to consume it (see
    /// [`Compiler::compile_coll_hof`]) and taken by [`Compiler::compile_lambda`].
    ///
    /// Kotlin infers a lambda parameter's type from the callee, and the width of
    /// an integer is part of that type. Without this the frontend saw only
    /// `it` — an operand of no known type — and had to keep every result 64 bits
    /// wide in case it was a `Long`, which silently skipped the `Int` wrap that
    /// `listOf(2000000000).map { it + it }` needs.
    /// Each entry is `(type, element type)`: the parameter's own type, and —
    /// when it is itself a sequence (a `windowed` group, a nested list) — the
    /// type of ITS elements, so a second lambda one level down is typed too.
    lambda_hint: Option<Vec<(Type, Type)>>,
    /// The RECEIVER type for the next lambda literal, published by a
    /// receiver-scope call site (`x.run { … }`, `x.apply { … }`,
    /// `with(x) { … }`). It makes the block's first parameter `this` instead of
    /// `it`, which is the only difference between the two families of scope
    /// function — and what lets the block name the receiver's members with no
    /// qualifier.
    lambda_recv: Option<(Type, Option<String>)>,
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
    finally_exits: Vec<FinallyExit>,
}

/// One enclosing `try`-with-`finally`'s return path: the slot a pending return
/// value waits in, and the jumps from each `return` statement inside it.
struct FinallyReturn {
    slot: u16,
    jumps: Vec<usize>,
}

/// The parked `break`/`continue` jumps of a `try` that owns a `finally`, for
/// exits whose target loop lies OUTSIDE that `try` — leaving it has to run the
/// finalizer first. An exit to a loop *inside* the `try` crosses nothing and is
/// never parked here.
struct FinallyExit {
    /// `self.loops.len()` when the `try` was entered. A target loop at a lower
    /// index is outside the `try`, which is exactly the crossing test.
    loops_at_entry: usize,
    /// `(jump site, is_break, label)` — the label decides which loop the exit
    /// resumes at once the finalizer has run.
    jumps: Vec<(usize, bool, Option<String>)>,
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

/// A local `fun` queued for emission, with the name environment its body must
/// see. Both tables are snapshots taken at the declaration: the body has to
/// resolve ITS OWN name (that is what makes a local `fun` able to recurse where
/// a closure cannot) and every local `fun` declared before it, and nothing that
/// the enclosing function declared afterwards.
struct PendingLocalFun {
    decl: FunDecl,
    local_funs: HashMap<String, String>,
    local_sigs: HashMap<String, FnSig>,
}

/// A lambda body queued for emission as a subroutine. `params` already has the
/// implicit `it` injected when the literal had no explicit parameters. `captures`
/// are the enclosing-frame bindings the lambda closes over (its upvalues), in the
/// same order the make-closure site pushed their values and the body's prologue
/// pops them into slots. `class` carries the enclosing class context so a lambda
/// defined inside a method can still reach `this`/fields.
struct PendingLambda {
    name_idx: u16,
    /// `(name, type, element type)` per parameter — the element type is what a
    /// lambda nested one level deeper types ITS parameter from.
    params: Vec<(String, Type, Type)>,
    captures: Vec<Captured>,
    body: Vec<Stmt>,
    class: Option<String>,
    /// The class/container name of a receiver-scope block's `this`, when the
    /// frontend could name it. Distinct from `class`, which is the *user* class
    /// whose members are in implicit scope: a `String` receiver names no user
    /// class but still binds `this`.
    recv_class: Option<String>,
    /// The local-`fun` environment visible at the literal, snapshotted for the
    /// same reason a queued local `fun` snapshots one: the body is emitted long
    /// after the frame that declared those names finished lowering.
    local_funs: HashMap<String, String>,
    local_sigs: HashMap<String, FnSig>,
}

/// One captured binding as a lambda sees it. `slot` is the ENCLOSING frame's,
/// read at closure-creation time; the lambda body re-declares the name in its
/// own frame. `boxed` travels because a captured heap cell must still be
/// read/written through, not treated as the value itself.
#[derive(Clone)]
struct Captured {
    name: String,
    slot: u16,
    ty: Type,
    class: Option<String>,
    elem: Type,
    boxed: bool,
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

/// Where a virtual call may land, and whether the receiver's class is known.
///
/// `static_recv` is what decides if the runtime class-tag test may be skipped
/// for a single candidate: with a KNOWN receiver class every instance reaching
/// the site runs the same body, while with an unknown one the receiver may not
/// be a candidate at all and the test is load-bearing.
#[derive(Clone, Copy)]
struct Targets<'a> {
    cands: &'a [(String, String)],
    static_recv: bool,
}

impl<'a> Targets<'a> {
    /// The receiver's static class picked these candidates.
    fn statik(cands: &'a [(String, String)]) -> Self {
        Self {
            cands,
            static_recv: true,
        }
    }

    /// The receiver's class is unknown; these are every class declaring the name.
    fn dynamic(cands: &'a [(String, String)]) -> Self {
        Self {
            cands,
            static_recv: false,
        }
    }
}

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
    // A supertype's DECLARED property (`val name: String` in an interface,
    // `abstract val` in a class) is overridable too, and an implementor may
    // satisfy it either with a stored `override val` or with a getter — which
    // lowers to a zero-argument method. So a zero-argument `override` also
    // counts as overriding a declared property of that name.
    let inherited_prop = |name: &str, arity: usize| -> bool {
        arity == 0
            && mro[1..]
                .iter()
                .filter_map(|a| by_name.get(a.as_str()))
                .any(|d| d.abstract_props.iter().any(|p| p.name == name))
    };
    for m in &cd.methods {
        let found = inherited(&m.name, m.params.len());
        match (m.is_override, found) {
            (true, None)
                if !ANY_MEMBERS.contains(&m.name.as_str())
                    && !inherited_prop(&m.name, m.params.len()) =>
            {
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

        // A `by <delegate>` whose class cannot be named at compile time has no
        // `getValue` to call. Rejected here rather than left to become a plain
        // stored field, which would silently print the DELEGATE where the
        // property's value belongs. `kotlin.properties.Delegates.observable` /
        // `vetoable` land here: they are stdlib factory calls, not constructor
        // calls, and no host-side delegate object backs them yet.
        for p in cd.obj_props.iter().filter(|p| p.delegate) {
            if !delegate_class_of(&p.init).is_some_and(|c| by_name.contains_key(c.as_str())) {
                return Err(format!(
                    "class {}: property {} delegates to a value whose class is not a \
                     user class declaring `operator fun getValue`; only that form of \
                     `by` is supported (besides `by lazy`)",
                    cd.name, p.name
                ));
            }
        }

        let own_field = |p: &CtorProp| PropMeta {
            name: p.name.clone(),
            ty: p.ty,
            class: p.class.clone(),
            mutable: p.kind == PropKind::Var,
            lazy: false,
            delegate: None,
            type_param_of: p.type_param_of,
            type_args: p.type_args.clone(),
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
        if throwable_base.is_some()
            && cd
                .parents
                .iter()
                .any(|p| Some(p) == throwable_base.as_ref())
        {
            own_props.push(PropMeta {
                name: MESSAGE_FIELD.to_string(),
                ty: Type::NullableString,
                class: None,
                mutable: false,
                lazy: false,
                delegate: None,
                // A throwable's `message` is a `String?`, never a type variable.
                type_param_of: None,
                type_args: Vec::new(),
            });
        }
        own_props.extend(
            cd.params
                .iter()
                .filter(|p| p.kind != PropKind::None)
                .map(own_field),
        );
        // Everything up to here comes from the primary constructor, which is
        // exactly what a `data class`'s derived members read (see `data_len`).
        let data_len = own_props.len();
        own_props.extend(cd.obj_props.iter().map(|p| PropMeta {
            name: p.name.clone(),
            ty: match (p.ty, body_prop_class(p, &by_name)) {
                // An unannotated body property holding a heap object types as
                // one. Without this its type stays `Unknown`, and a comparison
                // of two of them (`O.a == O.b`) would miss the object-equality
                // path and compare the raw handles with the native op.
                (Type::Unknown, Some(_)) => Type::Obj,
                (t, _) => t,
            },
            class: p.class.clone().or_else(|| body_prop_class(p, &by_name)),
            mutable: p.mutable,
            lazy: p.lazy,
            // The delegate's class, taken from the initializer — `by Upper()`
            // names `Upper`, whose `getValue`/`setValue` the accesses call.
            delegate: p.delegate.then(|| delegate_class_of(&p.init)).flatten(),
            // A BODY property does not FIX a type argument — it is not a
            // constructor parameter — but it does READ the one the construction
            // site fixed, exactly as a `T`-returning method does off its
            // receiver. `class Box<T>(v: T) { val w: T = v }` therefore answers
            // `Int` for `Box(65536).w`.
            type_param_of: p.type_param_of,
            type_args: p.type_args.clone(),
        }));

        // The type argument this class WROTE for a DIRECT supertype's `k`th type
        // parameter: `class Sub : Box<Int>()` answers `Int` for `("Box", 0)`.
        //
        // Only a direct parent is substituted. A grandparent's variable is
        // positional against the parent's list, and carrying it further would
        // mean composing substitutions through an intermediate class that may
        // pass its OWN variable up (`class B<U> : A<U>()`) — a type expression
        // the coarse system cannot represent. Every unsubstituted position stays
        // as it was, which narrows nothing.
        let written_arg = |anc: &str, k: usize| -> Option<TypeArg> {
            let at = cd.parents.iter().position(|p| p == anc)?;
            let arg = cd.parent_args.get(at)?.get(k)?;
            (!arg.is_unknown()).then(|| arg.clone())
        };
        // The same field seen from a SUBCLASS. A type-variable index is
        // positional against the ANCESTOR's list, which the subclass's own need
        // not share, so it is dropped — unless the subclass wrote the argument
        // in its supertype list, in which case the variable resolves to it and
        // the field is concrete from here down.
        let inherited = |anc: &str, p: &PropMeta| -> PropMeta {
            match p.type_param_of.and_then(|k| written_arg(anc, k)) {
                Some(arg) => PropMeta {
                    ty: arg.ty,
                    class: arg.class,
                    type_param_of: None,
                    type_args: arg.args,
                    ..p.clone()
                },
                None => PropMeta {
                    type_param_of: None,
                    ..p.clone()
                },
            }
        };

        // The full field record, base-most first — the order `KT_EXTEND` builds
        // an instance in, so property lookup and `data` display agree with it.
        let mut props: Vec<PropMeta> = Vec::new();
        for anc in mro.iter().skip(1).rev() {
            let Some(d) = by_name.get(anc.as_str()) else {
                continue;
            };
            for p in d.params.iter().filter(|p| p.kind != PropKind::None) {
                if !props.iter().any(|x| x.name == p.name) {
                    props.push(inherited(anc, &own_field(p)));
                }
            }
            // An ancestor's body properties are stored fields too, and sit after
            // its constructor ones — the same order its own constructor built
            // them in, which is what keeps the flat record aligned with the
            // instance.
            for p in &d.obj_props {
                if !props.iter().any(|x| x.name == p.name) {
                    props.push(inherited(
                        anc,
                        &PropMeta {
                            name: p.name.clone(),
                            ty: p.ty,
                            class: p.class.clone(),
                            mutable: p.mutable,
                            lazy: p.lazy,
                            delegate: p.delegate.then(|| delegate_class_of(&p.init)).flatten(),
                            type_param_of: p.type_param_of,
                            type_args: p.type_args.clone(),
                        },
                    ));
                }
            }
        }
        for p in &own_props {
            if !props.iter().any(|x| x.name == p.name) {
                props.push(p.clone());
            }
        }
        // A property this type only DECLARES (`val name: String` in an
        // interface, `abstract val` in a class). It reserves the name so a
        // receiver of this type resolves the read — and so an inherited default
        // method's body can name it — while owning no field: the read lands on
        // the storage the implementor's own `override val` contributed, which is
        // already in the record above for a class that has one.
        //
        // Appended LAST, and never into `own_props`, so it cannot shift the
        // field layout `KT_NEW`/`KT_EXTEND` build an instance in.
        for p in &cd.abstract_props {
            if !props.iter().any(|x| x.name == p.name) {
                props.push(PropMeta {
                    name: p.name.clone(),
                    ty: p.ty,
                    class: p.class.clone(),
                    mutable: p.kind == PropKind::Var,
                    lazy: false,
                    delegate: None,
                    type_param_of: None,
                    type_args: p.type_args.clone(),
                });
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
                methods
                    .entry(m.name.clone())
                    .or_insert_with(|| FnSig::of(m));
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
                type_param_count: cd.type_params.len(),
                is_data: cd.is_data,
                is_object: cd.is_object,
                is_enum: cd.is_enum,
                is_interface: cd.is_interface,
                is_abstract: cd.is_abstract || cd.is_sealed,
                props,
                own_props,
                data_len,
                ctor_params: cd.params.clone(),
                methods,
                own_methods,
                mro,
                parents: cd.parents.clone(),
                base,
                super_args: cd.super_args.clone(),
                has_primary: cd.has_primary,
                sec_arities: cd.secondaries.iter().map(|s| s.params.len()).collect(),
                sec_params: cd.secondaries.iter().map(|s| s.params.clone()).collect(),
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
    let by_name: HashMap<&str, &ClassDecl> = program
        .classes
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
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
    // Extensions live in their own table keyed by `(receiver type, name)`: they
    // are NOT callable as free functions, and two receivers may each declare one
    // of the same name.
    let mut fun_sig = HashMap::new();
    let mut extensions: HashMap<(String, String), FnSig> = HashMap::new();
    for f in &program.funs {
        match &f.recv {
            Some((recv, _, _)) => {
                if extensions
                    .insert((recv.clone(), f.name.clone()), FnSig::of(f))
                    .is_some()
                {
                    return Err(format!("conflicting declarations for {recv}.{}", f.name));
                }
            }
            None => {
                fun_sig.insert(f.name.clone(), FnSig::of(f));
            }
        }
    }

    let mut globals: HashMap<String, PropMeta> = HashMap::new();
    for p in &program.props {
        if globals
            .insert(
                p.name.clone(),
                PropMeta {
                    name: p.name.clone(),
                    ty: p.ty,
                    class: p.class.clone(),
                    mutable: p.mutable,
                    lazy: p.lazy,
                    delegate: p.delegate.then(|| delegate_class_of(&p.init)).flatten(),
                    // A top-level property belongs to no class, so it has no
                    // type parameter to name.
                    type_param_of: None,
                    type_args: p.type_args.clone(),
                },
            )
            .is_some()
        {
            return Err(format!("conflicting declarations for top-level {}", p.name));
        }
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
        extensions,
        classes,
        globals,
        method_index,
        cur_class: None,
        building_object: None,
        debug,
        has_ffi,
        loops: Vec::new(),
        pending_lambdas: Vec::new(),
        lambdas_seen: 0,
        pending_local_funs: Vec::new(),
        local_funs: HashMap::new(),
        local_sigs: HashMap::new(),
        local_funs_seen: 0,
        boxed_vars: HashSet::new(),
        lambda_hint: None,
        lambda_recv: None,
        math_scope: HashMap::new(),
        math_star: false,
        has_try: uses_exceptions(program),
        unwind: Vec::new(),
        finally_returns: Vec::new(),
        finally_exits: Vec::new(),
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
    c.emit_equality_registry();
    c.emit_enum_registry(program);
    for cd in &program.classes {
        if cd.is_object {
            c.build_object(cd)?;
        }
    }
    // Top-level properties initialize in declaration order, before `main`. A
    // `by lazy` one stores an unforced cell instead — its thunk runs at the
    // first READ, which is the whole difference between the two forms.
    for p in &program.props {
        let mut sc = Scope::new();
        let t = c.compile_expr(&mut sc, &p.init)?;
        if p.lazy {
            c.b.emit(Op::Extended(KT_LAZY_NEW, 0), 0);
        } else if c.globals[&p.name].ty == Type::Unknown {
            // An unannotated global takes the initializer's type.
            if let Some(g) = c.globals.get_mut(&p.name) {
                g.ty = t;
            }
        }
        let g = c.b.add_name(&p.name);
        c.b.emit(Op::SetVar(g), 0);
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
            for (i, sec) in cd.secondaries.iter().enumerate() {
                c.compile_secondary_ctor(cd, i, sec)?;
            }
        }
        for m in &cd.methods {
            if !m.is_abstract {
                c.compile_fun(m, Some(&cd.name))?;
            }
        }
    }
    // Lambda bodies emit as subroutine regions last. Draining may enqueue further
    // lambdas (one nested inside another), so loop until the queue is empty.
    // Lambda bodies and local `fun` bodies both emit after everything that can
    // enqueue them, and each may enqueue the other, so the two queues drain
    // together until both are empty.
    loop {
        if let Some(pl) = c.pending_lambdas.pop() {
            c.compile_lambda_body(pl)?;
            continue;
        }
        match c.pending_local_funs.pop() {
            Some(pf) => {
                c.local_funs = pf.local_funs;
                c.local_sigs = pf.local_sigs;
                c.compile_fun(&pf.decl, None)?;
                c.local_funs.clear();
                c.local_sigs.clear();
            }
            None => break,
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
        // Each initializer is evaluated in order and bound to a slot, so a later
        // one can name an earlier property (`val a = 1; val b = a + 1`). The
        // slots are read back below in field order.
        let outer = self.building_object.replace(cd.name.clone());
        for p in &cd.obj_props {
            let t = self.compile_expr(&mut sc, &p.init)?;
            if p.lazy {
                self.b.emit(Op::Extended(KT_LAZY_NEW, 0), cd.line);
            }
            let ty = if p.ty == Type::Unknown { t } else { p.ty };
            let slot = sc.declare_obj(&p.name, ty, p.mutable, p.class.clone());
            self.b.emit(Op::SetSlot(slot), cd.line);
        }
        self.building_object = outer;
        for p in &cd.obj_props {
            let slot = sc.slot(&p.name).expect("body property just declared");
            self.b.emit(Op::GetSlot(slot), cd.line);
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
            // Bind the INHERITED properties to slots, read off the base
            // instance the call just returned. A body property initializer and
            // an `init` block may both name one — `class Sub(x: Int) : Base(x)
            // { init { println(b) } }` — and at this point the subclass
            // instance does not exist yet, so there is no `this` to read them
            // through. The base instance is left back on the stack for the
            // `KT_EXTEND` at the end.
            let inherited = meta.props.len() - meta.own_props.len();
            if inherited > 0 {
                let base_slot = sc.declare_obj("$base", Type::Obj, false, Some(base.clone()));
                self.b.emit(Op::SetSlot(base_slot), cd.line);
                for p in &meta.props[..inherited] {
                    self.b.emit(Op::GetSlot(base_slot), cd.line);
                    let nidx = self.b.add_constant(Value::str(p.name.clone()));
                    self.b.emit(Op::LoadConst(nidx), cd.line);
                    self.b.emit(Op::Extended(KT_GETFIELD, 0), cd.line);
                    let slot = sc.declare_obj(&p.name, p.ty, p.mutable, p.class.clone());
                    self.b.emit(Op::SetSlot(slot), cd.line);
                }
                self.b.emit(Op::GetSlot(base_slot), cd.line);
            }
        }

        // Body properties (`class C { var c = 0 }`) initialize after the
        // superclass constructor has run — Kotlin's own order — and each binds a
        // slot so a later initializer can name an earlier property. The base
        // instance is already on the stack; every initializer is stack-neutral
        // (push then `SetSlot`), so it stays put.
        //
        // `init { … }` blocks are INTERLEAVED here rather than run as a group:
        // Kotlin executes the property initializers and the `init` blocks in
        // one declaration-order pass, so a block sees every property declared
        // above it and none below it. Each block records how many properties
        // preceded it, which is what drives the interleaving.
        // A property initializer or an `init` block may contain a lambda that
        // WRITES an enclosing property (`val n = run { log += "x"; 1 }`). Such a
        // variable has to live in a box the closure shares, exactly as in a
        // function body — without this the write is compiled against the
        // constructor's own slot, which the closure only ever sees a copy of.
        let ctor_body: Vec<Stmt> = cd
            .obj_props
            .iter()
            .map(|p| Stmt::new(cd.line, StmtKind::Expr(p.init.clone())))
            .chain(cd.inits.iter().flat_map(|b| b.body.iter().cloned()))
            .collect();
        let outer_boxed = std::mem::replace(&mut self.boxed_vars, lambda_writes(&ctor_body));
        let res: Result<(), String> = (|| {
            for (i, p) in cd.obj_props.iter().enumerate() {
                self.emit_init_blocks(&mut sc, cd, i)?;
                let t = self.compile_expr(&mut sc, &p.init)?;
                if p.lazy {
                    self.b.emit(Op::Extended(KT_LAZY_NEW, 0), cd.line);
                }
                let ty = if p.ty == Type::Unknown { t } else { p.ty };
                let slot = sc.declare_obj(&p.name, ty, p.mutable, p.class.clone());
                self.b.emit(Op::SetSlot(slot), cd.line);
            }
            self.emit_init_blocks(&mut sc, cd, cd.obj_props.len())
        })();
        self.boxed_vars = outer_boxed;
        res?;

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

    /// Emit every `init { … }` block declared after exactly `done` body
    /// properties, in source order.
    ///
    /// The instance does not exist yet at this point — it is allocated at the
    /// very end of the constructor — so a block reads the properties from the
    /// constructor frame's slots, which is where the initializers above it just
    /// put them. That is enough for the observable ordering Kotlin specifies;
    /// what it cannot do is call a method on the not-yet-built `this`.
    fn emit_init_blocks(
        &mut self,
        sc: &mut Scope,
        cd: &ClassDecl,
        done: usize,
    ) -> Result<(), String> {
        for blk in cd.inits.iter().filter(|b| b.after_props == done) {
            for s in &blk.body {
                self.compile_stmt(sc, s)?;
            }
        }
        Ok(())
    }

    /// Emit a secondary constructor's subroutine `Class#$ctorN`.
    ///
    /// The lowering mirrors the order Kotlin specifies and `kotlinc` shows:
    /// the delegated constructor runs to completion FIRST — property
    /// initializers, `init` blocks and, for a `: this(…)` chain, the other
    /// secondary's body included — and only then does this body run, on the
    /// instance that came back. The instance is bound as `this`, so the body
    /// assigns properties through the ordinary field path.
    fn compile_secondary_ctor(
        &mut self,
        cd: &ClassDecl,
        idx: usize,
        sec: &SecondaryCtor,
    ) -> Result<(), String> {
        let entry = self.b.current_pos();
        let name_idx = self.b.add_name(&sec_ctor_sub_name(&cd.name, idx));
        self.b.add_sub_entry(name_idx, entry);

        let mut sc = Scope::new();
        for p in &sec.params {
            sc.declare_obj(&p.name, p.ty, false, p.class.clone());
        }
        for i in (0..sec.params.len()).rev() {
            self.b.emit(Op::SetSlot(i as u16), sec.line);
        }

        // The delegation target. `: super(…)` is not a separate object here —
        // the primary constructor is the one that chains to the superclass — so
        // both spellings run the primary, which is also what an absent clause
        // does. `: this(…)` with an arity no primary can take picks the
        // secondary of that arity instead.
        let meta = self.classes[&cd.name].clone();
        // Where the delegation goes:
        //
        // * no clause — the primary, with no arguments. This is the shape a
        //   class with no primary constructor has, and it is what makes its
        //   property initializers and `init` blocks run exactly once, before
        //   this body.
        // * `: this(…)` — the same selection a `C(args)` call site uses, so
        //   defaults are filled and named arguments reordered identically. A
        //   secondary that selected ITSELF would recurse forever, which Kotlin
        //   rejects outright.
        // * `: super(…)` — the primary is the only thing that chains to the
        //   superclass here, so a NON-EMPTY argument list has nowhere to go.
        let explicit_this = sec.deleg.as_ref().filter(|d| !d.is_super);
        if let Some(d) = sec
            .deleg
            .as_ref()
            .filter(|d| d.is_super && !d.args.is_empty())
        {
            return Err(format!(
                "class {}: `: super({} argument(s))` from a secondary constructor is not \
                 supported; delegate through the primary constructor instead (line {})",
                cd.name,
                d.args.len(),
                sec.line
            ));
        }
        let args: Vec<Expr> = explicit_this.map(|d| d.args.clone()).unwrap_or_default();
        let (target_sec, params) = match explicit_this {
            Some(_) => self.select_ctor(&sc, &meta, &args),
            None => (
                None,
                meta.ctor_params.iter().map(|p| p.as_param()).collect(),
            ),
        };
        if target_sec == Some(idx) {
            return Err(format!(
                "class {}: secondary constructor delegates to itself (line {})",
                cd.name, sec.line
            ));
        }
        let target = match target_sec {
            Some(i) => sec_ctor_sub_name(&cd.name, i),
            None => ctor_sub_name(&cd.name),
        };
        self.cur_class = Some(cd.name.clone());
        let outer_boxed = std::mem::replace(&mut self.boxed_vars, lambda_writes(&sec.body));
        let res: Result<(), String> = (|| {
            let full = self.expand_args(&format!("constructor {}", cd.name), &params, &args)?;
            for a in &full {
                self.compile_expr(&mut sc, a)?;
            }
            let tidx = self.b.add_name(&target);
            self.b.emit(Op::Call(tidx, params.len() as u8), sec.line);
            // The delegated constructor's result is the instance; bind it as
            // `this` so the body's property reads and writes resolve.
            let this = sc.declare_obj("this", Type::Obj, false, Some(cd.name.clone()));
            self.b.emit(Op::SetSlot(this), sec.line);
            for s in &sec.body {
                self.compile_stmt(&mut sc, s)?;
            }
            // A constructor evaluates to the instance it built.
            self.b.emit(Op::GetSlot(this), sec.line);
            Ok(())
        })();
        self.boxed_vars = outer_boxed;
        self.cur_class = None;
        res?;
        self.b.emit(Op::ReturnValue, sec.line);
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
        self.emit_member_registry("toString", KT_TOSTRING_REG);
    }

    /// Publish each overriding class's `equals(Any?)` and `hashCode()` to the
    /// runtime, the same way [`Self::emit_tostring_registry`] publishes
    /// `toString`.
    ///
    /// These two are what `==` and every hash-based container consult, and both
    /// are silently wrong without the registry: `==` would run the built-in
    /// structural compare over a class whose author said otherwise, and a `Set`
    /// would fold an identity hash where the user supplied one.
    fn emit_equality_registry(&mut self) {
        for name in ["equals", "hashCode"] {
            if self.method_index.get(name).map_or(true, |t| t.is_empty()) {
                continue;
            }
            let op = if name == "equals" {
                KT_EQUALS_REG
            } else {
                KT_HASH_REG
            };
            self.emit_member_registry(name, op);
        }
    }

    /// Emit one `tag → subroutine` publication per class that declares `name`.
    fn emit_member_registry(&mut self, name: &str, op: u16) {
        for (tag, owner) in self.method_index[name].clone() {
            let t = self.b.add_constant(Value::str(tag));
            self.b.emit(Op::LoadConst(t), 0);
            let sub = self.b.add_name(&method_sub_name(&owner, name));
            self.b.emit(Op::LoadInt(sub as i64), 0);
            self.b.emit(Op::Extended(op, 0), 0);
        }
    }

    /// Publish every `enum` class tag, so the runtime can give its constants the
    /// `toString` and the ordering an enum has (see [`KT_ENUM_REG`]).
    fn emit_enum_registry(&mut self, program: &Program) {
        for cd in program.classes.iter().filter(|c| c.is_enum) {
            let t = self.b.add_constant(Value::str(cd.name.clone()));
            self.b.emit(Op::LoadConst(t), cd.line);
            self.b.emit(Op::Extended(KT_ENUM_REG, 0), cd.line);
        }
    }

    /// Lower a free function (`class` = `None`) or a class method (`class` =
    /// `Some(name)`, adding an implicit `this` in slot 0).
    fn compile_fun(&mut self, f: &FunDecl, class: Option<&str>) -> Result<(), String> {
        let entry = self.b.current_pos();
        let sub_name = match (class, &f.recv) {
            (Some(cls), _) => method_sub_name(cls, &f.name),
            (None, Some((recv, _, _))) => ext_sub_name(recv, &f.name),
            (None, None) => f.name.clone(),
        };
        let name_idx = self.b.add_name(&sub_name);
        self.b.add_sub_entry(name_idx, entry);

        let mut sc = Scope::new();
        let mut nslots = f.params.len();
        // A method receives `this` (the instance handle) as arg 0; an extension
        // receives its receiver there under the same name, which is what makes
        // `this` — and, for a user-class receiver, a bare property name — read
        // inside the body exactly as it does inside a method.
        if let Some(cls) = class {
            sc.declare_obj("this", Type::Obj, false, Some(cls.to_string()));
            nslots += 1;
        } else if let Some((_, ty, cls)) = &f.recv {
            sc.declare_obj("this", *ty, false, cls.clone());
            nslots += 1;
        }
        // Parameters occupy the following slots in declaration order. Kotlin
        // function parameters are read-only (`val`), so declared immutable.
        for p in &f.params {
            sc.declare_obj(&p.name, p.ty, false, p.class.clone());
            // `fun f(b: Box<Int>)` — the annotation is the only place the width
            // of a read through `b` is written down; the caller's construction
            // site is on the other side of the call.
            if !p.type_args.is_empty() {
                sc.set_type_args(&p.name, p.type_args.clone());
            }
        }
        // Bind args (stack top = last arg) into slots, deepest last.
        for i in (0..nslots).rev() {
            self.b.emit(Op::SetSlot(i as u16), f.line);
        }

        // An extension on a user class puts that class's members in implicit
        // scope too — `fun Person.loud() = name.uppercase()` reads `this.name`.
        self.cur_class = class.map(|s| s.to_string()).or_else(|| {
            f.recv
                .as_ref()
                .and_then(|(_, _, c)| c.clone())
                .filter(|c| self.classes.contains_key(c))
        });
        // The frame is its own unwind boundary: an exception with no handler
        // inside it leaves the frame, and the caller's check resumes the walk.
        // A `return` likewise belongs to this frame, so an enclosing `try`'s
        // return path (from a lambda's definition site) must not capture it.
        self.push_unwind(UnwindKind::Frame);
        let outer_returns = std::mem::take(&mut self.finally_returns);
        let outer_exits = std::mem::take(&mut self.finally_exits);
        let outer_boxed = std::mem::replace(&mut self.boxed_vars, lambda_writes(&f.body));
        // Local `fun` names belong to the body being lowered. The tables are
        // snapshotted rather than cleared, because a queued local `fun`'s own
        // body is compiled with the environment its declaration saw — which is
        // what the drain loop installs before calling here.
        let outer_locals = self.local_funs.clone();
        let outer_local_sigs = self.local_sigs.clone();
        let res: Result<(), String> = (|| {
            for s in &f.body {
                self.compile_stmt(&mut sc, s)?;
            }
            Ok(())
        })();
        self.local_funs = outer_locals;
        self.local_sigs = outer_local_sigs;
        self.boxed_vars = outer_boxed;
        self.cur_class = None;
        self.finally_returns = outer_returns;
        self.finally_exits = outer_exits;
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
            // A local `fun` emits no code here: it is registered under a unique
            // sub name and queued for emission after the enclosing body, so a
            // call to it — including a call from its own body — is an ordinary
            // direct `Op::Call`. Registration happens at the declaration's
            // position, which is where Kotlin makes the name visible.
            StmtKind::LocalFun(lf) => {
                if lf.recv.is_some() {
                    return Err(format!(
                        "local fun {}: an extension receiver is only supported at the top level",
                        lf.name
                    ));
                }
                let id = self.local_funs_seen;
                self.local_funs_seen += 1;
                let sub = format!("{}$local${id}", lf.name);
                self.local_funs.insert(lf.name.clone(), sub.clone());
                self.local_sigs.insert(lf.name.clone(), FnSig::of(lf));
                let mut decl = lf.clone();
                decl.name = sub;
                self.pending_local_funs.push(PendingLocalFun {
                    decl,
                    local_funs: self.local_funs.clone(),
                    local_sigs: self.local_sigs.clone(),
                });
            }
            StmtKind::Let {
                name,
                ty,
                fn_params,
                fn_ret,
                type_args: annotated,
                init,
                mutable,
                lazy,
            } => {
                let class = self.infer_class(sc, init);
                // Read the initializer's element type BEFORE lowering, while
                // the initializer expression is still in hand: it is what a
                // later `xs.map { … }` types its parameter from.
                let elem = self.infer_elem(sc, init);
                // Likewise the TYPE ARGUMENTS, when the initializer constructs a
                // generic class: `val b = Box(65536)` binds `T` to `Int` for
                // every read of a `T`-typed member through `b`.
                //
                // A WRITTEN annotation wins: `val b: Box<Int> = mk()` states the
                // static type outright, and the initializer need not reveal it
                // (a call through an opaque `fun mk(): Box<Int>` does not).
                let type_args = if annotated.is_empty() {
                    self.gen_ty(sc, init).args
                } else {
                    annotated.clone()
                };
                // `val f: (Int) -> Int = { it * 2 }` — the annotation IS the
                // lambda's parameter list, so publish it the same way a
                // collection HOF publishes its element type.
                if !fn_params.is_empty() {
                    self.lambda_hint =
                        Some(fn_params.iter().map(|t| (*t, Type::Unknown)).collect());
                }
                let it = self.compile_expr(sc, init)?;
                let mut vty = ty.unwrap_or(it);
                // A binding with a known class/container is a heap object.
                if class.is_some() {
                    vty = Type::Obj;
                }
                self.lambda_hint = None;
                // A `var` that a lambda in this frame writes to is stored in a
                // one-element heap cell instead of directly in the slot, so the
                // closure's captured copy of the HANDLE still reaches the same
                // storage (see [`Binding::boxed`]).
                let boxed = *mutable && self.boxed_vars.contains(name);
                if boxed {
                    self.b.emit(Op::Extended(KT_LIST, 1), 0);
                }
                // `val x by lazy { … }`: the slot holds the unforced CELL, not
                // the value, so the thunk on the stack is wrapped rather than
                // called. Every read of the binding forces it below.
                if *lazy {
                    self.b.emit(Op::Extended(KT_LAZY_NEW, 0), 0);
                    vty = Type::Unknown;
                }
                let slot = sc.declare_full(name, vty, *mutable, class, elem);
                if !type_args.is_empty() {
                    sc.set_type_args(name, type_args);
                }
                if *lazy {
                    sc.mark_lazy(name);
                }
                if let Some(r) = fn_ret {
                    sc.set_fn_ret(name, *r);
                }
                if boxed {
                    sc.box_binding(name);
                }
                // A raise inside the initializer must not commit its garbage
                // result to the new binding.
                self.unwind_check_dropping(1);
                self.b.emit(Op::SetSlot(slot), 0);
            }
            StmtKind::Assign { name, op, value } => {
                // A `val` (write-once) binding cannot be reassigned — Kotlin
                // reports this at compile time.
                if sc.is_mutable(name) == Some(false) {
                    // …but `val c = mutableListOf(1); c += 2` is not a
                    // reassignment. It is the `plusAssign` convention, which
                    // MUTATES the object the name is bound to and leaves the
                    // binding itself alone (see `operator_assign_fn`).
                    if let Some(fname) = op.and_then(operator_assign_fn) {
                        let recv = Expr::Var(name.clone());
                        if self.declares_operator(sc, &recv, fname) {
                            self.compile_member(
                                sc,
                                &recv,
                                fname,
                                std::slice::from_ref(value),
                                false,
                                0,
                            )?;
                            self.b.emit(Op::Pop, 0);
                            return Ok(());
                        }
                        if self.infer(sc, &recv) == Type::Obj {
                            self.emit_operator_call(sc, &recv, value, fname)?;
                            self.b.emit(Op::Pop, 0);
                            return Ok(());
                        }
                    }
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
                // A top-level `var`.
                if sc.slot(name).is_none() {
                    if let Some(p) = self.globals.get(name).cloned() {
                        if !p.mutable {
                            return Err(format!("val cannot be reassigned: {name}"));
                        }
                        let full = match op {
                            None => value.clone(),
                            Some(binop) => Expr::Binary {
                                op: *binop,
                                l: Box::new(Expr::Var(name.clone())),
                                r: Box::new(value.clone()),
                            },
                        };
                        self.compile_expr(sc, &full)?;
                        let g = self.b.add_name(name);
                        self.b.emit(Op::SetVar(g), 0);
                        return Ok(());
                    }
                }
                let slot = sc
                    .slot(name)
                    .ok_or_else(|| format!("unresolved reference: {name}"))?;
                // A reassignment that does not agree with the recorded type
                // arguments drops them. Well-typed Kotlin fixes a `var`'s type
                // arguments at its declaration and cannot change them, so this
                // fires only where the frontend resolved one side and not the
                // other — and an unresolved argument is the conservative answer
                // (it narrows nothing) where a stale one would not be.
                let assigned = sc.type_args_of(name);
                if !assigned.is_empty() && self.gen_ty(sc, value).args != assigned {
                    sc.set_type_args(name, Vec::new());
                }
                // A boxed `var` is written through its cell: the slot itself
                // holds the handle and never changes.
                if sc.is_boxed(name) {
                    let full = match op {
                        None => value.clone(),
                        Some(binop) => Expr::Binary {
                            op: *binop,
                            l: Box::new(Expr::Var(name.clone())),
                            r: Box::new(value.clone()),
                        },
                    };
                    self.b.emit(Op::GetSlot(slot), 0);
                    self.b.emit(Op::LoadInt(0), 0);
                    self.compile_expr(sc, &full)?;
                    self.b.emit(Op::CallBuiltin(KT_INDEX_SET_VM, 3), 0);
                    self.b.emit(Op::Pop, 0);
                    return Ok(());
                }
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
            // `do { … } while (cond)` — the body first, then the test jumping
            // back. `continue` targets the *test*, not the loop top, so the
            // condition still runs on the iteration it skipped out of.
            StmtKind::DoWhile { cond, body, label } => {
                let start = self.b.current_pos();
                self.loops.push(LoopCtx {
                    label: label.clone(),
                    breaks: Vec::new(),
                    continues: Vec::new(),
                });
                self.push_unwind(UnwindKind::Loop);
                let mark = sc.enter();
                for s in body {
                    self.compile_stmt(sc, s)?;
                }
                sc.exit(mark);
                let ctx = self.loops.pop().unwrap();
                let test = self.b.current_pos();
                for j in &ctx.continues {
                    self.b.patch_jump(*j, test);
                }
                self.compile_expr(sc, cond)?;
                let jf = self.b.emit(Op::JumpIfFalse(0), 0);
                // The repeat path. A raise while evaluating the condition would
                // otherwise spin here forever, so the check goes between the
                // test and the back-jump; a false condition exits to `end`,
                // where the statement's own check carries the raise outward.
                self.unwind_check();
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
                parts,
                iter,
                body,
                label,
            } => {
                self.compile_for_in(sc, var, parts, iter, body, label)?;
            }
            StmtKind::Break(label) => {
                let j = self.b.emit(Op::Jump(0), 0);
                self.route_loop_exit(j, true, label, s.line)?;
            }
            StmtKind::Continue(label) => {
                let j = self.b.emit(Op::Jump(0), 0);
                self.route_loop_exit(j, false, label, s.line)?;
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
    /// The index in `self.loops` of the loop a `break`/`continue` targets: the
    /// nearest one carrying `label`, or the innermost when unlabeled.
    fn loop_index(&self, label: &Option<String>, line: u32) -> Result<usize, String> {
        match label {
            Some(l) => self
                .loops
                .iter()
                .rposition(|c| c.label.as_deref() == Some(l.as_str()))
                .ok_or_else(|| format!("unresolved label: {l} (line {line})")),
            None => self
                .loops
                .len()
                .checked_sub(1)
                .ok_or_else(|| format!("break/continue outside a loop (line {line})")),
        }
    }

    /// Send an emitted `break`/`continue` jump to its destination.
    ///
    /// Normally that is the target loop's own jump list. But when the exit
    /// leaves a `try` that owns a `finally`, the finalizer must run first, so
    /// the jump is parked on that `try` instead and [`Compiler::compile_try`]
    /// re-dispatches it after emitting a copy of the finalizer. The test is
    /// positional: a target loop opened BEFORE the `try` is outside it.
    fn route_loop_exit(
        &mut self,
        jump: usize,
        is_break: bool,
        label: &Option<String>,
        line: u32,
    ) -> Result<(), String> {
        let target = self.loop_index(label, line)?;
        if let Some(f) = self.finally_exits.last_mut() {
            if target < f.loops_at_entry {
                f.jumps.push((jump, is_break, label.clone()));
                return Ok(());
            }
        }
        let ctx = &mut self.loops[target];
        if is_break {
            ctx.breaks.push(jump);
        } else {
            ctx.continues.push(jump);
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
        parts: &[String],
        iter: &Expr,
        body: &[Stmt],
        label: &Option<String>,
    ) -> Result<(), String> {
        let mark = sc.enter();
        // Iterating a `String` yields `Char`s. The runtime element is an integer
        // code unit like every other kotlinrs `Char`, so the element type has to
        // be decided here — otherwise `println(c)` would print the code, not the
        // character.
        // Every other iterable takes its element type from the receiver, so a
        // `for` variable over `listOf(1, 2)` is an `Int` and arithmetic on it
        // narrows to 32 bits like Kotlin's.
        let elem_ty = if self.infer(sc, iter).is_str() {
            Type::Char
        } else {
            self.infer_elem(sc, iter)
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
        // Iterating a list OF lists keeps the inner element type, so a nested
        // `for` types its own variable too.
        let inner = self.infer_elem(sc, iter);
        // The element's CLASS where the iterable names it, so a property read on
        // the loop variable resolves to its declared type.
        let elem_cls = self.infer_elem_class(sc, iter);
        let elem_ty = if elem_ty == Type::Unknown && elem_cls.is_some() {
            Type::Obj
        } else {
            elem_ty
        };
        let vslot = sc.declare_full(
            var,
            elem_ty,
            false,
            elem_cls,
            if elem_ty == Type::Unknown {
                inner
            } else {
                Type::Unknown
            },
        );

        let top = self.b.current_pos();
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::GetSlot(nslot), 0);
        self.b.emit(Op::NumLt, 0);
        let jf = self.b.emit(Op::JumpIfFalse(0), 0);
        self.b.emit(Op::GetSlot(cslot), 0);
        self.b.emit(Op::GetSlot(islot), 0);
        self.b.emit(Op::Extended(KT_ITER_GET, 0), 0);
        self.b.emit(Op::SetSlot(vslot), 0);
        // `for ((k, v) in map)` — split the element into its components. This
        // runs per iteration, so the names are declared once (outside the loop
        // body's own scope) and only re-stored here.
        let part_slots: Vec<u16> = parts
            .iter()
            .map(|nm| sc.declare(nm, Type::Unknown, false))
            .collect();
        for (i, (nm, slot)) in parts.iter().zip(&part_slots).enumerate() {
            if nm == "_" {
                continue; // `_` discards the component
            }
            self.b.emit(Op::GetSlot(vslot), 0);
            let cidx = self
                .b
                .add_constant(Value::str(format!("component{}", i + 1)));
            self.b.emit(Op::LoadConst(cidx), 0);
            self.b.emit(Op::CallBuiltin(KT_METHOD_VM, 0), 0);
            self.b.emit(Op::SetSlot(*slot), 0);
        }

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
            // A named argument is bound by the callee (see [`bind_args`]).
            // Reaching here means it was written where no parameter names are
            // known — a stdlib member or a lambda invocation.
            Expr::Named { name, .. } => Err(format!(
                "named argument `{name}` is not supported for this callee"
            )),
            Expr::Int(n) => {
                self.b.emit(Op::LoadInt(*n), 0);
                Ok(Type::Int)
            }
            Expr::Long(n) => {
                self.b.emit(Op::LoadInt(*n), 0);
                Ok(Type::Long)
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
                    // A boxed `var` holds a one-element cell; the value is
                    // element 0 of it.
                    if sc.is_boxed(name) {
                        self.b.emit(Op::LoadInt(0), 0);
                        self.b.emit(Op::CallBuiltin(KT_INDEX_GET_VM, 2), 0);
                    }
                    // A `by lazy` local holds a cell too, and reading it is what
                    // runs the thunk — the first read only.
                    if sc.is_lazy(name) {
                        self.b.emit(Op::CallBuiltin(KT_LAZY_GET, 0), 0);
                    }
                    return Ok(sc.ty(name));
                }
                // Implicit `this`: a bare name that is a property of the class
                // whose method we're lowering resolves to `this.name`.
                if let Some(cls) = self.cur_class.clone() {
                    if self.classes.get(&cls).is_some_and(|m| {
                        m.prop(name).is_some() || m.methods.get(name).is_some_and(|s| s.arity == 0)
                    }) {
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
                // A companion property named without a qualifier from inside the
                // owning class — `companion object { val K = 7 }` makes `K`
                // visible to every member.
                if let Some(comp) = self
                    .cur_class
                    .clone()
                    .and_then(|c| self.companion_of(&c))
                    .filter(|c| self.classes[c].prop(name).is_some())
                {
                    return self.compile_member(sc, &Expr::Var(comp), name, &[], false, 0);
                }
                // A top-level property. Declared after the local lookup, so a
                // local of the same name shadows it as Kotlin's does.
                if let Some(p) = self.globals.get(name).cloned() {
                    let g = self.b.add_name(name);
                    self.b.emit(Op::GetVar(g), 0);
                    if p.lazy {
                        self.b.emit(Op::CallBuiltin(KT_LAZY_GET, 0), 0);
                    }
                    return Ok(p.ty);
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
                // Inside a receiver scope whose receiver is NOT a user class — a
                // `String`/`List`/range block from `run`/`apply`/`with`, or an
                // extension on one — a bare name is a member of that receiver:
                // `"abc".run { length }`. Restricted to that case so an
                // unresolved name inside a class method keeps its compile-time
                // diagnostic, where the member set IS statically known.
                if sc.slot("this").is_some() && self.cur_class.is_none() {
                    return self.compile_member(sc, &Expr::Var("this".into()), name, &[], false, 0);
                }
                Err(format!("unresolved reference: {name}"))
            }
            Expr::Unary { op, expr } => {
                // `-x` is the `unaryMinus` convention and `!x` the `not` one, so
                // a class declaring either gets its own method rather than the
                // numeric/boolean instruction.
                let fname = match op {
                    UnOp::Neg => "unaryMinus",
                    UnOp::Not => "not",
                };
                if self.declares_operator(sc, expr, fname) {
                    return self.compile_member(sc, expr, fname, &[], false, 0);
                }
                let t = self.compile_expr(sc, expr)?;
                match op {
                    UnOp::Neg => {
                        self.b.emit(Op::Negate, 0);
                        let ty = match t {
                            Type::Double => Type::Double,
                            Type::Long => Type::Long,
                            _ => Type::Int,
                        };
                        // `-Int.MIN_VALUE` is `Int.MIN_VALUE`: negation is the
                        // one unary operator that can leave the `Int` range.
                        if ty == Type::Int && is_int_width(t) {
                            self.emit_wrap32();
                        }
                        Ok(ty)
                    }
                    UnOp::Not => {
                        self.b.emit(Op::LogNot, 0);
                        Ok(Type::Boolean)
                    }
                }
            }
            Expr::Binary { op, l, r } => self.compile_binary(sc, *op, l, r),
            Expr::Call { name, args, line } => self.compile_call(sc, name, args, *line),
            Expr::Invoke { target, args, line } => self.compile_invoke(sc, target, args, *line),
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
                // `a[i]` is the `get` convention.
                if self.declares_operator(sc, recv, "get") {
                    return self.compile_member(
                        sc,
                        recv,
                        "get",
                        std::slice::from_ref(index),
                        false,
                        *line,
                    );
                }
                let ty = self.index_elem_ty(sc, recv);
                self.compile_expr(sc, recv)?;
                self.compile_expr(sc, index)?;
                self.b.emit(Op::CallBuiltin(KT_INDEX_GET_VM, 2), *line);
                Ok(ty)
            }
            Expr::Pair { first, second } => {
                self.compile_expr(sc, first)?;
                self.compile_expr(sc, second)?;
                self.b.emit(Op::Extended(KT_PAIR, 0), 0);
                Ok(Type::Obj)
            }
            Expr::Range { start, end, kind } => {
                // `a..b` is the `rangeTo` convention. Only the `..` form is one:
                // `until`/`downTo` are ordinary infix functions, so a class
                // wanting those declares them by name and reaches them as
                // methods.
                if *kind == RangeKind::Inclusive && self.declares_operator(sc, start, "rangeTo") {
                    return self.compile_member(
                        sc,
                        start,
                        "rangeTo",
                        std::slice::from_ref(end),
                        false,
                        0,
                    );
                }
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
                // `a in b` is the `contains` convention on the CONTAINER, with
                // the operands the other way round: it is `b.contains(a)`.
                if self.declares_operator(sc, container, "contains") {
                    self.compile_member(
                        sc,
                        container,
                        "contains",
                        std::slice::from_ref(value),
                        false,
                        0,
                    )?;
                    if *negated {
                        self.b.emit(Op::LogNot, 0);
                    }
                    return Ok(Type::Boolean);
                }
                self.compile_expr(sc, value)?;
                self.compile_expr(sc, container)?;
                self.b.emit(Op::CallBuiltin(KT_IN_VM, 2), 0);
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
            // `x as T` — the value passes through unchanged; what the cast
            // supplies is the STATIC type `T`, which is what decides integer
            // width and `/` dispatch from here on. The runtime op only enforces
            // the check (throw for `as`, null for `as?`).
            Expr::As {
                value, ty, safe, ..
            } => {
                self.compile_expr(sc, value)?;
                let nidx = self.b.add_constant(Value::str(ty.clone()));
                self.b.emit(Op::LoadConst(nidx), 0);
                self.b.emit(Op::Extended(KT_AS, u8::from(*safe)), 0);
                Ok(cast_type(ty, *safe))
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
        // A `return`, `break` or `continue` that leaves a `try` owning a
        // `finally` all route the same way: park the jump, run the finalizer on
        // a dedicated copy, then resume the exit. See the return path and the
        // loop-exit path at the end of this function.
        let has_finally = !t.finally_body.is_empty();
        let mark = sc.enter();
        let res = sc.temp();
        let depth = sc.temp();
        if has_finally {
            let slot = sc.temp();
            self.finally_returns.push(FinallyReturn {
                slot,
                jumps: Vec::new(),
            });
            self.finally_exits.push(FinallyExit {
                loops_at_entry: self.loops.len(),
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
        let (pending_ret, pending_exit) = if has_finally {
            self.emit_finally(sc, &t.finally_body)?;
            // Both frames pop BEFORE their paths are emitted, so a parked exit
            // that is itself inside an enclosing `finally` re-parks there rather
            // than on this one.
            (self.finally_returns.pop(), self.finally_exits.pop())
        } else {
            (None, None)
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

        // ── loop-exit path ──
        // A `break`/`continue` whose target loop is outside this `try` parked
        // its jump; the finalizer runs here and the exit then resumes. Exits
        // sharing a target share one copy of the finalizer, so the common case
        // (a single `break`) emits it exactly once.
        if let Some(exit) = pending_exit.filter(|e| !e.jumps.is_empty()) {
            let over = self.b.emit(Op::Jump(0), 0);
            let mut groups: Vec<(bool, Option<String>, Vec<usize>)> = Vec::new();
            for (j, is_break, label) in exit.jumps {
                match groups
                    .iter_mut()
                    .find(|(b, l, _)| *b == is_break && *l == label)
                {
                    Some(g) => g.2.push(j),
                    None => groups.push((is_break, label, vec![j])),
                }
            }
            for (is_break, label, jumps) in groups {
                let path = self.b.current_pos();
                for j in jumps {
                    self.b.patch_jump(j, path);
                }
                self.emit_finally(sc, &t.finally_body)?;
                let resume = self.b.emit(Op::Jump(0), 0);
                self.route_loop_exit(resume, is_break, &label, t.line)?;
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
        // `x++` is the `inc` convention, NOT `x = x + 1`: a class declaring
        // `operator fun inc()` is stepped by that method, and it need not agree
        // with any `plus` the class also declares.
        let step = if inc { "inc" } else { "dec" };
        let conv = self
            .declares_operator(sc, target, step)
            .then(|| Expr::MethodCall {
                recv: Box::new(target.clone()),
                name: step.to_string(),
                args: Vec::new(),
                safe: false,
                line: 0,
            });
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
        // With an `inc`/`dec` convention the update is a plain `=` of the
        // method's result; without one it is the `+= 1` the primitive types use.
        let (upd_op, upd_value) = match conv {
            Some(call) => (None, call),
            None => (Some(op), Expr::Int(1)),
        };
        let update = match target {
            Expr::Var(name) => StmtKind::Assign {
                name: name.clone(),
                op: upd_op,
                value: upd_value,
            },
            Expr::Member {
                recv,
                name,
                safe: false,
                ..
            } => StmtKind::SetMember {
                recv: (**recv).clone(),
                name: name.clone(),
                op: upd_op,
                value: upd_value,
            },
            Expr::Index { recv, index, .. } => StmtKind::SetIndex {
                recv: (**recv).clone(),
                index: (**index).clone(),
                op: upd_op,
                value: upd_value,
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
        // A named argument on a STDLIB member. A user function binds its names
        // in `bind_args` from the declaration; a builtin has no declaration
        // here, so the parameter list comes from [`builtin_params`] and the
        // arguments are rewritten into positional order before any routing
        // below sees them.
        //
        // Only when no USER declaration claims the call: a `data class`'s
        // generated `copy(a = 8)` and a declared method both bind their names
        // from the declaration in `bind_args`, and rewriting those here against
        // a stdlib parameter list would reject them outright.
        let bound;
        let args = if args.iter().any(|a| matches!(a, Expr::Named { .. }))
            && builtin_params(name).is_some()
            && !self
                .infer_class(sc, recv)
                .and_then(|c| self.classes.get(&c).cloned())
                .is_some_and(|m| m.methods.contains_key(name) || name == "copy")
        {
            bound = bind_named_builtin(name, args)?;
            &bound[..]
        } else {
            args
        };
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
        // A fully-qualified reference, written out instead of imported. Kotlin
        // resolves `kotlin.math.floor(x)` with no `import` line at all, and the
        // two `java.lang` statics below are auto-imported on the JVM the same
        // way `Math` is.
        if let Some(path) = self.qualifier(sc, recv) {
            match path.as_str() {
                "kotlin.math" => {
                    return match name {
                        "PI" | "E" => self.compile_math_const(name, line),
                        _ if is_math_fn(name) => self.compile_math(sc, name, args, line),
                        _ => Err(format!("unresolved reference: kotlin.math.{name}")),
                    }
                }
                // `String.format(fmt, args…)` — the static spelling of the
                // `fmt.format(args…)` member, with the receiver moved into the
                // first argument position.
                "String" if name == "format" && !args.is_empty() => {
                    return self.compile_member(sc, &args[0], "format", &args[1..], false, line)
                }
                // `Integer.parseInt` / `Integer.valueOf` are `String.toInt()`
                // under another name — same parse, same NumberFormatException.
                "Integer" | "java.lang.Integer"
                    if matches!(name, "parseInt" | "valueOf") && args.len() == 1 =>
                {
                    return self.compile_member(sc, &args[0], "toInt", &[], false, line)
                }
                _ => {}
            }
        }
        if self.is_java_math(sc, recv) {
            match name {
                "PI" | "E" => return self.compile_math_const(name, line),
                "round" => return self.compile_math(sc, "jround", args, line),
                _ if is_math_fn(name) => return self.compile_math(sc, name, args, line),
                _ => return Err(format!("unresolved reference: Math.{name}")),
            }
        }
        // `Owner.member` where `Owner` names a class with a `companion object`:
        // the companion singleton is the real receiver. A rewrite rather than a
        // dedicated path, so property reads and method calls both reach it
        // through what a named `object` already uses.
        if let Expr::Var(cls) = recv {
            if let Some(comp) = self.companion_of(cls) {
                return self.compile_member(sc, &Expr::Var(comp), name, args, false, line);
            }
        }
        // A property of the `object` whose initializers are being lowered right
        // now, named through the qualified form. Its global is not published
        // until every initializer has run (see [`Compiler::building_object`]),
        // so the already-computed slot is the only place the value exists yet.
        if args.is_empty() {
            if let Expr::Var(obj) = recv {
                if self.building_object.as_deref() == Some(obj.as_str()) {
                    if let Some(slot) = sc.slot(name) {
                        self.b.emit(Op::GetSlot(slot), line);
                        return Ok(sc.ty(name));
                    }
                }
            }
        }
        // A companion constant on a primitive type (`Int.MAX_VALUE`,
        // `Double.NaN`). These are compile-time literals, so they fold here
        // rather than paying a host dispatch — and the receiver is a *type*
        // name, which has no runtime value to dispatch on at all.
        if args.is_empty() {
            if let Expr::Var(ty) = recv {
                if self.is_type_ref(sc, ty) {
                    if let Some((v, vty)) = primitive_const(ty, name) {
                        match v {
                            Value::Int(n) => self.b.emit(Op::LoadInt(n), line),
                            Value::Float(f) => self.b.emit(Op::LoadFloat(f), line),
                            _ => unreachable!("primitive_const yields only Int/Float"),
                        };
                        return Ok(vty);
                    }
                }
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
            let own_override = self.infer_class(sc, recv).is_some_and(|c| {
                self.classes
                    .get(&c)
                    .is_some_and(|m| m.methods.contains_key("toString"))
            });
            if name == "toString" && args.is_empty() && !own_override {
                self.compile_expr(sc, recv)?;
                self.b.emit(Op::CallBuiltin(KT_DISPLAY, 1), line);
                return Ok(Type::String);
            }
            // Only `joinToString()` / `joinToString(sep)` take this display
            // fast path. Every longer form carries an affix, a limit, or a
            // transform, and has to reach the full member (or the HOF) below.
            if name == "joinToString" && args.len() <= 1 && !args.iter().any(is_lambda) {
                self.compile_expr(sc, recv)?;
                for a in args {
                    let t = self.compile_expr(sc, a)?;
                    self.emit_display(t);
                }
                self.b
                    .emit(Op::CallBuiltin(KT_JOIN, args.len() as u8), line);
                return Ok(Type::String);
            }
        }
        // An extension function on this receiver's type. Kotlin resolves a
        // MEMBER of the same name first — an extension can never shadow one — so
        // a receiver whose static class declares the name at this arity skips
        // this arm and dispatches virtually below.
        let member_wins = self.infer_class(sc, recv).is_some_and(|c| {
            self.classes
                .get(&c)
                .and_then(|m| m.methods.get(name))
                .is_some_and(|s| s.arity == args.len())
        });
        if !member_wins {
            if let Some((sub, sig)) = self.resolve_ext(sc, recv, name) {
                let full = self.expand_args(name, &sig.params, args)?;
                self.compile_expr(sc, recv)?;
                for a in &full {
                    self.compile_expr(sc, a)?;
                }
                let idx = self.b.add_name(&sub);
                self.b.emit(Op::Call(idx, (full.len() + 1) as u8), line);
                return Ok(sig.ret);
            }
        }
        // The lambda-taking `Result` members. Routed before the collection HOFs
        // because `map`/`getOrElse` are spelled the same there and mean
        // something else — the receiver's static class is what tells them apart.
        if matches!(name, "getOrElse" | "onSuccess" | "onFailure" | "map")
            && args.len() == 1
            && self.infer_class(sc, recv).as_deref() == Some("Result")
        {
            self.compile_expr(sc, recv)?;
            self.compile_expr(sc, &args[0])?;
            let nidx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(nidx), line);
            self.b.emit(Op::CallBuiltin(KT_RESULT_HOF, 0), line);
            return Ok(Type::Unknown);
        }
        // Collection higher-order functions take a first-class lambda VALUE (a
        // trailing-lambda literal or a passed closure) and invoke it per element
        // at runtime via the `KT_COLL_HOF` builtin.
        if is_coll_hof(name) && !args.is_empty() {
            return self.compile_coll_hof(sc, recv, name, args, line);
        }
        // The overloaded forms route by whether a lambda was actually written.
        if is_optional_hof(name) && args.last().is_some_and(is_lambda) {
            return self.compile_coll_hof(sc, recv, name, args, line);
        }
        // `cmp.thenBy { … }` / `thenByDescending` extend a comparator with a
        // tiebreak key. The receiver is a `Comparator`, not a collection, so
        // this must precede nothing in particular — but it must not fall into
        // the member dispatch, which would look for the name on a heap kind
        // that has no members at all.
        if matches!(name, "thenBy" | "thenByDescending") && args.len() == 1 && is_lambda(&args[0]) {
            self.compile_expr(sc, recv)?;
            self.lambda_hint = Some(vec![(Type::Unknown, Type::Unknown)]);
            self.compile_expr(sc, &args[0])?;
            self.lambda_hint = None;
            let nidx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(nidx), line);
            self.b.emit(Op::CallBuiltin(KT_COMPARATOR, 1), line);
            return Ok(Type::Obj);
        }
        // Scope functions on any receiver. They split two ways: `let`/`also`/
        // `takeIf`/`takeUnless` hand the receiver to the block as the parameter
        // `it`, while `run`/`apply` bind it as the block's `this` — which is
        // what makes `"abc".run { length }` read a member with no qualifier.
        // The two are one lowering: both invoke a one-parameter closure with the
        // receiver, and only the parameter's NAME differs.
        if is_scope_fn(name) && args.len() == 1 {
            let rty = self.infer(sc, recv);
            let relem = self.infer_elem(sc, recv);
            let rcls = self.infer_class(sc, recv);
            self.compile_expr(sc, recv)?;
            if is_recv_scope_fn(name) {
                self.lambda_recv = Some((rty, rcls));
            } else {
                self.lambda_hint = Some(vec![(rty, relem)]);
            }
            self.compile_expr(sc, &args[0])?; // the lambda → closure value
            self.lambda_recv = None;
            self.lambda_hint = None;
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
                if let Some(sig) = meta.methods.get(name).cloned() {
                    let full =
                        self.expand_args(&format!("method {name} on {cls}"), &sig.params, args)?;
                    let cands = self.candidates(Some(cls), name, sig.arity);
                    if !cands.is_empty() {
                        self.emit_virtual_call(
                            sc,
                            recv,
                            name,
                            &full,
                            Targets::statik(&cands),
                            line,
                        )?;
                        // The result's width has to be the one `Compiler::infer`
                        // reports for the same node, or a type-variable result
                        // would narrow on one path and not the other.
                        return Ok(self.method_ret(sc, recv, &sig, args));
                    }
                }
                // A stored property read.
                if args.is_empty() {
                    if let Some(p) = meta.prop(name).cloned() {
                        // A delegated property has no storage: the read is the
                        // delegate's `getValue(thisRef, property)`.
                        if let Some(dc) = &p.delegate {
                            if !self.classes.contains_key(dc) {
                                return Err(format!(
                                    "property {name}: delegate {dc} is not a class declaring \
                                     `operator fun getValue`"
                                ));
                            }
                            self.emit_delegate_head(sc, recv, name, line)?;
                            let idx = self.b.add_name(&method_sub_name(dc, "getValue"));
                            self.b.emit(Op::Call(idx, 3), line);
                            return Ok(p.ty);
                        }
                        // Resolved before the receiver is lowered, so the answer
                        // is read off the scope the read is written against —
                        // and it must equal what `Compiler::infer` reports for
                        // this node, since that is what decides whether the
                        // enclosing arithmetic wraps at 32 bits.
                        let ty = match p.type_param_of {
                            Some(k) => self.type_arg_at(sc, recv, k).ty,
                            None => p.ty,
                        };
                        self.compile_expr(sc, recv)?;
                        let nidx = self.b.add_constant(Value::str(name.to_string()));
                        self.b.emit(Op::LoadConst(nidx), line);
                        self.b.emit(Op::Extended(KT_GETFIELD, 0), line);
                        // A `by lazy` property stores a cell; reading it is what
                        // runs the thunk, the first time.
                        if p.lazy {
                            self.b.emit(Op::CallBuiltin(KT_LAZY_GET, 0), line);
                        }
                        return Ok(ty);
                    }
                }
                // A property that HOLDS a function value, called through the
                // receiver: `box.f(5)`. The class declares no method `f`, so
                // this is a field read followed by an invocation — without it
                // the property read above would silently swallow the arguments
                // and yield the closure itself.
                if !args.is_empty() && !meta.methods.contains_key(name) {
                    if let Some(p) = meta.prop(name).cloned() {
                        self.compile_expr(sc, recv)?;
                        let nidx = self.b.add_constant(Value::str(name.to_string()));
                        self.b.emit(Op::LoadConst(nidx), line);
                        self.b.emit(Op::Extended(KT_GETFIELD, 0), line);
                        if p.lazy {
                            self.b.emit(Op::CallBuiltin(KT_LAZY_GET, 0), line);
                        }
                        for a in args {
                            self.compile_expr(sc, a)?;
                        }
                        self.b
                            .emit(Op::CallBuiltin(KT_CLOSURE_CALL, args.len() as u8), line);
                        return Ok(Type::Unknown);
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
        //
        // A receiver whose coarse type is a PRIMITIVE is excluded: no user class
        // instance is an `Int`/`Long`/`Double`/`Boolean`/`Char`/`String`, so the
        // runtime tag can only ever take the fallback arm — and taking it that
        // way LOSES the receiver's static width, which the members below push
        // along as an argument. `(-7L).hashCode()` is `6` in Kotlin (the `Long`
        // fold) and `-7` under the 32-bit one; it answered `-7` in any program
        // where some user class happened to declare `hashCode`, because that
        // made the candidate list non-empty and swallowed the call here.
        if static_cls.is_none() && !is_primitive_recv(self.infer(sc, recv)) {
            let cands = self.candidates(None, name, args.len());
            if !cands.is_empty() {
                self.emit_virtual_call(sc, recv, name, args, Targets::dynamic(&cands), line)?;
                return Ok(self.virtual_ret_type(&cands, name));
            }
        }
        // `f.invoke(args)` on a function value — the explicit spelling of
        // `f(args)`. A user class declaring its own `invoke` was already
        // dispatched above, so reaching here means the receiver is a closure.
        if name == "invoke" {
            self.compile_expr(sc, recv)?;
            for a in args {
                self.compile_expr(sc, a)?;
            }
            self.b
                .emit(Op::CallBuiltin(KT_CLOSURE_CALL, args.len() as u8), line);
            return Ok(Type::Unknown);
        }
        // The bitwise members (reached from their infix spelling, `x shl 4`).
        // `and`/`or`/`xor` cannot widen a value, so they only need a static
        // result type; the shifts and `inv` also need the RECEIVER'S WIDTH,
        // because Kotlin masks an `Int` shift count at 31 and truncates the
        // result to 32 bits where a `Long` masks at 63 and keeps all 64 (`1 shl
        // 32` is 1, `1L shl 32` is 4294967296). Every integer is one `i64` at
        // runtime, so the width cannot be recovered there — it is pushed as a
        // trailing argument and one host arm serves both.
        // `hashCode()` folds an `Int` and a `Long` differently (`(-1).hashCode()`
        // is -1, `(-1L).hashCode()` is 0) and the two share one runtime
        // representation, so the receiver's static width rides along exactly as
        // it does for the shifts below.
        if name == "hashCode" && args.is_empty() {
            let rt = self.infer(sc, recv);
            self.compile_expr(sc, recv)?;
            self.b
                .emit(Op::LoadInt(if rt == Type::Long { 64 } else { 32 }), line);
            let nidx = self.b.add_constant(Value::str(name.to_string()));
            self.b.emit(Op::LoadConst(nidx), line);
            self.b.emit(Op::CallBuiltin(KT_METHOD_VM, 1), line);
            return Ok(Type::Int);
        }
        if matches!(name, "shl" | "shr" | "ushr" | "inv" | "and" | "or" | "xor")
            && args.len() == usize::from(name != "inv")
        {
            let rt = self.infer(sc, recv);
            if matches!(rt, Type::Int | Type::Long | Type::Unknown) {
                let ty = if rt == Type::Long {
                    Type::Long
                } else {
                    Type::Int
                };
                if matches!(name, "shl" | "shr" | "ushr" | "inv") {
                    self.compile_expr(sc, recv)?;
                    for a in args {
                        self.compile_expr(sc, a)?;
                    }
                    self.b
                        .emit(Op::LoadInt(if ty == Type::Long { 64 } else { 32 }), line);
                    let nidx = self.b.add_constant(Value::str(name.to_string()));
                    self.b.emit(Op::LoadConst(nidx), line);
                    self.b
                        .emit(Op::CallBuiltin(KT_METHOD_VM, args.len() as u8 + 1), line);
                } else {
                    self.emit_kt_method(sc, recv, name, args, line)?;
                }
                return Ok(ty);
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
                Some(c) => self
                    .classes
                    .get(tag)
                    .is_some_and(|m| m.mro.iter().any(|a| a == c)),
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
        targets: Targets<'_>,
        line: u32,
    ) -> Result<(), String> {
        let Targets { cands, static_recv } = targets;
        let owners: HashSet<&str> = cands.iter().map(|(_, o)| o.as_str()).collect();
        // One owner and a receiver whose class is already known: every instance
        // reaching this site runs the same body, so the call is direct and the
        // program pays nothing for the hierarchy.
        //
        // With an UNKNOWN receiver it is not the same call. The candidate set is
        // then "every class that declares `name`", and the receiver may be none
        // of them — so a direct call sends an `Int` into a user body. One class
        // declaring `hashCode` was enough to make `(0).hashCode()` run it and
        // fail with `unresolved reference` on the class's own field, in any
        // program that declared such an override.
        if owners.len() == 1 && static_recv {
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
        self.b
            .emit(Op::CallBuiltin(KT_METHOD_VM, args.len() as u8), line);
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
        self.b
            .emit(Op::CallBuiltin(KT_METHOD_VM, args.len() as u8), line);
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
        // `copy` is where named arguments earn their keep — `p.copy(y = 2)`
        // rewrites one property and inherits the rest.
        let names: Vec<String> = meta.ctor_params.iter().map(|p| p.name.clone()).collect();
        let slots = bind_args(&format!("{}.copy", meta.name), &names, args)?;
        let mark = sc.enter();
        self.compile_expr(sc, recv)?; // [recv]
        let rslot = sc.temp();
        self.b.emit(Op::SetSlot(rslot), 0);
        for (i, p) in meta.ctor_params.iter().enumerate() {
            match slots[i] {
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
        // A receiver-scope block (`x.apply { … }`) takes the receiver as `this`
        // rather than as `it`, so it gets no implicit `it` at all.
        let recv = self.lambda_recv.take();
        // An unparameterized lambda has the single implicit parameter `it`.
        let declared: Vec<(String, Type)> = if params.is_empty() && recv.is_none() {
            vec![("it".to_string(), Type::Unknown)]
        } else {
            params.to_vec()
        };
        // A parameter the source did not annotate takes its type from the call
        // site (see [`Compiler::lambda_hint`]), which is what lets `it` inside
        // `listOf(1, 2).map { … }` be an `Int` rather than a width the frontend
        // has to play safe with.
        let hint = self.lambda_hint.take().unwrap_or_default();
        let mut effective: Vec<(String, Type, Type)> = declared
            .into_iter()
            .enumerate()
            .map(|(i, (n, t))| {
                let (ht, he) = hint
                    .get(i)
                    .copied()
                    .unwrap_or((Type::Unknown, Type::Unknown));
                (n, if t == Type::Unknown { ht } else { t }, he)
            })
            .collect();
        if let Some((rty, _)) = &recv {
            effective.insert(0, ("this".to_string(), *rty, Type::Unknown));
        }
        // Capture the whole visible enclosing environment (by value), minus names
        // the lambda's own parameters shadow. Captures use slots after the params
        // in the body; the push order here matches the body prologue's pop order.
        let caps: Vec<Captured> = sc
            .visible()
            .into_iter()
            .filter(|c| !effective.iter().any(|(p, _, _)| *p == c.name))
            .collect();
        for c in &caps {
            self.b.emit(Op::GetSlot(c.slot), 0);
        }
        let id = self.lambdas_seen;
        self.lambdas_seen += 1;
        let name_idx = self.b.add_name(&format!("$lambda${id}"));
        let recv_class = recv.as_ref().and_then(|(_, c)| c.clone());
        self.pending_lambdas.push(PendingLambda {
            name_idx,
            params: effective.clone(),
            captures: caps.clone(),
            body: body.to_vec(),
            // A receiver block over a user class puts THAT class's members in
            // implicit scope, replacing any enclosing one.
            class: recv_class
                .clone()
                .filter(|c| self.classes.contains_key(c))
                .or_else(|| {
                    if recv.is_some() {
                        None
                    } else {
                        self.cur_class.clone()
                    }
                }),
            recv_class,
            local_funs: self.local_funs.clone(),
            local_sigs: self.local_sigs.clone(),
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
        for (p, ty, elem) in &pl.params {
            // `this` carries the receiver's class so member dispatch inside the
            // block resolves as it would on the receiver itself.
            if p == "this" {
                sc.declare_obj(p, *ty, false, pl.recv_class.clone());
            } else {
                sc.declare_elem(p, *ty, false, *elem);
            }
        }
        for c in &pl.captures {
            // A captured `var` stays mutable inside the lambda: the write goes
            // through the shared cell, so the enclosing frame sees it.
            sc.declare_full(&c.name, c.ty, c.boxed, c.class.clone(), c.elem);
            if c.boxed {
                sc.box_binding(&c.name);
            }
        }
        let total = pl.params.len() + pl.captures.len();
        for i in (0..total).rev() {
            self.b.emit(Op::SetSlot(i as u16), 0);
        }

        let saved = self.cur_class.take();
        self.cur_class = pl.class.clone();
        let outer_locals = std::mem::replace(&mut self.local_funs, pl.local_funs.clone());
        let outer_local_sigs = std::mem::replace(&mut self.local_sigs, pl.local_sigs.clone());
        let outer_boxed = std::mem::replace(&mut self.boxed_vars, lambda_writes(&pl.body));
        // A lambda body is invoked through a nested `vm.run()`, so it is its own
        // frame for unwinding too: a raise inside it returns out, and the host
        // suppresses any further invocation while the exception is in flight.
        self.push_unwind(UnwindKind::Frame);
        let outer_returns = std::mem::take(&mut self.finally_returns);
        let outer_exits = std::mem::take(&mut self.finally_exits);
        let res = self.compile_block_value(&mut sc, &pl.body);
        self.local_funs = outer_locals;
        self.local_sigs = outer_local_sigs;
        self.boxed_vars = outer_boxed;
        self.cur_class = saved;
        self.finally_returns = outer_returns;
        self.finally_exits = outer_exits;
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
        // Publish the lambda's parameter types before lowering it, so an
        // unannotated `it` picks up the receiver's element type instead of
        // staying `Unknown` (see [`Compiler::lambda_hint`]).
        let elem = self.infer_elem(sc, recv);
        let hint = self.hof_param_types(sc, recv, name, elem, extras);
        self.compile_expr(sc, recv)?;
        for e in extras {
            self.compile_expr(sc, e)?;
        }
        if !hint.is_empty() {
            self.lambda_hint = Some(hint);
        }
        self.compile_expr(sc, closure)?;
        // A closure passed by NAME rather than written inline never reaches
        // `compile_lambda`, so clear any hint it left behind.
        self.lambda_hint = None;
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b
            .emit(Op::CallBuiltin(KT_COLL_HOF, extras.len() as u8), line);
        Ok(hof_ret_type(name))
    }

    /// `recv.field (op)= value` — an object property write.
    /// Push a delegated property's delegate, the `thisRef` it was declared in,
    /// and the `KProperty` naming it — the leading three arguments of both
    /// `getValue(thisRef, property)` and `setValue(thisRef, property, value)`.
    ///
    /// The receiver is compiled twice, which is why a delegated access is only
    /// emitted for a receiver that is a name or `this` — the shapes an access
    /// site actually takes.
    fn emit_delegate_head(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        line: u32,
    ) -> Result<(), String> {
        self.compile_expr(sc, recv)?; // [recv]
        let fidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(fidx), line);
        self.b.emit(Op::Extended(KT_GETFIELD, 0), line); // [delegate]
        self.compile_expr(sc, recv)?; // [delegate, thisRef]
        let midx = self.b.add_constant(Value::str(kproperty_meta()));
        self.b.emit(Op::LoadConst(midx), line);
        let nidx = self.b.add_constant(Value::str(name.to_string()));
        self.b.emit(Op::LoadConst(nidx), line);
        self.b.emit(Op::Extended(KT_NEW, 1), line); // [delegate, thisRef, property]
        Ok(())
    }

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
        // A delegated property has no storage: the write is the delegate's
        // `setValue(thisRef, property, value)`.
        if let Some(dc) = self
            .infer_class(sc, recv)
            .and_then(|c| self.classes.get(&c).and_then(|m| m.prop(name)).cloned())
            .and_then(|p| p.delegate)
        {
            if !self.classes.contains_key(&dc) {
                return Err(format!(
                    "property {name}: delegate {dc} is not a class declaring \
                     `operator fun setValue`"
                ));
            }
            self.emit_delegate_head(sc, recv, name, 0)?;
            let store = self.compound_value(recv, name, None, op, value);
            self.compile_expr(sc, &store)?; // [delegate, thisRef, property, value]
            let idx = self.b.add_name(&method_sub_name(&dc, "setValue"));
            self.b.emit(Op::Call(idx, 4), 0);
            self.b.emit(Op::Pop, 0); // `setValue` answers Unit
            return Ok(());
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
        // `a[i] = v` is the `set` convention. The compound `a[i] += v` reads
        // through `compound_value`, whose `Expr::Index` reaches `get`.
        if self.declares_operator(sc, recv, "set") {
            let store = self.compound_value(recv, "", Some(index), op, value);
            let args = [index.clone(), store];
            self.compile_member(sc, recv, "set", &args, false, 0)?;
            self.b.emit(Op::Pop, 0);
            return Ok(());
        }
        self.compile_expr(sc, recv)?; // [recv]
        self.compile_expr(sc, index)?; // [recv, index]
        let store = self.compound_value(recv, "", Some(index), op, value);
        self.compile_expr(sc, &store)?; // [recv, index, value]
        self.b.emit(Op::CallBuiltin(KT_INDEX_SET_VM, 3), 0);
        self.b.emit(Op::Pop, 0);
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
            self.b.emit(Op::CallBuiltin(KT_METHOD_VM, 0), 0);
            let slot = sc.declare(nm, Type::Unknown, false);
            self.b.emit(Op::SetSlot(slot), 0);
        }
        Ok(())
    }

    /// Lower a safe member/method access `recv?.member(args)`. Evaluates the
    /// receiver into a slot; if it is null the whole access is null, otherwise
    /// the member dispatches on the not-null path.
    ///
    /// That not-null path re-enters [`Compiler::compile_member`] with the slot
    /// standing in as the receiver, so `?.` reaches every routing the plain `.`
    /// does — collection higher-order functions (`xs?.map { … }`), the `it`-form
    /// scope functions (`s?.let { … }`), user-class virtual dispatch and
    /// property reads — instead of only the `KT_METHOD` stdlib table. The
    /// stand-in name carries a `$`, which the lexer never produces, so it cannot
    /// collide with a user binding, and it is declared inside this scope so
    /// nested safe calls each get their own slot.
    fn compile_safe_member(
        &mut self,
        sc: &mut Scope,
        recv: &Expr,
        name: &str,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        let mark = sc.enter();
        let rty = self.compile_expr(sc, recv)?; // [recv]
        let rclass = self.infer_class(sc, recv);
        let rslot = sc.declare_obj(SAFE_RECV, rty, false, rclass);
        self.b.emit(Op::SetSlot(rslot), 0); // []
        self.b.emit(Op::GetSlot(rslot), 0); // [recv]
        self.b.emit(Op::Extended(KT_ISNULL, 0), 0); // [isNull]
                                                    // Not null → jump to the call; null → fall through to the null result.
        let jf = self.b.emit(Op::JumpIfFalse(0), line);
        self.b.emit(Op::LoadUndef, 0); // [null]
        let jend = self.b.emit(Op::Jump(0), 0);
        let call_pos = self.b.current_pos();
        self.b.patch_jump(jf, call_pos);
        let stand_in = Expr::Var(SAFE_RECV.to_string());
        let ty = self.compile_member(sc, &stand_in, name, args, false, line)?;
        let end = self.b.current_pos();
        self.b.patch_jump(jend, end);
        sc.exit(mark);
        // The whole point of `?.` is that the result may be null, so the static
        // type has to be the nullable one.
        Ok(nullable_if_safe(ty, true))
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

        // `===`/`!==` never consult a type, an `equals` override or an operator
        // convention — they ask the host whether the two values are the same
        // object. So they resolve before every rule below, all of which exist
        // to pick a STRUCTURAL comparison.
        if matches!(op, BinOp::RefEq | BinOp::RefNe) {
            self.compile_expr(sc, l)?;
            self.compile_expr(sc, r)?;
            self.b.emit(Op::Extended(KT_IDENTITY, 0), 0);
            if op == BinOp::RefNe {
                self.b.emit(Op::LogNot, 0);
            }
            return Ok(Type::Boolean);
        }

        let lt = self.infer(sc, l);
        let rt = self.infer(sc, r);

        // `x == null` / `x != null` is a null test, not a value comparison: the
        // native numeric/string ops would compare an absent value's coerced form
        // (`0` / `""`) and answer `true` for a non-null `0`/`""` receiver. A
        // literal `null` operand is decided from the SYNTAX, so this precedes
        // the type-driven rule below and keeps the cheap one-operand op.
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

        // `==`/`!=` reaches the native ops only when the STATIC types say which
        // native op is right: both numeric/`Char`/`Boolean` (`Op::NumEq`) or
        // both `String` (`Op::StrEq`). Anything else — a heap object, or an
        // operand the frontend could not type — goes to the runtime-tagged
        // structural comparison, which reads each value's own kind.
        //
        // The `Unknown` half of that is not caution, it is a miscompile fixed:
        // `Op::NumEq` COERCES its operands, and two different strings both
        // coerce to `0`, so `xs.filter { it == "a" }` over a `List<String>`
        // reached through a declared parameter kept every element. `Unknown`
        // arises wherever inference stops — a declared `List<String>` parameter,
        // a `Map` entry half, a member call's result — and each of those can
        // hold a string just as easily as a number.
        let native_eq = (lt.is_str() && rt.is_str())
            || (matches!(
                lt,
                Type::Int | Type::Long | Type::Double | Type::Char | Type::Boolean
            ) && matches!(
                rt,
                Type::Int | Type::Long | Type::Double | Type::Char | Type::Boolean
            ));
        if matches!(op, BinOp::Eq | BinOp::Ne) && !native_eq {
            self.compile_expr(sc, l)?;
            self.compile_expr(sc, r)?;
            // A builtin rather than the `KT_OBJEQ` extension op: a user `equals`
            // override runs re-entrantly, which an extension handler cannot host
            // (see `KT_OBJEQ_VM`).
            self.b.emit(Op::CallBuiltin(KT_OBJEQ_VM, 2), 0);
            if op == BinOp::Ne {
                self.b.emit(Op::LogNot, 0);
            }
            return Ok(Type::Boolean);
        }

        // The arithmetic operators are CONVENTIONS resolved against the LEFT
        // operand (see `operator_fn`), so this precedes the string rule below:
        // `listOf(1, 2) + "a"` is a List's `plus` and answers `[1, 2, a]`, while
        // `"a" + listOf(1, 2)` is a String's and answers `a[1, 2]`.
        if let Some(fname) = operator_fn(op) {
            // A user class that declares the convention. Resolved statically,
            // like every other member call on a known class.
            if self.declares_operator(sc, l, fname) {
                return self.compile_member(sc, l, fname, std::slice::from_ref(r), false, 0);
            }
            // Any other heap receiver — a `List`, `Set`, `Map` or range.
            // Dispatched at run time because the frontend tracks these as one
            // `Type::Obj` and only the value knows which it is.
            if lt == Type::Obj {
                self.emit_operator_call(sc, l, r, fname)?;
                return Ok(Type::Obj);
            }
        }

        // `<`/`>`/`<=`/`>=` on a class declaring `compareTo` are that
        // convention: the operator tests the SIGN of `a.compareTo(b)`.
        if matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge)
            && self.declares_operator(sc, l, "compareTo")
        {
            self.compile_member(sc, l, "compareTo", std::slice::from_ref(r), false, 0)?;
            self.b.emit(Op::LoadInt(0), 0);
            self.b.emit(
                match op {
                    BinOp::Lt => Op::NumLt,
                    BinOp::Gt => Op::NumGt,
                    BinOp::Le => Op::NumLe,
                    _ => Op::NumGe,
                },
                0,
            );
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

        let both_str = lt.is_str() && rt.is_str();
        // A statically known `Double` on either side is the only thing that
        // forces IEEE division/remainder; every other combination is decided
        // from the runtime values by `KT_IDIV`/`KT_IMOD`.
        let known_double = lt == Type::Double || rt == Type::Double;
        let num_ty = promote(lt, rt);
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
                if known_double {
                    // IEEE division, not the native op: Kotlin's `x / 0.0` is a
                    // signed infinity and `0.0 / 0.0` is NaN, where `Op::Div`
                    // yields `Undef` (which printed as `null`).
                    self.b.emit(Op::Extended(KT_DDIV, 0), 0);
                    Type::Double
                } else {
                    // `KT_IDIV` picks truncating or IEEE division from the
                    // RUNTIME values, so it is also the right op when an operand
                    // went untyped: an integral value at run time means Kotlin
                    // gave that expression an integral static type, and the two
                    // integral widths both truncate. Committing to IEEE instead
                    // made `f(10) / f(3)` answer `3.3333333333333335` for a
                    // `val f: (Int) -> Int`, and turned `x / 0` from an
                    // ArithmeticException into `Infinity`.
                    self.b.emit(Op::Extended(KT_IDIV, 0), 0);
                    num_ty
                }
            }
            BinOp::Mod => {
                // `KT_IMOD` is value-directed in the same way, so only the
                // RESULT type is in question here.
                self.b.emit(Op::Extended(KT_IMOD, 0), 0);
                if known_double {
                    Type::Double
                } else {
                    num_ty
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
            BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => {
                unreachable!("handled above")
            }
        };
        // An `Int`-precision arithmetic result wraps at 32 bits. A comparison
        // yields a Boolean, a `Char` result is truncated to 16 bits by the host's
        // `char_of`, and a `Long`/`Double` result keeps its full width — so this
        // fires only for the arithmetic operators on two `Int`-width operands.
        if ty == Type::Int
            && matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            )
            && narrows_to_int(lt, rt)
        {
            self.emit_wrap32();
        }
        Ok(ty)
    }

    /// Narrow the value on top of the stack to a signed 32-bit `Int` — Kotlin's
    /// `Int` overflow wrap.
    ///
    /// `Shl 32` then `Shr 32` sign-extends the low 32 bits, because fusevm's
    /// `Shr` is an ARITHMETIC shift in the interpreter, the tracing JIT and the
    /// AOT backend alike. Two native ops rather than a host call is what keeps a
    /// hot `Int` loop traceable: a `CallBuiltin`/`Extended` here would abort
    /// trace recording and cost the loop its JIT. The pair is a no-op for a value
    /// already inside the `Int` range.
    fn emit_wrap32(&mut self) {
        self.b.emit(Op::LoadInt(32), 0);
        self.b.emit(Op::Shl, 0);
        self.b.emit(Op::LoadInt(32), 0);
        self.b.emit(Op::Shr, 0);
    }

    /// `target(args)` where `target` is an arbitrary expression rather than a
    /// name — `f()()`, `lst[0](7)`, `{ … }(9)`.
    ///
    /// Kotlin spells this as the `invoke` operator, so an instance of a class
    /// that declares `operator fun invoke` routes to that method; everything
    /// else is a function value and goes through the closure-call builtin, the
    /// same one a named `f(args)` on a local uses.
    fn compile_invoke(
        &mut self,
        sc: &mut Scope,
        target: &Expr,
        args: &[Expr],
        line: u32,
    ) -> Result<Type, String> {
        if let Some(cls) = self.infer_class(sc, target) {
            if self
                .classes
                .get(&cls)
                .is_some_and(|m| m.methods.contains_key("invoke"))
            {
                return self.compile_member(sc, target, "invoke", args, false, line);
            }
        }
        self.compile_expr(sc, target)?;
        for a in args {
            self.compile_expr(sc, a)?;
        }
        self.b
            .emit(Op::CallBuiltin(KT_CLOSURE_CALL, args.len() as u8), line);
        Ok(Type::Unknown)
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
            // …unless the local holds an instance of a class declaring
            // `operator fun invoke`, in which case `b(5)` is that method, not a
            // closure call. Kotlin resolves the same way: the `invoke` operator
            // is what makes any value callable.
            if let Some(cls) = sc.class_of(name) {
                if self
                    .classes
                    .get(&cls)
                    .is_some_and(|m| m.methods.contains_key("invoke"))
                {
                    let recv = Expr::Var(name.to_string());
                    return self.compile_member(sc, &recv, "invoke", args, false, line);
                }
            }
            self.b.emit(Op::GetSlot(slot), line);
            for a in args {
                self.compile_expr(sc, a)?;
            }
            self.b
                .emit(Op::CallBuiltin(KT_CLOSURE_CALL, args.len() as u8), line);
            // The declared result type of the annotation, when there was one.
            // It has to agree with what `infer` answers for the same node, or
            // the two disagree on whether a result needs the 32-bit wrap:
            // `-f(Int.MIN_VALUE)` narrows only if this says `Int`.
            return Ok(sc.fn_ret_of(name));
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
            // `with(x) { … }` — the free-function spelling of `x.run { … }`,
            // and the only scope function written as a call rather than on a
            // receiver. It routes through the same builtin.
            "with" if args.len() == 2 && is_lambda(&args[1]) => {
                self.compile_member(sc, &args[0], "run", &args[1..], false, line)
            }
            // `runCatching { … }` — run the block and package its outcome as a
            // `Result`. It is a catch, so the program counts as using
            // exceptions (see [`uses_exceptions`]) and the pending-slot
            // machinery is emitted.
            "runCatching" if args.len() == 1 && is_lambda(&args[0]) => {
                self.compile_expr(sc, &args[0])?;
                self.b.emit(Op::CallBuiltin(KT_RUN_CATCHING, 0), line);
                Ok(Type::Obj)
            }
            // `run { … }` with no receiver: a block evaluated on the spot for
            // its value. The closure takes no parameter at all, which is what
            // separates it from the receiver form.
            "run" if args.len() == 1 && is_lambda(&args[0]) => {
                self.compile_expr(sc, &args[0])?;
                self.b.emit(Op::CallBuiltin(KT_CLOSURE_CALL, 0), line);
                Ok(Type::Unknown)
            }
            // `generateSequence(seed) { next }` — the one sequence that is
            // genuinely lazy here, because it is the one that can be endless.
            // The step answers Kotlin `null` to end it.
            "generateSequence" if args.len() == 2 && is_lambda(&args[1]) => {
                self.compile_expr(sc, &args[0])?;
                self.lambda_hint = Some(vec![(Type::Unknown, Type::Unknown)]);
                self.compile_expr(sc, &args[1])?;
                self.lambda_hint = None;
                self.b.emit(Op::CallBuiltin(KT_GENSEQ, 0), line);
                Ok(Type::Obj)
            }
            // `compareBy { … }` / `compareByDescending { … }` — a `Comparator`
            // over one or more key selectors, for `sortedWith`. The selectors
            // are ordinary one-parameter lambdas, so each is compiled to a
            // closure value and the host holds the chain.
            "compareBy" | "compareByDescending"
                if !args.is_empty() && args.iter().all(is_lambda) =>
            {
                for a in args {
                    self.lambda_hint = Some(vec![(Type::Unknown, Type::Unknown)]);
                    self.compile_expr(sc, a)?;
                    self.lambda_hint = None;
                }
                let nidx = self.b.add_constant(Value::str(name.to_string()));
                self.b.emit(Op::LoadConst(nidx), line);
                self.b
                    .emit(Op::CallBuiltin(KT_COMPARATOR, args.len() as u8), line);
                Ok(Type::Obj)
            }
            // `Pair(a, b)` / `Triple(a, b, c)` — the constructor spellings of
            // what `a to b` already builds.
            "Pair" if args.len() == 2 => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_PAIR, 0), line);
                Ok(Type::Obj)
            }
            "Triple" if args.len() == 3 => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_PAIR, 1), line);
                Ok(Type::Obj)
            }
            // `StringBuilder()` / `StringBuilder(text)` / `StringBuilder(cap)`.
            // The two one-argument overloads are told apart by the VALUE at
            // run time (see [`crate::host::new_builder`]) rather than here: a
            // frontend that inferred `Unknown` for the argument would otherwise
            // have to guess which constructor was written.
            "StringBuilder" | "StringBuffer" if args.len() <= 1 => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b
                    .emit(Op::Extended(KT_BUILDER, args.len() as u8), line);
                Ok(Type::Obj)
            }
            // `buildString { … }` / `buildList { … }` are `apply` over a fresh
            // mutable builder, with the result read back out — literally how
            // `kotlin.text` and `kotlin.collections` define them. Desugaring to
            // that instead of adding two more host ops means the block's
            // implicit `this` (`append(x)` with no qualifier) is the receiver
            // scope machinery already used by `apply`, not a second one.
            //
            // The capacity overload (`buildString(64) { … }`) is accepted and
            // its hint discarded: nothing observable depends on it.
            "buildString" | "buildList" if args.last().is_some_and(is_lambda) => {
                let ctor = if name == "buildString" {
                    "StringBuilder"
                } else {
                    "mutableListOf"
                };
                let fresh = Expr::Call {
                    name: ctor.to_string(),
                    args: Vec::new(),
                    line,
                };
                let filled = Expr::MethodCall {
                    recv: Box::new(fresh),
                    name: "apply".to_string(),
                    args: vec![args[args.len() - 1].clone()],
                    safe: false,
                    line,
                };
                if name == "buildList" {
                    return self.compile_expr(sc, &filled).map(|_| Type::Obj);
                }
                self.compile_member(sc, &filled, "toString", &[], false, line)?;
                Ok(Type::String)
            }
            // `repeat(n) { … }` runs the block with the index `it`, exactly
            // what `(0 until n).forEach { … }` does — and Kotlin's own `repeat`
            // is that loop. Desugaring keeps the `it` binding, the `Unit`
            // result, and the zero-iteration case all on one code path.
            "repeat" if args.len() == 2 && is_lambda(&args[1]) => {
                let indices = Expr::Range {
                    start: Box::new(Expr::Int(0)),
                    end: Box::new(args[0].clone()),
                    kind: RangeKind::Until,
                };
                self.compile_member(sc, &indices, "forEach", &args[1..], false, line)
            }
            // The `kotlin` preconditions. Their message argument is passed
            // UNEVALUATED (as the lambda it was written as) so the failing path
            // is the only one that runs it; see [`KT_PRECOND`].
            "require" | "requireNotNull" | "check" | "checkNotNull" | "error" | "TODO"
                if args.len() <= 2 =>
            {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                let nidx = self.b.add_constant(Value::str(name.to_string()));
                self.b.emit(Op::LoadConst(nidx), line);
                self.b
                    .emit(Op::CallBuiltin(KT_PRECOND, args.len() as u8), line);
                // `requireNotNull`/`checkNotNull` answer their subject; the
                // rest answer `Unit` (or never return at all).
                Ok(match name {
                    "requireNotNull" | "checkNotNull" => Type::Unknown,
                    _ => Type::Unit,
                })
            }
            // `listOfNotNull(a, b, c)` is `listOf(a, b, c).filterNotNull()` —
            // the nulls are dropped at run time because the frontend cannot see
            // which arguments are null (`listOfNotNull(maybe, "b")` depends on
            // a value, not a type).
            "listOfNotNull" => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.b.emit(Op::Extended(KT_LIST, args.len() as u8), line);
                let nidx = self.b.add_constant(Value::str("filterNotNull".to_string()));
                self.b.emit(Op::LoadConst(nidx), line);
                self.b.emit(Op::CallBuiltin(KT_METHOD_VM, 0), line);
                Ok(Type::Obj)
            }
            // `ArrayList(other)` copies a collection, where `ArrayList()` builds
            // an empty one. A `List` iterates in position order however it was
            // built, so unlike its `Set`/`Map` siblings it needs no order spec.
            "ArrayList" if args.len() == 1 => {
                self.compile_member(sc, &args[0], "toMutableList", &[], false, line)
            }
            // Collection builders → heap objects.
            "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" | "ArrayList" => {
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
            // `Set` builders. `setOf`/`linkedSetOf` are `LinkedHashSet`-backed
            // and iterate in insertion order; `hashSetOf`/`HashSet` and
            // `sortedSetOf` do NOT, and the trailing spec argument says which
            // (see `COLL_HASH`).
            "setOf" | "mutableSetOf" | "hashSetOf" | "linkedSetOf" | "sortedSetOf" | "emptySet"
            | "HashSet" | "LinkedHashSet" | "TreeSet" => {
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.emit_coll_spec(name, args.len());
                self.b
                    .emit(Op::CallBuiltin(KT_SET_VM, args.len() as u8), line);
                Ok(Type::Obj)
            }
            "mapOf" | "mutableMapOf" | "hashMapOf" | "emptyMap" | "HashMap" | "LinkedHashMap" => {
                // Each argument is a `k to v` Pair — except in the copy form
                // `HashMap(other)`, whose single argument is a whole map.
                for a in args {
                    self.compile_expr(sc, a)?;
                }
                self.emit_coll_spec(name, args.len());
                self.b
                    .emit(Op::CallBuiltin(KT_MAP_VM, args.len() as u8), line);
                Ok(Type::Obj)
            }
            _ => {
                // A constructor call `Class(args)`.
                if let Some(meta) = self.classes.get(name).cloned() {
                    return self.compile_construct(sc, &meta, args, line);
                }
                // A free user function.
                if let Some(sig) = self
                    .local_sigs
                    .get(name)
                    .or_else(|| self.fun_sig.get(name))
                    .cloned()
                {
                    let full = self.expand_args(&format!("function {name}"), &sig.params, args)?;
                    for a in &full {
                        self.compile_expr(sc, a)?;
                    }
                    // A local `fun` shadows a top-level one of the same name and
                    // lives under its mangled sub.
                    let sub = self.local_funs.get(name).cloned();
                    let idx = self.b.add_name(sub.as_deref().unwrap_or(name));
                    self.b.emit(Op::Call(idx, sig.arity as u8), line);
                    return Ok(self.call_ret(sc, &sig, args));
                }
                // An extension on the enclosing receiver, called without a
                // qualifier from inside a method or another extension.
                if sc.slot("this").is_some() {
                    let this = Expr::Var("this".into());
                    if self.resolve_ext(sc, &this, name).is_some() {
                        return self.compile_member(sc, &this, name, args, false, line);
                    }
                }
                // A companion method called without a qualifier from inside the
                // owning class.
                if let Some(comp) = self
                    .cur_class
                    .clone()
                    .and_then(|c| self.companion_of(&c))
                    .filter(|c| self.classes[c].methods.contains_key(name))
                {
                    return self.compile_member(sc, &Expr::Var(comp), name, args, false, line);
                }
                // Implicit `this.f(args)` where `f` is a PROPERTY holding a
                // function value — `class Box(val f: (Int) -> Int) { fun go(x:
                // Int) = f(x) }`. Checked before the method case only in the
                // sense that a class cannot have both; a method of the same
                // name wins below because `methods` is consulted first there.
                if let Some(cls) = self.cur_class.clone() {
                    let is_prop = self
                        .classes
                        .get(&cls)
                        .is_some_and(|m| !m.methods.contains_key(name) && m.prop(name).is_some());
                    if is_prop && sc.slot("this").is_some() {
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
                // Inside a receiver scope whose receiver is not a user class, an
                // unqualified call is a member of it: `with("x") { uppercase() }`.
                // The `Expr::Var` arm applies the same rule to a bare name.
                if sc.slot("this").is_some() && self.cur_class.is_none() {
                    return self.compile_member(
                        sc,
                        &Expr::Var("this".into()),
                        name,
                        args,
                        false,
                        line,
                    );
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

    /// Rewrite a call's argument list into one expression per declared
    /// parameter, in declaration order — resolving named arguments, filling
    /// omitted ones from their defaults, and packing a `vararg` tail into an
    /// array literal.
    ///
    /// Doing it as an AST rewrite rather than at each emit site means every
    /// caller (free function, method, constructor, extension) gets defaults and
    /// `vararg` from one implementation, and the emit sites keep lowering a
    /// plain positional list.
    ///
    /// A default is evaluated at the CALL site, so it may not name another
    /// parameter of the callee. Kotlin evaluates it in the callee's frame, where
    /// it can; that form is rejected loudly rather than silently misbound.
    fn expand_args(
        &self,
        callee: &str,
        params: &[Param],
        args: &[Expr],
    ) -> Result<Vec<Expr>, String> {
        let vararg_at = params.iter().position(|p| p.vararg.is_some());
        if let Some(v) = vararg_at {
            if v + 1 != params.len() {
                return Err(format!(
                    "{callee}: a `vararg` parameter is only supported as the last one"
                ));
            }
        }
        // The positional arguments a trailing `vararg` collects: everything from
        // its own position on, up to the first named argument.
        let mut rest: Vec<Expr> = Vec::new();
        let mut head: Vec<Expr> = args.to_vec();
        if let Some(v) = vararg_at {
            let split = head
                .iter()
                .position(|a| matches!(a, Expr::Named { .. }))
                .unwrap_or(head.len())
                .max(v);
            if split > v {
                rest = head.drain(v..split).collect();
            }
        }
        let names = params.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        let slots = bind_args(callee, &names, &head)?;
        let mut out = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            match (slots[i], p.vararg, &p.default) {
                // A `vararg` given as a single named/positional argument passes
                // that value through — it is already the array (`f(xs = arr)`).
                (Some(a), Some(_), _) if rest.is_empty() => out.push(a.clone()),
                (Some(a), Some(elem), _) => {
                    rest.insert(0, a.clone());
                    out.push(vararg_array(elem, &rest));
                }
                (Some(a), None, _) => out.push(a.clone()),
                (None, Some(elem), _) => out.push(vararg_array(elem, &rest)),
                (None, _, Some(d)) => out.push(d.clone()),
                (None, None, None) => {
                    return Err(format!("{callee} has no argument for `{}`", p.name))
                }
            }
        }
        Ok(out)
    }

    /// The extension declared for `name` on the receiver's static type, if the
    /// program has one: its `(mangled sub name, signature)`.
    ///
    /// Resolution is by the receiver's *spelled* type name so `fun Int.f()` and
    /// `fun Long.f()` stay apart. A receiver the frontend cannot type falls back
    /// to a sole program-wide extension of that name — the shape a generic or
    /// container receiver takes — and an ambiguous one is a compile error rather
    /// than an arbitrary pick.
    fn resolve_ext(&self, sc: &Scope, recv: &Expr, name: &str) -> Option<(String, FnSig)> {
        if self.extensions.is_empty() {
            return None;
        }
        if let Some(tn) = self.recv_type_name(sc, recv) {
            if let Some(sig) = self.extensions.get(&(tn.clone(), name.to_string())) {
                return Some((ext_sub_name(&tn, name), sig.clone()));
            }
            // A user class inherits its supertypes' extensions.
            if let Some(meta) = self.classes.get(&tn) {
                for anc in meta.mro.iter().skip(1) {
                    if let Some(sig) = self.extensions.get(&(anc.clone(), name.to_string())) {
                        return Some((ext_sub_name(anc, name), sig.clone()));
                    }
                }
            }
        }
        let mut hits = self
            .extensions
            .iter()
            .filter(|((_, n), _)| n == name)
            .map(|((r, n), s)| (ext_sub_name(r, n), s.clone()));
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    }

    /// The declared `(return type, return class)` of the extension a call
    /// `recv.name(…)` resolves to, or `None` when it resolves to something else.
    ///
    /// It applies the same member-first rule as [`Compiler::compile_member`], so
    /// the static type this reports and the code that site emits can never
    /// disagree — which is what keeps integer narrowing and `/` dispatch correct
    /// through an extension's result.
    fn ext_ret(
        &self,
        sc: &Scope,
        recv: &Expr,
        name: &str,
        argc: usize,
    ) -> Option<(Type, Option<String>)> {
        let member_wins = self.infer_class(sc, recv).is_some_and(|c| {
            self.classes
                .get(&c)
                .and_then(|m| m.methods.get(name))
                .is_some_and(|s| s.arity == argc)
        });
        if member_wins {
            return None;
        }
        let (_, sig) = self.resolve_ext(sc, recv, name)?;
        Some((sig.ret, sig.ret_class))
    }

    /// The hoisted singleton name of `cls`'s `companion object`, if it declared
    /// one. `cls` must name a class rather than a value — a local of the same
    /// name shadows the type, exactly as in Kotlin.
    fn companion_of(&self, cls: &str) -> Option<String> {
        let comp = companion_name(cls);
        (self.classes.contains_key(cls) && self.classes.contains_key(&comp)).then_some(comp)
    }

    /// The receiver's spelled type name for extension lookup, or `None` when the
    /// frontend cannot name it.
    fn recv_type_name(&self, sc: &Scope, recv: &Expr) -> Option<String> {
        if let Some(cls) = self.infer_class(sc, recv) {
            return Some(cls);
        }
        match self.infer(sc, recv) {
            Type::Int => Some("Int".into()),
            Type::Long => Some("Long".into()),
            Type::Double => Some("Double".into()),
            Type::Boolean => Some("Boolean".into()),
            Type::Char => Some("Char".into()),
            Type::String | Type::NullableString => Some("String".into()),
            _ => None,
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
        matches!(
            self.qualifier(sc, recv).as_deref(),
            Some("Math") | Some("java.lang.Math")
        )
    }

    /// The dotted PATH a receiver expression spells, when it is nothing but a
    /// chain of plain names none of which is bound to anything —
    /// `kotlin.math`, `java.lang.Math`, `String`. `None` for any receiver that
    /// is a value.
    ///
    /// A fully-qualified reference reaches the compiler as ordinary member
    /// access (`kotlin.math.floor(x)` is `Member(Member(Var("kotlin"),
    /// "math"), "floor")` applied to an argument), so recognizing one means
    /// flattening the chain back to the name it spelled. The shadowing check is
    /// what keeps `val kotlin = 1; kotlin.math` from being a package: a local
    /// binding or a user class of that name wins, as it does for `Math`.
    fn qualifier(&self, sc: &Scope, recv: &Expr) -> Option<String> {
        match recv {
            Expr::Var(n) => (sc.slot(n).is_none()
                && !self.classes.contains_key(n)
                && self.companion_of(n).is_none())
            .then(|| n.clone()),
            Expr::Member {
                recv,
                name,
                safe: false,
                ..
            } => Some(format!("{}.{name}", self.qualifier(sc, recv)?)),
            _ => None,
        }
    }

    /// Whether `name` names a built-in type (rather than a value) in this
    /// program — the receiver position of a companion constant. A local binding
    /// or a user class of the same name shadows it, exactly as with `Math`.
    fn is_type_ref(&self, sc: &Scope, name: &str) -> bool {
        sc.slot(name).is_none() && !self.classes.contains_key(name)
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
        let ty = math_ret_type(name, &tys);
        // `abs(Int.MIN_VALUE)` is the one integral math result that leaves the
        // `Int` range — negating `Int.MIN_VALUE` does not fit — and the host has
        // no argument width to narrow it with, so it happens here.
        if ty == Type::Int && tys.iter().copied().all(is_int_width) {
            self.emit_wrap32();
        }
        Ok(ty)
    }

    /// Lower a constructor call `Class(args)`: push the class metadata string,
    /// then each stored-property value in declaration order, then `KT_NEW`. Only
    /// `val`/`var` primary-constructor params are stored; plain params are not
    /// modeled (they carry no property).
    /// Choose which constructor a `C(args)` call runs: `None` for the primary,
    /// `Some(i)` for the Nth secondary, together with that constructor's
    /// parameters.
    ///
    /// Arity narrows the field — a candidate must be able to take this many
    /// arguments, counting the primary's defaults. Where more than one still
    /// fits, the ARGUMENT TYPES decide, which is what tells
    /// `Def(2)` from `Def("xyz")` when `class Def(val a: Int, val b: Int = 5)`
    /// also declares `constructor(s: String)`. A class that wrote no primary
    /// constructor prefers a secondary outright, because Kotlin only
    /// synthesizes the implicit primary when nothing else is declared.
    fn select_ctor(
        &self,
        sc: &Scope,
        meta: &ClassMeta,
        args: &[Expr],
    ) -> (Option<usize>, Vec<Param>) {
        let primary: Vec<Param> = meta.ctor_params.iter().map(|p| p.as_param()).collect();
        let required = primary.iter().filter(|p| p.default.is_none()).count();
        let argc = args.len();
        let arg_types: Vec<Type> = args.iter().map(|a| self.infer(sc, a)).collect();

        let primary_fits = argc >= required
            && argc <= primary.len()
            && meta.has_primary
            && params_accept(&primary, &arg_types);
        if primary_fits {
            return (None, primary);
        }
        let sec = meta
            .sec_arities
            .iter()
            .enumerate()
            .filter(|(_, n)| **n == argc)
            .map(|(i, _)| i)
            // Prefer a secondary whose parameter types match; fall back to the
            // first of that arity so an unresolvable argument type still picks
            // one rather than failing.
            .find(|i| params_accept(&meta.sec_params[*i], &arg_types))
            .or_else(|| {
                meta.sec_arities
                    .iter()
                    .position(|n| *n == argc)
                    .filter(|_| !meta.has_primary || argc < required || argc > primary.len())
            });
        match sec {
            Some(i) => (Some(i), meta.sec_params[i].clone()),
            None => (None, primary),
        }
    }

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
        let (sec, params) = self.select_ctor(sc, meta, args);
        let full = self.expand_args(&format!("constructor {}", meta.name), &params, args)?;
        for a in &full {
            self.compile_expr(sc, a)?;
        }
        let name = match sec {
            Some(i) => sec_ctor_sub_name(&meta.name, i),
            None => ctor_sub_name(&meta.name),
        };
        let idx = self.b.add_name(&name);
        self.b.emit(Op::Call(idx, params.len() as u8), line);
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
            // The `when (val n = …)` form names that same slot, so an arm body
            // reads the subject through the binding with no second evaluation.
            let slot = match &w.binding {
                Some(n) => sc.declare_obj(n, t, false, self.infer_class(sc, subject)),
                None => sc.temp(),
            };
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
                    // A `when` arm is an `==` against the subject, so an object
                    // subject takes the same object equality `==` does — the
                    // native op would compare two heap HANDLES numerically and
                    // pick whichever arm happened to sit at the lower one.
                    if sty == Type::Obj || et == Type::Obj {
                        self.b.emit(Op::CallBuiltin(KT_OBJEQ_VM, 2), 0);
                    } else {
                        let str_eq = sty.is_str() || et.is_str();
                        self.b.emit(if str_eq { Op::StrEq } else { Op::NumEq }, 0);
                    }
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
            // The result of invoking a function value. The declared return type
            // of a `(Int) -> Int` is not tracked through the value, so this is
            // the same `Unknown` a named call on a local lambda yields, and it
            // reaches the same runtime-tagged display path.
            Expr::Invoke { .. } => Type::Unknown,
            // A named argument types as the value it carries.
            Expr::Named { value, .. } => self.infer(sc, value),
            Expr::Int(_) => Type::Int,
            Expr::Long(_) => Type::Long,
            Expr::Float(_) => Type::Double,
            Expr::Bool(_) => Type::Boolean,
            Expr::Char(_) => Type::Char,
            Expr::Null => Type::Unknown,
            Expr::Str(_) => Type::String,
            Expr::Var(n) => {
                if sc.slot(n).is_some() {
                    return sc.ty(n);
                }
                // A bare name that is a property of the enclosing class, or of
                // its companion. This mirrors the resolution order the emitter
                // uses for the same node (local, then implicit `this`, then
                // companion, then global) and MUST agree with it: inferring
                // `Unknown` for a property the emitter reads as an `Int` sent
                // `x / 2` inside a method down the `Double` division path, so
                // `class V(val x: Int) { fun a() = x / 2 }` answered `3.5`
                // where the reference toolchain truncates to `3`.
                if let Some(t) = self
                    .cur_class
                    .as_ref()
                    .and_then(|c| self.classes.get(c))
                    .and_then(|m| m.prop(n))
                    .map(|p| p.ty)
                {
                    return t;
                }
                if let Some(t) = self
                    .cur_class
                    .as_ref()
                    .and_then(|c| self.companion_of(c))
                    .and_then(|c| self.classes.get(&c))
                    .and_then(|m| m.prop(n))
                    .map(|p| p.ty)
                {
                    return t;
                }
                if let Some(p) = self.globals.get(n) {
                    p.ty
                } else if self.resolve_math_const(n).is_some() {
                    Type::Double
                } else {
                    sc.ty(n)
                }
            }
            Expr::Unary { op, expr } => match op {
                UnOp::Not => Type::Boolean,
                UnOp::Neg => match self.infer(sc, expr) {
                    Type::Double => Type::Double,
                    Type::Long => Type::Long,
                    _ => Type::Int,
                },
            },
            Expr::Binary { op, l, r } => match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::RefEq
                | BinOp::RefNe
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
                    } else {
                        promote(lt, rt)
                    }
                }
                BinOp::Sub => {
                    let lt = self.infer(sc, l);
                    let rt = self.infer(sc, r);
                    if lt == Type::Char && rt == Type::Char {
                        Type::Int // Char - Char → Int
                    } else if lt == Type::Char || rt == Type::Char {
                        Type::Char // Char - Int → Char
                    } else {
                        promote(lt, rt)
                    }
                }
                BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    promote(self.infer(sc, l), self.infer(sc, r))
                }
            },
            Expr::Call { name, args, .. } => match name.as_str() {
                "println" | "print" => Type::Unit,
                "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" | "ArrayList"
                | "mapOf" | "mutableMapOf" | "hashMapOf" | "emptyMap" | "setOf"
                | "mutableSetOf" | "hashSetOf" | "linkedSetOf" | "sortedSetOf" | "emptySet"
                | "HashSet" | "LinkedHashSet" | "TreeSet" | "HashMap" | "LinkedHashMap"
                | "arrayOf" | "intArrayOf" | "doubleArrayOf" | "booleanArrayOf" | "charArrayOf"
                | "IntArray" | "DoubleArray" | "BooleanArray" | "CharArray" | "Array" => Type::Obj,
                // `Pair`/`Triple`/`Result` are heap objects, and saying so is
                // what routes `==` on them to STRUCTURAL equality: the native
                // compare would coerce two handles to numbers and answer `true`
                // for any two of them.
                "Pair" if args.len() == 2 => Type::Obj,
                "Triple" if args.len() == 3 => Type::Obj,
                "runCatching" => Type::Obj,
                // A `StringBuilder` is a heap object, and saying so is what
                // keeps `==` on two of them IDENTITY (the JVM's, since
                // `StringBuilder` overrides no `equals`): left unknown, the
                // comparison coerces both handles and answers `true` for any
                // two builders. `buildList` yields a list; `buildString`
                // yields the plain `String` its `toString()` produced.
                "StringBuilder" | "StringBuffer" if args.len() <= 1 => Type::Obj,
                "buildList" | "listOfNotNull" => Type::Obj,
                "buildString" => Type::String,
                // `with(x) { … }` / `run { … }` evaluate to their block, whose
                // type the frontend does not track.
                "with" | "run" => Type::Unknown,
                _ if self.classes.contains_key(name) => Type::Obj, // constructor
                // A math call keeps its `Int` overload for integral arguments —
                // this is what makes `abs(-7) / 2` truncate rather than divide.
                _ if self.resolve_math_fn(name).is_some() => {
                    let tys: Vec<Type> = args.iter().map(|a| self.infer(sc, a)).collect();
                    math_ret_type(&self.resolve_math_fn(name).unwrap(), &tys)
                }
                _ => {
                    // A call through a binding whose function type was written
                    // down yields that type. Checked before the `fun` tables
                    // because a local of the same name shadows them, and it is
                    // the only place the result width survives — the value
                    // itself is an untyped closure handle.
                    if sc.slot(name).is_some() {
                        let r = sc.fn_ret_of(name);
                        if r != Type::Unknown {
                            return r;
                        }
                    }
                    // An unqualified call inside an extension body is a call on
                    // its receiver (`fun Int.quad() = dbl().dbl()`).
                    if !self.fun_sig.contains_key(name) && sc.slot("this").is_some() {
                        if let Some(t) =
                            self.ext_ret(sc, &Expr::Var("this".into()), name, args.len())
                        {
                            return t.0;
                        }
                    }
                    match self.local_sigs.get(name).or_else(|| self.fun_sig.get(name)) {
                        Some(s) => self.call_ret(sc, s, args),
                        None => Type::Unknown,
                    }
                }
            },
            Expr::Index { recv, .. } => self.index_elem_ty(sc, recv),
            Expr::Pair { .. } => Type::Obj,
            // A range and an array are heap objects; `in` is a predicate.
            Expr::Range { .. } | Expr::Step { .. } => Type::Obj,
            Expr::In { .. } | Expr::Is { .. } => Type::Boolean,
            Expr::As { ty, safe, .. } => cast_type(ty, *safe),
            // `++`/`--` keep the target's type, so `d++` on a `Double` stays a
            // `Double` for display and `/` dispatch.
            Expr::IncDec { target, .. } => self.infer(sc, target),
            Expr::Lambda { .. } => Type::Unknown,
            Expr::Member {
                recv, name, safe, ..
            } => {
                if self.is_java_math(sc, recv) {
                    return Type::Double; // `Math.PI` / `Math.E`
                }
                // A companion constant on a primitive type. It has to agree with
                // the type `compile_member` returns for the same node, or
                // `Int.MAX_VALUE + 1` would be inferred `Unknown` and skip the
                // 32-bit narrowing the emitted code needs.
                if let Expr::Var(tyname) = &**recv {
                    if self.is_type_ref(sc, tyname) {
                        if let Some((_, vty)) = primitive_const(tyname, name) {
                            return vty;
                        }
                    }
                }
                // A property read on a known class yields the property's type —
                // or, for a property declared with one of the class's type
                // VARIABLES, the type argument the receiver's instantiation
                // supplied for it.
                if let Some(cls) = self.infer_class(sc, recv) {
                    // A COMPUTED property (`val d: Int get() = k`) is a
                    // zero-argument method wearing property syntax, and
                    // `compile_member` resolves it as one — before any stored
                    // property of the name. Inference has to look in the same
                    // order, or the two disagree on the width: left to the
                    // fallback below, `C(2000000000).d + C(2000000000).d` was
                    // untyped and skipped the 32-bit wrap the emitted code was
                    // already applying for `C(a).f() + C(b).f()`.
                    if let Some(sig) = self
                        .classes
                        .get(&cls)
                        .and_then(|m| m.methods.get(name))
                        .filter(|s| s.arity == 0)
                    {
                        return nullable_if_safe(self.method_ret(sc, recv, sig, &[]), *safe);
                    }
                    if let Some(p) = self.classes.get(&cls).and_then(|m| m.prop(name)) {
                        let ty = match p.type_param_of {
                            Some(k) => self.type_arg_at(sc, recv, k).ty,
                            None => p.ty,
                        };
                        return nullable_if_safe(ty, *safe);
                    }
                }
                nullable_if_safe(method_ret_type(name), *safe)
            }
            Expr::MethodCall {
                recv,
                name,
                args,
                safe,
                ..
            } => {
                // `super.m()` / `super<T>.m()` — see `super_ret`. Resolved
                // before the member rules below, which have no notion of the
                // `super` receiver and would answer `Unknown` for it.
                if let Expr::Super { qualifier } = &**recv {
                    if let Some(t) = self.super_ret(qualifier.as_deref(), name) {
                        return nullable_if_safe(t, *safe);
                    }
                }
                // An extension function's declared return type. Checked with
                // the same member-first rule `compile_member` applies, so the
                // two agree on the node — an `Int` extension returning `Int` has
                // to be inferable, or arithmetic on its result would skip the
                // 32-bit narrowing the emitted code performs.
                if let Some(t) = self.ext_ret(sc, recv, name, args.len()) {
                    return t.0;
                }
                // `Math.round` returns a `Long`; the rest follow the shared
                // math overload rule.
                if self.is_java_math(sc, recv) {
                    if name == "round" {
                        return Type::Long;
                    }
                    let tys: Vec<Type> = args.iter().map(|a| self.infer(sc, a)).collect();
                    return math_ret_type(name, &tys);
                }
                // A bitwise member keeps the receiver's width, so it must agree
                // with the type `compile_member` returns for the same node.
                if matches!(
                    name.as_str(),
                    "shl" | "shr" | "ushr" | "inv" | "and" | "or" | "xor"
                ) && args.len() == usize::from(name != "inv")
                {
                    match self.infer(sc, recv) {
                        Type::Long => return Type::Long,
                        Type::Int | Type::Unknown => return Type::Int,
                        _ => {}
                    }
                }
                // The members that hand back one ELEMENT keep the receiver's
                // element type, so arithmetic on `it.first()` narrows the same
                // way arithmetic on the element itself does. Only the members
                // that cannot answer `null` are listed: an `…OrNull` result is
                // `T?`, whose display goes through the Kotlin stringifier.
                if matches!(
                    name.as_str(),
                    "first"
                        | "last"
                        | "get"
                        | "elementAt"
                        | "single"
                        | "random"
                        | "max"
                        | "min"
                        | "reduce"
                        | "maxBy"
                        | "minBy"
                ) {
                    let elem = self.infer_elem(sc, recv);
                    if elem != Type::Unknown {
                        return nullable_if_safe(elem, *safe);
                    }
                }
                // A higher-order collection method's result type is fixed.
                if is_coll_hof(name) {
                    return nullable_if_safe(hof_ret_type(name), *safe);
                }
                if let Some(cls) = self.infer_class(sc, recv) {
                    if let Some(sig) = self.classes.get(&cls).and_then(|m| m.methods.get(name)) {
                        return nullable_if_safe(self.method_ret(sc, recv, sig, args), *safe);
                    }
                }
                nullable_if_safe(method_ret_type(name), *safe)
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

    /// The static element type of `recv[i]`.
    ///
    /// A `String` indexes to a `Char`, and the primitive array factories carry
    /// their element type in their name — which is what makes `ia[0] + 1` wrap
    /// at 32 bits and `ia[0] / 2` divide as integers. A `List`/`Map`/`Array`
    /// element can be anything, so it stays `Unknown` and skips both.
    fn index_elem_ty(&self, sc: &Scope, recv: &Expr) -> Type {
        if self.infer(sc, recv).is_str() {
            return Type::Char;
        }
        match self.infer_class(sc, recv).as_deref() {
            Some("IntArray") => Type::Int,
            Some("DoubleArray") => Type::Double,
            Some("CharArray") => Type::Char,
            Some("BooleanArray") => Type::Boolean,
            // `xs[i]` is one element, so it has the receiver's element type.
            // A `Map` is excluded: `m[k]` is `V?`, not an element of the
            // entry sequence `infer_elem` describes.
            _ => self.infer_elem(sc, recv),
        }
    }

    /// The class/container name of an expression's value, when statically known:
    /// The static ELEMENT type of a sequence-valued expression — what a lambda
    /// over it, or a `for` variable bound to it, receives. `Unknown` when the
    /// frontend cannot see the elements.
    ///
    /// This is the static answer to a question that has no runtime one: every
    /// Kotlin integer is one `i64` at run time, so an `Int` element and a `Long`
    /// element are indistinguishable once inside a lambda — but Kotlin decides
    /// arithmetic width from the STATIC type, and that type is written right
    /// there in the receiver. Reading it here is what lets the per-site 32-bit
    /// narrowing fire inside a lambda instead of conservatively keeping 64 bits.
    ///
    /// Deliberately NOT covered: `map`/`flatMap`, whose element type is the
    /// lambda's return type and would need the lambda lowered first, and any
    /// receiver reached through a function return. Those keep `Unknown` and the
    /// conservative width.
    /// The CLASS of what iterating `e` yields, where the expression names it.
    ///
    /// [`Compiler::infer_elem`] answers the coarse type, which is `Obj` for every
    /// class alike; a `for` variable needs the class itself, or a property read
    /// on it (`for (c in Color.values()) c.rgb`) has no declared type and its
    /// arithmetic would not narrow — `rgb / 2` would divide as `Double`.
    fn infer_elem_class(&self, sc: &Scope, e: &Expr) -> Option<String> {
        match e {
            // `E.values()` / `E.entries` yield `E`. Both are synthesized by the
            // enum lowering onto the companion, so neither carries a declared
            // element type of its own.
            Expr::MethodCall { recv, name, .. } if name == "values" => match &**recv {
                Expr::Var(cls) if self.classes.get(cls).is_some_and(|m| m.is_enum) => {
                    Some(cls.clone())
                }
                _ => None,
            },
            Expr::Member { recv, name, .. } if name == "entries" => match &**recv {
                Expr::Var(cls) if self.classes.get(cls).is_some_and(|m| m.is_enum) => {
                    Some(cls.clone())
                }
                _ => None,
            },
            // A literal collection whose elements agree on a class.
            Expr::Call { name, args, .. }
                if matches!(
                    name.as_str(),
                    "listOf"
                        | "setOf"
                        | "mutableListOf"
                        | "mutableSetOf"
                        | "arrayOf"
                        | "listOfNotNull"
                        | "sequenceOf"
                ) =>
            {
                let mut it = args.iter().map(|a| self.infer_class(sc, a));
                let first = it.next().flatten()?;
                it.all(|c| c.as_deref() == Some(first.as_str()))
                    .then_some(first)
            }
            // The members that re-emit their receiver's own elements.
            Expr::MethodCall { recv, name, .. }
                if matches!(
                    name.as_str(),
                    "filter"
                        | "filterNot"
                        | "sorted"
                        | "sortedDescending"
                        | "sortedBy"
                        | "sortedByDescending"
                        | "sortedWith"
                        | "reversed"
                        | "take"
                        | "takeLast"
                        | "drop"
                        | "dropLast"
                        | "distinct"
                        | "toList"
                        | "toMutableList"
                        | "toSet"
                ) =>
            {
                self.infer_elem_class(sc, recv)
            }
            _ => None,
        }
    }

    fn infer_elem(&self, sc: &Scope, e: &Expr) -> Type {
        match e {
            Expr::Var(n) => sc.elem_of(n),
            // A range's elements are its endpoints' type: `Int`, or `Char` for
            // `'a'..'e'`.
            Expr::Range { start, .. } => match self.infer(sc, start) {
                Type::Char => Type::Char,
                Type::Long => Type::Long,
                _ => Type::Int,
            },
            Expr::Step { recv, .. } => self.infer_elem(sc, recv),
            Expr::Call { name, args, .. } => match name.as_str() {
                "listOf" | "setOf" | "mutableListOf" | "mutableSetOf" | "arrayOf"
                | "listOfNotNull" | "sequenceOf" => {
                    elem_of_args(&args.iter().map(|a| self.infer(sc, a)).collect::<Vec<_>>())
                }
                "intArrayOf" => Type::Int,
                "longArrayOf" => Type::Long,
                "doubleArrayOf" | "floatArrayOf" => Type::Double,
                "charArrayOf" => Type::Char,
                "booleanArrayOf" => Type::Boolean,
                _ => Type::Unknown,
            },
            // The members that re-emit their receiver's own elements. `sorted`
            // and friends reorder, `filter`/`take`/`drop` select — none of them
            // changes what an element IS.
            Expr::MethodCall { recv, name, .. } => match name.as_str() {
                "filter" | "filterNot" | "filterIndexed" | "filterNotNull" | "sorted"
                | "sortedDescending" | "sortedBy" | "sortedByDescending" | "sortedWith"
                | "reversed" | "asReversed" | "take" | "takeLast" | "takeWhile" | "drop"
                | "dropLast" | "dropWhile" | "distinct" | "toList" | "toMutableList" | "toSet"
                | "toMutableSet" | "shuffled" | "slice" | "subList" | "plus" | "minus"
                | "union" | "intersect" | "subtract" => self.infer_elem(sc, recv),
                _ => Type::Unknown,
            },
            _ => Type::Unknown,
        }
    }

    /// The parameter types a collection HOF hands its lambda, given the
    /// receiver's element type and the leading value arguments. Anything not
    /// listed takes the element type for a single parameter, which is the shape
    /// of nearly every `Iterable` member.
    fn hof_param_types(
        &self,
        sc: &Scope,
        recv: &Expr,
        name: &str,
        elem: Type,
        extras: &[Expr],
    ) -> Vec<(Type, Type)> {
        let first_extra = || extras.first().map(|e| self.infer(sc, e));
        // What an ELEMENT's own elements are, for a lambda that goes one level
        // deeper (`listOf(listOf(1)).map { l -> l.map { … } }`).
        let inner = self.infer_inner_elem(sc, recv);
        let one = |t: Type| vec![(t, inner)];
        match name {
            // `fold(initial) { acc, e }` — the accumulator starts at the
            // initial value's type and, for the fold to be well-typed, stays
            // there.
            "fold" => vec![
                (first_extra().unwrap_or(Type::Unknown), Type::Unknown),
                (elem, inner),
            ],
            "reduce" => vec![(elem, inner), (elem, inner)],
            // The index-first pairs.
            "mapIndexed" | "filterIndexed" | "forEachIndexed" => {
                vec![(Type::Int, Type::Unknown), (elem, inner)]
            }
            // `zip(other) { a, b }` — one element from each side.
            "zip" => vec![
                (elem, inner),
                (
                    extras
                        .first()
                        .map(|e| self.infer_elem(sc, e))
                        .unwrap_or(Type::Unknown),
                    Type::Unknown,
                ),
            ],
            // `sortedWith(comparator)`'s lambda compares two elements.
            "sortedWith" => vec![(elem, inner), (elem, inner)],
            // These hand the lambda a GROUP of the receiver's elements, so the
            // group's ELEMENT type is the receiver's element type.
            "chunked" | "windowed" => vec![(Type::Obj, elem)],
            // `getOrElse` hands an index and `mapValues`/`mapKeys` an entry —
            // neither is the element type, so neither is hinted.
            "getOrElse" | "mapValues" | "mapKeys" => Vec::new(),
            _ => one(elem),
        }
    }

    /// The element type of this sequence's ELEMENTS — one level further than
    /// [`Compiler::infer_elem`]. Only a literal spells it out; a sequence
    /// reached through a name keeps a single element type, so a nested list
    /// bound to a `val` answers `Unknown`.
    fn infer_inner_elem(&self, sc: &Scope, e: &Expr) -> Type {
        match e {
            Expr::Call { name, args, .. }
                if matches!(
                    name.as_str(),
                    "listOf" | "setOf" | "mutableListOf" | "mutableSetOf" | "arrayOf"
                ) =>
            {
                elem_of_args(
                    &args
                        .iter()
                        .map(|a| self.infer_elem(sc, a))
                        .collect::<Vec<_>>(),
                )
            }
            _ => Type::Unknown,
        }
    }

    /// a bound variable's class, a constructor call, `this`, a class-typed
    /// function return, or a class-typed property/method result. Drives method
    /// dispatch and property typing.
    /// The declared return type of the supertype implementation that
    /// `super.m()` / `super<T>.m()` resolves to, by the same owner rule
    /// [`Compiler::compile_super_call`] emits with.
    ///
    /// `infer` must agree with what the emitter produces for the node. Without
    /// this a `super` call inferred `Unknown`, so `super<Left>.pick() +
    /// super<Right>.pick()` on two `String`-returning members compiled as
    /// ARITHMETIC rather than concatenation. That went unnoticed because
    /// fusevm's `Op::Add` concatenates two strings anyway — until a genuinely
    /// `Int` operand joined the expression, which earned the whole thing the
    /// 32-bit narrowing and coerced the built string to `0`.
    fn super_ret(&self, qualifier: Option<&str>, name: &str) -> Option<Type> {
        let meta = self.classes.get(self.cur_class.as_ref()?)?;
        let owner = match qualifier {
            Some(t) => t.to_string(),
            None => meta.mro[1..]
                .iter()
                .find(|a| {
                    self.classes
                        .get(*a)
                        .is_some_and(|m| m.own_methods.contains(name))
                })
                .cloned()?,
        };
        self.classes
            .get(&owner)
            .and_then(|m| m.methods.get(name))
            .map(|s| s.ret)
    }

    /// Whether `e`'s static class declares the operator-convention method
    /// `name` — `operator fun plus`, `contains`, `get`, `compareTo`, …
    ///
    /// `ClassMeta::methods` is flattened over the MRO, so a convention a
    /// supertype declares counts for the subclass, as it does in Kotlin. The
    /// built-in collection tags [`Compiler::infer_class`] also answers
    /// (`"List"`, `"Map"`, …) are not user classes and never match here; they
    /// reach their conventions through the runtime dispatch instead.
    /// The type a call to `sig` yields, resolving a type-variable result from
    /// the argument that supplies the type argument (see
    /// [`crate::ast::FunDecl::ret_type_param_of`]).
    ///
    /// Both the emitter and [`Compiler::infer`] go through this, because they
    /// must agree on the result's width: the emitter's answer decides whether
    /// `-id(x)` narrows to 32 bits, and `infer`'s decides whether `id(a) + id(b)`
    /// does.
    ///
    /// A NAMED argument is not resolved — `f(b = 1, a = "x")` does not carry its
    /// types positionally, and guessing from the wrong one would be worse than
    /// leaving the call untyped.
    fn call_ret(&self, sc: &Scope, sig: &FnSig, args: &[Expr]) -> Type {
        if sig.ret != Type::Unknown {
            return sig.ret;
        }
        match sig.ret_type_param_of {
            Some(i) if i < args.len() && !args.iter().any(|a| matches!(a, Expr::Named { .. })) => {
                self.infer(sc, &args[i])
            }
            _ => sig.ret,
        }
    }

    /// The type a call to METHOD `sig` on `recv` yields.
    ///
    /// A type-variable result is resolved from the receiver's type argument
    /// first (`Box(65536).get()` is an `Int` because `Box(65536)` fixed `T`),
    /// then from the argument that carries the variable, exactly as
    /// [`Compiler::call_ret`] does for a free function. The receiver comes first
    /// because a method's own `<T>` list shadows nothing: a variable that
    /// resolves both ways resolves to the same type either way, and the receiver
    /// is the only source when the method takes no arguments.
    fn method_ret(&self, sc: &Scope, recv: &Expr, sig: &FnSig, args: &[Expr]) -> Type {
        if sig.ret != Type::Unknown {
            return sig.ret;
        }
        if let Some(k) = sig.ret_class_type_param_of {
            let t = self.type_arg_at(sc, recv, k).ty;
            if t != Type::Unknown {
                return t;
            }
        }
        self.call_ret(sc, sig, args)
    }

    /// The `k`th type argument of `recv`'s class, or the unknown join when the
    /// receiver's instantiation could not be resolved.
    fn type_arg_at(&self, sc: &Scope, recv: &Expr, k: usize) -> TypeArg {
        self.gen_ty(sc, recv)
            .args
            .get(k)
            .cloned()
            .unwrap_or_else(TypeArg::unknown)
    }

    /// The full generic type of `e` — its coarse type, its class, and the type
    /// arguments that class was instantiated with.
    ///
    /// This is [`Compiler::infer`] plus the type arguments, and it exists
    /// because a type VARIABLE has no width of its own: the width belongs to the
    /// instantiation, and only a walk back to the construction site can find it.
    ///
    /// The walk terminates because every recursive step moves to a strictly
    /// smaller expression (a receiver or an argument), and the fallback below
    /// re-enters `infer`/`infer_class` on `e` only for the node kinds
    /// [`Compiler::gen_ty_generic`] does NOT handle — so `infer`'s own member
    /// rules, which call back in on the RECEIVER, cannot cycle.
    fn gen_ty(&self, sc: &Scope, e: &Expr) -> TypeArg {
        if let Some(t) = self.gen_ty_generic(sc, e) {
            return t;
        }
        TypeArg::plain(self.infer(sc, e), self.infer_class(sc, e))
    }

    /// The type-argument-carrying cases of [`Compiler::gen_ty`]: a binding that
    /// holds a generic instance, a construction, and a read of a type-variable
    /// member. `None` for everything else, which the caller answers from the
    /// ordinary coarse inference.
    fn gen_ty_generic(&self, sc: &Scope, e: &Expr) -> Option<TypeArg> {
        match e {
            Expr::Var(n) => {
                let args = sc.type_args_of(n);
                if !args.is_empty() {
                    return Some(TypeArg {
                        ty: sc.ty(n),
                        class: sc.class_of(n),
                        args,
                    });
                }
                // A top-level property, whose annotation is the only place its
                // arguments were ever written (`val b: Box<Int> = mk()`). Only
                // consulted when the name is not a local, so a shadowing binding
                // still answers for itself.
                if sc.slot(n).is_some() {
                    return None;
                }
                let g = self.globals.get(n)?;
                Some(TypeArg {
                    ty: g.ty,
                    class: g.class.clone(),
                    args: nonempty(g.type_args.clone())?,
                })
            }
            Expr::Call { name, args, .. } if self.classes.contains_key(name) => Some(TypeArg {
                ty: Type::Obj,
                class: Some(name.clone()),
                args: self.ctor_type_args(sc, name, args),
            }),
            // A call to a user function whose RETURN annotation wrote its type
            // arguments: `fun mk(): Box<Int>` makes `mk().v * 2000000000` `Int`
            // arithmetic, with nothing at the call site to say so.
            Expr::Call { name, .. } => {
                let sig = self
                    .local_sigs
                    .get(name)
                    .or_else(|| self.fun_sig.get(name))?;
                Some(TypeArg {
                    ty: sig.ret,
                    class: sig.ret_class.clone(),
                    args: nonempty(sig.ret_type_args.clone())?,
                })
            }
            // A stored `T`-typed property, a property whose annotation wrote its
            // arguments, or a computed one — the zero-argument method a
            // `val d: T get() = …` lowers to.
            Expr::Member { recv, name, .. } => {
                if let Some(p) = self.member_prop(sc, recv, name) {
                    if let Some(k) = p.type_param_of {
                        return Some(self.type_arg_at(sc, recv, k));
                    }
                    if !p.type_args.is_empty() {
                        return Some(TypeArg {
                            ty: p.ty,
                            class: p.class.clone(),
                            args: p.type_args.clone(),
                        });
                    }
                }
                let cls = self.infer_class(sc, recv)?;
                let sig = self.classes.get(&cls)?.methods.get(name)?;
                if sig.arity != 0 {
                    return None;
                }
                self.generic_result(sc, recv, sig, &[])
            }
            Expr::MethodCall {
                recv, name, args, ..
            } => {
                // Only a method the receiver's own class declares — an
                // extension or a stdlib member reaches none of this.
                let cls = self.infer_class(sc, recv)?;
                let sig = self.classes.get(&cls)?.methods.get(name)?;
                self.generic_result(sc, recv, sig, args)
            }
            // `x as Box<Int>` — the JVM erases the argument, but the cast's
            // STATIC type is what the width downstream is read off.
            Expr::As {
                ty,
                type_args,
                safe: false,
                ..
            } if !type_args.is_empty() => Some(TypeArg {
                ty: Type::Obj,
                class: Some(ty.clone()),
                args: type_args.clone(),
            }),
            _ => None,
        }
    }

    /// The generic type a call to `sig` on `recv` yields, from whichever of the
    /// three sources named it: the arguments the return annotation WROTE, the
    /// receiver's type argument for a `T`-typed result, or the argument that
    /// carries the result's type variable.
    ///
    /// The written annotation comes first because it is the only one that is
    /// concrete on its own; the other two are positional and resolve to
    /// `Unknown` wherever the instantiation could not be traced.
    fn generic_result(
        &self,
        sc: &Scope,
        recv: &Expr,
        sig: &FnSig,
        args: &[Expr],
    ) -> Option<TypeArg> {
        if !sig.ret_type_args.is_empty() {
            return Some(TypeArg {
                ty: sig.ret,
                class: sig.ret_class.clone(),
                args: sig.ret_type_args.clone(),
            });
        }
        if sig.ret != Type::Unknown {
            return None;
        }
        if let Some(k) = sig.ret_class_type_param_of {
            let t = self.type_arg_at(sc, recv, k);
            if t.ty != Type::Unknown {
                return Some(t);
            }
        }
        // A type variable the ARGUMENTS supply, the shape `Compiler::call_ret`
        // resolves for a free function.
        let i = sig.ret_type_param_of?;
        let a = args.get(i)?;
        if args.iter().any(|a| matches!(a, Expr::Named { .. })) {
            return None;
        }
        Some(self.gen_ty(sc, a))
    }

    /// The stored property `name` of `recv`'s class, or `None` when the
    /// receiver's class is not known or declares no such property.
    fn member_prop(&self, sc: &Scope, recv: &Expr, name: &str) -> Option<&PropMeta> {
        let cls = self.infer_class(sc, recv)?;
        self.classes.get(&cls)?.prop(name)
    }

    /// The type arguments a construction site `Class(args)` fixes, by matching
    /// each argument against the type variable its constructor parameter was
    /// declared with.
    ///
    /// The parameter list matched is the one [`Compiler::select_ctor`] answers,
    /// so a call that runs a SECONDARY constructor reads its type argument off
    /// that constructor's parameters rather than off the primary's — the two
    /// need not agree on what any position means, and asking the selector is the
    /// only way to know which is which.
    ///
    /// Otherwise deliberately conservative — an unresolved position stays
    /// `Unknown`, which is what the frontend answered for every position before
    /// this existed:
    ///
    /// * NAMED arguments defeat the positional match, exactly as they do in
    ///   [`Compiler::call_ret`].
    /// * Two parameters naming the same variable must agree on the type they
    ///   supply; Kotlin would infer their common supertype, which the coarse
    ///   type system cannot name.
    fn ctor_type_args(&self, sc: &Scope, class: &str, args: &[Expr]) -> Vec<TypeArg> {
        let Some(meta) = self.classes.get(class) else {
            return Vec::new();
        };
        if meta.type_param_count == 0 {
            return Vec::new();
        }
        let mut out = vec![TypeArg::unknown(); meta.type_param_count];
        if args.iter().any(|a| matches!(a, Expr::Named { .. })) {
            return out;
        }
        let (_, params) = self.select_ctor(sc, meta, args);
        let mut seen = vec![false; meta.type_param_count];
        for (p, a) in params.iter().zip(args) {
            let Some(k) = p.type_param_of.filter(|k| *k < out.len()) else {
                continue;
            };
            let t = self.gen_ty(sc, a);
            if seen[k] && out[k] != t {
                out[k] = TypeArg::unknown();
                continue;
            }
            seen[k] = true;
            out[k] = t;
        }
        out
    }

    fn declares_operator(&self, sc: &Scope, e: &Expr, name: &str) -> bool {
        self.infer_class(sc, e)
            .and_then(|c| self.classes.get(&c))
            .is_some_and(|m| {
                m.methods.contains_key(name)
                    // Every enum inherits `Comparable<E>.compareTo` from `Enum`,
                    // ordering by `ordinal`. The member is real but has no
                    // declaration to find here — it is implemented host-side for
                    // every enum at once — so the convention is recognized by the
                    // kind rather than by a method table entry.
                    || (m.is_enum && name == "compareTo")
            })
    }

    /// Emit the trailing iteration-order spec a `Set`/`Map` builder passes to
    /// its host builtin — see [`COLL_HASH`].
    ///
    /// The JVM class the builder names decides how the result ITERATES, and
    /// only the call site knows it: every one of them lands in the same
    /// `HeapObj::Map`/`Set`. `HashMap()` and `HashMap(other)` differ too, and a
    /// runtime look at the arguments cannot separate them from `hashMapOf` —
    /// `hashSetOf(listOf(1))` is a set holding a list, not a copy of one.
    fn emit_coll_spec(&mut self, name: &str, argc: usize) {
        let order = match name {
            "hashSetOf" | "HashSet" | "hashMapOf" | "HashMap" => COLL_HASH,
            "sortedSetOf" | "TreeSet" => COLL_SORTED,
            _ => 0,
        };
        // The JVM constructors take a collection to copy, or nothing at all.
        // The Kotlin builders take their elements.
        let ctor = matches!(
            name,
            "HashSet" | "LinkedHashSet" | "TreeSet" | "HashMap" | "LinkedHashMap"
        );
        let shape = match (ctor, argc) {
            (true, 0) => COLL_DEFAULT_CAP,
            (true, _) => COLL_COPY,
            _ => 0,
        };
        self.b.emit(Op::LoadInt(i64::from(order | shape)), 0);
    }

    /// Emit the runtime operator-convention dispatch for a heap receiver whose
    /// class the frontend could not name — a `List`/`Set`/`Map`/range.
    /// Stack: `[lhs, rhs, nameStr]`; see [`KT_OPER_VM`].
    fn emit_operator_call(
        &mut self,
        sc: &mut Scope,
        l: &Expr,
        r: &Expr,
        name: &str,
    ) -> Result<(), String> {
        self.compile_expr(sc, l)?;
        self.compile_expr(sc, r)?;
        let n = self.b.add_constant(Value::str(name));
        self.b.emit(Op::LoadConst(n), 0);
        self.b.emit(Op::CallBuiltin(KT_OPER_VM, 3), 0);
        Ok(())
    }

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
                if let Some(c) = self.globals.get(n).and_then(|p| p.class.clone()) {
                    return Some(c);
                }
                // An `object` singleton referenced by name.
                if self.classes.get(n).is_some_and(|m| m.is_object) {
                    return Some(n.clone());
                }
                // A class name in receiver position IS its companion object, and
                // `compile_member` rewrites the node that way. Inference has to
                // agree, or `Owner.a == Owner.b` would infer `Unknown` on both
                // sides and compile to the NATIVE equality — comparing two heap
                // handles numerically instead of as objects.
                if let Some(comp) = self.companion_of(n) {
                    return Some(comp);
                }
                None
            }
            Expr::Call { name, .. } => {
                if self.classes.contains_key(name) {
                    return Some(name.clone()); // constructor
                }
                match name.as_str() {
                    "listOf" | "mutableListOf" | "arrayListOf" | "emptyList" | "ArrayList" => {
                        return Some("List".to_string())
                    }
                    "setOf" | "mutableSetOf" | "hashSetOf" | "linkedSetOf" | "sortedSetOf"
                    | "emptySet" | "HashSet" | "LinkedHashSet" | "TreeSet" => {
                        return Some("Set".to_string())
                    }
                    "mapOf" | "mutableMapOf" | "hashMapOf" | "emptyMap" | "HashMap"
                    | "LinkedHashMap" => return Some("Map".to_string()),
                    // The primitive array factories, whose name states the
                    // element type. `arrayOf`/`Array` are deliberately absent:
                    // their elements are unconstrained.
                    "runCatching" => return Some("Result".to_string()),
                    "Pair" => return Some("Pair".to_string()),
                    "Triple" => return Some("Triple".to_string()),
                    "intArrayOf" | "IntArray" => return Some("IntArray".to_string()),
                    "doubleArrayOf" | "DoubleArray" => return Some("DoubleArray".to_string()),
                    "charArrayOf" | "CharArray" => return Some("CharArray".to_string()),
                    "booleanArrayOf" | "BooleanArray" => return Some("BooleanArray".to_string()),
                    _ => {}
                }
                self.fun_sig.get(name).and_then(|s| s.ret_class.clone())
            }
            Expr::Member { recv, name, .. } => {
                // A `T`-typed property names whatever class the receiver's type
                // argument does — `Box(Person("a")).v.name` has to reach
                // `Person` for the read to dispatch as one.
                if let Some(k) = self
                    .member_prop(sc, recv, name)
                    .and_then(|p| p.type_param_of)
                {
                    return self.type_arg_at(sc, recv, k).class;
                }
                let cls = self.infer_class(sc, recv)?;
                let meta = self.classes.get(&cls)?;
                // A computed property, resolved as the zero-argument method it
                // is — the same member-first order `infer` and `compile_member`
                // apply to the node.
                if let Some(sig) = meta.methods.get(name).filter(|s| s.arity == 0) {
                    if let Some(k) = sig
                        .ret_class_type_param_of
                        .filter(|_| sig.ret == Type::Unknown)
                    {
                        let t = self.type_arg_at(sc, recv, k);
                        if t.class.is_some() {
                            return t.class;
                        }
                    }
                    if sig.ret_class.is_some() {
                        return sig.ret_class.clone();
                    }
                }
                meta.prop(name).and_then(|p| p.class.clone())
            }
            Expr::MethodCall {
                recv, name, args, ..
            } => {
                if let Some((_, cls)) = self.ext_ret(sc, recv, name, args.len()) {
                    return cls;
                }
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
                    // The scope functions that hand back the RECEIVER keep its
                    // class, which is what makes `Box(1, 2).apply { … }.area()`
                    // dispatch as a `Box` rather than fall through to the host.
                    "apply" | "also" | "takeIf" | "takeUnless" => {
                        return self.infer_class(sc, recv)
                    }
                    "runCatching" => return Some("Result".to_string()),
                    _ => {}
                }
                let cls = self.infer_class(sc, recv)?;
                let sig = self.classes.get(&cls).and_then(|m| m.methods.get(name))?;
                // Same rule as the property read above, for a method whose
                // declared result is one of the class's type variables.
                if let Some(k) = sig
                    .ret_class_type_param_of
                    .filter(|_| sig.ret == Type::Unknown)
                {
                    let t = self.type_arg_at(sc, recv, k);
                    if t.class.is_some() {
                        return t.class;
                    }
                }
                sig.ret_class.clone()
            }
            Expr::NotNull(inner) => self.infer_class(sc, inner),
            Expr::Elvis { left, .. } => self.infer_class(sc, left),
            // `x as Person` — naming a static type the expression did not
            // already have is the whole point of the cast, so the class it names
            // is the class of the result. [`Compiler::infer`] already answers
            // `Obj` here through `cast_type`; without this the CLASS half stayed
            // unknown, so a member read through a cast dispatched dynamically
            // and no type argument could be read off it.
            //
            // Only the non-`?` form: `x as? Person` may yield null, whose class
            // is nothing, and every read through it is a safe call whose result
            // this would over-narrow.
            Expr::As {
                ty, safe: false, ..
            } if Type::from_name(ty) == Type::Unknown => Some(ty.clone()),
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
        // `abs`/`max`/`min` keep their argument's width, so a `Long` argument
        // selects the `Long` overload and the result must not narrow.
        _ if args.contains(&Type::Long) => Type::Long,
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

/// The element type of an `Iterable`-shaped literal's arguments: the common
/// type when they agree, `Unknown` when they do not (a heterogeneous
/// `listOf(1, "a")` types as neither).
fn elem_of_args(types: &[Type]) -> Type {
    let mut acc: Option<Type> = None;
    for t in types {
        acc = Some(join_ty(acc, *t));
    }
    acc.unwrap_or(Type::Unknown)
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
/// Bind a call's arguments to `params` by position and by name, yielding one
/// slot per parameter: `Some(expr)` where an argument supplied it, `None` where
/// none did. Whether a `None` is legal is the caller's call — `copy` keeps the
/// receiver's value there, a constructor or `fun` requires every slot filled.
///
/// Kotlin's rule is that positional arguments come first and every named one
/// binds a distinct parameter, both of which are enforced here: a mixed-up order
/// or a duplicate/unknown name is a compile error, never a silent misbinding.
/// The static type a cast target name gives its result. A user class or an
/// unmodelled type is a heap object; the named primitives keep their width,
/// which is the whole point of writing the cast.
///
/// A failing `as?` yields null, so a safe cast to `String` is a *nullable*
/// String — the distinction the display path needs to render the four
/// characters `null` rather than the empty string.
fn cast_type(ty: &str, safe: bool) -> Type {
    match Type::from_name(ty) {
        Type::String if safe => Type::NullableString,
        Type::Unknown => Type::Obj,
        t => t,
    }
}

/// True when any STATEMENT reachable from `body` satisfies `f` — the statement
/// twin of [`body_any`], with the same reach: nested loop bodies, branch arms,
/// `try` sections, and the bodies of lambdas written in expression position.
/// The expression-borne blocks are found through [`body_any`] itself, so the two
/// visitors stay in step as the AST grows.
fn stmt_any(body: &[Stmt], f: &dyn Fn(&StmtKind) -> bool) -> bool {
    body.iter().any(|s| {
        f(&s.kind)
            || match &s.kind {
                StmtKind::While { body, .. }
                | StmtKind::DoWhile { body, .. }
                | StmtKind::For { body, .. }
                | StmtKind::ForIn { body, .. } => stmt_any(body, f),
                StmtKind::If(ie) => {
                    stmt_any(&ie.then, f) || ie.els.as_deref().is_some_and(|e| stmt_any(e, f))
                }
                StmtKind::When(w) => w.arms.iter().any(|a| stmt_any(&a.body, f)),
                StmtKind::LocalFun(lf) => stmt_any(&lf.body, f),
                _ => false,
            }
            || body_any(std::slice::from_ref(s), &|e| match e {
                Expr::Lambda { body, .. } => stmt_any(body, f),
                Expr::If(ie) => {
                    stmt_any(&ie.then, f) || ie.els.as_deref().is_some_and(|b| stmt_any(b, f))
                }
                Expr::When(w) => w.arms.iter().any(|a| stmt_any(&a.body, f)),
                Expr::Try(t) => {
                    stmt_any(&t.body, f)
                        || t.catches.iter().any(|c| stmt_any(&c.body, f))
                        || stmt_any(&t.finally_body, f)
                }
                _ => false,
            })
    })
}

/// Every name a lambda ANYWHERE inside `body` assigns to.
///
/// A `var` of the enclosing frame named here has to be boxed: a closure copies
/// its captures by value, so a plain slot write inside the lambda would update
/// the copy and leave the original untouched — a wrong answer rather than a
/// loud one. Over-approximating (a name that turns out to be the lambda's own
/// local, or a `val`) costs one heap cell and nothing else.
fn lambda_writes(body: &[Stmt]) -> HashSet<String> {
    let out = RefCell::new(HashSet::new());
    let note = |e: &Expr| {
        if let Expr::IncDec { target, .. } = e {
            if let Expr::Var(n) = &**target {
                out.borrow_mut().insert(n.clone());
            }
        }
        false
    };
    body_any(body, &|e| {
        if let Expr::Lambda { body, .. } = e {
            stmt_any(body, &|k| {
                if let StmtKind::Assign { name, .. } = k {
                    out.borrow_mut().insert(name.clone());
                }
                false
            });
            // `x++` / `--x` are writes too, and reach their target through an
            // expression rather than an `Assign`.
            body_any(body, &note);
        }
        false
    });
    out.into_inner()
}

/// Whether a parameter list can accept arguments of these coarse types.
///
/// Only a CONFLICT rejects: both the parameter and the argument must have a
/// known primitive type and they must disagree. `Unknown` on either side is
/// silent, and `Int`/`Long` are interchangeable because an integer literal
/// takes whichever width the parameter declares. Anything the coarse type
/// system cannot tell apart is therefore accepted, which keeps this a
/// tie-breaker rather than a type checker.
fn params_accept(params: &[Param], args: &[Type]) -> bool {
    fn primitive(t: Type) -> bool {
        matches!(
            t,
            Type::Int | Type::Long | Type::Double | Type::Boolean | Type::Char | Type::String
        )
    }
    !params.iter().zip(args).any(|(p, a)| {
        let (want, got) = (p.ty, *a);
        primitive(want)
            && primitive(got)
            && want != got
            && !matches!(
                (want, got),
                (Type::Int, Type::Long) | (Type::Long, Type::Int)
            )
    })
}

/// Whether `name` is one of the receiver-taking scope functions.
fn is_scope_fn(name: &str) -> bool {
    matches!(
        name,
        "let" | "also" | "takeIf" | "takeUnless" | "run" | "apply"
    )
}

/// Whether the scope function binds the receiver as the block's `this` (rather
/// than as the parameter `it`). `with` is the free-function spelling of `run`.
fn is_recv_scope_fn(name: &str) -> bool {
    matches!(name, "run" | "apply" | "with")
}

/// The array literal a `vararg` parameter's collected arguments pack into. The
/// builder is chosen from the declared ELEMENT type so a `vararg xs: Int` binds
/// an `IntArray`, as Kotlin's does.
fn vararg_array(elem: Type, items: &[Expr]) -> Expr {
    let name = match elem {
        Type::Int | Type::Long => "intArrayOf",
        Type::Double => "doubleArrayOf",
        Type::Boolean => "booleanArrayOf",
        Type::Char => "charArrayOf",
        _ => "arrayOf",
    };
    Expr::Call {
        name: name.to_string(),
        args: items.to_vec(),
        line: 0,
    }
}

fn bind_args<'a>(
    callee: &str,
    params: &[String],
    args: &'a [Expr],
) -> Result<Vec<Option<&'a Expr>>, String> {
    let mut slots: Vec<Option<&Expr>> = vec![None; params.len()];
    let mut seen_named = false;
    for (i, a) in args.iter().enumerate() {
        let Expr::Named { name, value } = a else {
            if seen_named {
                return Err(format!(
                    "{callee}: a positional argument cannot follow a named one"
                ));
            }
            match slots.get_mut(i) {
                Some(slot) => *slot = Some(a),
                None => {
                    return Err(format!(
                        "{callee} takes at most {} argument(s), got {}",
                        params.len(),
                        args.len()
                    ))
                }
            }
            continue;
        };
        seen_named = true;
        let at = params
            .iter()
            .position(|p| p == name)
            .ok_or_else(|| format!("{callee} has no parameter named `{name}`"))?;
        if slots[at].is_some() {
            return Err(format!("{callee}: argument for `{name}` given twice"));
        }
        slots[at] = Some(value);
    }
    Ok(slots)
}

/// The companion constant `ty.name` (`Int.MAX_VALUE`, `Double.NaN`), or `None`
/// when the pair is not one of them.
///
/// `Double.MIN_VALUE` and the `Float` bounds are deliberately absent: they are
/// the shortest decimal that round-trips a subnormal `Double` / a 32-bit
/// `Float`, and this frontend carries every floating value as an `f64` and
/// renders it with the `f64` shortest-repr — so it would print
/// `5.0E-324`/`3.4028234663852886E38` where Kotlin prints `4.9E-324`/
/// `3.4028235E38`. Leaving them unresolved keeps the divergence out of running
/// programs.
/// The constant's static type rides along with its value: `Long.MAX_VALUE` is a
/// `Long`, and the difference decides whether the arithmetic around it narrows
/// to 32 bits (`Int.MAX_VALUE + 1` is `-2147483648`, `Long.MAX_VALUE + 1L` is
/// `-9223372036854775808`).
fn primitive_const(ty: &str, name: &str) -> Option<(Value, Type)> {
    let v = match (ty, name) {
        ("Byte", "MAX_VALUE") => (Value::Int(i8::MAX as i64), Type::Int),
        ("Byte", "MIN_VALUE") => (Value::Int(i8::MIN as i64), Type::Int),
        ("Short", "MAX_VALUE") => (Value::Int(i16::MAX as i64), Type::Int),
        ("Short", "MIN_VALUE") => (Value::Int(i16::MIN as i64), Type::Int),
        ("Int", "MAX_VALUE") => (Value::Int(i32::MAX as i64), Type::Int),
        ("Int", "MIN_VALUE") => (Value::Int(i32::MIN as i64), Type::Int),
        ("Long", "MAX_VALUE") => (Value::Int(i64::MAX), Type::Long),
        ("Long", "MIN_VALUE") => (Value::Int(i64::MIN), Type::Long),
        ("Double", "MAX_VALUE") => (Value::Float(f64::MAX), Type::Double),
        ("Double" | "Float", "POSITIVE_INFINITY") => (Value::Float(f64::INFINITY), Type::Double),
        ("Double" | "Float", "NEGATIVE_INFINITY") => {
            (Value::Float(f64::NEG_INFINITY), Type::Double)
        }
        ("Double" | "Float", "NaN") => (Value::Float(f64::NAN), Type::Double),
        _ => return None,
    };
    Some(v)
}

// ── Integer width ──────────────────────────────────────────────────────────
//
// Every Kotlin integer runs as one fusevm `i64`, so a 32-bit `Int` result has
// to be narrowed back after each arithmetic op or `Int.MAX_VALUE + 1` would
// print 2147483648 where Kotlin prints -2147483648. The narrowing is emitted
// PER SITE from the operands' static types, which is what lets `Int` and `Long`
// arithmetic coexist in one chunk: a `Long` operand simply skips it and keeps
// the full 64-bit i64 result.

/// The Kotlin function a binary operator is a CONVENTION for.
///
/// Kotlin's operators are not instructions bound to primitive types: `a + b`
/// *means* `a.plus(b)`, resolved as an ordinary member/extension against the
/// LEFT operand. The compiler emits the arithmetic op when it can prove both
/// sides are primitive numbers, and must resolve the convention otherwise —
/// emitting `Op::Sub` for `listOf(1, 2, 3) - 2` coerced the list HANDLE to a
/// number and answered `-2.0`.
///
/// Only the five arithmetic operators are named here. The comparison operators
/// are the separate `compareTo` convention (they answer a `Boolean` about its
/// sign, not the method's own value), and `==` is `equals`, already routed
/// through [`KT_OBJEQ_VM`].
fn operator_fn(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Add => "plus",
        BinOp::Sub => "minus",
        BinOp::Mul => "times",
        BinOp::Div => "div",
        BinOp::Mod => "rem",
        BinOp::Eq | BinOp::Ne | BinOp::RefEq | BinOp::RefNe => return None,
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => return None,
        BinOp::And | BinOp::Or => return None,
    })
}

/// The in-place `op=` convention paired with an arithmetic operator.
///
/// Kotlin resolves `a += b` two ways, and the choice is observable through an
/// alias. Against a `var` it is `a = a.plus(b)`, which REBINDS the name to a
/// fresh object; against a `val` holding a mutable collection it is
/// `a.plusAssign(b)`, which mutates the one object every alias shares. Only the
/// second is legal on a `val`, which is why `val m = mutableListOf(1, 2); m +=
/// 3` compiles at all.
fn operator_assign_fn(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Add => "plusAssign",
        BinOp::Sub => "minusAssign",
        _ => return None,
    })
}

/// Kotlin's binary numeric promotion for `+ - * / %`: a `Double` operand makes
/// the result `Double`, otherwise a `Long` operand makes it a 64-bit `Long`,
/// and everything else is a 32-bit `Int`.
///
/// An operand the frontend could not type leaves the RESULT untyped rather than
/// defaulting it to `Int`. The default is a claim about DISPLAY — an `Int` result
/// renders through the integer coercion — and a type variable instantiated at
/// `Double` then printed `4` where the reference toolchain prints `4.0`.
/// `Unknown` routes the display through the runtime-tagged stringifier, which
/// reads the width off the value instead of guessing it.
fn promote(lt: Type, rt: Type) -> Type {
    if lt == Type::Unknown || rt == Type::Unknown {
        Type::Unknown
    } else if lt == Type::Double || rt == Type::Double {
        Type::Double
    } else if lt == Type::Long || rt == Type::Long {
        Type::Long
    } else {
        Type::Int
    }
}

/// True when a receiver of type `t` cannot be an instance of a user class, so a
/// member call on it needs no runtime class-tag dispatch.
///
/// [`Type::Obj`], [`Type::Unit`] and [`Type::Unknown`] are excluded, and that is
/// the whole point: those are exactly the types a user instance wears, so a
/// receiver carrying one still has to be decided at run time.
fn is_primitive_recv(t: Type) -> bool {
    matches!(
        t,
        Type::Int
            | Type::Long
            | Type::Double
            | Type::Boolean
            | Type::Char
            | Type::String
            | Type::NullableString
    )
}

/// True when `t` is a statically known operand of `Int` width. `Char` counts —
/// it is a 16-bit code unit, so `Char - Char` is inside `Int` either way.
///
/// [`Type::Unknown`] is deliberately excluded: a value whose type the frontend
/// could not resolve may well be a `Long`, and truncating one to 32 bits would
/// be a far worse error than leaving a rare `Int` overflow unwrapped.
/// `Some(v)` when the list has entries, `None` when it is empty — the shape a
/// resolver that must fall through to its next source needs.
fn nonempty(v: Vec<TypeArg>) -> Option<Vec<TypeArg>> {
    (!v.is_empty()).then_some(v)
}

fn is_int_width(t: Type) -> bool {
    matches!(t, Type::Int | Type::Char)
}

/// True when an operation on operands of these types produces a value that must
/// be narrowed to 32 bits: both operands are `Int`-width, so Kotlin computed the
/// result at `Int` precision.
fn narrows_to_int(lt: Type, rt: Type) -> bool {
    is_int_width(lt) && is_int_width(rt)
}

/// Widen a member's result type for a safe call `recv?.m()`, which yields null
/// whenever the receiver is null.
///
/// It matters for exactly the two types whose display skips the Kotlin
/// stringifier: a `String` renders through fusevm's native concat, which writes
/// an absent value as the EMPTY string rather than `null`, and a `Char` renders
/// through the code-unit op. Both widen to a type that stringifies through the
/// host, where `null` is spelled out. Every other type already routes there, so
/// they are returned unchanged and keep their sharper op selection.
///
/// Both the lowering ([`Compiler::compile_safe_member`]) and the coarse
/// inference ([`Compiler::infer`]) go through this, because a `+` picks its
/// display coercion from the INFERRED type of each side — so widening in only
/// one of the two would still print `"v=" + s?.uppercase()` without the `null`.
fn nullable_if_safe(t: Type, safe: bool) -> Type {
    if !safe {
        return t;
    }
    match t {
        Type::String => Type::NullableString,
        Type::Char => Type::Unknown,
        other => other,
    }
}

fn method_ret_type(name: &str) -> Type {
    match name {
        "length" | "code" => Type::Int,
        // The width conversions. `toByte`/`toShort` narrow the VALUE, but their
        // result still takes part in arithmetic at `Int` width (Kotlin promotes
        // both before every operator), so `Int` is their arithmetic type here.
        "toInt" | "toByte" | "toShort" => Type::Int,
        "toLong" => Type::Long,
        "toDouble" | "toFloat" => Type::Double,
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
            | "partition"
            | "takeWhile"
            | "dropWhile"
            | "firstOrNull"
            | "lastOrNull"
            | "forEach"
            | "onEach"
            | "mapNotNull"
            | "flatMapIndexed"
            | "runningFold"
            | "scan"
            | "runningReduce"
            | "scanReduce"
            | "fold"
            | "foldRight"
            | "reduce"
            | "reduceRight"
            | "any"
            | "all"
            | "none"
            | "count"
            | "sumOf"
            | "maxByOrNull"
            | "maxBy"
            | "minByOrNull"
            | "minBy"
            | "sortedBy"
            | "sortedByDescending"
            | "associate"
            | "associateBy"
            | "associateWith"
            | "groupBy"
            | "groupingBy"
            // The searching predicates. Each also has a no-argument member
            // spelling (`list.first()`), which the `!args.is_empty()` guard at
            // the call site keeps on the plain path.
            | "first"
            | "last"
            | "find"
            | "findLast"
            | "single"
            | "singleOrNull"
            | "indexOfFirst"
            | "indexOfLast"
            | "filterIndexed"
            | "forEachIndexed"
            | "maxOf"
            | "minOf"
            | "maxOfOrNull"
            | "minOfOrNull"
            | "mapValues"
            | "mapKeys"
            | "filterKeys"
            | "filterValues"
            | "sortedWith"
    )
}

/// The collection methods that take a lambda in ONE of their overloads and a
/// plain value in the others — `chunked(n)` vs `chunked(n) { … }`. They route to
/// [`crate::host::coll_hof`] only when a lambda is actually written, so the
/// no-lambda spelling keeps reaching the member dispatch.
fn is_optional_hof(name: &str) -> bool {
    matches!(
        name,
        // `trim`/`trimStart`/`trimEnd` belong here rather than in
        // [`is_coll_hof`] because their no-lambda spelling is the whitespace
        // trim, a plain `String` member — only the predicate form iterates.
        "chunked"
            | "windowed"
            | "zip"
            | "joinToString"
            | "getOrElse"
            | "trim"
            | "trimStart"
            | "trimEnd"
            | "zipWithNext"
    )
}

/// What a stdlib member's parameter falls back to when the call omits it.
enum Dflt {
    /// No default — omitting it is an error, as it is on Kotlin.
    Required,
    /// Compile a literal `null`. Every host member whose parameter takes this
    /// reads an absent value as its own default (`padStart`'s `padChar` is a
    /// space, `indexOf`'s `startIndex` is 0).
    Absent,
    Str(&'static str),
    Int(i64),
    Bool(bool),
}

/// The parameter list of a stdlib member that is worth naming arguments on —
/// the ones with several optional parameters, where positional order is the
/// part a reader cannot see.
///
/// Every optional parameter carries its DEFAULT rather than a hole, so a call
/// that names a later parameter and skips an earlier one still compiles to the
/// value Kotlin would have passed. Filling a skipped parameter with `null` and
/// hoping the host reads it as absent is right for some members and silently
/// wrong for others (`windowed`'s `step` would become 0, not 1).
fn builtin_params(name: &str) -> Option<&'static [(&'static str, Dflt)]> {
    Some(match name {
        "joinToString" => &[
            ("separator", Dflt::Str(", ")),
            ("prefix", Dflt::Str("")),
            ("postfix", Dflt::Str("")),
            ("limit", Dflt::Int(-1)),
            ("truncated", Dflt::Str("...")),
        ],
        "windowed" => &[
            ("size", Dflt::Required),
            ("step", Dflt::Int(1)),
            ("partialWindows", Dflt::Bool(false)),
        ],
        "padStart" | "padEnd" => &[("length", Dflt::Required), ("padChar", Dflt::Absent)],
        "indexOf" | "lastIndexOf" => &[("string", Dflt::Required), ("startIndex", Dflt::Absent)],
        "startsWith" => &[("prefix", Dflt::Required), ("startIndex", Dflt::Int(0))],
        "substring" => &[("startIndex", Dflt::Required), ("endIndex", Dflt::Absent)],
        "replace" | "replaceFirst" => &[("oldValue", Dflt::Required), ("newValue", Dflt::Required)],
        "chunked" => &[("size", Dflt::Required)],
        "take" | "drop" | "takeLast" | "dropLast" => &[("n", Dflt::Required)],
        "repeat" => &[("n", Dflt::Required)],
        "coerceIn" => &[
            ("minimumValue", Dflt::Required),
            ("maximumValue", Dflt::Required),
        ],
        _ => return None,
    })
}

/// Rewrite a stdlib member's arguments into positional order, filling every
/// parameter the call did not supply with its default.
///
/// Kotlin's own rules are enforced: positional arguments come first, each name
/// binds a parameter that exists, and no parameter is bound twice.
fn bind_named_builtin(name: &str, args: &[Expr]) -> Result<Vec<Expr>, String> {
    let Some(params) = builtin_params(name) else {
        let named = args
            .iter()
            .find_map(|a| match a {
                Expr::Named { name, .. } => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_default();
        return Err(format!(
            "named argument `{named}` is not supported for this callee"
        ));
    };

    let mut slots: Vec<Option<&Expr>> = vec![None; params.len()];
    let mut seen_named = false;
    for (i, a) in args.iter().enumerate() {
        let Expr::Named { name: arg, value } = a else {
            if seen_named {
                return Err(format!(
                    "{name}: a positional argument cannot follow a named one"
                ));
            }
            if i >= slots.len() {
                return Err(format!("{name}: too many arguments"));
            }
            slots[i] = Some(a);
            continue;
        };
        seen_named = true;
        let Some(at) = params.iter().position(|(p, _)| p == arg) else {
            return Err(format!("{name} has no parameter `{arg}`"));
        };
        if slots[at].is_some() {
            return Err(format!("{name}: parameter `{arg}` is bound twice"));
        }
        slots[at] = Some(value);
    }

    // Trailing defaults are dropped rather than passed: several host members
    // distinguish "absent" from "the default value" (`substring`'s `endIndex`
    // is the receiver's length, which no literal here knows).
    let last = slots.iter().rposition(|s| s.is_some());
    let mut out = Vec::new();
    for (i, (pname, dflt)) in params.iter().enumerate() {
        // Spelled out rather than `Option::is_none_or`, which is stable only
        // since 1.82 — this crate declares 1.80.
        match last {
            None => break,
            Some(l) if i > l => break,
            Some(_) => {}
        }
        out.push(match slots[i] {
            Some(e) => e.clone(),
            None => match dflt {
                Dflt::Required => {
                    return Err(format!("{name}: no value passed for parameter `{pname}`"))
                }
                Dflt::Absent => Expr::Null,
                Dflt::Str(s) => Expr::Str(vec![StrExpr::Text(s.to_string())]),
                Dflt::Int(n) => Expr::Int(*n),
                Dflt::Bool(b) => Expr::Bool(*b),
            },
        });
    }
    Ok(out)
}

/// Whether `e` is a lambda literal — what decides an [`is_optional_hof`] call.
fn is_lambda(e: &Expr) -> bool {
    matches!(e, Expr::Lambda { .. })
}

/// Static result type of a higher-order collection method, for display/`==` op
/// selection. Collection-returning methods yield a heap `Obj`; the rest are
/// coarse (`Unknown` display routes through the generic Kotlin stringifier).
fn hof_ret_type(name: &str) -> Type {
    match name {
        "map" | "mapIndexed" | "flatMap" | "filter" | "filterNot" | "partition" | "takeWhile"
        | "dropWhile" | "sortedBy" | "sortedByDescending" | "associate" | "associateBy"
        | "associateWith" | "groupBy" | "groupingBy" => Type::Obj,
        "filterIndexed" | "mapValues" | "mapKeys" | "sortedWith" | "chunked" | "windowed"
        | "zip" | "filterKeys" | "filterValues" | "zipWithNext" => Type::Obj,
        "forEach" | "forEachIndexed" => Type::Unit,
        "any" | "all" | "none" => Type::Boolean,
        "count" | "indexOfFirst" | "indexOfLast" => Type::Int,
        "joinToString" => Type::String,
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
        StmtKind::LocalFun(lf) => body_any(&lf.body, f),
        StmtKind::Return(Some(e)) => expr_any(e, f),
        StmtKind::Return(None) => false,
        StmtKind::While { cond, body, .. } | StmtKind::DoWhile { cond, body, .. } => {
            expr_any(cond, f) || body_any(body, f)
        }
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
        Expr::Invoke { target, args, .. } => {
            expr_any(target, f) || args.iter().any(|a| expr_any(a, f))
        }
        Expr::Member { recv, .. } => expr_any(recv, f),
        Expr::MethodCall { recv, args, .. } => {
            expr_any(recv, f) || args.iter().any(|a| expr_any(a, f))
        }
        Expr::Named { value, .. } => expr_any(value, f),
        Expr::As { value, .. } => expr_any(value, f),
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
        | Expr::Long(_)
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
    body_any(
        body,
        &|e| matches!(e, Expr::Call { name, .. } if name == RUST_COMPILE),
    )
}

/// True when the program contains a `try` or a `throw` anywhere — in `main`, a
/// free `fun`, a method, or a lambda body. Only such a program pays for the
/// per-statement unwind checks and the suppressible print builtins.
pub fn uses_exceptions(program: &Program) -> bool {
    // `runCatching` catches, so a program containing one needs the pending-slot
    // machinery even with no `try` written anywhere.
    let has = |body: &[Stmt]| {
        body_any(body, &|e| match e {
            Expr::Try(_) | Expr::Throw(_) => true,
            Expr::Call { name, .. } | Expr::MethodCall { name, .. } => name == "runCatching",
            _ => false,
        })
    };
    program.funs.iter().any(|f| has(&f.body))
        || program
            .classes
            .iter()
            .any(|c| c.methods.iter().any(|m| has(&m.body)))
        || program.props.iter().any(|p| {
            body_any(
                std::slice::from_ref(&Stmt::new(0, StmtKind::Expr(p.init.clone()))),
                &|e| matches!(e, Expr::Try(_) | Expr::Throw(_)),
            )
        })
}
