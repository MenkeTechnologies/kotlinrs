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
- **Kotlin-faithful boundaries** — a small extension handler supplies the
  behaviors the language-agnostic VM can't: Kotlin `toString()` for
  `Boolean`/`Double`, truncating integer `/` and `%` with an
  `ArithmeticException` on a zero divisor, the object heap behind classes /
  collections / lambdas, and the in-flight exception a VM with no unwind opcode
  cannot carry itself.

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
  `Char` (integral code unit), `String`, `Unit`; annotations optional (including
  nullable `T?`), coarsely inferred otherwise.
- **Declarations** — top-level `fun` with typed parameters and return type,
  block bodies **and** single-expression bodies (`fun f(...) = expr`);
  `val`/`var` locals (`val` reassignment is a compile error, matching Kotlin);
  `fun main()` entry (with or without `args`).
- **Expressions** — `+ - * / %`, unary `-`/`!`, comparisons `== != < > <= >=`,
  short-circuit `&&`/`||`, parentheses. `Int/Int` truncates toward zero;
  `Double` division is IEEE.
- **Strings** — literals with `\n`/`\t`/`\\`/`\"`/`\$` escapes and `$name` /
  `${expr}` templates; `+` concatenates when either side is a `String`.
- **Char** — `'A'` literals (with `\n`/`\t`/`\uXXXX`/… escapes); integral
  arithmetic (`'A' + 1` → `Char`, `'D' - 'A'` → `Int`), `.code` (→ `Int`) and
  `Int.toChar()` (→ `Char`), ordering by code unit.
- **Member access** — chainable postfix `.`: `String.length`,
  `.uppercase()`/`.lowercase()`, `.trim()`, `.isEmpty()`/`.isNotEmpty()`,
  `Char.code`, `Int.toChar()`, and `Any.toString()`.
- **Classes** — `class C(val x: Int, var y: Int) { fun m() {…} }`: primary-
  constructor properties (`val`/`var`), instance methods (dispatched as native
  fusevm `Op::Call`s with an implicit `this`), property get/set (`p.x`, `p.y =
  …`), and implicit-`this` member access inside methods. `val`-property
  reassignment is a compile error.
- **`data class`** — auto-generated `equals`/`hashCode` (structural),
  `toString()` (`C(x=1, y=2)`), `copy(...)` (positional overrides), and
  `componentN`, so `val (a, b) = p` destructures. `==` on a data class /
  collection is structural.
- **`object`** — singleton declarations with `val`/`var` properties and methods,
  built once and reachable by name (`Counter.inc()`).
- **Collections** — `listOf`/`mutableListOf` and `mapOf`/`mutableMapOf` (with
  `k to v` `Pair`s), indexing `xs[i]` / `m[k]` (and indexed assignment), `.size`,
  `.add`/`.get`/`.contains`/`.indexOf`/`.sum` on lists, `.containsKey`/`.keys`/
  `.values`/`.put` on maps. `List`s, arrays, and ranges share one sequence-member
  table: `.count()`, `.first()`/`.last()`, `.max()`/`.min()`, `.average()`,
  `.toList()`, `.joinToString(sep)`, `.reversed()`.
- **Ranges** — first-class values, not just `for` headers: `1..5`,
  `1 until 5`, `5 downTo 1`, `… step n`, bound to names, printed
  (`IntRange` shows `1..5`, `IntProgression` shows `1..9 step 2` — its last
  *reachable* element), aggregated (`(1..3).sum()`), mapped/filtered, and
  iterated. `x in r` / `x !in r` is step-aligned membership; `in` also works over
  a `List`, a `Map`'s keys, and a `String`'s substrings.
- **Arrays** — `arrayOf`/`intArrayOf`/`doubleArrayOf`/`booleanArrayOf`, the
  zero-filled `IntArray(n)`/`DoubleArray(n)`/`BooleanArray(n)`, and the
  index-lambda initializers `IntArray(n) { it * 2 }` / `DoubleArray(n) { … }` /
  `Array(n) { … }`, with `[i]` read
  and write, `.size`, the shared sequence members, and `for (x in a)`. An array
  keeps JVM semantics: `==` is reference identity (`arrayOf(1) == arrayOf(1)` is
  `false`) and `toString()` is `[I@…`-style (the identity-hash digits are ours,
  the shape is Kotlin's).
- **Math** — `kotlin.math` `abs`/`max`/`min`/`sqrt`/`floor`/`ceil`/`round` and
  `PI`/`E`, gated on `import kotlin.math.*` (or a single-name import, honouring
  `as` renames) exactly as Kotlin gates them; the auto-imported `maxOf`/`minOf`;
  and the `java.lang.Math` statics, which need no import. `round` and
  `Math.round` differ as they do in Kotlin — half-to-even returning `Double`
  versus half-up returning `Long`.
- **First-class lambdas** — `{ it * 2 }`, `{ a, b -> a + b }`, function-type
  values (`val f: (Int) -> Int = …`) and parameters (`fun apply(f: (Int) -> Int, x: Int) = f(x)`);
  store, pass, return, and invoke (`f(3)`); trailing-lambda call syntax; captures
  the enclosing scope by value (a returned lambda keeps its upvalues). Each
  lambda is a heap closure object invoked through a re-entrant VM run — no
  compiler inlining and no fusevm-core change.
- **Higher-order stdlib** — the lambda-taking collection functions operate on
  real lambda values: `.map`/`.filter`/`.forEach`, `.fold`/`.reduce`,
  `.any`/`.all`/`.count`, `.sumOf`, `.maxByOrNull`, `.sortedBy`,
  `.associateWith`, `.groupBy`. `it` is the implicit parameter.
- **Scope functions** — the `it`-form `.let`/`.also`/`.takeIf` on any receiver.
- **Control flow** — `if`/`else` (statement **and** expression, incl.
  `else if`); `when` (statement **and** expression) in subject and subjectless
  forms, with literal, comma-grouped, `in`/`!in` range, `is`/`!is` type, and
  `else` arms; `while`, and `for` — over a literal range (`a..b`, `a until b`,
  `a downTo b`, with optional `step`), which lowers to a counted native-op loop,
  or over any iterable value (a `List`, an array, a range held in a variable, or
  a `String` — `for (c in "abc")` walks its `Char`s).
  A loop body may be a block or a single statement;
  `break`/`continue`, including labeled `outer@ for (…)` with
  `break@outer` / `continue@outer`. Blocks are lexically scoped: bindings
  declared in a nested block (and the `for` variable) drop at the block's end;
  shadowing is restored.
- **Null safety** — `null` literal and nullable types `T?`; safe call `?.`
  (short-circuits to null, and chains: `a?.b?.c`), Elvis `?:`, and the not-null
  assertion `!!` (throws `NullPointerException` on null). `x == null` / `x != null`
  are null tests, not value comparisons, and a null `String?` renders as the four
  characters `null` in a template or a `+` — so `"v=$n"` reads `v=null`.
- **Exceptions** — `try` / `catch` / `finally` / `throw`, with `try` as an
  **expression** (`val n = try { f() } catch (e: Exception) { -1 }`). `catch`
  arms are tested in source order against the JVM throwable hierarchy, so
  `catch (e: RuntimeException)` claims an `IllegalArgumentException`; `finally`
  runs on both the normal and the exceptional path, and an exception raised *by*
  a finalizer replaces the one it interrupted. The modeled throwables construct
  and print like the JVM's (`RuntimeException("boom")` →
  `java.lang.RuntimeException: boom`, `.message` → `boom` or `null`), and the
  runtime faults kotlinrs already reported are the *same* catchable exceptions:
  `1 / 0` is an `ArithmeticException`, `!!` on null a `NullPointerException`, an
  out-of-range index an `IndexOutOfBoundsException`. An uncaught exception
  reports `Exception in thread "main" <class>: <message>` on stderr and exits
  non-zero.
- **Functions** — user calls, recursion, `return`, `Unit` functions.
- **Increment / decrement** — `x++`, `x--`, `++x`, `--x` on a variable, a
  property, or an indexed element, in statement **and** expression position
  (`println(i++)` yields the pre-update value, `println(++i)` the post-update
  one).
- **Built-ins** — `println(...)` / `print(...)`.
- **Imports** — `package` and `import` declarations, including `a.b.*` and
  `a.b.c as d`, parsed and used for name resolution.
- **Type arguments** — explicit call type arguments (`listOf<Int>()`,
  `emptyMap<String, Int>()`) are parsed and ignored; typing stays coarse, and
  `a < b` still parses as a comparison (a type-argument list may hold only names
  and must be followed by `(`, which is how Kotlin resolves the same ambiguity).
- **Comments** — `//` and nested `/* … */`.

Not yet (see roadmap): generic *declarations* (type parameters on a `fun`/`class`
are not accepted; only type arguments at a call site are), the `this`-receiver
scope functions (`.apply`/`.run`), directly invoking a call result (`f()()`;
bind it first), lambda element-type inference (an unannotated `it` is coarsely
typed, so `/` and `%` on it default to float — annotate the parameter `Int` for
integer semantics), interfaces/inheritance (so a user class cannot extend
`Exception` — `throw`/`catch` cover the built-in throwables), class body property
initializers, named / default arguments, the lambda-taking collection functions
on a `String` receiver (`"abc".map { … }`; `for (c in s)` works), and the rest of
the standard-library surface.

A `return` out of a `try` that owns a `finally` is honoured — the finalizer runs
first, nesting outward — but a `break`/`continue` out of one is refused at
compile time, because kotlinrs would run the jump without the finalizer and
silently skipping a cleanup block is worse than not accepting the program. A
`try` with neither a `catch` nor a `finally` is refused too (Kotlin rejects it
as well).

## [0x04] COMMAND-LINE FLAGS

| Flag | Effect |
|------|--------|
| `-e`, `--eval <src>` | Run a snippet (repeatable, newline-joined); wrapped in `fun main` if it has none. |
| `--dump-tokens` | Print the lexer token stream and exit. |
| `--dump-ast` | Print the parsed AST and exit. |
| `--dump-bytecode`, `--disasm` | Print the lowered fusevm chunk disassembly and exit. |
| `--tiers FILE` | Run it, then report which fusevm execution tier took each of its chunks. |
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
   │  compiler.rs   → fusevm::Chunk   (native ops + Kotlin extension ops)
   ▼
fusevm::VM  ──►  three-tier Cranelift JIT (linear · block · tracing)
   ▲
   │  host.rs       → value coercions + object heap (classes, List/Map/Pair,
   │                  closures) + lambda builtins (make/call/HOF/scope)
```

- `compiler.rs` keeps one invariant: every expression leaves exactly one value
  on the stack and every statement is stack-neutral, so `if`/`while`/`for`/`when`
  balance without a separate analysis pass.
- The only Kotlin-specific runtime code is a set of extension ops in `host.rs`
  (value coercions, member dispatch, `is` checks, null tests), a few builtins
  (`Op::CallBuiltin`) for the lambda operations (make-closure, closure-call, the
  higher-order collection dispatch, scope functions), plus a **frontend-owned
  object heap**: a `Value::Obj(u32)` handle indexes a host-side table of class
  instances, lists, maps, pairs, and closures. A lambda is a heap closure (body
  chunk index + captured upvalues) invoked through a re-entrant `vm.run()` — the
  builtin dispatch keeps the extension handler live across that nested run, so
  host ops work inside a lambda body. fusevm just carries the handle
  (identity-comparable); the frontend owns the pointed-to object — the same model
  the other mature fusevm frontends use. Everything else is a universal fusevm op.
- **Exceptions without an unwind opcode.** fusevm has none, and kotlinrs lowers
  `fun`s to *native* `Op::Call` frames, so a `throw` cannot longjmp out of a
  frame. It is instead the two-part protocol the sibling frontends (`javars`,
  `scalars`) converged on: the host parks the throwable in a pending slot and
  suppresses every side-effecting builtin while it is in flight (so nothing
  prints between the raise and its handler), while the compiler emits a pending
  check after each statement that jumps to the innermost enclosing handler — out
  of a loop, out of a frame, into a `catch` dispatch, or into the terminal report
  at the end of `main`. Unwinding is therefore statement-granular: the abandoned
  statement finishes evaluating on garbage operands, which the handler discards
  by truncating the value stack to the depth recorded at `try` entry. A program
  with no `try`/`throw` emits **zero** extra ops and keeps the native print op,
  so it pays nothing for the feature.

## [0x06] STATUS & ROADMAP

M0 (this release): the running language subset above, with a headless test
suite and `--dump-*` introspection.

Landed since M0: a host-side object heap backing classes, `data class`es,
`object` singletons, `List`/`Map`/`Pair` collections and their methods;
first-class lambda values (capture, store/pass/return/invoke, trailing-lambda
syntax) as heap closures; the lambda-taking higher-order collection functions
(`map`/`filter`/`forEach`/`fold`/`reduce`/`any`/`all`/`count`/`sumOf`/
`maxByOrNull`/`sortedBy`/`associateWith`/`groupBy`); the `it`-form scope
functions (`let`/`also`/`takeIf`); ranges as values (`1..5`, `until`, `downTo`,
`step`, `in`/`!in`) with the `IntRange`/`IntProgression` display split; arrays
(`arrayOf`, `IntArray(n)`, indexing, `.size`) with JVM reference equality;
`kotlin.math`/`java.lang.Math` under Kotlin's own import rules; `++`/`--` in
expression position; `try`/`catch`/`finally`/`throw` with `try` as an expression
and host faults raised as catchable JVM exceptions; the array index-lambda
initializers (`IntArray(n) { it * 2 }`, `Array(n) { … }`); `String` iteration
(`for (c in "abc")`); and the null-safety corners — `x == null` as a null test,
`String?` display, and the operator methods (`n?.plus(1)`).

Next: generics, `Set`, the `this`-receiver scope functions (`apply`/`run`),
lambda element-type inference, interfaces/inheritance (including user classes
extending `Exception`), named/default arguments, the lambda-taking collection
functions on a `String` receiver, and a growing standard-library surface —
alongside the sibling parity tooling (LSP/DAP, reference generator, differential
harness).

## [0xFF] LICENSE

MIT. See [LICENSE](LICENSE).
