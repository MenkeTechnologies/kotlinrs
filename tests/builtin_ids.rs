//! Source-level guard on the hand-assigned host dispatch ids.
//!
//! Every `KT_*` id in `src/host.rs` is a hand-picked number, and nothing in the
//! type system stops two of them from being the same. That is not a theoretical
//! risk: when two branches each add an op and both reach for the next free
//! number, the two `pub const` lines land in DIFFERENT parts of the file, so the
//! merge is clean and the compiler is silent. What happens next depends on the
//! table:
//!
//! * `Op::CallBuiltin` — `vm.register_builtin(id, f)` overwrites by id, so the
//!   LAST registration silently replaces the first. Calls to the earlier op then
//!   run the later op's handler, with no diagnostic anywhere.
//! * `Op::Extended` — the duplicate `match` arm in `handle_coercion` becomes
//!   unreachable. `rustc` warns about that, but the warning is easy to lose in a
//!   build that already prints some, and a guarded arm can suppress it entirely.
//!
//! So this test reads the constants back out of `src/host.rs` as TEXT — not
//! through the compiled values, which would happily agree with themselves — and
//! asserts the numbering is sane. It also checks the other half of the contract:
//! that every id has exactly one dispatch home, and that `src/compiler.rs` emits
//! each id through the opcode whose table actually holds it. Emitting
//! `Op::CallBuiltin` for an extension-handler id is a runtime fault, not a
//! compile error.

const HOST: &str = include_str!("../src/host.rs");
const COMPILER: &str = include_str!("../src/compiler.rs");

/// The identifier starting at byte `at` in `line`, or `""` if none does.
fn ident_at(line: &str, at: usize) -> &str {
    let tail = &line[at..];
    let len = tail
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(tail.len());
    &tail[..len]
}

/// `pub const KT_NAME: u16 = 42;` → `("KT_NAME", 42, line-number)`.
fn declared_ids() -> Vec<(&'static str, u16, usize)> {
    const DECL: &str = "pub const KT_";
    let mut out = Vec::new();
    for (i, line) in HOST.lines().enumerate() {
        if !line.starts_with(DECL) {
            continue;
        }
        let name = ident_at(line, DECL.len() - 3);
        let Some((ty, value)) = line[DECL.len() - 3 + name.len()..].split_once('=') else {
            continue;
        };
        if ty.trim() != ": u16" {
            continue;
        }
        let value = value.trim().trim_end_matches(';');
        let value: u16 = value.parse().unwrap_or_else(|_| {
            panic!(
                "src/host.rs:{}: {name} = {value} is not a literal u16 — this test reads the \
                 numbering as text and cannot follow an expression",
                i + 1
            )
        });
        out.push((name, value, i + 1));
    }
    out
}

/// Every `KT_*` name passed to `vm.register_builtin(NAME, …)` — the
/// `Op::CallBuiltin` table, in registration order.
fn registered_names() -> Vec<&'static str> {
    const CALL: &str = "register_builtin(KT_";
    let mut out = Vec::new();
    for line in HOST.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        if let Some(at) = line.find(CALL) {
            out.push(ident_at(line, at + CALL.len() - 3));
        }
    }
    out
}

/// Every `KT_*` name used as a `match` arm in `handle_coercion` — the
/// `Op::Extended` table.
///
/// Arms sit at exactly eight columns inside the one `match id` in that function,
/// which is what separates them from a `KT_*` mentioned inside an arm BODY.
fn coercion_arms() -> Vec<&'static str> {
    const ARM: &str = "        KT_";
    let mut out = Vec::new();
    let mut inside = false;
    for line in HOST.lines() {
        if line.starts_with("fn handle_coercion(") {
            inside = true;
        } else if inside && line == "}" {
            break;
        } else if inside && line.starts_with(ARM) && line.contains("=>") {
            out.push(ident_at(line, ARM.len() - 3));
        }
    }
    assert!(
        !out.is_empty(),
        "no `handle_coercion` match arms found — this test's parser has drifted from src/host.rs"
    );
    out
}

/// Every `KT_*` id the compiler emits through `opcode`, e.g. `Op::CallBuiltin`.
fn emitted(opcode: &str) -> Vec<&'static str> {
    let needle = format!("{opcode}(KT_");
    let mut out = Vec::new();
    for line in COMPILER.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut from = 0;
        while let Some(at) = line[from..].find(&needle) {
            let start = from + at + needle.len() - 3;
            out.push(ident_at(line, start));
            from = start;
        }
    }
    out
}

#[test]
fn host_dispatch_ids_are_unique() {
    let ids = declared_ids();
    assert!(
        ids.len() > 50,
        "parsed only {} KT_* ids out of src/host.rs — this test's parser has drifted",
        ids.len()
    );

    let mut collisions = Vec::new();
    for (i, (name_a, value_a, line_a)) in ids.iter().enumerate() {
        for (name_b, value_b, line_b) in &ids[i + 1..] {
            if value_a == value_b {
                collisions.push(format!(
                    "  id {value_a}: {name_a} (src/host.rs:{line_a}) and {name_b} (src/host.rs:{line_b})"
                ));
            }
        }
    }
    assert!(
        collisions.is_empty(),
        "{} host dispatch id(s) assigned twice. `register_builtin` overwrites by id — the last \
         registration silently wins — so this is a live miscompile, not a style nit. Give the \
         newer op the next unused number:\n{}",
        collisions.len(),
        collisions.join("\n")
    );
}

#[test]
fn every_declared_id_has_exactly_one_dispatch_home() {
    let ids = declared_ids();
    let registered = registered_names();
    let arms = coercion_arms();

    let mut problems = Vec::new();
    for (name, _, line) in &ids {
        let builtins = registered.iter().filter(|n| *n == name).count();
        let ext = arms.iter().filter(|n| *n == name).count();
        if builtins == 0 && ext == 0 {
            problems.push(format!(
                "  {name} (src/host.rs:{line}): declared but never dispatched — no \
                 `register_builtin` call and no `handle_coercion` arm"
            ));
        }
        if builtins > 0 && ext > 0 {
            problems.push(format!(
                "  {name} (src/host.rs:{line}): dispatched in BOTH tables — the compiler can \
                 only pick one opcode for it, so one of the two handlers is dead"
            ));
        }
        if builtins > 1 {
            problems.push(format!(
                "  {name} (src/host.rs:{line}): registered {builtins}× — every registration but \
                 the last is overwritten"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "{} dispatch problem(s):\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn compiler_emits_each_id_through_its_own_table() {
    let registered = registered_names();
    let arms = coercion_arms();

    let mut problems = Vec::new();
    for name in emitted("Op::CallBuiltin") {
        if !registered.contains(&name) {
            problems.push(format!(
                "  Op::CallBuiltin({name}) is emitted, but {name} is not in `register_builtins` \
                 — the VM faults on an unknown builtin id at run time"
            ));
        }
    }
    for name in emitted("Op::Extended") {
        if !arms.contains(&name) {
            problems.push(format!(
                "  Op::Extended({name}) is emitted, but `handle_coercion` has no arm for {name} \
                 — the op falls through the match and silently does nothing"
            ));
        }
    }
    problems.sort_unstable();
    problems.dedup();
    assert!(
        problems.is_empty(),
        "{} mis-routed emit site(s) in src/compiler.rs:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
