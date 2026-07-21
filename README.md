```
██╗  ██╗ ██████╗ ████████╗██╗     ██╗███╗   ██╗██████╗ ███████╗
██║ ██╔╝██╔═══██╗╚══██╔══╝██║     ██║████╗  ██║██╔══██╗██╔════╝
█████╔╝ ██║   ██║   ██║   ██║     ██║██╔██╗ ██║██████╔╝███████╗
██╔═██╗ ██║   ██║   ██║   ██║     ██║██║╚██╗██║██╔══██╗╚════██║
██║  ██╗╚██████╔╝   ██║   ███████╗██║██║ ╚████║██║  ██║███████║
╚═╝  ╚═╝ ╚═════╝    ╚═╝   ╚══════╝╚═╝╚═╝  ╚═══╝╚═╝  ╚═╝╚══════╝
```

[![CI](https://github.com/MenkeTechnologies/kotlinrs/actions/workflows/ci.yml/badge.svg)](https://github.com/MenkeTechnologies/kotlinrs/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2021-05d9e8?style=flat-square)
[![Docs](https://img.shields.io/badge/docs-online-blue.svg)](https://menketechnologies.github.io/kotlinrs/)
![license](https://img.shields.io/badge/license-MIT-ff2a6d?style=flat-square)
![status](https://img.shields.io/badge/status-active%20%C2%B7%20in%20development-9b5de5?style=flat-square)

### `[KOTLIN, COMPILED TO BYTECODE — JIT-COMPILED, NO JVM]`

> *"The JVM warms up. kotlinrs compiles and runs."*

**Kotlin in Rust** — a compiled Kotlin runtime, hosted on the
[`fusevm`](https://github.com/MenkeTechnologies/fusevm) bytecode VM with a
three-tier Cranelift JIT — the same engine behind `zshrs`, `strykelang`,
`awkrs`, `vimlrs`, `elisprs`, `rubylang`, `phplang`, `pythonrs`, and `node-js`.
No JVM, no `kotlinc`, no `.class` files.

### [`Read the Docs`](https://menketechnologies.github.io/kotlinrs/) &middot; [`Engineering Report`](https://menketechnologies.github.io/kotlinrs/report.html)

---

## Table of Contents

- [\[0x00\] Overview](#0x00-overview)
- [\[0x01\] Install](#0x01-install)
- [\[0x02\] Usage](#0x02-usage)
- [\[0x03\] Language Features](#0x03-language-features)
- [\[0x04\] Command-Line Flags](#0x04-command-line-flags)
- [\[0x05\] Architecture](#0x05-architecture)
- [\[0x06\] Status & Roadmap](#0x06-status--roadmap)
- [\[0xFF\] License](#0xff-license)

---

## [0x00] OVERVIEW

The reference Kotlin toolchain compiles to JVM bytecode and runs on a warm-up
JIT inside the JVM. `kotlinrs` lexes and parses Kotlin to an AST, lowers it to
`fusevm` bytecode, and runs it on a compiled VM with a Cranelift JIT — no JVM in
the loop. kotlinrs carries no VM or JIT of its own. Highlights:

- **Compiled, not tree-walked** — arithmetic, comparison, and control flow lower
  to native fusevm ops so the JIT can block- and trace-compile hot loops.
- **fusevm-hosted** — no local `vm.rs` / `jit.rs`; the shared engine behind
  `zshrs`, `strykelang`, `awkrs`, `vimlrs`, `elisprs`, `rubylang`, `phplang`,
  `pythonrs`, and `node-js`.
- **Native locals & calls** — `val`/`var` bindings compile to frame slots and
  `fun` calls to fusevm's native `Op::Call` sub-dispatch, with real recursion.
- **Kotlin-faithful boundaries** — a small extension handler supplies the three
  behaviors the language-agnostic VM can't: Kotlin `toString()` for
  `Boolean`/`Double`, and truncating integer `/` and `%` with an
  `ArithmeticException` on a zero divisor.

`kotlinrs` is an **M0 scaffold**: a genuinely running Kotlin subset (below), not
a stub. See the roadmap for what is next.

## [0x01] INSTALL

```sh
git clone https://github.com/MenkeTechnologies/kotlinrs
cd kotlinrs
cargo build --release
# the binary is target/release/kotlin
```

Requires a stable Rust toolchain. The only dependency is `fusevm` (which pulls
Cranelift for the JIT); everything else is std.

## [0x02] USAGE

```sh
# run a file
kotlin examples/fizzbuzz.kt

# one-liner (wrapped in `fun main` automatically)
kotlin -e 'println("2 + 2 = ${2 + 2}")'

# introspection
kotlin --dump-tokens   examples/hello.kt
kotlin --dump-ast      examples/hello.kt
kotlin --dump-bytecode examples/hello.kt
```

```kotlin
// examples/fib.kt
fun fib(n: Int): Int {
    return if (n < 2) n else fib(n - 1) + fib(n - 2)
}

fun main() {
    for (i in 0..10) {
        println("fib($i) = ${fib(i)}")
    }
}
```

## [0x03] LANGUAGE FEATURES

The M0 subset, all lowered to fusevm bytecode and exercised by the test suite:

- **Types** — `Int`/`Long` (`i64`), `Double`/`Float` (`f64`), `Boolean`,
  `String`, `Unit`; annotations optional, coarsely inferred otherwise.
- **Declarations** — top-level `fun` with typed parameters and return type;
  `val`/`var` locals; `fun main()` entry (with or without `args`).
- **Expressions** — `+ - * / %`, unary `-`/`!`, comparisons `== != < > <= >=`,
  short-circuit `&&`/`||`, parentheses. `Int/Int` truncates toward zero;
  `Double` division is IEEE.
- **Strings** — literals with `\n`/`\t`/`\\`/`\"`/`\$` escapes and `$name` /
  `${expr}` templates; `+` concatenates when either side is a `String`.
- **Control flow** — `if`/`else` (statement **and** expression, incl.
  `else if`), `while`, and `for` over `a..b`, `a until b`, `a downTo b`, with
  optional `step`.
- **Functions** — user calls, recursion, `return`, `Unit` functions.
- **Built-ins** — `println(...)` / `print(...)`.
- **Comments** — `//` and nested `/* … */`.

Not yet in M0 (see roadmap): classes/objects, collections and their methods,
lambdas, `when`, nullability, generics beyond parse-and-ignore, and the
standard library.

## [0x04] COMMAND-LINE FLAGS

| Flag | Effect |
|------|--------|
| `-e`, `--eval <src>` | Run a snippet (repeatable, newline-joined); wrapped in `fun main` if it has none. |
| `--dump-tokens` | Print the lexer token stream and exit. |
| `--dump-ast` | Print the parsed AST and exit. |
| `--dump-bytecode`, `--disasm` | Print the lowered fusevm chunk disassembly and exit. |
| `--lsp` | Speak the Language Server Protocol over stdio (diagnostics, completion, hover). |
| `--dap` | Speak the Debug Adapter Protocol over stdio (breakpoints, stepping, live locals). |
| `-v`, `--version` | Print the version and exit. |
| `-h`, `--help` | Print help and exit. |

An inline `rust { pub extern "C" fn … }` block inside a function body compiles
to a cached cdylib whose exported functions are callable by name from Kotlin
(via `fusevm::ffi`). Editor tooling (`--lsp`, `--dap`) and a generated
[reference page](https://menketechnologies.github.io/kotlinrs/reference.html)
share the language-server corpus, so they never drift.

## [0x05] ARCHITECTURE

```text
Kotlin source
   │  lexer.rs      → tokens (string templates pre-split)
   │  parser.rs     → AST (ast.rs)
   │  compiler.rs   → fusevm::Chunk   (native ops + 3 extension ops)
   ▼
fusevm::VM  ──►  three-tier Cranelift JIT (linear · block · tracing)
   ▲
   │  host.rs       → KT_TO_STRING / KT_IDIV / KT_IMOD
```

- `compiler.rs` keeps one invariant: every expression leaves exactly one value
  on the stack and every statement is stack-neutral, so `if`/`while`/`for`
  balance without a separate analysis pass.
- The only Kotlin-specific runtime code is three extension ops in `host.rs`;
  everything else is a universal fusevm op.

## [0x06] STATUS & ROADMAP

M0 (this release): the running language subset above, with a headless test
suite and `--dump-*` introspection.

Next: collections (`List`/`Map`/`Set`) and their methods, `when`, classes and
data classes, lambdas and higher-order functions, nullability, and a growing
standard-library surface — followed by the sibling parity tooling (LSP/DAP,
reference generator, differential harness).

## [0xFF] LICENSE

MIT. See [LICENSE](LICENSE).
