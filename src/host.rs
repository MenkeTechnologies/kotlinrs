//! Kotlin-specific runtime hooks reached through fusevm's extension-op
//! dispatch.
//!
//! fusevm's ops are language-agnostic, so three Kotlin behaviors the universal
//! ops can't express are handled here:
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

use fusevm::{Value, VM};
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
}

/// Clear the object heap. Called on every VM install so a fresh run starts with
/// no residual objects (handles are per-run identities).
fn reset_heap() {
    HEAP.with(|h| h.borrow_mut().clear());
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

fn fault(vm: &mut VM, msg: impl Into<String>) {
    KT_ERROR.with(|e| *e.borrow_mut() = Some(msg.into()));
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
            let code = vm.pop().to_int();
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
    vm.set_extension_handler(Box::new(handle_coercion));
}

/// Register the debug extension handler on a fresh VM (`kotlin --dap`). Identical
/// to [`install`] for the value coercions, but a `KT_DBG_LINE` marker fires the
/// DAP line hook (breakpoint / step check) instead of being ignored.
pub fn install_debug(vm: &mut VM) {
    reset_heap();
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

/// Dispatch a Kotlin stdlib member/method on `recv`. `Ok(value)` on success;
/// `Err(message)` for an unresolved member (surfaced as an uncaught exception,
/// matching Kotlin's compile-time `unresolved reference`).
///
/// Only the members faithfully backed here are handled — extend this table as
/// stdlib coverage grows. `String.length` counts UTF-16 code units, matching
/// the JVM `kotlin.String.length` contract (not Unicode scalar count).
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

        // ── kotlin.Char (carried as its integer code unit) ──
        // `Char.code` → the code unit as `Int`; `Int.toChar()` → a `Char` (the
        // low 16 bits). Both keep the same underlying integer value; the coarse
        // static type (Char vs Int) drives display, not the runtime tag.
        (Value::Int(n), "code") => Ok(Value::Int(*n)),
        (Value::Int(n), "toChar") => Ok(Value::Int(*n & 0xFFFF)),

        // ── kotlin.Any.toString() — defined on every type ──
        (_, "toString") => Ok(Value::str(kotlin_string(recv))),

        _ => {
            let _ = args; // reserved for arg-taking members
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

    // Read-only members.
    let res = with_obj(recv, |o| match (o, name) {
        // ── List ──
        (HeapObj::List(items), "size") => Some(Value::Int(items.len() as i64)),
        (HeapObj::List(items), "isEmpty") => Some(Value::Bool(items.is_empty())),
        (HeapObj::List(items), "isNotEmpty") => Some(Value::Bool(!items.is_empty())),
        (HeapObj::List(items), "first") => items.first().cloned(),
        (HeapObj::List(items), "last") => items.last().cloned(),
        (HeapObj::List(items), "get") => {
            let i = args.first().map(|v| v.to_int()).unwrap_or(0);
            usize::try_from(i).ok().and_then(|i| items.get(i).cloned())
        }
        (HeapObj::List(items), "contains") => Some(Value::Bool(
            args.first()
                .is_some_and(|a| items.iter().any(|v| value_eq(v, a))),
        )),
        (HeapObj::List(items), "indexOf") => Some(Value::Int(
            args.first()
                .and_then(|a| items.iter().position(|v| value_eq(v, a)))
                .map(|p| p as i64)
                .unwrap_or(-1),
        )),
        (HeapObj::List(items), "sum") => Some(sum_values(items)),
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

/// `componentN` for the ordered heap kinds (data-class field / list element /
/// pair half) — 1-based, as Kotlin destructuring uses.
fn component(recv: &Value, n: usize) -> Result<Value, String> {
    with_obj(recv, |o| match o {
        HeapObj::Instance { fields, .. } => fields.get(n - 1).map(|(_, v)| v.clone()),
        HeapObj::List(items) => items.get(n - 1).cloned(),
        HeapObj::Pair(a, b) => match n {
            1 => Some(a.clone()),
            2 => Some(b.clone()),
            _ => None,
        },
        HeapObj::Map(_) => None,
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
        HeapObj::List(items) => {
            let i = index.to_int();
            if i < 0 || i as usize >= items.len() {
                Err(format!(
                    "java.lang.IndexOutOfBoundsException: Index {i} out of bounds for length {}",
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
        HeapObj::List(items) => {
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
            HeapObj::List(items) => {
                for v in items {
                    kotlin_string(v).hash(&mut h);
                }
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
        }
        h.finish() as i64
    })
    .unwrap_or(0)
}

/// A coarse label for a heap object, for `unresolved reference` diagnostics.
fn obj_label(recv: &Value) -> String {
    with_obj(recv, |o| match o {
        HeapObj::Instance { class, .. } => class.clone(),
        HeapObj::List(_) => "List".to_string(),
        HeapObj::Map(_) => "Map".to_string(),
        HeapObj::Pair(_, _) => "Pair".to_string(),
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
        _ => false,
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

/// Kotlin `Double.toString()`: shortest round-trip, but whole values keep a
/// trailing `.0`, and the non-finite forms are `NaN` / `Infinity` / `-Infinity`.
pub fn format_double(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let s = format!("{f}");
    if s.bytes().any(|c| matches!(c, b'.' | b'e' | b'E')) {
        s
    } else {
        format!("{s}.0")
    }
}
