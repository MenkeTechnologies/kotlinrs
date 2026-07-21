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

thread_local! {
    /// Set by a runtime fault (e.g. integer divide-by-zero) so the CLI can
    /// report it as an uncaught exception after `VM::run` returns.
    static KT_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
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
        KT_TO_STRING => {
            let v = vm.pop();
            vm.push(Value::str(kotlin_string(&v)));
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
    vm.set_extension_handler(Box::new(handle_coercion));
}

/// Register the debug extension handler on a fresh VM (`kotlin --dap`). Identical
/// to [`install`] for the value coercions, but a `KT_DBG_LINE` marker fires the
/// DAP line hook (breakpoint / step check) instead of being ignored.
pub fn install_debug(vm: &mut VM) {
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
    match (recv, name) {
        // ── kotlin.String ──
        (Value::Str(s), "length") => Ok(Value::Int(s.encode_utf16().count() as i64)),
        (Value::Str(s), "uppercase" | "toUpperCase") => Ok(Value::str(s.to_uppercase())),
        (Value::Str(s), "lowercase" | "toLowerCase") => Ok(Value::str(s.to_lowercase())),
        (Value::Str(s), "trim") => Ok(Value::str(s.trim().to_string())),
        (Value::Str(s), "isEmpty") => Ok(Value::Bool(s.is_empty())),
        (Value::Str(s), "isNotEmpty") => Ok(Value::Bool(!s.is_empty())),

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
        Value::Undef => "kotlin.Unit".to_string(),
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
