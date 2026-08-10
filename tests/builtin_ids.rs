//! Source-level guard on the hand-keyed dispatch registries.
//!
//! Two halves. The first covers the NUMERIC space — the `KT_*` ids. The second
//! covers the NAME-keyed registries, which are the more dangerous of the two
//! because a duplicate there produces no build signal whatsoever:
//!
//! | registry | shape | what a duplicate key does |
//! | --- | --- | --- |
//! | `match name { … }` | `rustc` pattern | **warns** `unreachable pattern` |
//! | `matches!(name, "a" \| "b")` | `rustc` pattern | **warns** `unreachable pattern` |
//! | `&[("name", …)]` + `.find(…)` | plain slice | **silent** — the first entry wins forever |
//!
//! Only the third row is a registry in the sense that matters: nothing in the
//! language relates one tuple in a `const` slice to another, so a second
//! `("NumberFormatException", …)` row pasted above the real one simply takes
//! over every lookup and the build stays clean. `host::BUILTIN_THROWABLES`,
//! `host::THROWABLE_PARENTS` and `lsp::CORPUS` are all that shape. The
//! [`name_keyed_tables_key_each_name_once`] test reads them back out as TEXT
//! and rejects a repeated key.
//!
//! The same silence covers the OTHER name-keyed contract: two `matches!` name
//! sets that must stay disjoint (`compiler::is_coll_hof` vs `is_optional_hof`
//! — a name in both never reaches its no-lambda member spelling) and a name set
//! that must line up 1:1 with a dispatch table in another file
//! (`compiler::is_coll_hof` ∪ `is_optional_hof` vs the arms of
//! `host::coll_hof`). Neither relation exists in the type system;
//! [`hof_name_sets_are_disjoint_and_dispatch_once`] pins both.
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
const LSP: &str = include_str!("../src/lsp.rs");

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

    // FLOORS FIRST. Every loop below is driven by `emitted`, and `emitted` reads
    // `src/compiler.rs` as TEXT: it matches the literal `Op::CallBuiltin(KT_`.
    // Change the emit spelling — wrap it in a helper, rename the `KT_` prefix,
    // reformat the call across two lines — and it matches nothing, both loops
    // iterate zero times, and this test reports PASS having compared nothing at
    // all. Measured, by renaming the needle to `Op::CallBuiltin(RENAMED_KT_`:
    // this test stayed green while the other five in this file still ran.
    //
    // That is the worst failure a guard can have, because it goes silent
    // exactly when the thing it guards has moved. Every other parser in this
    // file already asserts its own yield (`declared_ids` > 50, `coercion_arms`
    // non-empty, `coll_hof_arms` > 30, the `name_keyed_tables` row floors);
    // this one did not. Raise these with the frontend; never lower them to
    // accommodate a parser that stopped matching.
    let call_sites = emitted("Op::CallBuiltin");
    let ext_sites = emitted("Op::Extended");
    assert!(
        call_sites.len() >= 40,
        "parsed only {} `Op::CallBuiltin(KT_…)` emit site(s) out of src/compiler.rs — \
         this test's parser has drifted, and a zero yield here is a silent PASS",
        call_sites.len()
    );
    assert!(
        ext_sites.len() >= 40,
        "parsed only {} `Op::Extended(KT_…)` emit site(s) out of src/compiler.rs — \
         this test's parser has drifted, and a zero yield here is a silent PASS",
        ext_sites.len()
    );

    let mut problems = Vec::new();
    for name in call_sites {
        if !registered.contains(&name) {
            problems.push(format!(
                "  Op::CallBuiltin({name}) is emitted, but {name} is not in `register_builtins` \
                 — the VM faults on an unknown builtin id at run time"
            ));
        }
    }
    for name in ext_sites {
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

// ── The NAME-keyed registries ──────────────────────────────────────────────

/// One row of a `const NAME: &[( … )] = &[ … ];` table: its string literals in
/// source order, and the 1-based line the row opens on.
struct Row {
    strings: Vec<String>,
    line: usize,
}

/// The rows of the `&[…]` slice literal `decl` introduces.
///
/// A hand-written scanner rather than a line regex because the tables wrap: a
/// row can span four lines, and `lsp::CORPUS`'s description strings contain
/// commas, parentheses and escaped quotes. Tracking string state and paren
/// depth is what keeps those out of the row split.
fn table_rows(src: &'static str, decl: &str) -> Vec<Row> {
    let at = src
        .find(decl)
        .unwrap_or_else(|| panic!("`{decl}` is gone — this test's parser has drifted"));
    // `= &[`, not the first `&[`: the DECLARED TYPE is also a slice literal
    // (`&[(&str, &str)]`), and starting there would read the type's own
    // parentheses as the table's first row.
    let open = at + src[at..].find("= &[").expect("table has no `= &[`") + 4;

    let mut rows = Vec::new();
    let mut line = 1 + src[..open].matches('\n').count();
    let mut cur: Option<Row> = None;
    let mut depth = 0usize;
    let mut chars = src[open..].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            // A `//` comment runs to end of line; its text is not table data.
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            s.push('\\');
                            if let Some(e) = chars.next() {
                                s.push(e);
                            }
                        }
                        '"' => break,
                        '\n' => {
                            line += 1;
                            s.push('\n');
                        }
                        other => s.push(other),
                    }
                }
                if let Some(row) = cur.as_mut() {
                    row.strings.push(s);
                }
            }
            '(' => {
                depth += 1;
                if depth == 1 {
                    cur = Some(Row {
                        strings: Vec::new(),
                        line,
                    });
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(row) = cur.take() {
                        rows.push(row);
                    }
                }
            }
            // The `]` that closes the slice — only meaningful outside a row.
            ']' if depth == 0 => break,
            _ => {}
        }
    }
    rows
}

/// The string literals of `matches!(name, "a" | "b" | …)` inside `fn`.
fn name_set(src: &'static str, decl: &str) -> Vec<String> {
    let at = src
        .find(decl)
        .unwrap_or_else(|| panic!("`{decl}` is gone — this test's parser has drifted"));
    let end = at + src[at..].find("\n}").expect("unterminated fn");
    let body = &src[at..end];
    assert!(
        body.contains("matches!("),
        "`{decl}` is no longer a `matches!` name set — this test's parser has drifted"
    );
    literals(body)
}

/// Every double-quoted literal in `body`, in order. The inputs here are
/// hand-written name lists with no escapes.
fn literals(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(a) = rest.find('"') {
        let tail = &rest[a + 1..];
        let Some(b) = tail.find('"') else { break };
        out.push(tail[..b].to_string());
        rest = &tail[b + 1..];
    }
    out
}

/// The names `host::coll_hof` has a `match` arm for. Arms sit at exactly eight
/// columns and open with the string key, so an arm BODY (twelve columns in) is
/// never mistaken for one.
fn coll_hof_arms() -> Vec<String> {
    let at = HOST
        .find("\nfn coll_hof(")
        .expect("`fn coll_hof` is gone — this test's parser has drifted");
    let mut out = Vec::new();
    for line in HOST[at + 1..].lines() {
        if line == "}" {
            break;
        }
        let Some(key) = line.strip_prefix("        \"") else {
            continue;
        };
        let Some(head) = key.split("=>").next() else {
            continue;
        };
        out.extend(literals(&format!("\"{head}")));
    }
    assert!(
        out.len() > 30,
        "parsed only {} `coll_hof` arms — this test's parser has drifted",
        out.len()
    );
    out
}

/// Names appearing more than once in `keys`, each with the lines it landed on.
fn repeats(keys: &[(String, usize)]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, (key, line)) in keys.iter().enumerate() {
        let dups: Vec<String> = keys[i + 1..]
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, l)| l.to_string())
            .collect();
        if !dups.is_empty() {
            out.push(format!(
                "  {key:?}: line {line}, again on line(s) {}",
                dups.join(", ")
            ));
        }
    }
    out
}

#[test]
fn name_keyed_tables_key_each_name_once() {
    // (table, file, how many leading strings form the key, floor on row count)
    let tables: [(&str, &str, usize, usize); 3] = [
        ("pub const BUILTIN_THROWABLES", "src/host.rs", 1, 15),
        ("const THROWABLE_PARENTS", "src/host.rs", 1, 15),
        // A corpus entry is `(name, chapter, signature, description, example)`
        // and the same NAME legitimately appears in several chapters — `count`
        // is both a member and a higher-order function. The key is the pair.
        ("const CORPUS", "src/lsp.rs", 2, 250),
    ];

    let mut problems = Vec::new();
    for (decl, file, key_len, floor) in tables {
        let src = if file == "src/lsp.rs" { LSP } else { HOST };
        let rows = table_rows(src, decl);
        assert!(
            rows.len() >= floor,
            "parsed only {} row(s) out of {decl} — this test's parser has drifted",
            rows.len()
        );
        let keys: Vec<(String, usize)> = rows
            .iter()
            .map(|r| {
                assert!(
                    r.strings.len() >= key_len,
                    "{file}:{}: row in {decl} has {} string(s), fewer than the {key_len}-string \
                     key — this test's parser has drifted",
                    r.line,
                    r.strings.len()
                );
                (r.strings[..key_len].join("\u{1f}"), r.line)
            })
            .collect();
        for dup in repeats(&keys) {
            problems.push(format!("{decl} ({file}):\n{dup}"));
        }
    }

    assert!(
        problems.is_empty(),
        "{} duplicate key(s) in a name-keyed table. Every one of these tables is read with \
         `.iter().find(…)`, so the FIRST row wins and the later one is dead — and unlike a \
         duplicate `match` arm, `rustc` says nothing about it. Merge the rows:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn throwable_hierarchy_is_closed() {
    let declared: Vec<String> = table_rows(HOST, "pub const BUILTIN_THROWABLES")
        .iter()
        .map(|r| r.strings[0].clone())
        .collect();
    let parents: Vec<(String, String)> = table_rows(HOST, "const THROWABLE_PARENTS")
        .iter()
        .map(|r| (r.strings[0].clone(), r.strings[1].clone()))
        .collect();

    // Both loops below are driven by these two vectors, so if the row scanner
    // ever yields nothing for BOTH tables — a reformat away from the
    // `&[(a, b)]` tuple shape does it — this test passes having checked no
    // class at all. `table_rows` panics only when the DECLARATION is gone; a
    // declaration that is still there but whose rows no longer parse is
    // silent, and one empty side alone is caught only because the other side
    // then flags every row. Floors close both modes directly.
    assert!(
        declared.len() >= 15,
        "parsed only {} row(s) out of BUILTIN_THROWABLES — this test's parser has drifted, \
         and two empty tables here are a silent PASS",
        declared.len()
    );
    assert!(
        parents.len() >= 15,
        "parsed only {} row(s) out of THROWABLE_PARENTS — this test's parser has drifted, \
         and two empty tables here are a silent PASS",
        parents.len()
    );

    let mut problems = Vec::new();
    for name in &declared {
        // `Throwable` is the root and is the one class with no parent row.
        if name != "Throwable" && !parents.iter().any(|(c, _)| c == name) {
            problems.push(format!(
                "  {name} is in BUILTIN_THROWABLES but has no THROWABLE_PARENTS row — \
                 `catch (e: Exception)` will not claim it"
            ));
        }
    }
    for (class, parent) in &parents {
        if !declared.contains(class) {
            problems.push(format!(
                "  {class} has a parent row but is not in BUILTIN_THROWABLES — \
                 `throwable_ancestry` stops before reaching it"
            ));
        }
        if !declared.contains(parent) {
            problems.push(format!(
                "  {class}'s parent {parent} is not in BUILTIN_THROWABLES — the ancestry walk \
                 falls off the end of the chain"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "{} break(s) in the throwable hierarchy:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn hof_name_sets_are_disjoint_and_dispatch_once() {
    let coll = name_set(COMPILER, "fn is_coll_hof(");
    let optional = name_set(COMPILER, "fn is_optional_hof(");
    let arms = coll_hof_arms();

    let mut problems = Vec::new();
    for name in &coll {
        if optional.contains(name) {
            problems.push(format!(
                "  {name:?} is in BOTH is_coll_hof and is_optional_hof. The call site tests \
                 is_coll_hof first, so the no-lambda member spelling can never reach the member \
                 dispatch — and neither `matches!` knows the other exists"
            ));
        }
    }
    for name in coll.iter().chain(&optional) {
        if !arms.contains(name) {
            problems.push(format!(
                "  {name:?} is declared a higher-order collection function in src/compiler.rs, \
                 but `host::coll_hof` has no arm for it — the call faults at run time"
            ));
        }
    }
    for name in &arms {
        if !coll.contains(name) && !optional.contains(name) {
            problems.push(format!(
                "  `host::coll_hof` has an arm for {name:?}, but neither is_coll_hof nor \
                 is_optional_hof names it — nothing in src/compiler.rs ever routes there"
            ));
        }
    }
    problems.sort_unstable();
    problems.dedup();
    assert!(
        problems.is_empty(),
        "{} higher-order-function routing problem(s) between src/compiler.rs and src/host.rs:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
