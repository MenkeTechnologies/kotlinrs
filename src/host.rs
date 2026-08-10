//! Kotlin-specific runtime hooks reached through fusevm's extension-op
//! dispatch.
//!
//! fusevm's ops are language-agnostic, so the Kotlin behaviors the universal
//! ops can't express are handled here — the value coercions below, the
//! frontend-owned object heap (`HeapObj`), and the in-flight exception a VM
//! with no unwind opcode cannot carry itself (see “Exception unwinding”):
//!
//! - **`KT_TO_STRING`** — Kotlin display form. fusevm's `Value::to_str` is
//!   Perl-flavored (`Bool` → `"1"`/`""`, whole `Double` → `"1"`); Kotlin needs
//!   `true`/`false` and `1.0`.
//! - **`KT_IDIV` / `KT_IMOD`** — truncating integer `/` and `%`. fusevm's native
//!   `Op::Div` is always-float, and Kotlin `Int` division truncates toward zero
//!   with an `ArithmeticException` on a zero divisor.
//!
//! Integer division by zero stores a message in `KT_ERROR` and halts the VM;
//! the runtime surfaces it as `kotlin: <reason>` on stderr (an uncaught
//! `ArithmeticException`).

use fusevm::{Frame, NumOp, VMResult, Value, VM};
use std::cell::RefCell;

/// Coerce the top of stack to its Kotlin `toString()` form.
pub const KT_TO_STRING: u16 = 1;
/// Truncating integer division (`Int`/`Long` `/`).
pub const KT_IDIV: u16 = 2;
/// Remainder (`%`) with Kotlin sign rules (sign of the dividend).
pub const KT_IMOD: u16 = 3;
/// Per-statement debug line marker (`kotlin --dap` only). Stack-neutral: the
/// normal handler ignores it; the debug handler routes it to the DAP hook. Its
/// `line` rides in `chunk.lines` at the marker op's index.
pub const KT_DBG_LINE: u16 = 4;
/// Compile + register an inline `rust { ... }` FFI block. Pops the base64 block
/// body (a `Str`) and hands it to `fusevm::ffi::compile_and_register`.
pub const KT_FFI_COMPILE: u16 = 5;
/// Call an FFI-exported function by name. The `arg` payload is the argument
/// count; the stack holds the args (deepest first) with the function name (a
/// `Str`) on top. Dispatches through `fusevm::ffi::try_call` and pushes the
/// result.
pub const KT_FFI_CALL: u16 = 6;
/// Dispatch a Kotlin stdlib member/method on a receiver. The `arg` payload is
/// the argument count. Stack layout: `[recv, arg0 .. arg{n-1}, name]` with the
/// method/property name (a `Str`) on top. Pops all, computes the result, and
/// pushes it. Property reads (`"s".length`) dispatch with `n == 0`.
pub const KT_METHOD: u16 = 7;
/// `when`'s `is Type` runtime type check. Stack: `[value, typeName]`; pops both
/// and pushes a `Bool` — whether `value`'s runtime kind matches `typeName`.
pub const KT_IS: u16 = 8;
/// Coerce a `Char` (carried as its integer code unit) to its one-character
/// string form. Pops the code, pushes the `Str`.
pub const KT_CHR_STRING: u16 = 9;
/// Test the top of stack for Kotlin `null` (fusevm `Undef`). Pops the value,
/// pushes a `Bool`. Backs the `?.` / `?:` short-circuit checks.
pub const KT_ISNULL: u16 = 10;
/// Not-null assertion `!!`. Peeks the top of stack: leaves it unchanged when
/// non-null, or raises a `NullPointerException` (halting the VM) when it is
/// `null`.
pub const KT_NOTNULL: u16 = 11;
/// Construct a class instance. Stack: `[metaStr, v0 .. v{n-1}]` (`arg` = field
/// count `n`); `metaStr` is `"Name\x1f(d|c)\x1ffield0\x1f…"`. Pops all, allocates
/// an instance on the host heap, pushes its `Obj` handle.
pub const KT_NEW: u16 = 12;
/// Read a property off an instance. Stack: `[obj, nameStr]`; pushes the value.
pub const KT_GETFIELD: u16 = 13;
/// Write a property on an instance. Stack: `[obj, value, nameStr]`; pops all
/// three, mutates the field, pushes nothing (stack-neutral, statement position).
pub const KT_SETFIELD: u16 = 14;
/// Build a `List` from `arg` stack values `[v0 .. v{n-1}]`; pushes its handle.
pub const KT_LIST: u16 = 15;
/// Build a `Map` from `arg` `Pair` handles `[p0 .. p{n-1}]`; pushes its handle.
pub const KT_MAP: u16 = 16;
/// Build a `Set` from `arg` stack values `[v0 .. v{n-1}]`, keeping the first
/// occurrence of a repeat; pushes its handle.
pub const KT_SET: u16 = 36;
/// Build a `Pair` from `[first, second]`; pushes its handle.
pub const KT_PAIR: u16 = 17;
/// Indexed read `recv[index]`. Stack: `[recv, index]`; pushes the element/value.
pub const KT_INDEX_GET: u16 = 18;
/// Indexed write `recv[index] = value`. Stack: `[recv, index, value]`; pops all
/// three, pushes nothing (stack-neutral).
pub const KT_INDEX_SET: u16 = 19;
/// Allocate an empty (mutable) `List`; pushes its handle. Used as the
/// accumulator when the compiler inlines `.map`/`.filter`.
pub const KT_NEWLIST: u16 = 20;
/// Append to a `List`. Stack: `[list, value]`; pops both, pushes nothing.
pub const KT_LISTPUSH: u16 = 21;
/// Structural equality `a == b` over heap objects (and primitives). Stack:
/// `[a, b]`; pushes a `Bool`.
pub const KT_OBJEQ: u16 = 22;
/// Kotlin floating-point `/`: both operands coerced to `Double` and divided
/// under IEEE-754, so `x / 0.0` is a signed infinity and `0.0 / 0.0` is NaN.
///
/// Emitted when the operands are statically not both `Int`. fusevm's native
/// `Op::Div` cannot serve there — its shell/awk flavour has no infinities and
/// yields `Undef` for a zero divisor, which printed as `null`. `KT_IDIV` cannot
/// either: it branches on the *runtime* representation, so a `Double`-typed
/// value still holding an `Int` would take the integer path and truncate.
pub const KT_DDIV: u16 = 23;
/// Build a range value. Stack: `[start, end]`; the `arg` payload is the
/// [`RangeForm`] discriminant (`0` = `a..b`, `1` = `a until b`, `2` = `a downTo
/// b`). Pushes the range's handle.
pub const KT_RANGE: u16 = 24;
/// `range step n` — re-step a range into an `IntProgression`. Stack:
/// `[range, n]`; pushes a new handle. A non-positive `n` is an
/// `IllegalArgumentException`, as in Kotlin.
pub const KT_RANGE_STEP: u16 = 25;
/// `value in container`. Stack: `[value, container]`; pushes a `Bool`.
pub const KT_IN: u16 = 26;
/// Element count of an iterable (range / `List` / array), for the general
/// `for (v in iterable)` lowering. Stack: `[iterable]`; pushes an `Int`.
pub const KT_ITER_SIZE: u16 = 27;
/// Element `i` of an iterable, for the general `for` lowering. Stack:
/// `[iterable, i]`; pushes the element. `i` is always in range — the loop
/// bounds it with [`KT_ITER_SIZE`].
pub const KT_ITER_GET: u16 = 28;
/// Build an array from `arg` stack values `[v0 .. v{n-1}]`; pushes its handle.
/// The JVM element descriptor (which drives the `[I@…` / `[Ljava.lang.Integer;@…`
/// display form) is inferred from the values.
pub const KT_ARRAY: u16 = 29;
/// Allocate a zero-filled primitive array. Stack: `[n, descStr]` with the JVM
/// descriptor (`"[I"`, `"[D"`, `"[Z"`) on top; pushes its handle.
pub const KT_ARRAY_NEW: u16 = 30;
/// Dispatch a `kotlin.math` / `java.lang.Math` function. The `arg` payload is
/// the argument count. Stack: `[arg0 .. arg{n-1}, nameStr]` with the function
/// name on top; pushes the result.
pub const KT_MATH: u16 = 31;
/// Register a declared type's supertypes. Stack: `[nameStr, supersCsvStr]`;
/// pops both. The runtime consults the table for `is` checks on user classes,
/// `catch` matching of a user class extending a built-in throwable, and the
/// throwable display form. Emitted once per declared class, before `main`.
pub const KT_TYPE_REG: u16 = 32;
/// Build a subclass instance on top of its superclass instance. Stack:
/// `[baseObj, metaStr, v0 .. v{n-1}]` (`arg` = the subclass's OWN field count
/// `n`). The result carries the base's fields followed by the subclass's own,
/// under the subclass's runtime class tag.
pub const KT_EXTEND: u16 = 33;
/// The runtime class tag of a value: the declared name for a class instance /
/// `object` singleton, or the empty string for anything else. Stack: `[value]`;
/// pushes a `Str`. Backs the virtual method dispatch chain the compiler emits.
pub const KT_CLASSOF: u16 = 34;
/// Register a class's `toString()` override. Stack: `[tagStr, subNameIdx]`; the
/// index is the emitted `Owner#toString` subroutine's name-pool slot, which
/// [`KT_DISPLAY`] resolves with `Chunk::find_sub`. Emitted once per overriding
/// class, before `main`, and only in a program that declares one.
pub const KT_TOSTRING_REG: u16 = 35;
/// Register a class's `equals(Any?)` override. Stack: `[tagStr, subNameIdx]`,
/// exactly as [`KT_TOSTRING_REG`]. Consulted by [`equal_vm`], which is what
/// makes `==` — and every equality-based collection member — run the user body
/// instead of the built-in structural compare.
/// Register a class tag as an `enum` constant's. Stack: `[tagStr]`.
///
/// The parser lowers an `enum class` to an ordinary class plus one singleton per
/// constant, so almost nothing about it is special by the time it runs. Three
/// things still are, and all three need the tag: an enum's `toString()` is its
/// `name` (not `Class@hash`), it is `Comparable` by `ordinal`, and a constant
/// with a body is a SUBCLASS whose own tag has to answer the same way.
pub const KT_ENUM_REG: u16 = 37;
pub const KT_EQUALS_REG: u16 = 38;
/// Register a class's `hashCode()` override. Stack: `[tagStr, subNameIdx]`.
/// Consulted by `hash_vm`, so a `List`/`Set`/`Map` fold over an instance
/// picks up the user's answer the way the JVM's does.
pub const KT_HASH_REG: u16 = 39;
/// Build a `StringBuilder`. Stack: `[]`, `[initial]`, or `[capacity]` with
/// `arg` = the argument count; pushes the new builder's `Value::Obj` handle.
///
/// The two one-argument forms are told apart by the value: `StringBuilder(64)`
/// preallocates and starts EMPTY where `StringBuilder("64")` starts with that
/// text, which is the JVM's `(int)` / `(CharSequence)` overload split. Capacity
/// itself is unobservable here — nothing in Kotlin reads it back except
/// `capacity()`, whose exact growth policy is a JVM implementation detail — so
/// the int form just yields an empty builder.
pub const KT_BUILDER: u16 = 40;
/// Referential identity, `a === b`. Stack: `[a, b]`; pops both and pushes a
/// `Bool`. An extension op rather than a builtin because — unlike `==`, which
/// may re-enter the VM to run a user `equals` override — identity never calls
/// back into Kotlin: it is a handle comparison and nothing more.
pub const KT_IDENTITY: u16 = 41;

// ── Builtin ids (`Op::CallBuiltin`) ─────────────────────────────────────────
//
// These are a SEPARATE dispatch namespace from the `Op::Extended` ids above:
// `Op::CallBuiltin` routes through the VM's `builtin_table` (a stable `fn`
// table), which — unlike `Op::Extended`'s take/restore of the single extension
// handler — stays live across a *re-entrant* `vm.run()`. That re-entrancy is
// exactly what invoking a first-class lambda needs (run the lambda's body chunk
// while the enclosing run is paused), and it keeps every `KT_*` extension op
// usable *inside* a lambda body. Numeric overlap with the `KT_*` ids above is
// harmless — the two tables never share a lookup.

/// Build a closure value. Stack: `[cap0 .. cap{k-1}, name_idx, params, ncap]`
/// (top is `ncap`); the three trailing ints are the body's name-pool index, the
/// parameter count, and the capture count. Registers a heap closure carrying the
/// captured upvalue values (by value) and returns its `Value::Obj` handle.
pub const KT_MAKE_CLOSURE: u16 = 100;
/// Invoke a closure `f(args)`. Stack: `[closure, arg0 .. arg{n-1}]` with `argc`
/// = `n`. Runs the closure body through a nested `vm.run()` and pushes its
/// result; faults when the value is not a closure.
pub const KT_CLOSURE_CALL: u16 = 101;
/// Dispatch a higher-order collection method that takes a lambda value. Stack:
/// `[recv, extra0 .. extra{m-1}, closure, nameStr]` with the method name (a
/// `Str`) on top and `argc` = `m` (the count of non-closure leading args, e.g.
/// `fold`'s initial value). Iterates `recv`, invoking `closure` per element, and
/// pushes the method's result.
pub const KT_COLL_HOF: u16 = 102;
/// Dispatch an `it`-form scope function (`let`/`also`/`takeIf`) on any receiver.
/// Stack: `[recv, closure, nameStr]` with the name (a `Str`) on top. Invokes the
/// lambda with the receiver bound to `it` and pushes the scope function's result.
pub const KT_SCOPE_FN: u16 = 103;

/// Builtin id for `IntArray(n) { … }` / `Array(n) { … }` — the lambda-initializer
/// array constructors. Stack: `[n, descStr, closure]` with the closure on top;
/// the closure is invoked once per index and its result becomes that element.
pub const KT_ARRAY_INIT: u16 = 104;

/// Builtin id for `println(x)` (`argc` = 0 or 1). Only emitted in a program that
/// uses exceptions: unlike fusevm's native `Op::PrintLn`, it is *suppressed*
/// while an exception is unwinding, so nothing is printed between a `throw` and
/// its handler. An exception-free program keeps the native op.
pub const KT_PRINTLN: u16 = 105;
/// Builtin id for `print(x)` — see [`KT_PRINTLN`].
pub const KT_PRINT: u16 = 106;

/// Builtin id for the Kotlin display form of a value in a program that declares
/// a `toString()` override. Stack: `[value]`; pushes a `Str`.
///
/// It is a *builtin* rather than an [`Op::Extended`] because rendering an
/// instance means running the user's `toString()` body through a nested
/// `vm.run()`, and only the builtin table stays live across that re-entry. It
/// recurses through `List`/`Map`/`Pair` so an override is honoured for an
/// element too (`println(listOf(shape))`), which the VM-less
/// [`KT_TO_STRING`] cannot do. A program with no override never emits it and
/// keeps the single extension op it had.
///
/// [`Op::Extended`]: fusevm::Op::Extended
pub const KT_DISPLAY: u16 = 107;
/// Builtin id for `joinToString(sep)` in a program that declares a `toString()`
/// override — the [`KT_DISPLAY`] element rendering with a separator. Stack:
/// `[recv]` or `[recv, sep]` (`argc` distinguishes).
pub const KT_JOIN: u16 = 108;
/// The re-entrant twins of the `KT_METHOD` / `KT_SET` / `KT_IN` /
/// `KT_INDEX_GET` / `KT_INDEX_SET` extension ops.
///
/// Each reaches container equality, and container equality can run a user
/// `equals`/`hashCode` through a nested `vm.run()`. fusevm dispatches
/// `Op::Extended` by *taking* the handler out of the VM for the duration of the
/// call, so a nested run finds none and every extension op it executes silently
/// does nothing — the member body would read its receiver's fields as `Undef`.
/// The `builtin_table` is indexed in place and survives re-entry, so these live
/// there instead. `KT_INDEX_SET_VM` is a statement and answers `Undef`, which
/// its emission site pops.
pub const KT_METHOD_VM: u16 = 125;
pub const KT_SET_VM: u16 = 126;
pub const KT_IN_VM: u16 = 127;
pub const KT_INDEX_GET_VM: u16 = 128;
pub const KT_INDEX_SET_VM: u16 = 129;
pub const KT_MAP_VM: u16 = 130;

/// A Kotlin operator CONVENTION applied to a heap receiver. Stack:
/// `[lhs, rhs, nameStr]`, where `nameStr` is `plus`/`minus` (or the
/// `plusAssign`/`minusAssign` in-place forms).
///
/// Kotlin's operators are not instructions. `a + b` *means* `a.plus(b)`,
/// resolved against the left operand's type, so a `List`/`Set`/`Map` receiver
/// answers with a collection. Emitting `Op::Add` for one instead coerces the
/// object HANDLE to a number: that is what made `listOf(1, 2, 3) - 2` evaluate
/// to `-2.0`, a silent wrong answer of the worst kind, since a collection
/// operation came back as arithmetic. The `operator_apply` routine below holds
/// the per-receiver semantics.
///
/// A builtin rather than an `Op::Extended` for the same reason as its
/// neighbours above: element equality can run a user `equals`/`hashCode`
/// through a nested `vm.run()`, which an extension handler cannot host.
pub const KT_OPER_VM: u16 = 131;

/// The `kotlin` preconditions — `require`, `requireNotNull`, `check`,
/// `checkNotNull`, `error`, and `TODO`. Stack: `[subject?, message?, nameStr]`
/// with `argc` = how many of the two leading slots are present; the name on top
/// picks which contract applies.
///
/// One id for all six because they differ only in which throwable they raise
/// and what its default message says. A builtin rather than an `Op::Extended`
/// because the optional message is a LAMBDA — `require(ok) { expensive() }`
/// must not evaluate `expensive()` when the condition holds — and invoking one
/// needs the re-entrant builtin table.
pub const KT_PRECOND: u16 = 132;
/// Build or extend a `Comparator`. Stack: `[base?, sel0 .. sel{n-1}, nameStr]`
/// with `arg` = the selector count `n`; `nameStr` is one of `compareBy`,
/// `compareByDescending`, `thenBy`, `thenByDescending`, and the two `then…`
/// forms pop a base comparator beneath the selectors. Pushes the comparator.
///
/// A builtin rather than an `Op::Extended` because the comparator's selectors
/// are closures that `sortedWith` later invokes re-entrantly.
pub const KT_COMPARATOR: u16 = 133;
/// Build a lazy sequence. Stack: `[seed, step]`; pops both and pushes the
/// `HeapObj::Gen` handle. `step` is a one-parameter closure answering the next
/// element or Kotlin `null` to end the sequence.
pub const KT_GENSEQ: u16 = 134;
/// Build a READ-ONLY `List` literal — `listOf(…)` / `emptyList()`. Stack: `n`
/// elements; pushes the handle. Identical to [`KT_LIST`] except that it records
/// the receiver's implementation class, which its out-of-range diagnostic then
/// names; see the `ListImpl` table in this module.
pub const KT_LIST_RO: u16 = 135;
/// Record an already-built `List`'s implementation class. Stack: `[list, tag]`,
/// `tag` on top; pushes the list back.
///
/// [`KT_LIST_RO`] can only tag a list it BUILDS from stack elements, and the two
/// callers here produce theirs some other way: `buildList { … }` desugars to
/// `mutableListOf().apply { … }`, and the one-argument `listOfNotNull(x)`
/// overload to `listOf(x).filterNotNull()`. Both hand back a fresh, untagged
/// handle, so the class their out-of-range diagnostic should name is lost
/// unless it is applied afterwards.
pub const KT_LIST_TAG: u16 = 136;

/// Kotlin `==` over heap objects. Stack: `[a, b]`; pushes a `Bool`.
///
/// A **builtin**, not an `Op::Extended`, for one reason: a user `equals` body
/// runs through a nested `vm.run()`, and fusevm's extension dispatch *takes*
/// the handler out of the VM for the duration of a call — so every
/// `Op::Extended` the nested run executes would find no handler and silently do
/// nothing. The `builtin_table` survives re-entry, which is the same reason
/// [`KT_DISPLAY`] is a builtin. See [`equal_vm`].
pub const KT_OBJEQ_VM: u16 = 109;

// ── Exception builtins (`try` / `catch` / `finally` / `throw`) ──────────────
//
// See the “Exception unwinding” section below for the protocol these implement.

/// Builtin id for constructing a throwable (`RuntimeException("boom")`). Stack:
/// `[fqnStr, message]` with the message on top (`Undef` for the no-arg form).
pub const KT_EXC_NEW: u16 = 110;
/// Builtin id for `throw e`: pops the throwable and makes it the in-flight
/// exception. Returns `Undef` (a `throw` expression's value is never observed).
pub const KT_EXC_THROW: u16 = 111;
/// Builtin id for the unwind check the compiler emits after each statement:
/// takes nothing and pushes a `Bool` — `true` while an exception is in flight.
pub const KT_EXC_PENDING: u16 = 112;
/// Builtin id for a `catch` type test. Pops the caught type's simple name and
/// pushes whether the in-flight exception is an instance of it (walking the JVM
/// throwable hierarchy). Does not consume the exception.
pub const KT_EXC_MATCH: u16 = 113;
/// Builtin id for consuming the in-flight exception once an arm matched: pushes
/// it and clears the in-flight slot so the handler body runs normally.
pub const KT_EXC_TAKE: u16 = 114;
/// Builtin id for the value-stack depth at `try` entry (pushes an `Int`).
pub const KT_EXC_DEPTH: u16 = 115;
/// Builtin id for truncating the value stack back to a depth recorded by
/// [`KT_EXC_DEPTH`]. Stack: `[depth]`. Discards whatever operands the statement
/// the exception abandoned had already pushed.
pub const KT_EXC_CUT: u16 = 116;
/// Builtin id for parking the in-flight exception across a `finally` body, so
/// the finalizer's own statements are not immediately unwound.
pub const KT_EXC_STASH: u16 = 117;
/// Builtin id for restoring the exception parked by [`KT_EXC_STASH`]. An
/// exception raised *by* the finalizer wins over the parked one — the JVM rule.
pub const KT_EXC_UNSTASH: u16 = 118;
/// Builtin id for reporting an exception no handler claimed: formats the JVM's
/// `Exception in thread "main" …` line and halts, so the process exits non-zero.
pub const KT_EXC_ABORT: u16 = 119;

/// `KT_AS`: the runtime half of `x as T` / `x as? T`. Stack (top-down):
/// `typeName, value`; `arg == 1` for the safe (`as?`) form.
///
/// The value itself is never converted — a cast does not change a
/// representation in Kotlin either — so all this does is check the type and
/// decide what a mismatch means: `ClassCastException` for `as`, null for `as?`.
/// The cast's real work is static, in the type the compiler gives the result.
pub const KT_AS: u16 = 120;

/// `KT_LAZY_NEW`: wrap a thunk closure in an unforced `by lazy` cell.
pub const KT_LAZY_NEW: u16 = 121;

/// `KT_RUN_CATCHING`: run a block and capture its outcome as a `Result`.
///
/// Stack: `closure`. The block's own frame already unwinds a raise back to
/// here — the pending slot is how this frontend carries an exception — so all
/// this does is read that slot, clear it, and package either outcome.
pub const KT_RUN_CATCHING: u16 = 123;

/// `KT_RESULT_HOF`: the lambda-taking `Result` members. Stack (top-down):
/// `nameStr, closure, result`.
pub const KT_RESULT_HOF: u16 = 124;

/// `KT_LAZY_GET`: read a `by lazy` cell, running its thunk the first time.
///
/// A BUILTIN rather than an extension op because forcing runs user code, and
/// only a builtin can re-enter the VM. A value that is not a cell passes
/// straight through, so the op is safe to emit on any read of a property the
/// compiler believes is lazy.
pub const KT_LAZY_GET: u16 = 122;

thread_local! {
    /// Set by a runtime fault (e.g. integer divide-by-zero) so the CLI can
    /// report it as an uncaught exception after `VM::run` returns.
    static KT_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };

    /// The host-side object heap. `Value::Obj(u32)` handles index into this
    /// `Vec`; the frontend owns the pointed-to object, fusevm only carries the
    /// handle. This is the same architecture the mature fusevm frontends use for
    /// their class/collection model. Reset per VM install so runs don't share
    /// object identity.
    static HEAP: RefCell<Vec<HeapObj>> = const { RefCell::new(Vec::new()) };

    /// Declared type name → its supertypes, nearest first, as published by
    /// [`KT_TYPE_REG`] before `main` runs. The flat instance record cannot carry
    /// a hierarchy, and three things need one: `is` on a user class, `catch` on a
    /// user class extending a built-in throwable, and the `Class: message`
    /// display form such a class inherits.
    static TYPES: RefCell<std::collections::HashMap<String, Vec<String>>> =
        RefCell::new(std::collections::HashMap::new());

    /// Class tag → the name-pool index of its `toString()` subroutine, as
    /// published by [`KT_TOSTRING_REG`]. Consulted by [`KT_DISPLAY`].
    static TOSTRING_SUBS: RefCell<std::collections::HashMap<String, u16>> =
        RefCell::new(std::collections::HashMap::new());

    /// Class tag → the name-pool index of its `equals(Any?)` subroutine, as
    /// published by [`KT_EQUALS_REG`]. Consulted by [`equal_vm`].
    static EQUALS_SUBS: RefCell<std::collections::HashMap<String, u16>> =
        RefCell::new(std::collections::HashMap::new());

    /// Class tag → the name-pool index of its `hashCode()` subroutine, as
    /// published by [`KT_HASH_REG`]. Consulted by [`hash_vm`].
    static HASHCODE_SUBS: RefCell<std::collections::HashMap<String, u16>> =
        RefCell::new(std::collections::HashMap::new());

    /// Class tag → one character per OWN property, `'l'` where that property is
    /// declared `Long` and `'.'` otherwise, as published in the `KT_NEW` /
    /// `KT_EXTEND` metadata string. It exists for one job: a `data class`'s
    /// generated `hashCode` folds `Long` fields with a different formula than
    /// `Int` ones, and every Kotlin integer is the same `i64` at run time (see
    /// [`int_hash`]). The compiler knows the declared type, so it says so.
    /// The class tags that are `enum` constants', as published by
    /// [`KT_ENUM_REG`]. See that op for what still depends on knowing.
    static ENUM_CLASSES: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());

    static LONG_FIELDS: RefCell<std::collections::HashMap<String, String>> =
        RefCell::new(std::collections::HashMap::new());

    /// Heap handle → the iteration discipline of that `Map`/`Set`, for the
    /// collections that do NOT iterate in insertion order. See [`CollOrder`].
    ///
    /// A side table rather than a field on the heap variants: `HeapObj::Map`
    /// and `HeapObj::Set` are matched at ~70 sites that all want the entries
    /// and nothing else, and iteration order is a property of the collection's
    /// IDENTITY, not of its contents.
    static COLL_ORDER: RefCell<std::collections::HashMap<u32, CollOrder>> =
        RefCell::new(std::collections::HashMap::new());

    /// Heap handle → which JVM `List` implementation that handle stands for,
    /// for the two that are not `java.util.ArrayList`. See [`ListImpl`].
    ///
    /// A side table for the same reason [`COLL_ORDER`] is one: `HeapObj::List`
    /// is matched at ~80 sites that want the elements and nothing else, and
    /// which class produced the list is a property of its IDENTITY.
    static LIST_IMPL: RefCell<std::collections::HashMap<u32, ListImpl>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Which JVM class backs a Kotlin `List`, to the extent a program can observe
/// it — which is exactly one place: the message an out-of-range index raises.
///
/// Kotlin's `List` is an interface over at least four implementations, and each
/// one words the fault differently. Measured on JDK 21.0.12:
///
/// | receiver | class | fault |
/// | --- | --- | --- |
/// | `listOf()`, `emptyList()` | `kotlin.collections.EmptyList` | `IndexOutOfBoundsException: Empty list doesn't contain element at index 9.` |
/// | `listOf(1)` | `java.util.Collections$SingletonList` | `IndexOutOfBoundsException: Index: 9, Size: 1` |
/// | `listOf(1, 2)` | `java.util.Arrays$ArrayList` | `ArrayIndexOutOfBoundsException: Index 9 out of bounds for length 2` |
/// | `mutableListOf(1, 2)`, `map`, `filter`, `subList` | `java.util.ArrayList` | `IndexOutOfBoundsException: Index 9 out of bounds for length 2` |
///
/// So the SIZE decides as much as the constructor does, and it decides it twice
/// over: a read-only list of 0 or 1 elements is never the class its builder
/// nominally returns, because `optimizeReadOnlyList` collapses it. That is why
/// this is a provenance tag and not a size test — `listOf(1, 2)` and
/// `listOf(1, 2).toList()` are the same two elements and different classes.
///
/// An untagged handle is the `ArrayList` row, which is what every list this
/// runtime builds for a member result is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListImpl {
    /// A `listOf(…)` / `emptyList()` LITERAL. Collapses to `EmptyList` at 0 and
    /// `SingletonList` at 1; at 2 or more it is `Arrays$ArrayList`, the one
    /// `List` whose `get` reaches a bare array and so raises the ARRAY
    /// subclass.
    Literal,
    /// The result of a member that ends in Kotlin's `optimizeReadOnlyList` —
    /// `toList`, `reversed`, `distinct`, `take`, `drop`, `takeLast`,
    /// `dropLast`, `slice`. Same collapse at 0 and 1, but at 2 or more it is
    /// the `ArrayList` the member built, NOT an array-backed one — which is the
    /// whole reason this is a second tag rather than a flag on [`Literal`].
    ///
    /// `listOfNotNull` used to be listed here and does not belong: the VARARG
    /// overload is `filterNotNull()`, a plain `ArrayList` that collapses at no
    /// size, and the ONE-argument overload is a [`Literal`]. Neither is this
    /// row, and nothing ever tagged it as one.
    ReadOnly,
    /// The `sorted…` family over a COLLECTION receiver. This row used to be
    /// listed under [`ReadOnly`], on the reading that a sorting member builds
    /// its own `ArrayList`; measured, it does not. `Iterable.sorted` takes the
    /// `Collection` fast path — `toTypedArray().apply { sort() }.asList()` —
    /// and `asList` hands back an `Arrays$ArrayList` wrapping that very array,
    /// so `listOf(2, 1).sorted()[9]` raises the ARRAY subclass exactly as a
    /// literal does. Verified on kotlinc 2.4.10 / JDK 21.0.12 for `sorted`,
    /// `sortedDescending`, `sortedBy`, `sortedByDescending` and `sortedWith`
    /// over `List`, `Set`, `Array` and `CharSequence` receivers.
    ///
    /// A RANGE receiver is the exception and stays [`ReadOnly`]: `IntRange` is
    /// an `Iterable` and not a `Collection`, so it misses that fast path and
    /// gets the `toMutableList().apply { sort() }` branch — a real `ArrayList`.
    Sorted,
    /// The list `buildList { … }` hands back. Kotlin's builder is neither of
    /// the two above and does not collapse at ANY size: it is a
    /// `kotlin.collections.builders.ListBuilder`, and its bounds check writes a
    /// lowercase, comma-separated message all of its own — `index: 9, size: 2`,
    /// where every other `List` here says `Index 9 out of bounds for length 2`.
    /// Measured on kotlinc 2.4.10 / JDK 21.0.12 at sizes 0, 1 and 2.
    Builder,
}

/// Record `list`'s implementation class. A no-op for a non-handle.
fn tag_list_impl(list: &Value, which: ListImpl) {
    if let Value::Obj(id) = list {
        LIST_IMPL.with(|t| t.borrow_mut().insert(*id, which));
    }
}

thread_local! {
    /// Handles that are a `Sequence` VIEW of the list they are carried as.
    ///
    /// `asSequence()` (and a `generateSequence` pipeline that materialized)
    /// keeps a `HeapObj::List` here, because kotlinrs evaluates a bounded
    /// sequence eagerly. That representation is not observable — but the
    /// DIAGNOSTICS are, and Kotlin declares its own `first`/`last`/`single`/
    /// `reduce`/`elementAt` on `Sequence` with their own wording:
    /// `listOf<Int>().asSequence().first()` says `Sequence is empty.` where the
    /// list says `List is empty.` (measured on kotlinc 2.4.10 / JDK 21.0.12).
    ///
    /// Deliberately NOT a [`ListImpl`] row. That table names which JVM `List`
    /// CLASS a handle is, and a `Sequence` is not a `List` of any class; adding
    /// a row there would make it answer a question it exists to answer
    /// precisely.
    static SEQ_VIEW: RefCell<std::collections::HashSet<u32>> =
        RefCell::new(std::collections::HashSet::new());
}

/// Mark `v` as a `Sequence` view — see [`SEQ_VIEW`] — and hand it back.
fn tag_sequence(v: Value) -> Value {
    if let Value::Obj(id) = v {
        SEQ_VIEW.with(|t| {
            t.borrow_mut().insert(id);
        });
    }
    v
}

/// Whether `kotlin.sequences` declares `name` to answer another `Sequence`.
///
/// The complement is what matters: `toList`/`toSet`/`toMutableList`/
/// `toTypedArray`/`asIterable`/`joinToString`/`groupBy`/`associate…` all answer
/// a COLLECTION, and an element accessor answers an element — none of those
/// carry the tag forward. Stated once here rather than at each derivation site,
/// which is what let the tag stop after one step.
fn sequence_preserving(name: &str) -> bool {
    matches!(
        name,
        "map"
            | "mapIndexed"
            | "mapNotNull"
            | "filter"
            | "filterNot"
            | "filterNotNull"
            | "filterIndexed"
            | "take"
            | "takeWhile"
            | "drop"
            | "dropWhile"
            | "distinct"
            | "distinctBy"
            | "sorted"
            | "sortedBy"
            | "sortedDescending"
            | "sortedByDescending"
            | "sortedWith"
            | "flatMap"
            | "minus"
            | "plus"
            | "onEach"
            | "withIndex"
            | "asSequence"
            | "reversed"
            | "zip"
            | "zipWithNext"
    )
}

/// Whether `v` is a `Sequence` view.
fn is_sequence_view(v: &Value) -> bool {
    match v {
        Value::Obj(id) => SEQ_VIEW.with(|t| t.borrow().contains(id)),
        _ => false,
    }
}

/// Allocate a `List` that a read-only member produced, tagged so its
/// out-of-range diagnostic names the class Kotlin would have handed back.
fn alloc_ro_list(items: Vec<Value>) -> Value {
    let v = alloc(HeapObj::List(items));
    tag_list_impl(&v, ListImpl::ReadOnly);
    v
}

/// Allocate the `List` a `sorted…` member produced. `collection` is whether the
/// receiver was one — see [`ListImpl::Sorted`] for why a range differs.
fn alloc_sorted_list(items: Vec<Value>, collection: bool) -> Value {
    let v = alloc(HeapObj::List(items));
    tag_list_impl(
        &v,
        if collection {
            ListImpl::Sorted
        } else {
            ListImpl::ReadOnly
        },
    );
    v
}

/// Pop `n` operands and hand them back in SOURCE order. The stack yields them
/// last-first, so a collection builder that skipped the reverse would store
/// `listOf(1, 2)` as `[2, 1]`.
fn pop_n(vm: &mut VM, n: u8) -> Vec<Value> {
    let n = n as usize;
    let mut vals = Vec::with_capacity(n);
    for _ in 0..n {
        vals.push(vm.pop());
    }
    vals.reverse();
    vals
}

/// Tag `v` when it IS a `List`, and leave every other kind alone — for the
/// members that answer their receiver's own kind rather than always a list.
fn tag_if_list(v: Value, which: ListImpl) -> Value {
    if with_obj(&v, |o| matches!(o, HeapObj::List(_))).unwrap_or(false) {
        tag_list_impl(&v, which);
    }
    v
}

/// How a `Map`/`Set` orders its own iteration.
///
/// Kotlin's `mapOf`/`setOf` build `LinkedHashMap`/`LinkedHashSet`, which iterate
/// in insertion order — that is the default, and it needs no entry here. The
/// other two JVM collections Kotlin exposes do not, and printing them in
/// insertion order is a silent wrong answer: `hashMapOf("banana" to 1, "apple"
/// to 2, "cherry" to 3, "zebra" to 4)` prints `{banana=1, zebra=4, apple=2,
/// cherry=3}` on the reference toolchain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CollOrder {
    /// `java.util.HashMap`/`HashSet` — bucket-table order, from a table that
    /// started at the carried initial capacity. See [`hash_order`].
    Hash(usize),
    /// `java.util.TreeSet`/`TreeMap` — ascending natural order of the keys.
    Sorted,
}

/// Iteration order `1`: a `java.util.HashMap`/`HashSet` bucket table. One of the
/// codes the compiler packs into the trailing spec argument of `KT_SET_VM` /
/// `KT_MAP_VM`; `0` (the absent bit pattern) is the insertion-ordered default.
pub const COLL_HASH: u8 = 1;
/// Iteration order `2`: a `java.util.TreeSet`, ascending by natural order.
pub const COLL_SORTED: u8 = 2;
/// Spec flag: the `HashSet(other)` copy form rather than the vararg builder.
pub const COLL_COPY: u8 = 0x10;
/// Spec flag: the no-argument constructor, which starts from Java's default
/// 16-bucket table instead of one sized to the element count.
pub const COLL_DEFAULT_CAP: u8 = 0x20;

/// The collection discipline a builder call asks for, as the compiler encodes it
/// in the trailing argument of `KT_SET_VM`/`KT_MAP_VM`.
///
/// The low nibble names the iteration order. Two flags above it separate the
/// three construction shapes, which a runtime look at the arguments cannot tell
/// apart: `hashSetOf(listOf(1))` builds a one-element set holding a list, where
/// `HashSet(listOf(1))` copies that list's elements, and the two differ again in
/// the table they start from — a no-arg `HashSet()` gets Java's default 16
/// buckets, while a sized builder pre-divides its element count by the load
/// factor and often starts SMALLER, which changes the mask and so the order.
#[derive(Clone, Copy)]
struct CollSpec {
    order: Option<CollOrder>,
    /// The `HashSet(other)` form: take the single argument's elements.
    copy: bool,
    /// The no-argument constructor: Java's default table, not a sized one.
    default_cap: bool,
}

impl CollSpec {
    /// Take the trailing spec argument off the stack.
    fn pop(vm: &mut VM) -> CollSpec {
        let code = vm.pop().to_int() as u8;
        CollSpec {
            order: match code & 0x0f {
                COLL_HASH => Some(CollOrder::Hash(DEFAULT_CAPACITY)),
                COLL_SORTED => Some(CollOrder::Sorted),
                _ => None,
            },
            copy: code & COLL_COPY != 0,
            default_cap: code & COLL_DEFAULT_CAP != 0,
        }
    }

    /// Record the built collection's discipline and put it in that order.
    fn apply(self, vm: &mut VM, v: &Value, size: usize) {
        let ord = match self.order {
            Some(CollOrder::Hash(_)) if self.default_cap => CollOrder::Hash(DEFAULT_CAPACITY),
            Some(CollOrder::Hash(_)) => CollOrder::Hash(map_capacity(size)),
            Some(o) => o,
            None => return,
        };
        set_order(vm, v, ord);
    }
}

/// Record `v`'s iteration discipline, and put it in that order straight away.
fn set_order(vm: &mut VM, v: &Value, ord: CollOrder) {
    if let Value::Obj(id) = v {
        COLL_ORDER.with(|c| c.borrow_mut().insert(*id, ord));
    }
    reorder(vm, v);
}

/// The discipline recorded for `v`, if it is not the insertion-ordered default.
fn order_of(v: &Value) -> Option<CollOrder> {
    match v {
        Value::Obj(id) => COLL_ORDER.with(|c| c.borrow().get(id).copied()),
        _ => None,
    }
}

/// Restore `v`'s iteration order after a mutation.
///
/// The stored sequence IS the iteration sequence — every reader walks the
/// `Vec` — so a `HashMap` is kept permanently in bucket order rather than
/// reordered at each read. An insertion-ordered collection has no entry in
/// [`COLL_ORDER`] and costs nothing here.
fn reorder(vm: &mut VM, v: &Value) {
    let Some(ord) = order_of(v) else {
        return;
    };
    // Keys first, under a short borrow: ranking them runs `hashCode`/
    // `compareTo`, which can re-enter the VM and reallocate the heap.
    let keys: Vec<Value> = match with_obj(v, |o| match o {
        HeapObj::Map(entries) => Some(entries.iter().map(|(k, _)| k.clone()).collect()),
        HeapObj::Set(items) => Some(items.clone()),
        _ => None,
    })
    .flatten()
    {
        Some(k) => k,
        None => return,
    };
    let order = match ord {
        CollOrder::Hash(initial) => hash_order(vm, &keys, initial),
        CollOrder::Sorted => {
            let mut idx: Vec<usize> = (0..keys.len()).collect();
            idx.sort_by(|&a, &b| value_cmp(&keys[a], &keys[b]));
            idx
        }
    };
    with_obj_mut(v, |o| match o {
        HeapObj::Map(entries) => {
            *entries = order.iter().map(|&i| entries[i].clone()).collect();
        }
        HeapObj::Set(items) => {
            *items = order.iter().map(|&i| items[i].clone()).collect();
        }
        _ => {}
    });
}

/// The positions of `keys` in `java.util.HashMap` ITERATION order.
///
/// A `HashMap` iterates its bucket TABLE, not its insertion sequence. The table
/// holds `n` buckets for a power-of-two `n`; a key lands in bucket
/// `(n - 1) & (h ^ (h >>> 16))`, where the exclusive-or is Java's `HashMap.hash`
/// spread, mixing the high bits down so they survive the mask; and a bucket
/// keeps its own arrivals in the order they came.
///
/// Java grows the table by splitting each bucket's chain into a low and a high
/// half, and Java 8 onward preserves relative order within each half. That is
/// what makes a STABLE sort of the insertion sequence by final bucket index
/// reproduce the whole resize history without replaying it.
///
/// Known boundary, deliberately not modelled: a bucket that reaches eight
/// entries in a table of at least 64 becomes a red-black tree, and iterates in
/// the tree's order rather than its arrival order. It takes eight keys colliding
/// under the mask to reach, and the tree order needs `Comparable` tie-breaking
/// that has no meaning for every key type.
fn hash_order(vm: &mut VM, keys: &[Value], initial: usize) -> Vec<usize> {
    let n = table_size(keys.len(), initial);
    let mut idx: Vec<usize> = (0..keys.len()).collect();
    let buckets: Vec<u32> = keys.iter().map(|k| bucket_of(vm, k, n)).collect();
    idx.sort_by_key(|&i| buckets[i]); // stable: ties keep arrival order
    idx
}

/// `java.util.HashMap`'s default table size.
const DEFAULT_CAPACITY: usize = 16;

/// The bucket `key` occupies in a table of `n` buckets.
fn bucket_of(vm: &mut VM, key: &Value, n: usize) -> u32 {
    let h = hash_vm(vm, key, false) as u32;
    let spread = h ^ (h >> 16);
    spread & (n as u32 - 1)
}

/// The table size a `HashMap` holding `size` entries has reached, having started
/// at `initial` buckets.
///
/// The table doubles when a put takes the size past `0.75 * n`, so the final
/// size is the smallest power of two at or above `initial` that still leaves the
/// entries under that load factor.
fn table_size(size: usize, initial: usize) -> usize {
    let mut n = initial.max(1).next_power_of_two();
    while size * 4 > n * 3 {
        n *= 2;
    }
    n
}

/// Kotlin's `mapCapacity` — the initial capacity its `hashMapOf`/`hashSetOf`
/// request for a known element count. It is not the count: asking for `size`
/// buckets would resize immediately, so the builders pre-divide by the load
/// factor. The resulting table is often SMALLER than the default 16, which
/// changes the bucket mask and so the printed order.
fn map_capacity(expected: usize) -> usize {
    if expected < 3 {
        expected + 1
    } else {
        ((expected as f32 / 0.75f32) + 1.0f32) as usize
    }
}

/// The registered `Long`-field mask for a class tag (see [`LONG_FIELDS`]).
fn long_fields(class: String) -> Option<String> {
    LONG_FIELDS.with(|f| f.borrow().get(&class).cloned())
}

/// Record a class's `Long`-field mask. Registration rides on construction
/// rather than on class declaration because the mask travels in the same
/// metadata string the fields do; it is idempotent, so re-registering on every
/// `C(...)` costs one map write and keeps the emitter to a single token.
fn register_widths(class: &str, widths: &str) {
    if widths.is_empty() {
        return;
    }
    LONG_FIELDS.with(|f| f.borrow_mut().insert(class.to_string(), widths.to_string()));
}

/// Whether the declared type `class` is `ty` itself or lists it as a supertype.
fn type_is_a(class: &str, ty: &str) -> bool {
    class == ty
        || TYPES.with(|t| {
            t.borrow()
                .get(class)
                .is_some_and(|s| s.iter().any(|x| x == ty))
        })
}

/// Whether the declared type `class` reaches `java.lang.Throwable` — i.e. it was
/// declared as `class C(…) : Exception(…)` or below one.
fn type_is_throwable(class: &str) -> bool {
    type_is_a(class, "Throwable")
}

/// A heap-resident object: a class instance, a `List`, a `Map`, or a `Pair`.
/// Instances keep fields in declaration order (name-carrying) so a `data class`
/// can render `C(x=1, y=2)` and destructure via `componentN` faithfully.
#[derive(Clone)]
enum HeapObj {
    Instance {
        class: String,
        is_data: bool,
        fields: Vec<(String, Value)>,
        /// Index in `fields` at which this class's **own** (primary-constructor)
        /// properties begin — everything before it was inherited.
        ///
        /// Kotlin derives a `data class`'s `toString`/`equals`/`hashCode`/
        /// `componentN` from the primary constructor *alone*, so
        /// `data class Child(val name: String) : Base(7)` prints
        /// `Child(name=x)` and compares only `name`, while `child.id` still
        /// reads the inherited field. The record stays flat (property lookup
        /// wants every field); this marks where the derived members start.
        data_from: usize,
        /// How many fields from `data_from` the primary constructor supplied.
        /// The record continues past them with the class's BODY properties
        /// (`class C(val a: Int) { val b = 2 }`), which Kotlin's derived members
        /// deliberately do not see — `data class D(val a: Int) { val b = 2 }`
        /// prints `D(a=1)` and compares only `a`. So the derived members read
        /// `fields[data_from .. data_from + data_len]`, not the whole tail.
        data_len: usize,
    },
    List(Vec<Value>),
    /// A `Set`, kept as its insertion-ordered distinct elements. Kotlin's
    /// `setOf`/`mutableSetOf` build a `LinkedHashSet`, so iteration and display
    /// follow insertion order and are reproducible; equality, by contrast, is
    /// order-insensitive (`setOf(1, 2) == setOf(2, 1)`).
    Set(Vec<Value>),
    /// Insertion-ordered key/value pairs (Kotlin `mapOf` preserves order).
    Map(Vec<(Value, Value)>),
    Pair(Value, Value),
    /// A `Map.Entry`. Structurally a key/value couple like [`HeapObj::Pair`],
    /// but Kotlin keeps the two types apart and they OBSERVABLY differ: an entry
    /// renders as `k=v` where a pair renders as `(k, v)`, its `hashCode` is
    /// `key xor value` where a pair folds like the `data class` it is, and
    /// `mapOf(1 to "a").entries.first() == (1 to "a")` is `false`. Sharing one
    /// variant made all three wrong.
    Entry(Value, Value),
    /// A `Result`: the success value, or the throwable a `runCatching` block
    /// raised. Kotlin's `Result` is an inline class over exactly this union, and
    /// it renders as `Success(v)` / `Failure(<throwable>)`.
    Res {
        value: Value,
        err: Option<Value>,
    },
    /// A `by lazy` cell: the thunk that computes the value, and the value once
    /// it has been computed. The distinction from an eagerly-stored value is
    /// observable — the thunk runs at first READ, not at declaration — so the
    /// cell has to exist at runtime rather than being folded away.
    Lazy {
        thunk: Value,
        value: Option<Value>,
    },
    /// A `Triple`. Kotlin's `Pair`/`Triple` are ordinary `data class`es, but
    /// they render as `(a, b)` / `(a, b, c)` rather than `Name(x=…)`, so they
    /// get their own variants instead of riding on [`HeapObj::Instance`].
    Triple(Value, Value, Value),
    /// A first-class lambda: the body's chunk name-pool index (resolved to an
    /// entry via `Chunk::find_sub` at call time), its parameter count, and the
    /// values captured from the enclosing frame at creation (its upvalues, stored
    /// by value so a lambda outlives the frame it closed over).
    Closure {
        name_idx: u16,
        params: u8,
        captures: Vec<Value>,
    },
    /// An `IntRange` / `IntProgression`. See [`RangeObj`].
    Range(RangeObj),
    /// A throwable: its fully-qualified JVM class name (`java.lang.RuntimeException`)
    /// and the constructor message (`None` for the no-arg form, whose `message`
    /// is Kotlin `null`). Kept apart from [`HeapObj::Instance`] because a
    /// throwable has no ordered field record and renders as `fqn` / `fqn: message`.
    Exc {
        class: String,
        msg: Option<String>,
    },
    /// A `Grouping` — the source elements and the key selector `groupingBy`
    /// was handed, held until a terminal operation asks for something.
    ///
    /// Kotlin's `Grouping` is an interface over exactly this pair, and it is
    /// deliberately lazy: `groupingBy { }.eachCount()` counts per key without
    /// ever building the per-key lists `groupBy` allocates.
    Grouping {
        items: Vec<Value>,
        key: Value,
    },
    /// A JVM array. `desc` is its JVM type descriptor (`"[I"`,
    /// `"[Ljava.lang.Integer;"`, …), which only exists to reproduce the
    /// `toString` form — arrays inherit `Object.toString`, so Kotlin prints them
    /// as `<descriptor>@<identity hash>`.
    Array {
        items: Vec<Value>,
        desc: String,
    },
    /// A `java.lang.StringBuilder`, as its content in UTF-16 code units plus the
    /// capacity of the array holding them.
    ///
    /// Code units rather than a Rust `String` because every index a builder
    /// takes is a JVM `char` offset, and the two disagree the moment a
    /// supplementary character appears: `StringBuilder("a😀b")` has `length` 4,
    /// `[1]` is the HIGH SURROGATE `\uD83D`, and `deleteCharAt(1)` leaves a
    /// three-unit sequence with half a pair in it — none of which a `String`
    /// can even represent, let alone index.
    ///
    /// `cap` is carried because `capacity()` is observable and its growth is
    /// specified: a builder starts at 16 (`StringBuilder()`), at
    /// `initial.length + 16` (`StringBuilder(text)`), or at the requested size,
    /// and an append that does not fit grows it to `max(2 * cap + 2, needed)`.
    Builder {
        units: Vec<u16>,
        cap: usize,
    },
    /// A LAZY sequence — `generateSequence(seed) { next }` plus the pipeline
    /// stages applied to it.
    ///
    /// Every other sequence here is a materialized `List`, which is right
    /// because every other one is finite. `generateSequence` is not: its step
    /// runs until it answers `null`, and `generateSequence(1) { it * 2 }` never
    /// does. So this one carries the recipe instead of the elements, and only a
    /// terminal operation pulls — which is what makes `.take(5).toList()` a
    /// five-element list rather than a hang.
    Gen {
        /// The next element to yield, or `None` once the step answered `null`.
        seed: Option<Value>,
        /// `(T) -> T?` — the step. Answering Kotlin `null` ends the sequence.
        step: Value,
        stages: Vec<Stage>,
    },
    /// A `Comparator` built by `compareBy` and extended by `thenBy`: an ORDERED
    /// chain of `(selector closure, descending)` keys, compared left to right
    /// with the first non-equal key deciding.
    ///
    /// The chain is kept rather than folded into one closure because `thenBy`
    /// answers a NEW comparator over the same keys plus one — Kotlin's
    /// comparators are immutable — and because each key carries its own
    /// direction: `compareBy { a }.thenByDescending { b }` sorts ascending by
    /// `a` and descending by `b`, which a single sign cannot express.
    Comparator(Vec<(Value, bool)>),
}

/// One stage of a lazy [`HeapObj::Gen`] pipeline, in application order.
///
/// Only the stages that can be applied ONE ELEMENT AT A TIME are here. A stage
/// that has to see the whole sequence (`sorted`, `distinct`, `groupBy`) cannot
/// be lazy at all, so it materializes instead — and on an endless generator
/// that is a fault, not an answer, which is also what the reference toolchain
/// gives you (it hangs).
#[derive(Clone)]
enum Stage {
    Map(Value),
    /// `filter` when `true`, `filterNot` when `false`.
    Filter(Value, bool),
    /// `takeWhile` — ends the sequence at the first element that fails.
    TakeWhile(Value),
    /// `dropWhile`, with the flag recording that it has already stopped
    /// dropping. It is per-PULL state, which is why a pipeline is cloned into
    /// each new `Gen` rather than shared.
    DropWhile(Value, bool),
    Take(i64),
    Drop(i64),
}

/// The most elements a terminal operation will pull from a generator before
/// giving up. A pipeline with no bound is an infinite loop — the reference
/// toolchain hangs on one — and a diagnosis beats a hang.
const GEN_PULL_CAP: usize = 1_000_000;

/// Which syntactic form produced a range. This is not cosmetic: `a..b` and
/// `a until b` build an `IntRange`, whose `toString` is `first..last`, while
/// `a downTo b` and any `step` build an `IntProgression`, whose `toString` is
/// `first..last step n` (or `first downTo last step n`). Reproducing Kotlin's
/// printed form therefore needs the distinction kept at runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RangeForm {
    /// `a..b` — an `IntRange`.
    Inclusive = 0,
    /// `a until b` — an `IntRange` over `a..(b-1)`.
    Until = 1,
    /// `a downTo b` — an `IntProgression` with step `-1`.
    DownTo = 2,
}

/// A range value: `first`, the *raw* endpoint `end` the source named, and the
/// signed `step`. `progression` marks the `IntProgression` display form.
///
/// `end` is kept raw rather than normalized because the two Kotlin types differ
/// in what they print: an `IntRange` prints the endpoint it was built with,
/// while an `IntProgression` prints its last *reachable* element
/// (`1..10 step 2` prints `1..9 step 2`). [`RangeObj::last`] derives the latter.
#[derive(Clone, Copy)]
struct RangeObj {
    first: i64,
    end: i64,
    step: i64,
    progression: bool,
    /// `'a'..'z'` — a `CharRange`/`CharProgression`. The endpoints are kept as
    /// code units (so the arithmetic below is shared verbatim); this only
    /// decides that its elements and its printed form are characters.
    is_char: bool,
}

impl RangeObj {
    fn new(first: i64, end: i64, form: RangeForm, is_char: bool) -> RangeObj {
        match form {
            RangeForm::Inclusive => RangeObj {
                first,
                end,
                step: 1,
                progression: false,
                is_char,
            },
            // `until` is exclusive: it builds the `IntRange` `first..(end-1)`.
            // Kotlin guards the underflow by yielding an empty range instead of
            // wrapping past `Int.MIN_VALUE`.
            RangeForm::Until => RangeObj {
                first,
                end: end.wrapping_sub(1),
                step: 1,
                progression: false,
                is_char,
            },
            RangeForm::DownTo => RangeObj {
                first,
                end,
                step: -1,
                progression: true,
                is_char,
            },
        }
    }

    /// The last element the progression actually reaches — Kotlin's
    /// `getProgressionLastElement`. An empty progression reports the raw
    /// endpoint (which is what Kotlin prints for, e.g., `1..0 step 2`).
    fn last(&self) -> i64 {
        if self.step > 0 {
            if self.first >= self.end {
                self.end
            } else {
                self.end - difference_modulo(self.end, self.first, self.step)
            }
        } else if self.first <= self.end {
            self.end
        } else {
            self.end + difference_modulo(self.first, self.end, -self.step)
        }
    }

    /// How many elements the range yields (0 when empty).
    fn count(&self) -> i64 {
        let last = self.last();
        if self.step > 0 {
            if self.first > last {
                0
            } else {
                (last - self.first) / self.step + 1
            }
        } else if self.first < last {
            0
        } else {
            (self.first - last) / -self.step + 1
        }
    }

    /// Element `i` (0-based), unchecked — callers bound `i` by [`RangeObj::count`].
    fn at(&self, i: i64) -> i64 {
        self.first + i * self.step
    }

    /// Element `i` as the Kotlin value it is — a `Char` for a `CharRange`.
    fn value_at(&self, i: i64) -> Value {
        self.wrap(self.at(i))
    }

    /// Wrap a code point drawn from this range in its element type.
    fn wrap(&self, n: i64) -> Value {
        if self.is_char {
            char_of(n)
        } else {
            Value::Int(n)
        }
    }

    /// Membership: within the bounds AND on a step boundary. Kotlin's
    /// `IntRange.contains` is a plain bounds test, and an `IntProgression`'s is
    /// an iteration — both agree with this formulation.
    fn contains(&self, v: i64) -> bool {
        let last = self.last();
        let (lo, hi) = if self.step > 0 {
            (self.first, last)
        } else {
            (last, self.first)
        };
        v >= lo && v <= hi && (v - self.first) % self.step == 0
    }

    fn to_vec(self) -> Vec<Value> {
        (0..self.count()).map(|i| self.value_at(i)).collect()
    }
}

/// Kotlin's `differenceModulo(a, b, c)` — `(a mod c) - (b mod c)` reduced into
/// `[0, c)`. Used to snap a progression's endpoint onto a step boundary.
fn difference_modulo(a: i64, b: i64, c: i64) -> i64 {
    let m = a.rem_euclid(c) - b.rem_euclid(c);
    m.rem_euclid(c)
}

/// Clear the object heap. Called on every VM install so a fresh run starts with
/// no residual objects (handles are per-run identities).
fn reset_heap() {
    HEAP.with(|h| h.borrow_mut().clear());
    COLL_ORDER.with(|c| c.borrow_mut().clear());
    LIST_IMPL.with(|t| t.borrow_mut().clear());
    SEQ_VIEW.with(|t| t.borrow_mut().clear());
    TYPES.with(|t| t.borrow_mut().clear());
    TOSTRING_SUBS.with(|t| t.borrow_mut().clear());
    EQUALS_SUBS.with(|t| t.borrow_mut().clear());
    HASHCODE_SUBS.with(|t| t.borrow_mut().clear());
    PENDING.with(|p| *p.borrow_mut() = None);
    STASH.with(|s| s.borrow_mut().clear());
}

// ── Exception unwinding ────────────────────────────────────────────────────
//
// fusevm has no unwind opcode and kotlinrs lowers `fun`s to fusevm's *native*
// `Op::Call` frames, so a thrown exception cannot longjmp out of a frame. `try`
// is therefore a cooperative two-part protocol, the same one the sibling
// frontends (javars, scalars) converged on:
//
//   * **Runtime half (here).** A raise parks the throwable in [`PENDING`]
//     instead of halting, provided the program contains a `try` at all
//     (`EXC_ENABLED`). Every builtin with an observable side effect (printing,
//     closure invocation) short-circuits while [`unwinding`] holds, so nothing
//     escapes between the raise and its handler.
//   * **Compile-time half (`crate::compiler`).** In a program that contains a
//     `try`, the compiler emits a [`KT_EXC_PENDING`] test after every statement;
//     the innermost enclosing construct decides where a `true` answer jumps —
//     out of a loop, out of a `fun` frame, into a `catch` dispatch, or into the
//     terminal abort at the end of `main`.
//
// Unwinding is therefore *statement-granular*: a raise mid-way through a
// statement finishes evaluating that statement's remaining operands (on garbage
// values, with side-effecting builtins suppressed) before control reaches the
// handler; the handler's [`KT_EXC_CUT`] then discards those stranded operands.
// A program with no `try` pays nothing — no check is emitted and a fault halts
// exactly as it did before.

thread_local! {
    /// The exception currently unwinding, if any.
    static PENDING: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Exceptions parked across `finally` bodies (one entry per nested `finally`
    /// currently running).
    static STASH: RefCell<Vec<Option<Value>>> = const { RefCell::new(Vec::new()) };
    /// True when the running program contains a `try`, so a runtime fault that
    /// names a JVM throwable is catchable rather than immediately fatal. Set by
    /// [`set_catchable`] before the run.
    static EXC_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Declare whether the program about to run contains a `try` (see
/// `EXC_ENABLED`). Called by the runner after compiling and before `VM::run`.
pub fn set_catchable(on: bool) {
    EXC_ENABLED.with(|e| e.set(on));
}

/// True while an exception is in flight and has not yet reached its handler.
/// Side-effecting builtins test this and become no-ops, so no output is produced
/// (and nothing further faults) during the walk out to the `catch`.
fn unwinding() -> bool {
    PENDING.with(|p| p.borrow().is_some())
}

/// Make `exc` the in-flight exception. In a program with a `try` this parks it
/// for the compiler-emitted unwind checks; without one it is uncatchable and
/// halts the run with the JVM's `Exception in thread "main" …` text.
///
/// A raise *while already unwinding* is dropped: the first exception wins, which
/// is also the JVM's rule (a second throw cannot occur there, since the
/// suppressed builtins never run).
fn raise(vm: &mut VM, exc: Value) {
    if unwinding() {
        return;
    }
    if EXC_ENABLED.with(|e| e.get()) {
        PENDING.with(|p| *p.borrow_mut() = Some(exc));
        return;
    }
    KT_ERROR.with(|e| {
        *e.borrow_mut() = Some(format!(
            "Exception in thread \"main\" {}",
            throwable_str(&exc)
        ))
    });
    vm.request_halt();
}

/// Parse a `java.lang.Xxx: message` fault string into a throwable, or `None`
/// when the message is a frontend-internal error rather than a Kotlin exception.
///
/// The recognizer is deliberately narrow — a fully-qualified `java.`/`kotlin.`
/// name whose last segment ends in `Exception` or `Error` — so a kotlinrs bug
/// ("unresolved reference: …") can never be silently swallowed by a user's
/// `catch`.
fn throwable_from_message(s: &str) -> Option<Value> {
    let (fqn, msg) = match s.split_once(": ") {
        Some((f, m)) => (f, Some(m)),
        None => (s, None),
    };
    let last = fqn.rsplit('.').next()?;
    let qualified = fqn.starts_with("java.") || fqn.starts_with("kotlin.");
    if !qualified || !(last.ends_with("Exception") || last.ends_with("Error")) {
        return None;
    }
    Some(new_throwable(fqn, msg))
}

/// Allocate a throwable with the fully-qualified class name `fqn`.
fn new_throwable(fqn: &str, msg: Option<&str>) -> Value {
    alloc(HeapObj::Exc {
        class: fqn.to_string(),
        msg: msg.map(|m| m.to_string()),
    })
}

/// The throwables kotlinrs can construct or raise, each mapped to its
/// fully-qualified JVM name. A table rather than a `java.lang.` prefix rule
/// because the package differs per class (`java.util.NoSuchElementException`)
/// and the package is observable through `toString`.
pub const BUILTIN_THROWABLES: &[(&str, &str)] = &[
    ("Throwable", "java.lang.Throwable"),
    ("Exception", "java.lang.Exception"),
    ("Error", "java.lang.Error"),
    ("RuntimeException", "java.lang.RuntimeException"),
    ("ArithmeticException", "java.lang.ArithmeticException"),
    (
        "IllegalArgumentException",
        "java.lang.IllegalArgumentException",
    ),
    ("IllegalStateException", "java.lang.IllegalStateException"),
    ("NumberFormatException", "java.lang.NumberFormatException"),
    (
        "IndexOutOfBoundsException",
        "java.lang.IndexOutOfBoundsException",
    ),
    (
        "StringIndexOutOfBoundsException",
        "java.lang.StringIndexOutOfBoundsException",
    ),
    (
        "ArrayIndexOutOfBoundsException",
        "java.lang.ArrayIndexOutOfBoundsException",
    ),
    ("NullPointerException", "java.lang.NullPointerException"),
    ("ClassCastException", "java.lang.ClassCastException"),
    (
        "UnsupportedOperationException",
        "java.lang.UnsupportedOperationException",
    ),
    (
        "NegativeArraySizeException",
        "java.lang.NegativeArraySizeException",
    ),
    ("NoSuchElementException", "java.util.NoSuchElementException"),
    // `TODO()`'s throwable. It is Kotlin's own, not the JVM's, so it keeps the
    // `kotlin.` package its `toString` prints.
    ("NotImplementedError", "kotlin.NotImplementedError"),
    // `java.util.Formatter`'s faults. Their MESSAGES were already produced
    // before these rows existed, which is precisely the defect: a fault raised
    // under a class the hierarchy has never heard of falls straight through to
    // `Throwable`, so `catch (e: IllegalArgumentException)` — which the JVM
    // answers YES to for every one of them — saw nothing. Measured on JDK
    // 21.0.12: `try { "%q".format(1) } catch (e: IllegalArgumentException)`
    // catches.
    ("IllegalFormatException", "java.util.IllegalFormatException"),
    (
        "UnknownFormatConversionException",
        "java.util.UnknownFormatConversionException",
    ),
    (
        "IllegalFormatConversionException",
        "java.util.IllegalFormatConversionException",
    ),
    (
        "MissingFormatArgumentException",
        "java.util.MissingFormatArgumentException",
    ),
    (
        "IllegalFormatWidthException",
        "java.util.IllegalFormatWidthException",
    ),
    (
        "IllegalFormatPrecisionException",
        "java.util.IllegalFormatPrecisionException",
    ),
    (
        "FormatFlagsConversionMismatchException",
        "java.util.FormatFlagsConversionMismatchException",
    ),
    // Runaway recursion. An `Error` under `VirtualMachineError`, so
    // `catch (e: Exception)` does NOT claim it and `catch (e: Throwable)` does
    // — see [`NESTED_RUN_LIMIT`].
    ("VirtualMachineError", "java.lang.VirtualMachineError"),
    ("StackOverflowError", "java.lang.StackOverflowError"),
];

/// The JVM throwable hierarchy kotlinrs models, as `(class, superclass)` simple
/// names. `catch (e: T)` matches when the thrown class is `T` or reaches `T` by
/// walking this chain — that is what makes `catch (e: Exception)` catch an
/// `IllegalArgumentException`.
const THROWABLE_PARENTS: &[(&str, &str)] = &[
    ("Error", "Throwable"),
    ("Exception", "Throwable"),
    ("RuntimeException", "Exception"),
    ("ArithmeticException", "RuntimeException"),
    ("IllegalArgumentException", "RuntimeException"),
    ("IllegalStateException", "RuntimeException"),
    ("NumberFormatException", "IllegalArgumentException"),
    ("IndexOutOfBoundsException", "RuntimeException"),
    (
        "StringIndexOutOfBoundsException",
        "IndexOutOfBoundsException",
    ),
    (
        "ArrayIndexOutOfBoundsException",
        "IndexOutOfBoundsException",
    ),
    ("NullPointerException", "RuntimeException"),
    ("ClassCastException", "RuntimeException"),
    ("UnsupportedOperationException", "RuntimeException"),
    ("NegativeArraySizeException", "RuntimeException"),
    ("NoSuchElementException", "RuntimeException"),
    // An `Error`, NOT an `Exception` — which is why `catch (e: Exception)` does
    // not catch what `TODO()` throws and `catch (e: Throwable)` does.
    ("NotImplementedError", "Error"),
    // `java.util.IllegalFormatException` extends `IllegalArgumentException`,
    // and every Formatter fault extends it. That one row is what makes a
    // format fault catchable at the three levels the JVM catches it at
    // (`IllegalArgumentException`, `RuntimeException`, `Exception`).
    ("IllegalFormatException", "IllegalArgumentException"),
    ("UnknownFormatConversionException", "IllegalFormatException"),
    ("IllegalFormatConversionException", "IllegalFormatException"),
    ("MissingFormatArgumentException", "IllegalFormatException"),
    ("IllegalFormatWidthException", "IllegalFormatException"),
    ("IllegalFormatPrecisionException", "IllegalFormatException"),
    (
        "FormatFlagsConversionMismatchException",
        "IllegalFormatException",
    ),
    ("VirtualMachineError", "Error"),
    ("StackOverflowError", "VirtualMachineError"),
];

/// The fully-qualified name of a throwable's simple name, or `None` when the
/// name is not one kotlinrs models.
pub fn throwable_fqn(simple: &str) -> Option<&'static str> {
    BUILTIN_THROWABLES
        .iter()
        .find(|(s, _)| *s == simple)
        .map(|(_, f)| *f)
}

/// Whether a thrown class (simple name) is an instance of the caught type
/// `want`, by walking [`THROWABLE_PARENTS`]. The chain is short and fixed, so a
/// linear walk beats any index.
fn throwable_is_a(thrown: &str, want: &str) -> bool {
    let mut cur = thrown;
    loop {
        if cur == want {
            return true;
        }
        match THROWABLE_PARENTS.iter().find(|(c, _)| *c == cur) {
            Some((_, parent)) => cur = parent,
            None => return false,
        }
    }
}

/// The built-in throwable ancestry of `simple`, nearest first and including
/// `simple` itself (`"IllegalStateException"` → `["IllegalStateException",
/// "RuntimeException", "Exception", "Throwable"]`). The compiler publishes it
/// with a user class's own supertypes so `catch (e: Exception)` claims a user
/// class declared `: IllegalStateException(…)`.
pub fn throwable_ancestry(simple: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut cur = simple;
    while let Some((s, _)) = BUILTIN_THROWABLES.iter().find(|(s, _)| *s == cur) {
        out.push(*s);
        match THROWABLE_PARENTS.iter().find(|(c, _)| *c == cur) {
            Some((_, parent)) => cur = parent,
            None => break,
        }
    }
    out
}

/// The simple class name used for `catch` matching: a built-in throwable's last
/// path segment, or a user class's declared name when its ancestry reaches
/// `Throwable`.
fn thrown_class(v: &Value) -> Option<String> {
    with_obj(v, |o| match o {
        HeapObj::Exc { class, .. } => Some(
            class
                .rsplit('.')
                .next()
                .unwrap_or(class.as_str())
                .to_string(),
        ),
        HeapObj::Instance { class, .. } if type_is_throwable(class) => Some(class.clone()),
        _ => None,
    })
    .flatten()
}

/// Whether `class` is an `enum` constant's (see [`KT_ENUM_REG`]).
fn is_enum_class(class: &str) -> bool {
    ENUM_CLASSES.with(|s| s.borrow().contains(class))
}

/// The `ordinal` an enum constant carries, or `None` for anything else. Backs
/// the `Comparable` ordering every enum has.
fn enum_ordinal(v: &Value) -> Option<i64> {
    with_obj(v, |o| match o {
        HeapObj::Instance { class, fields, .. } if is_enum_class(class) => fields
            .iter()
            .find(|(n, _)| n == "ordinal")
            .map(|(_, v)| num_of(v)),
        _ => None,
    })
    .flatten()
}

/// `String.toInt`/`toLong` and their `…OrNull` forms, honouring the optional
/// radix argument. `None` is the parse failure the two pairs report differently.
fn parse_radix(s: &str, args: &[Value], bits: u32) -> Result<Value, ParseFail> {
    let radix = match args.first() {
        Some(v) => num_of(v),
        None => 10,
    };
    // The radix is checked BEFORE the string, and the fault is not a
    // `NumberFormatException` at all — which is why the `…OrNull` forms raise
    // it instead of answering null.
    if !(2..=36).contains(&radix) {
        return Err(ParseFail::Radix(format!(
            "java.lang.IllegalArgumentException: radix {radix} was not in valid range 2..36"
        )));
    }
    // No trimming: `Integer.parseInt` rejects surrounding whitespace, unlike
    // `Double.parseDouble`, so `" 5".toInt()` throws where `" 5.0".toDouble()`
    // is `5.0`.
    let malformed = || {
        ParseFail::Number(format!(
            "java.lang.NumberFormatException: For input string: \"{s}\"{}",
            if radix == 10 {
                String::new()
            } else {
                format!(" under radix {radix}")
            }
        ))
    };
    // `Byte`/`Short` parse through `Integer.parseInt` and range-check after, so
    // a value past `Int` reports the MALFORMED message and only one inside it
    // reports the out-of-range one.
    let wide = i64::from_str_radix(s, radix as u32).map_err(|_| malformed())?;
    let fits = |b: u32| b >= 64 || (wide >= -(1i64 << (b - 1)) && wide < (1i64 << (b - 1)));
    if fits(bits) {
        return Ok(Value::Int(wide));
    }
    if !fits(32) {
        return Err(malformed());
    }
    Err(ParseFail::Number(format!(
        "java.lang.NumberFormatException: Value out of range. Value:\"{s}\" Radix:{radix}"
    )))
}

/// The bit width the `String.toXxx` member named `name` parses into.
fn int_width_of(name: &str) -> u32 {
    match name {
        "toByte" => 8,
        "toShort" => 16,
        "toLong" => 64,
        _ => 32,
    }
}

/// Why an integer parse failed, which decides whether an `…OrNull` form
/// swallows it.
enum ParseFail {
    /// The radix was outside `2..36`. Kotlin's `checkRadix` runs before the
    /// string is looked at, so even `toIntOrNull` raises this one.
    Radix(String),
    /// The string is not a number of the requested width. The `…OrNull` forms
    /// answer null here.
    Number(String),
}

/// An integer in `radix`, with the sign written out in front — the form
/// `Int.toString(radix)` produces, where `(-255).toString(16)` is `-ff`.
fn to_radix(n: i64, radix: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    // Accumulated over the ABSOLUTE value as `i128`, so `Long.MIN_VALUE` (whose
    // magnitude has no `i64`) is not a special case.
    let neg = n < 0;
    let mut m = (n as i128).abs();
    let mut out = Vec::new();
    while m > 0 {
        out.push(std::char::from_digit((m % radix as i128) as u32, radix).unwrap_or('0'));
        m /= radix as i128;
    }
    if neg {
        out.push('-');
    }
    out.iter().rev().collect()
}

/// Whether the in-flight throwable `v` is an instance of the caught type `want`.
/// A built-in walks the fixed [`THROWABLE_PARENTS`] chain; a user class walks the
/// supertypes it registered at startup, which already end in that chain.
fn throwable_matches(v: &Value, want: &str) -> bool {
    with_obj(v, |o| match o {
        HeapObj::Instance { class, .. } if type_is_throwable(class) => type_is_a(class, want),
        _ => false,
    })
    .unwrap_or(false)
        || thrown_class(v).is_some_and(|c| throwable_is_a(&c, want))
}

/// A throwable's `toString()`: `fqn` alone when the message is null, else
/// `fqn: message` (`java.lang.Throwable.toString`). A user class extending one
/// renders under its own (unqualified — kotlinrs compiles a single default-package
/// file) name and its stored `message`. A non-throwable value falls back to its
/// ordinary Kotlin display form.
fn throwable_str(v: &Value) -> String {
    with_obj(v, |o| match o {
        HeapObj::Exc { class, msg } => Some(match msg {
            Some(m) => format!("{class}: {m}"),
            None => class.clone(),
        }),
        HeapObj::Instance { class, fields, .. } if type_is_throwable(class) => {
            Some(match fields.iter().find(|(n, _)| n == "message") {
                Some((_, Value::Undef)) | None => class.clone(),
                Some((_, m)) => format!("{class}: {}", kotlin_string(m)),
            })
        }
        _ => None,
    })
    .flatten()
    .unwrap_or_else(|| kotlin_string(v))
}

/// `KT_EXC_NEW` — see [`KT_EXC_NEW`].
fn b_exc_new(vm: &mut VM, _argc: u8) -> Value {
    let msg = vm.pop();
    let class = vm.pop().to_str();
    let msg = match msg {
        Value::Undef => None,
        other => Some(kotlin_string(&other)),
    };
    new_throwable(&class, msg.as_deref())
}

/// `KT_EXC_THROW` — see [`KT_EXC_THROW`].
fn b_exc_throw(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.pop();
    raise(vm, v);
    Value::Undef
}

/// `KT_EXC_PENDING` — see [`KT_EXC_PENDING`].
fn b_exc_pending(_vm: &mut VM, _argc: u8) -> Value {
    Value::Bool(unwinding())
}

/// `KT_EXC_MATCH` — see [`KT_EXC_MATCH`].
fn b_exc_match(vm: &mut VM, _argc: u8) -> Value {
    let want = vm.pop().to_str();
    let thrown = PENDING.with(|p| p.borrow().clone());
    Value::Bool(match thrown {
        // `catch (e: Throwable)` catches everything, including a value outside
        // the modeled hierarchy.
        Some(_) if want == "Throwable" => true,
        Some(v) => throwable_matches(&v, &want),
        None => false,
    })
}

/// `KT_EXC_TAKE` — see [`KT_EXC_TAKE`].
fn b_exc_take(_vm: &mut VM, _argc: u8) -> Value {
    PENDING
        .with(|p| p.borrow_mut().take())
        .unwrap_or(Value::Undef)
}

/// `KT_EXC_DEPTH` — see [`KT_EXC_DEPTH`].
fn b_exc_depth(vm: &mut VM, _argc: u8) -> Value {
    Value::Int(vm.stack.len() as i64)
}

/// `KT_EXC_CUT` — see [`KT_EXC_CUT`].
fn b_exc_cut(vm: &mut VM, _argc: u8) -> Value {
    let depth = vm.pop().to_int().max(0) as usize;
    if depth <= vm.stack.len() {
        vm.stack.truncate(depth);
    }
    Value::Undef
}

/// `KT_EXC_STASH` — see [`KT_EXC_STASH`].
fn b_exc_stash(_vm: &mut VM, _argc: u8) -> Value {
    let held = PENDING.with(|p| p.borrow_mut().take());
    STASH.with(|s| s.borrow_mut().push(held));
    Value::Undef
}

/// `KT_EXC_UNSTASH` — see [`KT_EXC_UNSTASH`].
fn b_exc_unstash(_vm: &mut VM, _argc: u8) -> Value {
    let parked = STASH.with(|s| s.borrow_mut().pop()).flatten();
    // An exception raised by the finalizer itself replaces the parked one.
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            *p = parked;
        }
    });
    Value::Undef
}

/// `KT_EXC_ABORT` — see [`KT_EXC_ABORT`].
fn b_exc_abort(vm: &mut VM, _argc: u8) -> Value {
    let exc = PENDING
        .with(|p| p.borrow_mut().take())
        .unwrap_or(Value::Undef);
    KT_ERROR.with(|e| {
        *e.borrow_mut() = Some(format!(
            "Exception in thread \"main\" {}",
            throwable_str(&exc)
        ))
    });
    vm.request_halt();
    Value::Undef
}

/// `KT_PRINTLN` / `KT_PRINT` — the suppressible print builtins (see
/// [`KT_PRINTLN`]). The compiler has already coerced the argument to its Kotlin
/// display form, so the value is printed verbatim.
fn b_println(vm: &mut VM, argc: u8) -> Value {
    let arg = if argc > 0 { Some(vm.pop()) } else { None };
    if !unwinding() {
        match arg {
            Some(v) => println!("{}", v.to_str()),
            None => println!(),
        }
    }
    Value::Undef
}

fn b_print(vm: &mut VM, argc: u8) -> Value {
    let arg = if argc > 0 { Some(vm.pop()) } else { None };
    if !unwinding() {
        if let Some(v) = arg {
            print!("{}", v.to_str());
        }
    }
    Value::Undef
}

/// `java.lang.Math.round(double)` — a port of the JDK's algorithm, not a
/// rewrite of its documentation.
///
/// The one-line spelling `floor(x + 0.5)` is the doc's *description* and it is
/// not what the JDK computes; the difference is observable. `0.5` added to
/// `0.49999999999999994` — the double just below one half — rounds UP to
/// exactly `1.0` in the addition itself, so the floor answers 1 where the JVM
/// answers 0. JDK 8's `Math.round` had that very bug (JDK-8010430); the fix
/// replaced the addition with the bit manipulation ported here, so no JVM in
/// this frontend's supported range computes the floor form.
///
/// The JDK body, transcribed: read the biased exponent, work out how far the
/// significand must shift to leave the value with one fractional bit, restore
/// the implicit leading bit, apply the sign, then add one and shift that bit
/// away — an exact half-up rounding with no rounding error of its own. A shift
/// outside `0..64` means the value is already integral (or too large to be
/// anything else), which the plain cast handles, and that same branch is what
/// makes NaN answer 0 and the infinities saturate.
fn java_math_round(a: f64) -> i64 {
    const SIGNIFICAND_WIDTH: i64 = 53;
    const EXP_BIAS: i64 = 1023;
    const EXP_BIT_MASK: u64 = 0x7FF0_0000_0000_0000;
    const SIGNIF_BIT_MASK: u64 = 0x000F_FFFF_FFFF_FFFF;

    let bits = a.to_bits();
    let biased_exp = ((bits & EXP_BIT_MASK) >> (SIGNIFICAND_WIDTH - 1)) as i64;
    let shift = (SIGNIFICAND_WIDTH - 2 + EXP_BIAS) - biased_exp;
    if (shift & -64) == 0 {
        let mut r = ((bits & SIGNIF_BIT_MASK) | (SIGNIF_BIT_MASK + 1)) as i64;
        if (bits as i64) < 0 {
            r = -r;
        }
        ((r >> shift) + 1) >> 1
    } else {
        // `as i64` in Rust saturates and maps NaN to 0, which is exactly what
        // the JVM's `d2l` does.
        a as i64
    }
}

/// `java.lang.Double.parseDouble`, which is NOT Rust's `f64::from_str`.
///
/// The two grammars differ in both directions, and every difference below was
/// measured on kotlinc 2.4.10 / JDK 21.0.12:
///
///   * **Java accepts a type suffix.** `"1.5d"`, `"1.5D"`, `"1.5f"` and
///     `"1.5F"` all parse to `1.5`; Rust rejects every one of them. Exactly one
///     suffix character is allowed, so `"1.5dd"` still fails.
///   * **Java accepts hexadecimal floating point.** `"0x1p3"` is 8.0 and
///     `"0X1.8p1"` is 3.0 — the `p` exponent is a power of TWO and is
///     mandatory. Rust has no such literal.
///   * **Rust accepts short infinity spellings.** `"inf"` and `"infinity"`
///     parse in Rust; Java's grammar admits only the exact word `Infinity`
///     (with an optional sign), so `"inf".toDouble()` must fault.
///   * **The whitespace they strip differs.** Java trims characters `<= ' '`,
///     which is ASCII only; Rust's `str::trim` strips Unicode whitespace and
///     would have accepted a no-break space around the number.
fn java_parse_double(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c <= ' ');
    // At most one `d`/`D`/`f`/`F` suffix, and never on its own.
    let core = match t.chars().last() {
        Some('d' | 'D' | 'f' | 'F') if t.len() > 1 => &t[..t.len() - 1],
        _ => t,
    };
    let (sign, body) = match core.strip_prefix('-') {
        Some(b) => (-1.0, b),
        None => (1.0, core.strip_prefix('+').unwrap_or(core)),
    };
    if body == "Infinity" {
        return Some(sign * f64::INFINITY);
    }
    if body == "NaN" {
        return Some(f64::NAN);
    }
    if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        return java_parse_hex_double(hex).map(|v| sign * v);
    }
    // The decimal grammar, which Rust's parser is a superset of: reject the
    // spellings Rust adds (`inf`, `infinity`, `nan` in any case) by requiring a
    // digit, and let `from_str` do the rounding.
    if !body.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    if body
        .bytes()
        .any(|b| !matches!(b, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    body.parse::<f64>().ok().map(|v| sign * v)
}

/// The body of a Java hex-float literal, after `0x`/`0X` and the sign.
/// `hexDigits[.hexDigits]` then a MANDATORY binary exponent `p[+-]digits`.
fn java_parse_hex_double(hex: &str) -> Option<f64> {
    let (mantissa, exp) = hex.split_once(['p', 'P'])?;
    let exp: i32 = exp.parse().ok()?;
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let mut value = 0.0f64;
    for c in int_part.chars() {
        value = value * 16.0 + f64::from(c.to_digit(16)?);
    }
    let mut scale = 1.0f64 / 16.0;
    for c in frac_part.chars() {
        value += f64::from(c.to_digit(16)?) * scale;
        scale /= 16.0;
    }
    Some(value * 2f64.powi(exp))
}

/// UTF-16 code units back to text, substituting an UNPAIRED SURROGATE the way
/// the JVM's UTF-8 encoder does.
///
/// A `String` member that cuts between the halves of a surrogate pair leaves
/// one behind — `"\u{1f600}b".drop(1)` is `[DC00, 0062]` — and Kotlin's
/// `String` holds it, because a JVM string is a code-unit sequence and not a
/// scalar sequence. Rust's `String` cannot, so the unit has to become
/// something on the way in. `from_utf16_lossy` picks `U+FFFD`, which prints as
/// three bytes the JVM never writes; the JVM's own encoder picks `?`, so that
/// is what this picks.
///
/// This is the ENCODER's substitution, not surrogate support: `[DC00]` still
/// loses its identity here, and `.code` on it answers `0x3F` where the JVM
/// answers `0xDC00`. Holding the unit would take a UTF-16 string
/// representation throughout.
fn utf16_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or('?'))
        .collect()
}

/// `java.lang.Math.max(double, double)`, which is NOT `f64::max`.
///
/// Rust's `max` IGNORES a NaN operand and answers the other one; Java's
/// propagates it. They also disagree on the signed zeros, where Java is
/// explicit that `max(-0.0, 0.0)` is `+0.0` and Rust's is unspecified. Both
/// clauses are transcribed from the JDK body.
fn java_fmax(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && a.is_sign_negative() {
        return b;
    }
    if a >= b {
        a
    } else {
        b
    }
}

/// `java.lang.Math.min(double, double)` — see [`java_fmax`].
fn java_fmin(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        return a;
    }
    if a == 0.0 && b == 0.0 && b.is_sign_negative() {
        return b;
    }
    if a <= b {
        a
    } else {
        b
    }
}

/// Kotlin's `Char.isWhitespace()`, which is NOT Rust's `char::is_whitespace`.
///
/// Kotlin defines it as `Character.isWhitespace(c) || Character.isSpaceChar(c)`
/// — the union of Java's two predicates — and that set differs from Unicode's
/// `White_Space` property, which is what Rust implements, at both ends:
///
///   * `U+001C`–`U+001F` (FS, GS, RS, US) are whitespace to Java and to nobody
///     else. Rust answers false.
///   * `U+0085` (NEL) has `White_Space=Yes`, so Rust answers true. Java's
///     `isWhitespace` excludes it and `isSpaceChar` does not cover it, so
///     Kotlin answers false.
///
/// Both were measured on kotlinc 2.4.10 / JDK 21.0.12. The rest is the three
/// separator categories (`Zs`/`Zl`/`Zp`, including the no-break spaces that
/// `isWhitespace` alone would drop) plus the C0 run `U+0009`–`U+000D`. The
/// separators are enumerated rather than derived because Rust's standard
/// library exposes no general-category query.
fn kotlin_is_whitespace(c: char) -> bool {
    matches!(c,
        '\u{9}'..='\u{d}'          // TAB, LF, VT, FF, CR
        | '\u{1c}'..='\u{1f}'      // FS, GS, RS, US
        | '\u{20}'                 // SPACE                     Zs
        | '\u{a0}'                 // NO-BREAK SPACE            Zs
        | '\u{1680}'               // OGHAM SPACE MARK          Zs
        | '\u{2000}'..='\u{200a}'  // EN QUAD … HAIR SPACE      Zs
        | '\u{2028}'               // LINE SEPARATOR            Zl
        | '\u{2029}'               // PARAGRAPH SEPARATOR       Zp
        | '\u{202f}'               // NARROW NO-BREAK SPACE     Zs
        | '\u{205f}'               // MEDIUM MATHEMATICAL SPACE Zs
        | '\u{3000}'               // IDEOGRAPHIC SPACE         Zs
    )
}

/// `String.trim()` / `trimStart()` / `trimEnd()` / `isBlank()` all key on
/// [`kotlin_is_whitespace`], so none of them may use Rust's `str::trim`.
fn kotlin_trim(s: &str, start: bool, end: bool) -> &str {
    let mut out = s;
    if start {
        out = out.trim_start_matches(kotlin_is_whitespace);
    }
    if end {
        out = out.trim_end_matches(kotlin_is_whitespace);
    }
    out
}

/// The `NegativeArraySizeException` a negative array length raises.
///
/// The JVM puts the REQUESTED LENGTH in the detail message — `anewarray`'s
/// throw has carried it since well before this frontend's floor, and JDK 17,
/// 21 and 26 were each measured answering `java.lang.NegativeArraySizeException:
/// -5` for `IntArray(-5)`. The bare form this used to raise is not an older
/// dialect that a version gate could have caught; NO JVM produces it.
fn negative_array_size(n: i64) -> String {
    format!("java.lang.NegativeArraySizeException: {n}")
}

/// `KT_ARRAY_INIT` — see [`KT_ARRAY_INIT`]. An empty `desc` means the generic
/// `Array(n) { … }`, whose descriptor comes from the produced elements.
fn b_array_init(vm: &mut VM, _argc: u8) -> Value {
    let clo = vm.pop();
    let desc = vm.pop().to_str();
    let n = vm.pop().to_int();
    if n < 0 {
        fault(vm, negative_array_size(n));
        return Value::Undef;
    }
    let mut items = Vec::with_capacity(n as usize);
    for i in 0..n {
        match invoke_closure(vm, &clo, &[Value::Int(i)]) {
            Ok(v) => items.push(v),
            Err(e) => {
                fault(vm, e);
                return Value::Undef;
            }
        }
    }
    let desc = if desc.is_empty() {
        array_desc(&items)
    } else {
        desc
    };
    alloc(HeapObj::Array { items, desc })
}

/// Allocate `obj` on the heap and return its handle.
///
/// Handles are dealt out from `0` upward and the top 64 K of the space is
/// reserved for [`CHAR_TAG`], so the allocator stops before it would mint a
/// handle that reads back as a `Char`.
fn alloc(obj: HeapObj) -> Value {
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        let id = h.len() as u32;
        if id >= CHAR_TAG {
            return Value::Undef;
        }
        h.push(obj);
        Value::Obj(id)
    })
}

/// Run `f` with a shared borrow of heap object `id` (if the handle is live).
/// A `Char` handle is never live — it points into the reserved [`CHAR_TAG`]
/// region above the heap's length — so char-carrying values fall through to
/// each caller's non-object branch unless that caller handles them first.
fn with_obj<T>(v: &Value, f: impl FnOnce(&HeapObj) -> T) -> Option<T> {
    let Value::Obj(id) = v else { return None };
    HEAP.with(|h| h.borrow().get(*id as usize).map(f))
}

/// Run `f` with a mutable borrow of heap object `id` (if the handle is live).
fn with_obj_mut<T>(v: &Value, f: impl FnOnce(&mut HeapObj) -> T) -> Option<T> {
    let Value::Obj(id) = v else { return None };
    HEAP.with(|h| h.borrow_mut().get_mut(*id as usize).map(f))
}

// ── kotlin.Char ─────────────────────────────────────────────────────────────
//
// A Kotlin `Char` is a UTF-16 code unit — exactly 65 536 values — and it is a
// type of its own: `'a'` is not `97`, `'a' + 1` is `'b'` (not `98`), and
// `println(listOf('a'))` prints `[a]`. `fusevm::Value` has no `Char` variant, so
// the representation has to come out of a variant that already exists without
// colliding with the values Kotlin puts there.
//
// It is the top 64 K of the `Value::Obj` handle space: `Value::Obj(CHAR_TAG |
// code)`. Three properties fall out of that choice, and together they are what
// make a real `Char` affordable:
//
// - **No allocation and no interning.** The handle *is* the character, so
//   `'a'` built in two places is the same handle — `Op::NumEq` on two chars is
//   still a native integer compare, and a char works as a `Map`/`Set` key
//   through the existing handle-identity path.
// - **Disjoint from `Int`.** A `Char` is not a `Value::Int`, so no integer
//   value can be mistaken for one; `is Char` and `is Int` answer differently,
//   and a `Long` holding 97 never prints as `a`.
// - **Rejected, not coerced, by native arithmetic.** `Op::Add`/`Op::NumLt` on a
//   non-numeric operand delegate to the VM's [`fusevm::NumericHook`] (see
//   [`num_hook`]) instead of silently coercing it, which is what lets `it + 1`
//   inside a lambda — a statically untyped position, lowered to a *native* op —
//   do `Char` arithmetic. The same gate stops the JIT from compiling a block
//   whose slots hold a char, so a char never reaches native code as a 0.

/// The base of the reserved `Value::Obj` handle range that carries a `Char`.
pub const CHAR_TAG: u32 = 0xFFFF_0000;

/// The `Value` for code unit `code` (truncated to 16 bits, as the JVM does).
pub fn char_of(code: i64) -> Value {
    Value::Obj(CHAR_TAG | (code as u32 & 0xFFFF))
}

/// `Some(code unit)` when `v` is a `Char`.
pub fn char_code(v: &Value) -> Option<i64> {
    match v {
        Value::Obj(id) if *id >= CHAR_TAG => Some((*id & 0xFFFF) as i64),
        _ => None,
    }
}

/// Whether `v` is a `Char`.
fn is_char(v: &Value) -> bool {
    char_code(v).is_some()
}

/// The integral value of a number-like operand: a `Char`'s code unit, or the
/// value's own integer form.
fn num_of(v: &Value) -> i64 {
    char_code(v).unwrap_or_else(|| v.to_int())
}

/// The one-character string a `Char` displays as. An unpaired surrogate has no
/// `char` of its own; the JVM prints it as a lone code unit, which no Rust
/// `String` can hold, so it renders as the replacement character.
fn char_string(code: i64) -> String {
    char::from_u32(code as u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER)
        .to_string()
}

/// The Kotlin result of a native arithmetic or comparison op fusevm refused to
/// compute itself. Installed by [`install_numeric`] on a program that can build
/// a `Char`; see the module note above for why that is the seam.
///
/// Two cases reach here:
///
/// - a `Char` operand, which fusevm cannot add or order — the Kotlin rules are
///   `Char + Int`/`Char - Int` → `Char` (truncated to 16 bits, as `(char)` does
///   on the JVM), `Char - Char` → `Int`, and comparison by code unit;
/// - anything else fusevm's *strict* policy hands over — an `i64` overflow, or
///   a non-numeric operand. Both reproduce the non-strict result exactly
///   (wrapping arithmetic, `to_float` coercion), so installing the hook changes
///   nothing but `Char`.
fn num_hook(op: NumOp, a: &Value, b: &Value) -> Result<Value, String> {
    use NumOp::*;
    // Kotlin's `String.plus(Any?)`: `+` with a String operand concatenates, in
    // every position. The compiler emits `Op::Concat` where it can see a String
    // statically; this is the case where it cannot — `xs.fold("") { acc, c ->
    // acc + c }`, whose operands are both untyped.
    if op == Add && (matches!(a, Value::Str(_)) || matches!(b, Value::Str(_))) {
        return Ok(Value::str(format!(
            "{}{}",
            kotlin_string(a),
            kotlin_string(b)
        )));
    }
    if is_char(a) || is_char(b) {
        let (x, y) = (num_of(a), num_of(b));
        return Ok(match op {
            // `Char - Char` is the distance between two characters; every other
            // additive form keeps the Char side's type.
            Sub if is_char(a) && is_char(b) => Value::Int(x - y),
            Add | Sub => {
                let n = if op == Add { x + y } else { x - y };
                char_of(n)
            }
            Lt => Value::Bool(x < y),
            Gt => Value::Bool(x > y),
            Le => Value::Bool(x <= y),
            Ge => Value::Bool(x >= y),
            Eq => Value::Bool(x == y),
            Ne => Value::Bool(x != y),
            // Kotlin defines no `*`, `/`, `%`, `pow`, or unary `-` on `Char`, so
            // these are unreachable from a program `kotlinc` accepts; operating
            // on the code unit keeps them total rather than faulting.
            Mul => Value::Int(x * y),
            Div if y == 0 => return Err("java.lang.ArithmeticException: / by zero".to_string()),
            Div => Value::Int(x / y),
            Mod if y == 0 => return Err("java.lang.ArithmeticException: / by zero".to_string()),
            Mod => Value::Int(x % y),
            Pow => Value::Float((x as f64).powf(y as f64)),
            Neg => Value::Int(-x),
        });
    }
    // No Char in sight: reproduce fusevm's own non-strict result, so a program
    // that merely *could* build a char is not otherwise perturbed by the hook.
    let (fa, fb) = (a.to_float(), b.to_float());
    let ints = match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some((*x, *y)),
        _ => None,
    };
    Ok(match op {
        Add => match ints {
            Some((x, y)) => Value::Int(x.wrapping_add(y)),
            None => Value::Float(fa + fb),
        },
        Sub => match ints {
            Some((x, y)) => Value::Int(x.wrapping_sub(y)),
            None => Value::Float(fa - fb),
        },
        Mul => match ints {
            Some((x, y)) => Value::Int(x.wrapping_mul(y)),
            None => Value::Float(fa * fb),
        },
        Div => Value::Float(fa / fb),
        Mod => Value::Float(fa % fb),
        Pow => Value::Float(fa.powf(fb)),
        Neg => match a {
            Value::Int(x) => Value::Int(x.wrapping_neg()),
            _ => Value::Float(-fa),
        },
        Lt => Value::Bool(fa < fb),
        Gt => Value::Bool(fa > fb),
        Le => Value::Bool(fa <= fb),
        Ge => Value::Bool(fa >= fb),
        Eq => Value::Bool(fa == fb),
        Ne => Value::Bool(fa != fb),
    })
}

/// Take and clear any pending runtime-fault message.
pub fn take_error() -> Option<String> {
    KT_ERROR.with(|e| e.borrow_mut().take())
}

/// Stop the run with `msg` — or, when the message names a JVM throwable and the
/// program contains a `try`, raise it as a catchable exception instead.
///
/// Every runtime error the host can report flows through here, so routing the
/// throwable ones into [`raise`] is what makes `1 / 0`, `!!` on null, and an
/// out-of-range index catchable without touching each call site. A
/// *frontend-internal* fault (one whose message is not a `java.…`/`kotlin.…`
/// throwable) always halts: it is a kotlinrs gap, not a Kotlin exception, and
/// swallowing it in a `catch` would hide it. A fault raised while an exception
/// is already unwinding is dropped — it is computed on garbage operands the
/// abandoned statement left behind.
fn fault(vm: &mut VM, msg: impl Into<String>) {
    if unwinding() {
        return;
    }
    let msg = msg.into();
    let throwable = throwable_from_message(&msg).is_some();
    if throwable && EXC_ENABLED.with(|e| e.get()) {
        if let Some(exc) = throwable_from_message(&msg) {
            raise(vm, exc);
            return;
        }
    }
    // Uncatchable here (no `try` in the program, or a frontend-internal fault):
    // stop the run. A JVM throwable gets the report line `java` prints for one
    // that reached the top of `main`; a kotlinrs-internal message stays bare.
    let msg = if throwable {
        format!("Exception in thread \"main\" {msg}")
    } else {
        msg
    };
    KT_ERROR.with(|e| *e.borrow_mut() = Some(msg));
    vm.request_halt();
}

/// The Kotlin value coercions (`KT_TO_STRING`/`KT_IDIV`/`KT_IMOD`) that the
/// language-agnostic ops can't express. Shared by the normal and debug handlers.
/// `KT_DBG_LINE` is stack-neutral and handled by the caller (a no-op for normal
/// runs, the DAP hook under `--dap`).
fn handle_coercion(vm: &mut VM, id: u16, arg: u8) {
    match id {
        KT_FFI_COMPILE => {
            let body = vm.pop();
            let b64 = body.to_str();
            if let Err(e) = fusevm::ffi::compile_and_register(&b64) {
                fault(vm, format!("rust {{}} block: {e}"));
            }
        }
        KT_FFI_CALL => {
            // Stack: [arg0 .. arg{n-1}, name]; name on top.
            let name = vm.pop().to_str();
            let n = arg as usize;
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(vm.pop());
            }
            args.reverse();
            match fusevm::ffi::try_call(&name, &args) {
                Some(Ok(v)) => vm.push(v),
                Some(Err(e)) => {
                    fault(vm, format!("rust FFI call {name}: {e}"));
                    vm.push(Value::Undef);
                }
                None => {
                    fault(vm, format!("unresolved reference: {name}"));
                    vm.push(Value::Undef);
                }
            }
        }
        KT_METHOD => {
            // Stack: [recv, arg0 .. arg{n-1}, name]; name on top.
            let name = vm.pop().to_str();
            let n = arg as usize;
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(vm.pop());
            }
            args.reverse();
            let recv = vm.pop();
            match kt_method(vm, &recv, &name, &args) {
                Ok(v) => vm.push(v),
                Err(e) => {
                    fault(vm, e);
                    vm.push(Value::Undef);
                }
            }
        }
        KT_NEW => {
            // Stack: [metaStr, v0 .. v{n-1}]; n = arg.
            let n = arg as usize;
            let mut vals = Vec::with_capacity(n);
            for _ in 0..n {
                vals.push(vm.pop());
            }
            vals.reverse();
            let meta = vm.pop().to_str();
            let mut it = meta.split('\u{1f}');
            let class = it.next().unwrap_or("").to_string();
            let is_data = it.next() == Some("d");
            let data_len = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            register_widths(&class, it.next().unwrap_or(""));
            let fields: Vec<(String, Value)> = it.map(|s| s.to_string()).zip(vals).collect();
            vm.push(alloc(HeapObj::Instance {
                class,
                is_data,
                fields,
                // No superclass, so every field is this class's own.
                data_from: 0,
                data_len,
            }));
        }
        KT_TYPE_REG => {
            // Stack: [nameStr, supersCsvStr].
            let supers_csv = vm.pop().to_str();
            let name = vm.pop().to_str();
            let supers: Vec<String> = if supers_csv.is_empty() {
                Vec::new()
            } else {
                supers_csv.split(',').map(|s| s.to_string()).collect()
            };
            TYPES.with(|t| t.borrow_mut().insert(name, supers));
        }
        KT_EXTEND => {
            // Stack: [baseObj, metaStr, v0 .. v{n-1}]; n = arg (own fields).
            let n = arg as usize;
            let mut vals = Vec::with_capacity(n);
            for _ in 0..n {
                vals.push(vm.pop());
            }
            vals.reverse();
            let meta = vm.pop().to_str();
            let base = vm.pop();
            let mut it = meta.split('\u{1f}');
            let class = it.next().unwrap_or("").to_string();
            let is_data = it.next() == Some("d");
            let data_len = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            register_widths(&class, it.next().unwrap_or(""));
            // The base's fields come first, so a subclass's record reads
            // base-most first — the order the compiler's flattened property
            // table assumes.
            let mut fields = with_obj(&base, |o| match o {
                HeapObj::Instance { fields, .. } => fields.clone(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
            let data_from = fields.len();
            fields.extend(it.map(|s| s.to_string()).zip(vals));
            vm.push(alloc(HeapObj::Instance {
                class,
                is_data,
                fields,
                data_from,
                data_len,
            }));
        }
        KT_CLASSOF => {
            let v = vm.pop();
            vm.push(Value::str(instance_tag(&v).unwrap_or_default()));
        }
        KT_TOSTRING_REG => {
            // Stack: [tagStr, subNameIdx].
            let idx = vm.pop().to_int() as u16;
            let tag = vm.pop().to_str();
            TOSTRING_SUBS.with(|t| t.borrow_mut().insert(tag, idx));
        }
        KT_ENUM_REG => {
            let tag = vm.pop().to_str();
            ENUM_CLASSES.with(|s| s.borrow_mut().insert(tag));
        }
        KT_EQUALS_REG => {
            // Stack: [tagStr, subNameIdx].
            let idx = vm.pop().to_int() as u16;
            let tag = vm.pop().to_str();
            EQUALS_SUBS.with(|t| t.borrow_mut().insert(tag, idx));
        }
        KT_HASH_REG => {
            // Stack: [tagStr, subNameIdx].
            let idx = vm.pop().to_int() as u16;
            let tag = vm.pop().to_str();
            HASHCODE_SUBS.with(|t| t.borrow_mut().insert(tag, idx));
        }
        KT_BUILDER => {
            // Stack: [] or [arg]; `arg` is the argument count.
            let init = (arg == 1).then(|| vm.pop());
            let b = new_builder(init);
            vm.push(b);
        }
        KT_GETFIELD => {
            // Stack: [obj, nameStr].
            let name = vm.pop().to_str();
            let obj = vm.pop();
            let got = with_obj(&obj, |o| match o {
                HeapObj::Instance { fields, .. } => fields
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.clone()),
                _ => None,
            })
            .flatten();
            match got {
                Some(v) => vm.push(v),
                None => {
                    fault(vm, format!("unresolved reference: {name}"));
                    vm.push(Value::Undef);
                }
            }
        }
        KT_SETFIELD => {
            // Stack: [obj, value, nameStr].
            let name = vm.pop().to_str();
            let value = vm.pop();
            let obj = vm.pop();
            let ok = with_obj_mut(&obj, |o| match o {
                HeapObj::Instance { fields, .. } => {
                    if let Some(slot) = fields.iter_mut().find(|(n, _)| *n == name) {
                        slot.1 = value;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            })
            .unwrap_or(false);
            if !ok {
                fault(vm, format!("unresolved reference: {name}"));
            }
        }
        KT_LIST => {
            let list = alloc(HeapObj::List(pop_n(vm, arg)));
            vm.push(list);
        }
        KT_LIST_RO => {
            let list = alloc(HeapObj::List(pop_n(vm, arg)));
            tag_list_impl(&list, ListImpl::Literal);
            vm.push(list);
        }
        KT_LIST_TAG => {
            let tag = vm.pop().to_str();
            let list = vm.pop();
            let which = match tag.as_str() {
                "builder" => ListImpl::Builder,
                _ => ListImpl::Literal,
            };
            vm.push(tag_if_list(list, which));
        }
        KT_SET => {
            let n = arg as usize;
            let mut vals = Vec::with_capacity(n);
            for _ in 0..n {
                vals.push(vm.pop());
            }
            vals.reverse();
            let items = distinct(vm, &vals);
            vm.push(alloc(HeapObj::Set(items)));
        }
        KT_MAP => {
            // Stack: [pair0 .. pair{n-1}]; each a Pair handle.
            let n = arg as usize;
            let mut pairs = Vec::with_capacity(n);
            for _ in 0..n {
                pairs.push(vm.pop());
            }
            pairs.reverse();
            let mut entries: Vec<(Value, Value)> = Vec::with_capacity(n);
            for p in pairs {
                let kv = with_obj(&p, |o| match o {
                    HeapObj::Pair(a, b) => Some((a.clone(), b.clone())),
                    _ => None,
                })
                .flatten();
                if let Some((k, v)) = kv {
                    // Later duplicate keys overwrite (Kotlin `mapOf` semantics).
                    if let Some(slot) = entries.iter_mut().find(|(ek, _)| ek == &k) {
                        slot.1 = v;
                    } else {
                        entries.push((k, v));
                    }
                }
            }
            vm.push(alloc(HeapObj::Map(entries)));
        }
        // `a to b` / `Pair(a, b)` with `arg == 0`; `Triple(a, b, c)` with
        // `arg == 1`. One op because the two differ only in arity.
        KT_PAIR if arg == 1 => {
            let c = vm.pop();
            let b = vm.pop();
            let a = vm.pop();
            vm.push(alloc(HeapObj::Triple(a, b, c)));
        }
        KT_PAIR => {
            let b = vm.pop();
            let a = vm.pop();
            vm.push(alloc(HeapObj::Pair(a, b)));
        }
        KT_NEWLIST => {
            vm.push(alloc(HeapObj::List(Vec::new())));
        }
        KT_LISTPUSH => {
            let value = vm.pop();
            let list = vm.pop();
            with_obj_mut(&list, |o| {
                if let HeapObj::List(items) = o {
                    items.push(value);
                }
            });
        }
        KT_INDEX_GET => {
            let index = vm.pop();
            let recv = vm.pop();
            match index_get(vm, &recv, &index) {
                Ok(v) => vm.push(v),
                Err(e) => {
                    fault(vm, e);
                    vm.push(Value::Undef);
                }
            }
        }
        KT_INDEX_SET => {
            let value = vm.pop();
            let index = vm.pop();
            let recv = vm.pop();
            if let Err(e) = index_set(vm, &recv, &index, value) {
                fault(vm, e);
            }
        }
        KT_OBJEQ => {
            let b = vm.pop();
            let a = vm.pop();
            vm.push(Value::Bool(value_eq(&a, &b)));
        }
        KT_TO_STRING => {
            let v = vm.pop();
            // Checked, not bare: this is the coercion string interpolation and
            // `+` go through, so a cyclic structure reaches the render walk
            // here as readily as through `println`.
            RENDER_OVERFLOW.with(|f| f.set(false));
            let s = kotlin_string(&v);
            if RENDER_OVERFLOW.with(|f| f.replace(false)) {
                fault(vm, "java.lang.StackOverflowError");
            }
            vm.push(Value::str(s));
        }
        KT_IS => {
            // Stack: [value, typeName]; typeName on top.
            let ty = vm.pop().to_str();
            let v = vm.pop();
            vm.push(Value::Bool(value_is_type(&v, &ty)));
        }
        KT_LAZY_NEW => {
            let thunk = vm.pop();
            vm.push(alloc(HeapObj::Lazy { thunk, value: None }));
        }
        KT_AS => {
            let ty = vm.pop().to_str();
            let v = vm.pop();
            // Kotlin's `null as T?` succeeds and `null as? T` is null; the
            // parser drops the `?`, so a null value passes a safe cast and
            // fails an unsafe one exactly as the JVM's would.
            let matched = value_is_type(&v, &ty);
            if matched {
                vm.push(v);
            } else if arg == 1 {
                vm.push(Value::Undef);
            } else {
                fault(vm, cast_failure(&v, &ty));
                vm.push(Value::Undef);
            }
        }
        KT_CHR_STRING => {
            let v = vm.pop();
            // A `Char?` holding null renders as `null`, not as code point 0.
            if matches!(v, Value::Undef) {
                vm.push(Value::str("null"));
                return;
            }
            vm.push(Value::str(char_string(num_of(&v))));
        }
        KT_ISNULL => {
            let v = vm.pop();
            vm.push(Value::Bool(matches!(v, Value::Undef)));
        }
        KT_IDENTITY => {
            let b = vm.pop();
            let a = vm.pop();
            vm.push(Value::Bool(identical(&a, &b)));
        }
        KT_NOTNULL => {
            let v = vm.pop();
            if matches!(v, Value::Undef) {
                fault(vm, "java.lang.NullPointerException");
                vm.push(Value::Undef);
            } else {
                vm.push(v);
            }
        }
        KT_RANGE => {
            let end = vm.pop();
            let start = vm.pop();
            let form = match arg {
                1 => RangeForm::Until,
                2 => RangeForm::DownTo,
                _ => RangeForm::Inclusive,
            };
            // `'a'..'z'` is a `CharRange`, whose elements and printed form are
            // characters; the endpoints ride as code units either way.
            let is_char = is_char(&start) || is_char(&end);
            vm.push(alloc(HeapObj::Range(RangeObj::new(
                num_of(&start),
                num_of(&end),
                form,
                is_char,
            ))));
        }
        KT_RANGE_STEP => {
            let n = vm.pop().to_int();
            let recv = vm.pop();
            let base = with_obj(&recv, |o| match o {
                HeapObj::Range(r) => Some(*r),
                _ => None,
            })
            .flatten();
            match base {
                // Kotlin rejects a non-positive step at runtime; the sign of the
                // progression comes from the receiver, not from the argument.
                _ if n <= 0 => {
                    fault(
                        vm,
                        format!(
                            "java.lang.IllegalArgumentException: Step must be positive, was: {n}."
                        ),
                    );
                    vm.push(Value::Undef);
                }
                Some(r) => vm.push(alloc(HeapObj::Range(RangeObj {
                    step: if r.step < 0 { -n } else { n },
                    progression: true,
                    ..r
                }))),
                None => {
                    fault(
                        vm,
                        format!("unresolved reference: step on {}", obj_label(&recv)),
                    );
                    vm.push(Value::Undef);
                }
            }
        }
        KT_IN => {
            let container = vm.pop();
            let value = vm.pop();
            let has = contains_value(vm, &container, &value);
            vm.push(Value::Bool(has));
        }
        KT_ITER_SIZE => {
            let recv = vm.pop();
            match iter_len(&recv) {
                Some(n) => vm.push(Value::Int(n)),
                None => {
                    fault(
                        vm,
                        format!("for-in over a non-iterable value ({})", obj_label(&recv)),
                    );
                    vm.push(Value::Int(0));
                }
            }
        }
        KT_ITER_GET => {
            let i = vm.pop().to_int();
            let recv = vm.pop();
            vm.push(iter_at(&recv, i));
        }
        KT_ARRAY => {
            // The builder's own descriptor sits above the elements; an empty
            // one means `arrayOf`, which has no primitive type to name.
            let desc = vm.pop().to_str();
            let n = arg as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(vm.pop());
            }
            items.reverse();
            let desc = if desc.is_empty() {
                array_desc(&items)
            } else {
                desc
            };
            vm.push(alloc(HeapObj::Array { items, desc }));
        }
        KT_ARRAY_NEW => {
            let desc = vm.pop().to_str();
            let n = vm.pop().to_int();
            if n < 0 {
                fault(vm, negative_array_size(n));
                vm.push(Value::Undef);
            } else {
                // A primitive array is zero-filled: `0` for `[I`, `0.0` for `[D`,
                // `false` for `[Z`, `' '` for `[C` — matching JVM default
                // initialization. `[C`'s slot is a CHAR, not the integer 0, so
                // `CharArray(2)[0].code` resolves.
                let zero = match desc.as_str() {
                    "[D" => Value::Float(0.0),
                    "[Z" => Value::Bool(false),
                    "[C" => char_of(0),
                    _ => Value::Int(0),
                };
                vm.push(alloc(HeapObj::Array {
                    items: vec![zero; n as usize],
                    desc,
                }));
            }
        }
        KT_MATH => {
            let name = vm.pop().to_str();
            let n = arg as usize;
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(vm.pop());
            }
            args.reverse();
            match math_call(&name, &args) {
                Ok(v) => vm.push(v),
                Err(e) => {
                    fault(vm, e);
                    vm.push(Value::Undef);
                }
            }
        }
        KT_DDIV => {
            let b = vm.pop();
            let a = vm.pop();
            vm.push(Value::Float(a.to_float() / b.to_float()));
        }
        KT_IDIV => {
            let b = vm.pop();
            let a = vm.pop();
            if is_int(&a) && is_int(&b) {
                let d = b.to_int();
                if d == 0 {
                    fault(vm, "java.lang.ArithmeticException: / by zero");
                    vm.push(Value::Undef);
                } else {
                    vm.push(Value::Int(a.to_int().wrapping_div(d)));
                }
            } else {
                vm.push(Value::Float(a.to_float() / b.to_float()));
            }
        }
        KT_IMOD => {
            let b = vm.pop();
            let a = vm.pop();
            if is_int(&a) && is_int(&b) {
                let d = b.to_int();
                if d == 0 {
                    fault(vm, "java.lang.ArithmeticException: / by zero");
                    vm.push(Value::Undef);
                } else {
                    vm.push(Value::Int(a.to_int().wrapping_rem(d)));
                }
            } else {
                vm.push(Value::Float(a.to_float() % b.to_float()));
            }
        }
        KT_DBG_LINE => { /* marker: no-op on a normal run */ }
        _ => vm.push(Value::Undef),
    }
}

/// Register the Kotlin extension handler on a fresh VM (normal run). A
/// `KT_DBG_LINE` marker — present only in a `--dap` chunk — is a no-op here.
pub fn install(vm: &mut VM) {
    reset_heap();
    register_builtins(vm);
    vm.set_extension_handler(Box::new(handle_coercion));
}

/// Switch `vm` to fusevm's *strict* numeric policy, routing every arithmetic or
/// comparison op it cannot compute natively through `num_hook`.
///
/// This is what makes a real `Char` possible. A char is a `Value::Obj`, so
/// `'a' + 1` and `c < 'z'` — which lower to *native* `Op::Add`/`Op::NumLt` even
/// where the compiler cannot see a type, as with a lambda's `it` — would
/// otherwise be coerced to a number by fusevm's default awk-flavoured policy.
/// Under the strict policy they are handed back to Kotlin instead. The same
/// switch stops the JIT from compiling a block whose slots hold a non-number,
/// which is what keeps a char from reaching native code as a `0`.
///
/// It is installed unconditionally rather than only on a program that mentions a
/// char: a char can arrive from indexing or iterating *any* value that turns out
/// to be a `String`, so a syntactic test would have to be so broad it would
/// cover nearly every program anyway — and being wrong about it would be silent.
/// Int/Int arithmetic stays on fusevm's native fast path either way; only an
/// operand fusevm cannot compute on at all reaches `num_hook`.
pub fn install_numeric(vm: &mut VM) {
    vm.set_numeric_hook(std::sync::Arc::new(num_hook));
}

/// Register the lambda builtins (`Op::CallBuiltin` dispatch). Shared by the
/// normal and debug installs. These live in the VM's `builtin_table`, which
/// survives the re-entrant `vm.run()` a lambda invocation drives — see the
/// builtin-id doc comments above.
fn register_builtins(vm: &mut VM) {
    vm.register_builtin(KT_MAKE_CLOSURE, b_make_closure);
    vm.register_builtin(KT_CLOSURE_CALL, b_closure_call);
    vm.register_builtin(KT_COLL_HOF, b_coll_hof);
    vm.register_builtin(KT_SCOPE_FN, b_scope_fn);
    vm.register_builtin(KT_LAZY_GET, b_lazy_get);
    vm.register_builtin(KT_RUN_CATCHING, b_run_catching);
    vm.register_builtin(KT_RESULT_HOF, b_result_hof);
    vm.register_builtin(KT_ARRAY_INIT, b_array_init);
    vm.register_builtin(KT_PRINTLN, b_println);
    vm.register_builtin(KT_PRINT, b_print);
    vm.register_builtin(KT_DISPLAY, b_display);
    vm.register_builtin(KT_JOIN, b_join);
    vm.register_builtin(KT_OBJEQ_VM, b_objeq);
    vm.register_builtin(KT_METHOD_VM, b_method);
    vm.register_builtin(KT_SET_VM, b_set_new);
    vm.register_builtin(KT_IN_VM, b_in);
    vm.register_builtin(KT_INDEX_GET_VM, b_index_get);
    vm.register_builtin(KT_INDEX_SET_VM, b_index_set);
    vm.register_builtin(KT_MAP_VM, b_map_new);
    vm.register_builtin(KT_OPER_VM, b_operator);
    vm.register_builtin(KT_PRECOND, b_precond);
    vm.register_builtin(KT_COMPARATOR, b_comparator);
    vm.register_builtin(KT_GENSEQ, b_genseq);
    vm.register_builtin(KT_EXC_NEW, b_exc_new);
    vm.register_builtin(KT_EXC_THROW, b_exc_throw);
    vm.register_builtin(KT_EXC_PENDING, b_exc_pending);
    vm.register_builtin(KT_EXC_MATCH, b_exc_match);
    vm.register_builtin(KT_EXC_TAKE, b_exc_take);
    vm.register_builtin(KT_EXC_DEPTH, b_exc_depth);
    vm.register_builtin(KT_EXC_CUT, b_exc_cut);
    vm.register_builtin(KT_EXC_STASH, b_exc_stash);
    vm.register_builtin(KT_EXC_UNSTASH, b_exc_unstash);
    vm.register_builtin(KT_EXC_ABORT, b_exc_abort);
}

/// Register the debug extension handler on a fresh VM (`kotlin --dap`). Identical
/// to [`install`] for the value coercions, but a `KT_DBG_LINE` marker fires the
/// DAP line hook (breakpoint / step check) instead of being ignored.
pub fn install_debug(vm: &mut VM) {
    reset_heap();
    register_builtins(vm);
    install_numeric(vm);
    vm.set_extension_handler(Box::new(|vm, id, arg| {
        if id == KT_DBG_LINE {
            crate::dap::on_debug_line(vm);
        } else {
            handle_coercion(vm, id, arg);
        }
    }));
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Int(_))
}

// ── Ranges, arrays, iteration, and kotlin.math ──────────────────────────────

/// `value in container` for every container kind `in` accepts: numeric
/// membership in a range, element membership in a `List` or array, key
/// membership in a `Map`, and substring containment in a `String`.
fn contains_value(vm: &mut VM, container: &Value, value: &Value) -> bool {
    if let Value::Str(s) = container {
        return s.contains(&kotlin_string(value));
    }
    // A range answers without any element comparison. Everything else hands back
    // its elements so the search runs OUTSIDE the heap borrow: an `equals`
    // override allocates, and comparing under `with_obj`'s shared borrow would
    // panic the moment it did.
    enum Search {
        Range(bool),
        /// Elements, and whether membership is hash-gated (a `Set`/`Map` key).
        Elems(Vec<Value>, bool),
        No,
    }
    let search = with_obj(container, |o| match o {
        // `'x' in 'a'..'z'` compares code units, like every other range test.
        HeapObj::Range(r) => {
            Search::Range(r.is_char == is_char(value) && r.contains(num_of(value)))
        }
        HeapObj::List(items) | HeapObj::Array { items, .. } => Search::Elems(items.clone(), false),
        HeapObj::Set(items) => Search::Elems(items.clone(), true),
        HeapObj::Map(entries) => {
            Search::Elems(entries.iter().map(|(k, _)| k.clone()).collect(), true)
        }
        _ => Search::No,
    })
    .unwrap_or(Search::No);
    match search {
        Search::Range(hit) => hit,
        Search::Elems(items, hashed) => items.iter().any(|v| member_eq(vm, v, value, hashed)),
        Search::No => false,
    }
}

/// One element comparison for a container search.
///
/// `hashed` picks the container's rule, which Kotlin does NOT share between the
/// two families: a `List` compares with `equals` alone, while a `Set` or a `Map`
/// key reaches `equals` only once the hash buckets agree. The difference is
/// observable exactly when a class overrides one of the pair without the other —
/// `listOf(e).contains(e2)` is `true` while `setOf(e, e2).size` is `2` for a
/// class that defines `equals` but leaves `hashCode` identity.
fn member_eq(vm: &mut VM, a: &Value, b: &Value, hashed: bool) -> bool {
    if hashed {
        hash_eq_vm(vm, a, b)
    } else {
        equal_vm(vm, a, b)
    }
}

/// Element count of an iterable, or `None` when the value cannot be iterated —
/// which the general `for (v in …)` lowering reports as a fault rather than
/// silently looping zero times.
fn iter_len(recv: &Value) -> Option<i64> {
    // `for (c in "abc")` walks a String's UTF-16 code units — the same basis
    // `String.length` and `indexOf` use.
    if let Value::Str(s) = recv {
        return Some(s.encode_utf16().count() as i64);
    }
    with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Set(items) | HeapObj::Array { items, .. } => {
            Some(items.len() as i64)
        }
        HeapObj::Range(r) => Some(r.count()),
        // `for (e in map)` walks the entries.
        HeapObj::Map(entries) => Some(entries.len() as i64),
        HeapObj::Builder { units, .. } => Some(units.len() as i64),
        _ => None,
    })
    .flatten()
}

/// Element `i` of an iterable. Only called with an `i` the loop already bounded
/// by [`iter_len`], so an out-of-range index can only mean the collection was
/// mutated mid-loop; that yields `null` rather than faulting.
fn iter_at(recv: &Value, i: i64) -> Value {
    // A String yields `Char`s.
    if let Value::Str(s) = recv {
        return usize::try_from(i)
            .ok()
            .and_then(|i| s.encode_utf16().nth(i))
            .map(|u| char_of(u as i64))
            .unwrap_or(Value::Undef);
    }
    // A `Map` yields one `Map.Entry` per step, carried as a `Pair`. The entry is
    // cloned out from under the shared borrow first: `alloc` takes the heap
    // mutably, so building the pair inside `with_obj` would re-borrow it.
    let entry = with_obj(recv, |o| match o {
        HeapObj::Map(entries) => usize::try_from(i)
            .ok()
            .and_then(|i| entries.get(i).cloned()),
        _ => None,
    })
    .flatten();
    if let Some((k, v)) = entry {
        return alloc(HeapObj::Entry(k, v));
    }
    with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Set(items) | HeapObj::Array { items, .. } => {
            usize::try_from(i).ok().and_then(|i| items.get(i).cloned())
        }
        HeapObj::Range(r) => Some(r.value_at(i)),
        HeapObj::Builder { units, .. } => usize::try_from(i)
            .ok()
            .and_then(|i| units.get(i))
            .map(|u| char_of(*u as i64)),
        _ => None,
    })
    .flatten()
    .unwrap_or(Value::Undef)
}

/// The JVM type descriptor an `arrayOf(...)` call would produce, inferred from
/// the elements. `arrayOf` builds a boxed `Array<T>`, so the descriptor names
/// the boxed class; a mixed or empty literal widens to `Object`. This exists
/// only to reproduce the printed form (`[Ljava.lang.Integer;@1b6d3586`).
fn array_desc(items: &[Value]) -> String {
    let elem = match items.first() {
        Some(v) if is_char(v) => "java.lang.Character",
        Some(Value::Int(_)) => "java.lang.Integer",
        Some(Value::Float(_)) => "java.lang.Double",
        Some(Value::Str(_)) => "java.lang.String",
        Some(Value::Bool(_)) => "java.lang.Boolean",
        _ => "java.lang.Object",
    };
    let uniform = items.iter().all(|v| {
        elem == "java.lang.Character" && is_char(v)
            || matches!(
                (v, elem),
                (Value::Int(_), "java.lang.Integer")
                    | (Value::Float(_), "java.lang.Double")
                    | (Value::Str(_), "java.lang.String")
                    | (Value::Bool(_), "java.lang.Boolean")
            )
    });
    if uniform {
        format!("[L{elem};")
    } else {
        "[Ljava.lang.Object;".to_string()
    }
}

/// The `kotlin.math` / `java.lang.Math` functions the compiler routes here.
///
/// Overload selection is by runtime value kind, mirroring Kotlin's static
/// overload set: `abs`/`max`/`min` keep an `Int` result when every operand is
/// integral and widen to `Double` otherwise, while `sqrt`/`floor`/`ceil`/`round`
/// are `Double`-only.
///
/// `round` and `Math.round` deliberately differ, because they do in Kotlin:
/// `kotlin.math.round` is IEEE round-half-to-even and returns a `Double`
/// (`round(2.5) == 2.0`), whereas `java.lang.Math.round` is round-half-up and
/// returns a `Long` (`Math.round(2.5) == 3`).
fn math_call(name: &str, args: &[Value]) -> Result<Value, String> {
    let a = args.first().cloned().unwrap_or(Value::Int(0));
    match name {
        "abs" if is_int(&a) => Ok(Value::Int(a.to_int().wrapping_abs())),
        "abs" => Ok(Value::Float(a.to_float().abs())),
        // `kotlin.math.min`/`max` take two, but `minOf`/`maxOf` also have a
        // three-argument overload and a vararg one, so every argument counts —
        // `maxOf(1, 2, 3)` is 3, not the two-argument answer 2.
        "max" | "min" => {
            let want_max = name == "max";
            if args.iter().all(is_int) {
                let seed = a.to_int();
                Ok(Value::Int(args.iter().map(|v| v.to_int()).fold(
                    seed,
                    |acc, x| if (x > acc) == want_max { x } else { acc },
                )))
            } else {
                let seed = a.to_float();
                Ok(Value::Float(args.iter().map(|v| v.to_float()).fold(
                    seed,
                    |acc, x| {
                        if want_max {
                            java_fmax(acc, x)
                        } else {
                            java_fmin(acc, x)
                        }
                    },
                )))
            }
        }
        // `Math.floorDiv`/`Math.floorMod` and Kotlin's `Int.mod` — division that
        // rounds toward NEGATIVE INFINITY, unlike `/` and `%`, which truncate
        // toward zero. `-7 / 2` is -3 and `-7 % 2` is -1, where `floorDiv(-7, 2)`
        // is -4 and `floorMod(-7, 2)` is 1. The remainder's SIGN is the visible
        // difference: `%` follows the dividend, `mod`/`floorMod` the divisor.
        "floorDiv" | "floorMod" => {
            let b = args.get(1).cloned().unwrap_or(Value::Int(0));
            let (x, y) = (a.to_int(), b.to_int());
            if y == 0 {
                return Err("java.lang.ArithmeticException: / by zero".to_string());
            }
            Ok(Value::Int(if name == "floorDiv" {
                x.div_euclid(y) - i64::from(x.rem_euclid(y) != 0 && y < 0)
            } else {
                x - y * (x.div_euclid(y) - i64::from(x.rem_euclid(y) != 0 && y < 0))
            }))
        }
        // `kotlin.math.sign` — the SIGN as a `Double`, which keeps the sign of a
        // zero (`sign(-0.0)` is `-0.0`) and answers NaN for NaN. Rust's
        // `f64::signum` answers ±1.0 for ±0.0 and 1.0 for NaN, so it is not this.
        "sign" if !is_int(&a) => {
            let x = a.to_float();
            Ok(Value::Float(if x.is_nan() || x == 0.0 {
                x
            } else {
                x.signum()
            }))
        }
        // `Int.sign` / `Long.sign` — the same idea as an `Int`.
        "sign" => Ok(Value::Int(a.to_int().signum())),
        "sqrt" => Ok(Value::Float(a.to_float().sqrt())),
        "floor" => Ok(Value::Float(a.to_float().floor())),
        "ceil" => Ok(Value::Float(a.to_float().ceil())),
        "round" => Ok(Value::Float(a.to_float().round_ties_even())),
        "jround" => Ok(Value::Int(java_math_round(a.to_float()))),
        _ => Err(format!("unresolved reference: {name}")),
    }
}

// ── First-class lambdas ─────────────────────────────────────────────────────

/// `KT_MAKE_CLOSURE`: pop the capture count, parameter count, and body name
/// index, then the captured upvalue values (deepest-first), and register the
/// closure. Returns its `Value::Obj` handle.
fn b_make_closure(vm: &mut VM, _argc: u8) -> Value {
    let ncap = vm.pop().to_int() as usize;
    let params = vm.pop().to_int() as u8;
    let name_idx = vm.pop().to_int() as u16;
    let mut captures = Vec::with_capacity(ncap);
    for _ in 0..ncap {
        captures.push(vm.pop());
    }
    captures.reverse();
    alloc(HeapObj::Closure {
        name_idx,
        params,
        captures,
    })
}

/// Read a copy of a closure handle's metadata, if `v` is a closure.
fn closure_meta(v: &Value) -> Option<(u16, u8, Vec<Value>)> {
    with_obj(v, |o| match o {
        HeapObj::Closure {
            name_idx,
            params,
            captures,
        } => Some((*name_idx, *params, captures.clone())),
        _ => None,
    })
    .flatten()
}

/// Invoke closure `clo` with `args`, running its body through the fusevm frame
/// ABI via a nested `vm.run()`. The body's prologue expects exactly the declared
/// parameter count followed by the captures, so missing args are padded with
/// `null` and extras dropped. See [`run_sub`] for the frame mechanics.
fn invoke_closure(vm: &mut VM, clo: &Value, args: &[Value]) -> Result<Value, String> {
    // Suppressed while unwinding: the lambda body's own unwind check already
    // returned out of it once, and a higher-order call site would otherwise keep
    // invoking it (with side effects) for every remaining element.
    if unwinding() {
        return Ok(Value::Undef);
    }
    let (name_idx, params, captures) =
        closure_meta(clo).ok_or_else(|| "kotlin: value is not a function".to_string())?;
    let entry = vm
        .chunk
        .find_sub(name_idx)
        .ok_or_else(|| "kotlin: lambda body not found".to_string())?;
    let want = params as usize;
    let stack_base = vm.stack.len();
    for i in 0..want {
        vm.stack.push(args.get(i).cloned().unwrap_or(Value::Undef));
    }
    for cap in &captures {
        vm.stack.push(cap.clone());
    }
    run_sub(vm, entry, stack_base)
}

/// Run a subroutine body already positioned on the value stack (its prologue
/// values pushed above `stack_base`) via a nested `vm.run()`. A call frame whose
/// `return_ip` is past the chunk end is pushed so the nested run halts exactly
/// when the body's `ReturnValue` pops that frame; the interpreter IP is saved and
/// restored so the paused enclosing dispatch loop resumes cleanly. This is the
/// re-entrant pattern the mature fusevm frontends (groovyrs/scalars) use to give
/// closures their own frame without any VM change.
fn run_sub(vm: &mut VM, entry: usize, stack_base: usize) -> Result<Value, String> {
    // GUARD FIRST, before the frame is pushed. Every re-entrant `vm.run()` in
    // this file funnels through here, so this one check covers recursion
    // through a lambda, through a `toString`/`equals`/`hashCode` override, and
    // through a property delegate alike.
    let depth = NESTED_DEPTH.with(|d| d.get());
    if depth >= NESTED_RUN_LIMIT {
        // Kotlin's answer, and a catchable one. Reaching the Rust stack's own
        // end instead is `SIGABRT` — `fatal runtime error: stack overflow` —
        // which unwinds nothing and which no `catch` can ever see.
        fault(vm, "java.lang.StackOverflowError");
        return Ok(Value::Undef);
    }
    NESTED_DEPTH.with(|d| d.set(depth + 1));
    let out = run_sub_inner(vm, entry, stack_base);
    NESTED_DEPTH.with(|d| d.set(depth));
    out
}

/// How deep the re-entrant `vm.run()` nesting may go before
/// `java.lang.StackOverflowError` is raised.
///
/// This is a stack budget, not a taste. One level costs one Rust frame of the
/// whole interpreter dispatch loop, measured at roughly 100 KB in a `cargo
/// build` (dev, unoptimized) binary. Against
/// `runtime::INTERPRETER_STACK`'s 512 MiB that leaves room for some
/// thousands of levels; 2000 keeps a wide margin for the deepest frame the
/// dispatch loop can take and for anything the host itself recurses through
/// below this point (nested container rendering, `value_eq`).
///
/// Kotlin's own limit is a JVM stack, not a count, so no fixed number here can
/// match it exactly — a program that recurses 1900 deep succeeds on both, and
/// one that recurses forever raises `StackOverflowError` on both. What must NOT
/// differ is the KIND of failure, and before this guard existed it did.
pub const NESTED_RUN_LIMIT: usize = 2000;

thread_local! {
    /// Current re-entrant `vm.run()` nesting depth — see [`NESTED_RUN_LIMIT`].
    static NESTED_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`run_sub`] without the depth accounting.
fn run_sub_inner(vm: &mut VM, entry: usize, stack_base: usize) -> Result<Value, String> {
    let return_ip = vm.chunk.ops.len();
    vm.frames.push(Frame {
        return_ip,
        stack_base,
        slots: Vec::new(),
        // Same identity `Op::Call` records: this frame enters the subroutine
        // at `entry`, so `Chunk::sub_slot_names` is reachable from it.
        entry_ip: Some(entry),
    });
    let saved_ip = vm.ip;
    vm.ip = entry;
    let result = vm.run();
    vm.ip = saved_ip;
    match result {
        VMResult::Ok(v) => Ok(v),
        // A `request_halt` from a fault inside the body (e.g. `/ by zero`) ends
        // the nested run as `Halted`; the parked `KT_ERROR` propagates via the
        // still-set halt flag, which stops the enclosing run too.
        VMResult::Halted => Ok(vm.stack.pop().unwrap_or(Value::Undef)),
        VMResult::Error(e) => Err(e),
    }
}

/// The distinct elements of `vals`, keeping the first occurrence of a repeat —
/// the element order a Kotlin `LinkedHashSet` iterates in. Membership uses
/// [`value_eq`] (structural), so two equal `data class` instances collapse.
fn distinct(vm: &mut VM, vals: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(vals.len());
    for v in vals {
        // Hash-gated: `distinct()` and `setOf` both build a `LinkedHashSet`, so
        // a class that overrides `equals` without `hashCode` keeps its
        // duplicates here exactly as it does on the JVM.
        if !out.iter().any(|x| hash_eq_vm(vm, x, v)) {
            out.push(v.clone());
        }
    }
    out
}

/// Run a registered no-argument-or-one-argument override body for `recv`.
///
/// `extra` is the `other` an `equals` takes; `None` is the `hashCode` shape.
/// The heap is never borrowed across the call — the body can allocate — and a
/// fault inside it surfaces as `None` so the caller can fall back rather than
/// answer from a half-run frame.
fn run_override(
    vm: &mut VM,
    subs: SubRegistry,
    recv: &Value,
    extra: Option<&Value>,
) -> Option<Value> {
    let tag = instance_tag(recv)?;
    let idx = subs.lookup(&tag)?;
    let entry = vm.chunk.find_sub(idx)?;
    let base = vm.stack.len();
    vm.stack.push(recv.clone());
    if let Some(other) = extra {
        vm.stack.push(other.clone());
    }
    run_sub(vm, entry, base).ok()
}

/// Which registry [`run_override`] should consult.
#[derive(Clone, Copy)]
enum SubRegistry {
    Equals,
    HashCode,
}

impl SubRegistry {
    fn lookup(self, tag: &str) -> Option<u16> {
        match self {
            SubRegistry::Equals => EQUALS_SUBS.with(|t| t.borrow().get(tag).copied()),
            SubRegistry::HashCode => HASHCODE_SUBS.with(|t| t.borrow().get(tag).copied()),
        }
    }
}

/// Kotlin `==`, with a user `equals` in play.
///
/// `a == b` compiles to `a?.equals(b) ?: (b === null)` on the JVM, so dispatch
/// is on the LEFT operand's class. When that class declared an `equals`, its
/// body runs through a nested `vm.run()` — the same re-entrant pattern
/// `display_vm` uses for `toString` — and its answer is final. Otherwise the
/// structural walk recurses with `equal_vm` again, so an override still applies
/// to a list element, a `Pair` half or a `Map` value, and anything with no
/// overriding instance inside falls through to the VM-less [`value_eq`].
pub fn equal_vm(vm: &mut VM, a: &Value, b: &Value) -> bool {
    // A declared `equals` runs even when both sides are the SAME object.
    //
    // Kotlin's `==` lowers to `Intrinsics.areEqual(a, b)`, which is
    // `a == null ? b == null : a.equals(b)` — there is no `a === b` short-circuit
    // at the call site, and `java.util.ArrayList.indexOf` likewise calls
    // `equals` per element without one. Skipping the body for a self-comparison
    // is observable the moment `equals` has an effect: a counting `equals`
    // reports 1 call after `x == x` on the reference toolchain, and reported 0
    // here until this short-circuit was removed.
    if let Some(r) = run_override(vm, SubRegistry::Equals, a, Some(b)) {
        return truthy(&r);
    }
    // With no override the answer cannot depend on how many times it is asked,
    // so the identity of two equal handles settles it without a walk.
    if let (Value::Obj(ia), Value::Obj(ib)) = (a, b) {
        if ia == ib {
            return true;
        }
    }
    // No override on the left: recurse structurally, but only for the shapes
    // that can HOLD an instance. Clone the children out first — the nested run
    // an override needs will mutate the heap.
    enum Shape {
        Seq(Vec<Value>, bool),
        Map(Vec<(Value, Value)>),
        Couple(Value, Value),
        Triple(Value, Value, Value),
        Data(String, Vec<Value>),
        Plain,
    }
    fn shape_of(v: &Value) -> Shape {
        with_obj(v, |o| match o {
            HeapObj::List(items) => Shape::Seq(items.clone(), false),
            HeapObj::Set(items) => Shape::Seq(items.clone(), true),
            HeapObj::Map(entries) => Shape::Map(entries.clone()),
            HeapObj::Pair(x, y) | HeapObj::Entry(x, y) => Shape::Couple(x.clone(), y.clone()),
            HeapObj::Triple(x, y, z) => Shape::Triple(x.clone(), y.clone(), z.clone()),
            HeapObj::Instance {
                class,
                is_data: true,
                fields,
                data_from,
                data_len,
            } => Shape::Data(
                class.clone(),
                data_slice(fields, *data_from, *data_len)
                    .iter()
                    .map(|(_, v)| v.clone())
                    .collect(),
            ),
            _ => Shape::Plain,
        })
        .unwrap_or(Shape::Plain)
    }
    // A kind only ever equals its own kind, and `value_eq` already refuses every
    // cross-kind pairing; matching both sides keeps that true here.
    match (shape_of(a), shape_of(b)) {
        (Shape::Seq(xa, sa), Shape::Seq(xb, sb)) if sa == sb => {
            if xa.len() != xb.len() {
                return false;
            }
            if sa {
                // A `Set` compares order-insensitively — `AbstractSet.equals`
                // is `size` plus `containsAll`, so each probe goes through the
                // HASH-gated `contains`, not a bare `equals`. Two sets of a
                // class that declares `equals` without `hashCode` are therefore
                // NOT equal, even element for element.
                xa.iter().all(|x| xb.iter().any(|y| hash_eq_vm(vm, x, y)))
            } else {
                xa.iter().zip(&xb).all(|(x, y)| equal_vm(vm, x, y))
            }
        }
        (Shape::Map(ea), Shape::Map(eb)) => {
            // `AbstractMap.equals` looks each key up with `get` — hash-gated —
            // and compares the VALUE it finds with `equals`.
            ea.len() == eb.len()
                && ea.iter().all(|(k, v)| {
                    eb.iter()
                        .any(|(k2, v2)| hash_eq_vm(vm, k, k2) && equal_vm(vm, v, v2))
                })
        }
        (Shape::Couple(a1, a2), Shape::Couple(b1, b2)) => {
            // A `Pair` never equals an `Entry`, though both are a key/value
            // couple here, so the kinds are checked before the halves.
            kind_tag(a) == kind_tag(b) && equal_vm(vm, &a1, &b1) && equal_vm(vm, &a2, &b2)
        }
        (Shape::Triple(a1, a2, a3), Shape::Triple(b1, b2, b3)) => {
            equal_vm(vm, &a1, &b1) && equal_vm(vm, &a2, &b2) && equal_vm(vm, &a3, &b3)
        }
        (Shape::Data(ca, fa), Shape::Data(cb, fb)) => {
            ca == cb && fa.len() == fb.len() && fa.iter().zip(&fb).all(|(x, y)| equal_vm(vm, x, y))
        }
        _ => value_eq(a, b),
    }
}

/// A one-word discriminator for the heap kinds `equal_vm` folds together, so
/// two shapes that share a `Shape` arm but not a Kotlin type stay unequal.
fn kind_tag(v: &Value) -> &'static str {
    with_obj(v, |o| match o {
        HeapObj::Pair(..) => "Pair",
        HeapObj::Entry(..) => "Entry",
        _ => "",
    })
    .unwrap_or("")
}

/// Kotlin `hashCode()`, with a user `hashCode` in play — the [`value_hash`]
/// walk, re-entrant so an override answers for the instance itself AND for one
/// nested in a `List`/`Set`/`Map`/`Pair` whose fold reads its elements.
fn hash_vm(vm: &mut VM, v: &Value, long: bool) -> i32 {
    if let Some(r) = run_override(vm, SubRegistry::HashCode, v, None) {
        return r.to_int() as i32;
    }
    enum Shape {
        List(Vec<Value>),
        Set(Vec<Value>),
        Map(Vec<(Value, Value)>),
        Pair(Value, Value),
        Entry(Value, Value),
        Plain,
    }
    let shape = with_obj(v, |o| match o {
        HeapObj::List(items) => Shape::List(items.clone()),
        HeapObj::Set(items) => Shape::Set(items.clone()),
        HeapObj::Map(entries) => Shape::Map(entries.clone()),
        HeapObj::Pair(a, b) => Shape::Pair(a.clone(), b.clone()),
        HeapObj::Entry(k, val) => Shape::Entry(k.clone(), val.clone()),
        _ => Shape::Plain,
    })
    .unwrap_or(Shape::Plain);
    match shape {
        Shape::List(items) => items.iter().fold(1i32, |h, x| {
            h.wrapping_mul(31).wrapping_add(hash_vm(vm, x, false))
        }),
        Shape::Set(items) => items
            .iter()
            .fold(0i32, |h, x| h.wrapping_add(hash_vm(vm, x, false))),
        Shape::Map(entries) => entries.iter().fold(0i32, |h, (k, val)| {
            h.wrapping_add(hash_vm(vm, k, false) ^ hash_vm(vm, val, false))
        }),
        Shape::Pair(a, b) => hash_vm(vm, &a, false)
            .wrapping_mul(31)
            .wrapping_add(hash_vm(vm, &b, false)),
        Shape::Entry(k, val) => hash_vm(vm, &k, false) ^ hash_vm(vm, &val, false),
        Shape::Plain => value_hash(v, long),
    }
}

/// Membership for the HASH-based containers — `Set`, `Map` keys, `distinct`.
///
/// The JVM reaches `equals` only after the hash buckets agree, so a class that
/// overrides `equals` WITHOUT `hashCode` keeps its duplicates in a `HashSet`
/// even though the two compare equal. Modelling the gate rather than calling
/// `equals` directly is what reproduces that: an unoverridden `hashCode` on a
/// plain class is its identity, so two distinct instances never meet.
/// Unlike `==`, a hash container DOES short-circuit on identity:
/// `HashMap.getNode` tests `(k = e.key) == key || key.equals(k)`, so a lookup by
/// the very object already stored never runs the body.
fn hash_eq_vm(vm: &mut VM, a: &Value, b: &Value) -> bool {
    if let (Value::Obj(ia), Value::Obj(ib)) = (a, b) {
        if ia == ib {
            return true;
        }
    }
    hash_vm(vm, a, false) == hash_vm(vm, b, false) && equal_vm(vm, a, b)
}

/// The declared class tag of a value, when it is a class instance or an `object`
/// singleton; `None` for every other value kind.
fn instance_tag(v: &Value) -> Option<String> {
    with_obj(v, |o| match o {
        HeapObj::Instance { class, .. } => Some(class.clone()),
        _ => None,
    })
    .flatten()
}

/// How deep a display walk may nest before `java.lang.StackOverflowError`.
///
/// Rendering a container recurses in Rust, once per level, in BOTH the
/// VM-aware [`display_vm`] and the VM-less [`display_obj`]. A cyclic structure
/// the direct-self check below does not cover — `a` holds `b` and `b` holds `a`
/// — recurses forever, and hitting the Rust stack's end is `SIGABRT`, which no
/// `catch` can see. The JVM raises `StackOverflowError` there (measured on JDK
/// 21.0.12), and so does this.
///
/// Far below [`NESTED_RUN_LIMIT`] because a render frame is small and a
/// legitimately 512-deep printed structure does not occur.
const RENDER_DEPTH_LIMIT: usize = 512;

thread_local! {
    /// The heap ids of the containers currently being rendered, outermost
    /// first. The last entry is the IMMEDIATE parent, which is the only one
    /// `AbstractCollection.toString` compares against — see
    /// [`self_reference`].
    static RENDERING: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// Set when a display walk hit [`RENDER_DEPTH_LIMIT`]. Read and cleared by
    /// [`display_checked`], which is where a `vm` is in hand to raise with.
    static RENDER_OVERFLOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `AbstractCollection`/`AbstractMap`'s self-reference placeholder for `v` when
/// it is the container currently being rendered, else `None`.
///
/// The JVM's check is `if (e == this)` — reference identity against the
/// IMMEDIATE receiver, one level, no cycle detection of any kind. So
/// `xs.add(xs)` renders `[(this Collection)]` while `a.add(b); b.add(a)` still
/// overflows the stack. Both were measured on JDK 21.0.12; reproducing only the
/// first half is what makes the two cases differ here the way they differ
/// there. A `Map` words it `(this Map)`.
fn self_reference(v: &Value, in_map: bool) -> Option<&'static str> {
    let Value::Obj(id) = v else { return None };
    let parent = RENDERING.with(|r| r.borrow().last().copied());
    (parent == Some(*id)).then_some(if in_map {
        "(this Map)"
    } else {
        "(this Collection)"
    })
}

/// Run `body` with `v`'s heap id pushed as the container being rendered.
/// Answers `None` when [`RENDER_DEPTH_LIMIT`] is already reached, having
/// recorded the overflow for [`display_checked`].
fn rendering<T>(v: &Value, body: impl FnOnce() -> T) -> Option<T> {
    let Value::Obj(id) = v else {
        return Some(body());
    };
    let too_deep = RENDERING.with(|r| {
        let mut r = r.borrow_mut();
        if r.len() >= RENDER_DEPTH_LIMIT {
            return true;
        }
        r.push(*id);
        false
    });
    if too_deep {
        RENDER_OVERFLOW.with(|f| f.set(true));
        return None;
    }
    let out = body();
    RENDERING.with(|r| {
        r.borrow_mut().pop();
    });
    Some(out)
}

/// [`display_vm`], then raise `StackOverflowError` if the walk ran away.
///
/// The render functions answer a `String` and have nowhere to put a throwable,
/// so they park the fact in `RENDER_OVERFLOW` and this — the one render entry
/// point holding a `vm` — turns it into the exception.
fn display_checked(vm: &mut VM, v: &Value) -> String {
    RENDER_OVERFLOW.with(|f| f.set(false));
    let s = display_vm(vm, v);
    if RENDER_OVERFLOW.with(|f| f.replace(false)) {
        fault(vm, "java.lang.StackOverflowError");
    }
    s
}

/// The Kotlin display form of `v`, honouring a user `toString()` override.
///
/// An instance whose class registered an override runs that body through a
/// nested `vm.run()`; a `List`/`Map`/`Pair` recurses so an override applies to
/// its elements too. Everything else falls back to the VM-less
/// [`kotlin_string`]. The heap borrow is always released before re-entering the
/// VM, because the override body can allocate.
fn display_vm(vm: &mut VM, v: &Value) -> String {
    if let Some(tag) = instance_tag(v) {
        let sub = TOSTRING_SUBS.with(|t| t.borrow().get(&tag).copied());
        if let Some(idx) = sub {
            if let Some(entry) = vm.chunk.find_sub(idx) {
                let base = vm.stack.len();
                vm.stack.push(v.clone());
                return match run_sub(vm, entry, base) {
                    Ok(r) => kotlin_string(&r),
                    Err(_) => kotlin_string(v),
                };
            }
        }
        return kotlin_string(v);
    }
    // Clone the children out before recursing: the nested run mutates the heap.
    enum Shape {
        List(Vec<Value>),
        Map(Vec<(Value, Value)>),
        Pair(Value, Value),
        Entry(Value, Value),
        Plain,
    }
    let shape = with_obj(v, |o| match o {
        HeapObj::List(items) | HeapObj::Set(items) => Shape::List(items.clone()),
        HeapObj::Map(entries) => Shape::Map(entries.clone()),
        HeapObj::Pair(a, b) => Shape::Pair(a.clone(), b.clone()),
        HeapObj::Entry(k, v) => Shape::Entry(k.clone(), v.clone()),
        _ => Shape::Plain,
    })
    .unwrap_or(Shape::Plain);
    if matches!(shape, Shape::Plain) {
        return kotlin_string(v);
    }
    // Every recursive arm runs inside `rendering`, so the element walk can see
    // its own container (the `(this Collection)` check) and so a runaway cycle
    // stops at `RENDER_DEPTH_LIMIT` instead of at the Rust stack's end.
    rendering(v, || match shape {
        Shape::List(items) => {
            let body: Vec<String> = items
                .iter()
                .map(|x| match self_reference(x, false) {
                    Some(s) => s.to_string(),
                    None => display_vm(vm, x),
                })
                .collect();
            format!("[{}]", body.join(", "))
        }
        Shape::Map(entries) => {
            let body: Vec<String> = entries
                .iter()
                .map(|(k, val)| {
                    let ks = match self_reference(k, true) {
                        Some(s) => s.to_string(),
                        None => display_vm(vm, k),
                    };
                    let vs = match self_reference(val, true) {
                        Some(s) => s.to_string(),
                        None => display_vm(vm, val),
                    };
                    format!("{ks}={vs}")
                })
                .collect();
            format!("{{{}}}", body.join(", "))
        }
        Shape::Pair(a, b) => format!("({}, {})", display_vm(vm, &a), display_vm(vm, &b)),
        Shape::Entry(k, val) => format!("{}={}", display_vm(vm, &k), display_vm(vm, &val)),
        Shape::Plain => kotlin_string(v),
    })
    .unwrap_or_default()
}

/// `KT_DISPLAY` — see [`KT_DISPLAY`].
fn b_display(vm: &mut VM, _argc: u8) -> Value {
    let v = vm.pop();
    Value::str(display_checked(vm, &v))
}

/// `KT_MAP_VM` — see [`KT_MAP_VM`]. Stack: `[pair0 .. pairN]`, each a `Pair`.
///
/// `mapOf` fills a `LinkedHashMap` by `put`, so a repeated key keeps its first
/// POSITION and takes the last VALUE. Which keys count as repeated is the
/// hash-gated container equality — not the raw handle comparison this used,
/// which collapsed only keys that were literally the same object and so left
/// `mapOf(D(1) to 1, D(1) to 2)` with two entries for every structural key
/// (a `data class`, a `List`, or a class with a declared `equals`).
fn b_map_new(vm: &mut VM, argc: u8) -> Value {
    let spec = CollSpec::pop(vm);
    let mut pairs = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        pairs.push(vm.pop());
    }
    pairs.reverse();
    // The copy form `HashMap(other)` takes ONE map and re-buckets its entries;
    // the builder form takes `k to v` pairs.
    let sources: Vec<(Value, Value)> = if spec.copy {
        pairs.first().and_then(as_couples).unwrap_or_default()
    } else {
        pairs
            .iter()
            .filter_map(|p| {
                with_obj(p, |o| match o {
                    HeapObj::Pair(a, b) => Some((a.clone(), b.clone())),
                    _ => None,
                })
                .flatten()
            })
            .collect()
    };
    let mut entries: Vec<(Value, Value)> = Vec::with_capacity(sources.len());
    for (k, v) in sources {
        map_upsert(vm, &mut entries, k, v);
    }
    let n = entries.len();
    let m = alloc(HeapObj::Map(entries));
    spec.apply(vm, &m, n);
    m
}

/// `KT_METHOD_VM` — see [`KT_METHOD_VM`]. Stack: `[recv, arg0 .. argN, name]`.
fn b_method(vm: &mut VM, argc: u8) -> Value {
    let name = vm.pop().to_str();
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        args.push(vm.pop());
    }
    args.reverse();
    let recv = vm.pop();
    match kt_method(vm, &recv, &name, &args) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `KT_SET_VM` — see [`KT_SET_VM`]. Stack: `[v0 .. vN]`.
fn b_set_new(vm: &mut VM, argc: u8) -> Value {
    let spec = CollSpec::pop(vm);
    let mut vals = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        vals.push(vm.pop());
    }
    vals.reverse();
    // `HashSet(other)` copies the argument's elements; `hashSetOf(a, b)` takes
    // the arguments themselves.
    if spec.copy {
        vals = vals.first().and_then(as_iterable).unwrap_or_default();
    }
    let items = distinct(vm, &vals);
    let n = items.len();
    let s = alloc(HeapObj::Set(items));
    spec.apply(vm, &s, n);
    s
}

/// `KT_IN_VM` — see [`KT_IN_VM`]. Stack: `[value, container]`.
fn b_in(vm: &mut VM, _argc: u8) -> Value {
    let container = vm.pop();
    let value = vm.pop();
    Value::Bool(contains_value(vm, &container, &value))
}

/// `KT_INDEX_GET_VM` — see [`KT_INDEX_GET_VM`]. Stack: `[recv, index]`.
fn b_index_get(vm: &mut VM, _argc: u8) -> Value {
    let index = vm.pop();
    let recv = vm.pop();
    match index_get(vm, &recv, &index) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `KT_INDEX_SET_VM` — see [`KT_INDEX_SET_VM`]. Stack: `[recv, index, value]`.
fn b_index_set(vm: &mut VM, _argc: u8) -> Value {
    let value = vm.pop();
    let index = vm.pop();
    let recv = vm.pop();
    if let Err(e) = index_set(vm, &recv, &index, value) {
        fault(vm, e);
    }
    Value::Undef
}

/// `KT_OPER_VM` — see [`KT_OPER_VM`]. Stack: `[lhs, rhs, nameStr]`.
fn b_operator(vm: &mut VM, _argc: u8) -> Value {
    let name = vm.pop().to_str();
    let rhs = vm.pop();
    let lhs = vm.pop();
    match operator_apply(vm, &lhs, &name, &rhs) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// The source elements and key selector of a `Grouping` receiver.
fn grouping_parts(v: &Value) -> Option<(Vec<Value>, Value)> {
    with_obj(v, |o| match o {
        HeapObj::Grouping { items, key } => Some((items.clone(), key.clone())),
        _ => None,
    })
    .flatten()
}

/// The elements of `v` when it is a Kotlin `Iterable` — a `List`, a `Set`, a
/// range or an array; `None` for anything else.
///
/// A `String` is deliberately not one, unlike in [`sequence_items`]. Kotlin's
/// `CharSequence` does not implement `Iterable`, so `listOf("x") + "y"` picks
/// the `plus(element)` overload and appends the string whole — `[x, y]`, where
/// treating it as a sequence would have appended its characters.
fn as_iterable(v: &Value) -> Option<Vec<Value>> {
    with_obj(v, |o| match o {
        HeapObj::List(items) | HeapObj::Set(items) => Some(items.clone()),
        HeapObj::Array { items, .. } => Some(items.clone()),
        HeapObj::Range(r) => Some(r.to_vec()),
        _ => None,
    })
    .flatten()
}

/// The key/value couples of `v` when it is a `Map`, a `Pair` or a `Map.Entry` —
/// the operand shapes `Map.plus` accepts.
fn as_couples(v: &Value) -> Option<Vec<(Value, Value)>> {
    if let Some(entries) = with_obj(v, |o| match o {
        HeapObj::Map(entries) => Some(entries.clone()),
        HeapObj::Pair(k, val) | HeapObj::Entry(k, val) => Some(vec![(k.clone(), val.clone())]),
        _ => None,
    })
    .flatten()
    {
        return Some(entries);
    }
    // `map + listOf("a" to 1, "b" to 2)` — an iterable OF pairs.
    let items = as_iterable(v)?;
    let mut out = Vec::with_capacity(items.len());
    for it in &items {
        let couple = with_obj(it, |o| match o {
            HeapObj::Pair(k, val) | HeapObj::Entry(k, val) => Some((k.clone(), val.clone())),
            _ => None,
        })
        .flatten()?;
        out.push(couple);
    }
    Some(out)
}

/// Insert or overwrite `k` in `entries`, keeping a pre-existing key in place.
///
/// Kotlin's `Map.plus` is `LinkedHashMap(this).apply { putAll(other) }`, and
/// `put` on an existing key replaces the VALUE without moving the entry, so
/// `mapOf("a" to 1, "b" to 2) + ("a" to 9)` is `{a=9, b=2}` — not `{b=2, a=9}`.
fn map_upsert(vm: &mut VM, entries: &mut Vec<(Value, Value)>, k: Value, v: Value) {
    for slot in entries.iter_mut() {
        if hash_eq_vm(vm, &slot.0, &k) {
            slot.1 = v;
            return;
        }
    }
    entries.push((k, v));
}

/// Apply a Kotlin operator convention to a heap receiver — see [`KT_OPER_VM`].
///
/// `plus`/`minus` answer a NEW collection of the receiver's kind;
/// `plusAssign`/`minusAssign` mutate the receiver in place and answer `Undef`.
/// That split is Kotlin's, and it is observable through an alias: `var l =
/// listOf(1, 2); l += 3` rebinds `l` to a fresh list and leaves an alias at
/// `[1, 2]`, while `val m = mutableListOf(1, 2); m += 3` mutates the one object
/// an alias also sees.
///
/// Element removal follows the reference collections exactly: `minus(element)`
/// drops only the FIRST match (`listOf(1, 2, 2, 3) - 2` is `[1, 2, 3]`), while
/// `minus(elements)` drops every occurrence of every listed element.
fn operator_apply(vm: &mut VM, lhs: &Value, name: &str, rhs: &Value) -> Result<Value, String> {
    // Only `plus`/`minus` are conventions the built-in collections define.
    // `times`/`div`/`rem` are NOT, so `listOf(1) * 2` has to fail the way the
    // reference compiler fails it — matching a prefix instead would have made
    // every unlisted operator behave as `minus`, trading one silent wrong
    // answer for another.
    let (adding, assign) = match name {
        "plus" => (true, false),
        "minus" => (false, false),
        "plusAssign" => (true, true),
        "minusAssign" => (false, true),
        _ => {
            return Err(format!(
                "unresolved reference: {name} on {}",
                obj_label(lhs)
            ))
        }
    };

    // A `Map` receiver keys on entries rather than elements, so it resolves
    // first: `map + pair` upserts, `map - key` removes.
    if let Some(mut entries) = with_obj(lhs, |o| match o {
        HeapObj::Map(entries) => Some(entries.clone()),
        _ => None,
    })
    .flatten()
    {
        if adding {
            let add = as_couples(rhs).ok_or_else(|| {
                format!(
                    "unresolved reference: {name} on Map with {}",
                    obj_label(rhs)
                )
            })?;
            for (k, v) in add {
                map_upsert(vm, &mut entries, k, v);
            }
        } else {
            // `map - keys` takes a key or an iterable OF keys — never a map.
            let drop = as_iterable(rhs).unwrap_or_else(|| vec![rhs.clone()]);
            let mut kept: Vec<(Value, Value)> = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let mut hit = false;
                for d in &drop {
                    if hash_eq_vm(vm, &k, d) {
                        hit = true;
                        break;
                    }
                }
                if !hit {
                    kept.push((k, v));
                }
            }
            entries = kept;
        }
        if assign {
            with_obj_mut(lhs, |o| {
                if let HeapObj::Map(slot) = o {
                    *slot = entries;
                }
            });
            reorder(vm, lhs);
            return Ok(Value::Undef);
        }
        return Ok(alloc(HeapObj::Map(entries)));
    }

    // A `List`/`Set`/range/array receiver. A range answers a `List`, which is
    // what `(1..3) + 4` evaluates to on the reference toolchain.
    let is_set = with_obj(lhs, |o| matches!(o, HeapObj::Set(_))).unwrap_or(false);
    let Some(items) = as_iterable(lhs) else {
        return Err(format!(
            "unresolved reference: {name} on {}",
            obj_label(lhs)
        ));
    };

    let mut out: Vec<Value> = Vec::with_capacity(items.len() + 1);
    match (adding, as_iterable(rhs)) {
        (true, Some(more)) => {
            out.extend(items);
            out.extend(more);
        }
        (true, None) => {
            out.extend(items);
            out.push(rhs.clone());
        }
        // `minus(elements)` — drop every occurrence of every listed element.
        (false, Some(drop)) => {
            for it in items {
                let mut hit = false;
                for d in &drop {
                    if elem_eq(vm, is_set, &it, d) {
                        hit = true;
                        break;
                    }
                }
                if !hit {
                    out.push(it);
                }
            }
        }
        // `minus(element)` — drop the first match only.
        (false, None) => {
            let mut dropped = false;
            for it in items {
                if !dropped && elem_eq(vm, is_set, &it, rhs) {
                    dropped = true;
                    continue;
                }
                out.push(it);
            }
        }
    }

    let result = if is_set {
        HeapObj::Set(distinct(vm, &out))
    } else {
        HeapObj::List(out)
    };
    if assign {
        with_obj_mut(lhs, |o| match (o, result) {
            (HeapObj::Set(slot), HeapObj::Set(v)) => *slot = v,
            (HeapObj::List(slot), HeapObj::List(v)) => *slot = v,
            (HeapObj::Array { items: slot, .. }, HeapObj::List(v)) => *slot = v,
            _ => {}
        });
        reorder(vm, lhs);
        return Ok(Value::Undef);
    }
    Ok(alloc(result))
}

/// The index of the first element of `items` equal to `want`, by `equals` —
/// `ArrayList.indexOf`, which is what `minusElement` removes at.
fn position_eq(vm: &mut VM, items: &[Value], want: &Value) -> Option<usize> {
    (0..items.len()).find(|&i| equal_vm(vm, &items[i], want))
}

/// Element equality for collection `minus`.
///
/// A `Set` is a `LinkedHashSet`, whose lookup is hash-gated — a class that
/// overrides `equals` without `hashCode` keeps its "duplicates" there. A `List`
/// is an `ArrayList`, whose `remove(Object)`/`indexOf` walk calls `equals`
/// directly with no hash gate, so the same class DOES match there. The two
/// disagree, so they cannot share one predicate.
fn elem_eq(vm: &mut VM, is_set: bool, a: &Value, b: &Value) -> bool {
    if is_set {
        hash_eq_vm(vm, a, b)
    } else {
        equal_vm(vm, a, b)
    }
}

/// `KT_OBJEQ_VM` — see [`KT_OBJEQ_VM`].
fn b_objeq(vm: &mut VM, _argc: u8) -> Value {
    let b = vm.pop();
    let a = vm.pop();
    Value::Bool(equal_vm(vm, &a, &b))
}

/// `KT_JOIN` — see [`KT_JOIN`]. `argc` is 1 when a separator was supplied.
fn b_join(vm: &mut VM, argc: u8) -> Value {
    let sep = if argc >= 1 {
        kotlin_string(&vm.pop())
    } else {
        ", ".to_string()
    };
    let recv = vm.pop();
    let items = sequence_items(&recv);
    let body: Vec<String> = items.iter().map(|x| display_vm(vm, x)).collect();
    Value::str(body.join(&sep))
}

/// The elements of any iterable receiver — a `List`, an array, or a range.
/// The `windowed`/`chunked` walk: groups of `size`, advancing by `step`.
///
/// With `partial` set, a trailing group shorter than `size` is still emitted and
/// the walk runs until the start index passes the end — which is what makes
/// `chunked` (`step = size`, `partial = true`) and `windowed(size, step, true)`
/// one function. Without it, only full-length groups appear.
fn windows_of(items: &[Value], size: usize, step: usize, partial: bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < items.len() {
        let end = (at + size).min(items.len());
        if end - at < size && !partial {
            break;
        }
        out.push(alloc(HeapObj::List(items[at..end].to_vec())));
        at += step;
    }
    out
}

/// The per-element renderer `joinToString(…) { … }` supplies. It can fail,
/// because the lambda it wraps runs Kotlin code that may raise.
type JoinTransform<'a> = dyn FnMut(&Value) -> Result<String, String> + 'a;

/// `joinToString(separator, prefix, postfix, limit, truncated)` over `items`,
/// reading the arguments from `args` starting at `at`.
///
/// A non-negative `limit` caps how many elements are rendered and appends
/// `truncated` (default `"..."`) in place of the rest. `transform` renders one
/// element when supplied; otherwise an element goes through the Kotlin
/// stringifier.
fn join_to_string(
    items: &[Value],
    args: &[Value],
    at: usize,
    mut transform: Option<&mut JoinTransform<'_>>,
) -> String {
    let text = |i: usize, dflt: &str| -> String {
        args.get(at + i)
            .filter(|v| !matches!(v, Value::Undef))
            .map(kotlin_string)
            .unwrap_or_else(|| dflt.to_string())
    };
    let sep = text(0, ", ");
    let prefix = text(1, "");
    let postfix = text(2, "");
    let limit = args.get(at + 3).map(|v| v.to_int()).unwrap_or(-1);
    let truncated = text(4, "...");

    let mut body: Vec<String> = Vec::new();
    for (i, v) in items.iter().enumerate() {
        if limit >= 0 && i as i64 >= limit {
            body.push(truncated);
            break;
        }
        body.push(match transform.as_mut() {
            Some(f) => match f(v) {
                Ok(s) => s,
                // A raise inside the transform is reported by the invoking
                // builtin; rendering stops with what it produced.
                Err(_) => break,
            },
            None => kotlin_string(v),
        });
    }
    format!("{prefix}{}{postfix}", body.join(&sep))
}

fn sequence_items(v: &Value) -> Vec<Value> {
    // A `String` is a `CharSequence`, so it is a valid other-operand for the
    // members that take one (`"abc".zip("xy")`, `list.zip("xy")`).
    if let Value::Str(s) = v {
        return chars_of(s);
    }
    with_obj(v, |o| match o {
        HeapObj::List(items) | HeapObj::Set(items) => items.clone(),
        HeapObj::Array { items, .. } => items.clone(),
        HeapObj::Range(r) => r.to_vec(),
        _ => Vec::new(),
    })
    .unwrap_or_default()
}

/// `KT_CLOSURE_CALL`: invoke a closure directly, `f(args)`. Stack (top-down):
/// `arg{n-1} .. arg0, closure`, with `argc` = `n`.
fn b_closure_call(vm: &mut VM, argc: u8) -> Value {
    let n = argc as usize;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(vm.pop());
    }
    args.reverse();
    let clo = vm.pop();
    match invoke_closure(vm, &clo, &args) {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `KT_COLL_HOF`: a higher-order collection method taking a lambda. Stack
/// (top-down): `nameStr, closure, extra{m-1} .. extra0, recv`, with `argc` = `m`
/// (the leading non-closure args, e.g. `fold`'s initial). Iterates `recv`,
/// invoking `closure` per element, and returns the method's result.
fn b_coll_hof(vm: &mut VM, argc: u8) -> Value {
    let name = vm.pop().to_str();
    let clo = vm.pop();
    let m = argc as usize;
    let mut extras = Vec::with_capacity(m);
    for _ in 0..m {
        extras.push(vm.pop());
    }
    extras.reverse();
    let recv = vm.pop();
    let from_sequence = is_sequence_view(&recv);
    match coll_hof(vm, &name, &recv, &extras, &clo, seq_kind_of(&recv)) {
        Ok(v) => {
            // The lambda-taking members dispatch here rather than through
            // `kt_method`, so the sequence tag has to be carried forward on
            // this path too — otherwise
            // `xs.asSequence().map { }.filter { }.first()` falls back to the
            // `List` wording after the first stage. Same rule, stated by the
            // same predicate.
            if from_sequence && sequence_preserving(&name) {
                tag_sequence(v.clone());
            }
            v
        }
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `KT_RUN_CATCHING` — see [`KT_RUN_CATCHING`].
fn b_run_catching(vm: &mut VM, _argc: u8) -> Value {
    let clo = vm.pop();
    // Already unwinding: `runCatching` is not reached at all in Kotlin either,
    // because the enclosing statement never runs.
    if unwinding() {
        return Value::Undef;
    }
    let out = invoke_closure(vm, &clo, &[]);
    // A raise inside the block parked itself in the pending slot; taking it is
    // what makes `runCatching` a catch.
    if let Some(err) = PENDING.with(|p| p.borrow_mut().take()) {
        return alloc(HeapObj::Res {
            value: Value::Undef,
            err: Some(err),
        });
    }
    match out {
        Ok(v) => alloc(HeapObj::Res {
            value: v,
            err: None,
        }),
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// The `(value, error)` of a `Result` receiver.
fn result_parts(v: &Value) -> Option<(Value, Option<Value>)> {
    with_obj(v, |o| match o {
        HeapObj::Res { value, err } => Some((value.clone(), err.clone())),
        _ => None,
    })
    .flatten()
}

/// The `(seed, step, stages)` of a lazy sequence, or `None` for anything else.
fn gen_parts(v: &Value) -> Option<(Option<Value>, Value, Vec<Stage>)> {
    with_obj(v, |o| match o {
        HeapObj::Gen { seed, step, stages } => Some((seed.clone(), step.clone(), stages.clone())),
        _ => None,
    })
    .flatten()
}

/// A new lazy sequence: `gen` with `stage` appended.
///
/// The stages are COPIED rather than shared, which matters because `DropWhile`
/// carries per-pull state — two pipelines built from one base must not see each
/// other's progress.
fn gen_with(vm: &mut VM, gen: &Value, stage: Stage) -> Result<Value, String> {
    let (seed, step, mut stages) =
        gen_parts(gen).ok_or_else(|| format!("unresolved reference on {}", obj_label(gen)))?;
    let _ = vm;
    stages.push(stage);
    Ok(alloc(HeapObj::Gen { seed, step, stages }))
}

/// Pull from a lazy sequence until `want` elements have come out, the generator
/// ends, a stage stops it, or [`GEN_PULL_CAP`] is reached.
///
/// `want == None` means "everything", which only terminates because the step
/// eventually answers `null` or a `take`/`takeWhile` stage is in the pipeline.
/// That is exactly the contract Kotlin gives: an unbounded pipeline with a
/// non-short-circuiting terminal does not finish.
fn gen_pull(vm: &mut VM, gen: &Value, want: Option<usize>) -> Result<Vec<Value>, String> {
    let (mut seed, step, mut stages) =
        gen_parts(gen).ok_or_else(|| format!("unresolved reference on {}", obj_label(gen)))?;
    let mut out: Vec<Value> = Vec::new();
    let mut pulled = 0usize;
    'source: while let Some(cur) = seed.clone() {
        if want.is_some_and(|n| out.len() >= n) {
            break;
        }
        pulled += 1;
        if pulled > GEN_PULL_CAP {
            return Err(format!(
                "java.lang.IllegalStateException: sequence produced more than {GEN_PULL_CAP} \
                 elements without a `take`, `takeWhile` or a step returning null"
            ));
        }
        // Advance the source BEFORE the stages run: a stage may end the
        // sequence, and the next seed is a property of the source alone.
        let next = invoke_closure(vm, &step, std::slice::from_ref(&cur))?;
        seed = if matches!(next, Value::Undef) {
            None
        } else {
            Some(next)
        };

        let mut v = cur;
        for stage in stages.iter_mut() {
            match stage.clone() {
                Stage::Map(f) => v = invoke_closure(vm, &f, std::slice::from_ref(&v))?,
                Stage::Filter(f, keep) => {
                    if truthy(&invoke_closure(vm, &f, std::slice::from_ref(&v))?) != keep {
                        continue 'source;
                    }
                }
                Stage::TakeWhile(f) => {
                    if !truthy(&invoke_closure(vm, &f, std::slice::from_ref(&v))?) {
                        break 'source;
                    }
                }
                Stage::DropWhile(f, done) => {
                    if !done {
                        if truthy(&invoke_closure(vm, &f, std::slice::from_ref(&v))?) {
                            continue 'source;
                        }
                        *stage = Stage::DropWhile(f, true);
                    }
                }
                Stage::Take(n) => {
                    if n <= 0 {
                        break 'source;
                    }
                    *stage = Stage::Take(n - 1);
                }
                Stage::Drop(n) => {
                    if n > 0 {
                        *stage = Stage::Drop(n - 1);
                        continue 'source;
                    }
                }
            }
        }
        out.push(v);
    }
    Ok(out)
}

/// `KT_GENSEQ` — see [`KT_GENSEQ`].
fn b_genseq(vm: &mut VM, _argc: u8) -> Value {
    let step = vm.pop();
    let seed = vm.pop();
    // A `null` seed is an empty sequence, exactly as it ends one.
    let seed = (!matches!(seed, Value::Undef)).then_some(seed);
    alloc(HeapObj::Gen {
        seed,
        step,
        stages: Vec::new(),
    })
}

/// `KT_COMPARATOR` — see [`KT_COMPARATOR`].
fn b_comparator(vm: &mut VM, argc: u8) -> Value {
    let name = vm.pop().to_str();
    let mut keys = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        keys.push(vm.pop());
    }
    keys.reverse();
    let descending = name.ends_with("Descending");
    let mut chain = if name.starts_with("then") {
        // `thenBy` answers a NEW comparator: the base's keys are copied, not
        // extended in place, so `val byA = compareBy { … }` keeps its own order
        // after `byA.thenBy { … }` is built from it.
        let base = vm.pop();
        comparator_keys(&base).unwrap_or_default()
    } else {
        Vec::new()
    };
    chain.extend(keys.into_iter().map(|k| (k, descending)));
    alloc(HeapObj::Comparator(chain))
}

/// The `(selector, descending)` chain of a comparator value, or `None` when `v`
/// is something else — a plain two-argument lambda, most often.
fn comparator_keys(v: &Value) -> Option<Vec<(Value, bool)>> {
    with_obj(v, |o| match o {
        HeapObj::Comparator(keys) => Some(keys.clone()),
        _ => None,
    })
    .flatten()
}

/// Order two elements by `cmp`, which is either a `Comparator` built by
/// `compareBy` or a plain `(a, b) -> Int` lambda. Answers the sign convention
/// `Comparator.compare` uses: negative when `a` sorts first.
///
/// A comparator's keys are compared left to right and the FIRST non-equal key
/// decides, so a later key is consulted only on a tie — which is what makes
/// `compareBy { it.a }.thenBy { it.b }` a tiebreak rather than a second sort.
fn compare_with(vm: &mut VM, cmp: &Value, a: &Value, b: &Value) -> Result<i64, String> {
    let Some(keys) = comparator_keys(cmp) else {
        return Ok(invoke_closure(vm, cmp, &[a.clone(), b.clone()])?.to_int());
    };
    for (sel, descending) in keys {
        let ka = invoke_closure(vm, &sel, std::slice::from_ref(a))?;
        let kb = invoke_closure(vm, &sel, std::slice::from_ref(b))?;
        let ord = match value_cmp(&ka, &kb) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
        if ord != 0 {
            return Ok(if descending { -ord } else { ord });
        }
    }
    Ok(0)
}

/// `KT_PRECOND` — see [`KT_PRECOND`]. Stack: `[subject?, message?, nameStr]`.
fn b_precond(vm: &mut VM, argc: u8) -> Value {
    let name = vm.pop().to_str();
    let mut vals = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        vals.push(vm.pop());
    }
    vals.reverse();
    // `error(msg)` / `TODO(reason)` have no condition to test — their single
    // argument is the message. Everything else reads `(subject, message?)`.
    let unconditional = matches!(name.as_str(), "error" | "TODO");
    let mut vals = vals.into_iter();
    let subject = if unconditional {
        Value::Undef
    } else {
        vals.next().unwrap_or(Value::Undef)
    };
    let message = vals.next();
    // `require`/`check` fail on a false condition; the `…NotNull` pair fails on
    // Kotlin `null` and otherwise ANSWERS the value, which is what lets
    // `val x = checkNotNull(maybe)` stand in for the value. `error`/`TODO`
    // always fail.
    let (ok, value) = match name.as_str() {
        "require" | "check" => (truthy(&subject), Value::Undef),
        "requireNotNull" | "checkNotNull" => (!matches!(subject, Value::Undef), subject.clone()),
        _ => (false, Value::Undef),
    };
    if ok {
        return value;
    }
    // The lazy message is a lambda so it only runs on the failing path —
    // `require(ok) { expensive() }` must not call `expensive` when `ok` holds.
    let msg = match &message {
        Some(m) if closure_meta(m).is_some() => match invoke_closure(vm, m, &[]) {
            Ok(v) => Some(kotlin_string(&v)),
            Err(e) => {
                fault(vm, e);
                return Value::Undef;
            }
        },
        Some(m) => Some(kotlin_string(m)),
        None => None,
    };
    let (class, default) = match name.as_str() {
        "require" => ("IllegalArgumentException", "Failed requirement."),
        "requireNotNull" => ("IllegalArgumentException", "Required value was null."),
        "check" => ("IllegalStateException", "Check failed."),
        "checkNotNull" => ("IllegalStateException", "Required value was null."),
        "error" => ("IllegalStateException", ""),
        _ => ("NotImplementedError", ""),
    };
    // `TODO(reason)` APPENDS the reason to the fixed sentence rather than
    // replacing it: `An operation is not implemented: reason`.
    let text = match (name.as_str(), &msg) {
        ("TODO", Some(m)) => format!("An operation is not implemented: {m}"),
        ("TODO", None) => "An operation is not implemented.".to_string(),
        (_, Some(m)) => m.clone(),
        (_, None) => default.to_string(),
    };
    let fqn = throwable_fqn(class).unwrap_or(class);
    raise(vm, new_throwable(fqn, Some(&text)));
    Value::Undef
}

/// `KT_RESULT_HOF` — see [`KT_RESULT_HOF`].
fn b_result_hof(vm: &mut VM, _argc: u8) -> Value {
    let name = vm.pop().to_str();
    let clo = vm.pop();
    let recv = vm.pop();
    let Some((value, err)) = result_parts(&recv) else {
        fault(vm, format!("unresolved reference: {name}"));
        return Value::Undef;
    };
    let res = match (name.as_str(), &err) {
        // `getOrElse` hands the FAILURE to the block; a success skips it.
        ("getOrElse", Some(e)) => invoke_closure(vm, &clo, std::slice::from_ref(e)),
        ("getOrElse", None) => Ok(value),
        // `onSuccess`/`onFailure` run for their effect and yield the receiver.
        ("onSuccess", None) => invoke_closure(vm, &clo, &[value]).map(|_| recv),
        ("onFailure", Some(e)) => invoke_closure(vm, &clo, std::slice::from_ref(e)).map(|_| recv),
        ("onSuccess", Some(_)) | ("onFailure", None) => Ok(recv),
        // `map` transforms a success and passes a failure through unchanged.
        ("map", None) => invoke_closure(vm, &clo, &[value]).map(|v| {
            alloc(HeapObj::Res {
                value: v,
                err: None,
            })
        }),
        ("map", Some(_)) => Ok(recv),
        _ => Err(format!("unresolved reference: {name}")),
    };
    match res {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `KT_LAZY_GET`: the value of a `by lazy` cell, computing and caching it on the
/// first read. Stack: `cell`.
fn b_lazy_get(vm: &mut VM, _argc: u8) -> Value {
    let cell = vm.pop();
    let state = with_obj(&cell, |o| match o {
        HeapObj::Lazy { thunk, value } => Some((thunk.clone(), value.clone())),
        _ => None,
    })
    .flatten();
    // Not a cell: the read was of an ordinary value, so hand it back untouched.
    let Some((thunk, cached)) = state else {
        return cell;
    };
    if let Some(v) = cached {
        return v;
    }
    // The thunk runs with the heap borrow released — it is user code and may
    // allocate, or read this very cell.
    match invoke_closure(vm, &thunk, &[]) {
        Ok(v) => {
            with_obj_mut(&cell, |o| {
                if let HeapObj::Lazy { value, .. } = o {
                    *value = Some(v.clone());
                }
            });
            v
        }
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// `KT_SCOPE_FN`: an `it`-form scope function on any receiver. Stack (top-down):
/// `nameStr, closure, recv`.
fn b_scope_fn(vm: &mut VM, _argc: u8) -> Value {
    let name = vm.pop().to_str();
    let clo = vm.pop();
    let recv = vm.pop();
    let res = match name.as_str() {
        // `let` — run the block with `it` = receiver, yield the block's result.
        // `run` is the same call with the receiver bound as the block's `this`;
        // the compiler decided which by naming the block's parameter, so the two
        // are one arm here.
        "let" | "run" => invoke_closure(vm, &clo, std::slice::from_ref(&recv)),
        // `also` — run the block for its side effect, yield the receiver.
        // `apply` is the `this`-form of the same.
        "also" | "apply" => invoke_closure(vm, &clo, std::slice::from_ref(&recv)).map(|_| recv),
        // `takeIf`/`takeUnless` — yield the receiver when the predicate holds
        // (or fails to), else null.
        "takeIf" | "takeUnless" => {
            let want = name == "takeIf";
            invoke_closure(vm, &clo, std::slice::from_ref(&recv)).map(|p| {
                if truthy(&p) == want {
                    recv
                } else {
                    Value::Undef
                }
            })
        }
        _ => Err(format!("unresolved reference: {name}")),
    };
    match res {
        Ok(v) => v,
        Err(e) => {
            fault(vm, e);
            Value::Undef
        }
    }
}

/// Snapshot an iterable receiver's elements (a clone taken under a shared borrow,
/// so the borrow is released before any closure runs — a closure body may
/// re-enter the heap). Ranges materialize here, which is what makes
/// `(1..3).map { … }` work. `Map`/`Pair` receivers aren't iterable by these
/// methods here.
fn list_snapshot(recv: &Value) -> Option<Vec<Value>> {
    // A `Map` iterates as its entries, so `m.map { it.key }` and `m.any { … }`
    // see one `Map.Entry` per element — carried here as a `Pair`, whose
    // `key`/`value` members are the entry's accessors. The pairs are allocated
    // AFTER the borrow is released: `alloc` takes the heap mutably.
    let entries = with_obj(recv, |o| match o {
        HeapObj::Map(entries) => Some(entries.clone()),
        _ => None,
    })
    .flatten();
    if let Some(entries) = entries {
        return Some(
            entries
                .into_iter()
                .map(|(k, v)| alloc(HeapObj::Entry(k, v)))
                .collect(),
        );
    }
    with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Set(items) | HeapObj::Array { items, .. } => {
            Some(items.clone())
        }
        HeapObj::Range(r) => Some(r.to_vec()),
        _ => None,
    })
    .flatten()
}

/// Kotlin predicate truthiness: predicates return `Boolean`, so only `true`
/// counts (a `null`/non-Bool result is treated as `false`).
fn truthy(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}

/// Total order over the comparable values a selector yields: strings compare
/// lexicographically, everything else numerically. Used by `sortedBy` /
/// `maxByOrNull` selector results.
fn value_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        // Every enum is `Comparable` by DECLARATION order, which is exactly what
        // `ordinal` records — so `sorted()` on a `List<E>` restores the order the
        // constants were written in, whatever their names sort like.
        _ => match (enum_ordinal(a), enum_ordinal(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => value_cmp_scalar(a, b),
        },
    }
}

/// `java.lang.Double.compare` — a TOTAL order, which IEEE comparison is not.
///
/// `partial_cmp` answers `None` for a NaN operand, and the `unwrap_or(Equal)`
/// that used to follow it turned every NaN comparison into a tie: `NaN
/// .compareTo(1.0)` answered 0 where the JVM answers 1, `sorted()` left NaN
/// wherever it happened to sit, and `max()` over a list containing one answered
/// the largest non-NaN instead of NaN. It also made `0.0` and `-0.0`
/// interchangeable, where `Double.compare` separates them.
///
/// `Double.compare` falls back to the raw bits, so all NaNs (canonicalised by
/// `doubleToLongBits`) sort ABOVE every number and `-0.0` sorts BELOW `0.0`.
/// Rust's `total_cmp` is that same bit order, once NaN is canonicalised — it
/// otherwise puts a sign-bit-set NaN below everything, which the JVM's
/// canonicalisation rules out.
fn double_compare(x: f64, y: f64) -> std::cmp::Ordering {
    let canon = |v: f64| if v.is_nan() { f64::NAN } else { v };
    canon(x).total_cmp(&canon(y))
}

/// [`value_cmp`] for everything that is not an enum constant.
fn value_cmp_scalar(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        // `Char` is `Comparable<Char>` by code unit, so a `List<Char>` sorts and
        // reduces (`max`/`min`) like any other comparable element.
        _ if is_char(a) || is_char(b) => num_of(a).cmp(&num_of(b)),
        // Integral operands compare as integers. Routing them through `f64`
        // would collapse two `Long`s that differ past the 53-bit mantissa.
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => double_compare(a.to_float(), b.to_float()),
    }
}

/// The higher-order collection methods, over a snapshot of `recv`'s elements,
/// invoking `clo` per element. Mirrors the Kotlin stdlib signatures faithfully.
/// Re-wrap the surviving elements of a `filter`-family call in the receiver's
/// own container kind. Kotlin's `filter` is declared per receiver type — a
/// `Map` filters to a `Map` (`{b=2}`) and a `Set` to a `Set`, where a `List`
/// filters to a `List`. The elements of a filtered `Map` are the `Pair`s
/// [`list_snapshot`] produced, so they fold straight back into entries.
fn same_kind_as(recv: &Value, out: Vec<Value>) -> Value {
    let kind = with_obj(recv, |o| match o {
        HeapObj::Map(_) => 2u8,
        HeapObj::Set(_) => 1,
        _ => 0,
    })
    .unwrap_or(0);
    match kind {
        2 => {
            let entries = out
                .iter()
                .filter_map(|p| {
                    with_obj(p, |o| match o {
                        HeapObj::Pair(k, v) | HeapObj::Entry(k, v) => Some((k.clone(), v.clone())),
                        _ => None,
                    })
                    .flatten()
                })
                .collect();
            alloc(HeapObj::Map(entries))
        }
        1 => alloc(HeapObj::Set(out)),
        _ => alloc(HeapObj::List(out)),
    }
}

/// The `chunked`/`windowed` groups of a `CharSequence` receiver, each rebuilt
/// as a `String` — which is what the `kotlin.text` overload hands its lambda
/// and what its no-lambda form returns (`"abc".chunked(2)` is `[ab, c]`).
/// Kotlin's `checkWindowSizeStep` — the ONE bounds check behind every
/// `chunked`/`windowed` overload, on either a collection or a `CharSequence`.
///
/// It has TWO messages, not one: `chunked(size)` is `windowed(size, size)`, and
/// where size and step agree naming both would say the same number twice, so
/// the stdlib drops the step from the text. Three call sites here each spelled
/// their own single message, which was the `Both …` case's wording without the
/// `Both` and the `size … must be` case's numbers duplicated — wrong for both.
fn check_window_size_step(size: i64, step: i64) -> Result<(), String> {
    if size > 0 && step > 0 {
        return Ok(());
    }
    Err(format!(
        "java.lang.IllegalArgumentException: {}",
        if size == step {
            format!("size {size} must be greater than zero.")
        } else {
            format!("Both size {size} and step {step} must be greater than zero.")
        }
    ))
}

fn coll_hof_str_groups(name: &str, chars: &Value, extras: &[Value]) -> Result<Vec<Value>, String> {
    let items = list_snapshot(chars).unwrap_or_default();
    let size = extras.first().map(|v| v.to_int()).unwrap_or(0);
    let chunking = name == "chunked";
    let step = if chunking {
        size
    } else {
        extras.get(1).map(|v| v.to_int()).unwrap_or(1)
    };
    check_window_size_step(size, step)?;
    let partial = chunking || extras.get(2).is_some_and(truthy);
    Ok(windows_of(&items, size as usize, step as usize, partial)
        .iter()
        .map(chars_to_string)
        .collect())
}

/// A string's characters as `Char` values, indexed by UTF-16 code unit — the
/// same basis `String.length` and `s[i]` use.
pub fn chars_of(s: &str) -> Vec<Value> {
    s.encode_utf16().map(|u| char_of(u as i64)).collect()
}

/// Concatenate a sequence of `Char`s back into a `String`. The inverse of
/// [`chars_of`], used to give a `CharSequence` receiver the `String` result its
/// `kotlin.text` overload has where the `Iterable` one would give a `List`.
fn chars_to_string(v: &Value) -> Value {
    let items = list_snapshot(v).unwrap_or_default();
    Value::str(items.iter().map(kotlin_string).collect::<String>())
}

/// Whether the `kotlin.text` overload of `name` — the one with a `CharSequence`
/// receiver — answers a `String` where the `Iterable` overload answers a
/// `List`. `map`/`flatMap`/`groupBy` are deliberately absent: those keep their
/// `List`/`Map` result on a `String` receiver too (`"abc".map { it }` is
/// `[a, b, c]`, not `abc`).
fn charseq_returns_string(name: &str) -> bool {
    matches!(
        name,
        "filter"
            | "filterNot"
            | "filterIndexed"
            | "takeWhile"
            | "dropWhile"
            | "trim"
            | "trimStart"
            | "trimEnd"
            | "onEach"
    )
}

fn coll_hof(
    vm: &mut VM,
    name: &str,
    recv: &Value,
    extras: &[Value],
    clo: &Value,
    // The receiver kind the CALL was written against, carried through the two
    // recursions below because both change the representation: a `String`
    // re-enters as its `Char`s and a lazy pipeline as the list it pulled, and
    // neither is the type whose diagnostics Kotlin spells.
    kind: SeqKind,
) -> Result<Value, String> {
    // A lazy sequence receiver. The four stages that can be applied one element
    // at a time stay lazy — that is what keeps `generateSequence(1) { it * 2 }
    // .map { … }.take(5)` finite. The short-circuiting searches pull only as far
    // as they must, and every other member materializes first, which on an
    // unbounded pipeline is a fault (see [`GEN_PULL_CAP`]) rather than a hang.
    if gen_parts(recv).is_some() {
        match name {
            "map" => return gen_with(vm, recv, Stage::Map(clo.clone())),
            "filter" => return gen_with(vm, recv, Stage::Filter(clo.clone(), true)),
            "filterNot" => return gen_with(vm, recv, Stage::Filter(clo.clone(), false)),
            "takeWhile" => return gen_with(vm, recv, Stage::TakeWhile(clo.clone())),
            "dropWhile" => return gen_with(vm, recv, Stage::DropWhile(clo.clone(), false)),
            // `first { }` / `find { }` / `any { }` stop at the first match, so
            // they are pulled one element at a time and terminate on an endless
            // sequence — as they do on Kotlin's.
            "first" | "firstOrNull" | "find" | "any" | "indexOfFirst" => {
                let mut at = 0i64;
                loop {
                    let batch = gen_pull(vm, recv, Some(at as usize + 1))?;
                    let Some(v) = batch.get(at as usize) else {
                        return Ok(match name {
                            "any" => Value::Bool(false),
                            "indexOfFirst" => Value::Int(-1),
                            "first" => return Err(kind.no_match(name)),
                            _ => Value::Undef,
                        });
                    };
                    if truthy(&invoke_closure(vm, clo, std::slice::from_ref(v))?) {
                        return Ok(match name {
                            "any" => Value::Bool(true),
                            "indexOfFirst" => Value::Int(at),
                            _ => v.clone(),
                        });
                    }
                    at += 1;
                }
            }
            _ => {
                // Tagged: materializing a lazy sequence must not switch its diagnostics
                // onto the `List` wording — see [`SEQ_VIEW`].
                let items = tag_sequence(alloc(HeapObj::List(gen_pull(vm, recv, None)?)));
                return coll_hof(vm, name, &items, extras, clo, kind);
            }
        }
    }
    // A `String` receiver: `kotlin.text` mirrors most of the collection API on
    // `CharSequence`, iterating the characters. The shared implementation below
    // works on the materialized `Char`s; only the RESULT type differs, and
    // only for the members whose text overload rebuilds a string.
    if let Value::Str(s) = recv {
        let chars = alloc(HeapObj::List(chars_of(s)));
        // `chunked`/`windowed` hand each group to the lambda as a `String`, not
        // as a `List<Char>`, so the groups are rebuilt before the callback.
        if matches!(name, "chunked" | "windowed") {
            let groups = coll_hof_str_groups(name, &chars, extras)?;
            let mut out = Vec::with_capacity(groups.len());
            for g in groups {
                out.push(invoke_closure(vm, clo, &[g])?);
            }
            return Ok(alloc(HeapObj::List(out)));
        }
        let out = coll_hof(vm, name, &chars, extras, clo, kind)?;
        if charseq_returns_string(name) {
            return Ok(chars_to_string(&out));
        }
        // `partition` yields a `Pair<String, String>` on a `CharSequence`.
        if name == "partition" {
            if let Some((a, b)) = with_obj(&out, |o| match o {
                HeapObj::Pair(a, b) => Some((a.clone(), b.clone())),
                _ => None,
            })
            .flatten()
            {
                return Ok(alloc(HeapObj::Pair(
                    chars_to_string(&a),
                    chars_to_string(&b),
                )));
            }
        }
        return Ok(out);
    }
    let items = list_snapshot(recv)
        .ok_or_else(|| format!("unresolved reference: {name} on {}", obj_label(recv)))?;
    match name {
        "map" => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(invoke_closure(vm, clo, &[it])?);
            }
            Ok(alloc(HeapObj::List(out)))
        }
        "filter" => {
            let mut out = Vec::new();
            for it in items {
                if truthy(&invoke_closure(vm, clo, std::slice::from_ref(&it))?) {
                    out.push(it);
                }
            }
            Ok(same_kind_as(recv, out))
        }
        "forEach" => {
            for it in items {
                invoke_closure(vm, clo, &[it])?;
            }
            Ok(Value::Undef)
        }
        // `onEach` is `forEach` that answers the RECEIVER, so it chains
        // (`list.onEach { … }.size`). That return value is the whole
        // difference between the two.
        "onEach" => {
            for it in &items {
                invoke_closure(vm, clo, std::slice::from_ref(it))?;
            }
            Ok(alloc(HeapObj::List(items)))
        }
        "fold" => {
            let mut acc = extras.first().cloned().unwrap_or(Value::Undef);
            for it in items {
                acc = invoke_closure(vm, clo, &[acc, it])?;
            }
            Ok(acc)
        }
        // `foldRight` walks from the END, and its lambda takes the arguments the
        // other way round — `(element, acc)`, not `(acc, element)`. Both halves
        // are observable at once: `listOf(1, 2, 3).foldRight("") { a, b ->
        // "$a$b" }` is `123`, where `fold` on the same lambda gives `321`.
        "foldRight" => {
            let mut acc = extras.first().cloned().unwrap_or(Value::Undef);
            for it in items.into_iter().rev() {
                acc = invoke_closure(vm, clo, &[it, acc])?;
            }
            Ok(acc)
        }
        // `runningFold` (and its alias `scan`) is `fold` that keeps every
        // intermediate accumulator, INCLUDING the initial one — so the result
        // is one longer than the input.
        "runningFold" | "scan" => {
            let mut acc = extras.first().cloned().unwrap_or(Value::Undef);
            let mut out = vec![acc.clone()];
            for it in items {
                acc = invoke_closure(vm, clo, &[acc, it])?;
                out.push(acc.clone());
            }
            Ok(alloc(HeapObj::List(out)))
        }
        // The same for `reduce`: the first element seeds the accumulator, so an
        // empty input gives an empty result rather than an error.
        "runningReduce" | "scanReduce" => {
            let mut iter = items.into_iter();
            let Some(mut acc) = iter.next() else {
                return Ok(alloc(HeapObj::List(Vec::new())));
            };
            let mut out = vec![acc.clone()];
            for it in iter {
                acc = invoke_closure(vm, clo, &[acc, it])?;
                out.push(acc.clone());
            }
            Ok(alloc(HeapObj::List(out)))
        }
        // `mapNotNull` drops the results that came back null.
        "mapNotNull" => {
            let mut out = Vec::new();
            for it in items {
                let v = invoke_closure(vm, clo, &[it])?;
                if !matches!(v, Value::Undef) {
                    out.push(v);
                }
            }
            Ok(alloc(HeapObj::List(out)))
        }
        "flatMapIndexed" => {
            let mut out = Vec::new();
            for (i, it) in items.into_iter().enumerate() {
                let sub = invoke_closure(vm, clo, &[Value::Int(i as i64), it])?;
                out.extend(sequence_items(&sub));
            }
            Ok(alloc(HeapObj::List(out)))
        }
        // The predicate forms of `trim`/`trimStart`/`trimEnd`, which only a
        // `CharSequence` receiver has: drop elements from the requested end(s)
        // for as long as the predicate holds.
        "trim" | "trimStart" | "trimEnd" => {
            let mut lo = 0usize;
            let mut hi = items.len();
            if name != "trimEnd" {
                while lo < hi && truthy(&invoke_closure(vm, clo, &[items[lo].clone()])?) {
                    lo += 1;
                }
            }
            if name != "trimStart" {
                while hi > lo && truthy(&invoke_closure(vm, clo, &[items[hi - 1].clone()])?) {
                    hi -= 1;
                }
            }
            Ok(alloc(HeapObj::List(items[lo..hi].to_vec())))
        }
        "reduce" => {
            let mut iter = items.into_iter();
            let mut acc = iter.next().ok_or_else(|| {
                format!(
                    "java.lang.UnsupportedOperationException: Empty {} can't be reduced.",
                    kind.reduce_noun(name)
                )
            })?;
            for it in iter {
                acc = invoke_closure(vm, clo, &[acc, it])?;
            }
            Ok(acc)
        }
        // `reduceRight` is to `reduce` what `foldRight` is to `fold`: the seed
        // is the LAST element, the walk runs backwards, and the lambda's
        // arguments are `(element, acc)`.
        "reduceRight" => {
            let mut iter = items.into_iter().rev();
            let mut acc = iter.next().ok_or_else(|| {
                format!(
                    "java.lang.UnsupportedOperationException: Empty {} can't be reduced.",
                    kind.reduce_noun(name)
                )
            })?;
            for it in iter {
                acc = invoke_closure(vm, clo, &[it, acc])?;
            }
            Ok(acc)
        }
        "any" => {
            for it in items {
                if truthy(&invoke_closure(vm, clo, &[it])?) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        "all" => {
            for it in items {
                if !truthy(&invoke_closure(vm, clo, &[it])?) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        "count" => {
            let mut n = 0i64;
            for it in items {
                if truthy(&invoke_closure(vm, clo, &[it])?) {
                    n += 1;
                }
            }
            Ok(Value::Int(n))
        }
        "sumOf" => {
            let mut mapped = Vec::with_capacity(items.len());
            for it in items {
                mapped.push(invoke_closure(vm, clo, &[it])?);
            }
            Ok(sum_values(&mapped))
        }
        // `maxBy`/`minBy` and their `…OrNull` twins differ only on an EMPTY
        // receiver, where the plain pair throws — bare, with no detail, the way
        // `max`/`min` do. Kotlin deprecated the throwing spelling in 1.4 and
        // brought it back in 1.7 with these semantics, so a frontend that has
        // only the `…OrNull` form answers `unresolved reference` for code the
        // current compiler accepts.
        "maxByOrNull" | "maxBy" => {
            let mut best: Option<(Value, Value)> = None; // (element, selector)
            for it in items {
                let sel = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                let take = match &best {
                    Some((_, bsel)) => value_cmp(&sel, bsel) == std::cmp::Ordering::Greater,
                    None => true,
                };
                if take {
                    best = Some((it, sel));
                }
            }
            extremum_or(best.map(|(el, _)| el), name)
        }
        "sortedBy" => {
            // Decorate with the selector, stable-sort, undecorate (schwartzian) —
            // keeps the closure evaluated once per element and preserves the
            // input order among equal keys (Kotlin `sortedBy` is stable).
            let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(items.len());
            for it in items {
                let key = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                keyed.push((key, it));
            }
            keyed.sort_by(|a, b| value_cmp(&a.0, &b.0));
            Ok(alloc_sorted_list(
                keyed.into_iter().map(|(_, it)| it).collect(),
                kind.is_collection(),
            ))
        }
        "minByOrNull" | "minBy" => {
            let mut best: Option<(Value, Value)> = None;
            for it in items {
                let sel = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                let take = match &best {
                    Some((_, bsel)) => value_cmp(&sel, bsel) == std::cmp::Ordering::Less,
                    None => true,
                };
                if take {
                    best = Some((it, sel));
                }
            }
            extremum_or(best.map(|(el, _)| el), name)
        }
        "none" => {
            for it in items {
                if truthy(&invoke_closure(vm, clo, &[it])?) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        "filterNot" => {
            let mut out = Vec::new();
            for it in items {
                if !truthy(&invoke_closure(vm, clo, std::slice::from_ref(&it))?) {
                    out.push(it);
                }
            }
            Ok(same_kind_as(recv, out))
        }
        // `partition` returns a `Pair(matching, rest)` — one pass, predicate
        // applied to every element exactly once.
        "partition" => {
            let (mut yes, mut no) = (Vec::new(), Vec::new());
            for it in items {
                if truthy(&invoke_closure(vm, clo, std::slice::from_ref(&it))?) {
                    yes.push(it);
                } else {
                    no.push(it);
                }
            }
            Ok(alloc(HeapObj::Pair(
                alloc(HeapObj::List(yes)),
                alloc(HeapObj::List(no)),
            )))
        }
        // `takeWhile`/`dropWhile` cut at the FIRST element failing the
        // predicate — later matches do not rejoin, unlike `filter`.
        "takeWhile" | "dropWhile" => {
            let mut cut = items.len();
            for (i, it) in items.iter().enumerate() {
                if !truthy(&invoke_closure(vm, clo, std::slice::from_ref(it))?) {
                    cut = i;
                    break;
                }
            }
            let out = if name == "takeWhile" {
                items[..cut].to_vec()
            } else {
                items[cut..].to_vec()
            };
            Ok(alloc(HeapObj::List(out)))
        }
        // `firstOrNull`/`lastOrNull` with a predicate: the matching element, or
        // null when none matches.
        "flatMap" => {
            // Each result is itself iterable; its elements are spliced in.
            let mut out = Vec::new();
            for it in items {
                let sub = invoke_closure(vm, clo, &[it])?;
                out.extend(sequence_items(&sub));
            }
            Ok(alloc(HeapObj::List(out)))
        }
        "mapIndexed" => {
            let mut out = Vec::with_capacity(items.len());
            for (i, it) in items.into_iter().enumerate() {
                out.push(invoke_closure(vm, clo, &[Value::Int(i as i64), it])?);
            }
            Ok(alloc(HeapObj::List(out)))
        }
        "sortedByDescending" => {
            let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(items.len());
            for it in items {
                let key = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                keyed.push((key, it));
            }
            // Reversing a stable ascending sort would also reverse the ties;
            // Kotlin keeps them in input order, so the comparison is flipped
            // instead.
            keyed.sort_by(|a, b| value_cmp(&b.0, &a.0));
            Ok(alloc_sorted_list(
                keyed.into_iter().map(|(_, it)| it).collect(),
                kind.is_collection(),
            ))
        }
        // `associate` takes the lambda's `Pair` result as the entry; `associateBy`
        // takes its result as the KEY and the element as the value — the mirror
        // image of `associateWith`.
        "associate" | "associateBy" => {
            let mut entries: Vec<(Value, Value)> = Vec::with_capacity(items.len());
            for it in items {
                let out = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                let (k, v) = if name == "associateBy" {
                    (out, it)
                } else {
                    match with_obj(&out, |o| match o {
                        HeapObj::Pair(a, b) => Some((a.clone(), b.clone())),
                        _ => None,
                    })
                    .flatten()
                    {
                        Some(kv) => kv,
                        None => return Err("kotlin: associate expects a Pair".to_string()),
                    }
                };
                if let Some(slot) = entries.iter_mut().find(|(ek, _)| value_eq(ek, &k)) {
                    slot.1 = v;
                } else {
                    entries.push((k, v));
                }
            }
            Ok(alloc(HeapObj::Map(entries)))
        }
        "associateWith" => {
            let mut entries: Vec<(Value, Value)> = Vec::with_capacity(items.len());
            for it in items {
                let v = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                // Later duplicate keys overwrite (Kotlin `associateWith`).
                if let Some(slot) = entries.iter_mut().find(|(k, _)| value_eq(k, &it)) {
                    slot.1 = v;
                } else {
                    entries.push((it, v));
                }
            }
            Ok(alloc(HeapObj::Map(entries)))
        }
        // `groupingBy` does no work: it pairs the source with the key selector
        // and defers everything to a terminal operation. That laziness is the
        // whole point of `Grouping` over `groupBy` — `eachCount` never builds
        // the per-key LISTS that `groupBy` materializes.
        "groupingBy" => Ok(alloc(HeapObj::Grouping {
            items: items.to_vec(),
            key: clo.clone(),
        })),
        "groupBy" => {
            // key → list of elements, keys in first-appearance order.
            let mut entries: Vec<(Value, Vec<Value>)> = Vec::new();
            for it in items {
                let key = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                match entries.iter_mut().find(|(k, _)| value_eq(k, &key)) {
                    Some(slot) => slot.1.push(it),
                    None => entries.push((key, vec![it])),
                }
            }
            let entries = entries
                .into_iter()
                .map(|(k, v)| (k, alloc(HeapObj::List(v))))
                .collect();
            Ok(alloc(HeapObj::Map(entries)))
        }
        // The searching predicates. `first`/`last`/`single` FAULT when nothing
        // matches, where `find`/`findLast`/`singleOrNull` answer null — that is
        // the only difference between the two families. Each used to reach the
        // no-argument member instead, so the predicate was evaluated never and
        // `listOf(1, 2, 3).first { it > 1 }` answered 1.
        "first" | "last" | "find" | "findLast" | "firstOrNull" | "lastOrNull" => {
            let back = matches!(name, "last" | "findLast" | "lastOrNull");
            let mut hit = None;
            for it in items {
                if truthy(&invoke_closure(vm, clo, std::slice::from_ref(&it))?) {
                    hit = Some(it);
                    if !back {
                        break;
                    }
                }
            }
            match (hit, name) {
                (Some(v), _) => Ok(v),
                (None, "first" | "last") => Err(kind.no_match(name)),
                (None, _) => Ok(Value::Undef),
            }
        }
        "single" | "singleOrNull" => {
            let mut hits = Vec::new();
            for it in items {
                if truthy(&invoke_closure(vm, clo, std::slice::from_ref(&it))?) {
                    hits.push(it);
                }
            }
            match (hits.len(), name) {
                (1, _) => Ok(hits.remove(0)),
                (_, "singleOrNull") => Ok(Value::Undef),
                (0, _) => Err(kind.no_match(name)),
                _ => Err(kind.many_match(name)),
            }
        }
        "indexOfFirst" | "indexOfLast" => {
            let mut hit = -1i64;
            for (i, it) in items.into_iter().enumerate() {
                if truthy(&invoke_closure(vm, clo, std::slice::from_ref(&it))?) {
                    hit = i as i64;
                    if name == "indexOfFirst" {
                        break;
                    }
                }
            }
            Ok(Value::Int(hit))
        }
        // The indexed pair of `filter`/`forEach`: the lambda takes the position
        // first, then the element.
        "filterIndexed" | "forEachIndexed" => {
            let filtering = name == "filterIndexed";
            let mut out = Vec::new();
            for (i, it) in items.into_iter().enumerate() {
                let r = invoke_closure(vm, clo, &[Value::Int(i as i64), it.clone()])?;
                if filtering && truthy(&r) {
                    out.push(it);
                }
            }
            if filtering {
                Ok(alloc(HeapObj::List(out)))
            } else {
                Ok(Value::Undef)
            }
        }
        // `maxOf`/`minOf` take a SELECTOR and answer the extreme selected value
        // (not the element); they fault on an empty receiver where the
        // `…OrNull` forms answer null.
        "maxOf" | "minOf" | "maxOfOrNull" | "minOfOrNull" => {
            let want_max = name.starts_with("maxOf");
            let mut best: Option<Value> = None;
            for it in items {
                let sel = invoke_closure(vm, clo, &[it])?;
                let take = match &best {
                    None => true,
                    Some(b) => (value_cmp(&sel, b) == std::cmp::Ordering::Greater) == want_max,
                };
                if take {
                    best = Some(sel);
                }
            }
            extremum_or(best, name)
        }
        // `Map.filterKeys { }` / `filterValues { }` keep the entries whose KEY
        // (or VALUE) satisfies the predicate. Unlike `mapKeys`/`mapValues`
        // below, the lambda sees that half alone, not the whole entry — which
        // is why `filterKeys { it > 1 }` compares an `Int` key and not an
        // `Entry`.
        "filterKeys" | "filterValues" => {
            let on_keys = name == "filterKeys";
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for it in items {
                let Some((k, v)) = with_obj(&it, |o| match o {
                    HeapObj::Entry(k, v) | HeapObj::Pair(k, v) => Some((k.clone(), v.clone())),
                    _ => None,
                })
                .flatten() else {
                    return Err(format!(
                        "unresolved reference: {name} on {}",
                        obj_label(recv)
                    ));
                };
                let probe = if on_keys { &k } else { &v };
                if truthy(&invoke_closure(vm, clo, std::slice::from_ref(probe))?) {
                    entries.push((k, v));
                }
            }
            Ok(alloc(HeapObj::Map(entries)))
        }
        // `zipWithNext { a, b -> … }` transforms each element/successor pair.
        // The no-lambda spelling answers the pairs themselves and resolves in
        // `sequence_member`, which is why the name is an OPTIONAL higher-order
        // function rather than a plain one.
        "zipWithNext" => {
            let mut out = Vec::with_capacity(items.len().saturating_sub(1));
            for w in items.windows(2) {
                out.push(invoke_closure(vm, clo, &[w[0].clone(), w[1].clone()])?);
            }
            Ok(alloc(HeapObj::List(out)))
        }
        // `Map.mapValues { }` — the lambda sees each ENTRY and its result
        // replaces that entry's value; the keys and their order are kept.
        "mapValues" | "mapKeys" => {
            let keys = name == "mapKeys";
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for it in items {
                let (k, v) = match with_obj(&it, |o| match o {
                    HeapObj::Entry(k, v) | HeapObj::Pair(k, v) => Some((k.clone(), v.clone())),
                    _ => None,
                })
                .flatten()
                {
                    Some(kv) => kv,
                    None => {
                        return Err(format!(
                            "unresolved reference: {name} on {}",
                            obj_label(recv)
                        ))
                    }
                };
                let r = invoke_closure(vm, clo, std::slice::from_ref(&it))?;
                let (k, v) = if keys { (r, v) } else { (k, r) };
                if let Some(slot) = entries.iter_mut().find(|(ek, _)| value_eq(ek, &k)) {
                    slot.1 = v;
                } else {
                    entries.push((k, v));
                }
            }
            Ok(alloc(HeapObj::Map(entries)))
        }
        // `getOrElse(index) { default }` — the lambda supplies the fallback and
        // receives the out-of-range index.
        // `getOrElse` is two different members sharing a name. On a `Map` the
        // first argument is a KEY and the fallback lambda takes it; on a
        // sequence it is an INDEX. Routing a map receiver through the index form
        // read `items` as the entry list, so `mapOf("a" to 1).getOrElse("z") {
        // 9 }` answered the entry at index 0 — `a=1` — instead of `9`.
        "getOrElse" => {
            let key = extras.first().cloned().unwrap_or(Value::Undef);
            if matches!(with_obj(recv, |o| matches!(o, HeapObj::Map(_))), Some(true)) {
                let found = items.iter().find_map(|it| {
                    with_obj(it, |o| match o {
                        HeapObj::Entry(k, v) | HeapObj::Pair(k, v) if value_eq(k, &key) => {
                            Some(v.clone())
                        }
                        _ => None,
                    })
                    .flatten()
                });
                return match found {
                    Some(v) => Ok(v),
                    None => invoke_closure(vm, clo, std::slice::from_ref(&key)),
                };
            }
            let i = key.to_int();
            match usize::try_from(i).ok().and_then(|i| items.get(i)) {
                Some(v) => Ok(v.clone()),
                None => invoke_closure(vm, clo, &[Value::Int(i)]),
            }
        }
        // `sortedWith(comparator)` / `sortedBy`-with-a-comparator: the closure
        // answers a negative/zero/positive `Int` for a pair of elements.
        "sortedWith" => {
            // A comparison that raises must not be swallowed by `sort_by`, so
            // the ordering is computed up front over an index permutation.
            let mut order: Vec<usize> = (0..items.len()).collect();
            let mut err = None;
            // Insertion sort keeps the comparison count small and the sort
            // stable, which Kotlin's `sortedWith` guarantees.
            for i in 1..order.len() {
                let mut j = i;
                while j > 0 {
                    // Either a two-argument lambda or a `compareBy` comparator;
                    // `compare_with` knows both.
                    let r = compare_with(
                        vm,
                        clo,
                        &items[order[j - 1]].clone(),
                        &items[order[j]].clone(),
                    );
                    match r {
                        Ok(v) if v > 0 => order.swap(j - 1, j),
                        Ok(_) => break,
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                    j -= 1;
                }
                if err.is_some() {
                    break;
                }
            }
            match err {
                Some(e) => Err(e),
                None => Ok(tag_if_list(
                    same_kind_as(recv, order.into_iter().map(|i| items[i].clone()).collect()),
                    if kind.is_collection() {
                        ListImpl::Sorted
                    } else {
                        ListImpl::ReadOnly
                    },
                )),
            }
        }
        // The transform-taking forms of the grouping/rendering members. Each has
        // a no-lambda spelling that already worked; the lambda was dropped, so
        // `chunked(2) { it.sum() }` answered the raw groups.
        "chunked" | "windowed" => {
            let size = extras.first().map(|v| v.to_int()).unwrap_or(0);
            let chunking = name == "chunked";
            let step = if chunking {
                size
            } else {
                extras.get(1).map(|v| v.to_int()).unwrap_or(1)
            };
            check_window_size_step(size, step)?;
            let partial = chunking || extras.get(2).is_some_and(truthy);
            let groups = windows_of(&items, size as usize, step as usize, partial);
            let mut out = Vec::with_capacity(groups.len());
            for g in groups {
                out.push(invoke_closure(vm, clo, &[g])?);
            }
            Ok(alloc(HeapObj::List(out)))
        }
        "zip" => {
            let other = extras.first().map(sequence_items).unwrap_or_default();
            let mut out = Vec::new();
            for (a, b) in items.iter().zip(other) {
                out.push(invoke_closure(vm, clo, &[a.clone(), b])?);
            }
            Ok(alloc(HeapObj::List(out)))
        }
        "joinToString" => {
            let mut raised = None;
            let mut render = |v: &Value| match invoke_closure(vm, clo, std::slice::from_ref(v)) {
                Ok(r) => Ok(kotlin_string(&r)),
                Err(e) => {
                    raised = Some(e.clone());
                    Err(e)
                }
            };
            let s = join_to_string(&items, extras, 0, Some(&mut render));
            match raised {
                Some(e) => Err(e),
                None => Ok(Value::str(s)),
            }
        }
        _ => Err(format!(
            "unresolved reference: {name} on {}",
            obj_label(recv)
        )),
    }
}

/// Dispatch a Kotlin stdlib member/method on `recv`. `Ok(value)` on success;
/// `Err(message)` for an unresolved member (surfaced as an uncaught exception,
/// matching Kotlin's compile-time `unresolved reference`).
///
/// Only the members faithfully backed here are handled — extend this table as
/// stdlib coverage grows. `String.length` counts UTF-16 code units, matching
/// the JVM `kotlin.String.length` contract (not Unicode scalar count).
/// Render argument `i` as Kotlin would display it, for the arg-taking `String`
/// members. Missing arguments read as the empty string rather than faulting;
/// arity is a compile-time concern, not this table.
fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(kotlin_string).unwrap_or_default()
}

/// `x` with exactly `prec` fraction digits, rounded the way
/// `java.util.Formatter`'s `%f` rounds: HALF_UP applied to the value's SHORTEST
/// round-tripping decimal form, not to the exact binary value.
///
/// The distinction is visible at every tie. The `double` nearest 2.5 is exactly
/// 2.5, and Java's `%.0f` gives `3` where Rust's `{:.0}` gives `2` (it rounds
/// half-to-even). The `double` nearest 0.15 is slightly BELOW 0.15, so rounding
/// the exact value gives `0.1` while Java — rounding the shortest form `0.15` —
/// gives `0.2`. Rust's `{}` yields that same shortest form, always positional
/// and never in exponent notation, so the digits can be rounded as a string.
fn format_fixed(x: f64, prec: usize) -> String {
    if !x.is_finite() {
        return format_double(x);
    }
    let neg = x.is_sign_negative();
    let shortest = format!("{}", x.abs());
    let (int_part, frac_part) = match shortest.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (shortest, String::new()),
    };
    let mut digits: Vec<u8> = int_part.bytes().chain(frac_part.bytes()).collect();
    let mut int_len = int_part.len();
    if frac_part.len() > prec {
        // Drop the excess, then carry when the first dropped digit is >= 5.
        let round_up = frac_part.as_bytes()[prec] >= b'5';
        digits.truncate(int_len + prec);
        if round_up {
            let mut i = digits.len();
            loop {
                if i == 0 {
                    digits.insert(0, b'1');
                    int_len += 1;
                    break;
                }
                i -= 1;
                if digits[i] == b'9' {
                    digits[i] = b'0';
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
    } else {
        digits.resize(int_len + prec, b'0');
    }
    let text = String::from_utf8(digits).expect("ASCII digits");
    let (i, f) = text.split_at(int_len);
    let body = if prec == 0 {
        i.to_string()
    } else {
        format!("{i}.{f}")
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

/// Insert `en_US` thousands separators into the INTEGER part of an already
/// rendered number, leaving a leading sign and any fraction alone.
///
/// The grouping is fixed at three digits with a `,` because this runtime has no
/// locale: `String.format`'s separator and group size come from
/// `Locale.getDefault()` on the JVM, and `scripts/capture-parity.sh` pins the
/// oracle to `en_US` for exactly that reason.
fn group_thousands(body: &str) -> String {
    let (sign, rest) = match body.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", body),
    };
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    let digits: Vec<char> = int.chars().collect();
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (n, c) in digits.iter().enumerate() {
        // A separator precedes every digit whose distance from the end is a
        // positive multiple of three, which is what leaves `123` untouched.
        if n > 0 && (digits.len() - n) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*c);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// `String.format(args…)` / `java.util.Formatter`, over the conversions a
/// Kotlin program actually reaches for: `%d %s %f %e %x %X %o %c %b %%`, each
/// accepting the `-` (left-justify), `0` (zero-pad), `+` and space flags, a
/// width, and a precision. `%f` defaults to 6 fraction digits, as the JVM does.
///
/// An unknown conversion is an error rather than a silent pass-through, so a
/// format this does not model surfaces instead of quietly diverging.
/// `java.util.Formatter`'s fault for a conversion it does not know.
///
/// The character named is the one the JVM's format-specifier regex stopped on,
/// and BOTH ways of reaching this fault name a character — there is no bare
/// form. Measured on JDK 21.0.12: `%q` and `%5q` both answer `Conversion =
/// 'q'`; a spec that runs off the end answers with the character right after
/// the `%` (`%5` → `'5'`, `%-` → `'-'`, `%.2` → `'.'`) and with `%` itself
/// when the string ends there (`"%"` and `"abc%"` → `'%'`).
fn unknown_conversion(c: char) -> String {
    format!("java.util.UnknownFormatConversionException: Conversion = '{c}'")
}

/// The `Formatter` fault for a width or precision that does not fit an `int`.
///
/// Java parses both into an `int` and then rejects a negative one, so EVERY
/// overflowing spelling lands on the same number: measured on JDK 21.0.12,
/// `%2147483648d`, `%10000000000d`, `%4294967296d` and
/// `%99999999999999999999d` all answer `IllegalFormatWidthException:
/// -2147483648`, and `%.2147483648f` answers `IllegalFormatPrecisionException:
/// -2147483648`. kotlinrs parsed the digits into a `usize` and fell back to
/// `unwrap_or(0)`, so an overflowing width was silently *no* width — and the
/// widths that DO fit a `usize` but not an `int` (`%4294967296d`) tried to pad
/// to four billion characters and hung.
fn illegal_span(kind: &str) -> String {
    format!("java.util.IllegalFormat{kind}Exception: {}", i32::MIN)
}

/// The JVM binary name of the class `v` would be BOXED to, for
/// `IllegalFormatConversionException`.
///
/// `Long`/`Int` and `Float`/`Double` each share one runtime representation
/// here, so a `5L` rejected by `%f` is named `java.lang.Integer` where the JVM
/// names `java.lang.Long`. The CLASS of the throwable, its place in the
/// hierarchy and the conversion character are all exact; only that one operand
/// name is approximate, and only for the two widths kotlinrs does not carry at
/// run time.
fn format_arg_class(v: &Value) -> String {
    if char_code(v).is_some() {
        return "java.lang.Character".to_string();
    }
    match v {
        Value::Int(_) => "java.lang.Integer".to_string(),
        Value::Float(_) => "java.lang.Double".to_string(),
        Value::Bool(_) => "java.lang.Boolean".to_string(),
        Value::Str(_) => "java.lang.String".to_string(),
        _ => runtime_jvm_class(v)
            .map(|(n, _)| n)
            .or_else(|| jvm_class(&obj_label(v)).map(|(n, _)| n.to_string()))
            .unwrap_or_else(|| obj_label(v)),
    }
}

/// Whether `conv` accepts `arg`, per `java.util.Formatter`'s conversion
/// categories. Measured on JDK 21.0.12: `%f` REJECTS an `Int`
/// (`f != java.lang.Integer`), `%d` rejects a `Double`, `%c` takes a `Char` or
/// an `Int` but not a `String`, and `%s`/`%b` take anything. A null argument is
/// accepted everywhere — the JVM prints `null` rather than faulting.
fn conversion_accepts(conv: char, arg: &Value) -> bool {
    if matches!(arg, Value::Undef) {
        return true;
    }
    let is_char = char_code(arg).is_some();
    match conv {
        // Integral only. A `Char` is a `Character`, not an integral box.
        'd' | 'x' | 'X' | 'o' => matches!(arg, Value::Int(_)) && !is_char,
        'f' | 'e' | 'E' => matches!(arg, Value::Float(_)),
        'c' => is_char || matches!(arg, Value::Int(_)),
        _ => true,
    }
}

fn format_string(fmt: &str, args: &[Value]) -> Result<String, String> {
    let mut out = String::new();
    let mut argi = 0usize;
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // The whole specifier as WRITTEN, accumulated as it is consumed:
        // `MissingFormatArgumentException` quotes it in full, flags, width and
        // precision included (`'%-5.2f'`, `'%,d'` — measured on JDK 21.0.12),
        // so the text has to be kept rather than reconstructed from the parsed
        // pieces.
        let mut spec = String::from("%");
        // The character right after the `%`, kept for the truncated-spec fault
        // below — see [`unknown_conversion`].
        let after = it.peek().copied();
        let (mut left, mut zero, mut plus, mut space) = (false, false, false, false);
        let mut group = false;
        while let Some(f) = it.peek() {
            match f {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                ' ' => space = true,
                ',' => group = true,
                _ => break,
            }
            spec.push(*f);
            it.next();
        }
        let mut width_txt = String::new();
        while it.peek().is_some_and(|d| d.is_ascii_digit()) {
            let d = it.next().expect("peeked");
            spec.push(d);
            width_txt.push(d);
        }
        // Parsed as the `i32` Java parses it as, so a spelling that overflows
        // faults instead of silently becoming no width at all.
        let width: usize = match width_txt.parse::<i32>() {
            Ok(w) => w as usize,
            Err(_) if width_txt.is_empty() => 0,
            Err(_) => return Err(illegal_span("Width")),
        };
        let mut prec: Option<usize> = None;
        if it.peek() == Some(&'.') {
            it.next();
            spec.push('.');
            let mut p = String::new();
            while it.peek().is_some_and(|d| d.is_ascii_digit()) {
                let d = it.next().expect("peeked");
                spec.push(d);
                p.push(d);
            }
            prec = Some(match p.parse::<i32>() {
                Ok(v) => v as usize,
                Err(_) if p.is_empty() => 0,
                Err(_) => return Err(illegal_span("Precision")),
            });
        }
        // A spec that runs off the end of the format string. The JVM names the
        // character right after the `%` — not the last one it read — and names
        // `%` itself when nothing follows at all.
        let conv = it
            .next()
            .ok_or_else(|| unknown_conversion(after.unwrap_or('%')))?;
        spec.push(conv);
        // `%%` and `%n` take no argument, but they DO take the width and the
        // `-` flag: `%5%` is four spaces then `%`. Falling through to the
        // padding below is what applies them.
        let arg = if matches!(conv, '%' | 'n') {
            Value::Undef
        } else {
            // AN EXHAUSTED ARGUMENT LIST IS A FAULT, not a hole. It used to
            // become `Undef`, which `to_int()` reads as `0`, so
            // `"%d %d".format(1)` answered `1 0` — a wrong answer with no
            // diagnostic anywhere. The JVM raises here.
            let Some(a) = args.get(argi).cloned() else {
                return Err(format!(
                    "java.util.MissingFormatArgumentException: Format specifier '{spec}'"
                ));
            };
            argi += 1;
            a
        };
        // A CONVERSION IS TYPED. `%d` with a `String` used to coerce through
        // `to_int()` and answer `0`; the JVM refuses the pair outright.
        if !conversion_accepts(conv, &arg) {
            return Err(format!(
                "java.util.IllegalFormatConversionException: {conv} != {}",
                format_arg_class(&arg)
            ));
        }
        // A null argument renders as the four characters `null` under every
        // conversion but `%b`, which answers `false` for it. The width and the
        // `-` flag still apply, so `%5d` of a null is ` null`.
        if matches!(arg, Value::Undef) && !matches!(conv, '%' | 'n' | 'b') {
            out.push_str(&pad("null".to_string(), width, left, false, conv));
            continue;
        }
        let mut body = match conv {
            '%' => "%".to_string(),
            // `%n` is the platform line separator, which is `\n` everywhere
            // kotlinrs builds.
            'n' => "\n".to_string(),
            'd' => format!("{}", arg.to_int()),
            'x' => format!("{:x}", arg.to_int()),
            'X' => format!("{:X}", arg.to_int()),
            'o' => format!("{:o}", arg.to_int()),
            'f' => format_fixed(arg.to_float(), prec.unwrap_or(6)),
            'e' | 'E' => {
                let s = format!("{:.*e}", prec.unwrap_or(6), arg.to_float());
                // Rust writes `1.5e2`; the JVM writes `1.500000e+02`.
                let s = match s.split_once('e') {
                    Some((m, x)) => {
                        let (sign, digits) = match x.strip_prefix('-') {
                            Some(d) => ('-', d),
                            None => ('+', x),
                        };
                        format!("{m}e{sign}{digits:0>2}")
                    }
                    None => s,
                };
                if conv == 'E' {
                    s.to_uppercase()
                } else {
                    s
                }
            }
            'c' => char_string(num_of(&arg)),
            // `%b` is a NULL TEST, not a truth test: `false` for null and for a
            // `Boolean false`, `true` for every other value including `0`, `""`
            // and an empty list. `truthy` answered Kotlin's notion of truth
            // instead, so `"%b".format("x")` and `"%b".format(1)` both read
            // `false` where the JVM reads `true` (measured on JDK 21.0.12).
            'b' => match &arg {
                Value::Undef => "false".to_string(),
                Value::Bool(b) => b.to_string(),
                _ => "true".to_string(),
            },
            's' | 'S' => {
                let s = kotlin_string(&arg);
                let s = match prec {
                    Some(p) => s.chars().take(p).collect(),
                    None => s,
                };
                if conv == 'S' {
                    s.to_uppercase()
                } else {
                    s
                }
            }
            other => return Err(unknown_conversion(other)),
        };
        // `,` groups the INTEGER part in threes, and the JVM accepts it on the
        // two decimal conversions only: `%,e`, `%,x`, `%,o`, `%,s` and `%,b`
        // are each a `FormatFlagsConversionMismatchException` rather than a
        // silently-ignored flag. Grouping runs before the sign and the width,
        // so `%,+d` reads `+1,234,567` and `%,015d` pads with UNGROUPED zeros
        // out to the full width.
        if group {
            match conv {
                'd' | 'f' => body = group_thousands(&body),
                other => {
                    return Err(format!(
                        "java.util.FormatFlagsConversionMismatchException: \
                         Conversion = {other}, Flags = ,"
                    ))
                }
            }
        }
        // The sign flags apply to the numeric conversions only, and `+` wins
        // over ` ` when both are given (as in the JVM).
        if matches!(conv, 'd' | 'f' | 'e' | 'E') && !body.starts_with('-') {
            if plus {
                body.insert(0, '+');
            } else if space {
                body.insert(0, ' ');
            }
        }
        out.push_str(&pad(body, width, left, zero, conv));
    }
    Ok(out)
}

/// Widen `body` to `width`, the way `java.util.Formatter` does: spaces on the
/// right under `-`, zeros after any sign under `0` on the numeric conversions,
/// spaces on the left otherwise. Shared with the null-argument path, which pads
/// its literal `null` by the same rules.
fn pad(mut body: String, width: usize, left: bool, zero: bool, conv: char) -> String {
    let n = width.saturating_sub(body.chars().count());
    if n > 0 {
        if left {
            body.push_str(&" ".repeat(n));
        } else if zero && matches!(conv, 'd' | 'f' | 'e' | 'E' | 'x' | 'X' | 'o') {
            // Zero padding goes after any sign, not before it.
            let at = usize::from(body.starts_with(['-', '+', ' ']));
            body.insert_str(at, &"0".repeat(n));
        } else {
            body.insert_str(0, &" ".repeat(n));
        }
    }
    body
}

/// UTF-16 offset of `needle` in `hay`, or -1 — matching `String.indexOf` and
/// the UTF-16 basis `length` already uses.
/// The tail of `s` starting at UTF-16 offset `at`, or `None` when `at` is past
/// the end. The `String` search members index in UTF-16 units, matching
/// `length`.
fn utf16_slice_from(s: &str, at: i64) -> Option<String> {
    let units: Vec<u16> = s.encode_utf16().collect();
    if at < 0 || at > units.len() as i64 {
        return None;
    }
    Some(utf16_to_string(&units[at as usize..]))
}

/// Whether the optional flag argument at `i` was supplied and true — the
/// `ignoreCase` parameter the `String` comparisons take.
fn truthy_arg(args: &[Value], i: usize) -> bool {
    args.get(i).is_some_and(truthy)
}

fn utf16_index_of(hay: &str, needle: &str) -> i64 {
    match hay.find(needle) {
        Some(byte_off) => hay[..byte_off].encode_utf16().count() as i64,
        None => -1,
    }
}

/// The receiver width (32 or 64) the compiler appended at index `at` to a
/// width-sensitive bitwise member. An absent or unexpected value falls back to
/// 32 — Kotlin's `Int`, the width every such call had before the argument
/// existed.
fn trailing_width(args: &[Value], at: usize) -> i64 {
    match args.get(at).map(|v| v.to_int()) {
        Some(64) => 64,
        _ => 32,
    }
}

// ── java.lang.StringBuilder ─────────────────────────────────────────────────

/// The spare room every `StringBuilder` constructor leaves: `StringBuilder()`
/// starts at 16 and `StringBuilder(text)` at `text.length + 16`.
const BUILDER_SLACK: usize = 16;

/// The capacity that holds `needed` units, grown from `cap` the way
/// `AbstractStringBuilder` grows it — double it plus two, or jump straight to
/// what is needed when even that is too small.
fn builder_cap(cap: usize, needed: usize) -> usize {
    if needed <= cap {
        cap
    } else {
        (cap * 2 + 2).max(needed)
    }
}

/// Build the `Op::Extended(KT_BUILDER, argc)` result: `StringBuilder()`,
/// `StringBuilder(text)`, or `StringBuilder(capacity)`.
fn new_builder(arg: Option<Value>) -> Value {
    let (units, cap) = match arg {
        // The `(int)` overload: an EMPTY builder that has merely preallocated.
        Some(Value::Int(n)) => (Vec::new(), n.max(0) as usize),
        Some(v) => {
            let units: Vec<u16> = kotlin_string(&v).encode_utf16().collect();
            let cap = units.len() + BUILDER_SLACK;
            (units, cap)
        }
        None => (Vec::new(), BUILDER_SLACK),
    };
    alloc(HeapObj::Builder { units, cap })
}

/// A builder's content, or `None` for any other receiver.
fn builder_units(v: &Value) -> Option<Vec<u16>> {
    with_obj(v, |o| match o {
        HeapObj::Builder { units, .. } => Some(units.clone()),
        _ => None,
    })
    .flatten()
}

/// Rewrite a builder's content in place, growing its capacity to fit the
/// result. Answers `f`'s value, or `None` when `v` is not a builder.
fn edit_builder<T>(v: &Value, f: impl FnOnce(&mut Vec<u16>) -> T) -> Option<T> {
    with_obj_mut(v, |o| match o {
        HeapObj::Builder { units, cap } => {
            let out = f(units);
            *cap = builder_cap(*cap, units.len());
            Some(out)
        }
        _ => None,
    })
    .flatten()
}

/// `StringBuilder` members.
///
/// The mutating ones are here because they have no `String` counterpart, and
/// every one of them answers the RECEIVER so `sb.append(a).append(b)` keeps
/// building the same object. Everything else is inherited from `CharSequence`
/// and behaves identically on a `String`, so it delegates rather than being
/// written twice — `length`, `indexOf`, `substring`, `startsWith`, `first`,
/// `toList`, `count`, and the rest all resolve through [`kt_method`] on a
/// snapshot of the content.
/// `StringIndexOutOfBoundsException` for a SINGLE index — `s[i]`, `s.get(i)`,
/// `sb.deleteCharAt(i)`, `sb.setCharAt(i, c)`.
///
/// The wording is a property of the JDK the program runs on, not of Kotlin:
/// `String` and `AbstractStringBuilder` route their bounds checks through
/// `Preconditions.outOfBounds` from JDK 21 on, and printed a per-call-site
/// message before that. JDK 17 answers `index 9, length 3` here where JDK 21
/// and JDK 26 both answer `Index 9 out of bounds for length 3`, verified on
/// this machine's Corretto 17.0.4.1, Homebrew 21.0.12 and Homebrew 26.0.2.
///
/// kotlinrs targets the CURRENT wording, which is also the only choice that is
/// self-consistent: `Double.toString` here is the shortest-representation
/// algorithm JDK 19 introduced, so a kotlinrs that also spoke JDK 17's index
/// messages would be imitating a JVM that never existed.
fn sioobe_index(i: i64, len: usize) -> String {
    format!("java.lang.StringIndexOutOfBoundsException: Index {i} out of bounds for length {len}")
}

/// `StringIndexOutOfBoundsException` for a half-open RANGE — `substring`,
/// `subSequence`, `sb.insert`, `sb.delete`, `sb.replace`. See [`sioobe_index`]
/// for which JDK's wording this is and why.
fn sioobe_range(from: i64, to: i64, len: usize) -> String {
    format!(
        "java.lang.StringIndexOutOfBoundsException: \
         Range [{from}, {to}) out of bounds for length {len}"
    )
}

fn builder_method(
    vm: &mut VM,
    recv: &Value,
    units: Vec<u16>,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    let len = units.len();
    /// The JVM's SINGLE-INDEX diagnostic — `charAt`, `deleteCharAt`,
    /// `setCharAt`. See [`sioobe_index`] for why the wording is the one it is.
    fn oob(i: i64, len: usize) -> String {
        sioobe_index(i, len)
    }
    /// The JVM's RANGE diagnostic — `insert`, `delete`, `replace`, `substring`.
    /// The two forms are separate messages, not one message with a different
    /// noun: a range names both bounds, a single index names one.
    fn oob_range(from: i64, to: i64, len: usize) -> String {
        sioobe_range(from, to, len)
    }
    /// A member argument as the code units `String.valueOf` would append.
    ///
    /// Rendered up front, NOT inside the mutation closure below: an argument
    /// that is itself a heap object (`sb.append(listOf(1, 2))`) reads the heap
    /// to stringify, and the mutation already holds it borrowed.
    fn arg_units(args: &[Value], i: usize) -> Vec<u16> {
        args.get(i)
            .map(kotlin_string)
            .unwrap_or_default()
            .encode_utf16()
            .collect()
    }
    let int_arg = |i: usize| args.get(i).map(|v| v.to_int()).unwrap_or(0);
    // A member that mutates answers the receiver, so its arm only has to say
    // what changed.
    let chained = |f: &mut dyn FnMut(&mut Vec<u16>)| {
        edit_builder(recv, |u| f(u));
        Ok(recv.clone())
    };
    match name {
        "toString" => Ok(Value::str(utf16_to_string(&units))),
        // `StringBuilder` does not override `equals`/`hashCode`, so both are
        // `Object`'s — identity, NOT the content two equal-looking builders
        // share. Delegating them to the `String` snapshot would quietly make
        // `StringBuilder("ab") == StringBuilder("ab")` true.
        "equals" => Ok(Value::Bool(args.first().is_some_and(|o| value_eq(recv, o)))),
        "hashCode" => Ok(Value::Int(hash_vm(vm, recv, false) as i64)),
        "append" | "appendLine" => {
            let mut add = arg_units(args, 0);
            if name == "appendLine" {
                add.push(b'\n' as u16);
            }
            chained(&mut |u| u.extend_from_slice(&add))
        }
        "insert" => {
            let at = int_arg(0);
            if at < 0 || at as usize > len {
                return Err(oob_range(at, len as i64, len));
            }
            let ins = arg_units(args, 1);
            chained(&mut |u| {
                u.splice(at as usize..at as usize, ins.clone());
            })
        }
        // `appendRange(csq, start, end)` / `insertRange(index, csq, start, end)`
        // take a HALF-OPEN slice of the argument rather than all of it. The
        // bounds are the argument's, not the receiver's, so they are checked
        // against its length — a distinction the shared `oob` above cannot make
        // for us.
        "appendRange" | "insertRange" => {
            let inserting = name == "insertRange";
            let at = if inserting { int_arg(0) } else { len as i64 };
            if at < 0 || at as usize > len {
                return Err(oob_range(at, len as i64, len));
            }
            let src = arg_units(args, usize::from(inserting));
            let (from, to) = (
                int_arg(1 + usize::from(inserting)),
                int_arg(2 + usize::from(inserting)),
            );
            // The ARGUMENT's bounds fault as a plain `IndexOutOfBoundsException`
            // — `Preconditions.checkFromToIndex` on the source `CharSequence`,
            // not the `StringIndex…` subclass the receiver's own bounds raise.
            if from < 0 || to > src.len() as i64 || from > to {
                return Err(format!(
                    "java.lang.IndexOutOfBoundsException: \
                     Range [{from}, {to}) out of bounds for length {}",
                    src.len()
                ));
            }
            let slice = src[from as usize..to as usize].to_vec();
            chained(&mut |u| {
                u.splice(at as usize..at as usize, slice.clone());
            })
        }
        // `deleteAt` is `kotlin.text`'s spelling of `deleteCharAt`, and
        // `deleteRange` of `delete` — both are `StringBuilder` extensions that
        // forward to the JVM member, so they fault identically.
        "deleteCharAt" | "deleteAt" => {
            let at = int_arg(0);
            if at < 0 || at as usize >= len {
                return Err(oob(at, len));
            }
            chained(&mut |u| {
                u.remove(at as usize);
            })
        }
        // `delete`/`replace` CLAMP their end to the length rather than
        // throwing — `StringBuilder("abc").delete(1, 99)` is `a` — but still
        // reject a start outside the sequence.
        "delete" | "deleteRange" | "replace" => {
            // The JVM clamps `end` to the length BEFORE checking, so the bound
            // the diagnostic reports is the clamped one: `delete(9, 10)` on a
            // 5-unit builder says `Range [9, 5)`, not `Range [9, 10)`.
            let (start, end) = (int_arg(0), int_arg(1).min(len as i64));
            if start < 0 || start > end {
                return Err(oob_range(start, end, len));
            }
            let (start, end) = (start as usize, end as usize);
            let with = if name == "replace" {
                arg_units(args, 2)
            } else {
                Vec::new()
            };
            chained(&mut |u| {
                u.splice(start..end, with.clone());
            })
        }
        // The JVM reverses code units but keeps each surrogate PAIR facing
        // forward, so `"a😀b"` reverses to `"b😀a"` and not to two broken
        // halves. Reversing twice and swapping the halves back is exactly what
        // `AbstractStringBuilder.reverse` does.
        "reverse" => chained(&mut |u| {
            u.reverse();
            let mut i = 0;
            while i + 1 < u.len() {
                // Reversing put each pair back to front, as `[low, high]`.
                if (0xDC00..0xE000).contains(&u[i]) && (0xD800..0xDC00).contains(&u[i + 1]) {
                    u.swap(i, i + 1);
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }),
        // `clear()` is `kotlin.text`'s builder-returning spelling of
        // `setLength(0)`; `setLength` itself answers `Unit` and PADS with NUL
        // when it grows.
        "clear" => chained(&mut |u| u.clear()),
        "setLength" => {
            let n = int_arg(0);
            // The one bounds check `AbstractStringBuilder` did NOT move onto
            // `Preconditions` in JDK 21: `setLength` still raises its own
            // hand-written message, identical on JDK 17 and JDK 26.
            if n < 0 {
                return Err(format!(
                    "java.lang.StringIndexOutOfBoundsException: String index out of range: {n}"
                ));
            }
            edit_builder(recv, |u| u.resize(n as usize, 0));
            Ok(Value::Undef)
        }
        "setCharAt" => {
            let at = int_arg(0);
            if at < 0 || at as usize >= len {
                return Err(oob(at, len));
            }
            let c = args
                .get(1)
                .and_then(char_code)
                .unwrap_or_else(|| int_arg(1)) as u16;
            edit_builder(recv, |u| u[at as usize] = c);
            Ok(Value::Undef)
        }
        "capacity" => Ok(Value::Int(
            with_obj(recv, |o| match o {
                HeapObj::Builder { cap, .. } => *cap as i64,
                _ => 0,
            })
            .unwrap_or(0),
        )),
        "ensureCapacity" | "trimToSize" => {
            let want = if name == "trimToSize" {
                len
            } else {
                int_arg(0).max(0) as usize
            };
            with_obj_mut(recv, |o| {
                if let HeapObj::Builder { cap, .. } = o {
                    *cap = if name == "trimToSize" {
                        len
                    } else {
                        builder_cap(*cap, want)
                    };
                }
            });
            Ok(Value::Undef)
        }
        _ => kt_method(vm, &Value::str(utf16_to_string(&units)), name, args)
            .map_err(|e| e.replace("on String", "on StringBuilder")),
    }
}

fn kt_method(vm: &mut VM, recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    // A `Char` shares the `Value::Obj` variant with the heap but is not a heap
    // object, so its members resolve first.
    if let Some(code) = char_code(recv) {
        return char_method(code, name, args);
    }
    // Heap objects (List/Map/Pair/data-class members) dispatch through the heap.
    if let Value::Obj(_) = recv {
        let from_sequence = is_sequence_view(recv);
        let out = obj_method(vm, recv, name, args);
        // A DERIVED sequence is still a sequence. `Sequence.map`/`filter`/
        // `take`/`drop`/… are declared to answer a `Sequence`, so
        // `xs.asSequence().drop(1).first()` must still say `Sequence is empty.`
        // A materialized one is carried as a `List`, and without this the tag
        // stopped at the first derivation. The members that answer a
        // COLLECTION instead (`toList`, `toSet`, `toMutableList`, …) are
        // exactly the ones named here, so the rule is stated once.
        if from_sequence && sequence_preserving(name) {
            if let Ok(v) = &out {
                tag_sequence(v.clone());
            }
        }
        // A mutating member (`add`, `put`, `remove`, …) may have appended to a
        // collection that does not iterate in insertion order. Restoring the
        // discipline here rather than at each mutation keeps the ~10 mutating
        // members from having to remember to; it is a map lookup and no more
        // for the insertion-ordered collections, which are the common case.
        reorder(vm, recv);
        return out;
    }
    match (recv, name) {
        // ── kotlin.String ──
        (Value::Str(s), "length") => Ok(Value::Int(s.encode_utf16().count() as i64)),
        (Value::Str(s), "uppercase" | "toUpperCase") => Ok(Value::str(s.to_uppercase())),
        (Value::Str(s), "lowercase" | "toLowerCase") => Ok(Value::str(s.to_lowercase())),
        (Value::Str(s), "trim") => Ok(Value::str(kotlin_trim(s, true, true).to_string())),
        (Value::Str(s), "isEmpty") => Ok(Value::Bool(s.is_empty())),
        (Value::Str(s), "isNotEmpty") => Ok(Value::Bool(!s.is_empty())),
        (Value::Str(s), "isBlank") => Ok(Value::Bool(kotlin_trim(s, true, true).is_empty())),
        (Value::Str(s), "isNotBlank") => Ok(Value::Bool(!kotlin_trim(s, true, true).is_empty())),

        // Arg-taking `String` members. The argument is rendered through
        // `kotlin_string` so a `Char` (carried as an integer code unit) or a
        // number reads the way Kotlin would print it.
        (Value::Str(s), "contains") => Ok(Value::Bool(s.contains(&arg_str(args, 0)))),
        // `startsWith(prefix, startIndex)` tests at an OFFSET, not from 0 —
        // `"abc".startsWith("b", 1)` is true. `endsWith` has no such overload.
        (Value::Str(s), "startsWith") => {
            let at = args.get(1).map(|v| v.to_int()).unwrap_or(0);
            Ok(Value::Bool(match utf16_slice_from(s, at) {
                Some(tail) => tail.starts_with(&arg_str(args, 0)),
                None => false,
            }))
        }
        (Value::Str(s), "endsWith") => Ok(Value::Bool(s.ends_with(&arg_str(args, 0)))),
        (Value::Str(s), "plus") => Ok(Value::str(format!("{s}{}", arg_str(args, 0)))),
        (Value::Str(s), "replace") => {
            Ok(Value::str(s.replace(&arg_str(args, 0), &arg_str(args, 1))))
        }
        (Value::Str(s), "replaceFirst") => Ok(Value::str(s.replacen(
            &arg_str(args, 0),
            &arg_str(args, 1),
            1,
        ))),
        (Value::Str(s), "repeat") => {
            let n = args.first().map(|v| v.to_int()).unwrap_or(0);
            if n < 0 {
                Err(format!(
                    "java.lang.IllegalArgumentException: Count 'n' must be non-negative, but was {n}."
                ))
            } else {
                Ok(Value::str(s.repeat(n as usize)))
            }
        }
        // Index and slice positions are UTF-16 offsets, matching `length` above.
        // `indexOf(needle, startIndex)` searches FROM an offset and still
        // answers an absolute index, so a match before `startIndex` is not a
        // match at all. `startIndex` is CLAMPED to `0..length` rather than
        // rejected, which is observable for an empty needle:
        // `"abc".indexOf("", 9)` is 3, not -1.
        (Value::Str(s), "indexOf") => {
            let len = s.encode_utf16().count() as i64;
            let at = args.get(1).map(|v| v.to_int()).unwrap_or(0).clamp(0, len);
            Ok(Value::Int(match utf16_slice_from(s, at) {
                Some(tail) => match utf16_index_of(&tail, &arg_str(args, 0)) {
                    -1 => -1,
                    found => found + at,
                },
                None => -1,
            }))
        }
        // `subSequence` is the `CharSequence` spelling of the two-argument
        // `substring`. It is typed `CharSequence` rather than `String`, but the
        // JVM answers a `String` for both receivers kotlinrs has.
        (Value::Str(s), "substring" | "subSequence") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let start = args.first().map(|v| v.to_int()).unwrap_or(0);
            let end = args
                .get(1)
                .map(|v| v.to_int())
                .unwrap_or(units.len() as i64);
            if start < 0 || end > units.len() as i64 || start > end {
                Err(sioobe_range(start, end, units.len()))
            } else {
                Ok(Value::str(utf16_to_string(
                    &units[start as usize..end as usize],
                )))
            }
        }
        // `lastIndexOf(needle, startIndex)` answers the LAST position at which
        // the needle STARTS at or before `startIndex` — the match itself may run
        // past it. Kotlin's default `startIndex` is `lastIndex`, NOT the Java
        // `String.lastIndexOf`'s `length`, which is observable only for an empty
        // needle: `"abc".lastIndexOf("")` is 2 on Kotlin and 3 on Java.
        (Value::Str(s), "lastIndexOf") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let needle: Vec<u16> = arg_str(args, 0).encode_utf16().collect();
            let last_start = units.len() as i64 - needle.len() as i64;
            let mut at = args
                .get(1)
                .map(|v| v.to_int())
                .unwrap_or(units.len() as i64 - 1)
                .min(last_start);
            while at >= 0 {
                if units[at as usize..at as usize + needle.len()] == needle[..] {
                    return Ok(Value::Int(at));
                }
                at -= 1;
            }
            Ok(Value::Int(-1))
        }
        (Value::Str(s), "trimStart") => Ok(Value::str(kotlin_trim(s, true, false).to_string())),
        (Value::Str(s), "trimEnd") => Ok(Value::str(kotlin_trim(s, false, true).to_string())),
        (Value::Str(s), "removePrefix") => {
            let p = arg_str(args, 0);
            Ok(Value::str(s.strip_prefix(&p).unwrap_or(s).to_string()))
        }
        (Value::Str(s), "removeSuffix") => {
            let p = arg_str(args, 0);
            Ok(Value::str(s.strip_suffix(&p).unwrap_or(s).to_string()))
        }
        // `substringBefore`/`substringAfter` yield the whole receiver when the
        // delimiter is absent — Kotlin's default `missingDelimiterValue`.
        (Value::Str(s), "substringBefore") => {
            let d = arg_str(args, 0);
            Ok(Value::str(match s.split_once(&d) {
                Some((head, _)) => head.to_string(),
                None => s.to_string(),
            }))
        }
        (Value::Str(s), "substringAfter") => {
            let d = arg_str(args, 0);
            Ok(Value::str(match s.split_once(&d) {
                Some((_, tail)) => tail.to_string(),
                None => s.to_string(),
            }))
        }
        // `String.reversed()` reverses whole characters, not code units — the
        // JVM's `StringBuilder.reverse` keeps surrogate pairs intact.
        (Value::Str(s), "reversed") => Ok(Value::str(s.chars().rev().collect::<String>())),
        // `split(vararg delimiters)` on literal delimiters (no regex overload
        // here). An empty delimiter splits between every character AND at both
        // ends, which is what both Kotlin and Rust's `str::split("")` produce.
        // With several delimiters, the earliest match in the string wins at
        // each position — Kotlin scans left to right, not delimiter by
        // delimiter, so `"a1b2c".split("1", "2")` is `[a, b, c]`.
        // `splitToSequence` is `split` returning a lazy `Sequence` rather than
        // a `List`. The result is FINITE either way — the receiver bounds it —
        // so it materializes like every other finite sequence here; only
        // `generateSequence` needs the lazy representation.
        (Value::Str(s), "split" | "splitToSequence") => {
            let delims: Vec<String> = args.iter().map(kotlin_string).collect();
            let mut parts: Vec<Value> = Vec::new();
            if delims.len() <= 1 {
                let d = delims.first().cloned().unwrap_or_default();
                parts.extend(s.split(&d as &str).map(|p| Value::str(p.to_string())));
            } else {
                let mut rest = s.as_str();
                'scan: loop {
                    let hit = delims
                        .iter()
                        .filter(|d| !d.is_empty())
                        .filter_map(|d| rest.find(d.as_str()).map(|at| (at, d.len())))
                        .min();
                    match hit {
                        Some((at, len)) => {
                            parts.push(Value::str(rest[..at].to_string()));
                            rest = &rest[at + len..];
                        }
                        None => {
                            parts.push(Value::str(rest.to_string()));
                            break 'scan;
                        }
                    }
                }
            }
            Ok(alloc(HeapObj::List(parts)))
        }
        (Value::Str(s), "lines") => Ok(alloc(HeapObj::List(
            s.split('\n')
                .map(|l| Value::str(l.strip_suffix('\r').unwrap_or(l).to_string()))
                .collect(),
        ))),
        (Value::Str(s), "toCharArray") => {
            let items: Vec<Value> = s.encode_utf16().map(|u| char_of(u as i64)).collect();
            Ok(alloc(HeapObj::Array {
                items,
                desc: "[C".to_string(),
            }))
        }
        // `first()`/`last()` are `Char`; both throw on an empty receiver.
        (Value::Str(s), "first" | "last") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let pick = if name == "first" {
                units.first()
            } else {
                units.last()
            };
            match pick {
                Some(u) => Ok(char_of(*u as i64)),
                None => {
                    Err("java.util.NoSuchElementException: Char sequence is empty.".to_string())
                }
            }
        }
        (Value::Str(s), "get") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            match usize::try_from(i).ok().and_then(|i| units.get(i)) {
                Some(u) => Ok(char_of(*u as i64)),
                None => Err(sioobe_index(i, units.len())),
            }
        }
        // `take`/`drop` clamp an oversized count and fault on a negative one,
        // matching the `List` overloads.
        (Value::Str(s), "take" | "drop") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let n = args.first().map(|v| v.to_int()).unwrap_or(0);
            if n < 0 {
                return Err(format!(
                    "java.lang.IllegalArgumentException: Requested character count {n} is less than zero."
                ));
            }
            let n = (n as usize).min(units.len());
            let cut = if name == "take" {
                &units[..n]
            } else {
                &units[n..]
            };
            Ok(Value::str(utf16_to_string(cut)))
        }
        // The `kotlin.text` from-the-end pair. It answers a `String`, not the
        // `List<Char>` the shared sequence path would build, so it resolves here
        // rather than through the character snapshot.
        (Value::Str(s), "takeLast" | "dropLast") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let n = args.first().map(|v| v.to_int()).unwrap_or(0);
            if n < 0 {
                return Err(format!(
                    "java.lang.IllegalArgumentException: Requested character count {n} is less than zero."
                ));
            }
            let at = units.len() - (n as usize).min(units.len());
            let cut = if name == "takeLast" {
                &units[at..]
            } else {
                &units[..at]
            };
            Ok(Value::str(utf16_to_string(cut)))
        }
        // `padStart`/`padEnd` pad to a UTF-16 length with a `Char` (default
        // space); a receiver already that long is returned unchanged.
        (Value::Str(s), "padStart" | "padEnd") => {
            let want = args.first().map(|v| v.to_int()).unwrap_or(0);
            if want < 0 {
                return Err(format!(
                    "java.lang.IllegalArgumentException: Desired length {want} is less than zero."
                ));
            }
            let have = s.encode_utf16().count() as i64;
            let fill = match args.get(1).and_then(char_code) {
                Some(c) => char::from_u32(c as u32).unwrap_or(' '),
                None => ' ',
            };
            let pad = fill.to_string().repeat((want - have).max(0) as usize);
            Ok(Value::str(if name == "padStart" {
                format!("{pad}{s}")
            } else {
                format!("{s}{pad}")
            }))
        }
        // `String.compareTo` is the JVM's: the code-unit difference at the
        // first mismatch, else the length difference — NOT clamped to -1/0/1.
        // `equals(other, ignoreCase)` is the only `String` `equals` that is not
        // plain structural equality, so it needs its own arm ahead of the
        // universal one.
        (Value::Str(s), "equals") => {
            let other = arg_str(args, 0);
            Ok(Value::Bool(if truthy_arg(args, 1) {
                s.to_lowercase() == other.to_lowercase()
            } else {
                args.first().is_some_and(|o| value_eq(recv, o))
            }))
        }
        (Value::Str(s), "compareTo") => {
            let other = arg_str(args, 0);
            // `compareTo(other, ignoreCase = true)` compares case-folded, which
            // makes `"a".compareTo("A", true)` 0 rather than 32.
            let (s, other) = if truthy_arg(args, 1) {
                (s.to_lowercase(), other.to_lowercase())
            } else {
                (s.to_string(), other)
            };
            let s = &s;
            let (a, b): (Vec<u16>, Vec<u16>) =
                (s.encode_utf16().collect(), other.encode_utf16().collect());
            let d = a
                .iter()
                .zip(b.iter())
                .find(|(x, y)| x != y)
                .map(|(x, y)| *x as i64 - *y as i64)
                .unwrap_or(a.len() as i64 - b.len() as i64);
            Ok(Value::Int(d))
        }
        // `String.format(args…)` — the receiver is the format string.
        (Value::Str(s), "format") => format_string(s, args).map(Value::str),
        // Numeric parses. The `…OrNull` forms answer null where the plain ones
        // throw, which is the only difference between the pairs.
        // Both take an optional RADIX (`"ff".toInt(16)`). Dropping it would not
        // fail loudly — it would answer the base-10 reading, or throw on a
        // string that is perfectly valid in the base that was asked for.
        // The destination WIDTH is part of the parse, not a cast after it:
        // `"300".toByte()` is a fault, not `44`. Each width also reports its
        // own diagnostic — see [`parse_radix`].
        (Value::Str(s), "toInt" | "toLong" | "toByte" | "toShort") => {
            parse_radix(s, args, int_width_of(name)).map_err(|e| match e {
                ParseFail::Radix(m) | ParseFail::Number(m) => m,
            })
        }
        (Value::Str(s), "toIntOrNull" | "toLongOrNull" | "toByteOrNull" | "toShortOrNull") => {
            match parse_radix(s, args, int_width_of(name.trim_end_matches("OrNull"))) {
                Ok(v) => Ok(v),
                Err(ParseFail::Number(_)) => Ok(Value::Undef),
                Err(ParseFail::Radix(m)) => Err(m),
            }
        }
        // `FloatingDecimal.readJavaFormatString` trims first and reports a
        // nothing-left input with its OWN message, not the `For input string`
        // one every other numeric parse uses. `Integer.parseInt` does not do
        // this: `"".toInt()` still says `For input string: ""`.
        (Value::Str(s), "toDouble" | "toFloat") => {
            java_parse_double(s).map(Value::Float).ok_or_else(|| {
                if s.trim_matches(|c: char| c <= ' ').is_empty() {
                    "java.lang.NumberFormatException: empty String".to_string()
                } else {
                    format!("java.lang.NumberFormatException: For input string: \"{s}\"")
                }
            })
        }
        (Value::Str(s), "toDoubleOrNull" | "toFloatOrNull") => Ok(java_parse_double(s)
            .map(Value::Float)
            .unwrap_or(Value::Undef)),

        // `Int.toChar()` → the `Char` for the low 16 bits of the receiver.
        (Value::Int(n), "toChar") => Ok(char_of(*n)),
        // The bitwise member functions, which the parser reaches through their
        // infix spelling (`a and b`, `x shl 4`). `and`/`or`/`xor` cannot widen a
        // value, so they operate on the `i64` directly.
        (Value::Int(a), "and") => Ok(Value::Int(
            a & args.first().map(|v| v.to_int()).unwrap_or(0),
        )),
        (Value::Int(a), "or") => Ok(Value::Int(
            a | args.first().map(|v| v.to_int()).unwrap_or(0),
        )),
        (Value::Int(a), "xor") => Ok(Value::Int(
            a ^ args.first().map(|v| v.to_int()).unwrap_or(0),
        )),
        // `inv` and the shifts DO depend on the receiver's width, which every
        // integer being one `i64` hides at runtime — so the compiler pushes it
        // as the last argument (32 for an `Int` receiver, 64 for a `Long`).
        // Kotlin masks the shift count at `width - 1` and truncates the result
        // to `width`: `1 shl 32` is 1, `1L shl 32` is 4294967296.
        (Value::Int(a), "inv") => Ok(Value::Int(if trailing_width(args, 0) == 64 {
            !*a
        } else {
            !(*a as i32) as i64
        })),
        (Value::Int(a), "shl" | "shr" | "ushr") => {
            let width = trailing_width(args, 1);
            let count = args.first().map(|v| v.to_int()).unwrap_or(0);
            let bits = (count & (width - 1)) as u32;
            Ok(Value::Int(if width == 64 {
                match name {
                    "shl" => a.wrapping_shl(bits),
                    "shr" => a.wrapping_shr(bits),
                    _ => (*a as u64).wrapping_shr(bits) as i64,
                }
            } else {
                let a = *a as i32;
                match name {
                    "shl" => a.wrapping_shl(bits) as i64,
                    "shr" => a.wrapping_shr(bits) as i64,
                    _ => ((a as u32).wrapping_shr(bits)) as i32 as i64,
                }
            }))
        }
        // `coerceIn`/`coerceAtLeast`/`coerceAtMost` clamp to a bound. The result
        // stays integral only when receiver and bounds all are.
        (Value::Int(_) | Value::Float(_), "coerceIn" | "coerceAtLeast" | "coerceAtMost") => {
            let ints = is_int(recv) && args.iter().all(is_int);
            let lo = match name {
                "coerceAtMost" => None,
                _ => args.first(),
            };
            let hi = match name {
                "coerceIn" => args.get(1),
                "coerceAtMost" => args.first(),
                _ => None,
            };
            // A crossed pair is REFUSED, not silently reordered. `f64::max`
            // followed by `f64::min` clamped `5.coerceIn(3, 1)` to 1 instead.
            if let (Some(lo), Some(hi)) = (lo, hi) {
                let empty = if ints {
                    lo.to_int() > hi.to_int()
                } else {
                    lo.to_float() > hi.to_float()
                };
                if empty {
                    return Err(format!(
                        "java.lang.IllegalArgumentException: Cannot coerce value to an \
                         empty range: maximum {} is less than minimum {}.",
                        kotlin_string(hi),
                        kotlin_string(lo)
                    ));
                }
            }
            if ints {
                let mut v = recv.to_int();
                if let Some(lo) = lo {
                    v = v.max(lo.to_int());
                }
                if let Some(hi) = hi {
                    v = v.min(hi.to_int());
                }
                Ok(Value::Int(v))
            } else {
                // Kotlin clamps with `<` and `>`, and returns the RECEIVER when
                // neither fires. That is not `f64::max`/`min`: those ignore a
                // NaN receiver and answer a bound, where Kotlin's comparisons
                // are both false for NaN and hand the NaN straight back. The
                // same shape keeps `-0.0` out of `coerceIn(0.0, 1.0)`'s clamp,
                // since `-0.0 < 0.0` is false.
                let v = recv.to_float();
                if let Some(lo) = lo {
                    if v < lo.to_float() {
                        return Ok(Value::Float(lo.to_float()));
                    }
                }
                if let Some(hi) = hi {
                    if v > hi.to_float() {
                        return Ok(Value::Float(hi.to_float()));
                    }
                }
                Ok(Value::Float(v))
            }
        }
        // `kotlin.math` members in their receiver spelling: `2.0.pow(3.0)`,
        // `(-1.5).absoluteValue`, `2.6.roundToInt()`.
        (Value::Int(_) | Value::Float(_), "pow") => Ok(Value::Float(
            recv.to_float()
                .powf(args.first().map(|v| v.to_float()).unwrap_or(0.0)),
        )),
        // `Int.sign` / `Double.sign` — the `kotlin.math` property spelling of
        // the same function; see [`math_call`] for why neither is `f64::signum`.
        (Value::Int(_) | Value::Float(_), "sign") => math_call("sign", std::slice::from_ref(recv)),
        // `Int.mod(Int)` — the FLOOR remainder, whose sign follows the DIVISOR.
        // `(-7).mod(2)` is 1 where `-7 % 2` is -1.
        (Value::Int(_), "mod") => math_call(
            "floorMod",
            &[recv.clone(), args.first().cloned().unwrap_or(Value::Int(0))],
        ),
        (Value::Int(_) | Value::Float(_), "absoluteValue") => {
            if is_int(recv) {
                Ok(Value::Int(recv.to_int().wrapping_abs()))
            } else {
                Ok(Value::Float(recv.to_float().abs()))
            }
        }
        // `roundToInt`/`roundToLong` are `Math.round` (see [`java_math_round`])
        // behind two guards the JVM method does not have. `Math.round(NaN)` is
        // 0; Kotlin REFUSES it instead, and `roundToInt` additionally clamps to
        // the `Int` range rather than letting the `Long` result wrap.
        (Value::Int(_) | Value::Float(_), "roundToInt" | "roundToLong") => {
            let x = recv.to_float();
            if x.is_nan() {
                return Err(
                    "java.lang.IllegalArgumentException: Cannot round NaN value.".to_string(),
                );
            }
            if name == "roundToInt" {
                if x > i32::MAX as f64 {
                    return Ok(Value::Int(i32::MAX as i64));
                }
                if x < i32::MIN as f64 {
                    return Ok(Value::Int(i32::MIN as i64));
                }
            }
            Ok(Value::Int(java_math_round(x)))
        }

        // ── the arithmetic operators in their method spelling ──
        // `a.plus(b)` is what `a + b` compiles to on the JVM, and the method
        // form is how the operators are reached through a safe call
        // (`count?.plus(1)`). Integer `div`/`rem` truncate and take the
        // dividend's sign, exactly as the operator forms do.
        (Value::Int(_) | Value::Float(_), "plus" | "minus" | "times" | "div" | "rem") => {
            let b = args.first().cloned().unwrap_or(Value::Int(0));
            let int_op = matches!(recv, Value::Int(_)) && matches!(b, Value::Int(_));
            if int_op {
                let (x, y) = (recv.to_int(), b.to_int());
                match name {
                    "plus" => Ok(Value::Int(x.wrapping_add(y))),
                    "minus" => Ok(Value::Int(x.wrapping_sub(y))),
                    "times" => Ok(Value::Int(x.wrapping_mul(y))),
                    _ if y == 0 => Err("java.lang.ArithmeticException: / by zero".to_string()),
                    "div" => Ok(Value::Int(x.wrapping_div(y))),
                    _ => Ok(Value::Int(x.wrapping_rem(y))),
                }
            } else {
                let (x, y) = (recv.to_float(), b.to_float());
                Ok(Value::Float(match name {
                    "plus" => x + y,
                    "minus" => x - y,
                    "times" => x * y,
                    "div" => x / y,
                    _ => x % y,
                }))
            }
        }
        (Value::Int(n), "toDouble" | "toFloat") => Ok(Value::Float(*n as f64)),
        // Integer-to-integer conversions TRUNCATE to the target width (they are
        // the JVM's `i2b`/`i2s`/`l2i`), so `2147483648L.toInt()` is
        // `-2147483648` and `200.toByte()` is `-56`. `toLong` is the identity:
        // every integer already runs as an `i64`.
        (Value::Int(n), "toInt") => Ok(Value::Int(*n as i32 as i64)),
        (Value::Int(n), "toShort") => Ok(Value::Int(*n as i16 as i64)),
        (Value::Int(n), "toByte") => Ok(Value::Int(*n as i8 as i64)),
        (Value::Int(n), "toLong") => Ok(Value::Int(*n)),
        (Value::Float(f), "toDouble" | "toFloat") => Ok(Value::Float(*f)),
        // A floating-to-integer conversion SATURATES at the target width and
        // maps NaN to zero — Rust's `as` cast has exactly those semantics.
        (Value::Float(f), "toInt") => Ok(Value::Int(*f as i32 as i64)),
        (Value::Float(f), "toShort") => Ok(Value::Int(*f as i32 as i16 as i64)),
        (Value::Float(f), "toByte") => Ok(Value::Int(*f as i32 as i8 as i64)),
        (Value::Float(f), "toLong") => Ok(Value::Int(*f as i64)),

        // `Int`/`Long`.toString(radix)` renders in the given base, sign first
        // (`(-255).toString(16)` is `-ff`, not the two's-complement form).
        (Value::Int(n), "toString") if !args.is_empty() => {
            let radix = num_of(&args[0]) as u32;
            if !(2..=36).contains(&radix) {
                return Err(format!(
                    "java.lang.IllegalArgumentException: radix {radix} was not in valid range 2..36"
                ));
            }
            Ok(Value::str(to_radix(*n, radix)))
        }
        // ── kotlin.Any — defined on every type ──
        (_, "toString") => Ok(Value::str(kotlin_string(recv))),
        // `hashCode()` needs the receiver's WIDTH, because `Int` and `Long` fold
        // differently and share one runtime representation. The compiler
        // appends 32/64 the way it does for the shifts; see [`int_hash`].
        (_, "hashCode") => Ok(Value::Int(
            hash_vm(vm, recv, trailing_width(args, 0) == 64) as i64
        )),
        (_, "equals") => Ok(Value::Bool(args.first().is_some_and(|o| value_eq(recv, o)))),
        // `compareTo` on the primitives answers the sign, unlike `String`'s
        // (handled above), which answers the code-unit difference.
        (Value::Int(_) | Value::Float(_) | Value::Bool(_), "compareTo") => {
            let other = args.first().cloned().unwrap_or(Value::Int(0));
            Ok(Value::Int(match value_cmp(recv, &other) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }))
        }

        // A `String` member the text-specific arms above did not claim:
        // `kotlin.text` also mirrors the LAMBDA-FREE collection members on
        // `CharSequence`, over the characters. Reached last so a member that
        // exists on both — `take`, `first`, `indexOf`, `reversed` — keeps the
        // `String` behaviour, which differs (`"abc".take(2)` is `ab`, not
        // `[a, b]`).
        (Value::Str(s), _) => charseq_member(vm, s, name, args)
            .unwrap_or_else(|| Err(format!("unresolved reference: {name} on String"))),

        _ => Err(format!(
            "unresolved reference: {name} on {}",
            type_label(recv)
        )),
    }
}

/// The lambda-free collection members of a `CharSequence` receiver, over the
/// string's characters. `None` when the name is not one of them.
fn charseq_member(
    vm: &mut VM,
    s: &str,
    name: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    let chars = chars_of(s);
    // `chunked`/`windowed` answer a `List<String>` here, where the `Iterable`
    // overload answers a `List<List<T>>`.
    if matches!(name, "chunked" | "windowed") {
        let list = alloc(HeapObj::List(chars));
        return Some(coll_hof_str_groups(name, &list, args).map(|g| alloc(HeapObj::List(g))));
    }
    sequence_member(vm, &chars, None, SeqKind::CharSeq, None, name, args)
}

/// The `kotlin.Char` members, on the code unit `code`.
///
/// The classification predicates delegate to Rust's Unicode tables, which agree
/// with the JVM's `Character` over ASCII; a supplementary-plane character has no
/// single `Char` on either platform, so the surrogate halves classify as neither
/// letter nor digit in both.
fn char_method(code: i64, name: &str, args: &[Value]) -> Result<Value, String> {
    let c = char::from_u32(code as u32).unwrap_or(char::REPLACEMENT_CHARACTER);
    let other = || args.first().map(num_of).unwrap_or(0);
    // `uppercaseChar`/`lowercaseChar` map to a single Char, so a mapping that
    // expands (`'ß'.uppercase()` is `"SS"`) keeps the original — the JVM's
    // `Character.toUpperCase(char)` contract.
    fn single(c: char, mut mapped: impl Iterator<Item = char>) -> char {
        match (mapped.next(), mapped.next()) {
            (Some(m), None) => m,
            _ => c,
        }
    }
    Ok(match name {
        "code" => Value::Int(code),
        "toString" => Value::str(char_string(code)),
        "hashCode" => Value::Int(code),
        "equals" => Value::Bool(args.first().is_some_and(|o| value_eq(&char_of(code), o))),
        "compareTo" => Value::Int((code - other()).signum()),
        "plus" => char_of(code + other()),
        "minus" if args.first().is_some_and(is_char) => Value::Int(code - other()),
        "minus" => char_of(code - other()),
        "isDigit" => Value::Bool(c.is_numeric()),
        "isLetter" => Value::Bool(c.is_alphabetic()),
        "isLetterOrDigit" => Value::Bool(c.is_alphanumeric()),
        "isWhitespace" => Value::Bool(kotlin_is_whitespace(c)),
        "isUpperCase" => Value::Bool(c.is_uppercase()),
        "isLowerCase" => Value::Bool(c.is_lowercase()),
        "uppercaseChar" => char_of(single(c, c.to_uppercase()) as i64),
        "lowercaseChar" => char_of(single(c, c.to_lowercase()) as i64),
        "uppercase" => Value::str(c.to_uppercase().to_string()),
        "lowercase" => Value::str(c.to_lowercase().to_string()),
        "digitToInt" => match c.to_digit(10) {
            Some(d) => Value::Int(d as i64),
            None => {
                return Err(format!(
                    "java.lang.IllegalArgumentException: Char {c} is not a decimal digit"
                ))
            }
        },
        _ => return Err(format!("unresolved reference: {name} on Char")),
    })
}

/// Dispatch a member/method on a heap object (`List`/`Map`/`Pair`, or a `data`
/// class's synthesized members). User-defined class methods never reach here —
/// the compiler lowers those to direct `Op::Call`s on method subs.
fn obj_method(vm: &mut VM, recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    // A `StringBuilder` resolves first and entirely on its own: it is a
    // `CharSequence`, not a collection, so none of the collection members below
    // apply to it, and the ones whose NAMES collide (`clear`, `remove`, `set`)
    // mean something different or nothing at all.
    if let Some(units) = builder_units(recv) {
        return builder_method(vm, recv, units, name, args);
    }
    // A lazy sequence. `take`/`drop` stay lazy — `take` is the ONLY thing that
    // bounds an endless generator, so materializing it first would defeat the
    // whole point — and everything else pulls the pipeline into a `List` and
    // then resolves against that.
    if gen_parts(recv).is_some() {
        match name {
            "take" | "drop" => {
                let n = args.first().map(|v| v.to_int()).unwrap_or(0);
                if n < 0 {
                    return Err(format!(
                        "java.lang.IllegalArgumentException: Requested element count {n} is less than zero."
                    ));
                }
                let stage = if name == "take" {
                    Stage::Take(n)
                } else {
                    Stage::Drop(n)
                };
                return gen_with(vm, recv, stage);
            }
            // `asSequence` on a sequence is the identity.
            "asSequence" => return Ok(recv.clone()),
            _ => {
                // Tagged: materializing a lazy sequence must not switch its diagnostics
                // onto the `List` wording — see [`SEQ_VIEW`].
                let items = tag_sequence(alloc(HeapObj::List(gen_pull(vm, recv, None)?)));
                return obj_method(vm, &items, name, args);
            }
        }
    }
    // `componentN` (destructuring) is uniform across the ordered kinds.
    if let Some(idx) = name
        .strip_prefix("component")
        .and_then(|d| d.parse::<usize>().ok())
    {
        return component(recv, idx);
    }
    // `Throwable.message` — the constructor message, or Kotlin `null`.
    if name == "message" {
        if let Some(m) = with_obj(recv, |o| match o {
            HeapObj::Exc { msg, .. } => Some(match msg {
                Some(m) => Value::str(m.clone()),
                None => Value::Undef,
            }),
            _ => None,
        })
        .flatten()
        {
            return Ok(m);
        }
    }
    match name {
        "toString" => return Ok(Value::str(kotlin_string(recv))),
        "hashCode" => return Ok(Value::Int(hash_vm(vm, recv, false) as i64)),
        "equals" => return Ok(Value::Bool(args.first().is_some_and(|o| value_eq(recv, o)))),
        // The operator conventions are ordinary members too, so `a.plus(b)` and
        // `a + b` are the same call and must answer the same thing. The
        // `…Element` forms pin the `plus(element)` overload for a collection
        // argument that would otherwise pick `plus(elements)`.
        "plus" | "minus" | "plusAssign" | "minusAssign" => {
            let rhs = args.first().cloned().unwrap_or(Value::Undef);
            return operator_apply(vm, recv, name, &rhs);
        }
        // The `Grouping` terminal operations. `eachCount` is the one that takes
        // no lambda, so it resolves here; `fold`/`reduce`/`aggregate` carry one
        // and arrive through the higher-order path instead.
        "eachCount" | "eachCountTo" => {
            if let Some((items, key)) = grouping_parts(recv) {
                let mut entries: Vec<(Value, Value)> = Vec::new();
                for it in items {
                    let k = invoke_closure(vm, &key, std::slice::from_ref(&it))?;
                    // Keys come out in FIRST-ENCOUNTER order: `eachCount` fills
                    // a `LinkedHashMap`.
                    match entries.iter_mut().find(|(ek, _)| value_eq(ek, &k)) {
                        Some(slot) => slot.1 = Value::Int(slot.1.to_int() + 1),
                        None => entries.push((k, Value::Int(1))),
                    }
                }
                return Ok(alloc(HeapObj::Map(entries)));
            }
        }
        "plusElement" | "minusElement" => {
            let rhs = args.first().cloned().unwrap_or(Value::Undef);
            let items = as_iterable(recv)
                .ok_or_else(|| format!("unresolved reference: {name} on {}", obj_label(recv)))?;
            let mut out = items;
            if name == "plusElement" {
                out.push(rhs);
            } else if let Some(i) = position_eq(vm, &out, &rhs) {
                out.remove(i);
            }
            return Ok(alloc(HeapObj::List(out)));
        }
        _ => {}
    }

    // Mutating list operations need a mutable borrow.
    match name {
        // `MutableList.add` always appends and answers `true`; `MutableSet.add`
        // answers whether the element was NEW, which is Kotlin's contract and
        // the reason the two share one arm but not one result.
        // `add(index, element)` is a SEPARATE overload that inserts and answers
        // `Unit`, not the appending one. Reading only the first argument ran the
        // append and pushed the INDEX as the element, which is silent: it
        // answers a longer list either way and only the contents disagree.
        // A `MutableSet` has no positional overload, so this arm is list-only.
        "add" if args.len() == 2 => {
            let at = args[0].to_int();
            let v = args[1].clone();
            let out = with_obj_mut(recv, |o| match o {
                HeapObj::List(items) => Some(
                    match usize::try_from(at).ok().filter(|i| *i <= items.len()) {
                        Some(i) => {
                            items.insert(i, v);
                            Ok(())
                        }
                        // `ArrayList.rangeCheckForAdd` has its OWN message —
                        // `Index: 9, Size: 3`, not the `Index 9 out of bounds
                        // for length 3` every other position check uses.
                        None => Err(format!(
                            "java.lang.IndexOutOfBoundsException: Index: {at}, Size: {}",
                            items.len()
                        )),
                    },
                ),
                _ => None,
            })
            .flatten();
            return match out {
                Some(r) => r.map(|()| Value::Undef),
                None => Err(format!("unresolved reference: add on {}", obj_label(recv))),
            };
        }
        "add" => {
            let v = args.first().cloned().unwrap_or(Value::Undef);
            let seen = key_position(vm, recv, &v).is_some();
            let added = with_obj_mut(recv, |o| match o {
                HeapObj::List(items) => {
                    items.push(v);
                    Some(true)
                }
                HeapObj::Set(items) => {
                    if seen {
                        Some(false)
                    } else {
                        items.push(v);
                        Some(true)
                    }
                }
                _ => None,
            })
            .flatten();
            return match added {
                Some(b) => Ok(Value::Bool(b)),
                None => Err(format!("unresolved reference: add on {}", obj_label(recv))),
            };
        }
        // The bulk mutators. All three answer whether the receiver CHANGED,
        // which is why each one compares before and after rather than reporting
        // the argument's size: `addAll(emptyList())` is `false`, and so is
        // `removeAll` of elements that were never there.
        //
        // Membership runs through `key_position` — outside the heap borrow —
        // because a user `equals`/`hashCode` re-enters the VM.
        "addAll" | "removeAll" | "retainAll" => {
            let other = args.first().map(sequence_items).unwrap_or_default();
            let keep: Vec<bool> = if name == "addAll" {
                Vec::new()
            } else {
                // `retainAll` keeps what the argument holds; `removeAll` drops it.
                let want = name == "retainAll";
                let items = as_iterable(recv).unwrap_or_default();
                items
                    .iter()
                    .map(|v| other.iter().any(|o| value_eq(v, o)) == want)
                    .collect()
            };
            // A `Set` ignores an element it already holds, so `addAll` has to
            // ask per element rather than extending blindly.
            let fresh: Vec<Value> = if name == "addAll" {
                let mut seen: Vec<Value> = Vec::new();
                other
                    .iter()
                    .filter(|v| {
                        let dup = key_position(vm, recv, v).is_some()
                            || seen.iter().any(|s| value_eq(s, v));
                        seen.push((*v).clone());
                        !dup
                    })
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            let changed = with_obj_mut(recv, |o| {
                // Only a `Set` de-duplicates; a `MutableList` appends every
                // element it is given, repeats included.
                let is_set = matches!(o, HeapObj::Set(_));
                let items = match o {
                    HeapObj::List(items) | HeapObj::Set(items) => items,
                    _ => return None,
                };
                let before = items.len();
                if name == "addAll" {
                    items.extend(if is_set { fresh } else { other });
                } else {
                    let mut keep = keep.into_iter();
                    items.retain(|_| keep.next().unwrap_or(true));
                }
                Some(items.len() != before)
            })
            .flatten();
            return match changed {
                Some(b) => Ok(Value::Bool(b)),
                None => Err(format!(
                    "unresolved reference: {name} on {}",
                    obj_label(recv)
                )),
            };
        }
        // `remove(element)` on a mutable list or set — `removeAt(index)` is the
        // by-position form and stays separate.
        "remove" => {
            let v = args.first().cloned().unwrap_or(Value::Undef);
            let at = key_position(vm, recv, &v);
            let removed = with_obj_mut(recv, |o| match o {
                HeapObj::List(items) | HeapObj::Set(items) => Some(match at {
                    Some(i) => {
                        items.remove(i);
                        true
                    }
                    None => false,
                }),
                // `MutableMap.remove(key)` answers the previous value, or null.
                HeapObj::Map(entries) => Some(match at {
                    Some(i) => {
                        entries.remove(i);
                        true
                    }
                    None => false,
                }),
                _ => None,
            })
            .flatten();
            return match removed {
                Some(b) => Ok(Value::Bool(b)),
                None => Err(format!(
                    "unresolved reference: remove on {}",
                    obj_label(recv)
                )),
            };
        }
        "removeAt" => {
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            let len = with_obj(recv, |o| match o {
                HeapObj::List(items) => Some(items.len()),
                _ => None,
            })
            .flatten();
            let out = with_obj_mut(recv, |o| match o {
                HeapObj::List(items) if i >= 0 && (i as usize) < items.len() => {
                    Some(items.remove(i as usize))
                }
                _ => None,
            })
            .flatten();
            return out.ok_or_else(|| match len {
                Some(n) => format!(
                    "java.lang.IndexOutOfBoundsException: Index {i} out of bounds for length {n}"
                ),
                None => format!("unresolved reference: removeAt on {}", obj_label(recv)),
            });
        }
        // `entries` is the `Map.Entry` SET — a `Set`, not a list, which is what
        // makes `entries.hashCode()` the sum of the entry hashes rather than a
        // 31-fold and `keys == setOf(…)` order-insensitive. Each entry is a
        // [`HeapObj::Entry`], the same representation `for (e in map)` and
        // `map.map { … }` iterate.
        "entries" => {
            let out = with_obj(recv, |o| match o {
                HeapObj::Map(entries) => Some(entries.clone()),
                _ => None,
            })
            .flatten();
            return match out {
                Some(entries) => Ok(alloc(HeapObj::Set(
                    entries
                        .into_iter()
                        .map(|(k, v)| alloc(HeapObj::Entry(k, v)))
                        .collect(),
                ))),
                None => Err(format!(
                    "unresolved reference: entries on {}",
                    obj_label(recv)
                )),
            };
        }
        "keys" | "values" => {
            // Snapshot the entries under a shared borrow, then allocate the
            // result list separately (allocating inside `with_obj` would re-borrow
            // the heap).
            // `keys` is a `Set`; `values` is a plain `Collection`, whose
            // `equals`/`hashCode` the JVM leaves as identity (and whose answer
            // even varies with the map implementation `mapOf` picked), so only
            // the key side gets set semantics here.
            let want_keys = name == "keys";
            let out = with_obj(recv, |o| match o {
                HeapObj::Map(entries) => Some(
                    entries
                        .iter()
                        .map(|(k, v)| if want_keys { k.clone() } else { v.clone() })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten();
            return match out {
                Some(items) if want_keys => Ok(alloc(HeapObj::Set(items))),
                Some(items) => Ok(alloc(HeapObj::List(items))),
                None => Err(format!(
                    "unresolved reference: {name} on {}",
                    obj_label(recv)
                )),
            };
        }
        // `Map.toList()` flattens to a `List<Pair<K, V>>` in iteration order —
        // the map's own order, so a `LinkedHashMap` keeps insertion order and a
        // `HashMap` keeps its bucket order. The generic sequence `toList` below
        // cannot serve this: it would hand back the `Entry` objects, which print
        // `1=a` where a `Pair` prints `(1, a)`.
        "toList" => {
            if let Some(entries) = with_obj(recv, |o| match o {
                HeapObj::Map(entries) => Some(entries.clone()),
                _ => None,
            })
            .flatten()
            {
                let out = entries
                    .into_iter()
                    .map(|(k, v)| alloc(HeapObj::Pair(k, v)))
                    .collect();
                return Ok(alloc(HeapObj::List(out)));
            }
        }
        // `toSortedMap()` is a `java.util.TreeMap`: the entries re-ordered by
        // NATURAL key order, which is what makes it observably different from
        // the receiver even though the entries are the same.
        "toSortedMap" => {
            if let Some(entries) = with_obj(recv, |o| match o {
                HeapObj::Map(entries) => Some(entries.clone()),
                _ => None,
            })
            .flatten()
            {
                let sorted = alloc(HeapObj::Map(entries));
                // Recording the discipline rather than sorting once: a `TreeMap`
                // stays in key order across later `put`s, which is exactly what
                // [`reorder`] maintains for the other sorted collections.
                set_order(vm, &sorted, CollOrder::Sorted);
                return Ok(sorted);
            }
        }
        "put" => {
            // Map.put(k, v) → previous value or null.
            let k = args.first().cloned().unwrap_or(Value::Undef);
            let v = args.get(1).cloned().unwrap_or(Value::Undef);
            let at = key_position(vm, recv, &k);
            let prev = with_obj_mut(recv, |o| match o {
                HeapObj::Map(entries) => match at.and_then(|i| entries.get_mut(i)) {
                    Some(slot) => Some(std::mem::replace(&mut slot.1, v)),
                    None => {
                        entries.push((k, v));
                        None
                    }
                },
                _ => None,
            })
            .flatten();
            return Ok(prev.unwrap_or(Value::Undef));
        }
        _ => {}
    }

    // Members shared by every ordered sequence — `List`, arrays, and ranges.
    // Handled once here, on a snapshot taken before any allocation, so the three
    // kinds can't drift apart member by member. A range is flagged because its
    // `first`/`last` are *properties* of the progression (defined even when it is
    // empty), where a list's are element accessors.
    //
    // WHICH kind comes from [`seq_kind_of`], not from a second match written
    // out here. This site used to answer the question itself, and the two
    // answers had already drifted: a materialized `Sequence` is a
    // `HeapObj::List`, so the local match called it a `List` and
    // `listOf<Int>().asSequence().first()` said `List is empty.` where Kotlin
    // says `Sequence is empty.` The `with_obj` below decides only WHETHER the
    // receiver is an ordered sequence at all.
    let ordered = with_obj(recv, |o| {
        matches!(
            o,
            HeapObj::List(_) | HeapObj::Set(_) | HeapObj::Array { .. } | HeapObj::Range(_)
        )
    })
    .unwrap_or(false);
    if ordered {
        let kind = seq_kind_of(recv);
        let range = with_obj(recv, |o| match o {
            HeapObj::Range(r) => Some(*r),
            _ => None,
        })
        .flatten();
        let items = list_snapshot(recv).unwrap_or_default();
        if let Some(v) = sequence_member(
            vm,
            &items,
            Some(recv),
            kind,
            range.map(|r| (r.wrap(r.first), r.wrap(r.last()))),
            name,
            args,
        ) {
            return v;
        }
    }

    // `Map` key lookup runs BEFORE the read-only block: locating a key uses the
    // hash-gated container equality, which re-enters the VM for a user
    // `equals`/`hashCode` and so cannot run under the heap borrow that block
    // holds.
    let is_map = with_obj(recv, |o| matches!(o, HeapObj::Map(_))).unwrap_or(false);
    if is_map && matches!(name, "containsKey" | "get") {
        let k = args.first().cloned().unwrap_or(Value::Undef);
        let at = key_position(vm, recv, &k);
        return Ok(match name {
            "containsKey" => Value::Bool(at.is_some()),
            _ => at
                .and_then(|i| {
                    with_obj(recv, |o| match o {
                        HeapObj::Map(entries) => entries.get(i).map(|(_, v)| v.clone()),
                        _ => None,
                    })
                    .flatten()
                })
                .unwrap_or(Value::Undef),
        });
    }

    // Read-only members.
    let res = with_obj(recv, |o| match (o, name) {
        // ── Map ──
        (HeapObj::Map(entries), "size") => Some(Value::Int(entries.len() as i64)),
        (HeapObj::Map(entries), "isEmpty") => Some(Value::Bool(entries.is_empty())),
        (HeapObj::Map(entries), "isNotEmpty") => Some(Value::Bool(!entries.is_empty())),
        // ── Pair ──
        // `key`/`value` alias `first`/`second`: iterating a `Map` yields
        // `Map.Entry`, which is carried as a `Pair` here, and an entry is read
        // through `it.key`/`it.value`.
        (HeapObj::Pair(a, _), "first" | "key") => Some(a.clone()),
        (HeapObj::Pair(_, b), "second" | "value") => Some(b.clone()),
        // ── Result ──
        (HeapObj::Res { err, .. }, "isSuccess") => Some(Value::Bool(err.is_none())),
        (HeapObj::Res { err, .. }, "isFailure") => Some(Value::Bool(err.is_some())),
        // `getOrNull()` is null on failure; `exceptionOrNull()` is null on
        // success. Both are the total readers of the union.
        (HeapObj::Res { value, err }, "getOrNull") => Some(match err {
            Some(_) => Value::Undef,
            None => value.clone(),
        }),
        (HeapObj::Res { err, .. }, "exceptionOrNull") => Some(err.clone().unwrap_or(Value::Undef)),
        // ── Triple ──
        (HeapObj::Triple(a, _, _), "first") => Some(a.clone()),
        (HeapObj::Triple(_, b, _), "second") => Some(b.clone()),
        (HeapObj::Triple(_, _, c), "third") => Some(c.clone()),
        // A `Map.Entry` has `key`/`value` and — unlike a `Pair` — no
        // `first`/`second`.
        (HeapObj::Entry(k, _), "key") => Some(k.clone()),
        (HeapObj::Entry(_, v), "value") => Some(v.clone()),
        // Every enum constant is `Comparable<E>` by `ordinal`. The member lives
        // here rather than being lowered like the other enum members, because a
        // compiler-generated body would have to exist per enum; the ordering is
        // the same for all of them. The JVM answers the ordinal DIFFERENCE, not
        // its sign — `Dir.NORTH.compareTo(Dir.WEST)` is `-3`.
        (HeapObj::Instance { class, fields, .. }, "compareTo") if is_enum_class(class) => {
            let mine = fields
                .iter()
                .find(|(n, _)| n == "ordinal")
                .map(|(_, v)| num_of(v));
            match (mine, args.first().and_then(enum_ordinal)) {
                (Some(x), Some(y)) => Some(Value::Int(x - y)),
                _ => None,
            }
        }
        // ── Instance property read (dynamic fallback when the compiler couldn't
        // statically resolve the receiver's class, e.g. `list[i].field`) ──
        (HeapObj::Instance { fields, .. }, _) => fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone()),
        _ => None,
    });
    match res.flatten() {
        Some(v) => Ok(v),
        None => Err(format!(
            "unresolved reference: {name} on {}",
            obj_label(recv)
        )),
    }
}

/// Which kind of receiver a shared sequence member was reached through.
///
/// The members themselves are kind-agnostic — that is the point of sharing one
/// implementation — but their DIAGNOSTICS are not: Kotlin's stdlib declares
/// `first`/`single` separately for `List`, `Array`, `Iterable` and
/// `CharSequence`, and each spells its own empty-receiver message. Passing only
/// a `hashed` flag (all a `Set`'s equality needed) left every kind answering
/// the `List` wording, so `arrayOf<Int>().first()` said `List is empty.`
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeqKind {
    List,
    Array,
    /// An `IntRange`/`IntProgression`. Its shared members reach it as a plain
    /// `Iterable`, which is the `Collection` wording.
    Range,
    Set,
    /// A `String`/`StringBuilder` read as its characters.
    CharSeq,
    /// A lazy `Sequence`. Only a `generateSequence` pipeline is one here —
    /// `asSequence()` materializes into a `List`, and its diagnostics follow
    /// that representation rather than the type the source wrote.
    Seq,
}

impl SeqKind {
    /// Whether membership is hash-gated (a `Set`) rather than sequential; see
    /// [`member_eq`].
    fn hashed(self) -> bool {
        self == SeqKind::Set
    }

    /// Whether the receiver is a `java.util.Collection`, which decides which
    /// branch of `Iterable.sorted` runs — see [`ListImpl::Sorted`]. A range and
    /// a lazy sequence are `Iterable` only.
    fn is_collection(self) -> bool {
        matches!(
            self,
            SeqKind::List | SeqKind::Set | SeqKind::Array | SeqKind::CharSeq
        )
    }

    /// The `NoSuchElementException` detail for an empty receiver.
    fn empty(self) -> &'static str {
        match self {
            SeqKind::List => "List is empty.",
            SeqKind::Array => "Array is empty.",
            SeqKind::Set | SeqKind::Range => "Collection is empty.",
            SeqKind::CharSeq => "Char sequence is empty.",
            SeqKind::Seq => "Sequence is empty.",
        }
    }

    /// The `IllegalArgumentException` subject `single()` raises for a receiver
    /// with more than one element.
    fn more_than_one(self) -> &'static str {
        match self {
            SeqKind::List => "List",
            SeqKind::Array => "Array",
            SeqKind::Set | SeqKind::Range => "Collection",
            SeqKind::CharSeq => "Char sequence",
            SeqKind::Seq => "Sequence",
        }
    }

    /// The subject of a PREDICATE member's diagnostics, which is the receiver
    /// type the stdlib declared that member on — not always this receiver's own
    /// type. `first`/`single`/`find` are declared on `Iterable`, so a list
    /// receiver is a "Collection" to them; `last` needs to walk backwards and
    /// is declared on `List`, so the same receiver names itself there.
    fn predicate_subject(self, name: &str) -> &'static str {
        match self {
            SeqKind::List if name.starts_with("last") => "List",
            SeqKind::List | SeqKind::Set | SeqKind::Range => "Collection",
            _ => self.more_than_one(),
        }
    }

    /// `NoSuchElementException` for a predicate nothing matched. A
    /// `CharSequence` searches for a CHARACTER, so the noun changes with the
    /// subject.
    fn no_match(self, name: &str) -> String {
        format!(
            "java.util.NoSuchElementException: {} contains no {} matching the predicate.",
            self.predicate_subject(name),
            if self == SeqKind::CharSeq {
                "character"
            } else {
                "element"
            }
        )
    }

    /// `IllegalArgumentException` for a `single { … }` more than one element
    /// matched. Unlike [`SeqKind::no_match`] the noun here is always "element",
    /// even for a `CharSequence`.
    fn many_match(self, name: &str) -> String {
        format!(
            "java.lang.IllegalArgumentException: {} contains more than one matching element.",
            self.predicate_subject(name)
        )
    }

    /// The noun in `Empty X can't be reduced.` `reduceRight` walks backwards,
    /// which only an indexed receiver can do, so it is declared on `List` where
    /// `reduce` is declared on `Iterable` — and each names its own declaration.
    fn reduce_noun(self, name: &str) -> &'static str {
        match self {
            SeqKind::Array => "array",
            SeqKind::CharSeq => "char sequence",
            SeqKind::Seq => "sequence",
            SeqKind::List if name.starts_with("reduceRight") => "list",
            _ => "collection",
        }
    }
}

/// The result of a selector-driven extremum — `maxBy`/`minBy`/`maxOf`/`minOf`
/// and their `…OrNull` twins.
///
/// An empty receiver is null for the `…OrNull` spelling and a `NoSuchElement`
/// for the plain one. The exception is raised BARE: the whole extremum family
/// spells `throw NoSuchElementException()` with no argument, unlike
/// `first`/`last`/`single`, which name the receiver kind.
fn extremum_or(best: Option<Value>, name: &str) -> Result<Value, String> {
    match (best, name.ends_with("OrNull")) {
        (Some(v), _) => Ok(v),
        (None, true) => Ok(Value::Undef),
        (None, false) => Err("java.util.NoSuchElementException".to_string()),
    }
}

/// Which [`SeqKind`] a receiver presents to the shared higher-order members.
fn seq_kind_of(v: &Value) -> SeqKind {
    if matches!(v, Value::Str(_)) {
        return SeqKind::CharSeq;
    }
    // A materialized `Sequence` is carried as a `List` and would otherwise
    // answer the `List` wording — see [`SEQ_VIEW`].
    if is_sequence_view(v) {
        return SeqKind::Seq;
    }
    with_obj(v, |o| match o {
        HeapObj::Set(_) => SeqKind::Set,
        HeapObj::Map(_) => SeqKind::Set,
        HeapObj::Array { .. } => SeqKind::Array,
        HeapObj::Range(_) => SeqKind::Range,
        HeapObj::Gen { .. } => SeqKind::Seq,
        HeapObj::Builder { .. } => SeqKind::CharSeq,
        _ => SeqKind::List,
    })
    .unwrap_or(SeqKind::List)
}

/// The out-of-range diagnostic for `get`/`elementAt` on a `kind` receiver.
///
/// Four different exceptions, because four different stdlib declarations answer
/// the call. An ARRAY's `get` is the JVM's `aaload`, so it raises
/// `ArrayIndexOutOfBoundsException`. A `Set` or a progression has no positional
/// access at all: `Iterable.elementAt` walks and reports having run out. A
/// `CharSequence` raises the `String` subclass. And a `List` raises whichever
/// its IMPLEMENTATION does — see [`ListImpl`] for the table, which `recv`
/// carries the provenance for.
///
/// `recv` is `None` for the `String`-as-characters caller, which has no handle
/// and needs none.
fn index_fault(kind: SeqKind, recv: Option<&Value>, i: i64, len: usize) -> String {
    let plain =
        format!("java.lang.IndexOutOfBoundsException: Index {i} out of bounds for length {len}");
    match kind {
        SeqKind::Array => format!(
            "java.lang.ArrayIndexOutOfBoundsException: Index {i} out of bounds for length {len}"
        ),
        SeqKind::List => {
            let which = recv.and_then(|v| match v {
                Value::Obj(id) => LIST_IMPL.with(|t| t.borrow().get(id).copied()),
                _ => None,
            });
            match (which, len) {
                // The builder collapses at NO size and words the fault its own
                // way, so it is decided before the two small-size rows.
                (Some(ListImpl::Builder), _) => {
                    format!("java.lang.IndexOutOfBoundsException: index: {i}, size: {len}")
                }
                // `optimizeReadOnlyList` collapses BOTH read-only kinds at the
                // two small sizes, so the constructor stops mattering there.
                (Some(_), 0) => format!(
                    "java.lang.IndexOutOfBoundsException: \
                     Empty list doesn't contain element at index {i}."
                ),
                (Some(_), 1) => {
                    format!("java.lang.IndexOutOfBoundsException: Index: {i}, Size: 1")
                }
                // A literal of two or more is array-backed, and so is the
                // result of a `sorted…` over a collection — both wrap a bare
                // array, and both raise the ARRAY subclass.
                (Some(ListImpl::Literal | ListImpl::Sorted), _) => format!(
                    "java.lang.ArrayIndexOutOfBoundsException: \
                     Index {i} out of bounds for length {len}"
                ),
                _ => plain,
            }
        }
        // `Iterable.elementAt` and `Sequence.elementAt` are two declarations
        // with two messages — `Collection doesn't contain…` and `Sequence
        // doesn't contain…`. Measured on kotlinc 2.4.10 / JDK 21.0.12.
        SeqKind::Set | SeqKind::Range => format!(
            "java.lang.IndexOutOfBoundsException: Collection doesn't contain element at index {i}."
        ),
        SeqKind::Seq => format!(
            "java.lang.IndexOutOfBoundsException: Sequence doesn't contain element at index {i}."
        ),
        SeqKind::CharSeq => sioobe_index(i, len),
    }
}

/// The read-only members every ordered sequence shares — `List`, arrays, and
/// ranges. `range` carries `(first, last)` when the receiver is a range, whose
/// `first`/`last` are progression *properties* (defined even for an empty range)
/// rather than element accessors.
///
/// Returns `None` when `name` is not a sequence member at all, so the caller can
/// keep looking (a `Map`/`Pair`/instance member, or the unresolved-reference
/// diagnostic).
fn sequence_member(
    vm: &mut VM,
    items: &[Value],
    // The receiver handle, for the diagnostics that name its implementation
    // class. `None` from the `String`-as-characters caller, which has none.
    recv: Option<&Value>,
    kind: SeqKind,
    range: Option<(Value, Value)>,
    name: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    let hashed = kind.hashed();
    // Kotlin throws on `first()`/`last()`/`single()` over an empty sequence
    // rather than returning null, and the message NAMES the receiver kind.
    let need = |v: Option<Value>| -> Result<Value, String> {
        v.ok_or_else(|| format!("java.util.NoSuchElementException: {}", kind.empty()))
    };
    let v = match name {
        "size" | "count" => Value::Int(items.len() as i64),
        "isEmpty" => Value::Bool(items.is_empty()),
        "isNotEmpty" => Value::Bool(!items.is_empty()),
        "first" => match range {
            Some((first, _)) => first,
            None => return Some(need(items.first().cloned())),
        },
        "last" => match range {
            Some((_, last)) => last,
            None => return Some(need(items.last().cloned())),
        },
        // `get`/`elementAt` throw out of range; `getOrNull`/`elementAtOrNull`
        // answer null there. `first`/`last` above draw the same distinction
        // against their `…OrNull` forms.
        // Kotlin's `elementAt` on a `List` or an array IS `get`, so it faults
        // the way an INDEX does — naming the index and the length. Only the
        // kinds with no positional access fall back to the `Iterable` walk,
        // which reports having run out rather than a bound.
        //
        // Answering `List is empty.` here named neither the index nor the real
        // length, and said "empty" about a receiver that was not.
        "get" | "elementAt" => {
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            return Some(
                match usize::try_from(i).ok().and_then(|i| items.get(i).cloned()) {
                    Some(v) => Ok(v),
                    None => Err(index_fault(kind, recv, i, items.len())),
                },
            );
        }
        "getOrNull" | "elementAtOrNull" => {
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            usize::try_from(i)
                .ok()
                .and_then(|i| items.get(i).cloned())
                .unwrap_or(Value::Undef)
        }
        "firstOrNull" => items.first().cloned().unwrap_or(Value::Undef),
        "lastOrNull" => items.last().cloned().unwrap_or(Value::Undef),
        // `subList(from, to)` is the half-open slice, and unlike `take`/`drop`
        // it does NOT clamp — an out-of-range bound throws.
        // `slice(indices)` takes ONE argument — a range or any index sequence —
        // and picks those positions, where `subList(from, to)` takes two bounds.
        // Sharing an arm read the range as `from` and dropped the upper bound,
        // so `slice(0..1)` answered the whole receiver.
        "slice" => {
            let idx = args.first().map(sequence_items).unwrap_or_default();
            let mut out = Vec::with_capacity(idx.len());
            for i in idx {
                let i = i.to_int();
                match usize::try_from(i).ok().and_then(|i| items.get(i)) {
                    Some(v) => out.push(v.clone()),
                    None => {
                        return Some(Err(format!(
                            "java.lang.IndexOutOfBoundsException: \
                             Index: {i}, Size: {}",
                            items.len()
                        )))
                    }
                }
            }
            return Some(Ok(alloc_ro_list(out)));
        }
        "subList" => {
            let from = args.first().map(|v| v.to_int()).unwrap_or(0);
            let to = args
                .get(1)
                .map(|v| v.to_int())
                .unwrap_or(items.len() as i64);
            // `AbstractList.subListRangeCheck` reports ONE bound per fault, in
            // this order, and a crossed pair is not out of bounds at all — it
            // is an `IllegalArgumentException` naming both.
            if from < 0 {
                return Some(Err(format!(
                    "java.lang.IndexOutOfBoundsException: fromIndex = {from}"
                )));
            }
            if to > items.len() as i64 {
                return Some(Err(format!(
                    "java.lang.IndexOutOfBoundsException: toIndex = {to}"
                )));
            }
            if from > to {
                return Some(Err(format!(
                    "java.lang.IllegalArgumentException: fromIndex({from}) > toIndex({to})"
                )));
            }
            return Some(Ok(alloc(HeapObj::List(
                items[from as usize..to as usize].to_vec(),
            ))));
        }
        "contains" => {
            let needle = args.first().cloned();
            Value::Bool(needle.is_some_and(|a| items.iter().any(|v| member_eq(vm, v, &a, hashed))))
        }
        // `Collection.containsAll` — every element of the argument present, by
        // the receiver's own equality rule. Vacuously true for an empty
        // argument, as on the JVM.
        "containsAll" => {
            let wanted = args.first().map(sequence_items).unwrap_or_default();
            Value::Bool(
                wanted
                    .iter()
                    .all(|w| items.iter().any(|v| member_eq(vm, v, w, hashed))),
            )
        }
        "indexOf" => {
            let needle = args.first().cloned();
            Value::Int(
                needle
                    .and_then(|a| items.iter().position(|v| member_eq(vm, v, &a, hashed)))
                    .map(|p| p as i64)
                    .unwrap_or(-1),
            )
        }
        "sum" => sum_values(items),
        // An empty average is NaN in Kotlin, not an error.
        "average" => {
            Value::Float(items.iter().map(|v| v.to_float()).sum::<f64>() / items.len() as f64)
        }
        // `max`/`min` throw on an empty sequence; the `…OrNull` pair answers
        // null instead. That is the only difference between them.
        //
        // Their `NoSuchElementException` carries NO message, unlike
        // `first`/`last`/`single`: the stdlib raises it bare
        // (`throw NoSuchElementException()`), so a `need`-shaped message here
        // would print a detail the JVM never does.
        "max" | "min" | "maxOrNull" | "minOrNull" => {
            let want_max = name.starts_with("max");
            let best = items.iter().cloned().reduce(|a, b| {
                // A floating-point receiver takes the `Math.min`/`Math.max`
                // overload, which PROPAGATES NaN rather than ordering it. The
                // comparator route agrees for `max` — the total order already
                // puts NaN on top — but disagrees for `min`, which would answer
                // the smallest number and leave the NaN behind.
                if matches!((&a, &b), (Value::Float(_), _) | (_, Value::Float(_)))
                    && (a.to_float().is_nan() || b.to_float().is_nan())
                {
                    return Value::Float(f64::NAN);
                }
                let take_b = (value_cmp(&b, &a) == std::cmp::Ordering::Greater) == want_max;
                if take_b {
                    b
                } else {
                    a
                }
            });
            if name.ends_with("OrNull") {
                return Some(Ok(best.unwrap_or(Value::Undef)));
            }
            return Some(best.ok_or_else(|| "java.util.NoSuchElementException".to_string()));
        }
        // `flatten()` concatenates one nesting level; a non-iterable element has
        // no `Iterable` receiver in Kotlin, so it cannot occur here.
        "flatten" => {
            let mut out = Vec::new();
            for it in items {
                out.extend(sequence_items(it));
            }
            return Some(Ok(alloc(HeapObj::List(out))));
        }
        // `zip(other)` pairs element-wise and stops at the shorter sequence.
        "zip" => {
            let other = args.first().map(sequence_items).unwrap_or_default();
            let out: Vec<Value> = items
                .iter()
                .zip(other)
                .map(|(a, b)| alloc(HeapObj::Pair(a.clone(), b)))
                .collect();
            return Some(Ok(alloc(HeapObj::List(out))));
        }
        // `chunked(n)` splits into consecutive groups, the last one possibly
        // short; `windowed(n)` slides by one and — with the default
        // `partialWindows = false` — emits only full-length windows.
        // `chunked(size)` is `windowed(size, step = size, partialWindows = true)`
        // — Kotlin defines it that way, and sharing the walk here is what keeps
        // the two consistent once `step`/`partialWindows` are honoured. Dropping
        // them silently re-ran the defaults: `windowed(2, 2)` slid by one.
        "chunked" | "windowed" => {
            let n = args.first().map(|v| v.to_int()).unwrap_or(0);
            let chunking = name == "chunked";
            let step = if chunking {
                n
            } else {
                args.get(1).map(|v| v.to_int()).unwrap_or(1)
            };
            if let Err(e) = check_window_size_step(n, step) {
                return Some(Err(e));
            }
            let partial = chunking || args.get(2).is_some_and(truthy);
            return Some(Ok(alloc(HeapObj::List(windows_of(
                items,
                n as usize,
                step as usize,
                partial,
            )))));
        }
        // `toList` re-optimizes and its four neighbours do not: Kotlin's
        // `toList` ends in `optimizeReadOnlyList`, so a one-element receiver
        // comes back a `SingletonList`, while `toMutableList` is an `ArrayList`
        // by definition and the `as…` views wrap the receiver. Same elements,
        // different class, and the difference shows in an out-of-range message.
        "toList" => return Some(Ok(alloc_ro_list(items.to_vec()))),
        "toMutableList" | "toTypedArray" | "asList" | "asIterable" => {
            return Some(Ok(alloc(HeapObj::List(items.to_vec()))))
        }
        // Same eager representation, but tagged: its empty/exhausted
        // diagnostics are the `Sequence` ones, not the `List` ones.
        "asSequence" => return Some(Ok(tag_sequence(alloc(HeapObj::List(items.to_vec()))))),
        // `withIndex()` pairs each element with its position. Kotlin's element
        // type is the data class `IndexedValue`, whose `index`/`value` are read
        // as ordinary properties and which prints as
        // `IndexedValue(index=0, value=a)` — so it is built as a data instance
        // rather than as a `Pair`, which would print `(0, a)`.
        "withIndex" => {
            let out = items
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    alloc(HeapObj::Instance {
                        class: "IndexedValue".to_string(),
                        is_data: true,
                        fields: vec![
                            ("index".to_string(), Value::Int(i as i64)),
                            ("value".to_string(), v.clone()),
                        ],
                        data_from: 0,
                        data_len: 2,
                    })
                })
                .collect();
            return Some(Ok(alloc(HeapObj::List(out))));
        }
        // `single()` — the element of a one-element sequence, and an error for
        // any other length. The predicate form is a higher-order member and
        // lives in `coll_hof`.
        "single" | "singleOrNull" if args.is_empty() => {
            return Some(match (items.len(), name) {
                (1, _) => Ok(items[0].clone()),
                (_, "singleOrNull") => Ok(Value::Undef),
                (0, _) => Err(format!(
                    "java.util.NoSuchElementException: {}",
                    kind.empty()
                )),
                _ => Err(format!(
                    "java.lang.IllegalArgumentException: {} has more than one element.",
                    kind.more_than_one()
                )),
            })
        }
        // `toSet` yields a `Set`; `distinct` yields a `List` with the same
        // elements — the pair Kotlin draws the distinction between.
        "toSet" | "toMutableSet" | "toHashSet" => {
            return Some(Ok(alloc(HeapObj::Set(distinct(vm, items)))))
        }
        "distinct" => return Some(Ok(alloc_ro_list(distinct(vm, items)))),
        // `filterNotNull` drops Kotlin `null` (carried as `Undef`) and always
        // answers a `List`, whichever kind the receiver was — it is the
        // type-narrowing member, not a `filter` overload.
        "filterNotNull" => {
            return Some(Ok(alloc(HeapObj::List(
                items
                    .iter()
                    .filter(|v| !matches!(v, Value::Undef))
                    .cloned()
                    .collect(),
            ))))
        }
        // The set operators are defined on any `Iterable` and all return a
        // `Set`, whichever kind the receiver was.
        "union" | "intersect" | "subtract" => {
            let other = args.first().map(sequence_items).unwrap_or_default();
            let out: Vec<Value> = match name {
                "union" => distinct(vm, &[items.to_vec(), other].concat()),
                "intersect" => distinct(vm, items)
                    .into_iter()
                    .filter(|x| other.iter().any(|y| value_eq(x, y)))
                    .collect(),
                _ => distinct(vm, items)
                    .into_iter()
                    .filter(|x| !other.iter().any(|y| value_eq(x, y)))
                    .collect(),
            };
            return Some(Ok(alloc(HeapObj::Set(out))));
        }
        "sorted" | "sortedDescending" => {
            let mut out = items.to_vec();
            out.sort_by(value_cmp);
            if name == "sortedDescending" {
                out.reverse();
            }
            return Some(Ok(alloc_sorted_list(out, kind.is_collection())));
        }
        // `take`/`drop` clamp rather than fault: Kotlin returns the whole
        // sequence for an oversized `take` and an empty one for an oversized
        // `drop`. A negative count is an IllegalArgumentException there.
        "take" | "drop" => {
            let n = args.first().map(|v| v.to_int()).unwrap_or(0);
            if n < 0 {
                return Some(Err(format!(
                    "java.lang.IllegalArgumentException: Requested element count {n} is less than zero."
                )));
            }
            let n = (n as usize).min(items.len());
            let out = if name == "take" {
                items[..n].to_vec()
            } else {
                items[n..].to_vec()
            };
            return Some(Ok(alloc_ro_list(out)));
        }
        // The from-the-end pair. Same clamping and the same message on a
        // negative count as `take`/`drop`, counted from the other end: for a
        // count of `n`, `takeLast` keeps the tail and `dropLast` keeps
        // everything before it, so the two partition the receiver.
        "takeLast" | "dropLast" => {
            let n = args.first().map(|v| v.to_int()).unwrap_or(0);
            if n < 0 {
                return Some(Err(format!(
                    "java.lang.IllegalArgumentException: Requested element count {n} is less than zero."
                )));
            }
            let cut = items.len() - (n as usize).min(items.len());
            let out = if name == "takeLast" {
                items[cut..].to_vec()
            } else {
                items[..cut].to_vec()
            };
            return Some(Ok(alloc_ro_list(out)));
        }
        // `unzip` is `zip` run backwards: a list of pairs becomes a PAIR of
        // lists, `[(1, a), (2, b)]` → `([1, 2], [a, b])`.
        "unzip" => {
            let (mut lhs, mut rhs) = (Vec::with_capacity(items.len()), Vec::new());
            for it in items {
                match with_obj(it, |o| match o {
                    HeapObj::Pair(a, b) | HeapObj::Entry(a, b) => Some((a.clone(), b.clone())),
                    _ => None,
                })
                .flatten()
                {
                    Some((a, b)) => {
                        lhs.push(a);
                        rhs.push(b);
                    }
                    None => return Some(Err(format!("unresolved reference: {name} on List"))),
                }
            }
            return Some(Ok(alloc(HeapObj::Pair(
                alloc(HeapObj::List(lhs)),
                alloc(HeapObj::List(rhs)),
            ))));
        }
        // `zipWithNext()` pairs each element with its SUCCESSOR, so the result
        // is one shorter than the receiver and empty for a receiver of one. The
        // lambda overload transforms each pair instead and lives in `coll_hof`.
        "zipWithNext" => {
            let out: Vec<Value> = items
                .windows(2)
                .map(|w| alloc(HeapObj::Pair(w[0].clone(), w[1].clone())))
                .collect();
            return Some(Ok(alloc(HeapObj::List(out))));
        }
        // `joinToString(separator, prefix, postfix, limit, truncated)`. Only
        // the separator used to be read, so every affix was silently dropped
        // and `joinToString("-", "<", ">")` printed `1-2-3`.
        "joinToString" => Value::str(join_to_string(items, args, 0, None)),
        "reversed" => {
            // `IntRange.reversed()` is an `IntProgression` counting down; a
            // list's is a plain reversed list.
            return Some(Ok(match range {
                Some((first, last)) => alloc(HeapObj::Range(RangeObj {
                    first: num_of(&last),
                    end: num_of(&first),
                    step: -1,
                    progression: true,
                    is_char: is_char(&first),
                })),
                None => alloc_ro_list(items.iter().rev().cloned().collect()),
            }));
        }
        _ => return None,
    };
    Some(Ok(v))
}

/// `componentN` for the ordered heap kinds (data-class field / list element /
/// pair half) — 1-based, as Kotlin destructuring uses.
fn component(recv: &Value, n: usize) -> Result<Value, String> {
    with_obj(recv, |o| match o {
        // `componentN` counts from the first primary-constructor property, so
        // destructuring an inheriting `data class` skips its inherited fields.
        HeapObj::Instance {
            fields,
            data_from,
            data_len,
            ..
        } => (n <= *data_len)
            .then(|| fields.get(data_from + n - 1).map(|(_, v)| v.clone()))
            .flatten(),
        HeapObj::List(items) | HeapObj::Set(items) | HeapObj::Array { items, .. } => {
            items.get(n - 1).cloned()
        }
        // `for ((k, v) in map)` destructures an entry exactly as it does a
        // pair — the two differ in display and equality, not in arity.
        // A `by lazy` cell has no components; the compiler forces every read of
        // one, so it never reaches user code in the first place.
        HeapObj::Lazy { .. } | HeapObj::Res { .. } => None,
        HeapObj::Triple(a, b, c) => match n {
            1 => Some(a.clone()),
            2 => Some(b.clone()),
            3 => Some(c.clone()),
            _ => None,
        },
        HeapObj::Pair(a, b) | HeapObj::Entry(a, b) => match n {
            1 => Some(a.clone()),
            2 => Some(b.clone()),
            _ => None,
        },
        HeapObj::Map(_)
        | HeapObj::Closure { .. }
        | HeapObj::Comparator(_)
        | HeapObj::Gen { .. }
        | HeapObj::Grouping { .. }
        | HeapObj::Range(_)
        | HeapObj::Builder { .. }
        | HeapObj::Exc { .. } => None,
    })
    .flatten()
    .ok_or_else(|| format!("no component{n} on {}", obj_label(recv)))
}

/// Sum a list of numbers — `Int` result when every element is integral, else
/// `Double` (Kotlin `List<Int>.sum()` / `List<Double>.sum()`).
fn sum_values(items: &[Value]) -> Value {
    if items.iter().all(|v| matches!(v, Value::Int(_))) {
        Value::Int(items.iter().map(|v| v.to_int()).sum())
    } else {
        Value::Float(items.iter().map(|v| v.to_float()).sum())
    }
}

/// `recv[index]` — list element (bounds-checked) or map value (null if absent).
fn index_get(vm: &mut VM, recv: &Value, index: &Value) -> Result<Value, String> {
    // `s[i]` is a `Char`, indexed by UTF-16 code unit — the same basis
    // `String.length` uses.
    if let Value::Str(s) = recv {
        let i = index.to_int();
        let len = s.encode_utf16().count();
        return match usize::try_from(i)
            .ok()
            .and_then(|i| s.encode_utf16().nth(i))
        {
            Some(u) => Ok(char_of(u as i64)),
            None => Err(sioobe_index(i, len)),
        };
    }
    // `sb[i]` indexes the builder's code units, with the same diagnostic a
    // `String` gives.
    if let Some(units) = builder_units(recv) {
        let i = index.to_int();
        return match usize::try_from(i).ok().and_then(|i| units.get(i)) {
            Some(u) => Ok(char_of(*u as i64)),
            None => Err(sioobe_index(i, units.len())),
        };
    }
    // A `Map` key search runs first and OUTSIDE the heap borrow below: it is
    // hash-gated container equality, which re-enters the VM for a user
    // `equals`/`hashCode`.
    if with_obj(recv, |o| matches!(o, HeapObj::Map(_))).unwrap_or(false) {
        let at = key_position(vm, recv, index);
        // Map get returns null (Kotlin `V?`) when the key is absent.
        return Ok(at
            .and_then(|i| {
                with_obj(recv, |o| match o {
                    HeapObj::Map(entries) => entries.get(i).map(|(_, v)| v.clone()),
                    _ => None,
                })
                .flatten()
            })
            .unwrap_or(Value::Undef));
    }
    // `xs[i]` and `xs.get(i)` are the same call in Kotlin and must fault
    // identically, so both route through [`index_fault`] — which for a `List`
    // names the implementation class its provenance recorded. The array arm
    // keeps the JVM's `aaload` exception; a `Set` reaches here only defensively
    // (Kotlin gives it no positional `get`) and is read as an array.
    let out = with_obj(recv, |o| {
        let (items, kind) = match o {
            HeapObj::List(items) => (items, SeqKind::List),
            HeapObj::Set(items) | HeapObj::Array { items, .. } => (items, SeqKind::Array),
            _ => return Err(format!("{} does not support indexing", obj_label(recv))),
        };
        let i = index.to_int();
        match usize::try_from(i).ok().and_then(|i| items.get(i)) {
            Some(v) => Ok(v.clone()),
            None => Err(index_fault(kind, Some(recv), i, items.len())),
        }
    });
    out.unwrap_or_else(|| Err("indexing a non-object value".to_string()))
}

/// The index of the element (or map key) of `recv` that equals `v`.
///
/// Kept apart from the mutating members that need it because structural equality
/// re-enters the heap — comparing two lists compares their elements — so the
/// search must run under a *shared* borrow. Nesting it inside the `borrow_mut`
/// the mutation takes would panic, which is why every mutator below locates
/// first and mutates second.
fn key_position(vm: &mut VM, recv: &Value, v: &Value) -> Option<usize> {
    // Extracted before the scan for the reason above, and because a user
    // `equals` re-enters the VM and can allocate.
    let (keys, hashed) = with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Array { items, .. } => (items.clone(), false),
        HeapObj::Set(items) => (items.clone(), true),
        HeapObj::Map(entries) => (entries.iter().map(|(k, _)| k.clone()).collect(), true),
        _ => (Vec::new(), false),
    })?;
    keys.iter().position(|x| member_eq(vm, x, v, hashed))
}

/// Store `value` at `i`, or the JVM's out-of-bounds diagnostic.
///
/// `kind` is the exception-class prefix: `"Array"` for an array store, which
/// the JVM checks with `aastore` and reports as
/// `ArrayIndexOutOfBoundsException`, and `""` for a `MutableList` store, which
/// goes through `ArrayList.set` and reports the plain
/// `IndexOutOfBoundsException`. Both carry the same message text.
fn store_at(items: &mut [Value], i: i64, value: Value, kind: &str) -> Result<(), String> {
    match usize::try_from(i).ok().and_then(|i| items.get_mut(i)) {
        Some(slot) => {
            *slot = value;
            Ok(())
        }
        None => Err(format!(
            "java.lang.{kind}IndexOutOfBoundsException: Index {i} out of bounds for length {}",
            items.len()
        )),
    }
}

/// `recv[index] = value` — list set (bounds-checked) or map put.
fn index_set(vm: &mut VM, recv: &Value, index: &Value, value: Value) -> Result<(), String> {
    // Only a `Map` looks its slot up by equality; a list/array indexes by
    // position, so it must not pay for the scan.
    let is_map = with_obj(recv, |o| matches!(o, HeapObj::Map(_))).unwrap_or(false);
    let at = if is_map {
        key_position(vm, recv, index)
    } else {
        None
    };
    let out = with_obj_mut(recv, |o| match o {
        // A store past the end names its bound, and the two receivers do not
        // raise the same class: an array's store is a JVM `aastore`, so it
        // faults as `ArrayIndexOutOfBoundsException`, where a `MutableList`'s
        // goes through `ArrayList.set` and faults as the plain
        // `IndexOutOfBoundsException`. The message text is shared.
        HeapObj::List(items) => store_at(items, index.to_int(), value, ""),
        HeapObj::Array { items, .. } => store_at(items, index.to_int(), value, "Array"),
        HeapObj::Map(entries) => {
            match at.and_then(|i| entries.get_mut(i)) {
                Some(slot) => slot.1 = value,
                None => entries.push((index.clone(), value)),
            }
            Ok(())
        }
        // `sb[i] = c` — `kotlin.text`'s operator spelling of `setCharAt`.
        HeapObj::Builder { units, .. } => {
            let i = index.to_int();
            match usize::try_from(i).ok().filter(|i| *i < units.len()) {
                Some(i) => {
                    units[i] = char_code(&value).unwrap_or_else(|| value.to_int()) as u16;
                    Ok(())
                }
                None => Err(sioobe_index(i, units.len())),
            }
        }
        _ => Err(format!(
            "{} does not support indexed assignment",
            obj_label(recv)
        )),
    });
    // A new key appended to a `HashMap` belongs in its bucket, not at the end.
    reorder(vm, recv);
    out.unwrap_or_else(|| Err("indexing a non-object value".to_string()))
}

/// Structural equality — `==` over heap objects (recursively) and value
/// equality over primitives. Ints and Doubles compare by numeric value.
pub fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        // The same handle is the same object — this is what makes the identity
        // equality an array inherits from `Object` come out `true` for `a == a`
        // while `arrayOf(1) == arrayOf(1)` stays `false`. It is also all a
        // `Char` needs, its handle being its value, and it is checked before the
        // heap is touched so neither answer takes a borrow it does not need.
        (Value::Obj(ia), Value::Obj(ib)) if ia == ib => true,
        (Value::Obj(ia), Value::Obj(ib)) => HEAP.with(|h| {
            let h = h.borrow();
            match (h.get(*ia as usize), h.get(*ib as usize)) {
                (Some(oa), Some(ob)) => heap_eq(oa, ob),
                _ => false,
            }
        }),
        (Value::Obj(_), _) | (_, Value::Obj(_)) => false,
        (Value::Int(x), Value::Int(y)) => x == y,
        // `Double.equals`, NOT `==`. Kotlin has both, and they disagree on two
        // values: the boxed form compares `doubleToLongBits`, so `NaN` equals
        // itself and `0.0` does NOT equal `-0.0` — the exact reverse of the IEEE
        // comparison. Which one runs is decided statically: two `Double`-typed
        // operands under `==` reach `Op::NumEq` (IEEE) and never come here,
        // while `equals`, an `Any`-typed `==`, and every collection membership
        // test (`contains`, `indexOf`, `distinct`, `Set`/`Map` keys, `minus`,
        // `intersect`) route through this function. Answering IEEE here made
        // `listOf(NaN).contains(NaN)` false and `setOf(0.0, -0.0).size` 2 where
        // the JVM answers true and 1.
        (Value::Float(x), Value::Float(y)) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
        }
        (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
            a.to_float() == b.to_float()
        }
        (Value::Undef, Value::Undef) => true,
        _ => a == b,
    }
}

/// Referential identity — `===`, which asks whether two expressions denote the
/// SAME object rather than equal ones.
///
/// For a heap value that is the handle, full stop: `listOf(1, 2) === listOf(1,
/// 2)` is `false` where `==` is `true`, and a `data class`'s `equals` override
/// changes only the latter. `Char` rides in the reserved handle range and so
/// falls out of the same comparison, its handle being its value.
///
/// Everything else is unboxed here — kotlinrs has no `java.lang.Integer` — so
/// identity on a non-heap value is value equality. That agrees with the JVM
/// wherever a Kotlin program can see the answer without boxing: `1 === 1` is
/// `true` at declared `Int`, `null === null` is `true`, and a `String` literal
/// is interned so `"x" === "x"` is `true`. It DIVERGES on the two answers that
/// are artifacts of boxing rather than of the language — an `Any`-typed `1000`
/// (outside the `Integer` cache, so `false` on the JVM) and a `String` built at
/// run time (`false` on the JVM, since only constants are interned). Modelling
/// either would mean modelling the box, which nothing else in this runtime has.
fn identical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Obj(ia), Value::Obj(ib)) => ia == ib,
        (Value::Obj(_), _) | (_, Value::Obj(_)) => false,
        // Not `value_eq`: that one crosses widths, and `1 === 1.0` does not
        // even compile on Kotlin, so there is no cross-width answer to give.
        _ => a == b,
    }
}

/// The primary-constructor slice of an instance's field record — what every
/// `data class`-derived member reads. Clamped so a malformed record cannot
/// panic.
fn data_slice(fields: &[(String, Value)], from: usize, len: usize) -> &[(String, Value)] {
    let from = from.min(fields.len());
    let to = (from + len).min(fields.len());
    &fields[from..to]
}

/// Structural equality between two heap objects.
fn heap_eq(a: &HeapObj, b: &HeapObj) -> bool {
    match (a, b) {
        (
            HeapObj::Instance {
                class: ca,
                fields: fa,
                data_from: da,
                data_len: la,
                is_data: ia,
            },
            HeapObj::Instance {
                class: cb,
                fields: fb,
                data_from: db,
                data_len: lb,
                ..
            },
        ) => {
            // A class that declares NEITHER `equals` NOR `data` inherits
            // `Any.equals`, which is reference identity — two separately
            // constructed `Foo(1)` are NOT equal on the JVM. Distinct handles
            // are all that reach here (the same handle short-circuits in
            // `value_eq`), so the answer is `false`.
            //
            // A declared `equals` is answered by `equal_vm`, which needs the VM
            // to run the body; this VM-less path cannot, and reports the
            // identity its caller would otherwise get wrong in the other
            // direction.
            if !*ia {
                return false;
            }
            // A `data class`'s generated `equals` compares the primary-
            // constructor properties only, so neither an inherited field nor a
            // body property is part of the comparison even though the record
            // carries both.
            let (fa, fb) = (data_slice(fa, *da, *la), data_slice(fb, *db, *lb));
            ca == cb
                && fa.len() == fb.len()
                && fa.iter().zip(fb).all(|((_, x), (_, y))| value_eq(x, y))
        }
        (HeapObj::List(xa), HeapObj::List(xb)) => {
            xa.len() == xb.len() && xa.iter().zip(xb).all(|(x, y)| value_eq(x, y))
        }
        // A `Set`'s equality is order-INSENSITIVE (`setOf(1, 2) == setOf(2, 1)`),
        // unlike a `List`'s, and a Set never equals a List.
        (HeapObj::Set(xa), HeapObj::Set(xb)) => {
            xa.len() == xb.len() && xa.iter().all(|x| xb.iter().any(|y| value_eq(x, y)))
        }
        (HeapObj::Pair(a1, a2), HeapObj::Pair(b1, b2)) => value_eq(a1, b1) && value_eq(a2, b2),
        (HeapObj::Triple(a1, a2, a3), HeapObj::Triple(b1, b2, b3)) => {
            value_eq(a1, b1) && value_eq(a2, b2) && value_eq(a3, b3)
        }
        // An entry equals an entry, never a pair: `mapOf(1 to "a").entries
        // .first() == (1 to "a")` is `false` in Kotlin.
        (HeapObj::Entry(k1, v1), HeapObj::Entry(k2, v2)) => value_eq(k1, k2) && value_eq(v1, v2),
        (HeapObj::Map(ea), HeapObj::Map(eb)) => {
            ea.len() == eb.len()
                && ea
                    .iter()
                    .all(|(k, v)| eb.iter().any(|(k2, v2)| value_eq(k, k2) && value_eq(v, v2)))
        }
        // `IntRange`/`IntProgression` define structural `equals`; two empty
        // ranges are equal regardless of their endpoints, as in Kotlin.
        (HeapObj::Range(a), HeapObj::Range(b)) => {
            let (ea, eb) = (a.count() == 0, b.count() == 0);
            (ea && eb)
                || (!ea && !eb && a.first == b.first && a.last() == b.last() && a.step == b.step)
        }
        // A JVM array inherits `Object.equals`, i.e. reference identity —
        // `arrayOf(1) == arrayOf(1)` is `false`. Two handles reaching here are
        // distinct objects by construction, so this is always false.
        (HeapObj::Array { .. }, HeapObj::Array { .. }) => false,
        _ => false,
    }
}

/// Kotlin `Int.hashCode()` — the value itself — and `Long.hashCode()`, the
/// JVM's `(int)(v ^ (v >>> 32))` fold of the two 32-bit halves.
///
/// Every Kotlin integer is one `i64` at run time, so the two cannot be told
/// apart from the value alone. They only DISAGREE for a negative number inside
/// `Int` range (`(-1).hashCode()` is `-1`, `(-1L).hashCode()` is `0`): a
/// non-negative one folds against a zero high half, and one outside `Int` range
/// can only be a `Long`. `long` therefore carries the static answer wherever the
/// compiler had one — a direct `x.hashCode()` pushes its receiver width, and a
/// `data class` field consults its declared type — and falls back to the
/// magnitude, which is exact except in that one case.
fn int_hash(n: i64, long: bool) -> i32 {
    if long || i32::try_from(n).is_err() {
        (n ^ ((n as u64) >> 32) as i64) as i32
    } else {
        n as i32
    }
}

/// `String.hashCode()` — the JVM's `s[0]*31^(n-1) + s[1]*31^(n-2) + …` over
/// UTF-16 code units, so a non-BMP character contributes its surrogate pair.
fn string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(u as i32))
}

/// `Double.hashCode()` — the JVM folds the two halves of `doubleToLongBits`,
/// which canonicalizes every NaN to one bit pattern and keeps `-0.0` distinct
/// from `0.0`.
fn double_hash(f: f64) -> i32 {
    let bits = if f.is_nan() {
        0x7ff8_0000_0000_0000u64
    } else {
        f.to_bits()
    };
    (bits ^ (bits >> 32)) as u32 as i32
}

/// Kotlin's `hashCode()` for any value, following the JVM contract exactly so a
/// hash is reproducible across runs and matches the reference toolchain.
///
/// `long` resolves the `Int`/`Long` ambiguity for the RECEIVER only (see
/// [`int_hash`]); nested values fall back to the magnitude rule.
///
/// The identity-hashed kinds — a non-`data` class instance, an array, a
/// throwable, a lambda — inherit `Object.hashCode`, which is the JVM's
/// per-object identity value and is not reproducible in Kotlin either. They
/// answer their heap handle: deterministic within a run, and never equal for two
/// distinct objects, which is all the contract requires.
fn value_hash(v: &Value, long: bool) -> i32 {
    if let Some(code) = char_code(v) {
        return code as i32;
    }
    match v {
        Value::Undef => 0,
        Value::Bool(b) => {
            if *b {
                1231
            } else {
                1237
            }
        }
        Value::Int(n) => int_hash(*n, long),
        Value::Float(f) => double_hash(*f),
        Value::Str(s) => string_hash(s),
        Value::Obj(id) => obj_hash(v).unwrap_or(*id as i32),
        _ => 0,
    }
}

/// The `hashCode()` of a heap object, or `None` when the object is one of the
/// identity-hashed kinds (see [`value_hash`]).
fn obj_hash(recv: &Value) -> Option<i32> {
    let widths = instance_tag(recv).and_then(long_fields);
    with_obj(recv, |o| match o {
        // A `data class` hashes over exactly what its `equals` compares — its
        // own (primary-constructor) properties, folded `h = h * 31 + field`.
        // A declared-`Long` field takes the `Long` fold; `widths` carries the
        // per-property answer the compiler registered at class declaration.
        HeapObj::Instance {
            fields,
            data_from,
            data_len,
            is_data,
            ..
        } => {
            if !*is_data {
                return None;
            }
            let own = data_slice(fields, *data_from, *data_len);
            let mut h: i32 = 0;
            for (i, (_, v)) in own.iter().enumerate() {
                let long = widths
                    .as_ref()
                    .and_then(|w| w.as_bytes().get(i))
                    .is_some_and(|c| *c == b'l');
                let e = value_hash(v, long);
                h = if i == 0 {
                    e
                } else {
                    h.wrapping_mul(31).wrapping_add(e)
                };
            }
            Some(h)
        }
        // `List.hashCode()` seeds at 1 and folds `h = h * 31 + element`.
        HeapObj::List(items) => Some(items.iter().fold(1i32, |h, v| {
            h.wrapping_mul(31).wrapping_add(value_hash(v, false))
        })),
        // A `Set` hashes order-independently: the SUM of its element hashes, so
        // two equal sets built in different insertion orders agree.
        HeapObj::Set(items) => Some(
            items
                .iter()
                .fold(0i32, |h, v| h.wrapping_add(value_hash(v, false))),
        ),
        // A `Map` sums its entry hashes, and an entry's is `key ^ value`.
        HeapObj::Map(entries) => Some(entries.iter().fold(0i32, |h, (k, v)| {
            h.wrapping_add(value_hash(k, false) ^ value_hash(v, false))
        })),
        // A `by lazy` cell inherits `Object.hashCode` — identity, which
        // `value_hash` reports by answering `None`.
        HeapObj::Lazy { .. } | HeapObj::Res { .. } => None,
        // `Map.Entry.hashCode()` is `key ^ value`; a `Pair` is a `data class`,
        // so it folds like one.
        HeapObj::Entry(k, v) => Some(value_hash(k, false) ^ value_hash(v, false)),
        HeapObj::Pair(a, b) => Some(
            value_hash(a, false)
                .wrapping_mul(31)
                .wrapping_add(value_hash(b, false)),
        ),
        // A `Triple` folds like the three-property `data class` it is.
        HeapObj::Triple(a, b, c) => Some(
            value_hash(a, false)
                .wrapping_mul(31)
                .wrapping_add(value_hash(b, false))
                .wrapping_mul(31)
                .wrapping_add(value_hash(c, false)),
        ),
        // An empty progression hashes to -1. `IntRange` folds `31 * first +
        // last`; `IntProgression` adds the step, `31 * (31 * first + last) +
        // step`. `last` is the last ELEMENT, not the written endpoint.
        HeapObj::Range(r) => Some(if r.count() == 0 {
            -1
        } else {
            let base = (r.first as i32)
                .wrapping_mul(31)
                .wrapping_add(r.last() as i32);
            if r.progression {
                base.wrapping_mul(31).wrapping_add(r.step as i32)
            } else {
                base
            }
        }),
        // Identity-hashed: `Object.hashCode`.
        HeapObj::Array { .. }
        | HeapObj::Builder { .. }
        | HeapObj::Closure { .. }
        | HeapObj::Comparator(_)
        | HeapObj::Gen { .. }
        | HeapObj::Grouping { .. }
        | HeapObj::Exc { .. } => None,
    })
    .flatten()
}

/// A coarse label for a heap object, for `unresolved reference` diagnostics.
fn obj_label(recv: &Value) -> String {
    if matches!(recv, Value::Str(_)) {
        return "String".to_string();
    }
    if is_char(recv) {
        return "Char".to_string();
    }
    with_obj(recv, |o| match o {
        HeapObj::Instance { class, .. } => class.clone(),
        HeapObj::List(_) => "List".to_string(),
        HeapObj::Set(_) => "Set".to_string(),
        HeapObj::Map(_) => "Map".to_string(),
        HeapObj::Pair(_, _) => "Pair".to_string(),
        HeapObj::Triple(_, _, _) => "Triple".to_string(),
        HeapObj::Lazy { .. } => "Lazy".to_string(),
        HeapObj::Res { .. } => "Result".to_string(),
        HeapObj::Entry(_, _) => "Map.Entry".to_string(),
        HeapObj::Closure { .. } => "Function".to_string(),
        HeapObj::Comparator(_) => "Comparator".to_string(),
        HeapObj::Gen { .. } => "Sequence".to_string(),
        HeapObj::Grouping { .. } => "Grouping".to_string(),
        HeapObj::Range(r) => match (r.is_char, r.progression) {
            (true, true) => "CharProgression",
            (true, false) => "CharRange",
            (false, true) => "IntProgression",
            (false, false) => "IntRange",
        }
        .to_string(),
        HeapObj::Array { .. } => "Array".to_string(),
        HeapObj::Builder { .. } => "StringBuilder".to_string(),
        HeapObj::Exc { class, .. } => class.rsplit('.').next().unwrap_or(class).to_string(),
    })
    .unwrap_or_else(|| "value".to_string())
}

/// Kotlin display form for a heap object — `data` class `C(x=1, y=2)`, a plain
/// class `C@<hash>`, a `List` `[a, b]`, a `Map` `{k=v, …}`, a `Pair` `(a, b)`.
fn display_obj(id: u32) -> String {
    HEAP.with(|h| {
        let h = h.borrow();
        let Some(o) = h.get(id as usize) else {
            return "null".to_string();
        };
        match o {
            HeapObj::Instance {
                class,
                is_data,
                fields,
                data_from,
                data_len,
            } => {
                if *is_data {
                    // Only the primary-constructor properties, which is what
                    // Kotlin's generated `toString` renders. Guarded like a
                    // container: `data class D(var next: Any?)` holding itself
                    // recurses here, and the JVM's generated `toString` has no
                    // self-check — it raises `StackOverflowError`, which is what
                    // `RENDER_DEPTH_LIMIT` delivers.
                    let me = Value::Obj(id);
                    rendering(&me, || {
                        let body = data_slice(fields, *data_from, *data_len)
                            .iter()
                            .map(|(n, v)| format!("{n}={}", kotlin_string(v)))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{class}({body})")
                    })
                    .unwrap_or_default()
                } else if is_enum_class(class) {
                    // `Enum.toString()` is the constant's name.
                    match fields.iter().find(|(n, _)| n == "name") {
                        Some((_, v)) => kotlin_string(v),
                        None => class.clone(),
                    }
                } else if type_is_throwable(class) {
                    // A user class extending a built-in throwable inherits
                    // `Throwable.toString()`, not `Object`'s identity form.
                    match fields.iter().find(|(n, _)| n == "message") {
                        Some((_, Value::Undef)) | None => class.clone(),
                        Some((_, m)) => format!("{class}: {}", kotlin_string(m)),
                    }
                } else {
                    format!("{class}@{id:x}")
                }
            }
            // A `Set` prints exactly like a `List` in Kotlin — square brackets,
            // elements in iteration (insertion) order.
            HeapObj::List(items) | HeapObj::Set(items) => {
                let me = Value::Obj(id);
                rendering(&me, || {
                    let body = items
                        .iter()
                        .map(|x| match self_reference(x, false) {
                            Some(s) => s.to_string(),
                            None => kotlin_string(x),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("[{body}]")
                })
                .unwrap_or_default()
            }
            HeapObj::Map(entries) => {
                let me = Value::Obj(id);
                rendering(&me, || {
                    let body = entries
                        .iter()
                        .map(|(k, v)| {
                            let ks = match self_reference(k, true) {
                                Some(s) => s.to_string(),
                                None => kotlin_string(k),
                            };
                            let vs = match self_reference(v, true) {
                                Some(s) => s.to_string(),
                                None => kotlin_string(v),
                            };
                            format!("{ks}={vs}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{{{body}}}")
                })
                .unwrap_or_default()
            }
            HeapObj::Pair(a, b) => format!("({}, {})", kotlin_string(a), kotlin_string(b)),
            // Kotlin's `Lazy.toString()` reports whether the value has been
            // computed; a forced cell renders as the value it holds.
            // Kotlin renders a `Result` as `Success(v)` / `Failure(<throwable>)`.
            HeapObj::Res { value, err } => match err {
                Some(e) => format!("Failure({})", throwable_str(e)),
                None => format!("Success({})", kotlin_string(value)),
            },
            HeapObj::Lazy { value, .. } => match value {
                Some(v) => kotlin_string(v),
                None => "Lazy value not initialized yet.".to_string(),
            },
            HeapObj::Triple(a, b, c) => format!(
                "({}, {}, {})",
                kotlin_string(a),
                kotlin_string(b),
                kotlin_string(c)
            ),
            // `Map.Entry.toString()` is `key=value` — the form a printed `Map`
            // is built from, and NOT the pair's `(key, value)`.
            HeapObj::Entry(k, v) => format!("{}={}", kotlin_string(k), kotlin_string(v)),
            // Kotlin renders a lambda as an opaque `Function` reference; the exact
            // JVM form is `(kotlin.jvm.functions.FunctionN)…`, which we don't
            // reproduce — a stable placeholder is enough (lambdas are rarely
            // printed, only invoked).
            HeapObj::Closure { params, .. } => format!("(lambda arity={params})"),
            // A `Comparator` is an anonymous JVM object with no printed form
            // worth reproducing; like a lambda it exists to be invoked.
            HeapObj::Comparator(keys) => format!("(comparator keys={})", keys.len()),
            // A lazy sequence has no printed form worth reproducing either —
            // the JVM renders it as `kotlin.sequences.GeneratorSequence@…`,
            // an identity hash. It exists to be consumed, not displayed.
            HeapObj::Gen { stages, .. } => format!("(sequence stages={})", stages.len()),
            // A `Grouping` is an anonymous object on the JVM, so it has no
            // meaningful printed form; it exists to be consumed by a terminal
            // operation, not displayed.
            HeapObj::Grouping { items, .. } => format!("(grouping size={})", items.len()),
            // `IntRange.toString` is `first..last`; `IntProgression.toString` is
            // `first..last step n` ascending and `first downTo last step n`
            // descending, where `last` is the last element actually reached
            // (`1..10 step 2` prints `1..9 step 2`).
            HeapObj::Range(r) => {
                // A `CharRange` prints its endpoints as characters (`a..e`);
                // the `step` count stays a number in both.
                let (first, last, end) = (
                    kotlin_string(&r.wrap(r.first)),
                    kotlin_string(&r.wrap(r.last())),
                    kotlin_string(&r.wrap(r.end)),
                );
                if !r.progression {
                    format!("{first}..{end}")
                } else if r.step > 0 {
                    format!("{first}..{last} step {}", r.step)
                } else {
                    format!("{first} downTo {last} step {}", -r.step)
                }
            }
            // An array inherits `Object.toString`: its JVM type descriptor and
            // identity hash. The hash is the JVM's per-run object address, which
            // no reimplementation can reproduce — the heap handle stands in, so
            // the SHAPE matches (`[I@1b6d3586`) but the digits do not.
            HeapObj::Array { desc, .. } => format!("{desc}@{id:x}"),
            // `StringBuilder.toString()` is its content, which is why an
            // interpolated or printed builder reads as plain text.
            HeapObj::Builder { units, .. } => utf16_to_string(units),
            // `Throwable.toString()`: the qualified class name, plus `": " +
            // message` when the constructor was given one.
            HeapObj::Exc { class, msg } => match msg {
                Some(m) => format!("{class}: {m}"),
                None => class.clone(),
            },
        }
    })
}

/// Whether `v`'s runtime kind matches the Kotlin type name `ty` — backs
/// `when`'s `is Type` check.
/// The JVM binary name of the class `ty` names, and whether it lives in
/// `java.base`.
///
/// `None` for a name kotlinrs cannot resolve to ONE JVM class. A Kotlin `List`
/// is the case that matters: `listOf(1)` is a
/// `java.util.Collections$SingletonList`, `listOf(1, 2)` an
/// `java.util.Arrays$ArrayList` and `mutableListOf()` a `java.util.ArrayList`,
/// and one `HeapObj::List` carries all three — so any single name would be
/// right for a third of the receivers and confidently wrong for the rest.
fn jvm_class(ty: &str) -> Option<(&'static str, bool)> {
    Some(match ty {
        "Int" => ("java.lang.Integer", true),
        "Long" => ("java.lang.Long", true),
        "Short" => ("java.lang.Short", true),
        "Byte" => ("java.lang.Byte", true),
        "Double" => ("java.lang.Double", true),
        "Float" => ("java.lang.Float", true),
        "Boolean" => ("java.lang.Boolean", true),
        "Char" => ("java.lang.Character", true),
        "String" => ("java.lang.String", true),
        "StringBuilder" => ("java.lang.StringBuilder", true),
        _ => return None,
    })
}

/// The JVM binary name of the class a runtime VALUE is, and whether it lives in
/// `java.base`.
///
/// [`jvm_class`] answers for a type NAME, which cannot resolve a container:
/// `List` is three different JVM classes depending on how the handle was built.
/// A VALUE can be resolved, because the provenance those classes turn on is
/// already recorded — [`ListImpl`] for lists, the descriptor for arrays — and
/// the rest are one class each. Naming them is not invention: every row below
/// was read off `kotlinc` 2.4.10 / JDK 21.0.12, and the short Kotlin labels
/// this replaces (`class List cannot be cast to class String`) are a spelling
/// no JVM produces.
///
/// `Set` and `Map` are deliberately absent. Their class turns on the same
/// literal-versus-mutable provenance a list's does — `setOf()` is a
/// `kotlin.collections.EmptySet` where `mutableSetOf()` is a
/// `java.util.LinkedHashSet` — and kotlinrs records no such tag for them, so
/// there is nothing here to read. They keep the approximate label rather than
/// take a guess that would be wrong for half the receivers.
fn runtime_jvm_class(v: &Value) -> Option<(String, bool)> {
    let len = with_obj(v, |o| match o {
        HeapObj::List(items) => Some(items.len()),
        _ => None,
    })
    .flatten();
    let impl_tag = match v {
        Value::Obj(id) => LIST_IMPL.with(|t| t.borrow().get(id).copied()),
        _ => None,
    };
    with_obj(v, |o| match o {
        // An array's descriptor IS its binary name, and every array class is in
        // `java.base`.
        HeapObj::Array { desc, .. } => Some((desc.clone(), true)),
        HeapObj::List(_) => {
            let n = len.unwrap_or(0);
            Some(match (impl_tag, n) {
                (Some(ListImpl::Builder), _) => {
                    ("kotlin.collections.builders.ListBuilder".to_string(), false)
                }
                // `optimizeReadOnlyList` collapses every read-only builder at
                // the two small sizes, exactly as the index diagnostics do.
                (Some(_), 0) => ("kotlin.collections.EmptyList".to_string(), false),
                (Some(_), 1) => ("java.util.Collections$SingletonList".to_string(), true),
                (Some(ListImpl::Literal | ListImpl::Sorted), _) => {
                    ("java.util.Arrays$ArrayList".to_string(), true)
                }
                // A read-only member's own list, and every untagged handle.
                _ => ("java.util.ArrayList".to_string(), true),
            })
        }
        HeapObj::Pair(_, _) => Some(("kotlin.Pair".to_string(), false)),
        HeapObj::Triple(_, _, _) => Some(("kotlin.Triple".to_string(), false)),
        HeapObj::Range(r) => Some((
            match (r.is_char, r.progression) {
                (true, true) => "kotlin.ranges.CharProgression",
                (true, false) => "kotlin.ranges.CharRange",
                (false, true) => "kotlin.ranges.IntProgression",
                (false, false) => "kotlin.ranges.IntRange",
            }
            .to_string(),
            false,
        )),
        _ => None,
    })
    .flatten()
}

/// Where a class lives, as the JVM's `ClassCastException` spells it.
fn class_home(java_base: bool) -> &'static str {
    if java_base {
        "module java.base of loader 'bootstrap'"
    } else {
        "unnamed module of loader 'app'"
    }
}

/// The `ClassCastException` a failed `as` raises.
///
/// The JVM names both classes in FULL and appends where each one lives, and it
/// merges the two locations when they agree — three message shapes, not one.
/// Falls back to the short Kotlin labels when either side is a kind kotlinrs
/// cannot name exactly (see [`jvm_class`]); inventing a `java.util` name there
/// would trade a visibly approximate message for an invisibly wrong one.
fn cast_failure(v: &Value, ty: &str) -> String {
    let from_label = obj_label(v);
    // The receiver is nameable when it is a primitive/`String` box or a user
    // instance; the TARGET when it is one of those names or a name that is not
    // a builtin container at all, which is what `value_is_type` treats as a
    // user class.
    let is_instance = with_obj(v, |o| matches!(o, HeapObj::Instance { .. })).unwrap_or(false);
    // A primitive's BOX is the class the JVM reports, and `obj_label` has no
    // name for one — it answers the placeholder `value`. Kotlin's `Int`, `Long`,
    // `Byte` and `Short` all share `Value::Int` here, so the box named is the
    // one the majority of them are: an autoboxed literal is an `Integer`.
    let boxed = match v {
        Value::Int(_) => jvm_class("Int"),
        Value::Float(_) => jvm_class("Double"),
        Value::Bool(_) => jvm_class("Boolean"),
        _ => None,
    };
    // `obj_label` first: it already names `String` and `Char`, and a `Char` is
    // carried as an `Int` here, so the box table below would claim it.
    let recv = jvm_class(&from_label)
        .or(boxed)
        .map(|(n, b)| (n.to_string(), b))
        .or_else(|| runtime_jvm_class(v))
        .or_else(|| is_instance.then(|| (from_label.clone(), false)));
    let target = jvm_class(ty)
        .map(|(n, b)| (n.to_string(), b))
        .or_else(|| (!is_builtin_type_name(ty)).then(|| (ty.to_string(), false)));
    let (Some((a, a_base)), Some((b, b_base))) = (recv, target) else {
        return format!(
            "java.lang.ClassCastException: class {from_label} cannot be cast to class {ty}"
        );
    };
    let home = if a_base == b_base {
        format!("{a} and {b} are in {}", class_home(a_base))
    } else {
        format!(
            "{a} is in {}; {b} is in {}",
            class_home(a_base),
            class_home(b_base)
        )
    };
    format!("java.lang.ClassCastException: class {a} cannot be cast to class {b} ({home})")
}

/// Whether `ty` is a name [`value_is_type`] answers WITHOUT consulting the user
/// class registry — the builtin containers and the type names that stand for a
/// whole family. Their JVM class is not fixed, so a failed cast to one of them
/// keeps the short Kotlin label.
fn is_builtin_type_name(ty: &str) -> bool {
    matches!(
        ty,
        "Any"
            | "Unit"
            | "Nothing"
            | "Number"
            | "Comparable"
            | "CharSequence"
            | "StringBuffer"
            | "Appendable"
            | "List"
            | "MutableList"
            | "Iterable"
            | "Collection"
            | "Set"
            | "MutableSet"
            | "Map"
            | "MutableMap"
            | "Pair"
            | "Triple"
            | "Array"
            | "IntArray"
            | "DoubleArray"
            | "CharArray"
            | "BooleanArray"
    )
}

fn value_is_type(v: &Value, ty: &str) -> bool {
    match ty {
        // A `Char` has its own runtime representation, so `is Char` and `is Int`
        // answer for exactly one value kind each.
        "Char" => is_char(v),
        "Int" | "Long" | "Byte" | "Short" => matches!(v, Value::Int(_)),
        "Double" | "Float" => matches!(v, Value::Float(_)),
        "Boolean" => matches!(v, Value::Bool(_)),
        "String" => matches!(v, Value::Str(_)),
        // A `StringBuilder` is a `CharSequence` alongside `String`, but neither
        // is the other: `sb is String` and `"x" is StringBuilder` are both
        // false on the JVM. `Appendable` is the builder's alone.
        "CharSequence" => {
            matches!(v, Value::Str(_))
                || with_obj(v, |o| matches!(o, HeapObj::Builder { .. })).unwrap_or(false)
        }
        "StringBuilder" | "StringBuffer" | "Appendable" => {
            with_obj(v, |o| matches!(o, HeapObj::Builder { .. })).unwrap_or(false)
        }
        // `Any` matches any non-null value; unknown names never match.
        "Any" => !matches!(v, Value::Undef),
        // The built-in container types. Type arguments are erased on the JVM,
        // so `is List<String>` can only ever test the container kind — which is
        // why the parser drops them.
        //
        // A `Set` is a `Collection` but NOT a `List`, so the two names cannot
        // share an arm: `setOf(1) is List<*>` is `false` in Kotlin.
        "List" | "MutableList" => with_obj(v, |o| matches!(o, HeapObj::List(_))).unwrap_or(false),
        "Iterable" | "Collection" => with_obj(v, |o| {
            matches!(o, HeapObj::List(_) | HeapObj::Set(_) | HeapObj::Range(_))
        })
        .unwrap_or(false),
        "Set" | "MutableSet" => with_obj(v, |o| matches!(o, HeapObj::Set(_))).unwrap_or(false),
        "Map" | "MutableMap" => with_obj(v, |o| matches!(o, HeapObj::Map(_))).unwrap_or(false),
        "Pair" => with_obj(v, |o| matches!(o, HeapObj::Pair(_, _))).unwrap_or(false),
        "Triple" => with_obj(v, |o| matches!(o, HeapObj::Triple(_, _, _))).unwrap_or(false),
        "Array" | "IntArray" | "DoubleArray" | "CharArray" | "BooleanArray" => {
            with_obj(v, |o| matches!(o, HeapObj::Array { .. })).unwrap_or(false)
        }
        other => {
            // A class instance matches its own class and every supertype it
            // registered — the `is Dog` / `is Animal` / `is Greeter` chain.
            let inst = with_obj(v, |o| match o {
                HeapObj::Instance { class, .. } => Some(class.clone()),
                _ => None,
            })
            .flatten();
            if let Some(class) = inst {
                return type_is_a(&class, other);
            }
            // A built-in throwable matches its own class and every supertype it
            // reaches, so `when (e) { is RuntimeException -> … }` behaves like a
            // `catch` arm.
            match thrown_class(v) {
                Some(c) => other == "Throwable" || throwable_is_a(&c, other),
                None => false,
            }
        }
    }
}

/// A coarse Kotlin type label for `recv`, for the `unresolved reference`
/// diagnostic. Not a full type name — just enough to identify the receiver kind.
fn type_label(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "Boolean",
        Value::Int(_) => "Int",
        Value::Float(_) => "Double",
        Value::Str(_) => "String",
        _ if is_char(v) => "Char",
        _ => "value",
    }
}

/// Kotlin `Any?.toString()` for the value kinds kotlinrs produces.
pub fn kotlin_string(v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format_double(*f),
        Value::Str(s) => s.to_string(),
        // Kotlin `null` (carried as `Undef`) stringifies to `null` in
        // interpolation / `println`. `Unit` is displayed statically by the
        // compiler (it emits the literal `kotlin.Unit`), so it never reaches
        // here as an `Undef`.
        Value::Undef => "null".to_string(),
        // A `Char` is a handle in the reserved region, not a heap object, so it
        // renders as its character wherever a value is displayed — including
        // inside a printed `List`/`Set`/`Map`, which is what `Char`-as-an-`Int`
        // could not do.
        Value::Obj(id) => match char_code(v) {
            Some(code) => char_string(code),
            None => display_obj(*id),
        },
        other => other.to_str(),
    }
}

/// Kotlin `Double.toString()`: shortest round-trip, whole values keep a trailing
/// `.0`, and the non-finite forms are `NaN` / `Infinity` / `-Infinity`.
///
/// Kotlin delegates to the JVM's `Double.toString`, which uses plain decimal
/// only inside `[1e-3, 1e7)` and switches to "computerized scientific notation"
/// outside that range. Rust's `{}` never switches, so the range test has to be
/// explicit or large and small magnitudes print as long digit strings
/// (`25000000.0` where Kotlin says `2.5E7`, `0.0009999` where it says `9.999E-4`).
pub fn format_double(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if f == 0.0 {
        // The signed zeroes are distinguished: `-0.0` prints with its sign.
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }

    let mag = f.abs();
    if (1e-3..1e7).contains(&mag) {
        let s = format!("{f}");
        return if s.contains('.') { s } else { format!("{s}.0") };
    }

    // Scientific form. Rust renders `2.5e7` / `1e7`; the JVM wants `2.5E7` /
    // `1.0E7` — uppercase exponent, no `+`, and a mantissa that always carries a
    // fractional digit.
    let s = format!("{f:e}");
    let (mantissa, exp) = match s.split_once('e') {
        Some((m, e)) => (m, e),
        None => return s,
    };
    let mantissa = if mantissa.contains('.') {
        mantissa.to_string()
    } else {
        format!("{mantissa}.0")
    };
    format!("{mantissa}E{exp}")
}
