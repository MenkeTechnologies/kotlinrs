//! Language Server Protocol over stdio (`kotlin --lsp`).
//!
//! Self-contained and read-only: diagnostics come from the same
//! `parser::parse_program` the runtime uses (a syntax error maps to the reported
//! line); hover and completion draw on the keyword/type/builtin corpus below. No
//! output ever reaches the terminal — JSON-RPC on stdio only. Structure follows
//! the sibling `-rs` frontends' `lsp.rs` (see `pythonrs/src/lsp.rs`).

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, ExtractError, Message, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    MarkupContent, MarkupKind, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};

/// A reference-corpus entry: `(name, chapter, signature, doc, example)`.
///
/// Single source of truth for LSP completion and hover, and for the offline
/// `gen-docs` reference page. Every entry mirrors something the runtime actually
/// recognizes, and the chapter says where the evidence lives:
///   * "Keywords & Declarations" → `lexer.rs` reserved words, `parser.rs`
///     modifiers and soft keywords
///   * "Operators"               → `lexer::operator` tokens and the `ast::Expr`
///     nodes the parser builds from them
///   * "Types"                   → `ast::Type::from_name`, plus the container
///     names the compiler resolves for dispatch
///   * "Builtin Functions"       → a call arm in `compiler::compile_call`
///   * "Math"                    → `compiler::is_math_fn` / `is_math_const` and
///     `host::math_call`
///   * "Throwables"              → `host::BUILTIN_THROWABLES`
///   * the member chapters       → `host::kt_method`, `char_method`,
///     `obj_method` and `sequence_member`
///   * "Higher-Order …"          → `compiler::is_coll_hof` and `host::coll_hof`
///   * "Scope Functions"         → `host::b_scope_fn`
///
/// A description says what THIS runtime does. Where the behaviour differs from
/// Kotlin proper, the entry says so rather than quoting the Kotlin contract.
type Entry = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

const CORPUS: &[Entry] = &[
    // ── Keywords & Declarations ──
    (
        "fun",
        "Keywords & Declarations",
        "fun name(p: T, …): R { … }\nfun name(p: T, …): R = expr",
        "Declares a function. Execution enters `fun main`, with or without an `args: Array<String>` parameter. A `= expr` body is a single-expression function. Only `fun`, `class`, `object` and `interface` may appear at the top level — kotlinrs has no top-level `val`/`var`.",
        "fun cube(n: Int): Int = n * n * n\nfun main() { println(cube(3)) }   // 27",
    ),
    (
        "val",
        "Keywords & Declarations",
        "val name: T = expr",
        "Declares a read-only binding. The type annotation is optional and inferred from the initializer when it is absent.",
        "val x = 41\nprintln(x + 1)   // 42",
    ),
    (
        "var",
        "Keywords & Declarations",
        "var name: T = expr",
        "Declares a reassignable binding. The compound assignments (`+=`, `-=`, `*=`, `/=`, `%=`) and `++`/`--` all write through a `var`.",
        "var i = 0\ni += 1\nprintln(i)   // 1",
    ),
    (
        "if",
        "Keywords & Declarations",
        "if (cond) { … } else { … }",
        "Conditional branch. It is also an expression whose value is the last statement of the branch taken; an `if` with no `else` that falls through evaluates to `null`.",
        "val m = if (3 > 2) 3 else 2\nprintln(m)   // 3",
    ),
    (
        "else",
        "Keywords & Declarations",
        "if (cond) { … } else { … }\nwhen { … else -> … }",
        "The fallback branch of an `if`, and the catch-all arm of a `when`. In a `when` it is terminal: arms written after it are unreachable and are not compiled at all.",
        "println(if (4 % 2 == 0) \"even\" else \"odd\")   // even",
    ),
    (
        "while",
        "Keywords & Declarations",
        "while (cond) { … }",
        "Loops while the condition stays true. kotlinrs has no `do … while`; the post-test loop is not part of the grammar.",
        "var i = 0\nwhile (i < 3) i += 1\nprintln(i)   // 3",
    ),
    (
        "for",
        "Keywords & Declarations",
        "for (v in a..b) { … }\nfor (v in iterable) { … }",
        "Iterates a range or an iterable value — a `List`, `Set`, `Array`, range, or `String` (by UTF-16 code unit). A syntactic range header compiles to a counted loop the JIT can trace; every other receiver goes through the host iterator. A `Map` is not iterable here and faults at run time.",
        "for (i in 1..3) print(i)      // 123\nfor (c in \"ab\") print(c)      // ab",
    ),
    (
        "in",
        "Keywords & Declarations",
        "for (v in iterable)\nvalue in container",
        "Two roles: the `for` header separator, and the membership operator over a range, `List`, `Set`, `Array`, a `Map`'s keys, or a `String`'s substrings. A `when` arm may also read `in a..b`.",
        "println(2 in 1..3)                  // true\nprintln(\"b\" in \"abc\")               // true\nprintln(\"a\" in mapOf(\"a\" to 1))     // true",
    ),
    (
        "return",
        "Keywords & Declarations",
        "return\nreturn expr",
        "Returns from the enclosing function. A bare `return` yields `Unit`. There is no labeled `return@label` for returning out of a lambda.",
        "fun answer(): Int { return 42 }\nfun main() { println(answer()) }",
    ),
    (
        "until",
        "Keywords & Declarations",
        "a until b",
        "Half-open ascending range: `a until b` includes `a` and excludes `b`. An infix function, so it binds looser than `..` and than arithmetic.",
        "for (i in 0 until 3) print(i)   // 012",
    ),
    (
        "downTo",
        "Keywords & Declarations",
        "a downTo b",
        "Descending inclusive range, stepping by -1. Printing one shows the progression form, `a downTo b step 1`.",
        "for (i in 3 downTo 1) print(i)   // 321",
    ),
    (
        "step",
        "Keywords & Declarations",
        "range step n",
        "Re-steps a range into a progression. Its left operand is any range expression, not just a literal one, because `step` is an infix function at a looser precedence than `..`. A non-positive `n` raises `IllegalArgumentException`.",
        "for (i in 0..10 step 5) print(i)   // 0510\nprintln(1..10 step 3)              // 1..10 step 3",
    ),
    (
        "when",
        "Keywords & Declarations",
        "when (subject) { cond -> … else -> … }\nwhen { boolExpr -> … }",
        "Multi-way branch, statement or expression. The subject form tests each arm against the subject with `==`, `in`, or `is`; the subjectless form tests each arm as a Boolean. Arms are tried top to bottom and the first match wins; several conditions may share one arm, comma-separated. With no matching arm and no `else` the value is `null`.",
        "val n = 5\nprintln(when (n) { 1 -> \"one\"; in 2..9 -> \"few\"; else -> \"many\" })   // few",
    ),
    (
        "is",
        "Keywords & Declarations",
        "value is Type\nvalue !is Type",
        "Runtime type check, in a `when` arm or in ordinary expression position. It compares the receiver's runtime class tag, so it recognizes the built-in kinds (`Int`, `String`, `List`, …) and user classes including inherited supertypes. A trailing `?` on the type and a type-argument list are both accepted and ignored.",
        "val x: Any = \"s\"\nprintln(when (x) { is String -> \"str\"; is Int -> \"int\"; else -> \"?\" })   // str",
    ),
    (
        "class",
        "Keywords & Declarations",
        "class Name(val p: T, …) : Super(args), Iface { … }",
        "Declares a class. Only the primary constructor exists — there are no secondary constructors and no `init` block. A `val`/`var` primary-constructor parameter becomes a stored property; a plain parameter does not. The supertype list may name one class and any number of interfaces.",
        "open class Animal(val name: String)\nclass Dog(n: String) : Animal(n)\nfun main() { println(Dog(\"rex\").name) }   // rex",
    ),
    (
        "interface",
        "Keywords & Declarations",
        "interface Name { fun m(): T; fun d(): T = expr }",
        "Declares an interface. A member with no body is abstract; one with a body is a default implementation an implementor inherits. Interfaces cannot be instantiated. A soft keyword — `interface` is still usable as an ordinary identifier elsewhere.",
        "interface Greeter {\n    fun greet(): String\n    fun loud(): String = greet().uppercase()\n}\nclass En : Greeter { override fun greet(): String = \"hi\" }\nfun main() { println(En().loud()) }   // HI",
    ),
    (
        "object",
        "Keywords & Declarations",
        "object Name { … }",
        "Declares a singleton, constructed once before `main` runs and reachable by its own name. kotlinrs has no `companion object` and no anonymous `object : T { }` expression.",
        "object Counter { var n = 0 }\nfun main() { Counter.n = 5; println(Counter.n) }   // 5",
    ),
    (
        "data",
        "Keywords & Declarations",
        "data class Name(val p: T, …)",
        "Marks a class as a data class, which generates `equals`, `hashCode`, `toString`, `copy` and `componentN` over the primary-constructor properties only. An inherited field is carried but is not part of the comparison and is skipped by `componentN`. A soft keyword — `x.data` is still a legal property read.",
        "data class Pt(val x: Int, val y: Int)\nfun main() { println(Pt(1, 2)) }   // Pt(x=1, y=2)",
    ),
    (
        "open",
        "Keywords & Declarations",
        "open class Name\nopen fun m(): T",
        "Marks a class as extendable or a member as overridable, and it is enforced. Inheriting from a class that is not `open` is rejected with `B is final, so it cannot be inherited from`, and overriding a member that is not `open` with `` `f` in B is final and cannot be overridden ``.",
        "open class Base { open fun f(): Int = 1 }\nclass D : Base() { override fun f(): Int = 2 }\nfun main() { println(D().f()) }   // 2",
    ),
    (
        "override",
        "Keywords & Declarations",
        "override fun m(): T { … }",
        "Replaces a supertype's member, and it is required — redeclaring a supertype member without it is rejected with `` `f` hides a member of supertype B and needs an `override` modifier ``, and writing it on a member that overrides nothing is rejected too. Dispatch is by the receiver's runtime class, resolved at the call site against every instantiable implementor; a single candidate compiles to a direct call with no test.",
        "open class Base { open fun f(): Int = 1 }\nclass D : Base() { override fun f(): Int = 2 }\nfun main() { val b: Base = D(); println(b.f()) }   // 2",
    ),
    (
        "abstract",
        "Keywords & Declarations",
        "abstract class Name\nabstract fun m(): T",
        "Declares a class that cannot be constructed, or a member with no body that subtypes must supply. Constructing one is a compile error: `cannot construct abstract class Name`.",
        "abstract class Shape { abstract fun area(): Double }\nclass Sq(val s: Double) : Shape() { override fun area(): Double = s * s }\nfun main() { println(Sq(3.0).area()) }   // 9.0",
    ),
    (
        "sealed",
        "Keywords & Declarations",
        "sealed class Name",
        "Declares a sealed class — abstract, and conventionally the root of a closed set of subtypes matched by `is` arms. kotlinrs treats it exactly as `abstract`: it does not check that a `when` over the subtypes is exhaustive, so an unmatched subject still falls through to `null`.",
        "sealed class Expr\ndata class Num(val v: Int) : Expr()\nfun main() { val e: Expr = Num(3); println(when (e) { is Num -> e.v; else -> 0 }) }   // 3",
    ),
    (
        "final",
        "Keywords & Declarations",
        "final fun m(): T",
        "Accepted and discarded — it restates the default. A class or member that is not marked `open` is already final, and the compiler enforces that, so writing `final` changes nothing.",
        "open class B { open fun f(): Int = 1 }\nclass C : B() { final override fun f(): Int = 2 }\nfun main() { println(C().f()) }   // 2",
    ),
    (
        "private",
        "Keywords & Declarations",
        "private fun m()\nprivate class C",
        "A visibility modifier. `public`, `private`, `internal` and `protected` are all parsed and then discarded — a single-file program has no visibility boundary to enforce, so every declaration is reachable from every other one.",
        "private class Box(val v: Int)\nfun main() { println(Box(7).v) }   // 7",
    ),
    (
        "inner",
        "Keywords & Declarations",
        "inner class C",
        "Accepted and discarded. kotlinrs has no nested classes at all — every `class` is top-level — so the modifier never changes what is compiled.",
        "class Holder(val v: Int)\nfun main() { println(Holder(1).v) }   // 1",
    ),
    (
        "super",
        "Keywords & Declarations",
        "super.m(args)\nsuper<T>.m(args)",
        "Calls the supertype's implementation rather than the overriding one, resolved statically. The unqualified form walks the linearized ancestry for the nearest supertype that implements the member; `super<T>` names the supertype explicitly, and `T` must be a direct one.",
        "open class B { open fun f(): Int = 1 }\nclass D : B() { override fun f(): Int = super.f() + 1 }\nfun main() { println(D().f()) }   // 2",
    ),
    (
        "this",
        "Keywords & Declarations",
        "this\nthis.property",
        "The receiver inside a class method or a property initializer. A bare name that matches a property of the enclosing class resolves to `this.name` implicitly, so writing `this` is optional. There is no `this@Label`.",
        "class P(val x: Int) { fun twice(): Int = this.x * 2 }\nfun main() { println(P(4).twice()) }   // 8",
    ),
    (
        "it",
        "Keywords & Declarations",
        "{ it }",
        "The implicit single parameter of a lambda written with no parameter list. A lambda with an explicit list (`{ a, b -> … }`) has no `it`. Its type is unknown to the compiler, so `it + 1` lowers to a native op that stays on the fast path when the value turns out to be a number and is handed back to the runtime when it turns out to be a `Char` — which is what keeps `{ it + 1 }` over a `List<Char>` correct.",
        "println(listOf(1, 2, 3).map { it * 2 })   // [2, 4, 6]",
    ),
    (
        "break",
        "Keywords & Declarations",
        "break\nbreak@label",
        "Exits the enclosing loop. `break@label` exits the loop carrying that label, which is written `label@` before the `for`/`while`.",
        "for (i in 1..9) { if (i == 3) break; print(i) }   // 12",
    ),
    (
        "continue",
        "Keywords & Declarations",
        "continue\ncontinue@label",
        "Skips to the enclosing loop's next iteration. `continue@label` targets a labeled loop, which is how an inner loop advances an outer one.",
        "outer@ for (i in 1..2) { for (j in 1..3) { if (j == 2) continue@outer; print(\"$i$j\") } }   // 1121",
    ),
    (
        "try",
        "Keywords & Declarations",
        "try { … } catch (e: T) { … } finally { … }",
        "Guarded block, and an expression: its value is the last statement of the body, or of the handler that ran. While an exception is unwinding, `println`/`print` are suppressed so nothing is emitted between the `throw` and its handler.",
        "val n = try { 1 / 0 } catch (e: ArithmeticException) { -1 }\nprintln(n)   // -1",
    ),
    (
        "catch",
        "Keywords & Declarations",
        "catch (name: Type) { … }",
        "A handler arm. The first arm whose type the in-flight throwable is an instance of wins, walking the modelled JVM parent chain — so `catch (e: Exception)` catches an `IllegalArgumentException`. `catch (e: Throwable)` catches anything.",
        "try { \"x\".substring(9) } catch (e: Exception) { println(e.message) }",
    ),
    (
        "finally",
        "Keywords & Declarations",
        "finally { … }",
        "Cleanup block, run on the normal and the exceptional path alike. Its own value is discarded — the `try` expression's value still comes from the body or the handler.",
        "try { println(\"work\") } finally { println(\"done\") }",
    ),
    (
        "throw",
        "Keywords & Declarations",
        "throw expr",
        "Raises a throwable. Kotlin types it `Nothing`, so it is an expression and may appear on the right of `?:` or as the body of a `when` arm. An uncaught throw prints `Exception in thread \"main\" <fqn>: <message>` and exits non-zero.",
        "val x: Int? = null\nval v = x ?: throw IllegalStateException(\"missing\")\n// Exception in thread \"main\" java.lang.IllegalStateException: missing",
    ),
    (
        "null",
        "Keywords & Declarations",
        "null",
        "The null reference, carried internally as the VM's undefined value. It prints and interpolates as `null`. Nullability is not checked statically: `T?` is parsed and discarded, so a null only makes itself known through `?.`, `?:`, `!!`, or a runtime fault.",
        "val x: Int? = null\nprintln(x ?: 0)   // 0",
    ),
    (
        "true",
        "Keywords & Declarations",
        "true",
        "The Boolean true literal. `&&` and `||` short-circuit on it; a lambda predicate is only treated as satisfied when it returns exactly `true` (a `null` or non-Boolean result counts as false).",
        "val ok: Boolean = true\nprintln(ok)   // true",
    ),
    (
        "false",
        "Keywords & Declarations",
        "false",
        "The Boolean false literal.",
        "val done: Boolean = false\nprintln(!done)   // true",
    ),
    (
        "import",
        "Keywords & Declarations",
        "import a.b.c\nimport a.b.*\nimport a.b.c as alias",
        "Records an import. It matters for exactly one thing: `kotlin.math` names are unresolvable without it, matching Kotlin. A star import opens the whole package; a single-name import opens only that name; an `as` alias *replaces* the original spelling, so after `import kotlin.math.abs as absolute` the name `abs` is no longer in scope.",
        "import kotlin.math.*\nfun main() { println(sqrt(9.0)) }   // 3.0",
    ),
    (
        "package",
        "Keywords & Declarations",
        "package a.b",
        "Accepted and discarded. A kotlinrs program is a single file with no package-level name resolution, so the declaration exists only so real Kotlin source parses.",
        "package demo\nfun main() { println(\"ok\") }",
    ),
    (
        "as",
        "Keywords & Declarations",
        "import a.b.c as alias",
        "The import-renaming keyword — and only that. kotlinrs has no `as` cast operator and no `as?`: `x as Int` is a parse error. Use `is` for a runtime type check.",
        "import kotlin.math.abs as absolute\nfun main() { println(absolute(-4)) }   // 4",
    ),
    (
        "rust",
        "Keywords & Declarations",
        "rust { … }",
        "An inline Rust FFI block, recognized before lexing and rewritten in place into a `__rust_compile(\"<base64>\", line)` call. The block must sit inside a function body. Its `#[no_mangle] pub extern` exports become callable barewords from Kotlin, dispatched by name at run time.",
        "fun main() {\n    rust { #[no_mangle] pub extern \"C\" fn twice(n: i64) -> i64 { n * 2 } }\n    println(twice(21))   // 42\n}",
    ),
    // ── Operators ──
    (
        "+",
        "Operators",
        "a + b",
        "Addition on numbers, concatenation on `String`, and code-unit displacement on `Char` (`'A' + 1` is `'B'`). Two `Int` operands wrap on overflow; a `Double` operand makes the result `Double`. Its method spelling is `plus`.",
        "println(1 + 2)          // 3\nprintln(\"a\" + \"b\")      // ab\nprintln('A' + 1)        // B",
    ),
    (
        "-",
        "Operators",
        "a - b\n-a",
        "Subtraction, and unary negation. `Char - Char` is the `Int` distance between the code units; `Char - Int` is a `Char`. Its method spelling is `minus`.",
        "println(5 - 2)          // 3\nprintln('c' - 'a')      // 2",
    ),
    (
        "*",
        "Operators",
        "a * b",
        "Multiplication, wrapping for two `Int` operands. Its method spelling is `times`. There is no `String * Int` repeat operator — use `String.repeat`.",
        "println(6 * 7)   // 42",
    ),
    (
        "/",
        "Operators",
        "a / b",
        "Division. Two integral operands divide truncating toward zero; a `Double` operand switches the whole expression to IEEE-754 division. Integer division by zero raises `ArithmeticException: / by zero`; `Double` division by zero yields `Infinity`.",
        "println(7 / 2)       // 3\nprintln(7 / 2.0)     // 3.5",
    ),
    (
        "%",
        "Operators",
        "a % b",
        "Remainder, taking the dividend's sign for integers (`-7 % 2` is `-1`). Integer `%` by zero raises `ArithmeticException`. Its method spelling is `rem`.",
        "println(7 % 3)    // 1\nprintln(-7 % 3)   // -1",
    ),
    (
        "=",
        "Operators",
        "name = value\nrecv.prop = value\nrecv[i] = value",
        "Assignment to a `var`, to a property, or to an indexed slot. An indexed write into a `Map` inserts the key when it is absent; into a `List` or `Array` it is bounds-checked and raises `IndexOutOfBoundsException` past the end.",
        "var i = 1\ni = 2\nval m = mutableMapOf<String, Int>()\nm[\"a\"] = 1\nprintln(\"$i ${m[\"a\"]}\")   // 2 1",
    ),
    (
        "+=",
        "Operators",
        "target += value",
        "Compound add-assign. It works on a `var`, a property, and an indexed slot alike, and reuses the `+` semantics — so it concatenates on a `String` target.",
        "var s = \"a\"\ns += \"b\"\nprintln(s)   // ab",
    ),
    (
        "-=",
        "Operators",
        "target -= value",
        "Compound subtract-assign, on a `var`, a property, or an indexed slot.",
        "var n = 5\nn -= 2\nprintln(n)   // 3",
    ),
    (
        "*=",
        "Operators",
        "target *= value",
        "Compound multiply-assign.",
        "var n = 6\nn *= 7\nprintln(n)   // 42",
    ),
    (
        "/=",
        "Operators",
        "target /= value",
        "Compound divide-assign, truncating when both sides are integral.",
        "var n = 9\nn /= 2\nprintln(n)   // 4",
    ),
    (
        "%=",
        "Operators",
        "target %= value",
        "Compound remainder-assign.",
        "var n = 9\nn %= 4\nprintln(n)   // 1",
    ),
    (
        "++",
        "Operators",
        "x++\n++x",
        "Increment by one. The postfix form's value is the target *before* the update, the prefix form's is the value *after* — and both work in expression position, not only as a statement. The target may be a variable, a property, or an indexed element.",
        "var k = 0\nprintln(k++)   // 0\nprintln(++k)   // 2",
    ),
    (
        "--",
        "Operators",
        "x--\n--x",
        "Decrement by one, with the same prefix/postfix value rule as `++`.",
        "var k = 2\nprintln(k--)   // 2\nprintln(k)     // 1",
    ),
    (
        "==",
        "Operators",
        "a == b",
        "Structural equality. It compares numbers by value across `Int`/`Double`, `List`s element-wise, `Set`s order-insensitively, `Map`s entry-wise, and data-class instances over their primary-constructor properties. An `Array` inherits identity equality, so `arrayOf(1) == arrayOf(1)` is `false`. There is no `===` identity operator.",
        "println(listOf(1, 2) == listOf(1, 2))   // true\nprintln(setOf(1, 2) == setOf(2, 1))     // true",
    ),
    (
        "!=",
        "Operators",
        "a != b",
        "The negation of `==`, with the same structural rules. There is no `!==`.",
        "println(1 != 2)   // true",
    ),
    (
        "<",
        "Operators",
        "a < b",
        "Less-than. Numbers compare numerically and `Char`s by code unit. `String`s do not compare with the relational operators here — kotlinrs implements no `compareTo` on `String`.",
        "println(1 < 2)       // true\nprintln('a' < 'z')   // true",
    ),
    (
        ">",
        "Operators",
        "a > b",
        "Greater-than, over numbers and `Char`s.",
        "println(3 > 2)   // true",
    ),
    (
        "<=",
        "Operators",
        "a <= b",
        "Less-than-or-equal, over numbers and `Char`s.",
        "println(2 <= 2)   // true",
    ),
    (
        ">=",
        "Operators",
        "a >= b",
        "Greater-than-or-equal, over numbers and `Char`s.",
        "println(2 >= 3)   // false",
    ),
    (
        "&&",
        "Operators",
        "a && b",
        "Short-circuiting logical AND: the right operand is not evaluated when the left is false.",
        "val xs = listOf(1)\nprintln(xs.isNotEmpty() && xs[0] == 1)   // true",
    ),
    (
        "||",
        "Operators",
        "a || b",
        "Short-circuiting logical OR: the right operand is not evaluated when the left is true.",
        "println(false || 1 < 2)   // true",
    ),
    (
        "!",
        "Operators",
        "!a",
        "Logical negation. Written twice as a postfix it is instead the not-null assertion `!!`, and followed by `=`, `in` or `is` it forms `!=`, `!in`, `!is`.",
        "println(!(1 > 2))   // true",
    ),
    (
        "..",
        "Operators",
        "a..b",
        "Inclusive ascending range. It is a value, not only a loop header: it can be bound, passed, iterated, and asked for `sum`, `first`, `last`, `reversed` and the higher-order collection functions. `'a'..'z'` builds a `Char` range.",
        "val r = 1..5\nprintln(r.sum())   // 15",
    ),
    (
        "?.",
        "Operators",
        "recv?.member\nrecv?.method(args)",
        "Safe call. The receiver is evaluated once; when it is null the whole expression is `null` and the member is never dispatched. Chains short-circuit as a unit.",
        "val s: String? = null\nprintln(s?.length)   // null",
    ),
    (
        "?:",
        "Operators",
        "a ?: b",
        "Elvis. Yields the left operand unless it is null, in which case it evaluates and yields the right. Right-associative, binding looser than arithmetic and tighter than the comparisons — and its right side may be a `throw`.",
        "val x: Int? = null\nprintln(x ?: 0)   // 0",
    ),
    (
        "!!",
        "Operators",
        "expr!!",
        "Not-null assertion: yields the operand, or raises `NullPointerException` when it is null. Lexed as two consecutive `!` tokens, so `a != b` is unaffected.",
        "val x: Int? = 1\nprintln(x!! + 1)   // 2",
    ),
    (
        "?",
        "Operators",
        "T?",
        "Marks a type nullable. kotlinrs parses the mark and then discards it — there is no static null checking, so a nullable and a non-nullable annotation compile identically and a null only surfaces at run time.",
        "val x: Int? = null\nprintln(x)   // null",
    ),
    (
        "[]",
        "Operators",
        "recv[index]",
        "Indexed read. On a `String` the index is a UTF-16 code unit offset and the result is a `Char`; on a `List`, `Set` or `Array` it is a bounds-checked position; on a `Map` it is a key lookup that yields `null` when absent. Chainable: `m[k][i]`.",
        "println(\"abc\"[1])                 // b\nprintln(listOf(10, 20)[1])        // 20\nprintln(mapOf(\"a\" to 1)[\"z\"])     // null",
    ),
    (
        "[]=",
        "Operators",
        "recv[index] = value",
        "Indexed write. A `List` or `Array` slot is bounds-checked and raises `IndexOutOfBoundsException` past the end; a `Map` key is inserted when absent. Compound forms (`xs[0] += 1`) go through the same path.",
        "val xs = mutableListOf(1, 2)\nxs[0] = 9\nprintln(xs)   // [9, 2]",
    ),
    (
        "->",
        "Operators",
        "{ params -> body }\ncond -> result",
        "Two roles: it separates a lambda's parameters from its body, and a `when` arm's conditions from its result. It also appears in a function type annotation, `(Int) -> Int`, whose parameter and return types are parsed and discarded.",
        "val f: (Int) -> Int = { n -> n * 2 }\nprintln(f(21))   // 42",
    ),
    (
        "@",
        "Operators",
        "label@ for (…) { … }\nbreak@label",
        "Loop labels. A `label@` prefix names the loop that follows, and `break@label` / `continue@label` target it. There is no `this@Label` and no `return@label`.",
        "outer@ for (i in 1..3) { for (j in 1..3) { if (j == 2) continue@outer; print(i) } }   // 123",
    ),
    (
        "$",
        "Operators",
        "\"text $name text ${expr}\"",
        "String template interpolation. A bare `$name` splices an identifier; `${…}` splices an arbitrary expression, re-parsed from the source between the braces. Each interpolated value is rendered by the same stringifier `println` uses, so a `Double` keeps its `.0` and `null` reads as `null`. `\\$` escapes a literal dollar.",
        "val x = 2\nprintln(\"x=$x sq=${x * x}\")   // x=2 sq=4",
    ),
    (
        "to",
        "Operators",
        "first to second",
        "The infix `Pair` constructor — the only way to build a `Pair` here, since `Pair(a, b)` is not a resolvable constructor. It is what `mapOf` takes as each argument.",
        "val p = 1 to \"one\"\nprintln(p.first)   // 1",
    ),
    (
        "!in",
        "Operators",
        "value !in container",
        "Negated membership, over the same containers `in` accepts. A `when` arm may also read `!in a..b`.",
        "println(4 !in 1..3)   // true",
    ),
    (
        "!is",
        "Operators",
        "value !is Type",
        "Negated runtime type check, usable in a `when` arm and in ordinary expression position.",
        "val x: Any = 1\nprintln(x !is String)   // true",
    ),
    (
        ".",
        "Operators",
        "recv.member\nrecv.method(args)",
        "Member access. kotlinrs does not distinguish a property from a zero-argument method: both resolve through the same dispatch, so `xs.size` and `xs.size()` — and `xs.sum` and `xs.sum()` — are the same call. Chains are left-associative and bind tighter than the prefix unary operators.",
        "println(listOf(1, 2, 3).size)   // 3",
    ),
    (
        ":",
        "Operators",
        "name: Type\nclass C : Super(), Iface",
        "Two roles: the type annotation separator on a binding, parameter or return type, and the supertype-list introducer on a class. Any identifier is accepted as a type name; only the eight primitives resolve to a static type, and every other name is carried as an opaque class name for dispatch.",
        "val n: Int = 1\nprintln(n)   // 1",
    ),
    (
        ";",
        "Operators",
        "stmt; stmt",
        "The optional statement separator. Newlines already terminate statements, so `;` matters only when two statements share a line — including inside a `when` arm list written on one line.",
        "var a = 1; a += 1; println(a)   // 2",
    ),
    // ── Types ──
    (
        "Int",
        "Types",
        "Int",
        "Signed 32-bit integer. Values are carried in a 64-bit slot at run time, so every `Int` arithmetic result is narrowed back to 32 bits at the point it is produced: `Int.MAX_VALUE + 1` wraps to `Int.MIN_VALUE`, exactly as on the JVM. `/` and `%` truncate toward zero.",
        "val n: Int = 7 / 2\nprintln(n)              // 3\nprintln(Int.MAX_VALUE + 1)   // -2147483648",
    ),
    (
        "Long",
        "Types",
        "Long",
        "Signed 64-bit integer, sharing `Int`'s division rules. An `L` literal suffix marks a value as 64-bit and prints without it, so `println(10L)` writes `10`. A `Long` operand keeps the whole expression at 64 bits, so it is not narrowed the way an `Int` result is: `2147483647L + 1L` is `2147483648`.",
        "val big: Long = 10L\nprintln(big)                 // 10\nprintln(2147483647L + 1L)    // 2147483648",
    ),
    (
        "Double",
        "Types",
        "Double",
        "IEEE-754 double. It stringifies the way the JVM does: a whole value keeps a trailing `.0`, magnitudes outside `[1e-3, 1e7)` switch to scientific form (`2.5E7`), and the non-finite values print as `NaN` / `Infinity` / `-Infinity`.",
        "println(3.0)          // 3.0\nprintln(1.0 / 0.0)    // Infinity",
    ),
    (
        "Float",
        "Types",
        "Float",
        "Accepted as an annotation and as an `f`-suffixed literal, but there is no distinct single-precision type: `Type::from_name` folds `Float` into `Double`, so a `Float` is stored, computed and printed at double precision.",
        "val f: Float = 1.5f\nprintln(f)   // 1.5",
    ),
    (
        "Boolean",
        "Types",
        "Boolean",
        "The `true`/`false` type, printed as `true` or `false`. A lambda predicate must return one: any other result — including `null` — is treated as not satisfied.",
        "val b: Boolean = 1 < 2\nprintln(b)   // true",
    ),
    (
        "Char",
        "Types",
        "Char",
        "A single UTF-16 code unit, and a distinct type from `Int` — it is carried as a tagged handle rather than a number, which is what makes it print as its character inside a `List` or a `Map`. Supports `+`/`-` displacement, comparison, `.code`, and `'a'..'z'` ranges.",
        "val c: Char = 'A'\nprintln(c + 1)         // B\nprintln(listOf(c))     // [A]",
    ),
    (
        "String",
        "Types",
        "String",
        "Text, with `+` concatenation and `\"$x\"` interpolation. Every length, index and slice position is a UTF-16 code-unit offset, matching the JVM contract rather than the Unicode scalar count. Indexing yields a `Char`; iterating a `String` walks its code units. String *literals* are read one source byte at a time, so a non-ASCII character written inside `\"…\"` splits into its UTF-8 bytes — a `Char` literal (`'é'`) decodes correctly, a string literal does not.",
        "val s: String = \"n = ${1 + 1}\"\nprintln(s)   // n = 2",
    ),
    (
        "Unit",
        "Types",
        "Unit",
        "The no-value type — the result of a function with no `return` and of `println`, `forEach` and the other effect-only calls. The compiler renders it statically as the literal `kotlin.Unit`.",
        "fun log(): Unit { println(\"hi\") }\nfun main() { log() }",
    ),
    (
        "Any",
        "Types",
        "Any",
        "The top type. It resolves to no static type here, so it behaves as an unannotated binding: members dispatch dynamically on the runtime value and `is` decides what it actually holds.",
        "val x: Any = \"s\"\nprintln(x is String)   // true",
    ),
    (
        "List",
        "Types",
        "List<T>",
        "An ordered sequence, built by `listOf`. kotlinrs does not enforce read-only-ness — `List` and `MutableList` are the same runtime object, so `listOf(1, 2).add(3)` succeeds here where Kotlin rejects it at compile time. Prints as `[a, b]`.",
        "val xs: List<Int> = listOf(1, 2, 3)\nprintln(xs)   // [1, 2, 3]",
    ),
    (
        "MutableList",
        "Types",
        "MutableList<T>",
        "The mutable `List` annotation, built by `mutableListOf` / `arrayListOf`. It denotes the same runtime object as `List`; the distinction is documentation only.",
        "val xs: MutableList<Int> = mutableListOf(1)\nxs.add(2)\nprintln(xs)   // [1, 2]",
    ),
    (
        "Set",
        "Types",
        "Set<T>",
        "A distinct-element collection built by `setOf`. It keeps insertion order for display but compares order-insensitively, so `setOf(1, 2) == setOf(2, 1)` and a `Set` never equals a `List`. Prints as `[a, b]`.",
        "println(setOf(3, 1, 3))   // [3, 1]",
    ),
    (
        "MutableSet",
        "Types",
        "MutableSet<T>",
        "The mutable `Set` annotation, built by `mutableSetOf`. `add` answers whether the element was new, which is what distinguishes it from a list's `add`.",
        "val s: MutableSet<Int> = mutableSetOf(1)\nprintln(s.add(1))   // false",
    ),
    (
        "Map",
        "Types",
        "Map<K, V>",
        "A key/value association built by `mapOf` from `k to v` pairs. It is an insertion-ordered entry list, not a hash table — lookup is a linear scan under structural key equality, and iteration order is always insertion order even for `hashMapOf`. Prints as `{k=v, k=v}`.",
        "val m: Map<String, Int> = mapOf(\"a\" to 1)\nprintln(m)   // {a=1}",
    ),
    (
        "MutableMap",
        "Types",
        "MutableMap<K, V>",
        "The mutable `Map` annotation, built by `mutableMapOf` / `hashMapOf`. `put`, `remove` and indexed assignment all write through it.",
        "val m: MutableMap<String, Int> = mutableMapOf()\nm[\"a\"] = 1\nprintln(m)   // {a=1}",
    ),
    (
        "Pair",
        "Types",
        "Pair<A, B>",
        "A two-element tuple, built only by the infix `to` — `Pair(a, b)` is not a resolvable constructor here. Its members are `first` and `second`, and it destructures through `component1`/`component2`.",
        "val p: Pair<Int, String> = 1 to \"one\"\nprintln(p.second)   // one",
    ),
    (
        "Array",
        "Types",
        "Array<T>",
        "A JVM-style array. It inherits identity equality and the JVM's descriptor `toString`, so `arrayOf(1, 2)` prints as `[Ljava.lang.Integer;@0` rather than showing its elements — use `joinToString` or `toList` to see them.",
        "val a: Array<Int> = arrayOf(1, 2, 3)\nprintln(a[1])              // 2\nprintln(a.joinToString())  // 1, 2, 3",
    ),
    (
        "IntArray",
        "Types",
        "IntArray",
        "A primitive `Int` array, descriptor `[I`. `DoubleArray` (`[D`), `BooleanArray` (`[Z`) and `CharArray` (`[C`) are the other three. All four share the sequence members with `List`.",
        "val a: IntArray = IntArray(3)\nprintln(a.sum())   // 0",
    ),
    (
        "IntRange",
        "Types",
        "IntRange",
        "The value `a..b` builds — iterable, summable, and a receiver for the higher-order collection functions. A re-stepped or reversed range becomes an `IntProgression`, which prints in its `a..b step n` / `a downTo b step n` form.",
        "val r: IntRange = 1..5\nprintln(r.sum())        // 15\nprintln(r.reversed())   // 5 downTo 1 step 1",
    ),
    (
        "Nothing",
        "Types",
        "Nothing",
        "The bottom type, the static type Kotlin gives a `throw`. kotlinrs accepts the annotation but resolves no static type from it — like every non-primitive name it is carried as an opaque class name.",
        "fun fail(): Nothing = throw IllegalStateException(\"no\")\nfun main() { println(try { fail() } catch (e: Exception) { \"caught\" }) }",
    ),
    // ── Builtin Functions ──
    (
        "println",
        "Builtin Functions",
        "println()\nprintln(value: Any?)",
        "Writes a value to stdout followed by a newline, and returns `Unit`. It takes at most one argument. While an exception is unwinding it is suppressed, so nothing is emitted between a `throw` and its handler.",
        "println(6 * 7)   // 42",
    ),
    (
        "print",
        "Builtin Functions",
        "print()\nprint(value: Any?)",
        "Writes a value to stdout with no trailing newline. Same one-argument limit and unwinding suppression as `println`.",
        "print(\"a\"); print(\"b\")   // ab",
    ),
    (
        "listOf",
        "Builtin Functions",
        "listOf(vararg elements: T): List<T>",
        "Builds a `List` from its arguments. The result is a plain mutable heap list — kotlinrs does not enforce read-only-ness, so `add` on it succeeds.",
        "val xs = listOf(1, 2, 3)\nprintln(xs.size)   // 3",
    ),
    (
        "mutableListOf",
        "Builtin Functions",
        "mutableListOf(vararg elements: T): MutableList<T>",
        "Builds a mutable `List`. Identical to `listOf` at run time; the two differ only in what the annotation documents.",
        "val xs = mutableListOf(1)\nxs.add(2)\nprintln(xs)   // [1, 2]",
    ),
    (
        "arrayListOf",
        "Builtin Functions",
        "arrayListOf(vararg elements: T): MutableList<T>",
        "The `java.util.ArrayList` spelling of `mutableListOf`. It builds the same heap list.",
        "println(arrayListOf(1, 2))   // [1, 2]",
    ),
    (
        "emptyList",
        "Builtin Functions",
        "emptyList(): List<T>",
        "Builds an empty `List`. An explicit type argument (`emptyList<Int>()`) is accepted and ignored — typing here is coarse.",
        "println(emptyList<Int>())   // []",
    ),
    (
        "setOf",
        "Builtin Functions",
        "setOf(vararg elements: T): Set<T>",
        "Builds a `Set`: duplicates are dropped on the way in, insertion order is kept for display, and equality is order-insensitive. This is Kotlin's `LinkedHashSet`-backed behaviour.",
        "println(setOf(3, 1, 3))   // [3, 1]",
    ),
    (
        "mutableSetOf",
        "Builtin Functions",
        "mutableSetOf(vararg elements: T): MutableSet<T>",
        "Builds a mutable `Set`. `add` on it answers whether the element was new.",
        "val s = mutableSetOf(1)\nprintln(s.add(2))   // true",
    ),
    (
        "hashSetOf",
        "Builtin Functions",
        "hashSetOf(vararg elements: T): MutableSet<T>",
        "Builds the same insertion-ordered `Set` every other set builder does. Kotlin's `HashSet` has no order guarantee; here the display order is always insertion order.",
        "println(hashSetOf(3, 1))   // [3, 1]",
    ),
    (
        "linkedSetOf",
        "Builtin Functions",
        "linkedSetOf(vararg elements: T): MutableSet<T>",
        "The `LinkedHashSet` spelling. Insertion-ordered, which is what every `Set` here already is.",
        "println(linkedSetOf(2, 1))   // [2, 1]",
    ),
    (
        "sortedSetOf",
        "Builtin Functions",
        "sortedSetOf(vararg elements: T): MutableSet<T>",
        "Accepted, but it does **not** sort — it builds the same insertion-ordered distinct set as `setOf`. This diverges from Kotlin, where the result is a `TreeSet` in ascending order. Call `sorted()` on it if order matters.",
        "println(sortedSetOf(3, 1, 2))            // [3, 1, 2]\nprintln(sortedSetOf(3, 1, 2).sorted())   // [1, 2, 3]",
    ),
    (
        "emptySet",
        "Builtin Functions",
        "emptySet(): Set<T>",
        "Builds an empty `Set`.",
        "println(emptySet<Int>())   // []",
    ),
    (
        "mapOf",
        "Builtin Functions",
        "mapOf(vararg pairs: Pair<K, V>): Map<K, V>",
        "Builds a `Map` from `k to v` pairs. Entries stay in insertion order and keys are matched by structural equality on a linear scan.",
        "val m = mapOf(\"a\" to 1, \"b\" to 2)\nprintln(m[\"a\"])   // 1",
    ),
    (
        "mutableMapOf",
        "Builtin Functions",
        "mutableMapOf(vararg pairs: Pair<K, V>): MutableMap<K, V>",
        "Builds a mutable `Map`. `put`, `remove` and `m[k] = v` all write through it.",
        "val m = mutableMapOf(\"a\" to 1)\nm[\"b\"] = 2\nprintln(m)   // {a=1, b=2}",
    ),
    (
        "hashMapOf",
        "Builtin Functions",
        "hashMapOf(vararg pairs: Pair<K, V>): MutableMap<K, V>",
        "Builds the same insertion-ordered `Map` `mutableMapOf` does. Kotlin's `HashMap` has no order guarantee; here iteration and display order are always insertion order.",
        "println(hashMapOf(\"b\" to 1, \"a\" to 2))   // {b=1, a=2}",
    ),
    (
        "emptyMap",
        "Builtin Functions",
        "emptyMap(): Map<K, V>",
        "Builds an empty `Map`, which prints as `{}`.",
        "println(emptyMap<String, Int>())   // {}",
    ),
    (
        "arrayOf",
        "Builtin Functions",
        "arrayOf(vararg elements: T): Array<T>",
        "Builds a JVM-style array. The element values decide the descriptor at run time, so a boxed array prints as `[Ljava.lang.Integer;@n` and compares by identity.",
        "val a = arrayOf(1, 2, 3)\nprintln(a[1])   // 2",
    ),
    (
        "intArrayOf",
        "Builtin Functions",
        "intArrayOf(vararg elements: Int): IntArray",
        "Builds a primitive `Int` array from the given elements.",
        "println(intArrayOf(1, 2).sum())   // 3",
    ),
    (
        "doubleArrayOf",
        "Builtin Functions",
        "doubleArrayOf(vararg elements: Double): DoubleArray",
        "Builds a primitive `Double` array.",
        "println(doubleArrayOf(1.5, 2.5).sum())   // 4.0",
    ),
    (
        "booleanArrayOf",
        "Builtin Functions",
        "booleanArrayOf(vararg elements: Boolean): BooleanArray",
        "Builds a primitive `Boolean` array.",
        "println(booleanArrayOf(true, false).size)   // 2",
    ),
    (
        "charArrayOf",
        "Builtin Functions",
        "charArrayOf(vararg elements: Char): CharArray",
        "Builds a primitive `Char` array.",
        "println(charArrayOf('a', 'b').joinToString(\"\"))   // ab",
    ),
    (
        "IntArray",
        "Builtin Functions",
        "IntArray(size: Int): IntArray\nIntArray(size: Int) { i -> … }: IntArray",
        "Builds a zero-filled `Int` array of the given size, or fills each slot with the lambda applied to its index. A negative size raises `NegativeArraySizeException`.",
        "println(IntArray(3).sum())              // 0\nprintln(IntArray(3) { it * 2 }.sum())   // 6",
    ),
    (
        "DoubleArray",
        "Builtin Functions",
        "DoubleArray(size: Int): DoubleArray\nDoubleArray(size: Int) { i -> … }: DoubleArray",
        "Builds a zero-filled `Double` array, or fills it from the index lambda.",
        "println(DoubleArray(2).sum())   // 0.0",
    ),
    (
        "BooleanArray",
        "Builtin Functions",
        "BooleanArray(size: Int): BooleanArray\nBooleanArray(size: Int) { i -> … }: BooleanArray",
        "Builds a zero-filled `Boolean` array, or fills it from the index lambda.",
        "println(BooleanArray(3).size)   // 3",
    ),
    (
        "CharArray",
        "Builtin Functions",
        "CharArray(size: Int): CharArray\nCharArray(size: Int) { i -> … }: CharArray",
        "Builds a zero-filled `Char` array, or fills it from the index lambda.",
        "println(CharArray(2).size)   // 2",
    ),
    (
        "Array",
        "Builtin Functions",
        "Array(size: Int) { i -> … }: Array<T>",
        "Builds a generic array from an index lambda. Unlike the four primitive builders it exists *only* in the initializer form — Kotlin has no zero-filled `Array(n)` — and its descriptor is inferred from the elements the lambda produced.",
        "println(Array(3) { it * 2 }.joinToString())   // 0, 2, 4",
    ),
    // ── Math ──
    (
        "abs",
        "Math",
        "abs(n: Int): Int\nabs(x: Double): Double",
        "Absolute value, keeping an `Int` result for an integral argument. Lives in `kotlin.math`, so it is unresolvable without `import kotlin.math.abs` or a star import — exactly as in Kotlin.",
        "import kotlin.math.abs\nfun main() { println(abs(-3)) }   // 3",
    ),
    (
        "sqrt",
        "Math",
        "sqrt(x: Double): Double",
        "Square root, always `Double`. Needs the `kotlin.math` import.",
        "import kotlin.math.sqrt\nfun main() { println(sqrt(9.0)) }   // 3.0",
    ),
    (
        "floor",
        "Math",
        "floor(x: Double): Double",
        "Largest `Double` no greater than the argument. Needs the `kotlin.math` import.",
        "import kotlin.math.floor\nfun main() { println(floor(-1.5)) }   // -2.0",
    ),
    (
        "ceil",
        "Math",
        "ceil(x: Double): Double",
        "Smallest `Double` no less than the argument. Needs the `kotlin.math` import.",
        "import kotlin.math.ceil\nfun main() { println(ceil(1.2)) }   // 2.0",
    ),
    (
        "round",
        "Math",
        "round(x: Double): Double",
        "Rounds to the closest integer as a `Double`, with **ties to even** — `round(2.5)` is `2.0` and `round(3.5)` is `4.0`. This is `kotlin.math.round`; `Math.round` is the different, half-up, `Long`-returning one.",
        "import kotlin.math.round\nfun main() { println(round(2.5)) }   // 2.0",
    ),
    (
        "max",
        "Math",
        "max(a: Int, b: Int): Int\nmax(a: Double, b: Double): Double",
        "Larger of two values, keeping an `Int` result when both are integral. Lives in `kotlin.math` and needs the import; `maxOf` is the auto-imported spelling of the same operation.",
        "import kotlin.math.max\nfun main() { println(max(2, 9)) }   // 9",
    ),
    (
        "min",
        "Math",
        "min(a: Int, b: Int): Int\nmin(a: Double, b: Double): Double",
        "Smaller of two values, keeping an `Int` result when both are integral. Needs the `kotlin.math` import; `minOf` is the auto-imported spelling.",
        "import kotlin.math.min\nfun main() { println(min(2, 9)) }   // 2",
    ),
    (
        "maxOf",
        "Math",
        "maxOf(a: T, b: T): T",
        "Larger of two values. It lives in the auto-imported `kotlin` package, so unlike `max` it needs no import; it dispatches to the same implementation.",
        "println(maxOf(2, 9))   // 9",
    ),
    (
        "minOf",
        "Math",
        "minOf(a: T, b: T): T",
        "Smaller of two values, auto-imported like `maxOf` and sharing `min`'s implementation.",
        "println(minOf(2, 9))   // 2",
    ),
    (
        "PI",
        "Math",
        "PI: Double",
        "The `kotlin.math` circle constant, in scope only under the import. It folds to a literal at compile time rather than paying a host dispatch. Also reachable as `Math.PI`, which needs no import.",
        "import kotlin.math.PI\nfun main() { println(PI) }   // 3.141592653589793",
    ),
    (
        "E",
        "Math",
        "E: Double",
        "The `kotlin.math` base of the natural logarithm, in scope only under the import, and also reachable as `Math.E`.",
        "import kotlin.math.E\nfun main() { println(E) }   // 2.718281828459045",
    ),
    (
        "Math",
        "Math",
        "Math.abs(…)  Math.max(…)  Math.min(…)\nMath.sqrt(…)  Math.floor(…)  Math.ceil(…)\nMath.round(…)  Math.PI  Math.E",
        "The `java.lang.Math` statics. Kotlin auto-imports `java.lang.*` on the JVM, so these need no import line — which is the practical difference from the `kotlin.math` top-level spellings. A local binding or a user class named `Math` shadows the whole thing.",
        "println(Math.abs(-3))   // 3",
    ),
    (
        "Math.round",
        "Math",
        "Math.round(x: Double): Long",
        "The odd one out of the rounding family: **half-up** (`floor(x + 0.5)`) and returning a `Long`, where `kotlin.math.round` is ties-to-even and returns a `Double`. It dispatches under its own name for exactly that reason.",
        "println(Math.round(2.5))   // 3",
    ),
    // ── Throwables ──
    (
        "Throwable",
        "Throwables",
        "Throwable()\nThrowable(message: String)",
        "The root of the modelled hierarchy — `java.lang.Throwable`. `catch (e: Throwable)` matches anything in flight, including a value that is not one of the sixteen built-in classes. A constructor with no argument leaves `message` null.",
        "try { throw Throwable(\"boom\") } catch (e: Throwable) { println(e.message) }   // boom",
    ),
    (
        "Exception",
        "Throwables",
        "Exception()\nException(message: String)",
        "`java.lang.Exception`, the parent of `RuntimeException`. Catching it catches every runtime exception below it but not an `Error`.",
        "try { 1 / 0 } catch (e: Exception) { println(\"caught\") }   // caught",
    ),
    (
        "Error",
        "Throwables",
        "Error()\nError(message: String)",
        "`java.lang.Error`, a direct child of `Throwable` and a sibling of `Exception` — so `catch (e: Exception)` does *not* catch it.",
        "try { throw Error(\"fatal\") } catch (e: Throwable) { println(\"caught\") }   // caught",
    ),
    (
        "RuntimeException",
        "Throwables",
        "RuntimeException()\nRuntimeException(message: String)",
        "`java.lang.RuntimeException`, the parent of every fault the runtime itself raises.",
        "try { throw RuntimeException(\"x\") } catch (e: RuntimeException) { println(e.message) }   // x",
    ),
    (
        "ArithmeticException",
        "Throwables",
        "ArithmeticException(message: String)",
        "Raised by integer `/` and `%` when the divisor is zero, with the message `/ by zero`. Floating-point division never raises it — it yields `Infinity` or `NaN`.",
        "try { 1 / 0 } catch (e: ArithmeticException) { println(e.message) }   // / by zero",
    ),
    (
        "IllegalArgumentException",
        "Throwables",
        "IllegalArgumentException(message: String)",
        "Raised by a negative `take`/`drop`/`repeat` count, a non-positive range `step`, and `digitToInt` on a non-digit `Char`.",
        "try { listOf(1).take(-1) } catch (e: IllegalArgumentException) { println(\"caught\") }   // caught",
    ),
    (
        "IllegalStateException",
        "Throwables",
        "IllegalStateException(message: String)",
        "Never raised by the runtime itself — it is here for user code, most often as the right operand of `?:`.",
        "val x: Int? = null\ntry { x ?: throw IllegalStateException(\"missing\") } catch (e: Exception) { println(e.message) }",
    ),
    (
        "NumberFormatException",
        "Throwables",
        "NumberFormatException(message: String)",
        "A child of `IllegalArgumentException`. kotlinrs never raises it — there is no `String.toInt` to fail — so it exists for user code and for the hierarchy's shape.",
        "try { throw NumberFormatException(\"bad\") } catch (e: IllegalArgumentException) { println(\"caught\") }",
    ),
    (
        "IndexOutOfBoundsException",
        "Throwables",
        "IndexOutOfBoundsException(message: String)",
        "The parent of the string and array out-of-range classes. It is raised directly by `removeAt` past the end of a list and by an indexed write past the end.",
        "try { mutableListOf(1).removeAt(5) } catch (e: IndexOutOfBoundsException) { println(\"caught\") }",
    ),
    (
        "StringIndexOutOfBoundsException",
        "Throwables",
        "StringIndexOutOfBoundsException(message: String)",
        "Raised by `s[i]` and `substring` when a UTF-16 offset falls outside the string.",
        "try { \"abc\".substring(9) } catch (e: Exception) { println(e.message) }",
    ),
    (
        "ArrayIndexOutOfBoundsException",
        "Throwables",
        "ArrayIndexOutOfBoundsException(message: String)",
        "Raised by an out-of-range indexed read on a `List`, `Set` or `Array` — note that the *method* form, `get(i)`, raises `NoSuchElementException` instead, which diverges from Kotlin.",
        "try { listOf(1, 2)[5] } catch (e: Exception) { println(e.message) }",
    ),
    (
        "NullPointerException",
        "Throwables",
        "NullPointerException()\nNullPointerException(message: String)",
        "Raised by `!!` on a null value. Since kotlinrs does no static null checking, `!!` is the only place the runtime produces one on its own.",
        "val x: Int? = null\ntry { x!! } catch (e: NullPointerException) { println(\"caught\") }",
    ),
    (
        "ClassCastException",
        "Throwables",
        "ClassCastException(message: String)",
        "Modelled for the hierarchy but never raised: kotlinrs has no `as` cast operator, so there is no cast to fail.",
        "try { throw ClassCastException(\"x\") } catch (e: RuntimeException) { println(\"caught\") }",
    ),
    (
        "UnsupportedOperationException",
        "Throwables",
        "UnsupportedOperationException(message: String)",
        "Raised by `reduce` on an empty sequence, with the message `Empty collection can't be reduced.`",
        "try { listOf<Int>().reduce { a, b -> a + b } } catch (e: Exception) { println(\"caught\") }",
    ),
    (
        "NegativeArraySizeException",
        "Throwables",
        "NegativeArraySizeException(message: String)",
        "Raised by an array builder given a negative size, such as `IntArray(-1)`.",
        "try { IntArray(-1) } catch (e: Exception) { println(\"caught\") }",
    ),
    (
        "NoSuchElementException",
        "Throwables",
        "NoSuchElementException(message: String)",
        "The one built-in throwable outside `java.lang` — it is `java.util.NoSuchElementException`, and the package is observable through `toString`. Raised by `first`, `last`, `get`, `max` and `min` on an empty sequence, with the message `List is empty.`",
        "try { listOf<Int>().first() } catch (e: NoSuchElementException) { println(e.message) }",
    ),
    // ── String Members ──
    (
        "length",
        "String Members",
        "String.length: Int",
        "Number of UTF-16 code units — the JVM `kotlin.String.length` contract, not the Unicode scalar count. Every index, slice and `indexOf` result uses the same basis. A non-ASCII character written inside a string literal was split into UTF-8 bytes by the lexer, so its length is counted in those bytes.",
        "println(\"abc\".length)   // 3",
    ),
    (
        "uppercase",
        "String Members",
        "String.uppercase(): String",
        "Full Unicode uppercase mapping, so a character whose mapping expands does expand (`ß` becomes `SS`). No locale argument is accepted.",
        "println(\"abc\".uppercase())   // ABC",
    ),
    (
        "toUpperCase",
        "String Members",
        "String.toUpperCase(): String",
        "The deprecated Kotlin spelling of `uppercase`, accepted here and dispatching to the same implementation.",
        "println(\"abc\".toUpperCase())   // ABC",
    ),
    (
        "lowercase",
        "String Members",
        "String.lowercase(): String",
        "Full Unicode lowercase mapping. No locale argument is accepted.",
        "println(\"ABC\".lowercase())   // abc",
    ),
    (
        "toLowerCase",
        "String Members",
        "String.toLowerCase(): String",
        "The deprecated Kotlin spelling of `lowercase`, dispatching to the same implementation.",
        "println(\"ABC\".toLowerCase())   // abc",
    ),
    (
        "trim",
        "String Members",
        "String.trim(): String",
        "Strips leading and trailing whitespace, by Unicode's whitespace definition. There is no predicate or char-set overload, and no `trimStart`/`trimEnd`/`trimIndent`.",
        "println(\"  hi  \".trim())   // hi",
    ),
    (
        "isEmpty",
        "String Members",
        "String.isEmpty(): Boolean",
        "True when the string has no code units at all.",
        "println(\"\".isEmpty())   // true",
    ),
    (
        "isNotEmpty",
        "String Members",
        "String.isNotEmpty(): Boolean",
        "The negation of `isEmpty`.",
        "println(\"a\".isNotEmpty())   // true",
    ),
    (
        "isBlank",
        "String Members",
        "String.isBlank(): Boolean",
        "True when the string is empty or made only of whitespace.",
        "println(\"  \".isBlank())   // true",
    ),
    (
        "isNotBlank",
        "String Members",
        "String.isNotBlank(): Boolean",
        "The negation of `isBlank`.",
        "println(\" x \".isNotBlank())   // true",
    ),
    (
        "contains",
        "String Members",
        "String.contains(other: Any): Boolean",
        "Substring test. The argument is rendered by the same stringifier `println` uses, so a `Char` argument reads as its character — but an `Int` reads as its digits, which means `\"abc\".contains(97)` is `false` where the JVM's `Char` overload would say `true`. There is no `ignoreCase` parameter and no `Regex` overload.",
        "println(\"abc\".contains(\"bc\"))   // true\nprintln(\"abc\".contains('b'))     // true",
    ),
    (
        "startsWith",
        "String Members",
        "String.startsWith(prefix: Any): Boolean",
        "Prefix test, with the argument rendered the same way `contains` renders it. No `ignoreCase` or start-offset parameter.",
        "println(\"abc\".startsWith(\"ab\"))   // true",
    ),
    (
        "endsWith",
        "String Members",
        "String.endsWith(suffix: Any): Boolean",
        "Suffix test. No `ignoreCase` parameter.",
        "println(\"abc\".endsWith(\"bc\"))   // true",
    ),
    (
        "plus",
        "String Members",
        "String.plus(other: Any): String",
        "The method spelling of `+` — what `a + b` compiles to on the JVM, and the form `+` has to take to be reached through a safe call.",
        "println(\"ab\".plus(\"c\"))   // abc",
    ),
    (
        "replace",
        "String Members",
        "String.replace(old: Any, new: Any): String",
        "Replaces every occurrence of a literal substring. There is no `Regex` overload and no `ignoreCase` parameter.",
        "println(\"a-b-c\".replace(\"-\", \"+\"))   // a+b+c",
    ),
    (
        "repeat",
        "String Members",
        "String.repeat(n: Int): String",
        "Concatenates the string with itself `n` times. `n` of zero yields the empty string; a negative `n` raises `IllegalArgumentException: Count 'n' must be non-negative, but was N.`",
        "println(\"ab\".repeat(2))   // abab",
    ),
    (
        "indexOf",
        "String Members",
        "String.indexOf(needle: Any): Int",
        "UTF-16 offset of the first occurrence, or -1 when there is none. There is no start-index parameter and no `lastIndexOf`.",
        "println(\"abc\".indexOf(\"c\"))   // 2",
    ),
    (
        "substring",
        "String Members",
        "String.substring(start: Int): String\nString.substring(start: Int, end: Int): String",
        "Slice between two UTF-16 offsets, `end` exclusive and defaulting to the length. An out-of-range or inverted pair raises `StringIndexOutOfBoundsException: Range [start, end) out of bounds for length N`.",
        "println(\"hello\".substring(1, 3))   // el",
    ),
    // ── Char Members ──
    (
        "code",
        "Char Members",
        "Char.code: Int",
        "The character's UTF-16 code unit as an `Int`. The inverse is `Int.toChar()`.",
        "println('A'.code)   // 65",
    ),
    (
        "digitToInt",
        "Char Members",
        "Char.digitToInt(): Int",
        "The decimal value of a digit character. Radix 10 only — no radix parameter — and a non-digit raises `IllegalArgumentException: Char c is not a decimal digit`. There is no `digitToIntOrNull`.",
        "println('7'.digitToInt())   // 7",
    ),
    (
        "isDigit",
        "Char Members",
        "Char.isDigit(): Boolean",
        "True for a Unicode numeric character. The classification delegates to Rust's Unicode tables, which agree with the JVM's `Character` over ASCII.",
        "println('7'.isDigit())   // true",
    ),
    (
        "isLetter",
        "Char Members",
        "Char.isLetter(): Boolean",
        "True for a Unicode alphabetic character. A lone surrogate half is neither letter nor digit, on this runtime and on the JVM alike.",
        "println('a'.isLetter())   // true",
    ),
    (
        "isLetterOrDigit",
        "Char Members",
        "Char.isLetterOrDigit(): Boolean",
        "True for a Unicode alphanumeric character.",
        "println('_'.isLetterOrDigit())   // false",
    ),
    (
        "isWhitespace",
        "Char Members",
        "Char.isWhitespace(): Boolean",
        "True for a Unicode whitespace character.",
        "println(' '.isWhitespace())   // true",
    ),
    (
        "isUpperCase",
        "Char Members",
        "Char.isUpperCase(): Boolean",
        "True for a Unicode uppercase character.",
        "println('A'.isUpperCase())   // true",
    ),
    (
        "isLowerCase",
        "Char Members",
        "Char.isLowerCase(): Boolean",
        "True for a Unicode lowercase character.",
        "println('a'.isLowerCase())   // true",
    ),
    (
        "uppercaseChar",
        "Char Members",
        "Char.uppercaseChar(): Char",
        "The uppercase mapping as a single `Char`. When the mapping would expand to more than one character the original is kept — the JVM's `Character.toUpperCase(char)` contract, so `'ß'.uppercaseChar()` is still `'ß'`.",
        "println('a'.uppercaseChar())   // A",
    ),
    (
        "lowercaseChar",
        "Char Members",
        "Char.lowercaseChar(): Char",
        "The lowercase mapping as a single `Char`, keeping the original when the mapping would expand.",
        "println('A'.lowercaseChar())   // a",
    ),
    (
        "uppercase",
        "Char Members",
        "Char.uppercase(): String",
        "The full uppercase mapping as a `String`, which is what lets an expanding mapping expand — unlike `uppercaseChar`.",
        "println('a'.uppercase())   // A",
    ),
    (
        "lowercase",
        "Char Members",
        "Char.lowercase(): String",
        "The full lowercase mapping as a `String`.",
        "println('A'.lowercase())   // a",
    ),
    (
        "compareTo",
        "Char Members",
        "Char.compareTo(other: Char): Int",
        "Orders two characters by code unit. It returns the sign only — -1, 0 or 1 — where the JVM returns the code-unit difference. The sign is all Kotlin's `Comparable` contract promises, but a program that reads the magnitude will see a different number here.",
        "println('b'.compareTo('a'))   // 1",
    ),
    (
        "plus",
        "Char Members",
        "Char.plus(n: Int): Char",
        "Displaces the code unit upward, yielding a `Char`. The result wraps into 16 bits.",
        "println('A'.plus(1))   // B",
    ),
    (
        "minus",
        "Char Members",
        "Char.minus(n: Int): Char\nChar.minus(other: Char): Int",
        "Two behaviours chosen by the argument: subtracting an `Int` displaces the code unit and yields a `Char`; subtracting another `Char` yields the `Int` distance between them.",
        "println('c'.minus(1))     // b\nprintln('c'.minus('a'))   // 2",
    ),
    (
        "equals",
        "Char Members",
        "Char.equals(other: Any?): Boolean",
        "Code-unit equality. A `Char` is a tagged handle rather than a heap object, so this needs no heap read.",
        "println('a'.equals('a'))   // true",
    ),
    (
        "hashCode",
        "Char Members",
        "Char.hashCode(): Int",
        "The code unit itself, matching the JVM's `Character.hashCode`.",
        "println('A'.hashCode())   // 65",
    ),
    (
        "toString",
        "Char Members",
        "Char.toString(): String",
        "The one-character string. The compiler resolves this statically from the receiver's type rather than through the generic stringifier, because the runtime representation alone could not tell a `Char` from a number.",
        "println('A'.toString() + \"!\")   // A!",
    ),
    // ── Sequence Members ──
    (
        "size",
        "Sequence Members",
        "size: Int",
        "Element count. Implemented once and shared by `List`, `Set`, `Array` and a range, so the four cannot drift apart. `count()` with no argument is the same call.",
        "println(listOf(1, 2, 3).size)   // 3",
    ),
    (
        "count",
        "Sequence Members",
        "count(): Int",
        "Element count, identical to `size` when called with no argument. With a lambda it is instead the predicate-counting higher-order function.",
        "println((1..5).count())   // 5",
    ),
    (
        "isEmpty",
        "Sequence Members",
        "isEmpty(): Boolean",
        "True when the sequence holds no elements.",
        "println(emptyList<Int>().isEmpty())   // true",
    ),
    (
        "isNotEmpty",
        "Sequence Members",
        "isNotEmpty(): Boolean",
        "The negation of `isEmpty`.",
        "println(listOf(1).isNotEmpty())   // true",
    ),
    (
        "first",
        "Sequence Members",
        "first(): T",
        "The first element. On a range it is instead a progression *property* — the start value, defined even when the range is empty. On a list, set or array an empty receiver raises `NoSuchElementException: List is empty.` There is no `firstOrNull` and no predicate overload.",
        "println(listOf(10, 20).first())   // 10",
    ),
    (
        "last",
        "Sequence Members",
        "last(): T",
        "The last element, or a range's end value as a progression property. An empty list, set or array raises `NoSuchElementException`. There is no `lastOrNull`.",
        "println(listOf(10, 20).last())   // 20",
    ),
    (
        "get",
        "Sequence Members",
        "get(index: Int): T",
        "Element at a position. Out of range it raises `NoSuchElementException: List is empty.` — which diverges from Kotlin, and from the `[]` operator on the same receiver, which raises `ArrayIndexOutOfBoundsException`.",
        "println(listOf(10, 20).get(1))   // 20",
    ),
    (
        "contains",
        "Sequence Members",
        "contains(element: T): Boolean",
        "Membership by structural equality — the same comparison `==` uses, so a nested list or data-class element compares by value.",
        "println(listOf(1, 2).contains(2))   // true",
    ),
    (
        "indexOf",
        "Sequence Members",
        "indexOf(element: T): Int",
        "Position of the first structurally equal element, or -1. There is no `lastIndexOf` and no `indexOfFirst`.",
        "println(listOf(\"a\", \"b\").indexOf(\"b\"))   // 1",
    ),
    (
        "sum",
        "Sequence Members",
        "sum(): Int\nsum(): Double",
        "Adds the elements. The result is an `Int` when every element is integral and a `Double` otherwise — so a mixed sequence sums to a `Double`. An empty sequence sums to `0`.",
        "println(listOf(1, 2, 3).sum())   // 6",
    ),
    (
        "average",
        "Sequence Members",
        "average(): Double",
        "Arithmetic mean, always a `Double`. An empty sequence averages to `NaN` rather than raising, matching Kotlin.",
        "println(listOf(1, 2).average())   // 1.5",
    ),
    (
        "max",
        "Sequence Members",
        "max(): T",
        "The largest element — strings lexicographically, chars by code unit, everything else numerically. An empty sequence raises `NoSuchElementException`. kotlinrs implements the pre-1.4 spelling only: there is no `maxOrNull`.",
        "println(listOf(3, 1, 2).max())   // 3",
    ),
    (
        "min",
        "Sequence Members",
        "min(): T",
        "The smallest element, by the same ordering as `max`, raising on an empty sequence. There is no `minOrNull`.",
        "println(listOf(3, 1, 2).min())   // 1",
    ),
    (
        "toList",
        "Sequence Members",
        "toList(): List<T>",
        "A `List` holding the same elements, in order. On a range this is what materializes it.",
        "println((1..3).toList())   // [1, 2, 3]",
    ),
    (
        "toMutableList",
        "Sequence Members",
        "toMutableList(): MutableList<T>",
        "The same copy `toList` makes — `List` and `MutableList` are one runtime object here.",
        "println(setOf(1, 2).toMutableList())   // [1, 2]",
    ),
    (
        "toTypedArray",
        "Sequence Members",
        "toTypedArray(): Array<T>",
        "Yields a `List`, not an `Array` — it shares `toList`'s implementation. This diverges from Kotlin: the result prints its elements as `[1, 2]` and compares structurally, where a real array would print its JVM descriptor and compare by identity.",
        "println(listOf(1, 2).toTypedArray())   // [1, 2]",
    ),
    (
        "asList",
        "Sequence Members",
        "asList(): List<T>",
        "A `List` over the same elements. It is a copy here rather than a view, so mutating the receiver afterwards does not show through.",
        "println(intArrayOf(1, 2).asList())   // [1, 2]",
    ),
    (
        "toSet",
        "Sequence Members",
        "toSet(): Set<T>",
        "A `Set` of the distinct elements, first occurrence kept, in encounter order.",
        "println(listOf(3, 1, 3).toSet())   // [3, 1]",
    ),
    (
        "toMutableSet",
        "Sequence Members",
        "toMutableSet(): MutableSet<T>",
        "The same distinct `Set` `toSet` builds.",
        "println(listOf(1, 1).toMutableSet())   // [1]",
    ),
    (
        "toHashSet",
        "Sequence Members",
        "toHashSet(): MutableSet<T>",
        "The same insertion-ordered distinct `Set`. Kotlin's `HashSet` gives no order guarantee; here encounter order is always kept.",
        "println(listOf(2, 1, 2).toHashSet())   // [2, 1]",
    ),
    (
        "distinct",
        "Sequence Members",
        "distinct(): List<T>",
        "The distinct elements as a **`List`**, where `toSet` gives the same elements as a `Set`. That is the whole difference between the two, and it decides how the result compares and prints.",
        "println(listOf(3, 1, 3).distinct())   // [3, 1]",
    ),
    (
        "union",
        "Sequence Members",
        "union(other: Iterable<T>): Set<T>",
        "Distinct elements of the receiver followed by those of the argument, as a `Set`. Defined on any iterable receiver, and always returning a `Set` whatever kind the receiver was. Only the method form parses — `a union b` as an infix call is a syntax error.",
        "println(listOf(1, 2).union(listOf(2, 3)))   // [1, 2, 3]",
    ),
    (
        "intersect",
        "Sequence Members",
        "intersect(other: Iterable<T>): Set<T>",
        "Distinct receiver elements that also appear in the argument, as a `Set`.",
        "println(listOf(1, 2, 3).intersect(listOf(2, 3, 4)))   // [2, 3]",
    ),
    (
        "subtract",
        "Sequence Members",
        "subtract(other: Iterable<T>): Set<T>",
        "Distinct receiver elements that do not appear in the argument, as a `Set`.",
        "println(listOf(1, 2, 3).subtract(listOf(2)))   // [1, 3]",
    ),
    (
        "sorted",
        "Sequence Members",
        "sorted(): List<T>",
        "Ascending sort into a new `List` — strings lexicographically, chars by code unit, everything else numerically. There is no `sortedWith` and no in-place `sort`.",
        "println(listOf(3, 1, 2).sorted())   // [1, 2, 3]",
    ),
    (
        "sortedDescending",
        "Sequence Members",
        "sortedDescending(): List<T>",
        "Descending sort into a new `List`.",
        "println(listOf(3, 1, 2).sortedDescending())   // [3, 2, 1]",
    ),
    (
        "take",
        "Sequence Members",
        "take(n: Int): List<T>",
        "The first `n` elements as a `List`. An oversized `n` clamps to the whole sequence; a negative one raises `IllegalArgumentException: Requested element count N is less than zero.` There is no `takeWhile` and no `takeLast`.",
        "println(listOf(1, 2, 3).take(2))   // [1, 2]",
    ),
    (
        "drop",
        "Sequence Members",
        "drop(n: Int): List<T>",
        "Everything after the first `n` elements, as a `List`. An oversized `n` yields an empty list; a negative one raises `IllegalArgumentException`. There is no `dropWhile` and no `dropLast`.",
        "println(listOf(1, 2, 3).drop(1))   // [2, 3]",
    ),
    (
        "joinToString",
        "Sequence Members",
        "joinToString(): String\njoinToString(separator: Any): String",
        "Renders the elements with the same stringifier `println` uses, joined by a separator that defaults to `\", \"`. Only the separator parameter exists — there is no `prefix`, `postfix`, `limit` or `transform`.",
        "println(listOf(1, 2, 3).joinToString(\"-\"))   // 1-2-3",
    ),
    (
        "reversed",
        "Sequence Members",
        "reversed(): List<T>\nIntRange.reversed(): IntProgression",
        "Reverses the order. On a list, set or array the result is a reversed `List`; on a range it is instead a descending `IntProgression`, which prints in its `b downTo a step 1` form.",
        "println(listOf(1, 2, 3).reversed())   // [3, 2, 1]\nprintln((1..3).reversed())            // 3 downTo 1 step 1",
    ),
    // ── Mutable Collection Members ──
    (
        "add",
        "Mutable Collection Members",
        "MutableList.add(element: T): Boolean\nMutableSet.add(element: T): Boolean",
        "Appends to a list, always answering `true`; on a set it inserts only when the element is new and answers whether it was. That difference in the answer is Kotlin's contract and is why one implementation serves both.",
        "val s = mutableSetOf(1)\nprintln(s.add(1))   // false\nprintln(s.add(2))   // true",
    ),
    (
        "remove",
        "Mutable Collection Members",
        "MutableList.remove(element: T): Boolean\nMutableSet.remove(element: T): Boolean\nMutableMap.remove(key: K): Boolean",
        "Removes the first structurally equal element (or the entry under the key) and answers whether anything was removed. On a `Map` this diverges from Kotlin, which answers the removed *value* or `null`; here it is always a `Boolean`.",
        "val xs = mutableListOf(1, 2)\nprintln(xs.remove(1))   // true\nprintln(xs)             // [2]",
    ),
    (
        "removeAt",
        "Mutable Collection Members",
        "MutableList.removeAt(index: Int): T",
        "Removes the element at a position and answers it. Out of range it raises `IndexOutOfBoundsException`. Lists only — a set has no positional removal.",
        "val xs = mutableListOf(1, 2)\nprintln(xs.removeAt(0))   // 1",
    ),
    // ── Map Members ──
    (
        "size",
        "Map Members",
        "Map.size: Int",
        "Number of entries.",
        "println(mapOf(\"a\" to 1, \"b\" to 2).size)   // 2",
    ),
    (
        "isEmpty",
        "Map Members",
        "Map.isEmpty(): Boolean",
        "True when the map has no entries.",
        "println(emptyMap<String, Int>().isEmpty())   // true",
    ),
    (
        "isNotEmpty",
        "Map Members",
        "Map.isNotEmpty(): Boolean",
        "The negation of `isEmpty`.",
        "println(mapOf(\"a\" to 1).isNotEmpty())   // true",
    ),
    (
        "containsKey",
        "Map Members",
        "Map.containsKey(key: K): Boolean",
        "Whether a structurally equal key is present. It is a linear scan of the entry list, not a hash lookup. There is no `containsValue`.",
        "println(mapOf(\"a\" to 1).containsKey(\"a\"))   // true",
    ),
    (
        "get",
        "Map Members",
        "Map.get(key: K): V?",
        "The value under a key, or `null` when the key is absent — the same lookup `m[k]` performs. It never raises. There is no `getOrDefault` or `getOrElse`.",
        "println(mapOf(\"a\" to 1).get(\"z\"))   // null",
    ),
    (
        "keys",
        "Map Members",
        "Map.keys: List<K>",
        "The keys in insertion order. It returns a **`List`**, not the `Set` Kotlin returns — so it prints as `[a, b]` and compares order-sensitively.",
        "println(mapOf(\"a\" to 1, \"b\" to 2).keys)   // [a, b]",
    ),
    (
        "values",
        "Map Members",
        "Map.values: List<V>",
        "The values in insertion order, as a `List`. There is no `entries` member.",
        "println(mapOf(\"a\" to 1, \"b\" to 2).values)   // [1, 2]",
    ),
    (
        "put",
        "Map Members",
        "MutableMap.put(key: K, value: V): V?",
        "Sets a key, appending the entry when it is new. It answers the previous value, or `null` when there was none — the same write `m[k] = v` performs.",
        "val m = mutableMapOf(\"a\" to 1)\nprintln(m.put(\"a\", 2))   // 1",
    ),
    (
        "remove",
        "Map Members",
        "MutableMap.remove(key: K): Boolean",
        "Drops the entry under a key. It answers a `Boolean` — whether anything was removed — where Kotlin answers the removed value or `null`.",
        "val m = mutableMapOf(\"a\" to 1)\nprintln(m.remove(\"a\"))   // true",
    ),
    // ── Pair Members ──
    (
        "first",
        "Pair Members",
        "Pair.first: A",
        "The left half of a `Pair`. Also reachable as `component1()`, which is what destructuring uses.",
        "println((1 to \"one\").first)   // 1",
    ),
    (
        "second",
        "Pair Members",
        "Pair.second: B",
        "The right half of a `Pair`, also reachable as `component2()`.",
        "println((1 to \"one\").second)   // one",
    ),
    // ── Numeric Members ──
    (
        "plus",
        "Numeric Members",
        "Int.plus(other: Int): Int\nDouble.plus(other: Double): Double",
        "The method spelling of `+` — the form the operator has to take to be reached through a safe call, as in `count?.plus(1)`. Two integral operands wrap; any `Double` operand makes the result `Double`.",
        "println(2.plus(3))   // 5",
    ),
    (
        "minus",
        "Numeric Members",
        "Int.minus(other: Int): Int\nDouble.minus(other: Double): Double",
        "The method spelling of `-`.",
        "println(5.minus(2))   // 3",
    ),
    (
        "times",
        "Numeric Members",
        "Int.times(other: Int): Int\nDouble.times(other: Double): Double",
        "The method spelling of `*`, wrapping on two integral operands.",
        "println(6.times(7))   // 42",
    ),
    (
        "div",
        "Numeric Members",
        "Int.div(other: Int): Int\nDouble.div(other: Double): Double",
        "The method spelling of `/`. Two integral operands truncate toward zero and a zero divisor raises `ArithmeticException: / by zero`; a `Double` operand switches to IEEE division.",
        "println(7.div(2))     // 3\nprintln(2.5.div(2.0)) // 1.25",
    ),
    (
        "rem",
        "Numeric Members",
        "Int.rem(other: Int): Int\nDouble.rem(other: Double): Double",
        "The method spelling of `%`, taking the dividend's sign for integers and raising on a zero integral divisor.",
        "println(7.rem(2))   // 1",
    ),
    (
        "toDouble",
        "Numeric Members",
        "Int.toDouble(): Double\nDouble.toDouble(): Double",
        "Widens to `Double`, which is what makes a following `/` divide in IEEE rather than truncate.",
        "println(3.toDouble())   // 3.0",
    ),
    (
        "toInt",
        "Numeric Members",
        "Int.toInt(): Int\nLong.toInt(): Int\nDouble.toInt(): Int\nString.toInt(): Int",
        "Narrows to 32 bits. From a `Long` that is a truncation of the low 32 bits (`2147483648L.toInt()` is `-2147483648`); from a `Double` it truncates toward zero and then saturates at the `Int` bounds; from a `String` it parses the text.",
        "println(3.9.toInt())            // 3\nprintln(2147483648L.toInt())    // -2147483648",
    ),
    (
        "toLong",
        "Numeric Members",
        "Int.toLong(): Long\nDouble.toLong(): Long\nString.toLong(): Long",
        "Widens to 64 bits, which stops the surrounding arithmetic from being narrowed back to `Int`. From a `Double` it truncates toward zero and saturates at the `Long` bounds.",
        "println(3.9.toLong())                // 3\nprintln(2147483647.toLong() + 1L)    // 2147483648",
    ),
    (
        "toShort",
        "Numeric Members",
        "Int.toShort(): Short",
        "Truncates to the low 16 bits, signed — so `70000.toShort()` is `4464`. The result promotes back to `Int` for arithmetic, as Kotlin's does.",
        "println(32768.toShort())   // -32768",
    ),
    (
        "toByte",
        "Numeric Members",
        "Int.toByte(): Byte",
        "Truncates to the low 8 bits, signed — so `200.toByte()` is `-56`. The result promotes back to `Int` for arithmetic, as Kotlin's does.",
        "println(200.toByte())   // -56",
    ),
    (
        "toChar",
        "Numeric Members",
        "Int.toChar(): Char",
        "The `Char` for the low 16 bits of the receiver — the inverse of `Char.code`.",
        "println(65.toChar())   // A",
    ),
    // ── Universal & Generated Members ──
    (
        "toString",
        "Universal & Generated Members",
        "Any.toString(): String",
        "Renders any receiver as Kotlin would print it: a `Double` keeps its `.0`, `null` reads as `null`, a `List` as `[a, b]`, a `Map` as `{k=v}`, an array as its JVM descriptor, and a data class as `Name(p=v, …)`. In a program that overrides `toString`, calls route through a re-entrant display builtin so the override is what runs.",
        "println(listOf(1, 2).toString())   // [1, 2]",
    ),
    (
        "hashCode",
        "Universal & Generated Members",
        "Any.hashCode(): Int",
        "An order-independent structural hash over a heap object. Two structurally equal values hash equal — the property a data class's generated `hashCode` needs — but the numbers themselves are not the JVM's.",
        "data class Pt(val x: Int, val y: Int)\nfun main() { println(Pt(1, 2).hashCode() == Pt(1, 2).hashCode()) }   // true",
    ),
    (
        "equals",
        "Universal & Generated Members",
        "Any.equals(other: Any?): Boolean",
        "The method spelling of `==`, with the same structural rules — including a data class comparing only its primary-constructor properties and an `Array` comparing by identity.",
        "println(listOf(1).equals(listOf(1)))   // true",
    ),
    (
        "componentN",
        "Universal & Generated Members",
        "component1(): T  component2(): T  …",
        "Positional accessors, 1-based, used by `val (a, b) = expr` destructuring. Defined on a data-class instance (over its primary-constructor properties, skipping inherited fields), a `List`, a `Set`, an `Array` and a `Pair`. Destructuring works in a `val` declaration only — a `for ((k, v) in …)` header is a parse error here.",
        "data class Pt(val x: Int, val y: Int)\nfun main() { val (a, b) = Pt(1, 2); println(\"$a $b\") }   // 1 2",
    ),
    (
        "copy",
        "Universal & Generated Members",
        "copy(vararg overrides: Any): T",
        "A data class's generated clone-with-overrides. The arguments are **positional**, overriding the leading properties in declaration order — kotlinrs has no named arguments, so `p.copy(y = 9)` is not available; `p.copy(9)` overrides the first property. It calls the primary constructor, so a data class under a superclass re-runs its `: Super(args)` header.",
        "data class Pt(val x: Int, val y: Int)\nfun main() { println(Pt(1, 2).copy(9)) }   // Pt(x=9, y=2)",
    ),
    (
        "message",
        "Universal & Generated Members",
        "Throwable.message: String?",
        "The message a throwable was constructed with, or `null` when it was constructed without one.",
        "try { 1 / 0 } catch (e: Exception) { println(e.message) }   // / by zero",
    ),
    // ── Higher-Order Collection Functions ──
    (
        "map",
        "Higher-Order Collection Functions",
        "map(transform: (T) -> R): List<R>",
        "Applies the lambda to every element and collects the results into a `List`. The receiver may be a `List`, `Set`, `Array` or range — a range materializes first, which is what makes `(1..3).map { … }` work. A `Map` or `Pair` receiver is an unresolved reference.",
        "println(listOf(1, 2, 3).map { it * 2 })   // [2, 4, 6]",
    ),
    (
        "mapIndexed",
        "Higher-Order Collection Functions",
        "mapIndexed(transform: (Int, T) -> R): List<R>",
        "Like `map`, but the lambda takes the element's index first and the element second. There is no `forEachIndexed` or `filterIndexed`.",
        "println(listOf(1, 2, 3).mapIndexed { i, v -> i * v })   // [0, 2, 6]",
    ),
    (
        "flatMap",
        "Higher-Order Collection Functions",
        "flatMap(transform: (T) -> Iterable<R>): List<R>",
        "Applies the lambda to every element and splices each iterable result into one flat `List`. A result that is not iterable contributes nothing rather than raising.",
        "println(listOf(1, 2).flatMap { listOf(it, it) })   // [1, 1, 2, 2]",
    ),
    (
        "filter",
        "Higher-Order Collection Functions",
        "filter(predicate: (T) -> Boolean): List<T>",
        "Keeps the elements whose predicate returns exactly `true`; a `null` or non-Boolean result counts as false. The result is always a `List`, even from a `Set` receiver.",
        "println(listOf(1, 2, 3, 4).filter { it % 2 == 0 })   // [2, 4]",
    ),
    (
        "filterNot",
        "Higher-Order Collection Functions",
        "filterNot(predicate: (T) -> Boolean): List<T>",
        "Keeps the elements whose predicate does *not* hold. There is no `filterNotNull` or `filterIsInstance`.",
        "println(listOf(1, 2, 3).filterNot { it > 2 })   // [1, 2]",
    ),
    (
        "forEach",
        "Higher-Order Collection Functions",
        "forEach(action: (T) -> Unit): Unit",
        "Runs the lambda once per element for its side effect and yields `Unit`.",
        "listOf(1, 2).forEach { print(it) }   // 12",
    ),
    (
        "fold",
        "Higher-Order Collection Functions",
        "fold(initial: R, operation: (R, T) -> R): R",
        "Threads an accumulator through the sequence left to right, starting from the given initial value. The lambda takes the accumulator first. It is the one higher-order function here that takes a non-lambda argument as well.",
        "println(listOf(1, 2, 3).fold(0) { acc, n -> acc + n })   // 6",
    ),
    (
        "reduce",
        "Higher-Order Collection Functions",
        "reduce(operation: (T, T) -> T): T",
        "Like `fold` but seeded with the first element instead of an explicit initial. An empty receiver raises `UnsupportedOperationException: Empty collection can't be reduced.`",
        "println(listOf(1, 2, 3).reduce { a, b -> a * b })   // 6",
    ),
    (
        "any",
        "Higher-Order Collection Functions",
        "any(predicate: (T) -> Boolean): Boolean",
        "True as soon as one element satisfies the predicate, short-circuiting on the first hit. The no-argument `any()` overload is not implemented — use `isNotEmpty()`.",
        "println(listOf(1, 2, 3).any { it > 2 })   // true",
    ),
    (
        "all",
        "Higher-Order Collection Functions",
        "all(predicate: (T) -> Boolean): Boolean",
        "True when every element satisfies the predicate, short-circuiting on the first failure. Vacuously true for an empty receiver.",
        "println(listOf(2, 4).all { it % 2 == 0 })   // true",
    ),
    (
        "none",
        "Higher-Order Collection Functions",
        "none(predicate: (T) -> Boolean): Boolean",
        "True when no element satisfies the predicate, short-circuiting on the first hit.",
        "println(listOf(1, 2).none { it > 5 })   // true",
    ),
    (
        "count",
        "Higher-Order Collection Functions",
        "count(predicate: (T) -> Boolean): Int",
        "How many elements satisfy the predicate. Called with no argument, `count()` is instead the sequence member that reports `size`.",
        "println(listOf(1, 2, 3).count { it > 1 })   // 2",
    ),
    (
        "sumOf",
        "Higher-Order Collection Functions",
        "sumOf(selector: (T) -> Int): Int\nsumOf(selector: (T) -> Double): Double",
        "Sums the lambda's results, yielding an `Int` when every one is integral and a `Double` otherwise — the same rule `sum()` uses.",
        "println(listOf(\"ab\", \"c\").sumOf { it.length })   // 3",
    ),
    (
        "maxByOrNull",
        "Higher-Order Collection Functions",
        "maxByOrNull(selector: (T) -> R): T?",
        "The element whose selector value is largest, or `null` when the receiver is empty. Ties keep the first such element. The selector runs once per element.",
        "println(listOf(\"a\", \"abc\").maxByOrNull { it.length })   // abc",
    ),
    (
        "minByOrNull",
        "Higher-Order Collection Functions",
        "minByOrNull(selector: (T) -> R): T?",
        "The element whose selector value is smallest, or `null` on an empty receiver, keeping the first of any tie.",
        "println(listOf(\"abc\", \"a\").minByOrNull { it.length })   // a",
    ),
    (
        "sortedBy",
        "Higher-Order Collection Functions",
        "sortedBy(selector: (T) -> R): List<T>",
        "Ascending sort by the selector's value, evaluated once per element and stable — equal keys keep their input order.",
        "println(listOf(\"abc\", \"a\").sortedBy { it.length })   // [a, abc]",
    ),
    (
        "sortedByDescending",
        "Higher-Order Collection Functions",
        "sortedByDescending(selector: (T) -> R): List<T>",
        "Descending sort by the selector's value. The comparison is flipped rather than the result reversed, so ties still come out in input order as Kotlin requires.",
        "println(listOf(\"a\", \"abc\").sortedByDescending { it.length })   // [abc, a]",
    ),
    (
        "associate",
        "Higher-Order Collection Functions",
        "associate(transform: (T) -> Pair<K, V>): Map<K, V>",
        "Builds a `Map` from the `Pair` each lambda call returns. A lambda result that is not a `Pair` raises `kotlin: associate expects a Pair`. Later duplicate keys overwrite earlier ones.",
        "println(listOf(1, 2).associate { it to it * it })   // {1=1, 2=4}",
    ),
    (
        "associateBy",
        "Higher-Order Collection Functions",
        "associateBy(keySelector: (T) -> K): Map<K, T>",
        "Builds a `Map` whose keys are the lambda's results and whose values are the elements — the mirror image of `associateWith`.",
        "println(listOf(\"ab\", \"c\").associateBy { it.length })   // {2=ab, 1=c}",
    ),
    (
        "associateWith",
        "Higher-Order Collection Functions",
        "associateWith(valueSelector: (T) -> V): Map<T, V>",
        "Builds a `Map` keyed by the elements, with the lambda's results as the values.",
        "println(listOf(1, 2, 3).associateWith { it * 2 })   // {1=2, 2=4, 3=6}",
    ),
    (
        "groupBy",
        "Higher-Order Collection Functions",
        "groupBy(keySelector: (T) -> K): Map<K, List<T>>",
        "Buckets the elements by the lambda's result. Keys appear in first-encounter order and each bucket keeps its elements in input order. There is no `groupingBy`.",
        "println(listOf(1, 2, 3, 4).groupBy { it % 2 })   // {1=[1, 3], 0=[2, 4]}",
    ),
    // ── Scope Functions ──
    (
        "let",
        "Scope Functions",
        "T.let(block: (T) -> R): R",
        "Runs the block with the receiver bound to `it` and yields the block's result. Works on any receiver, not just a collection. Paired with a safe call (`x?.let { … }`) it is the null-guard idiom.",
        "println(listOf(1, 2).let { it.size })   // 2",
    ),
    (
        "also",
        "Scope Functions",
        "T.also(block: (T) -> Unit): T",
        "Runs the block with the receiver bound to `it` for its side effect and yields the **receiver**, not the block's result — which is what makes it chainable mid-expression.",
        "println(listOf(1, 2).also { print(it.size) })   // 2[1, 2]",
    ),
    (
        "takeIf",
        "Scope Functions",
        "T.takeIf(predicate: (T) -> Boolean): T?",
        "Yields the receiver when the predicate returns `true`, and `null` otherwise.",
        "println(5.takeIf { it > 3 })   // 5\nprintln(2.takeIf { it > 3 })   // null",
    ),
    (
        "takeUnless",
        "Scope Functions",
        "T.takeUnless(predicate: (T) -> Boolean): T?",
        "The negation of `takeIf`: yields the receiver when the predicate returns `false`, and `null` otherwise.",
        "println(5.takeUnless { it > 3 })   // null\nprintln(2.takeUnless { it > 3 })   // 2",
    ),
    (
        "run",
        "Scope Functions",
        "T.run(block: T.() -> R): R\nrun(block: () -> R): R",
        "Runs the block with the receiver bound to **`this`** — so the receiver's members are reachable without a qualifier — and yields the block's result. The receiverless form `run { … }` is a block evaluated on the spot for its value.",
        "println(\"abc\".run { length })   // 3\nprintln(run { 1 + 2 })         // 3",
    ),
    (
        "apply",
        "Scope Functions",
        "T.apply(block: T.() -> Unit): T",
        "Runs the block with the receiver bound to **`this`** for its side effect and yields the **receiver** — the configure-then-return idiom. `also` is the same shape with the receiver as `it` instead.",
        "class Box(var w: Int)\nprintln(Box(1).apply { w = 5 }.w)   // 5",
    ),
    (
        "with",
        "Scope Functions",
        "with(receiver: T, block: T.() -> R): R",
        "The free-function spelling of `run`: the argument becomes the block's `this`, and the block's result is the value.",
        "println(with(\"hello\") { uppercase() + length })   // HELLO5",
    ),
    // ── Result ──
    (
        "runCatching",
        "Result",
        "runCatching(block: () -> T): Result<T>",
        "Runs the block and packages its outcome: `Success(v)` for a normal return, `Failure(<throwable>)` for a `throw` — including the runtime faults this frontend raises, so `runCatching { 1 / 0 }` is a failure rather than a halt.",
        "println(runCatching { 6 / 2 })   // Success(3)\nprintln(runCatching { 1 / 0 }.isFailure)   // true",
    ),
    (
        "getOrNull",
        "Result",
        "Result<T>.getOrNull(): T?",
        "The success value, or `null` on failure. `exceptionOrNull()` is its mirror — the throwable, or `null` on success.",
        "println(runCatching { 6 / 2 }.getOrNull())   // 3\nprintln(runCatching { 1 / 0 }.getOrNull())   // null",
    ),
    (
        "getOrElse",
        "Result",
        "Result<T>.getOrElse(onFailure: (Throwable) -> T): T",
        "The success value, or the block applied to the throwable. The block does not run at all on success.",
        "println(runCatching { 1 / 0 }.getOrElse { -1 })   // -1",
    ),
    (
        "isSuccess",
        "Result",
        "Result<T>.isSuccess: Boolean\nResult<T>.isFailure: Boolean",
        "Which branch of the union the result holds. `onSuccess`/`onFailure` run a block for the matching branch and yield the result unchanged; `map` transforms a success and passes a failure through.",
        "println(runCatching { 6 / 2 }.isSuccess)   // true\nprintln(runCatching { 6 / 2 }.map { it + 1 })   // Success(4)",
    ),
    // ── Keywords & Declarations (later additions) ──
    (
        "companion",
        "Keywords & Declarations",
        "class C { companion object { val K = 7; fun of(…): C = … } }",
        "Declares the class's singleton companion. Its properties and functions are reached through the class name (`C.K`, `C.of(…)`) and, from inside the class, without any qualifier. One per class; a named companion is reached the same way.",
        "class C { companion object { val K = 7 } }\nprintln(C.K)   // 7",
    ),
    (
        "vararg",
        "Keywords & Declarations",
        "fun f(vararg xs: T)",
        "Collects the call's trailing positional arguments into an array of the declared element type, which the body iterates or measures like any array. Supported as the last parameter.",
        "fun total(vararg xs: Int): Int { var t = 0; for (x in xs) t += x; return t }\nprintln(total(1, 2, 3))   // 6",
    ),
    (
        "by",
        "Keywords & Declarations",
        "val name: T by lazy { … }",
        "Property delegation. Only `by lazy` is supported: the block runs at the FIRST read and its value is cached, so an initializer with an effect fires at use rather than at startup. `lazy` requires `val`; any other delegate is a compile error.",
        "val z: Int by lazy { println(\"forcing\"); 42 }\nfun main() { println(\"before\"); println(z); println(z) }",
    ),
    (
        "as",
        "Operators",
        "value as T\nvalue as? T",
        "A checked cast. The runtime value is unchanged — what the cast supplies is the static type `T`, which then decides integer width and `/` dispatch downstream. A mismatch throws `ClassCastException`; the safe form `as?` yields `null` instead. `Int` and `Long` share one runtime representation here, so a cast cannot tell them apart.",
        "val a: Any = 5\nprintln((a as Int) / 2)   // 2\nprintln(a as? String)     // null",
    ),
];
/// The reference corpus, exposed for offline doc generation (`gen-docs`).
pub fn corpus() -> &'static [Entry] {
    CORPUS
}

/// Open document text keyed by URI, kept current from the sync notifications so
/// hover can look up the identifier under the cursor.
type Docs = HashMap<String, String>;

/// Entry point for `kotlin --lsp`.
pub fn run() -> Result<(), String> {
    spawn_orphan_guard();
    let (conn, io_threads) = Connection::stdio();
    let (init_id, _params) = conn
        .initialize_start()
        .map_err(|e| format!("lsp initialize: {e}"))?;
    let init_result = serde_json::json!({
        "capabilities": server_capabilities(),
        "serverInfo": { "name": "kotlinrs", "version": env!("CARGO_PKG_VERSION") },
    });
    conn.sender
        .send(Response::new_ok(init_id, init_result).into())
        .map_err(|e| format!("lsp send: {e}"))?;

    let mut docs: Docs = HashMap::new();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn
                    .handle_shutdown(&req)
                    .map_err(|e| format!("lsp shutdown: {e}"))?
                {
                    break;
                }
                dispatch_request(&conn, &docs, req);
            }
            Message::Notification(not) => dispatch_notification(&conn, &mut docs, not),
            Message::Response(_) => {}
        }
    }
    drop(conn);
    io_threads.join().map_err(|_| "lsp io join".to_string())?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

fn handle<P, R>(conn: &Connection, req: Request, f: impl FnOnce(P) -> R)
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    let method = req.method.clone();
    let id = req.id.clone();
    match req.extract::<P>(&method) {
        Ok((id, params)) => {
            let value = serde_json::to_value(f(params)).unwrap_or(serde_json::Value::Null);
            let _ = conn.sender.send(Response::new_ok(id, value).into());
        }
        Err(ExtractError::JsonError { error, .. }) => {
            let _ = conn.sender.send(
                Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()).into(),
            );
        }
        Err(ExtractError::MethodMismatch(_)) => unreachable!("method matched before extract"),
    }
}

fn dispatch_request(conn: &Connection, docs: &Docs, req: Request) {
    match req.method.as_str() {
        Completion::METHOD => handle(conn, req, |_p: CompletionParams| completions()),
        HoverRequest::METHOD => handle(conn, req, |p: HoverParams| hover(docs, &p)),
        _ => {
            let _ = conn.sender.send(
                Response::new_err(req.id, ErrorCode::MethodNotFound as i32, "unhandled".into())
                    .into(),
            );
        }
    }
}

fn dispatch_notification(conn: &Connection, docs: &mut Docs, not: lsp_server::Notification) {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.insert(uri.as_str().to_string(), p.text_document.text.clone());
                publish_diagnostics(conn, &uri, &p.text_document.text);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(not.params) {
                if let Some(change) = p.content_changes.into_iter().last() {
                    let uri = p.text_document.uri;
                    docs.insert(uri.as_str().to_string(), change.text.clone());
                    publish_diagnostics(conn, &uri, &change.text);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(not.params) {
                let uri = p.text_document.uri;
                docs.remove(uri.as_str());
                publish_diagnostics(conn, &uri, "");
            }
        }
        _ => {}
    }
}

/// The LSP completion-item kind a corpus chapter maps to: a keyword, a type, an
/// operator, or (for every function and member chapter) a callable.
fn completion_kind(chapter: &str) -> CompletionItemKind {
    match chapter {
        "Keywords & Declarations" => CompletionItemKind::KEYWORD,
        "Types" => CompletionItemKind::CLASS,
        "Operators" => CompletionItemKind::OPERATOR,
        "Throwables" => CompletionItemKind::CLASS,
        _ => CompletionItemKind::FUNCTION,
    }
}

fn completions() -> CompletionResponse {
    let items = CORPUS
        .iter()
        .map(|(name, chapter, sig, doc, _example)| CompletionItem {
            label: name.to_string(),
            kind: Some(completion_kind(chapter)),
            detail: Some(format!("{} — {}", sig.lines().next().unwrap_or(name), doc)),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

/// Hover: look up the identifier under the cursor in the corpus and render its
/// chapter, doc, and example. Falls back to a short banner otherwise.
fn hover(docs: &Docs, params: &HoverParams) -> Hover {
    let pos = params.text_document_position_params.position;
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .as_str();
    let word = docs
        .get(uri)
        .and_then(|text| word_at(text, pos))
        .unwrap_or_default();

    let matches: Vec<&Entry> = CORPUS.iter().filter(|(name, ..)| *name == word).collect();

    let body = if matches.is_empty() {
        "**kotlinrs** — Kotlin on the fusevm bytecode VM + Cranelift JIT.".to_string()
    } else {
        let mut out = String::new();
        for (name, chapter, sig, doc, example) in matches {
            out.push_str(&format!(
                "**`{name}`** — _{chapter}_\n\n```kotlin\n{sig}\n```\n\n{doc}\n\n```kotlin\n{example}\n```\n\n"
            ));
        }
        out.trim_end().to_string()
    };

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: body,
        }),
        range: None,
    }
}

/// Extract the identifier (`[A-Za-z0-9_]+`) spanning the given position, if any.
fn word_at(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let col = (pos.character as usize).min(chars.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';

    let mut start = col;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

fn publish_diagnostics(conn: &Connection, uri: &Uri, text: &str) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: compute_diagnostics(text),
        version: None,
    };
    let not = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
    let _ = conn.sender.send(not.into());
}

/// Parse the whole document with the runtime's own parser; a syntax error maps
/// to a single diagnostic on the line named in its `(line N)` suffix.
fn compute_diagnostics(text: &str) -> Vec<Diagnostic> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    // Snippets without `fun main` are wrapped the same way the runtime wraps a
    // `-e` one-liner, so an editor scratch buffer of bare statements does not
    // report a spurious "expected fun" error.
    let prepared = crate::prepare_source(text);
    let wrapped = prepared != text;
    match crate::parser::parse_program(&prepared) {
        Ok(_) => Vec::new(),
        Err(e) => {
            // When wrapped, the reported line is offset by the injected `fun main`
            // header line; shift it back so it points at the user's source.
            let raw = parse_error_line(&e);
            let line = if wrapped { raw.saturating_sub(1) } else { raw }.saturating_sub(1);
            vec![Diagnostic {
                range: Range {
                    start: Position { line, character: 0 },
                    end: Position {
                        line,
                        character: 200,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: e,
                ..Default::default()
            }]
        }
    }
}

/// Extract the (1-based) line number from a kotlinrs parser error, which embeds
/// it as `… (line N)`. Defaults to line 1 when no such marker is present.
fn parse_error_line(e: &str) -> u32 {
    e.rsplit_once("(line ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Exit if reparented to pid 1 (the editor died) so we never leak.
fn spawn_orphan_guard() {
    std::thread::spawn(|| {
        #[cfg(target_os = "linux")]
        // SAFETY: prctl(PR_SET_PDEATHSIG, ...) only registers a signal disposition.
        unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            // SAFETY: getppid takes no arguments and never fails.
            if unsafe { libc::getppid() } == 1 {
                std::process::exit(0);
            }
        }
    });
}
