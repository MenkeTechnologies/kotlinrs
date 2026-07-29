//! Kotlin-specific runtime hooks reached through fusevm's extension-op
//! dispatch.
//!
//! fusevm's ops are language-agnostic, so the Kotlin behaviors the universal
//! ops can't express are handled here — the value coercions below, the
//! frontend-owned object heap ([`HeapObj`]), and the in-flight exception a VM
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

use fusevm::{Frame, VMResult, Value, VM};
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
    },
    List(Vec<Value>),
    /// Insertion-ordered key/value pairs (Kotlin `mapOf` preserves order).
    Map(Vec<(Value, Value)>),
    Pair(Value, Value),
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
    Exc { class: String, msg: Option<String> },
    /// A JVM array. `desc` is its JVM type descriptor (`"[I"`,
    /// `"[Ljava.lang.Integer;"`, …), which only exists to reproduce the
    /// `toString` form — arrays inherit `Object.toString`, so Kotlin prints them
    /// as `<descriptor>@<identity hash>`.
    Array {
        items: Vec<Value>,
        desc: String,
    },
}

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
}

impl RangeObj {
    fn new(first: i64, end: i64, form: RangeForm) -> RangeObj {
        match form {
            RangeForm::Inclusive => RangeObj {
                first,
                end,
                step: 1,
                progression: false,
            },
            // `until` is exclusive: it builds the `IntRange` `first..(end-1)`.
            // Kotlin guards the underflow by yielding an empty range instead of
            // wrapping past `Int.MIN_VALUE`.
            RangeForm::Until => RangeObj {
                first,
                end: end.wrapping_sub(1),
                step: 1,
                progression: false,
            },
            RangeForm::DownTo => RangeObj {
                first,
                end,
                step: -1,
                progression: true,
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
        (0..self.count()).map(|i| Value::Int(self.at(i))).collect()
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
//     ([`EXC_ENABLED`]). Every builtin with an observable side effect (printing,
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
/// [`EXC_ENABLED`]). Called by the runner after compiling and before `VM::run`.
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

/// The simple class name used for `catch` matching.
fn thrown_class(v: &Value) -> Option<String> {
    with_obj(v, |o| match o {
        HeapObj::Exc { class, .. } => Some(
            class
                .rsplit('.')
                .next()
                .unwrap_or(class.as_str())
                .to_string(),
        ),
        _ => None,
    })
    .flatten()
}

/// A throwable's `toString()`: `fqn` alone when the message is null, else
/// `fqn: message` (`java.lang.Throwable.toString`). A non-throwable value falls
/// back to its ordinary Kotlin display form.
fn throwable_str(v: &Value) -> String {
    with_obj(v, |o| match o {
        HeapObj::Exc { class, msg } => Some(match msg {
            Some(m) => format!("{class}: {m}"),
            None => class.clone(),
        }),
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
    let thrown = PENDING.with(|p| p.borrow().as_ref().and_then(thrown_class));
    Value::Bool(match thrown {
        // `catch (e: Throwable)` catches everything, including a value outside
        // the modeled hierarchy.
        Some(_) if want == "Throwable" => true,
        Some(c) => throwable_is_a(&c, &want),
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

/// `KT_ARRAY_INIT` — see [`KT_ARRAY_INIT`]. An empty `desc` means the generic
/// `Array(n) { … }`, whose descriptor comes from the produced elements.
fn b_array_init(vm: &mut VM, _argc: u8) -> Value {
    let clo = vm.pop();
    let desc = vm.pop().to_str();
    let n = vm.pop().to_int();
    if n < 0 {
        fault(vm, "java.lang.NegativeArraySizeException");
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
fn alloc(obj: HeapObj) -> Value {
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        let id = h.len() as u32;
        h.push(obj);
        Value::Obj(id)
    })
}

/// Run `f` with a shared borrow of heap object `id` (if the handle is live).
fn with_obj<T>(v: &Value, f: impl FnOnce(&HeapObj) -> T) -> Option<T> {
    let Value::Obj(id) = v else { return None };
    HEAP.with(|h| h.borrow().get(*id as usize).map(f))
}

/// Run `f` with a mutable borrow of heap object `id` (if the handle is live).
fn with_obj_mut<T>(v: &Value, f: impl FnOnce(&mut HeapObj) -> T) -> Option<T> {
    let Value::Obj(id) = v else { return None };
    HEAP.with(|h| h.borrow_mut().get_mut(*id as usize).map(f))
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
            match kt_method(&recv, &name, &args) {
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
            let fields: Vec<(String, Value)> = it.map(|s| s.to_string()).zip(vals).collect();
            vm.push(alloc(HeapObj::Instance {
                class,
                is_data,
                fields,
            }));
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
            let n = arg as usize;
            let mut vals = Vec::with_capacity(n);
            for _ in 0..n {
                vals.push(vm.pop());
            }
            vals.reverse();
            vm.push(alloc(HeapObj::List(vals)));
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
            match index_get(&recv, &index) {
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
            if let Err(e) = index_set(&recv, &index, value) {
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
            vm.push(Value::str(kotlin_string(&v)));
        }
        KT_IS => {
            // Stack: [value, typeName]; typeName on top.
            let ty = vm.pop().to_str();
            let v = vm.pop();
            vm.push(Value::Bool(value_is_type(&v, &ty)));
        }
        KT_CHR_STRING => {
            let v = vm.pop();
            // A `Char?` holding null renders as `null`, not as code point 0.
            if matches!(v, Value::Undef) {
                vm.push(Value::str("null"));
                return;
            }
            let code = v.to_int();
            let s = char::from_u32(code as u32)
                .map(|c| c.to_string())
                .unwrap_or_default();
            vm.push(Value::str(s));
        }
        KT_ISNULL => {
            let v = vm.pop();
            vm.push(Value::Bool(matches!(v, Value::Undef)));
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
            let end = vm.pop().to_int();
            let start = vm.pop().to_int();
            let form = match arg {
                1 => RangeForm::Until,
                2 => RangeForm::DownTo,
                _ => RangeForm::Inclusive,
            };
            vm.push(alloc(HeapObj::Range(RangeObj::new(start, end, form))));
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
                    first: r.first,
                    end: r.end,
                    step: if r.step < 0 { -n } else { n },
                    progression: true,
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
            vm.push(Value::Bool(contains_value(&container, &value)));
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
            let n = arg as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(vm.pop());
            }
            items.reverse();
            let desc = array_desc(&items);
            vm.push(alloc(HeapObj::Array { items, desc }));
        }
        KT_ARRAY_NEW => {
            let desc = vm.pop().to_str();
            let n = vm.pop().to_int();
            if n < 0 {
                fault(vm, "java.lang.NegativeArraySizeException");
                vm.push(Value::Undef);
            } else {
                // A primitive array is zero-filled: `0` for `[I`, `0.0` for `[D`,
                // `false` for `[Z` — matching JVM default initialization.
                let zero = match desc.as_str() {
                    "[D" => Value::Float(0.0),
                    "[Z" => Value::Bool(false),
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

/// Register the lambda builtins (`Op::CallBuiltin` dispatch). Shared by the
/// normal and debug installs. These live in the VM's `builtin_table`, which
/// survives the re-entrant `vm.run()` a lambda invocation drives — see the
/// builtin-id doc comments above.
fn register_builtins(vm: &mut VM) {
    vm.register_builtin(KT_MAKE_CLOSURE, b_make_closure);
    vm.register_builtin(KT_CLOSURE_CALL, b_closure_call);
    vm.register_builtin(KT_COLL_HOF, b_coll_hof);
    vm.register_builtin(KT_SCOPE_FN, b_scope_fn);
    vm.register_builtin(KT_ARRAY_INIT, b_array_init);
    vm.register_builtin(KT_PRINTLN, b_println);
    vm.register_builtin(KT_PRINT, b_print);
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
fn contains_value(container: &Value, value: &Value) -> bool {
    if let Value::Str(s) = container {
        return s.contains(&kotlin_string(value));
    }
    with_obj(container, |o| match o {
        HeapObj::Range(r) => r.contains(value.to_int()),
        HeapObj::List(items) | HeapObj::Array { items, .. } => {
            items.iter().any(|v| value_eq(v, value))
        }
        HeapObj::Map(entries) => entries.iter().any(|(k, _)| value_eq(k, value)),
        _ => false,
    })
    .unwrap_or(false)
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
        HeapObj::List(items) | HeapObj::Array { items, .. } => Some(items.len() as i64),
        HeapObj::Range(r) => Some(r.count()),
        _ => None,
    })
    .flatten()
}

/// Element `i` of an iterable. Only called with an `i` the loop already bounded
/// by [`iter_len`], so an out-of-range index can only mean the collection was
/// mutated mid-loop; that yields `null` rather than faulting.
fn iter_at(recv: &Value, i: i64) -> Value {
    // A String yields `Char`s, carried (as everywhere in kotlinrs) as their
    // integer code unit; the compiler types the loop variable `Char` so it
    // displays as a character.
    if let Value::Str(s) = recv {
        return usize::try_from(i)
            .ok()
            .and_then(|i| s.encode_utf16().nth(i))
            .map(|u| Value::Int(u as i64))
            .unwrap_or(Value::Undef);
    }
    with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Array { items, .. } => {
            usize::try_from(i).ok().and_then(|i| items.get(i).cloned())
        }
        HeapObj::Range(r) => Some(Value::Int(r.at(i))),
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
        Some(Value::Int(_)) => "java.lang.Integer",
        Some(Value::Float(_)) => "java.lang.Double",
        Some(Value::Str(_)) => "java.lang.String",
        Some(Value::Bool(_)) => "java.lang.Boolean",
        _ => "java.lang.Object",
    };
    let uniform = items.iter().all(|v| {
        matches!(
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
    let b = args.get(1).cloned().unwrap_or(Value::Int(0));
    let both_int = is_int(&a) && args.get(1).map(is_int).unwrap_or(true);
    match name {
        "abs" if is_int(&a) => Ok(Value::Int(a.to_int().wrapping_abs())),
        "abs" => Ok(Value::Float(a.to_float().abs())),
        "max" if both_int => Ok(Value::Int(a.to_int().max(b.to_int()))),
        "max" => Ok(Value::Float(a.to_float().max(b.to_float()))),
        "min" if both_int => Ok(Value::Int(a.to_int().min(b.to_int()))),
        "min" => Ok(Value::Float(a.to_float().min(b.to_float()))),
        "sqrt" => Ok(Value::Float(a.to_float().sqrt())),
        "floor" => Ok(Value::Float(a.to_float().floor())),
        "ceil" => Ok(Value::Float(a.to_float().ceil())),
        "round" => Ok(Value::Float(a.to_float().round_ties_even())),
        // `Math.round(Double)` → `Long`, half-up (i.e. `floor(x + 0.5)`).
        "jround" => Ok(Value::Int((a.to_float() + 0.5).floor() as i64)),
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
    let return_ip = vm.chunk.ops.len();
    vm.frames.push(Frame {
        return_ip,
        stack_base,
        slots: Vec::new(),
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
    match coll_hof(vm, &name, &recv, &extras, &clo) {
        Ok(v) => v,
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
        "let" => invoke_closure(vm, &clo, std::slice::from_ref(&recv)),
        // `also` — run the block for its side effect, yield the receiver.
        "also" => invoke_closure(vm, &clo, std::slice::from_ref(&recv)).map(|_| recv),
        // `takeIf` — yield the receiver when the predicate holds, else null.
        "takeIf" => invoke_closure(vm, &clo, std::slice::from_ref(&recv)).map(|p| {
            if truthy(&p) {
                recv
            } else {
                Value::Undef
            }
        }),
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
    with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Array { items, .. } => Some(items.clone()),
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
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => a
            .to_float()
            .partial_cmp(&b.to_float())
            .unwrap_or(Ordering::Equal),
    }
}

/// The higher-order collection methods, over a snapshot of `recv`'s elements,
/// invoking `clo` per element. Mirrors the Kotlin stdlib signatures faithfully.
fn coll_hof(
    vm: &mut VM,
    name: &str,
    recv: &Value,
    extras: &[Value],
    clo: &Value,
) -> Result<Value, String> {
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
            Ok(alloc(HeapObj::List(out)))
        }
        "forEach" => {
            for it in items {
                invoke_closure(vm, clo, &[it])?;
            }
            Ok(Value::Undef)
        }
        "fold" => {
            let mut acc = extras.first().cloned().unwrap_or(Value::Undef);
            for it in items {
                acc = invoke_closure(vm, clo, &[acc, it])?;
            }
            Ok(acc)
        }
        "reduce" => {
            let mut iter = items.into_iter();
            let mut acc = iter.next().ok_or_else(|| {
                "java.lang.UnsupportedOperationException: Empty collection can't be reduced."
                    .to_string()
            })?;
            for it in iter {
                acc = invoke_closure(vm, clo, &[acc, it])?;
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
        "maxByOrNull" => {
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
            Ok(best.map(|(el, _)| el).unwrap_or(Value::Undef))
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
            Ok(alloc(HeapObj::List(
                keyed.into_iter().map(|(_, it)| it).collect(),
            )))
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

/// UTF-16 offset of `needle` in `hay`, or -1 — matching `String.indexOf` and
/// the UTF-16 basis `length` already uses.
fn utf16_index_of(hay: &str, needle: &str) -> i64 {
    match hay.find(needle) {
        Some(byte_off) => hay[..byte_off].encode_utf16().count() as i64,
        None => -1,
    }
}

fn kt_method(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
    // Heap objects (List/Map/Pair/data-class members) dispatch through the heap.
    if let Value::Obj(_) = recv {
        return obj_method(recv, name, args);
    }
    match (recv, name) {
        // ── kotlin.String ──
        (Value::Str(s), "length") => Ok(Value::Int(s.encode_utf16().count() as i64)),
        (Value::Str(s), "uppercase" | "toUpperCase") => Ok(Value::str(s.to_uppercase())),
        (Value::Str(s), "lowercase" | "toLowerCase") => Ok(Value::str(s.to_lowercase())),
        (Value::Str(s), "trim") => Ok(Value::str(s.trim().to_string())),
        (Value::Str(s), "isEmpty") => Ok(Value::Bool(s.is_empty())),
        (Value::Str(s), "isNotEmpty") => Ok(Value::Bool(!s.is_empty())),
        (Value::Str(s), "isBlank") => Ok(Value::Bool(s.trim().is_empty())),
        (Value::Str(s), "isNotBlank") => Ok(Value::Bool(!s.trim().is_empty())),

        // Arg-taking `String` members. The argument is rendered through
        // `kotlin_string` so a `Char` (carried as an integer code unit) or a
        // number reads the way Kotlin would print it.
        (Value::Str(s), "contains") => Ok(Value::Bool(s.contains(&arg_str(args, 0)))),
        (Value::Str(s), "startsWith") => Ok(Value::Bool(s.starts_with(&arg_str(args, 0)))),
        (Value::Str(s), "endsWith") => Ok(Value::Bool(s.ends_with(&arg_str(args, 0)))),
        (Value::Str(s), "plus") => Ok(Value::str(format!("{s}{}", arg_str(args, 0)))),
        (Value::Str(s), "replace") => Ok(Value::str(
            s.replace(&arg_str(args, 0), &arg_str(args, 1)),
        )),
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
        (Value::Str(s), "indexOf") => Ok(Value::Int(utf16_index_of(s, &arg_str(args, 0)))),
        (Value::Str(s), "substring") => {
            let units: Vec<u16> = s.encode_utf16().collect();
            let start = args.first().map(|v| v.to_int()).unwrap_or(0);
            let end = args.get(1).map(|v| v.to_int()).unwrap_or(units.len() as i64);
            if start < 0 || end > units.len() as i64 || start > end {
                Err(format!(
                    "java.lang.StringIndexOutOfBoundsException: \
                     Range [{start}, {end}) out of bounds for length {}",
                    units.len()
                ))
            } else {
                Ok(Value::str(String::from_utf16_lossy(
                    &units[start as usize..end as usize],
                )))
            }
        }

        // ── kotlin.Char (carried as its integer code unit) ──
        // `Char.code` → the code unit as `Int`; `Int.toChar()` → a `Char` (the
        // low 16 bits). Both keep the same underlying integer value; the coarse
        // static type (Char vs Int) drives display, not the runtime tag.
        (Value::Int(n), "code") => Ok(Value::Int(*n)),
        (Value::Int(n), "toChar") => Ok(Value::Int(*n & 0xFFFF)),

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
        (Value::Int(n), "toDouble") => Ok(Value::Float(*n as f64)),
        (Value::Int(n), "toInt" | "toLong") => Ok(Value::Int(*n)),
        (Value::Float(f), "toDouble") => Ok(Value::Float(*f)),
        (Value::Float(f), "toInt" | "toLong") => Ok(Value::Int(*f as i64)),

        // ── kotlin.Any.toString() — defined on every type ──
        (_, "toString") => Ok(Value::str(kotlin_string(recv))),

        _ => {
            Err(format!(
                "unresolved reference: {name} on {}",
                type_label(recv)
            ))
        }
    }
}

/// Dispatch a member/method on a heap object (`List`/`Map`/`Pair`, or a `data`
/// class's synthesized members). User-defined class methods never reach here —
/// the compiler lowers those to direct `Op::Call`s on method subs.
fn obj_method(recv: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
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
        "hashCode" => return Ok(Value::Int(obj_hash(recv))),
        "equals" => return Ok(Value::Bool(args.first().is_some_and(|o| value_eq(recv, o)))),
        _ => {}
    }

    // Mutating list operations need a mutable borrow.
    match name {
        "add" => {
            let v = args.first().cloned().unwrap_or(Value::Undef);
            let ok = with_obj_mut(recv, |o| match o {
                HeapObj::List(items) => {
                    items.push(v);
                    true
                }
                _ => false,
            })
            .unwrap_or(false);
            return if ok {
                Ok(Value::Bool(true))
            } else {
                Err(format!("unresolved reference: add on {}", obj_label(recv)))
            };
        }
        "removeAt" => {
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            let out = with_obj_mut(recv, |o| match o {
                HeapObj::List(items) if i >= 0 && (i as usize) < items.len() => {
                    Some(items.remove(i as usize))
                }
                _ => None,
            })
            .flatten();
            return out.ok_or_else(|| "java.lang.IndexOutOfBoundsException".to_string());
        }
        "keys" | "values" => {
            // Snapshot the entries under a shared borrow, then allocate the
            // result list separately (allocating inside `with_obj` would re-borrow
            // the heap).
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
                Some(items) => Ok(alloc(HeapObj::List(items))),
                None => Err(format!(
                    "unresolved reference: {name} on {}",
                    obj_label(recv)
                )),
            };
        }
        "put" => {
            // Map.put(k, v) → previous value or null.
            let k = args.first().cloned().unwrap_or(Value::Undef);
            let v = args.get(1).cloned().unwrap_or(Value::Undef);
            let prev = with_obj_mut(recv, |o| match o {
                HeapObj::Map(entries) => {
                    if let Some(slot) = entries.iter_mut().find(|(ek, _)| value_eq(ek, &k)) {
                        Some(std::mem::replace(&mut slot.1, v))
                    } else {
                        entries.push((k, v));
                        None
                    }
                }
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
    let kind = with_obj(recv, |o| match o {
        HeapObj::List(_) => 1u8,
        HeapObj::Array { .. } => 2,
        HeapObj::Range(_) => 3,
        _ => 0,
    })
    .unwrap_or(0);
    if kind != 0 {
        let range = with_obj(recv, |o| match o {
            HeapObj::Range(r) => Some(*r),
            _ => None,
        })
        .flatten();
        let items = list_snapshot(recv).unwrap_or_default();
        if let Some(v) = sequence_member(&items, range.map(|r| (r.first, r.last())), name, args) {
            return v;
        }
    }

    // Read-only members.
    let res = with_obj(recv, |o| match (o, name) {
        // ── Map ──
        (HeapObj::Map(entries), "size") => Some(Value::Int(entries.len() as i64)),
        (HeapObj::Map(entries), "isEmpty") => Some(Value::Bool(entries.is_empty())),
        (HeapObj::Map(entries), "isNotEmpty") => Some(Value::Bool(!entries.is_empty())),
        (HeapObj::Map(entries), "containsKey") => {
            Some(Value::Bool(args.first().is_some_and(|k| {
                entries.iter().any(|(ek, _)| value_eq(ek, k))
            })))
        }
        (HeapObj::Map(entries), "get") => Some(
            args.first()
                .and_then(|k| entries.iter().find(|(ek, _)| value_eq(ek, k)))
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Undef),
        ),
        // ── Pair ──
        (HeapObj::Pair(a, _), "first") => Some(a.clone()),
        (HeapObj::Pair(_, b), "second") => Some(b.clone()),
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

/// The read-only members every ordered sequence shares — `List`, arrays, and
/// ranges. `range` carries `(first, last)` when the receiver is a range, whose
/// `first`/`last` are progression *properties* (defined even for an empty range)
/// rather than element accessors.
///
/// Returns `None` when `name` is not a sequence member at all, so the caller can
/// keep looking (a `Map`/`Pair`/instance member, or the unresolved-reference
/// diagnostic).
fn sequence_member(
    items: &[Value],
    range: Option<(i64, i64)>,
    name: &str,
    args: &[Value],
) -> Option<Result<Value, String>> {
    /// Kotlin throws on `first()`/`last()`/`max()`/`min()` over an empty
    /// sequence rather than returning null.
    fn need(v: Option<Value>) -> Result<Value, String> {
        v.ok_or_else(|| "java.util.NoSuchElementException: List is empty.".to_string())
    }
    let v = match name {
        "size" | "count" => Value::Int(items.len() as i64),
        "isEmpty" => Value::Bool(items.is_empty()),
        "isNotEmpty" => Value::Bool(!items.is_empty()),
        "first" => match range {
            Some((first, _)) => Value::Int(first),
            None => return Some(need(items.first().cloned())),
        },
        "last" => match range {
            Some((_, last)) => Value::Int(last),
            None => return Some(need(items.last().cloned())),
        },
        "get" => {
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            return Some(need(
                usize::try_from(i).ok().and_then(|i| items.get(i).cloned()),
            ));
        }
        "contains" => Value::Bool(
            args.first()
                .is_some_and(|a| items.iter().any(|v| value_eq(v, a))),
        ),
        "indexOf" => Value::Int(
            args.first()
                .and_then(|a| items.iter().position(|v| value_eq(v, a)))
                .map(|p| p as i64)
                .unwrap_or(-1),
        ),
        "sum" => sum_values(items),
        // An empty average is NaN in Kotlin, not an error.
        "average" => {
            Value::Float(items.iter().map(|v| v.to_float()).sum::<f64>() / items.len() as f64)
        }
        "max" | "min" => {
            let want_max = name == "max";
            return Some(need(items.iter().cloned().reduce(|a, b| {
                let take_b = (value_cmp(&b, &a) == std::cmp::Ordering::Greater) == want_max;
                if take_b {
                    b
                } else {
                    a
                }
            })));
        }
        "toList" | "toMutableList" | "toTypedArray" | "asList" => {
            return Some(Ok(alloc(HeapObj::List(items.to_vec()))))
        }
        // `joinToString(separator)` — the separator defaults to `", "`.
        "joinToString" => {
            let sep = match args.first() {
                Some(v) => kotlin_string(v),
                None => ", ".to_string(),
            };
            Value::str(
                items
                    .iter()
                    .map(kotlin_string)
                    .collect::<Vec<_>>()
                    .join(&sep),
            )
        }
        "reversed" => {
            // `IntRange.reversed()` is an `IntProgression` counting down; a
            // list's is a plain reversed list.
            return Some(Ok(match range {
                Some((first, last)) => alloc(HeapObj::Range(RangeObj {
                    first: last,
                    end: first,
                    step: -1,
                    progression: true,
                })),
                None => alloc(HeapObj::List(items.iter().rev().cloned().collect())),
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
        HeapObj::Instance { fields, .. } => fields.get(n - 1).map(|(_, v)| v.clone()),
        HeapObj::List(items) | HeapObj::Array { items, .. } => items.get(n - 1).cloned(),
        HeapObj::Pair(a, b) => match n {
            1 => Some(a.clone()),
            2 => Some(b.clone()),
            _ => None,
        },
        HeapObj::Map(_)
        | HeapObj::Closure { .. }
        | HeapObj::Range(_)
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
fn index_get(recv: &Value, index: &Value) -> Result<Value, String> {
    let out = with_obj(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Array { items, .. } => {
            let i = index.to_int();
            if i < 0 || i as usize >= items.len() {
                Err(format!(
                    "java.lang.ArrayIndexOutOfBoundsException: Index {i} out of bounds for length {}",
                    items.len()
                ))
            } else {
                Ok(items[i as usize].clone())
            }
        }
        // Map get returns null (Kotlin `V?`) when the key is absent.
        HeapObj::Map(entries) => Ok(entries
            .iter()
            .find(|(k, _)| value_eq(k, index))
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Undef)),
        _ => Err(format!("{} does not support indexing", obj_label(recv))),
    });
    out.unwrap_or_else(|| Err("indexing a non-object value".to_string()))
}

/// `recv[index] = value` — list set (bounds-checked) or map put.
fn index_set(recv: &Value, index: &Value, value: Value) -> Result<(), String> {
    let out = with_obj_mut(recv, |o| match o {
        HeapObj::List(items) | HeapObj::Array { items, .. } => {
            let i = index.to_int();
            if i < 0 || i as usize >= items.len() {
                Err("java.lang.IndexOutOfBoundsException".to_string())
            } else {
                items[i as usize] = value;
                Ok(())
            }
        }
        HeapObj::Map(entries) => {
            if let Some(slot) = entries.iter_mut().find(|(k, _)| value_eq(k, index)) {
                slot.1 = value;
            } else {
                entries.push((index.clone(), value));
            }
            Ok(())
        }
        _ => Err(format!(
            "{} does not support indexed assignment",
            obj_label(recv)
        )),
    });
    out.unwrap_or_else(|| Err("indexing a non-object value".to_string()))
}

/// Structural equality — `==` over heap objects (recursively) and value
/// equality over primitives. Ints and Doubles compare by numeric value.
pub fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Obj(_), Value::Obj(_)) => HEAP.with(|h| {
            let h = h.borrow();
            let (Value::Obj(ia), Value::Obj(ib)) = (a, b) else {
                return false;
            };
            // The same handle is the same object — this is what makes the
            // identity equality an array inherits from `Object` come out `true`
            // for `a == a` while `arrayOf(1) == arrayOf(1)` stays `false`.
            if ia == ib {
                return true;
            }
            match (h.get(*ia as usize), h.get(*ib as usize)) {
                (Some(oa), Some(ob)) => heap_eq(oa, ob),
                _ => false,
            }
        }),
        (Value::Obj(_), _) | (_, Value::Obj(_)) => false,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
            a.to_float() == b.to_float()
        }
        (Value::Undef, Value::Undef) => true,
        _ => a == b,
    }
}

/// Structural equality between two heap objects.
fn heap_eq(a: &HeapObj, b: &HeapObj) -> bool {
    match (a, b) {
        (
            HeapObj::Instance {
                class: ca,
                fields: fa,
                ..
            },
            HeapObj::Instance {
                class: cb,
                fields: fb,
                ..
            },
        ) => {
            ca == cb
                && fa.len() == fb.len()
                && fa.iter().zip(fb).all(|((_, x), (_, y))| value_eq(x, y))
        }
        (HeapObj::List(xa), HeapObj::List(xb)) => {
            xa.len() == xb.len() && xa.iter().zip(xb).all(|(x, y)| value_eq(x, y))
        }
        (HeapObj::Pair(a1, a2), HeapObj::Pair(b1, b2)) => value_eq(a1, b1) && value_eq(a2, b2),
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

/// A simple order-independent hash for a heap object (data-class `hashCode`).
fn obj_hash(recv: &Value) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    with_obj(recv, |o| {
        let mut h = DefaultHasher::new();
        match o {
            HeapObj::Instance { class, fields, .. } => {
                class.hash(&mut h);
                for (n, v) in fields {
                    n.hash(&mut h);
                    kotlin_string(v).hash(&mut h);
                }
            }
            HeapObj::List(items) | HeapObj::Array { items, .. } => {
                for v in items {
                    kotlin_string(v).hash(&mut h);
                }
            }
            HeapObj::Range(r) => {
                r.first.hash(&mut h);
                r.last().hash(&mut h);
                r.step.hash(&mut h);
            }
            HeapObj::Pair(a, b) => {
                kotlin_string(a).hash(&mut h);
                kotlin_string(b).hash(&mut h);
            }
            HeapObj::Map(entries) => {
                for (k, v) in entries {
                    kotlin_string(k).hash(&mut h);
                    kotlin_string(v).hash(&mut h);
                }
            }
            HeapObj::Closure { name_idx, .. } => name_idx.hash(&mut h),
            HeapObj::Exc { class, msg } => {
                class.hash(&mut h);
                msg.hash(&mut h);
            }
        }
        h.finish() as i64
    })
    .unwrap_or(0)
}

/// A coarse label for a heap object, for `unresolved reference` diagnostics.
fn obj_label(recv: &Value) -> String {
    if matches!(recv, Value::Str(_)) {
        return "String".to_string();
    }
    with_obj(recv, |o| match o {
        HeapObj::Instance { class, .. } => class.clone(),
        HeapObj::List(_) => "List".to_string(),
        HeapObj::Map(_) => "Map".to_string(),
        HeapObj::Pair(_, _) => "Pair".to_string(),
        HeapObj::Closure { .. } => "Function".to_string(),
        HeapObj::Range(r) => if r.progression {
            "IntProgression"
        } else {
            "IntRange"
        }
        .to_string(),
        HeapObj::Array { .. } => "Array".to_string(),
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
            } => {
                if *is_data {
                    let body = fields
                        .iter()
                        .map(|(n, v)| format!("{n}={}", kotlin_string(v)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{class}({body})")
                } else {
                    format!("{class}@{id:x}")
                }
            }
            HeapObj::List(items) => {
                let body = items
                    .iter()
                    .map(kotlin_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{body}]")
            }
            HeapObj::Map(entries) => {
                let body = entries
                    .iter()
                    .map(|(k, v)| format!("{}={}", kotlin_string(k), kotlin_string(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{{body}}}")
            }
            HeapObj::Pair(a, b) => format!("({}, {})", kotlin_string(a), kotlin_string(b)),
            // Kotlin renders a lambda as an opaque `Function` reference; the exact
            // JVM form is `(kotlin.jvm.functions.FunctionN)…`, which we don't
            // reproduce — a stable placeholder is enough (lambdas are rarely
            // printed, only invoked).
            HeapObj::Closure { params, .. } => format!("(lambda arity={params})"),
            // `IntRange.toString` is `first..last`; `IntProgression.toString` is
            // `first..last step n` ascending and `first downTo last step n`
            // descending, where `last` is the last element actually reached
            // (`1..10 step 2` prints `1..9 step 2`).
            HeapObj::Range(r) => {
                if !r.progression {
                    format!("{}..{}", r.first, r.end)
                } else if r.step > 0 {
                    format!("{}..{} step {}", r.first, r.last(), r.step)
                } else {
                    format!("{} downTo {} step {}", r.first, r.last(), -r.step)
                }
            }
            // An array inherits `Object.toString`: its JVM type descriptor and
            // identity hash. The hash is the JVM's per-run object address, which
            // no reimplementation can reproduce — the heap handle stands in, so
            // the SHAPE matches (`[I@1b6d3586`) but the digits do not.
            HeapObj::Array { desc, .. } => format!("{desc}@{id:x}"),
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
/// `when`'s `is Type` check. `Char` is carried as an `Int` at runtime and is
/// not distinguishable here, so `is Char` is treated as `is Int`.
fn value_is_type(v: &Value, ty: &str) -> bool {
    match ty {
        "Int" | "Long" | "Char" | "Byte" | "Short" => matches!(v, Value::Int(_)),
        "Double" | "Float" => matches!(v, Value::Float(_)),
        "Boolean" => matches!(v, Value::Bool(_)),
        "String" | "CharSequence" => matches!(v, Value::Str(_)),
        // `Any` matches any non-null value; unknown names never match.
        "Any" => !matches!(v, Value::Undef),
        // A throwable matches its own class and every supertype it reaches, so
        // `when (e) { is RuntimeException -> … }` behaves like a `catch` arm.
        other => match thrown_class(v) {
            Some(c) => other == "Throwable" || throwable_is_a(&c, other),
            None => false,
        },
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
        Value::Obj(id) => display_obj(*id),
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
