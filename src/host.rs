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
//! Integer division by zero stores a message in [`KT_ERROR`] and halts the VM;
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

/// Register the Kotlin extension handler on a fresh VM.
pub fn install(vm: &mut VM) {
    vm.set_extension_handler(Box::new(|vm, id, _arg| match id {
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
        _ => vm.push(Value::Undef),
    }));
}

fn is_int(v: &Value) -> bool {
    matches!(v, Value::Int(_))
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
