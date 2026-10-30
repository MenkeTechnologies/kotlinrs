//! End-to-end language tests: drive the `kotlin` binary and assert on stdout /
//! stderr / exit code. Runs headless on Linux CI (no JVM, no TTY, no network).

use std::process::{Command, Output};

/// Run `-e <src>` and capture the result.
fn eval(src: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg("-e")
        .arg(src)
        .output()
        .expect("spawn kotlin")
}

fn stdout(src: &str) -> String {
    let out = eval(src);
    assert!(
        out.status.success(),
        "expected success for {src:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(stdout("println(2 + 3 * 4)"), "14\n");
    assert_eq!(stdout("println((2 + 3) * 4)"), "20\n");
    assert_eq!(stdout("println(10 - 2 - 3)"), "5\n");
    assert_eq!(stdout("println(-5 + 8)"), "3\n");
}

#[test]
fn integer_division_truncates() {
    assert_eq!(stdout("println(7 / 2)"), "3\n");
    assert_eq!(stdout("println(-7 / 2)"), "-3\n"); // toward zero, like Kotlin
    assert_eq!(stdout("println(7 % 3)"), "1\n");
    assert_eq!(stdout("println(-7 % 3)"), "-1\n"); // sign of dividend
}

#[test]
fn float_division_and_display() {
    assert_eq!(stdout("println(7.0 / 2.0)"), "3.5\n");
    assert_eq!(stdout("println(1.0)"), "1.0\n"); // whole double keeps .0
    assert_eq!(stdout("println(10.0 / 4.0)"), "2.5\n");
}

#[test]
fn boolean_display_and_logic() {
    assert_eq!(stdout("println(true)"), "true\n");
    assert_eq!(stdout("println(3 > 2 && 1 < 0)"), "false\n");
    assert_eq!(stdout("println(3 > 2 || 1 < 0)"), "true\n");
    assert_eq!(stdout("println(!(1 == 1))"), "false\n");
}

#[test]
fn string_templates() {
    assert_eq!(
        stdout(r#"val x = 5; println("x=$x sq=${x * x}")"#),
        "x=5 sq=25\n"
    );
    assert_eq!(stdout(r#"println("a" + "b" + "c")"#), "abc\n");
    assert_eq!(stdout(r#"println("n=" + 42)"#), "n=42\n");
}

#[test]
fn if_expression_value() {
    assert_eq!(stdout("val m = if (3 > 2) 10 else 20; println(m)"), "10\n");
    assert_eq!(stdout("val m = if (3 < 2) 10 else 20; println(m)"), "20\n");
}

#[test]
fn recursion_fibonacci() {
    let src = "fun fib(n: Int): Int { return if (n < 2) n else fib(n-1) + fib(n-2) }\n\
               fun main() { println(fib(10)) }";
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg("-e")
        .arg(src)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "55\n");
}

#[test]
fn for_ranges() {
    assert_eq!(
        stdout("var s = 0; for (i in 1..5) { s += i }; println(s)"),
        "15\n"
    );
    assert_eq!(
        stdout("var s = 0; for (i in 1 until 5) { s += i }; println(s)"),
        "10\n"
    );
    assert_eq!(
        stdout(r#"for (i in 3 downTo 1) { print("$i") }; println("")"#),
        "321\n"
    );
    assert_eq!(
        stdout(r#"for (i in 0 until 6 step 2) { print("$i") }; println("")"#),
        "024\n"
    );
}

#[test]
fn while_and_compound_assign() {
    let src = "var i = 0; var acc = 1; while (i < 5) { acc *= 2; i += 1 }; println(acc)";
    assert_eq!(stdout(src), "32\n"); // 2^5
}

#[test]
fn integer_divide_by_zero_is_uncaught() {
    assert_eq!(
        uncaught_line("fun main() { val z = 0; println(10 / z) }"),
        "Exception in thread \"main\" java.lang.ArithmeticException: / by zero"
    );
}

#[test]
fn unresolved_reference_is_a_compile_error() {
    let out = eval("println(nope)");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unresolved reference"), "stderr was: {err}");
}

#[test]
fn val_cannot_be_reassigned() {
    // A `val` is write-once: reassigning it is a compile error.
    let out = eval("val x = 5; x = 6; println(x)");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("val cannot be reassigned"),
        "stderr was: {err}"
    );

    // Compound assignment to a `val` is equally rejected.
    let out = eval("val x = 5; x += 1");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("val cannot be reassigned"));

    // A function parameter is a read-only `val`.
    let out = eval("fun f(n: Int): Int { n = 3; return n }\nfun main() { println(f(1)) }");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("val cannot be reassigned"));

    // A `var`, by contrast, reassigns fine.
    assert_eq!(stdout("var x = 5; x = 6; println(x)"), "6\n");
}

#[test]
fn block_scoping_drops_inner_bindings() {
    // A binding declared inside an inner block is not visible after the block.
    let out = eval("fun main() { if (true) { val y = 5 }; println(y) }");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unresolved reference"),
        "stderr was: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A `for` loop variable is likewise out of scope after the loop.
    let out = eval("fun main() { for (i in 1..3) { print(i) }; println(i) }");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unresolved reference"));

    // Shadowing inside a block is restored to the outer binding on exit:
    // the inner `x` prints 99, the outer `x` remains 1.
    assert_eq!(
        stdout("fun main() { val x = 1; if (true) { val x = 99; println(x) }; println(x) }"),
        "99\n1\n"
    );
}

#[test]
fn member_and_method_access() {
    // String property + methods dispatched through the host path.
    assert_eq!(stdout(r#"println("hello".length)"#), "5\n");
    assert_eq!(stdout(r#"println("hello".uppercase())"#), "HELLO\n");
    assert_eq!(stdout(r#"println("ABC".lowercase())"#), "abc\n");
    // Int.toString() (the `.` lexes distinctly from a float point).
    assert_eq!(stdout("println(42.toString())"), "42\n");
    // Chained calls: trim then uppercase.
    assert_eq!(stdout(r#"println("  hi  ".trim().uppercase())"#), "HI\n");
    // A member result flows into further expressions.
    assert_eq!(stdout(r#"println("abc".length + 1)"#), "4\n");
}

#[test]
fn single_expression_function_body() {
    // `fun f(...) = expr` desugars to `{ return expr }`.
    assert_eq!(
        stdout("fun sq(n: Int): Int = n * n\nfun main() { println(sq(7)) }"),
        "49\n"
    );
    // Works without a return-type annotation, and with a method call in the body.
    assert_eq!(
        stdout("fun shout(s: String) = s.uppercase()\nfun main() { println(shout(\"hi\")) }"),
        "HI\n"
    );
}

#[test]
fn when_expression_subject_forms() {
    // Literal arms, comma-grouped arms, and `else`, used as an expression.
    assert_eq!(
        stdout(
            r#"val x = 3; println(when (x) { 1 -> "one"; 2, 3 -> "two-or-three"; else -> "other" })"#
        ),
        "two-or-three\n"
    );
    // `in range` membership (inclusive / until / downTo).
    assert_eq!(
        stdout(r#"println(when (7) { in 1..5 -> "low"; in 6..10 -> "mid"; else -> "hi" })"#),
        "mid\n"
    );
    assert_eq!(
        stdout(r#"println(when (5) { in 1 until 5 -> "lt5"; else -> "ge5" })"#),
        "ge5\n" // `until` excludes the upper bound
    );
    // `is Type` runtime checks distinguish String vs Int subjects.
    assert_eq!(
        stdout(
            r#"val s = "hi"; println(when (s) { is Int -> "int"; is String -> "str"; else -> "?" })"#
        ),
        "str\n"
    );
    assert_eq!(
        stdout(r#"println(when (5) { is String -> "str"; is Int -> "int"; else -> "?" })"#),
        "int\n"
    );
    // Negated `!in`.
    assert_eq!(
        stdout(r#"println(when (20) { !in 1..10 -> "out"; else -> "in" })"#),
        "out\n"
    );
    // String-subject equality dispatches through the string comparison path.
    assert_eq!(
        stdout(r#"println(when ("hi") { "yo" -> 1; "hi" -> 2; else -> 3 })"#),
        "2\n"
    );
}

#[test]
fn when_subjectless_and_statement_and_fallthrough() {
    // Subjectless `when` — each arm is a boolean condition.
    assert_eq!(
        stdout(
            r#"val n = 8; println(when { n < 5 -> "small"; n < 10 -> "medium"; else -> "big" })"#
        ),
        "medium\n"
    );
    // `when` as a statement (value discarded); the matched arm runs for effect.
    assert_eq!(
        stdout(r#"when (2) { 1 -> println("a"); 2 -> println("b"); else -> println("c") }"#),
        "b\n"
    );
    // A block arm's last expression is its value.
    assert_eq!(
        stdout(r#"println(when (1) { 1 -> { val y = 10; y + 5 }; else -> 0 })"#),
        "15\n"
    );
    // A non-exhaustive `when` whose value is used is refused: kotlinc 2.4.21
    // reports `'when' expression must be exhaustive. Add an 'else' branch.`
    assert!(prog_err(r#"println(when (9) { 1 -> "a" })"#)
        .contains("'when' expression must be exhaustive. Add an 'else' branch."));
}

#[test]
fn break_and_continue_in_loops() {
    // `break` exits the loop; only 1..4 accumulate before i == 5.
    assert_eq!(
        stdout("var s = 0; for (i in 1..10) { if (i == 5) break; s += i }; println(s)"),
        "10\n"
    );
    // `continue` skips even values; odds 1+3+5+7+9 = 25.
    assert_eq!(
        stdout("var s = 0; for (i in 1..10) { if (i % 2 == 0) continue; s += i }; println(s)"),
        "25\n"
    );
    // `break` out of an otherwise-infinite `while`.
    assert_eq!(
        stdout("var i = 0; while (true) { i += 1; if (i == 4) break }; println(i)"),
        "4\n"
    );
    // `while` + `continue` still re-evaluates the condition; skipping i == 3
    // sums 1+2+4+5+6 = 18.
    assert_eq!(
        stdout("var i = 0; var s = 0; while (i < 6) { i += 1; if (i == 3) continue; s += i }; println(s)"),
        "18\n"
    );
}

#[test]
fn labeled_break_and_continue() {
    // `break@outer` leaves both loops: only (i=1,j=1) runs before j == 2.
    assert_eq!(
        stdout("var hits = 0; outer@ for (i in 1..3) { for (j in 1..3) { if (j == 2) break@outer; hits += 1 } }; println(hits)"),
        "1\n"
    );
    // `continue@outer` advances the outer loop: one inner hit per i (3 total).
    assert_eq!(
        stdout("var hits = 0; outer@ for (i in 1..3) { for (j in 1..3) { if (j == 2) continue@outer; hits += 1 } }; println(hits)"),
        "3\n"
    );
}

#[test]
fn break_continue_and_labels_are_checked() {
    // `break` outside any loop is a compile error.
    let out = eval("break");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("outside a loop"));

    // A `break@label` to an unknown label is a compile error.
    let out = eval("for (i in 1..3) { break@nope }");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unresolved label"));
}

#[test]
fn char_literals_and_arithmetic() {
    // A `Char` literal displays as its character, not its code.
    assert_eq!(stdout("println('A')"), "A\n");
    // `Char + Int` → `Char` (integral, shifts the code unit).
    assert_eq!(stdout("println('A' + 1)"), "B\n");
    // `Char - Char` → `Int` (the distance between code units).
    assert_eq!(stdout("println('D' - 'A')"), "3\n");
    // `.code` is the Int code unit; `Int.toChar()` maps back to a Char.
    assert_eq!(stdout("println('A'.code)"), "65\n");
    assert_eq!(stdout("println(65.toChar())"), "A\n");
    // Char comparisons order by code unit.
    assert_eq!(stdout("println('a' < 'b')"), "true\n");
    assert_eq!(stdout("println('x' == 'x')"), "true\n");
    // Char in interpolation, concatenation, and `.toString()`.
    assert_eq!(stdout(r#"val c = 'Z'; println("letter=$c")"#), "letter=Z\n");
    assert_eq!(stdout(r#"println("x" + 'y')"#), "xy\n");
    assert_eq!(stdout("println('Q'.toString())"), "Q\n");
    // Char subject in a `when`.
    assert_eq!(
        stdout("val c = 'b'; println(when (c) { 'a' -> 1; 'b' -> 2; else -> 3 })"),
        "2\n"
    );
}

#[test]
fn char_is_a_runtime_type_not_an_int() {
    // The point of a real `Char`: it stays a character in an *untyped* position,
    // where the compiler cannot annotate the display. A collection element is
    // the sharpest case — it used to print as the code unit.
    assert_eq!(stdout("println(listOf('a', 'b'))"), "[a, b]\n");
    assert_eq!(stdout("println(setOf('b', 'a'))"), "[b, a]\n");
    assert_eq!(stdout("println(mapOf('a' to 1))"), "{a=1}\n");
    assert_eq!(stdout("println('a' to 'b')"), "(a, b)\n");
    assert_eq!(stdout("val x: Any = 'q'; println(x)"), "q\n");
    // `is` tells `Char` and `Int` apart, which an Int-carried char could not.
    assert_eq!(stdout("val x: Any = 'q'; println(x is Char)"), "true\n");
    assert_eq!(stdout("val x: Any = 'q'; println(x is Int)"), "false\n");
    assert_eq!(stdout("val x: Any = 3; println(x is Char)"), "false\n");
    // Arithmetic and ordering inside a lambda — the operands are statically
    // untyped there, so these lower to native fusevm ops and only reach Kotlin
    // through the numeric hook.
    assert_eq!(stdout("println(listOf('a').map { it + 1 })"), "[b]\n");
    assert_eq!(
        stdout("println(listOf('a', 'z').filter { it < 'm' })"),
        "[a]\n"
    );
    assert_eq!(stdout("println(listOf('c', 'a').sorted())"), "[a, c]\n");
    // `String + Char` in an untyped position concatenates rather than adding.
    assert_eq!(
        stdout(r#"println(listOf('h', 'i').fold("") { a, b -> a + b })"#),
        "hi\n"
    );
    // Iterating and indexing a String yield real Chars, so they display as
    // characters after passing through a collection.
    assert_eq!(
        stdout("val v = mutableListOf<Char>(); for (c in \"hi\") v.add(c); println(v)"),
        "[h, i]\n"
    );
    assert_eq!(stdout(r#"println(listOf("hi"[0]))"#), "[h]\n");
}

#[test]
fn char_ranges() {
    // `'a'..'e'` is a `CharRange`: its elements and its printed form are chars.
    assert_eq!(stdout("println(('a'..'e').toList())"), "[a, b, c, d, e]\n");
    assert_eq!(stdout("println('a'..'e')"), "a..e\n");
    assert_eq!(stdout("println('a'..'z' step 5)"), "a..z step 5\n");
    assert_eq!(
        stdout("println(('a'..'e').reversed())"),
        "e downTo a step 1\n"
    );
    assert_eq!(stdout("println('c' in 'a'..'z')"), "true\n");
    assert_eq!(stdout("println('C' in 'a'..'z')"), "false\n");
    assert_eq!(stdout("for (c in 'a'..'d') print(c)"), "abcd");
    assert_eq!(stdout("for (c in 'd' downTo 'a') print(c)"), "dcba");
}

#[test]
fn char_members() {
    assert_eq!(stdout("println('7'.isDigit())"), "true\n");
    assert_eq!(stdout("println('x'.isDigit())"), "false\n");
    assert_eq!(stdout("println('x'.isLetter())"), "true\n");
    assert_eq!(stdout("println(' '.isWhitespace())"), "true\n");
    assert_eq!(stdout("println('Z'.isUpperCase())"), "true\n");
    assert_eq!(stdout("println('Z'.isLowerCase())"), "false\n");
    assert_eq!(stdout("println('a'.uppercaseChar())"), "A\n");
    assert_eq!(stdout("println('A'.lowercase())"), "a\n");
    assert_eq!(stdout("println('7'.digitToInt())"), "7\n");
    assert_eq!(stdout("println('a'.hashCode())"), "97\n");
    assert_eq!(stdout("println('a'.compareTo('b'))"), "-1\n");
}

#[test]
fn null_safety_operators() {
    // `null` literal and a nullable-typed binding both display as `null`.
    assert_eq!(stdout("println(null)"), "null\n");
    assert_eq!(stdout("val x: Int? = null; println(x)"), "null\n");
    // Elvis `?:` falls back on null, passes through on non-null.
    assert_eq!(stdout("val x: Int? = null; println(x ?: 99)"), "99\n");
    assert_eq!(stdout("val x: Int? = 5; println(x ?: 99)"), "5\n");
    // Safe call `?.` short-circuits to null on a null receiver.
    assert_eq!(
        stdout(r#"val s: String? = null; println(s?.length)"#),
        "null\n"
    );
    assert_eq!(
        stdout(r#"val s: String? = "hello"; println(s?.length)"#),
        "5\n"
    );
    // Safe call combined with Elvis.
    assert_eq!(
        stdout(r#"val s: String? = null; println(s?.uppercase() ?: "EMPTY")"#),
        "EMPTY\n"
    );
    // `!!` passes a non-null value through.
    assert_eq!(
        stdout(r#"val s: String? = "hi"; println(s!!.uppercase())"#),
        "HI\n"
    );
}

#[test]
fn not_null_assertion_throws_on_null() {
    // `!!` on null raises an uncaught NullPointerException — and this one is
    // MESSAGELESS, which `contains("NullPointerException")` could not check.
    assert_eq!(
        uncaught_line("fun main() { val s: String? = null; println(s!!) }"),
        "Exception in thread \"main\" java.lang.NullPointerException"
    );
}

#[test]
fn unit_still_displays_as_kotlin_unit() {
    // Changing null's display to "null" must not regress Unit — a Unit value is
    // rendered statically as `kotlin.Unit`, not `null`.
    assert_eq!(
        stdout("fun f(): Unit {}\nfun main() { println(f()) }"),
        "kotlin.Unit\n"
    );
}

#[test]
fn bytecode_lowers_to_native_ops() {
    // The whole point: arithmetic lowers to native fusevm ops, not host calls.
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg("--dump-bytecode")
        .arg("-e")
        .arg("fun main() { var s = 0; for (i in 1..3) { s += i }; println(s) }")
        .output()
        .unwrap();
    let asm = String::from_utf8(out.stdout).unwrap();
    assert!(asm.contains("Add"), "expected native Add in:\n{asm}");
    assert!(asm.contains("NumLe"), "expected native compare in:\n{asm}");
    assert!(
        asm.contains("JumpIfFalse"),
        "expected native branch in:\n{asm}"
    );
}

// ─── Host object model: classes, data classes, collections, lambdas ───────
//
// These exercise the frontend-owned object heap (`Value::Obj(u32)` handles into
// `src/host.rs`). Each drives a full program with `fun main` through the binary.

/// Run a whole-program source (must contain `fun main`) and return stdout.
fn prog(src: &str) -> String {
    let out = eval(src);
    assert!(
        out.status.success(),
        "expected success for program; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Run a whole-program source expected to fail; return combined stderr.
fn prog_err(src: &str) -> String {
    let out = eval(src);
    assert!(!out.status.success(), "expected failure, got success");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The FIRST stderr line of a run that ended in an uncaught throw, with the
/// `kotlin: ` prefix removed — which is the whole line the JVM writes before
/// its stack trace.
///
/// This exists because `stderr.contains("SomeException")` is a predicate the
/// wrong answer also satisfies. It cannot tell `IndexOutOfBoundsException` from
/// `ArrayIndexOutOfBoundsException`, cannot see the detail message, cannot see
/// the package, and passes whether or not the `Exception in thread "main"`
/// report line is there at all — so it scores a bare `ArithmeticException` and
/// the JVM's `Exception in thread "main" java.lang.ArithmeticException: / by
/// zero` as the same answer. Every caller below compares the whole line.
fn uncaught_line(src: &str) -> String {
    let err = prog_err(src);
    let first = err.lines().next().unwrap_or_default();
    first.strip_prefix("kotlin: ").unwrap_or(first).to_string()
}

#[test]
fn class_primary_ctor_properties_and_methods() {
    // Primary-ctor `val`/`var` become stored properties; a method reads them via
    // implicit `this`; a `var` property is reassignable through `this`.
    let src = "\
class Point(val x: Int, var y: Int) {
    fun sum(): Int = x + y
    fun bump() { y = y + 1 }
}
fun main() {
    val p = Point(3, 4)
    println(p.sum())
    p.bump()
    println(p.sum())
    println(p.x)
    p.y = 100
    println(p.y)
}";
    assert_eq!(prog(src), "7\n8\n3\n100\n");
}

#[test]
fn val_property_cannot_be_reassigned() {
    // A `val` primary-ctor property is write-once — reassigning it is a
    // compile-time error, mirroring Kotlin.
    let err = prog_err("class C(val x: Int)\nfun main() { val c = C(1); c.x = 2 }");
    assert!(
        err.contains("val cannot be reassigned"),
        "stderr was: {err}"
    );
}

#[test]
fn instances_have_distinct_heap_identity() {
    // Two constructions are distinct objects: mutating one must not touch the
    // other (the heap handle is the identity).
    let src = "\
class Box(var n: Int)
fun main() {
    val a = Box(1)
    val b = Box(1)
    a.n = 99
    println(a.n)
    println(b.n)
}";
    assert_eq!(prog(src), "99\n1\n");
}

#[test]
fn data_class_tostring_form() {
    // `data class` renders as `Name(field=value, …)`.
    let src = "\
data class Person(val name: String, val age: Int)
fun main() { println(Person(\"Ann\", 30)) }";
    assert_eq!(prog(src), "Person(name=Ann, age=30)\n");
}

#[test]
fn data_class_structural_equality_and_hashcode() {
    // `==` is structural; equal instances share a hashCode; differing ones don't
    // compare equal.
    let src = "\
data class Pt(val x: Int, val y: Int)
fun main() {
    val a = Pt(1, 2)
    val b = Pt(1, 2)
    val c = Pt(1, 3)
    println(a == b)
    println(a == c)
    println(a != c)
    println(a.hashCode() == b.hashCode())
}";
    assert_eq!(prog(src), "true\nfalse\ntrue\ntrue\n");
}

#[test]
fn data_class_copy_positional_override() {
    // `copy()` clones; `copy(arg)` overrides leading properties in order.
    let src = "\
data class Pt(val x: Int, val y: Int)
fun main() {
    val a = Pt(1, 2)
    println(a.copy())
    println(a.copy(9))
    println(a == a.copy())
}";
    assert_eq!(prog(src), "Pt(x=1, y=2)\nPt(x=9, y=2)\ntrue\n");
}

#[test]
fn data_class_destructuring() {
    // `val (a, b) = p` binds via `componentN`; `_` discards a component.
    let src = "\
data class Pt(val x: Int, val y: Int)
fun main() {
    val p = Pt(10, 20)
    val (a, b) = p
    println(a + b)
    val (_, y) = p
    println(y)
}";
    assert_eq!(prog(src), "30\n20\n");
}

#[test]
fn inheritance_modifiers_are_enforced() {
    // Each of these is rejected by `kotlinc` too. The check matches a member by
    // name AND arity, so a same-named member at another arity stays an overload.
    let cases = [
        (
            "class Base { fun f(): Int = 1 }\n\
             class Sub : Base() { override fun f(): Int = 2 }\n\
             fun main() { println(Sub().f()) }",
            "final, so it cannot be inherited from",
        ),
        (
            "open class Base { fun f(): Int = 1 }\n\
             class Sub : Base() { override fun f(): Int = 2 }\n\
             fun main() { println(Sub().f()) }",
            "is final and cannot be overridden",
        ),
        (
            "open class Base { open fun f(): Int = 1 }\n\
             class Sub : Base() { fun f(): Int = 2 }\n\
             fun main() { println(Sub().f()) }",
            "needs an `override` modifier",
        ),
        (
            "open class Base { open fun f(): Int = 1 }\n\
             class Sub : Base() { override fun g(): Int = 2 }\n\
             fun main() { println(Sub().g()) }",
            "overrides nothing",
        ),
        (
            "interface I { fun f(): Int }\n\
             class C : I { fun f(): Int = 1 }\n\
             fun main() { println(C().f()) }",
            "needs an `override` modifier",
        ),
    ];
    for (src, want) in cases {
        let err = prog_err(src);
        assert!(
            err.contains(want),
            "expected {want:?} for {src:?}, got {err}"
        );
    }
}

#[test]
fn valid_override_shapes_still_compile() {
    // The enforcement above must not reject anything `kotlinc` accepts: an
    // `override` is itself open, an `interface` member is implicitly open, an
    // `abstract` member may be re-declared abstract, a member may be inherited
    // through a silent middle class, and the `Any` members need no user
    // supertype to override.
    let src = "\
interface Base { fun m(): String = \"b\" }
interface Mid : Base { override fun m(): String = \"m\" }
class Impl : Mid { override fun m(): String = \"i/\" + super<Mid>.m() }
object Single : Base { override fun m(): String = \"s\" }
abstract class AA { abstract fun k(): Int }
abstract class AB : AA() { abstract override fun k(): Int }
class AC : AB() { override fun k(): Int = 9 }
open class G { open fun g(): Int = 1 }
open class H : G()
class I2 : H() { override fun g(): Int = 2 }
class Plain { override fun toString(): String = \"P\" }
class MyErr(m: String) : Exception(m)
fun main() {
    println(Impl().m())
    println(Single.m())
    println(AC().k())
    println(I2().g())
    println(H().g())
    println(Plain())
    println(try { throw MyErr(\"x\") } catch (e: MyErr) { e.message })
}";
    assert_eq!(prog(src), "i/m\ns\n9\n2\n1\nP\nx\n");
}

#[test]
fn qualified_super_picks_the_named_supertype() {
    // `super<T>.m()` names WHICH inherited `m` to run. Kotlin requires it when
    // more than one supertype implements the member, so this is the only
    // spelling that compiles for `Both` — an unqualified `super.hi()` there
    // would be ambiguous.
    let src = "\
interface A { fun hi(): String = \"A\" }
interface B { fun hi(): String = \"B\" }
open class Base { open fun hi(): String = \"Base\" }
class Both : A, B {
    override fun hi(): String = super<A>.hi() + \"/\" + super<B>.hi()
}
class Mixed : Base(), A {
    override fun hi(): String = super<Base>.hi() + \"+\" + super<A>.hi()
}
class Plain : Base() {
    override fun hi(): String = \"E(\" + super.hi() + \")\"
}
fun main() {
    println(Both().hi())
    println(Mixed().hi())
    println(Plain().hi())
    val a: A = Both()
    println(a.hi())
}";
    assert_eq!(prog(src), "A/B\nBase+A\nE(Base)\nA/B\n");
}

#[test]
fn qualified_super_rejects_a_type_that_is_not_a_direct_supertype() {
    // Kotlin rejects `super<T>` for a `T` the class does not directly extend.
    let src = "\
interface A { fun hi(): String = \"A\" }
interface B { fun hi(): String = \"B\" }
class Only : A {
    override fun hi(): String = super<B>.hi()
}
fun main() { println(Only().hi()) }";
    let err = prog_err(src);
    assert!(
        err.contains("not a direct supertype"),
        "unexpected diagnostic: {err}"
    );
}

#[test]
fn data_class_inheriting_stored_properties() {
    // Kotlin derives a `data class`'s members from the primary constructor
    // ALONE, so an inherited field is readable but is not part of `toString`,
    // `equals`, `hashCode`, or `componentN`.
    let src = "\
open class Node(val depth: Int)
data class Leaf(val v: Int) : Node(1)
data class Branch(val l: String, val r: Int) : Node(2)
fun main() {
    val f = Leaf(5)
    println(f)
    println(f.depth)
    println(f == Leaf(5))
    println(f == Leaf(6))
    println(f.hashCode() == Leaf(5).hashCode())
    println(Branch(\"x\", 1))
    val (a, b) = Branch(\"x\", 1)
    println(\"$a/$b\")
    println(listOf(Leaf(1), Branch(\"a\", 2)))
    println(setOf(Leaf(1), Leaf(1), Leaf(2)))
}";
    assert_eq!(
        prog(src),
        "Leaf(v=5)\n1\ntrue\nfalse\ntrue\nBranch(l=x, r=1)\nx/1\n\
         [Leaf(v=1), Branch(l=a, r=2)]\n[Leaf(v=1), Leaf(v=2)]\n"
    );
}

#[test]
fn data_class_copy_reruns_the_superclass_constructor() {
    // Kotlin's generated `copy` calls the primary constructor, so a superclass
    // argument written in terms of a constructor parameter is recomputed from
    // the NEW value rather than carried over from the receiver.
    let src = "\
open class Base(val len: Int)
data class W(val s: String) : Base(s.length)
fun main() {
    val w = W(\"abc\")
    println(w.len)
    val z = w.copy(\"zzzzz\")
    println(z)
    println(z.len)
}";
    assert_eq!(prog(src), "3\nW(s=zzzzz)\n5\n");
}

#[test]
fn class_typed_parameters_and_returns() {
    // A function taking and returning a class type dispatches faithfully.
    let src = "\
data class Vec2(val x: Int, val y: Int)
fun add(a: Vec2, b: Vec2): Vec2 = Vec2(a.x + b.x, a.y + b.y)
fun main() { println(add(Vec2(1, 2), Vec2(3, 4))) }";
    assert_eq!(prog(src), "Vec2(x=4, y=6)\n");
}

#[test]
fn method_returning_instance_chains() {
    // A method whose return type is its own class chains method calls.
    let src = "\
data class Box(val n: Int) { fun bump(): Box = Box(n + 1) }
fun main() { println(Box(1).bump().bump()) }";
    assert_eq!(prog(src), "Box(n=3)\n");
}

#[test]
fn implicit_this_method_call() {
    // A bare call inside a method resolves to `this.method()`.
    let src = "\
class Rect(val w: Int, val h: Int) {
    fun area(): Int = w * h
    fun describe(): String = \"area=\" + area()
}
fun main() { println(Rect(3, 4).describe()) }";
    assert_eq!(prog(src), "area=12\n");
}

#[test]
fn object_singleton_holds_state() {
    // An `object` is a single instance with mutable state across calls.
    let src = "\
object Counter {
    var n: Int = 0
    fun inc(): Int { n = n + 1; return n }
}
fun main() {
    println(Counter.inc())
    println(Counter.inc())
    println(Counter.n)
}";
    assert_eq!(prog(src), "1\n2\n2\n");
}

#[test]
fn list_literal_indexing_and_size() {
    let src = "\
fun main() {
    val xs = listOf(10, 20, 30)
    println(xs)
    println(xs.size)
    println(xs[0])
    println(xs[2])
    println(xs.sum())
    println(xs.contains(20))
    println(xs.indexOf(30))
}";
    assert_eq!(prog(src), "[10, 20, 30]\n3\n10\n30\n60\ntrue\n2\n");
}

#[test]
fn mutable_list_add_and_indexed_set() {
    let src = "\
fun main() {
    val xs = mutableListOf(1, 2)
    xs.add(3)
    println(xs)
    xs[0] = 99
    println(xs)
    println(xs.size)
}";
    assert_eq!(prog(src), "[1, 2, 3]\n[99, 2, 3]\n3\n");
}

#[test]
fn map_literal_indexing_and_membership() {
    let src = "\
fun main() {
    val m = mapOf(\"a\" to 1, \"b\" to 2)
    println(m)
    println(m[\"a\"])
    println(m.size)
    println(m.containsKey(\"b\"))
    println(m[\"missing\"])
}";
    // `m["missing"]` is null for an absent key (Kotlin operator get is nullable).
    assert_eq!(prog(src), "{a=1, b=2}\n1\n2\ntrue\nnull\n");
}

#[test]
fn mutable_map_put_and_keys_values() {
    let src = "\
fun main() {
    val m = mutableMapOf(\"a\" to 1)
    m[\"b\"] = 2
    println(m[\"b\"])
    println(m.keys)
    println(m.values)
}";
    assert_eq!(prog(src), "2\n[a, b]\n[1, 2]\n");
}

#[test]
fn collection_map_filter_foreach_lambdas() {
    // The closure-taking higher-order functions: `map` transforms, `filter`
    // selects, `forEach` runs for effect. `it` is the implicit parameter.
    let src = "\
fun main() {
    val xs = listOf(1, 2, 3, 4)
    println(xs.map { it * 2 })
    println(xs.filter { it % 2 == 0 })
    xs.forEach { println(it) }
}";
    assert_eq!(prog(src), "[2, 4, 6, 8]\n[2, 4]\n1\n2\n3\n4\n");
}

#[test]
fn chained_higher_order_calls() {
    // `filter` returns a `List`, so it chains into `map`.
    let src = "fun main() { println(listOf(1, 2, 3, 4, 5).filter { it > 2 }.map { it * 10 }) }";
    assert_eq!(prog(src), "[30, 40, 50]\n");
}

#[test]
fn lambda_named_parameter_and_multi_statement_body() {
    // A named lambda parameter and a multi-statement body (last expression is
    // the result).
    let src = "\
fun main() {
    val r = listOf(1, 2, 3).map { n ->
        val sq = n * n
        sq + 1
    }
    println(r)
}";
    assert_eq!(prog(src), "[2, 5, 10]\n");
}

#[test]
fn nested_collections_and_dynamic_field_read() {
    // A map of lists, and a property read off an indexed instance (the receiver
    // type is statically unknown, so this pins the host's dynamic field read).
    let src = "\
data class P(val n: String)
fun main() {
    val m = mapOf(1 to listOf(\"a\", \"b\"))
    println(m[1])
    val ps = listOf(P(\"x\"), P(\"y\"))
    println(ps[0].n)
    println(ps[1].n)
}";
    assert_eq!(prog(src), "[a, b]\nx\ny\n");
}

#[test]
fn list_equality_is_structural() {
    let src = "\
fun main() {
    println(listOf(1, 2, 3) == listOf(1, 2, 3))
    println(listOf(1, 2) == listOf(1, 2, 3))
}";
    assert_eq!(prog(src), "true\nfalse\n");
}

#[test]
fn pair_to_infix_and_destructuring() {
    let src = "\
fun main() {
    val p = \"k\" to 42
    println(p)
    val (k, v) = p
    println(k)
    println(v)
}";
    assert_eq!(prog(src), "(k, 42)\nk\n42\n");
}

#[test]
fn index_out_of_bounds_throws() {
    // WHICH subclass, and with which numbers. A `listOf` of two or more is
    // array-backed (`Arrays$ArrayList`) and raises the ARRAY subclass, where
    // the `ArrayList` a `mutableListOf` builds raises the plain one — the same
    // Kotlin expression shape, two different class names. So does the result of
    // a `sorted…`, which takes `Iterable.sorted`'s `Collection` fast path and
    // comes back wrapping a bare array. Asserting `contains("IndexOutOfBounds")`
    // scored all three the same, and it is a substring of the array subclass's
    // name too, so it could not fail whichever one was raised.
    assert_eq!(
        uncaught_line("fun main() { val xs = listOf(1, 2); println(xs[5]) }"),
        "Exception in thread \"main\" java.lang.ArrayIndexOutOfBoundsException: \
         Index 5 out of bounds for length 2"
    );
    assert_eq!(
        uncaught_line("fun main() { val xs = mutableListOf(1, 2); println(xs[5]) }"),
        "Exception in thread \"main\" java.lang.IndexOutOfBoundsException: \
         Index 5 out of bounds for length 2"
    );
    assert_eq!(
        uncaught_line("fun main() { val xs = listOf(3, 1, 2).sorted(); println(xs[5]) }"),
        "Exception in thread \"main\" java.lang.ArrayIndexOutOfBoundsException: \
         Index 5 out of bounds for length 3"
    );
    // The two small sizes collapse onto `optimizeReadOnlyList`'s own classes,
    // which word the fault a third and fourth way.
    assert_eq!(
        uncaught_line("fun main() { val xs = listOf<Int>(); println(xs[5]) }"),
        "Exception in thread \"main\" java.lang.IndexOutOfBoundsException: \
         Empty list doesn't contain element at index 5."
    );
    assert_eq!(
        uncaught_line("fun main() { val xs = listOf(1); println(xs[5]) }"),
        "Exception in thread \"main\" java.lang.IndexOutOfBoundsException: Index: 5, Size: 1"
    );
}

#[test]
fn constructor_lowers_to_host_extension_op() {
    // A construction lowers to the `KT_NEW` (Extended) host op over the object
    // heap — not a native array/hash — confirming the heap-backed model.
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg("--dump-bytecode")
        .arg("-e")
        .arg("data class P(val x: Int)\nfun main() { println(P(1)) }")
        .output()
        .unwrap();
    let asm = String::from_utf8(out.stdout).unwrap();
    assert!(
        asm.contains("Extended"),
        "expected a host Extended op in:\n{asm}"
    );
}

// ── First-class lambda values ───────────────────────────────────────────────

#[test]
fn lambda_value_binding_and_invocation() {
    // A lambda stored in a `val` of function type, then invoked by `f(args)`.
    assert_eq!(
        stdout("val f: (Int) -> Int = { it * 2 }\nprintln(f(3))"),
        "6\n"
    );
    // Implicit `it` and an explicit single parameter are interchangeable.
    assert_eq!(
        stdout("val g = { x: Int -> x + 100 }\nprintln(g(5))"),
        "105\n"
    );
}

#[test]
fn lambda_multiple_parameters() {
    assert_eq!(
        stdout("val add = { a: Int, b: Int -> a + b }\nprintln(add(2, 5))"),
        "7\n"
    );
}

#[test]
fn lambda_captures_enclosing_scope() {
    // The lambda reads `n` from the enclosing frame — a by-value upvalue capture.
    let src = "\
fun main() {
    val n = 10
    val addN = { x: Int -> x + n }
    println(addN(5))
    println(addN(20))
}";
    assert_eq!(prog(src), "15\n30\n");
}

#[test]
fn lambda_capture_survives_returning_frame() {
    // A lambda returned from a function still sees the captured `n` after the
    // defining frame has returned — the capture is stored by value in the handle.
    let src = "\
fun adder(n: Int): (Int) -> Int = { it + n }
fun main() {
    val add100 = adder(100)
    val add1 = adder(1)
    println(add100(5))
    println(add1(5))
}";
    assert_eq!(prog(src), "105\n6\n");
}

#[test]
fn function_type_parameter_is_invoked() {
    // A function-typed parameter is a first-class value the callee invokes.
    let src = "\
fun apply(f: (Int) -> Int, x: Int) = f(x)
fun main() {
    println(apply({ it + 1 }, 41))
    println(apply({ it * it }, 9))
}";
    assert_eq!(prog(src), "42\n81\n");
}

#[test]
fn trailing_lambda_on_free_function() {
    // A trailing-lambda call with no parenthesized args: `run2 { … }`.
    let src = "\
fun run2(f: (Int) -> Int) = f(10)
fun main() {
    println(run2 { it * 3 })
}";
    assert_eq!(prog(src), "30\n");
}

#[test]
fn nested_curried_closures() {
    // A closure returning a closure — the inner one captures the outer parameter.
    let src = "\
fun main() {
    val make = { x: Int -> { y: Int -> x + y } }
    val add10 = make(10)
    val add100 = make(100)
    println(add10(5))
    println(add100(1))
}";
    assert_eq!(prog(src), "15\n101\n");
}

#[test]
fn lambda_body_uses_host_ops_via_reentrant_run() {
    // The lambda body runs on a re-entrant `vm.run()`; string interpolation
    // (a `KT_*` host op) inside the body must still resolve — proving the
    // extension handler stays live across the nested run (lambda invocation is a
    // `CallBuiltin`, which does not take/restore the handler).
    assert_eq!(
        stdout("val g = { x: Int -> \"val=$x\" }\nprintln(g(7))"),
        "val=7\n"
    );
}

#[test]
fn lambda_lowers_to_make_closure_builtin() {
    // A lambda literal lowers to the `KT_MAKE_CLOSURE` builtin (a `CallBuiltin`,
    // id 100) that registers a heap closure — confirming the heap-closure model
    // rather than any fusevm-core change.
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg("--dump-bytecode")
        .arg("-e")
        .arg("val f = { it * 2 }\nprintln(f(3))")
        .output()
        .unwrap();
    let asm = String::from_utf8(out.stdout).unwrap();
    assert!(
        asm.contains("CallBuiltin"),
        "expected a CallBuiltin (make-closure / closure-call) in:\n{asm}"
    );
}

// ── Higher-order collection functions (real lambda values) ──────────────────

#[test]
fn hof_map_filter_foreach_with_lambda_values() {
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4).map { it * it })"),
        "[1, 4, 9, 16]\n"
    );
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4, 5, 6).filter { it % 2 == 0 })"),
        "[2, 4, 6]\n"
    );
    assert_eq!(
        stdout("listOf(\"a\", \"b\").forEach { println(it) }"),
        "a\nb\n"
    );
}

#[test]
fn hof_accepts_a_lambda_passed_by_name() {
    // The HOF takes a first-class lambda VALUE — a variable holding a closure,
    // not just an inline literal.
    let src = "\
fun main() {
    val dbl = { x: Int -> x * 2 }
    println(listOf(1, 2, 3).map(dbl))
}";
    assert_eq!(prog(src), "[2, 4, 6]\n");
}

#[test]
fn hof_fold_and_reduce() {
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4).fold(0) { acc, x -> acc + x })"),
        "10\n"
    );
    // `fold`'s initial seeds the accumulator (here building a String — the
    // accumulator param is annotated `String` so `+` is concatenation, not
    // arithmetic, under the coarse typing).
    assert_eq!(
        stdout("println(listOf(1, 2, 3).fold(\"n\") { acc: String, x: Int -> acc + x })"),
        "n123\n"
    );
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4, 5).reduce { a, b -> a + b })"),
        "15\n"
    );
}

#[test]
fn hof_reduce_on_empty_collection_throws() {
    // Kotlin `reduce` on an empty collection throws UnsupportedOperationException.
    let err =
        prog_err("fun main() { println(listOf(1).filter { it > 9 }.reduce { a, b -> a + b }) }");
    assert!(
        err.contains("UnsupportedOperationException"),
        "stderr was: {err}"
    );
}

#[test]
fn hof_any_all_count() {
    assert_eq!(stdout("println(listOf(1, 2, 3).any { it > 2 })"), "true\n");
    assert_eq!(stdout("println(listOf(1, 2, 3).any { it > 9 })"), "false\n");
    assert_eq!(stdout("println(listOf(1, 2, 3).all { it > 0 })"), "true\n");
    assert_eq!(stdout("println(listOf(1, 2, 3).all { it > 1 })"), "false\n");
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4, 5).count { it % 2 == 1 })"),
        "3\n"
    );
}

#[test]
fn hof_sum_of_and_max_by() {
    assert_eq!(stdout("println(listOf(1, 2, 3).sumOf { it * it })"), "14\n");
    // `maxByOrNull` returns the ELEMENT with the greatest selector value.
    assert_eq!(
        stdout("println(listOf(1, 2, 3).maxByOrNull { -it })"),
        "1\n"
    );
    assert_eq!(
        stdout("println(listOf(\"a\", \"bbb\", \"cc\").maxByOrNull { it.length })"),
        "bbb\n"
    );
}

#[test]
fn hof_sorted_by_is_stable_and_selector_driven() {
    assert_eq!(
        stdout("println(listOf(3, 1, 2).sortedBy { it })"),
        "[1, 2, 3]\n"
    );
    // Sort by a derived key (descending via negation).
    assert_eq!(
        stdout("println(listOf(1, 2, 3).sortedBy { -it })"),
        "[3, 2, 1]\n"
    );
}

#[test]
fn hof_group_by_and_associate_with() {
    // groupBy keeps keys in first-appearance order, values in input order.
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4).groupBy { it % 2 })"),
        "{1=[1, 3], 0=[2, 4]}\n"
    );
    assert_eq!(
        stdout("println(listOf(1, 2, 3).associateWith { it * 10 })"),
        "{1=10, 2=20, 3=30}\n"
    );
}

#[test]
fn hof_chained_pipeline() {
    // Each stage takes a fresh lambda value; the result of one feeds the next.
    assert_eq!(
        stdout("println(listOf(1, 2, 3, 4, 5, 6).filter { it % 2 == 0 }.map { it * 10 })"),
        "[20, 40, 60]\n"
    );
}

#[test]
fn lambda_can_close_over_this_and_mutate_a_field() {
    // A lambda defined in a method captures the enclosing `this`; mutating a
    // `var` field through it is visible after the call (the instance is a shared
    // heap handle).
    let src = "\
class Counter(var n: Int) {
    fun addAll(xs: List<Int>) {
        xs.forEach { n = n + it }
    }
}
fun main() {
    val c = Counter(0)
    c.addAll(listOf(1, 2, 3, 4))
    println(c.n)
}";
    assert_eq!(prog(src), "10\n");
}

#[test]
fn typed_lambda_param_uses_integer_division() {
    // An explicitly `Int`-typed lambda parameter drives Kotlin integer division
    // (truncating), and division by zero throws ArithmeticException.
    assert_eq!(
        stdout("val d = { a: Int, b: Int -> a / b }\nprintln(d(7, 2))"),
        "3\n"
    );
    assert_eq!(
        uncaught_line("fun main() { val d = { a: Int, b: Int -> a / b }; println(d(10, 0)) }"),
        "Exception in thread \"main\" java.lang.ArithmeticException: / by zero"
    );
}

// ── Scope functions ─────────────────────────────────────────────────────────

#[test]
fn scope_function_let_transforms_receiver() {
    assert_eq!(stdout("val n = 5\nprintln(n.let { it * 2 })"), "10\n");
    assert_eq!(stdout("println(\"hi\".let { it.uppercase() })"), "HI\n");
}

#[test]
fn scope_function_also_returns_receiver() {
    // `also` runs the block for its side effect and yields the receiver itself.
    let src = "\
fun main() {
    val xs = mutableListOf(1, 2)
    val same = xs.also { it.add(3) }
    println(same)
}";
    assert_eq!(prog(src), "[1, 2, 3]\n");
}

#[test]
fn scope_function_take_if() {
    assert_eq!(stdout("println(10.takeIf { it > 5 })"), "10\n");
    assert_eq!(stdout("println(3.takeIf { it > 5 })"), "null\n");
}

// ── Ranges ──────────────────────────────────────────────────────────────────

#[test]
fn range_tostring_distinguishes_intrange_from_intprogression() {
    // The two Kotlin range types print differently: an `IntRange` shows its
    // endpoints, an `IntProgression` shows its step AND its last *reachable*
    // element (`1..10 step 3` reaches 10, `1..10 step 2` stops at 9).
    assert_eq!(stdout("println(1..5)"), "1..5\n");
    assert_eq!(stdout("println(1 until 5)"), "1..4\n");
    assert_eq!(stdout("println(5 downTo 1)"), "5 downTo 1 step 1\n");
    assert_eq!(stdout("println(1..10 step 2)"), "1..9 step 2\n");
    assert_eq!(stdout("println(1..10 step 3)"), "1..10 step 3\n");
    assert_eq!(
        stdout("println(10 downTo 1 step 3)"),
        "10 downTo 1 step 3\n"
    );
    assert_eq!(stdout("println(1..0)"), "1..0\n"); // empty ranges still print
}

#[test]
fn range_aggregates_and_membership() {
    assert_eq!(stdout("println((1..3).sum())"), "6\n");
    assert_eq!(stdout("println((1 until 5).sum())"), "10\n");
    assert_eq!(stdout("println((5 downTo 1).sum())"), "15\n");
    assert_eq!(stdout("println((1..5 step 2).sum())"), "9\n");
    assert_eq!(stdout("println((1..0).sum())"), "0\n");
    assert_eq!(stdout("println((1..5).count())"), "5\n");
    assert_eq!(stdout("println((1..4).toList())"), "[1, 2, 3, 4]\n");
    assert_eq!(stdout("println((1..3).map { it * 2 })"), "[2, 4, 6]\n");
    // Membership on a progression is step-aligned, not just a bounds test.
    assert_eq!(stdout("println(3 in 1..5)"), "true\n");
    assert_eq!(stdout("println(7 !in 1..5)"), "true\n");
    assert_eq!(stdout("println(4 in (1..10 step 2))"), "false\n");
    assert_eq!(stdout("println(3 in (1..10 step 2))"), "true\n");
}

#[test]
fn range_is_a_value_usable_in_a_for_header() {
    // A range bound to a name still drives a `for`, through the general
    // iterate-a-value lowering rather than the counted one.
    let src = "\
fun main() {
    val r = 1..3
    var s = 0
    for (i in r) s += i
    println(s)
    println(r.first + r.last)
}";
    assert_eq!(prog(src), "6\n4\n");
}

#[test]
fn range_precedence_matches_kotlin() {
    // `..` binds tighter than `step`, looser than `+`/`-`, and `in` is looser
    // than both — so none of these needs parentheses.
    assert_eq!(stdout("val n = 4\nprintln(1..n-1)"), "1..3\n");
    assert_eq!(stdout("println(1..2+3 step 2)"), "1..5 step 2\n");
    assert_eq!(stdout("println(2 in 1..3 == true)"), "true\n");
}

// ── Arrays ──────────────────────────────────────────────────────────────────

#[test]
fn array_indexing_size_and_mutation() {
    let src = "\
fun main() {
    val a = arrayOf(1, 2, 3)
    println(a.size)
    println(a[1])
    a[1] = 9
    println(a[1])
    println(a.sum())
    println(a.joinToString())
    println(a.joinToString(\"-\"))
    println(a.toList())
    println(9 in a)
}";
    assert_eq!(prog(src), "3\n2\n9\n13\n1, 9, 3\n1-9-3\n[1, 9, 3]\ntrue\n");
}

#[test]
fn primitive_arrays_are_zero_filled() {
    let src = "\
fun main() {
    val a = IntArray(3)
    println(a.size)
    println(a.sum())
    a[0] = 5
    println(a.joinToString())
    println(DoubleArray(2).joinToString())
    println(BooleanArray(2).joinToString())
}";
    assert_eq!(prog(src), "3\n0\n5, 0, 0\n0.0, 0.0\nfalse, false\n");
}

#[test]
fn array_iteration_and_reference_equality() {
    // An array inherits `Object.equals`, so equal contents are NOT equal —
    // unlike a `List`, which compares structurally.
    let src = "\
fun main() {
    var t = 0
    for (x in intArrayOf(1, 2, 3)) t += x
    println(t)
    println(arrayOf(1) == arrayOf(1))
    println(listOf(1) == listOf(1))
}";
    assert_eq!(prog(src), "6\nfalse\ntrue\n");
}

// ── kotlin.math / java.lang.Math ────────────────────────────────────────────

#[test]
fn math_functions_need_the_kotlin_math_import() {
    // Kotlin does not auto-import `kotlin.math`, so a bare `abs` without the
    // import is an unresolved reference — the same diagnostic kotlinc gives.
    let err = prog_err("fun main() { println(abs(-3)) }");
    assert!(
        err.contains("unresolved reference: abs"),
        "stderr was: {err}"
    );
    // A single-name import brings in only that name.
    let err = prog_err("import kotlin.math.sqrt\nfun main() { println(abs(-3)) }");
    assert!(
        err.contains("unresolved reference: abs"),
        "stderr was: {err}"
    );
    // …and `as` renames it, so the original spelling is gone.
    let err = prog_err("import kotlin.math.abs as absolute\nfun main() { println(abs(-3)) }");
    assert!(
        err.contains("unresolved reference: abs"),
        "stderr was: {err}"
    );
}

#[test]
fn an_aliased_math_import_moves_the_name_even_under_a_star_import() {
    // A star import is not a floor an alias sits on top of. `as` MOVES the
    // name, and it wins over the star, so a file holding both
    // `import kotlin.math.*` and `import kotlin.math.abs as A` can call `A` and
    // cannot call `abs` — measured against the reference toolchain, which
    // answers `unresolved reference 'abs'` for exactly that pair of lines.
    //
    // The rule is a property of the IMPORT and not of what was imported, so it
    // is checked on all three kinds the star opens: a function, a constant, and
    // an extension member on a numeric receiver.
    for (decl, gone, alias, want) in [
        ("abs as A", "abs(-3)", "A(-3)", "3\n"),
        ("PI as Tau", "PI", "Tau", "3.141592653589793\n"),
        ("roundToInt as rti", "3.7.roundToInt()", "3.7.rti()", "4\n"),
    ] {
        let src = format!("import kotlin.math.*\nimport kotlin.math.{decl}\nfun main() ");
        let err = prog_err(&format!("{src}{{ println({gone}) }}"));
        let bare = gone.split(['(', '.']).next().unwrap_or(gone);
        let named = if bare == "3" { "roundToInt" } else { bare };
        assert!(
            err.contains(&format!("unresolved reference: {named}")),
            "star + `{decl}` must hide `{gone}`; stderr was: {err}"
        );
        // The alias itself resolves, and the star still opens every name the
        // alias did not rename — `max` is untouched by an alias on `abs`.
        assert_eq!(prog(&format!("{src}{{ println({alias}) }}")), want);
        assert_eq!(prog(&format!("{src}{{ println(max(1, 2)) }}")), "2\n");
    }
    // An explicit import of the original spelling puts it back on its own
    // account, so the two lines together leave BOTH spellings callable.
    assert_eq!(
        prog(
            "import kotlin.math.abs\nimport kotlin.math.abs as A\n\
             fun main() { println(\"\" + abs(-3) + A(-4)) }"
        ),
        "34\n"
    );
}

#[test]
fn the_kotlin_math_extension_members_are_gated_on_the_import() {
    // `roundToInt`, `pow`, `absoluteValue` and `sign` are EXTENSIONS declared in
    // `kotlin.math`, not members of the numeric types, so a call with no import
    // line is an unresolved reference exactly as a bare `abs` is — the
    // reference toolchain answers `unresolved reference 'roundToInt'` for
    // `3.7.roundToInt()` in a file with no imports.
    for (name, call, want) in [
        ("roundToInt", "3.7.roundToInt()", "4\n"),
        ("roundToLong", "3.7.roundToLong()", "4\n"),
        ("pow", "2.0.pow(3.0)", "8.0\n"),
        ("absoluteValue", "(-5).absoluteValue", "5\n"),
        ("sign", "(-5).sign", "-1\n"),
    ] {
        let err = prog_err(&format!("fun main() {{ println({call}) }}"));
        assert!(
            err.contains(&format!("unresolved reference: {name}")),
            "`{call}` must need the import; stderr was: {err}"
        );
        assert_eq!(
            prog(&format!(
                "import kotlin.math.*\nfun main() {{ println({call}) }}"
            )),
            want
        );
        assert_eq!(
            prog(&format!(
                "import kotlin.math.{name}\nfun main() {{ println({call}) }}"
            )),
            want
        );
    }
    // The RECEIVER is what makes the name an extension, so a method of one's
    // own that happens to share a spelling is untouched by the gate.
    assert_eq!(
        prog("class K { fun pow(n: Int) = n * 2 }\nfun main() { println(K().pow(3)) }"),
        "6\n"
    );
    // `mod` is a `kotlin` package member, not a `kotlin.math` extension, so it
    // needs no import at all.
    assert_eq!(prog("fun main() { println((-5).mod(3)) }"), "1\n");
}

#[test]
fn math_functions_keep_their_int_and_double_overloads() {
    let src = "\
import kotlin.math.*
fun main() {
    println(abs(-3))
    println(abs(-3.5))
    println(max(2, 9))
    println(min(-2.5, 9.5))
    println(sqrt(9.0))
    println(maxOf(2, 9))
    println(abs(-7) / 2)
    println(PI)
}";
    assert_eq!(prog(src), "3\n3.5\n9\n-2.5\n3.0\n9\n3\n3.141592653589793\n");
}

#[test]
fn kotlin_round_and_java_math_round_differ() {
    // `kotlin.math.round` is half-to-even and yields a `Double`;
    // `java.lang.Math.round` is half-up and yields a `Long`. `java.lang` is
    // auto-imported, so `Math.abs` needs no import line.
    let src = "\
import kotlin.math.*
fun main() {
    println(round(2.5))
    println(round(3.5))
    println(Math.round(2.5))
    println(Math.round(-2.5))
    println(floor(2.7))
    println(ceil(2.1))
}";
    assert_eq!(prog(src), "2.0\n4.0\n3\n-2\n2.0\n3.0\n");
    assert_eq!(stdout("println(Math.abs(-3))"), "3\n");
}

// ── ++ / -- in expression position ──────────────────────────────────────────

#[test]
fn increment_yields_the_pre_or_post_update_value() {
    // Postfix yields the value from before the update, prefix the one after.
    assert_eq!(stdout("var i = 0\nprintln(i++)\nprintln(i)"), "0\n1\n");
    assert_eq!(stdout("var i = 0\nprintln(++i)\nprintln(i)"), "1\n1\n");
    assert_eq!(stdout("var i = 0\nprintln(i--)\nprintln(i)"), "0\n-1\n");
    assert_eq!(stdout("var i = 0\nprintln(--i)"), "-1\n");
    // Both operands are read before either update lands.
    assert_eq!(
        stdout("var j = 5\nprintln(j++ + j++)\nprintln(j)"),
        "11\n7\n"
    );
}

#[test]
fn increment_works_on_elements_and_keeps_the_val_check() {
    let src = "\
fun main() {
    val a = intArrayOf(1, 2)
    println(a[0]++)
    println(a.joinToString())
}";
    assert_eq!(prog(src), "1\n2, 2\n");
    let err = prog_err("fun main() { val n = 1; n++ }");
    assert!(
        err.contains("val cannot be reassigned"),
        "stderr was: {err}"
    );
}

// ─── Exceptions: `try` / `catch` / `finally` / `throw` ────────────────────
//
// fusevm has no unwind opcode, so an in-flight exception is a host-side pending
// value plus compiler-emitted per-statement checks (see the “Exception
// unwinding” sections in `src/host.rs` / `src/compiler.rs`). These tests pin the
// observable contract that protocol has to deliver: values, ordering, and the
// exit status. Every expectation below was captured from the reference
// `kotlinc` + `kotlin` toolchain.

#[test]
fn try_is_an_expression_and_catch_supplies_the_value() {
    let src = "\
fun main() {
    val a = try { 1 / 0 } catch (e: ArithmeticException) { -1 }
    println(a)
    println(try { 6 * 7 } catch (e: Exception) { 0 })
    println(try { throw RuntimeException(\"x\") } catch (e: Exception) { e.message })
}";
    assert_eq!(prog(src), "-1\n42\nx\n");
}

#[test]
fn catch_matches_the_throwable_hierarchy_in_source_order() {
    // A subclass is caught by a supertype arm, and the FIRST matching arm wins.
    let src = "\
fun main() {
    try { throw IllegalArgumentException(\"bad\") } catch (e: RuntimeException) { println(e) }
    try { throw RuntimeException(\"x\") } catch (e: IllegalStateException) { println(\"wrong\") } catch (e: RuntimeException) { println(\"right\") }
    try { throw Error(\"fatal\") } catch (e: Throwable) { println(e.message) }
}";
    assert_eq!(
        prog(src),
        "java.lang.IllegalArgumentException: bad\nright\nfatal\n"
    );
}

#[test]
fn finally_runs_on_both_paths_and_a_finally_throw_wins() {
    let src = "\
fun main() {
    try { println(\"body\") } finally { println(\"fin1\") }
    try { throw RuntimeException(\"a\") } catch (e: Exception) { println(\"c\") } finally { println(\"fin2\") }
    try { try { throw RuntimeException(\"a\") } finally { throw IllegalStateException(\"b\") } } catch (e: Exception) { println(e.message) }
}";
    assert_eq!(prog(src), "body\nfin1\nc\nfin2\nb\n");
}

#[test]
fn a_raise_suppresses_the_rest_of_the_abandoned_statement() {
    // `println` must NOT run once the argument's evaluation threw, and a
    // compound assignment must leave its target's previous value intact.
    let src = "\
fun boom(): Int = throw RuntimeException(\"no\")
fun main() {
    var acc = 7
    try { println(\"v=\" + boom()) } catch (e: Exception) { println(\"caught\") }
    try { acc += 10 / 0 } catch (e: Exception) { println(acc) }
}";
    assert_eq!(prog(src), "caught\n7\n");
}

#[test]
fn an_exception_unwinds_out_of_call_frames_loops_and_lambdas() {
    let src = "\
fun deep(n: Int): Int { if (n == 0) throw IllegalStateException(\"bottom\") ; return deep(n - 1) }
fun main() {
    println(try { deep(3) } catch (e: IllegalStateException) { -9 })
    var sum = 0
    for (i in 1..4) { try { if (i == 3) throw RuntimeException(\"skip\") ; sum += i } catch (e: Exception) { sum += 10 } }
    println(sum)
    try { listOf(1, 2, 3).forEach { if (it == 2) throw RuntimeException(\"stop\") else println(it) } } catch (e: Exception) { println(e.message) }
}";
    assert_eq!(prog(src), "-9\n17\n1\nstop\n");
}

#[test]
fn host_faults_are_catchable_as_their_jvm_exceptions() {
    // The runtime errors kotlinrs already reported become ordinary catchable
    // exceptions once the program has a handler for them.
    let src = "\
fun main() {
    println(try { 1 % 0 } catch (e: ArithmeticException) { -1 })
    val n: String? = null
    println(try { n!!.length } catch (e: NullPointerException) { -2 })
    println(try { listOf(1, 2)[7] } catch (e: IndexOutOfBoundsException) { -3 })
}";
    assert_eq!(prog(src), "-1\n-2\n-3\n");
}

#[test]
fn an_uncaught_exception_reports_like_the_jvm_and_exits_nonzero() {
    let out =
        eval("fun main() {\n    println(\"before\")\n    throw IllegalStateException(\"dead\")\n}");
    assert!(!out.status.success(), "expected a non-zero exit");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Exception in thread \"main\" java.lang.IllegalStateException: dead"),
        "stderr was: {err}"
    );
}

#[test]
fn a_return_out_of_a_try_runs_the_finally_first() {
    // The resource-cleanup idiom: the finalizer runs before the frame is left,
    // on the value path and the exceptional one alike — and nests.
    let src = "\
fun f(n: Int): Int { try { if (n < 0) throw IllegalStateException(\"neg\") ; return n * 2 } finally { println(\"close \" + n) } }
fun g(): String { try { try { return \"deep\" } finally { println(\"inner\") } } finally { println(\"outer\") } }
fun main() {
    println(f(3))
    println(try { f(-1) } catch (e: Exception) { -1 })
    println(g())
}";
    assert_eq!(prog(src), "close 3\n6\nclose -1\n-1\ninner\nouter\ndeep\n");
}

#[test]
fn a_break_or_continue_out_of_a_try_runs_its_finally_first() {
    // A `break`/`continue` leaving a `try` that owns a `finally` must run the
    // finalizer on the way out, exactly as `return` does. The four shapes below
    // are the ones that differ in lowering:
    //   * `break` — the loop ends, so the finalizer runs once and nothing after
    //     the `try` in that iteration does;
    //   * `continue` — the finalizer runs, then the NEXT iteration starts (the
    //     `println` after the `try` is skipped for that one);
    //   * a labeled `break` out of a loop nested INSIDE the `try` — the jump
    //     crosses the `try` even though the `break` is not lexically in its
    //     body, which is the shape a purely syntactic check misses;
    //   * nested `try`s — every finalizer between the jump and its target runs,
    //     innermost first.
    // Output captured from the reference `kotlinc` + `kotlin` toolchain.
    let src = "\
fun main() {
    for (i in 1..3) { try { if (i == 2) break; println(\"b$i\") } finally { println(\"fin$i\") } }
    for (i in 1..3) { try { if (i == 2) continue; println(\"c$i\") } finally { println(\"f$i\") } }
    outer@ for (i in 1..3) { try { for (j in 1..3) { if (j == 2) break@outer } } finally { println(\"nest$i\") } }
    for (i in 1..3) { try { try { if (i == 2) continue } finally { println(\"in$i\") } } finally { println(\"out$i\") } ; println(\"tail$i\") }
}";
    assert_eq!(
        prog(src),
        "b1\nfin1\nfin2\n\
         c1\nf1\nf2\nc3\nf3\n\
         nest1\n\
         in1\nout1\ntail1\nin2\nout2\nin3\nout3\ntail3\n"
    );
}

#[test]
fn a_try_needs_a_catch_or_a_finally() {
    let err = prog_err("fun main() { try { println(1) } }");
    assert!(
        err.contains("at least one `catch` or a `finally`"),
        "stderr was: {err}"
    );
}

#[test]
fn throwables_render_and_carry_their_message() {
    let src = "\
fun main() {
    println(RuntimeException(\"m\"))
    println(RuntimeException(\"m\").message)
    println(IllegalStateException().message)
    val any: Any = IllegalArgumentException(\"z\")
    println(when (any) { is IllegalStateException -> \"ise\" ; is RuntimeException -> \"rte\" ; else -> \"?\" })
}";
    assert_eq!(prog(src), "java.lang.RuntimeException: m\nm\nnull\nrte\n");
}

// ─── Array lambda initializers and String iteration ───────────────────────

#[test]
fn array_constructors_take_an_index_lambda() {
    let src = "\
fun main() {
    val a = IntArray(4) { it * 2 }
    println(a.joinToString())
    println(a.sum())
    println(DoubleArray(3) { it * 1.5 }.joinToString())
    println(Array(3) { it * it }.toList())
    println(IntArray(2).joinToString())
}";
    assert_eq!(
        prog(src),
        "0, 2, 4, 6\n12\n0.0, 1.5, 3.0\n[0, 1, 4]\n0, 0\n"
    );
}

#[test]
fn for_in_walks_a_strings_characters() {
    // The element is a `Char`, so it displays as a character (not its code) and
    // `+` on it appends to a String.
    let src = "\
fun main() {
    for (c in \"abc\") println(c)
    var t = 0
    for (c in \"Hello\") t += c.code
    println(t)
    var s = \"\"
    val src = \"kot\"
    for (c in src) s += c
    println(s + \"|\" + s.length)
}";
    assert_eq!(prog(src), "a\nb\nc\n500\nkot|3\n");
}

// ─── Null safety end to end ───────────────────────────────────────────────

#[test]
fn nullable_values_compare_and_display_like_kotlin() {
    // `x == null` is a null test (not a coerced value compare), and a null
    // `String?` renders as the four characters `null` in a template or a `+`.
    let src = "\
fun main() {
    val n: String? = null
    val s: String? = \"hey\"
    println(n == null)
    println(s != null)
    println(n?.length)
    println(s?.length)
    println(n ?: \"dflt\")
    println(\"v=$n\")
    println(\"c\" + n)
    println(s?.uppercase() ?: \"none\")
    val k: Int? = null
    println(k?.plus(1) ?: 0)
}";
    assert_eq!(
        prog(src),
        "true\ntrue\nnull\n3\ndflt\nv=null\ncnull\nHEY\n0\n"
    );
}

// ─── Class inheritance ────────────────────────────────────────────────────
//
// Every expected string below was captured from the reference `kotlinc` +
// `kotlin` toolchain on the identical source (kotlinc-jvm 2.4.10).

#[test]
fn override_dispatches_virtually_through_a_supertype_method() {
    // `call()` is declared once on `A` and invokes `v()`, which each level
    // overrides: the body a call lands in is decided by the receiver's RUNTIME
    // class, not by the class that declared the caller. `B`'s constructor also
    // passes a computed argument up (`A(n + 1)`), so the inherited `n` differs
    // from the one written at the construction site.
    let src = "\
open class A(val n: Int) {
    open fun v(): Int = n
    fun call(): Int = v() * 2
}
open class B(n: Int) : A(n + 1) {
    override fun v(): Int = n * 10
}
class C(n: Int) : B(n) {
    override fun v(): Int = super.v() + 1
}
fun main() {
    println(A(3).call())
    println(B(3).call())
    println(C(3).call())
    println(A(3).n)
    println(B(3).n)
    println(C(3).n)
}";
    assert_eq!(prog(src), "6\n80\n82\n3\n4\n4\n");
}

#[test]
fn interface_default_member_calls_the_implementors_override() {
    let src = "\
interface Src { fun get(): Int
    fun twice(): Int = get() * 2 }
interface Sink { fun put(v: Int): String }
class Both(val k: Int) : Src, Sink {
    override fun get(): Int = k
    override fun put(v: Int): String = \"put$v\"
}
fun main() {
    val b = Both(4)
    println(b.get())
    println(b.twice())
    println(b.put(9))
    val s: Src = Both(6)
    println(s.twice())
    println(s is Sink)
    println(s is Src)
}";
    assert_eq!(prog(src), "4\n8\nput9\n12\ntrue\ntrue\n");
}

#[test]
fn abstract_member_is_reached_from_a_concrete_base_method() {
    let src = "\
abstract class Shape {
    abstract fun area(): Int
    abstract fun name(): String
    fun show(): String = name() + \":\" + area()
}
class Sq(val s: Int) : Shape() {
    override fun area(): Int = s * s
    override fun name(): String = \"sq\"
}
class Rc(val w: Int, val h: Int) : Shape() {
    override fun area(): Int = w * h
    override fun name(): String = \"rc\"
}
fun main() {
    val xs = listOf(Sq(3), Rc(2, 5))
    for (x in xs) println(x.show())
    println(xs.map { it.area() })
    println(xs.filter { it.area() > 9 }.map { it.name() })
}";
    assert_eq!(prog(src), "sq:9\nrc:10\n[9, 10]\n[rc]\n");
}

#[test]
fn user_class_extending_a_jvm_throwable_is_caught_and_printed_like_one() {
    // The `catch` arms are tested against the user class's own supertypes, so a
    // `ParseError : IllegalArgumentException` is claimed by a
    // `catch (e: IllegalArgumentException)` and by `catch (e: Exception)`, and it
    // renders through `Throwable.toString()` rather than the identity form.
    let src = "\
class ParseError(msg: String) : IllegalArgumentException(msg)
class EmptyError : RuntimeException()
fun parse(s: String): Int {
    if (s == \"\") throw EmptyError()
    if (s == \"x\") throw ParseError(\"bad token x\")
    return s.length
}
fun main() {
    for (s in listOf(\"ab\", \"\", \"x\")) {
        println(try { parse(s) }
            catch (e: ParseError) { \"PE:\" + e.message }
            catch (e: RuntimeException) { \"RE:\" + e.message })
    }
    try { throw ParseError(\"z\") } catch (e: IllegalArgumentException) { println(\"IAE \" + e.message) }
    try { throw ParseError(\"z\") } catch (e: Exception) { println(e) }
    println(ParseError(\"q\") is IllegalArgumentException)
    println(ParseError(\"q\") is RuntimeException)
    println(EmptyError() is Exception)
}";
    assert_eq!(
        prog(src),
        "2\nRE:null\nPE:bad token x\nIAE z\nParseError: z\ntrue\ntrue\ntrue\n"
    );
}

#[test]
fn tostring_override_is_honoured_for_a_nested_element_too() {
    let src = "\
open class Vec(val x: Int, val y: Int) {
    override fun toString(): String = \"<$x,$y>\"
}
class Vec3(x: Int, y: Int, val z: Int) : Vec(x, y) {
    override fun toString(): String = \"<$x,$y,$z>\"
}
fun main() {
    val vs = listOf(Vec(1, 2), Vec3(1, 2, 3))
    println(vs)
    println(vs.joinToString(\" | \"))
    println(\"${vs[0]} then ${vs[1]}\")
    println(vs[1].toString())
    println(mapOf(\"a\" to Vec(0, 1)))
}";
    assert_eq!(
        prog(src),
        "[<1,2>, <1,2,3>]\n<1,2> | <1,2,3>\n<1,2> then <1,2,3>\n<1,2,3>\n{a=<0,1>}\n"
    );
}

#[test]
fn an_abstract_or_sealed_type_cannot_be_constructed() {
    assert!(
        prog_err("abstract class S(val n: Int)\nfun main() { println(S(1)) }")
            .contains("cannot construct abstract class S")
    );
    assert!(prog_err("sealed class S\nfun main() { println(S()) }")
        .contains("cannot construct abstract class S"));
    assert!(
        prog_err("interface I { fun f(): Int }\nfun main() { println(I()) }")
            .contains("cannot construct interface I")
    );
    assert!(prog_err("class D : Missing()\nfun main() { println(1) }")
        .contains("unresolved supertype Missing"));
}

#[test]
fn soft_keywords_are_usable_as_member_names() {
    // `step`/`until`/`downTo`/`data` are infix functions and a modifier in
    // Kotlin, not reserved words, so a declaration may use them as names.
    let src = "\
class Walker(val data: Int) {
    fun step(): Int = data + 1
    fun until(k: Int): Int = k - data
}
fun main() {
    val w = Walker(4)
    println(w.step())
    println(w.until(10))
    println(w.data)
    println((1..9 step 3).toList())
}";
    assert_eq!(prog(src), "5\n6\n4\n[1, 4, 7]\n");
}

// ─── Set and the collection operations ────────────────────────────────────
//
// Expected strings captured from the reference toolchain (kotlinc-jvm 2.4.10).

#[test]
fn set_keeps_insertion_order_and_compares_without_it() {
    // `setOf` builds a LinkedHashSet: iteration and display follow insertion
    // order (so the output is reproducible), while equality does not.
    let src = "\
fun main() {
    val s = setOf(3, 1, 2, 3, 1)
    println(s)
    println(s.size)
    println(2 in s)
    println(9 in s)
    println(s.toList())
    println(s.sum())
    println(setOf(1, 2) == setOf(2, 1))
    println(setOf(1, 2) == setOf(1, 3))
    println(listOf(1, 2, 2, 3).toSet())
    println(listOf(1, 2, 2, 3).distinct())
}";
    assert_eq!(
        prog(src),
        "[3, 1, 2]\n3\ntrue\nfalse\n[3, 1, 2]\n6\ntrue\nfalse\n[1, 2, 3]\n[1, 2, 3]\n"
    );
}

#[test]
fn set_operators_and_mutation() {
    // `MutableSet.add` answers whether the element was NEW, unlike
    // `MutableList.add`, which always appends and answers `true`.
    let src = "\
fun main() {
    println(setOf(1, 2).union(setOf(2, 3)))
    println(setOf(1, 2).intersect(setOf(2, 3)))
    println(setOf(1, 2).subtract(setOf(2)))
    val m = mutableSetOf(1, 2)
    println(m.add(3))
    println(m.add(2))
    println(m)
    println(m.remove(1))
    println(m)
    val l = mutableListOf(1, 2, 3)
    println(l.add(3))
    println(l.remove(9))
    println(l)
}";
    assert_eq!(
        prog(src),
        "[1, 2, 3]\n[2]\n[1]\ntrue\nfalse\n[1, 2, 3]\ntrue\n[2, 3]\ntrue\nfalse\n[1, 2, 3, 3]\n"
    );
}

#[test]
fn ordering_and_slicing_members() {
    // `take`/`drop` clamp to the sequence's length rather than faulting, and
    // `sortedByDescending` keeps ties in input order (it flips the comparison
    // rather than reversing a stable ascending sort).
    let src = "\
fun main() {
    val ns = listOf(5, 3, 9, 1, 3)
    println(ns.sorted())
    println(ns.sortedDescending())
    println(ns.take(2))
    println(ns.drop(2))
    println(ns.take(99))
    println(ns.drop(99))
    println((1..5).take(2))
    println(listOf(\"bb\", \"a\", \"cc\").sortedByDescending { it.length })
}";
    assert_eq!(
        prog(src),
        "[1, 3, 3, 5, 9]\n[9, 5, 3, 3, 1]\n[5, 3]\n[9, 1, 3]\n[5, 3, 9, 1, 3]\n[]\n[1, 2]\n[bb, cc, a]\n"
    );
}

#[test]
fn associate_family_and_the_new_higher_order_members() {
    // `associate` reads the lambda's Pair as the entry; `associateBy` reads its
    // result as the KEY and the element as the value.
    let src = "\
data class P(val n: String, val a: Int)
fun main() {
    val ps = listOf(P(\"ann\", 30), P(\"bob\", 25), P(\"cid\", 30))
    println(ps.associate { it.n to it.a })
    println(ps.associateBy { it.a })
    println(ps.groupBy { it.a })
    println(ps.minByOrNull { it.a })
    println(ps.filterNot { it.a > 26 })
    println(ps.none { it.a > 99 })
    println(ps.flatMap { listOf(it.n, it.n) })
    println(ps.mapIndexed { i, x -> \"$i:${x.n}\" })
}";
    assert_eq!(
        prog(src),
        "{ann=30, bob=25, cid=30}\n\
         {30=P(n=cid, a=30), 25=P(n=bob, a=25)}\n\
         {30=[P(n=ann, a=30), P(n=cid, a=30)], 25=[P(n=bob, a=25)]}\n\
         P(n=bob, a=25)\n\
         [P(n=bob, a=25)]\n\
         true\n\
         [ann, ann, bob, bob, cid, cid]\n\
         [0:ann, 1:bob, 2:cid]\n"
    );
}

#[test]
fn string_indexing_yields_a_char() {
    // `s[i]` indexes by UTF-16 code unit, the same basis as `length`, and its
    // static type is `Char` — so it displays as a character, takes part in Char
    // arithmetic, and appends as one. Out of range is a
    // StringIndexOutOfBoundsException, catchable like any other.
    let src = "\
fun main() {
    val s = \"hello\"
    println(s[0])
    println(s[4])
    println(s[1] == 'e')
    println(s[0] + 1)
    println(s[1].code)
    var t = \"\"
    for (i in 0 until s.length) t += s[i]
    println(t)
    println(try { s[9] } catch (e: StringIndexOutOfBoundsException) { 'X' })
    println(try { s[-1].toString() } catch (e: Exception) { \"oob\" })
}";
    assert_eq!(prog(src), "h\no\ntrue\ni\n101\nhello\nX\noob\n");
}

// ── Class bodies, extensions, and the scope functions ─────────────────────
//
// Each of these was a loud "unsupported" before; the assertions below are the
// answers the reference toolchain gives (see `tests/data/parity_expected.txt`
// for the frozen record of the same programs).

#[test]
fn class_body_properties_are_per_instance_and_not_data_members() {
    // A body property is stored, initialized per instance in declaration order,
    // and may name a constructor parameter — but a `data class`'s generated
    // members read the PRIMARY CONSTRUCTOR alone, so `extra` is absent from
    // `toString`, `equals`, and `hashCode` while still being readable.
    let src = "\
class C(val a: Int) {
    var c = 0
    val d = a * 2
    fun bump(): Int { c = c + 1; return c }
}
data class D(val a: Int) { val extra = a + 1 }
fun main() {
    val x = C(3)
    x.bump()
    println(x.bump())
    println(x.c)
    println(x.d)
    println(D(1))
    println(D(1) == D(2).copy(a = 1))
    println(D(1).extra)
    println(D(4).copy(a = 9).extra)
}";
    assert_eq!(prog(src), "2\n2\n6\nD(a=1)\ntrue\n2\n10\n");
}

#[test]
fn extension_dispatch_is_by_the_receivers_static_type() {
    // `Int` and `Long` share one runtime representation, so only the DECLARED
    // receiver decides which body runs — and with it whether the arithmetic
    // wraps at 32 bits. An extension calling another unqualified goes through
    // its own receiver.
    let src = "\
fun Int.dbl(): Int = this * 2
fun Long.dbl(): Long = this * 2
fun Int.quad(): Int = dbl().dbl()
fun String.shout(): String = uppercase() + \"!\"
fun main() {
    println(3.dbl())
    println(2000000000.dbl())
    println(2000000000L.dbl())
    println(2000000000.quad())
    println(\"hi\".shout())
    println(7.dbl() / 4)
}";
    assert_eq!(prog(src), "6\n-294967296\n4000000000\n-589934592\nHI!\n3\n");
}

#[test]
fn companion_members_are_reachable_with_and_without_the_class_name() {
    let src = "\
class Counter(val start: Int) {
    var n = start
    companion object {
        val ZERO = 0
        fun of(k: Int): Counter = Counter(k)
    }
    fun reset(): Int { n = ZERO; return n }
    fun clone2(): Counter = of(n)
}
fun main() {
    println(Counter.ZERO)
    println(Counter.of(5).n)
    println(Counter(2).reset())
    println(Counter(7).clone2().n)
}";
    assert_eq!(prog(src), "0\n5\n0\n7\n");
}

#[test]
fn scope_functions_split_between_it_and_this() {
    // `run`/`apply`/`with` bind the receiver as `this`, so an unqualified name
    // inside reads a MEMBER of it; `let`/`also`/`takeIf` pass it as `it`.
    // `apply`/`also` yield the receiver, `run`/`let` the block — and the
    // receiver's declared width still reaches the block's arithmetic.
    let src = "\
class Box(var w: Int, var h: Int) { fun area(): Int = w * h }
fun main() {
    println(\"abc\".run { length })
    println(with(\"hello\") { uppercase() + length })
    println(run { 1 + 2 })
    println(Box(2, 3).apply { w = 5 }.area())
    println(Box(2, 3).run { area() + w })
    println(\"abc\".let { it.length })
    println(7.takeIf { it > 3 })
    println(7.takeUnless { it > 3 })
    println(2000000000.let { it + it })
    println(2000000000L.let { it + it })
}";
    assert_eq!(
        prog(src),
        "3\nHELLO5\n3\n15\n8\n3\n7\nnull\n-294967296\n4000000000\n"
    );
}

#[test]
fn default_named_and_vararg_arguments_bind_by_declaration() {
    // A default fills an omitted slot, a named argument binds by NAME (so an
    // out-of-order pair cannot silently swap), and a `vararg` collects the
    // positional tail — including none at all.
    let src = "\
fun greet(who: String, times: Int = 2, sep: String = \"-\"): String {
    var s = \"\"
    for (i in 1..times) s = s + who + sep
    return s
}
fun total(vararg xs: Int): Int { var t = 0; for (x in xs) t += x; return t }
fun mixed(a: Int, vararg rest: Int): Int { var t = a * 100; for (x in rest) t += x; return t }
data class Cfg(val a: Int = 1, val b: String = \"x\")
fun main() {
    println(greet(\"x\"))
    println(greet(\"y\", 3))
    println(greet(sep = \"+\", who = \"z\"))
    println(greet(\"q\", sep = \"*\"))
    println(total())
    println(total(1, 2, 3))
    println(mixed(1))
    println(mixed(1, 2, 3))
    println(Cfg())
    println(Cfg(b = \"z\"))
}";
    assert_eq!(
        prog(src),
        "x-x-\ny-y-y-\nz+z+\nq*q*\n0\n6\n100\n105\nCfg(a=1, b=x)\nCfg(a=1, b=z)\n"
    );
}

#[test]
fn pair_and_triple_are_data_classes_that_print_as_tuples() {
    // Both render `(a, b[, c])` rather than `Name(x=…)`, compare structurally,
    // and fold their `hashCode` the way a `data class` does — three places a
    // frontend that reuses another heap kind answers something plausible.
    let src = "\
fun main() {
    println(Pair(1, \"a\"))
    println(Triple(1, 2, \"z\"))
    println(Pair(1, \"a\") == Pair(1, \"a\"))
    println(Triple(1, 2, \"z\") == Triple(2, 1, \"z\"))
    println(Pair(1, 2).hashCode())
    println(Triple(1, 2, 3).hashCode())
    println((1 to 2) == Pair(1, 2))
    val (a, b, c) = Triple(1, 2, \"z\")
    println(\"\" + a + b + c)
}";
    assert_eq!(
        prog(src),
        "(1, a)\n(1, 2, z)\ntrue\nfalse\n33\n1026\ntrue\n12z\n"
    );
}

#[test]
fn a_lambda_write_reaches_the_enclosing_var() {
    // A closure copies its captures by value, so a `var` a lambda ASSIGNS to has
    // to live in shared storage — and keep its declared width while it does.
    let src = "\
fun main() {
    var n = 0
    listOf(1, 2, 3).forEach { n += it }
    println(n)
    var s = \"\"
    listOf(\"a\", \"b\").forEach { s = s + it }
    println(s)
    var c = 0
    (1..5).forEach { if (it % 2 == 0) c++ }
    println(c)
    var big = 2000000000
    listOf(1).forEach { big += big }
    println(big)
    var lbig = 2000000000L
    listOf(1).forEach { lbig += lbig }
    println(lbig)
}";
    assert_eq!(prog(src), "6\nab\n2\n-294967296\n4000000000\n");
}

#[test]
fn a_cast_supplies_the_static_type_and_checks_at_runtime() {
    // The value is unchanged; what the cast gives is the type that then decides
    // `/` dispatch. `as` throws on a mismatch where `as?` is null — and a null
    // `as? String` has to PRINT as `null`, not as the empty string.
    let src = "\
fun anyAt(i: Int): Any = listOf<Any>(7, \"ab\", 2.5)[i]
fun main() {
    println((anyAt(0) as Int) + 1)
    println(anyAt(0) as Int / 2)
    println(anyAt(2) as Double / 2)
    println((anyAt(1) as String).length)
    println(anyAt(0) as? String)
    println(anyAt(1) as? Int)
    println((anyAt(1) as? Int) ?: -1)
    try { println(anyAt(1) as Int) } catch (e: ClassCastException) { println(\"cce\") }
}";
    assert_eq!(prog(src), "8\n3\n1.25\n2\nnull\nnull\n-1\ncce\n");
}

#[test]
fn top_level_properties_initialize_before_main_and_by_lazy_does_not() {
    // A plain top-level property runs its initializer at startup; a `by lazy`
    // one runs at the FIRST READ and caches, so the marker lands between the
    // prints rather than before them, and only once.
    let src = "\
val K = 7
val NAME: String = \"kt\"
val DERIVED = K * 3
var counter = 0
val z: Int by lazy { println(\"forcing\"); 41 + 1 }
fun bump(): Int { counter += 1; return counter }
fun main() {
    println(K)
    println(NAME.uppercase())
    println(DERIVED)
    println(K / 2)
    println(bump())
    println(bump())
    counter = 100
    println(counter)
    println(\"before\")
    println(z)
    println(z)
}";
    assert_eq!(
        prog(src),
        "7\nKT\n21\n3\n1\n2\n100\nbefore\nforcing\n42\n42\n"
    );
}

#[test]
fn run_catching_packages_both_outcomes_as_a_result() {
    // Every reader of a `Result` is total: `getOrNull` is null on failure,
    // `exceptionOrNull` null on success, `map` transforms only a success. A
    // throw inside the block must be CAUGHT, not escape the program.
    let src = "\
fun rboom(n: Int): Int {
    if (n < 0) throw IllegalStateException(\"neg\")
    return n * 2
}
fun main() {
    val ok = runCatching { rboom(3) }
    val bad = runCatching { rboom(-1) }
    println(ok)
    println(bad)
    println(ok.isSuccess)
    println(bad.isFailure)
    println(ok.getOrNull())
    println(bad.getOrNull())
    println(bad.exceptionOrNull())
    println(bad.getOrElse { -99 })
    println(ok.map { it + 1 })
    println(bad.map { it + 1 })
    println(runCatching { 1 / 0 }.isFailure)
}";
    assert_eq!(
        prog(src),
        "Success(6)\n\
         Failure(java.lang.IllegalStateException: neg)\n\
         true\ntrue\n6\nnull\n\
         java.lang.IllegalStateException: neg\n\
         -99\nSuccess(7)\n\
         Failure(java.lang.IllegalStateException: neg)\n\
         true\n"
    );
}

#[test]
fn a_local_fun_is_a_subroutine_so_it_can_recurse() {
    // Lowered as a real sub rather than a closure value: a closure captures by
    // value at creation, so a self-reference would read an uninitialized slot.
    // It still takes defaults, shadows a top-level function of its name, and is
    // callable from a lambda in the same body.
    let src = "\
fun shadowed(n: Int): Int = n * 1000
fun main() {
    fun g(x: Int): Int = x * 3
    println(g(4))
    fun fact(n: Int): Int = if (n <= 1) 1 else n * fact(n - 1)
    println(fact(5))
    fun tag(k: Int, sep: String = \"-\"): String = sep + k
    println(tag(2) + tag(2, \"+\"))
    fun shadowed(k: Int): Int = k + 1
    println(shadowed(4))
    println(listOf(1, 2, 3).map { g(it) })
    fun outer(k: Int): Int {
        fun inner(j: Int): Int = j + 3
        return inner(k) * 2
    }
    println(outer(3))
}";
    assert_eq!(prog(src), "12\n120\n-2+2\n5\n[3, 6, 9]\n12\n");
}

#[test]
fn a_local_fun_closes_over_the_enclosing_frame_without_losing_recursion() {
    // The captures ride as synthesized trailing parameters, so the body reaches
    // them at fixed slots and a self-call passes them straight on. Every line
    // below was measured against kotlinc 2.4.10 and again against 2.4.20, both on
    // JDK 21.0.12.1, before it was frozen here; the shapes that motivate each
    // one:
    //
    //   * `a` calls `b`, so `a` has to carry `b`'s capture even though it never
    //     spells it — captures compose along the call graph.
    //   * `shadow` has a parameter named `k`, and still owes `b` the OUTER `k`.
    //     A capture the parameter list shadows is bound under a key no Kotlin
    //     identifier can spell, so both reach the right binding.
    //   * `bump` ASSIGNS to a captured `var`, which is what forces the enclosing
    //     `c` into a heap cell — by-value capture would have counted a copy.
    //   * `inner` reads a field, which needs both the captured `this` and the
    //     declaring method's class context.
    //   * the `forEach` block and `f` both call outward into a local `fun`, so
    //     the lambda has to hold captures for names it never mentions.
    let src = "\
class P(val n: Int) {
    fun go(): Int {
        fun inner(m: Int) = n * m
        return inner(2)
    }
}
fun main() {
    val k = 5
    fun h(x: Int) = x + k
    println(h(1))
    fun b(x: Int) = x + k
    fun a(y: Int) = b(y) * 2
    println(a(1))
    println(listOf(1, 2, 3).map { b(it) })
    var c = 0
    fun bump() { c += 1 }
    bump(); bump(); bump()
    println(c)
    fun fact(n: Int): Int = if (n <= 1) k else n * fact(n - 1)
    println(fact(4))
    fun shadow(k: Int): Int = b(0) + k
    println(shadow(100))
    println(P(21).go())
    listOf(7).forEach { e ->
        fun show(t: String) = \"$t:$e:$k\"
        println(show(\"v\"))
    }
    val f = { z: Int -> b(z) + 1 }
    println(f(2))
}";
    assert_eq!(prog(src), "6\n12\n[6, 7, 8]\n3\n120\n105\n42\nv:7:5\n8\n");
}

#[test]
fn a_plain_type_parameter_test_is_rejected_rather_than_answered() {
    // The coarse type system carries no type arguments, so `x is T` inside a
    // generic function has no answer it could give — and answering `false` (the
    // shape a name-based lookup falls into) would be silently wrong. It is a
    // compile error instead.
    //
    // A `reified` T is the case that DOES have an answer, and this test used to
    // assert that it did not: the type argument was unavailable at run time
    // before the call site started binding it. See the test below, which pins
    // what it answers now; this one keeps the diagnostic for the parameter that
    // is still unanswerable.
    let err =
        prog_err("fun <T> isA(x: Any): Boolean = x is T\nfun main() { println(isA<Int>(5)) }");
    assert!(
        err.contains("type parameter `T`") && err.contains("reified"),
        "unexpected diagnostic: {err}"
    );
}

#[test]
fn a_reified_type_parameter_is_available_at_run_time() {
    // `reified` means the type argument survives into the body. It does here
    // without an inliner: the call site BINDS the name and the body reads it
    // back, so `T::class`, `x is T` and `x as T` all answer for whatever the
    // call supplied. Both spellings are covered — a free function and an
    // extension — because they take different paths to the call site.
    let src = "inline fun <reified T> nameOf(): String = T::class.simpleName ?: \"?\"\n\
               inline fun <reified T> Any.isA(): Boolean = this is T\n\
               fun main() {\n\
                   println(nameOf<Int>())\n\
                   println(nameOf<String>())\n\
                   println(5.isA<Int>())\n\
                   println(5.isA<String>())\n\
                   println(\"x\".isA<String>())\n\
               }";
    assert_eq!(prog(src), "Int\nString\ntrue\nfalse\ntrue\n");
}

// ── Secondary constructors, delegation, invocation ──
//
// The frozen parity corpus pins the RESULTS of these against the reference
// toolchain; what it cannot hold is a program `kotlinc` rejects. These cover
// the diagnostics instead — each is a case where answering something would be
// worse than failing, because the wrong answer is silent.

#[test]
fn secondary_constructor_delegating_to_itself_is_rejected() {
    // `constructor(a: Int) : this(0)` would call itself forever. Kotlin reports
    // it at compile time; so must we, rather than emitting a program that
    // exhausts the stack at run time.
    let out = eval(
        "class C(val v: Int) {\n\
         \x20   constructor(a: Int, b: Int) : this(a, b) { }\n\
         }\n\
         fun main() { println(C(1, 2).v) }",
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("delegates to itself"), "stderr was: {err}");
}

#[test]
fn a_secondary_constructor_calling_super_beside_a_primary_is_rejected() {
    // kotlinc 2.4.21: `error: primary constructor call expected.` With a
    // primary constructor declared, the superclass call belongs to the header
    // and every secondary must reach the primary through `this(…)`. An empty
    // `super()` is rejected the same way, not routed through the primary.
    for src in [
        "open class B(val n: Int)\n\
         class C(v: Int) : B(v) {\n\
         \x20   constructor(s: String) : super(s.length)\n\
         }\n\
         fun main() { println(C(\"x\").n) }",
        "class C(val v: Int) {\n\
         \x20   constructor(s: String) : super() { }\n\
         }\n\
         fun main() { println(C(\"x\").v) }",
    ] {
        let out = eval(src);
        assert!(!out.status.success(), "accepted: {src}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("may not call `super(…)` when the class has a primary constructor"),
            "stderr was: {err}"
        );
    }
}

#[test]
fn a_secondary_constructor_calls_super_with_its_own_parameters() {
    // Measured on kotlinc 2.4.21 / JRE 25.0.4.1. A class with no primary
    // constructor chains to the superclass from each secondary, with the
    // arguments written in terms of THAT secondary's parameters; the body
    // properties and `init` blocks run once, before the secondary's body; and
    // a `: Super(args)` header selects a SECONDARY of the superclass when that
    // is what fits, including through a grandchild on the in-place scheme.
    assert_eq!(
        stdout(
            "open class Base(val n: Int) {\n\
             \x20   constructor(s: String) : this(s.length) { println(\"Base(String)\") }\n\
             \x20   open fun show() = \"Base$n\"\n\
             }\n\
             open class Kid : Base {\n\
             \x20   val tag: String\n\
             \x20   init { println(\"Kid init\") }\n\
             \x20   constructor(n: Int, t: String) : super(n) { tag = t }\n\
             \x20   constructor(s: String) : super(s) { tag = \"s\" }\n\
             \x20   override fun show() = \"Kid$n$tag\"\n\
             }\n\
             class Grand : Kid {\n\
             \x20   constructor() : super(5, \"g\") { println(\"Grand after ${show()}\") }\n\
             }\n\
             class E : IllegalStateException {\n\
             \x20   constructor(m: String, c: Throwable) : super(m, c)\n\
             }\n\
             fun main() {\n\
             \x20   println(Kid(1, \"x\").show())\n\
             \x20   println(Kid(\"abcd\").show())\n\
             \x20   println(Grand().show())\n\
             \x20   try { throw E(\"m\", RuntimeException(\"c\")) } catch (e: RuntimeException) { println(\"${e.message} <- ${e.cause?.message}\") }\n\
             }"
        ),
        "Kid init\nKid1x\nBase(String)\nKid init\nKid4s\nKid init\nGrand after Kid5g\nKid5g\nm <- c\n"
    );
}

#[test]
fn an_enum_constant_selects_a_secondary_constructor() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13. An enum's constants select among
    // its constructors like any call: `B("abc")` reaches the String secondary,
    // `C` the no-argument one, `D(2.5)` a secondary that delegates to another
    // secondary; and an enum with no primary constructor at all builds every
    // constant through a secondary. `name` and `ordinal` reach each instance
    // through the delegation.
    assert_eq!(
        stdout(
            "enum class F(val v: Int) {\n\
             \x20   A(1), B(\"abc\"), C, D(2.5);\n\
             \x20   constructor(s: String) : this(s.length)\n\
             \x20   constructor() : this(v = 42)\n\
             \x20   constructor(d: Double) : this(\"x\".repeat(d.toInt()))\n\
             }\n\
             enum class G {\n\
             \x20   P(1), Q(\"s\"), R;\n\
             \x20   val k: String\n\
             \x20   constructor(i: Int) { k = \"int$i\" }\n\
             \x20   constructor(s: String) { k = \"str$s\" }\n\
             \x20   constructor() { k = \"none\" }\n\
             }\n\
             fun main() {\n\
             \x20   for (f in F.values()) println(\"$f ${f.v} ${f.ordinal}\")\n\
             \x20   for (g in G.entries) println(\"$g ${g.k} ${g.name} ${g.ordinal}\")\n\
             \x20   println(F.valueOf(\"B\").v)\n\
             }"
        ),
        "A 1 0\nB 3 1\nC 42 2\nD 2 3\nP int1 P 0\nQ strs Q 1\nR none R 2\n3\n"
    );
}

#[test]
fn an_enum_constant_takes_defaults_and_named_arguments() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13. A constant may omit a defaulted
    // parameter and name the ones it passes, with or without a body.
    assert_eq!(
        stdout(
            "enum class E(val a: Int = 5, val b: String = \"z\") {\n\
             \x20   X(), Y(1), Z(b = \"q\"), W(b = \"w\", a = 7) { override fun f() = \"body$a\" };\n\
             \x20   open fun f() = \"base\"\n\
             }\n\
             fun main() { for (e in E.entries) println(\"$e ${e.a} ${e.b} ${e.ordinal} ${e.f()}\") }"
        ),
        "X 5 z 0 base\nY 1 z 1 base\nZ 5 q 2 base\nW 7 w 3 body7\n"
    );
    // Too many arguments is still reported in the constant's own terms, not
    // as a clash with the hidden `name`/`ordinal` parameters. A defaulted
    // parameter makes the count a maximum.
    let out = eval("enum class E(val a: Int = 0) { X(1, 2) }\nfun main() { println(E.X.a) }");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("enum constant E.X takes at most 1 argument(s), but 2 were given"),
        "stderr was: {err}"
    );
}

#[test]
fn a_string_member_result_selects_the_string_constructor() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13. `padStart`, `repeat` and
    // `substring` answer a `String`, so each argument below selects the
    // `String` secondary — before, the result had no static type and was
    // stored into the `Int` primary as is.
    assert_eq!(
        stdout(
            "class K(val v: Int) {\n\
             \x20   var via = \"primary\"\n\
             \x20   constructor(s: String) : this(s.length) { via = \"string\" }\n\
             \x20   constructor(d: Double) : this(\"x\".repeat(d.toInt()))\n\
             }\n\
             fun main() {\n\
             \x20   val s = \"ab\".padStart(5)\n\
             \x20   val k = K(s); println(\"${k.v} ${k.via}\")\n\
             \x20   val d = K(3.0); println(\"${d.v} ${d.via}\")\n\
             \x20   val t = K(\"abcdef\".substring(2)); println(\"${t.v} ${t.via}\")\n\
             }"
        ),
        "5 string\n3 string\n4 string\n"
    );
}

#[test]
fn a_class_property_may_delegate_to_the_stdlib_observable() {
    // This pinned a compile error for a program the reference accepts (`1`,
    // measured on kotlinc 2.4.21): `Delegates.observable` is supplied by the
    // frontend, so no user `getValue` is needed. A delegate that is neither a
    // user class with `getValue` nor one of those stays rejected, see
    // `a_local_by_a_delegate_with_no_getvalue_is_rejected`.
    assert_eq!(
        stdout(
            "import kotlin.properties.Delegates\n\
             class O { var o: Int by Delegates.observable(1) { p, a, b -> } }\n\
             fun main() { println(O().o) }"
        ),
        "1\n"
    );
}

#[test]
fn by_delegation_requires_an_interface_supertype() {
    let out = eval(
        "open class B { fun m(): Int = 1 }\n\
         class C(b: B) : B by b\n\
         fun main() { println(C(B()).m()) }",
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("interface supertype"), "stderr was: {err}");
}

#[test]
fn init_block_runs_interleaved_with_property_initializers() {
    // Kotlin makes ONE declaration-order pass over the property initializers
    // and the `init` blocks, so a block sees every property above it and none
    // below it. Running them as two groups would still print both lines, in
    // the wrong order — which is why the order is what is asserted.
    assert_eq!(
        stdout(
            "class C {\n\
             \x20   val a = 1\n\
             \x20   init { println(\"i1 \" + a) }\n\
             \x20   val b = a + 1\n\
             \x20   init { println(\"i2 \" + b) }\n\
             }\n\
             fun main() { C() }"
        ),
        "i1 1\ni2 2\n"
    );
}

#[test]
fn statement_leading_paren_after_a_call_is_not_an_invocation() {
    // The lexer drops newlines, so postfix `(` has to be gated on the token
    // being glued to the previous one. Without that, `f()` followed by a
    // statement that STARTS with `(` reads as `f()(…)`.
    assert_eq!(
        stdout(
            "fun f(): Int = 1\n\
             println(f())\n\
             (1..3).forEach { print(it) }\n\
             println(\"\")"
        ),
        "1\n123\n"
    );
}

#[test]
fn string_receiver_collection_result_follows_kotlin_text() {
    // `kotlin.text` gives a `CharSequence` receiver a `String` result where the
    // `Iterable` overload gives a `List` — but only for some members. A
    // lowering that materializes the characters and reuses the list
    // implementation wholesale is right for `map` and wrong for `filter`.
    assert_eq!(stdout("println(\"abc\".map { it })"), "[a, b, c]\n");
    assert_eq!(stdout("println(\"hello\".filter { it != 'l' })"), "heo\n");
    assert_eq!(stdout("println(\"abc\".chunked(2))"), "[ab, c]\n");
    assert_eq!(
        stdout("println(\"abc\".partition { it < 'b' })"),
        "(a, bc)\n"
    );
    assert_eq!(stdout("println(\"\".map { it })"), "[]\n");
    assert_eq!(stdout("println(\"\".filter { true })"), "\n");
}

#[test]
fn plain_class_equality_is_reference_identity() {
    // A class that declares neither `data` nor `equals` inherits `Any.equals`,
    // which on the JVM is reference identity — so two separate constructions of
    // the same arguments are NOT equal, and no container finds one by the other.
    // This frontend answered structural equality here, which is `true` where
    // Kotlin says `false` for every line below.
    let src = "\
class Plain(val a: Int)
data class Dat(val a: Int)
fun main() {
    val p = Plain(1)
    println(p == p)
    println(Plain(1) == Plain(1))
    println(Plain(1) != Plain(1))
    println(listOf(Plain(1)) == listOf(Plain(1)))
    println(Plain(1) in listOf(Plain(1)))
    println(setOf(Plain(1), Plain(1)).size)
    println(mapOf(Plain(1) to 1, Plain(2) to 2).containsKey(Plain(1)))
    println(Dat(1) == Dat(1))
    println(listOf(Dat(1)) == listOf(Dat(1)))
    println(setOf(Dat(1), Dat(1)).size)
}";
    // Captured from kotlinc-jvm 2.4.10 / JRE 26.0.2.
    assert_eq!(
        prog(src),
        "true\nfalse\ntrue\nfalse\nfalse\n2\nfalse\ntrue\ntrue\n1\n"
    );
}

#[test]
fn declared_equals_without_hashcode_reaches_lists_but_not_hash_containers() {
    // Overriding `equals` alone breaks the JVM's equals/hashCode contract, and
    // the two container families diverge as a result: a `List` compares with
    // `equals` and finds the twin, while a `Set`/`Map` key hashes FIRST and the
    // identity hashes never meet — so the duplicates survive and the lookup
    // misses. Calling the override for every container would answer `1` and
    // `5` on the last three lines; ignoring it would answer `false` on the
    // list lines. Both are wrong, in opposite directions.
    let src = "\
class OnlyEq(val a: Int) {
    override fun equals(other: Any?): Boolean = other is OnlyEq && other.a == a
}
fun main() {
    println(OnlyEq(1) == OnlyEq(1))
    println(OnlyEq(1) == OnlyEq(2))
    println(OnlyEq(1) != OnlyEq(2))
    println(listOf(OnlyEq(1)).contains(OnlyEq(1)))
    println(listOf(OnlyEq(9), OnlyEq(1)).indexOf(OnlyEq(1)))
    println(listOf(OnlyEq(1)) == listOf(OnlyEq(1)))
    println(setOf(OnlyEq(1), OnlyEq(1)).size)
    println(listOf(OnlyEq(1), OnlyEq(1)).distinct().size)
    println(mapOf(OnlyEq(1) to 5, OnlyEq(2) to 6)[OnlyEq(1)])
}";
    assert_eq!(prog(src), "true\nfalse\ntrue\ntrue\n1\ntrue\n2\n2\nnull\n");
}

#[test]
fn declared_equals_and_hashcode_reach_every_container() {
    // With both halves of the contract supplied, the hash gate opens and every
    // container sees the user's answer — including the `hashCode()` folds, which
    // read the override per element rather than the instance's identity.
    let src = "\
class Both(val a: Int) {
    override fun equals(other: Any?): Boolean = other is Both && other.a == a
    override fun hashCode(): Int = a
}
fun main() {
    println(Both(1) == Both(1))
    println(setOf(Both(1), Both(1), Both(2)).size)
    println(listOf(Both(1), Both(1)).distinct().size)
    println(mapOf(Both(1) to 5, Both(2) to 6)[Both(1)])
    println(mapOf(Both(1) to 5, Both(2) to 6).containsKey(Both(2)))
    println(listOf(Both(3)).hashCode())
    println(setOf(Both(3)).hashCode())
    println((Both(3) to 0).hashCode())
    println(listOf(listOf(Both(1))) == listOf(listOf(Both(1))))
    println((Both(1) to 2) == (Both(1) to 2))
    val m = mutableMapOf(Both(1) to 1)
    m[Both(1)] = 9
    println(m.size)
    println(m[Both(1)])
}";
    assert_eq!(
        prog(src),
        "true\n2\n1\n5\ntrue\n34\n3\n93\ntrue\ntrue\n1\n9\n"
    );
}

#[test]
fn equals_override_runs_even_for_a_self_comparison() {
    // Kotlin's `==` lowers to `Intrinsics.areEqual(a, b)` — `a?.equals(b)` with
    // NO `a === b` short-circuit — and `ArrayList.indexOf` calls `equals` per
    // element the same way. A counting override therefore fires on `x == x`.
    // A hash container is the exception: `HashMap.getNode` tests `k == key`
    // before `key.equals(k)`, so a lookup by the stored object skips the body.
    // The counts below are the observable difference, and they were measured
    // against the reference toolchain rather than reasoned about — an earlier
    // reading of this test had the `==` lines at 0 calls and was wrong.
    let src = "\
var calls = 0
class C(val a: Int) {
    override fun equals(other: Any?): Boolean {
        calls = calls + 1
        return other is C && other.a == a
    }
    override fun hashCode(): Int = a
}
fun main() {
    val x = C(1)
    val y = C(2)
    println(x == x)
    println(calls)
    println(x == C(1))
    println(calls)
    calls = 0
    println(listOf(x, y).contains(x))
    println(\"list: \" + calls)
    calls = 0
    println(setOf(x, y).contains(x))
    println(\"set: \" + calls)
    calls = 0
    println(mapOf(x to 1, y to 2)[x])
    println(\"map: \" + calls)
    calls = 0
    println(listOf(x, y).distinct().size)
    println(\"distinct: \" + calls)
}";
    assert_eq!(
        prog(src),
        "true\n1\ntrue\n2\ntrue\nlist: 1\ntrue\nset: 0\n1\nmap: 0\n2\ndistinct: 0\n"
    );
}

#[test]
fn contains_all_uses_the_receivers_equality_rule() {
    // `Collection.containsAll`, vacuous on an empty argument.
    assert_eq!(
        prog("fun main() { println(listOf(1, 2, 3).containsAll(listOf(1, 3))) }"),
        "true\n"
    );
    assert_eq!(
        prog("fun main() { println(listOf(1, 2).containsAll(listOf(4))) }"),
        "false\n"
    );
    assert_eq!(
        prog("fun main() { println(listOf(1).containsAll(listOf<Int>())) }"),
        "true\n"
    );
}

#[test]
fn set_and_map_equality_go_through_the_hash_gate() {
    // `AbstractSet.equals` is `size` plus `containsAll`, and `AbstractMap.equals`
    // looks each key up with `get` — both HASH-gated. So two sets holding
    // element-wise "equal" instances of a class that declares `equals` without
    // `hashCode` are NOT equal, while the same lists ARE. Comparing set elements
    // with a bare `equals` answers `true` on the first line and is wrong.
    let src = "\
class E(val a: Int) {
    override fun equals(other: Any?): Boolean = other is E && other.a == a
}
class B(val a: Int) {
    override fun equals(other: Any?): Boolean = other is B && other.a == a
    override fun hashCode(): Int = a
}
data class D(val a: Int)
fun main() {
    println(setOf(E(2), E(9)) == setOf(E(2), E(9)))
    println(setOf(B(2), B(9)) == setOf(B(2), B(9)))
    println(setOf(D(2), D(9)) == setOf(D(2), D(9)))
    println(setOf(1, 2) == setOf(2, 1))
    println(mapOf(E(1) to 1, E(2) to 2) == mapOf(E(1) to 1, E(2) to 2))
    println(mapOf(B(1) to 1, B(2) to 2) == mapOf(B(1) to 1, B(2) to 2))
    println(mapOf(1 to \"a\") == mapOf(1 to \"a\"))
    println(listOf(E(2), E(9)) == listOf(E(2), E(9)))
}";
    assert_eq!(
        prog(src),
        "false\ntrue\ntrue\ntrue\nfalse\ntrue\ntrue\ntrue\n"
    );
}

#[test]
fn map_literal_collapses_a_repeated_key_by_kotlin_equality() {
    // `mapOf` fills a `LinkedHashMap` by `put`, so a repeated key keeps its
    // first POSITION and takes the last VALUE. The key match is Kotlin
    // equality, not object identity — comparing handles left two entries for
    // every structural key (a `data class`, a `List`, a declared `equals`).
    let src = "\
data class D(val a: Int)
fun main() {
    println(mapOf(D(1) to 1, D(1) to 2)[D(1)])
    println(mapOf(D(1) to 1, D(1) to 2).size)
    println(mapOf(\"x\" to 1, \"x\" to 2)[\"x\"])
    println(mapOf(listOf(1) to 1, listOf(1) to 2).size)
    println(mapOf(listOf(1) to 1, listOf(2) to 2).size)
    println(mapOf(1 to 1, 1 to 2)[1])
}";
    assert_eq!(prog(src), "2\n1\n2\n1\n2\n2\n");
}

#[test]
fn any_members_on_a_non_instance_receiver_ignore_user_overrides() {
    // Virtual dispatch skipped the runtime class-tag test whenever exactly ONE
    // class in the program declared the member — sound only when the receiver's
    // static class is already known to be that class. With an unknown receiver
    // the candidate set is "every class declaring the name", and the receiver
    // may be none of them, so a single `hashCode` override anywhere made
    // `(0).hashCode()` call it and die on the class's own field. The same held
    // for `toString` and `equals`.
    let src = "\
class Both(val a: Int) {
    override fun equals(other: Any?): Boolean = other is Both && other.a == a
    override fun hashCode(): Int = a
    override fun toString(): String = \"B\" + a
}
fun main() {
    println((0).hashCode())
    println((-1).hashCode())
    println(\"s\".hashCode())
    println(listOf(1, 2).hashCode())
    println(mapOf(0 to \"x\", 0 to \"y\").keys.hashCode())
    println(Both(7).hashCode())
    println(1.toString())
    println(listOf(1).toString())
    println(Both(7).toString())
    println((1).equals(1))
    println(\"a\".equals(\"a\"))
    println(Both(7).equals(Both(7)))
}";
    assert_eq!(
        prog(src),
        "0\n-1\n115\n994\n0\n7\n1\n[1]\nB7\ntrue\ntrue\ntrue\n"
    );
}

#[test]
fn collection_operators_are_conventions_not_arithmetic() {
    // Kotlin's `+`/`-` on a collection resolve to `plus`/`minus` and answer a
    // COLLECTION. Lowering them to the native arithmetic ops coerced the object
    // handle to a number, so `listOf(1, 2, 3) - 2` evaluated to `-2.0` — a
    // collection operation silently answering with arithmetic.
    assert_eq!(
        prog("fun main() { println(listOf(1, 2, 3) - 2) }"),
        "[1, 3]\n"
    );
    assert_eq!(
        prog("fun main() { println(listOf(1, 2, 3) + 4) }"),
        "[1, 2, 3, 4]\n"
    );
    // `minus(element)` drops the FIRST match only; `minus(elements)` drops all.
    assert_eq!(
        prog("fun main() { println(listOf(1, 2, 2, 3) - 2) }"),
        "[1, 2, 3]\n"
    );
    assert_eq!(
        prog("fun main() { println(listOf(1, 2, 2, 3) - listOf(2)) }"),
        "[1, 3]\n"
    );
    // The `Iterable` overload wins whenever the argument is one, even where the
    // receiver's own elements are collections.
    assert_eq!(
        prog("fun main() { println(listOf(listOf(1)) + listOf(2)) }"),
        "[[1], 2]\n"
    );
    // A String argument is not an Iterable — `CharSequence` does not implement
    // it — so this appends the string whole rather than its characters.
    assert_eq!(
        prog(r#"fun main() { println(listOf("x") + "y") }"#),
        "[x, y]\n"
    );
}

#[test]
fn a_collection_operator_the_stdlib_lacks_fails_loudly() {
    // `times`/`div`/`rem` are not collection conventions. The point of routing
    // `+`/`-` away from the arithmetic ops is that everything else must now
    // REFUSE rather than answer with a coerced handle: a loud failure is the
    // whole improvement over `listOf(1, 2) * 2` quietly being a number.
    let err = prog_err("fun main() { println(listOf(1, 2) * 2) }");
    assert!(err.contains("unresolved reference"), "stderr was: {err}");
    assert!(err.contains("times"), "stderr was: {err}");
    let err = prog_err(r#"fun main() { println(mapOf("a" to 1) / 2) }"#);
    assert!(err.contains("unresolved reference"), "stderr was: {err}");
}

#[test]
fn plus_assign_mutates_where_plus_rebinds() {
    // The `val`/`var` split is Kotlin's, and it is observable only through an
    // alias. `var l: List` takes `plus` and REBINDS the name to a fresh list,
    // leaving an alias behind; `val m: MutableList` takes `plusAssign` and
    // mutates the one object the alias also sees. Answering both with the same
    // lowering would be right on the receiver and wrong on every alias.
    let src = "\
fun main() {
    val m = mutableListOf(1, 2)
    val shared = m
    m += 3
    println(shared)
    var l = listOf(1, 2)
    val snapshot = l
    l += 3
    println(snapshot)
    println(l)
}";
    assert_eq!(prog(src), "[1, 2, 3]\n[1, 2]\n[1, 2, 3]\n");
}

#[test]
fn user_declared_operator_conventions_dispatch_to_their_methods() {
    // Every operator Kotlin defines as a convention resolves to the class's own
    // method, not to an instruction — including the ones whose operands or
    // result are not the receiver's type (`contains` flips them, `compareTo`
    // answers a Boolean about the method's SIGN).
    let src = "\
class V(val x: Int) {
    operator fun plus(o: V) = V(x + o.x)
    operator fun div(o: V) = V(x / o.x)
    operator fun unaryMinus() = V(-x)
    operator fun not() = V(x * 100)
    operator fun contains(v: Int) = v == x
    operator fun get(i: Int) = x + i
    operator fun compareTo(o: V) = x.compareTo(o.x)
    operator fun inc() = V(x + 10)
    override fun toString() = \"V(\" + x + \")\"
}
fun main() {
    println(V(7) + V(2))
    println(V(7) / V(2))
    println(-V(7))
    println(!V(7))
    println(7 in V(7))
    println(9 in V(7))
    println(V(7)[5])
    println(V(7) < V(2))
    println(V(7) >= V(2))
    var c = V(1)
    c++
    println(c)
}";
    assert_eq!(
        prog(src),
        "V(9)\nV(3)\nV(-7)\nV(700)\ntrue\nfalse\n12\nfalse\ntrue\nV(11)\n"
    );
}

#[test]
fn a_bare_property_read_keeps_its_declared_type() {
    // `infer` has to agree with what the emitter produces for the same node.
    // A bare `x` inside a method is an implicit-`this` property read, and
    // inferring it `Unknown` sent `x / 2` down the Double division path while
    // `this.x / 2` and `o.x / 2` truncated — the same expression answering two
    // different numbers depending on how the receiver was spelled.
    let src = "\
class V(val x: Int) {
    fun bare() = x / 2
    fun qualified() = this.x / 2
    fun other(o: V) = o.x / 2
}
fun main() {
    println(V(7).bare())
    println(V(7).qualified())
    println(V(7).other(V(7)))
}";
    assert_eq!(prog(src), "3\n3\n3\n");
}

#[test]
fn a_super_call_keeps_its_declared_return_type() {
    // `super<T>.m()` inferred `Unknown`, so concatenating two String-returning
    // super calls compiled as ARITHMETIC. That stayed invisible because
    // fusevm's `Op::Add` concatenates two strings anyway — until an Int operand
    // joined the expression and earned it the 32-bit narrowing, which coerced
    // the built string to 0.
    let src = "\
interface Left { fun pick(): String = \"L\" }
interface Right { fun pick(): String = \"R\" }
class Both(val k: Int) : Left, Right {
    fun joined(): String = super<Left>.pick() + super<Right>.pick() + 5
    fun withProp(): String = super<Left>.pick() + k
    override fun pick(): String = \"x\"
}
fun main() {
    println(Both(5).joined())
    println(Both(5).withProp())
}";
    assert_eq!(prog(src), "LR5\nL5\n");
}

#[test]
fn hash_collections_iterate_in_bucket_order_not_insertion_order() {
    // `java.util.HashMap` iterates its bucket TABLE. Storing these in insertion
    // order printed an order the reference toolchain never produces — a silent
    // wrong answer that looked plausible because the entries were all there.
    assert_eq!(
        prog(
            r#"fun main() { println(hashMapOf("banana" to 1, "apple" to 2, "cherry" to 3, "zebra" to 4)) }"#
        ),
        "{banana=1, zebra=4, apple=2, cherry=3}\n"
    );
    // The table a builder starts from is sized from the element count, not
    // fixed at 16, and that changes the mask and so the order: the same five
    // keys come out differently through `hashSetOf` than through repeated adds
    // to a default-capacity `HashSet`.
    assert_eq!(
        prog("fun main() { println(hashSetOf(10, 3, 7, 1, 25)) }"),
        "[1, 25, 10, 3, 7]\n"
    );
    assert_eq!(
        prog("fun main() { val h = HashMap<Int, String>(); for (i in listOf(10, 3, 7, 1, 25)) h[i] = \"v\" + i; println(h) }"),
        "{1=v1, 3=v3, 7=v7, 25=v25, 10=v10}\n"
    );
    // `linkedSetOf` IS insertion-ordered and `sortedSetOf` is a TreeSet — the
    // three disciplines have to stay distinguishable.
    assert_eq!(
        prog(r#"fun main() { println(linkedSetOf("banana", "apple", "cherry")) }"#),
        "[banana, apple, cherry]\n"
    );
    assert_eq!(
        prog(r#"fun main() { println(sortedSetOf("banana", "apple", "cherry")) }"#),
        "[apple, banana, cherry]\n"
    );
}

#[test]
fn jvm_collection_constructors_build_and_copy() {
    let src = "\
fun main() {
    val m = HashMap<String, Int>()
    m[\"a\"] = 1
    println(m[\"a\"])
    println(m.size)
    println(LinkedHashMap(mapOf(\"b\" to 2)))
    println(ArrayList(listOf(3, 1, 2)))
    println(HashSet(listOf(\"zebra\", \"apple\")))
    println(ArrayList<Int>())
}";
    assert_eq!(prog(src), "1\n1\n{b=2}\n[3, 1, 2]\n[zebra, apple]\n[]\n");
}

#[test]
fn grouping_by_counts_per_key_in_first_encounter_order() {
    // `eachCount` fills a LinkedHashMap, so the keys come out in the order they
    // were first seen — not sorted, and not in the source's element order.
    assert_eq!(
        prog(
            r#"fun main() { println(listOf("a", "bb", "cc", "d", "eee").groupingBy { it.length }.eachCount()) }"#
        ),
        "{1=2, 2=2, 3=1}\n"
    );
    assert_eq!(
        prog("fun main() { println(listOf(1, 1, 2, 3, 3, 3).groupingBy { it }.eachCount()) }"),
        "{1=2, 2=1, 3=3}\n"
    );
    assert_eq!(
        prog("fun main() { println(emptyList<String>().groupingBy { it }.eachCount()) }"),
        "{}\n"
    );
}

#[test]
fn enum_valueof_rejects_an_unknown_name_the_way_the_jvm_does() {
    // The frozen corpus can only hold programs that EXIT ZERO, so the fault
    // path lives here. Both the exception type and the message are the JVM's —
    // `kotlinc` prints exactly `No enum constant Dir.UP`.
    let src = r#"
enum class Dir { NORTH, SOUTH }
fun main() {
    try { Dir.valueOf("UP") } catch (e: IllegalArgumentException) { println(e.message) }
}"#;
    assert_eq!(stdout(src), "No enum constant Dir.UP\n");

    let out = eval("enum class E { X }\nfun main() { println(E.valueOf(\"Q\")) }");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("IllegalArgumentException") && err.contains("No enum constant E.Q"),
        "stderr was: {err}"
    );
}

#[test]
fn enum_constant_argument_list_must_match_the_primary_constructor() {
    // A constant is the only place an enum's constructor is called, so a
    // missing argument has to be caught rather than becoming a missing field.
    // `kotlinc` rejects it too: "no value passed for parameter 'a'".
    let out = eval("enum class E(val a: Int) { X }\nfun main() { println(E.X) }");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("takes 1 argument"), "stderr was: {err}");
}

#[test]
fn enum_cannot_redeclare_the_two_properties_enum_itself_declares() {
    // `name`/`ordinal` are appended to the primary constructor by the enum
    // lowering, so a declared one would silently occupy the same slot.
    // `kotlinc` rejects it as "hides member of supertype 'Enum'".
    for src in [
        "enum class E(val name: String) { X(\"a\") }\nfun main() { println(E.X) }",
        "enum class E(val ordinal: Int) { X(1) }\nfun main() { println(E.X) }",
    ] {
        let out = eval(src);
        assert!(!out.status.success(), "expected rejection for {src:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("already declared by Enum"),
            "stderr was: {err}"
        );
    }
}

#[test]
fn an_interface_property_may_be_declared_but_never_initialized() {
    // A DECLARATION is storage-free and legal; an initializer needs a field an
    // interface has none of, which `kotlinc` rejects as "property initializers
    // in interfaces are prohibited".
    let src = r#"
interface I { val tag: String }
class C(override val tag: String) : I
fun main() {
    val i: I = C("hi")
    println(i.tag)
}"#;
    assert_eq!(stdout(src), "hi\n");

    let out = eval("interface I { val x: Int = 5 }\nfun main() { println(1) }");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("interfaces have none"), "stderr was: {err}");
}

#[test]
fn a_plain_class_compares_by_identity_and_a_data_class_by_value() {
    // Kotlin's `==` is `Any.equals` unless the class overrides it or is a
    // `data class`, so `K(1) == K(1)` is FALSE. The same question is asked
    // wherever value equality is consulted, not only at `==`.
    let src = r#"
class K(val v: Int)
fun main() {
    val a = K(1)
    println(a == K(1))
    println(a == a)
    println(listOf(K(1)).contains(K(1)))
    println(listOf(a).contains(a))
}"#;
    assert_eq!(stdout(src), "false\ntrue\nfalse\ntrue\n");

    // A `when` arm is an `==` against the subject. Comparing heap HANDLES with
    // the native op instead would let the first arm win whatever the subject is.
    let when_src = r#"
data class P(val x: Int)
fun main() {
    val p = P(2)
    println(when (p) { P(1) -> "one"; P(2) -> "two"; else -> "other" })
}"#;
    assert_eq!(stdout(when_src), "two\n");
}

#[test]
fn a_declared_equals_decides_its_own_equality() {
    // The override ignores `tag`, so two instances differing only there are
    // equal — which neither identity nor a structural compare over every field
    // would answer. Every equality-based member has to reach the declared body,
    // not just `==`: a `contains` that fell back to comparing all the fields
    // would answer `false` for the third line, and a `Set` that hashed by
    // identity would keep both elements on the fourth.
    let src = r#"
class P(val v: Int, val tag: Int) {
    override fun equals(other: Any?): Boolean = other is P && other.v == v
    override fun hashCode(): Int = v
}
fun main() {
    println(P(1, 9) == P(1, 8))
    println(P(1, 9) == P(2, 9))
    println(listOf(P(1, 9)).contains(P(1, 0)))
    println(setOf(P(1, 9), P(1, 0)).size)
}"#;
    assert_eq!(stdout(src), "true\nfalse\ntrue\n1\n");
}

#[test]
fn an_object_property_reads_through_its_owner_while_the_object_initializes() {
    // An object publishes its singleton only once every initializer has run, so
    // a later initializer naming an earlier property through the QUALIFIED form
    // has to resolve to the value already computed rather than to the global
    // that is still unset. The enum lowering emits exactly this shape.
    let src = r#"
class K(val v: Int)
object O {
    val A = K(1)
    val B = K(2)
    val all = listOf(O.A, O.B)
    val total = O.A.v + O.B.v
}
fun main() {
    println(O.all.size)
    println(O.total)
}"#;
    assert_eq!(stdout(src), "2\n3\n");
}

#[test]
fn a_bare_property_read_inside_a_method_keeps_its_declared_type() {
    // The bare name resolves through an implicit `this`, and INFERENCE has to
    // agree or the operands are untyped: `rgb / 2` would divide as `Double` and
    // print `127.5`.
    let src = r#"
class Plain(val rgb: Int) { fun half(): Int = rgb / 2 }
fun main() { println(Plain(255).half()) }"#;
    assert_eq!(stdout(src), "127\n");

    // Same failure in the other direction: a `String`-returning `super<T>.m()`
    // that infers as untyped makes the enclosing `+` arithmetic, and the
    // concatenation is then narrowed to 32 bits.
    let super_src = r#"
interface L { fun pick(): String = "L" }
interface R { fun pick(): String = "R" }
class B(val k: Int) : L, R {
    override fun pick(): String = super<L>.pick() + super<R>.pick() + k
}
fun main() { println(B(5).pick()) }"#;
    assert_eq!(stdout(super_src), "LR5\n");
}

#[test]
fn a_computed_property_runs_its_getter_on_every_read() {
    // `val x get() = …` has no backing field, so it must not be folded into a
    // value stored once at construction.
    let src = r#"
class Counter {
    var n = 0
    val next: Int get() { n += 1; return n }
}
fun main() {
    val c = Counter()
    println(c.next)
    println(c.next)
    println(c.n)
}"#;
    assert_eq!(stdout(src), "1\n2\n2\n");

    // A settable computed property: no initializer and no backing field, the
    // setter writes other state and the getter reads it back. On one line the
    // accessors are separated by `;`. Output captured from kotlinc 2.4.20.
    let settable = r#"
class Temp { var c: Double = 0.0; var f: Double get() = c * 9 / 5 + 32; set(v) { c = (v - 32) * 5 / 9 } }
class T2 {
    var c = 0.0
    var f: Double
        get() = c * 9 / 5 + 32
        set(v) { c = (v - 32) * 5 / 9 }
}
fun main() {
    val t = Temp(); t.f = 212.0; println(t.c); println(t.f)
    val u = T2(); u.f = 32.0; println(u.c); u.f += 18.0; println(u.c)
}"#;
    assert_eq!(stdout(settable), "100.0\n212.0\n0.0\n10.0\n");
}

#[test]
fn a_string_builder_mutates_one_object_through_a_chain() {
    // Every mutator answers the RECEIVER, not a copy: if `append` returned a
    // fresh builder the chain would still print `a1true`, but `sb` itself would
    // be left holding only `a` — which is what this checks.
    let src = r#"
fun main() {
    val sb = StringBuilder()
    val same = sb.append("a").append(1).append(true)
    same.append("!")
    println(sb)
    println(sb.length)
}"#;
    assert_eq!(stdout(src), "a1true!\n7\n");
}

#[test]
fn a_string_builder_indexes_utf16_code_units_like_the_jvm() {
    // `length`, `[i]`, `substring`, and `reverse` all count `char`s, so a
    // supplementary character is TWO positions. A Rust-`char` implementation
    // would answer 3 / `😀` / `😀b` / `b😀a` here and only the last would match.
    let src = r#"
fun main() {
    val sb = StringBuilder("a😀b")
    println(sb.length)
    println(sb[1].code)
    println(sb.substring(1, 3))
    println(StringBuilder("a😀b").reverse())
}"#;
    assert_eq!(
        stdout(src),
        "4\n55357\n\u{1F600}\n b\u{1F600}a\n".replace(" ", "")
    );
}

#[test]
fn two_string_builders_holding_the_same_text_are_not_equal() {
    // `StringBuilder` overrides neither `equals` nor `hashCode`, so `==` is
    // identity. Inferring the constructor's result as an untyped value made the
    // comparison coerce both handles and answer `true`.
    let src = r#"
fun main() {
    val a = StringBuilder("ab")
    println(a == StringBuilder("ab"))
    println(a == a)
    println(a.equals(StringBuilder("ab")))
    println(buildString { append("ab") } == "ab")
}"#;
    assert_eq!(stdout(src), "false\ntrue\nfalse\ntrue\n");
}

#[test]
fn a_string_builder_grows_its_capacity_the_way_the_jvm_does() {
    // 16 by default, `text.length + 16` from a text, and `max(2 * cap + 2, n)`
    // once an append does not fit.
    let src = r#"
fun main() {
    println(StringBuilder().capacity())
    println(StringBuilder("abc").capacity())
    println(StringBuilder(5).capacity())
    val tight = StringBuilder(2)
    tight.append("abcdef")
    println(tight.capacity())
    val grown = StringBuilder()
    repeat(20) { grown.append("x") }
    println(grown.capacity())
}"#;
    assert_eq!(stdout(src), "16\n19\n5\n6\n34\n");
}

#[test]
fn a_string_builder_inherits_the_char_sequence_members_from_string() {
    // The read-only half is delegated rather than reimplemented, so it must
    // answer exactly what the same call on the text would — including the
    // members whose `CharSequence` overload differs from the `Iterable` one
    // (`reversed` is a String, `toList` is a List).
    let src = r#"
fun main() {
    val sb = StringBuilder("abc")
    println(sb.indexOf("b"))
    println(sb.startsWith("ab"))
    println(sb.reversed())
    println(sb.toList())
    println(sb.count())
    println(sb.contains("b"))
    println(sb.isEmpty())
    println(sb is CharSequence)
    println(sb is StringBuilder)
}"#;
    assert_eq!(
        stdout(src),
        "1\ntrue\ncba\n[a, b, c]\n3\ntrue\nfalse\ntrue\ntrue\n"
    );
}

#[test]
fn a_string_builder_reports_the_jvm_index_diagnostics() {
    // A SINGLE index and a RANGE are two different messages — `insert` names
    // both bounds where `deleteCharAt` names one — and `delete` CLAMPS its end
    // instead of throwing.
    //
    // These were pinned to JDK 17's per-call-site wording (`index 9, length 3`
    // / `offset 9, length 3`), which was what the capturing shell's `JAVA_HOME`
    // selected. `String` and `AbstractStringBuilder` route their bounds checks
    // through `Preconditions` from JDK 21 on, so no JDK from 21 answers that,
    // and kotlinrs's own `Double.toString` already followed JDK 19's algorithm
    // — the pair described no real JVM. Re-verified against `kotlinc` 2.4.10 on
    // Homebrew OpenJDK 21.0.12 and 26.0.2, which agree with each other.
    let src = r#"
fun main() {
    try { StringBuilder("abc").deleteCharAt(9) } catch (e: Exception) { println(e) }
    try { StringBuilder("abc").insert(9, "x") } catch (e: Exception) { println(e) }
    try { println(StringBuilder("abc")[9]) } catch (e: Exception) { println(e) }
    println(StringBuilder("abc").delete(1, 99))
}"#;
    assert_eq!(
        stdout(src),
        "java.lang.StringIndexOutOfBoundsException: Index 9 out of bounds for length 3\n\
         java.lang.StringIndexOutOfBoundsException: Range [9, 3) out of bounds for length 3\n\
         java.lang.StringIndexOutOfBoundsException: Index 9 out of bounds for length 3\n\
         a\n"
    );
}

#[test]
fn a_deleted_range_reports_the_bound_it_clamped_to() {
    // The three shapes the builder's range check has, all verified against
    // `kotlinc` 2.4.10 on OpenJDK 21.0.12. `delete`/`replace` clamp `end` to the
    // length BEFORE checking, so a start past the end reports the CLAMPED
    // bound: `delete(9, 10)` on five units says `Range [9, 5)`, never
    // `Range [9, 10)`. `setLength` is the one check the JDK left as its own
    // hand-written message, identical on 17 and 26.
    let src = r#"
fun main() {
    try { StringBuilder("abcde").delete(9, 10) } catch (e: Exception) { println(e) }
    try { StringBuilder("abcde").delete(3, 1) } catch (e: Exception) { println(e) }
    try { StringBuilder("abcde").replace(9, 10, "z") } catch (e: Exception) { println(e) }
    try { StringBuilder("abcde").appendRange("xyz", 0, 9) } catch (e: Exception) { println(e) }
    try { StringBuilder("abcde").setLength(-1) } catch (e: Exception) { println(e) }
    println(StringBuilder("abcde").deleteRange(1, 9))
    println(StringBuilder("abcde").deleteAt(1))
}"#;
    assert_eq!(
        stdout(src),
        "java.lang.StringIndexOutOfBoundsException: Range [9, 5) out of bounds for length 5\n\
         java.lang.StringIndexOutOfBoundsException: Range [3, 1) out of bounds for length 5\n\
         java.lang.StringIndexOutOfBoundsException: Range [9, 5) out of bounds for length 5\n\
         java.lang.IndexOutOfBoundsException: Range [0, 9) out of bounds for length 3\n\
         java.lang.StringIndexOutOfBoundsException: String index out of range: -1\n\
         a\n\
         acde\n"
    );
}

#[test]
fn an_empty_receiver_names_its_own_kind() {
    // Kotlin declares `first`/`single`/`reduce` separately for `List`, `Array`,
    // `Iterable` and `CharSequence`, and each spells its own message. One
    // shared implementation with only a `Set`-or-not flag answered the `List`
    // wording for every kind. Verified against `kotlinc` 2.4.10 on OpenJDK
    // 21.0.12.
    let src = r#"
fun t(f: () -> Any?) {
    try { println(f()) } catch (e: Throwable) { println(e) }
}
fun main() {
    t { listOf<Int>().first() }
    t { arrayOf<Int>().first() }
    t { emptySet<Int>().first() }
    t { "".single() }
    t { arrayOf(1, 2).single() }
    t { emptySet<Int>().reduce { a, _ -> a } }
    t { arrayOf<Int>().reduce { a, _ -> a } }
    t { "".reduce { a, _ -> a } }
    t { listOf<Int>().reduceRight { a, _ -> a } }
    t { arrayOf(1).first { it > 9 } }
    t { "a".first { it > 'z' } }
    t { listOf(1).last { it > 9 } }
    t { emptySet<Int>().last { it > 9 } }
}"#;
    assert_eq!(
        stdout(src),
        "java.util.NoSuchElementException: List is empty.\n\
         java.util.NoSuchElementException: Array is empty.\n\
         java.util.NoSuchElementException: Collection is empty.\n\
         java.util.NoSuchElementException: Char sequence is empty.\n\
         java.lang.IllegalArgumentException: Array has more than one element.\n\
         java.lang.UnsupportedOperationException: Empty collection can't be reduced.\n\
         java.lang.UnsupportedOperationException: Empty array can't be reduced.\n\
         java.lang.UnsupportedOperationException: Empty char sequence can't be reduced.\n\
         java.lang.UnsupportedOperationException: Empty list can't be reduced.\n\
         java.util.NoSuchElementException: Array contains no element matching the predicate.\n\
         java.util.NoSuchElementException: Char sequence contains no character matching the predicate.\n\
         java.util.NoSuchElementException: List contains no element matching the predicate.\n\
         java.util.NoSuchElementException: Collection contains no element matching the predicate.\n"
    );
}

#[test]
fn an_extremum_over_an_empty_receiver_throws_bare() {
    // `max`/`min`/`maxBy`/`maxOf` raise `NoSuchElementException()` with NO
    // argument, unlike `first`/`last`/`single`, which name the receiver kind.
    // `maxBy`/`minBy` are the throwing twins Kotlin deprecated in 1.4 and
    // restored in 1.7; without them a current-Kotlin program was rejected
    // outright. Verified against `kotlinc` 2.4.10 on OpenJDK 21.0.12.
    let src = r#"
fun t(f: () -> Any?) {
    try { println(f()) } catch (e: Throwable) { println(e) }
}
fun main() {
    t { listOf<Int>().max() }
    t { listOf<Int>().maxBy { it } }
    t { listOf<Int>().minBy { it } }
    t { listOf<Int>().maxOf { it } }
    t { listOf<Int>().maxByOrNull { it } }
    t { listOf(1, 5, 3).maxBy { -it } }
    t { listOf(1, 5, 3).minBy { -it } }
}"#;
    assert_eq!(
        stdout(src),
        "java.util.NoSuchElementException\n\
         java.util.NoSuchElementException\n\
         java.util.NoSuchElementException\n\
         java.util.NoSuchElementException\n\
         null\n\
         1\n\
         5\n"
    );
}

#[test]
fn a_positional_add_inserts_rather_than_appending() {
    // `add(index, element)` is a different overload from `add(element)`, and
    // reading only the first argument ran the appending one with the INDEX as
    // the element — a longer list either way, so nothing failed loudly.
    // `ArrayList.rangeCheckForAdd` also has its own message shape, `Index: 9,
    // Size: 3`, which no other position check uses. Verified against `kotlinc`
    // 2.4.10 on OpenJDK 21.0.12.
    let src = r#"
fun main() {
    val m = mutableListOf(1, 2, 3)
    m.add(1, 9)
    println(m)
    m.add(4)
    println(m)
    try { m.add(99, 0) } catch (e: Exception) { println(e) }
    try { m.removeAt(9) } catch (e: Exception) { println(e) }
    try { m[9] = 0 } catch (e: Exception) { println(e) }
    val a = arrayOf(1, 2, 3)
    try { a[9] = 0 } catch (e: Exception) { println(e) }
    try { println(listOf(1, 2).elementAt(9)) } catch (e: Exception) { println(e) }
    try { println(emptySet<Int>().elementAt(9)) } catch (e: Exception) { println(e) }
    try { println(listOf(1, 2, 3).subList(0, 9)) } catch (e: Exception) { println(e) }
    try { println(listOf(1, 2, 3).subList(-1, 2)) } catch (e: Exception) { println(e) }
    try { println(listOf(1, 2, 3).subList(2, 1)) } catch (e: Exception) { println(e) }
}"#;
    assert_eq!(
        stdout(src),
        "[1, 9, 2, 3]\n\
         [1, 9, 2, 3, 4]\n\
         java.lang.IndexOutOfBoundsException: Index: 99, Size: 5\n\
         java.lang.IndexOutOfBoundsException: Index 9 out of bounds for length 5\n\
         java.lang.IndexOutOfBoundsException: Index 9 out of bounds for length 5\n\
         java.lang.ArrayIndexOutOfBoundsException: Index 9 out of bounds for length 3\n\
         java.lang.ArrayIndexOutOfBoundsException: Index 9 out of bounds for length 2\n\
         java.lang.IndexOutOfBoundsException: Collection doesn't contain element at index 9.\n\
         java.lang.IndexOutOfBoundsException: toIndex = 9\n\
         java.lang.IndexOutOfBoundsException: fromIndex = -1\n\
         java.lang.IllegalArgumentException: fromIndex(2) > toIndex(1)\n"
    );
}

#[test]
fn a_string_parse_is_bounded_by_the_width_it_parses_into() {
    // The destination width is part of the parse, not a cast after it, and each
    // failure mode has its own message: a malformed string names the string, an
    // out-of-`Int` value for a narrow target names the string too (because
    // `Byte.parseByte` goes through `Integer.parseInt` first), and only a value
    // inside `Int` but outside the target reports the range. A bad radix is not
    // a `NumberFormatException` at all, which is why even `toIntOrNull` raises
    // it. Verified against `kotlinc` 2.4.10 on OpenJDK 21.0.12.
    let src = r#"
fun t(f: () -> Any?) {
    try { println(f()) } catch (e: Throwable) { println(e) }
}
fun main() {
    t { "99999999999".toInt() }
    t { "-2147483649".toInt() }
    t { "2147483647".toInt() }
    t { "99999999999".toIntOrNull() }
    t { " 5".toInt() }
    t { " 5.0".toDouble() }
    t { "300".toByte() }
    t { "12".toByte() }
    t { "40000".toShort() }
    t { "99999999999".toByte() }
    t { "300".toByteOrNull() }
    t { "8000".toShort(16) }
    t { "ffffffffff".toInt(16) }
    t { "5".toIntOrNull(1) }
}"#;
    assert_eq!(
        stdout(src),
        "java.lang.NumberFormatException: For input string: \"99999999999\"\n\
         java.lang.NumberFormatException: For input string: \"-2147483649\"\n\
         2147483647\n\
         null\n\
         java.lang.NumberFormatException: For input string: \" 5\"\n\
         5.0\n\
         java.lang.NumberFormatException: Value out of range. Value:\"300\" Radix:10\n\
         12\n\
         java.lang.NumberFormatException: Value out of range. Value:\"40000\" Radix:10\n\
         java.lang.NumberFormatException: For input string: \"99999999999\"\n\
         null\n\
         java.lang.NumberFormatException: Value out of range. Value:\"8000\" Radix:16\n\
         java.lang.NumberFormatException: For input string: \"ffffffffff\" under radix 16\n\
         java.lang.IllegalArgumentException: radix 1 was not in valid range 2..36\n"
    );
}

#[test]
fn a_window_check_drops_the_step_when_it_equals_the_size() {
    // `chunked(size)` IS `windowed(size, size)`, so naming both would print the
    // same number twice — the stdlib drops the step there and keeps it (behind
    // `Both`) otherwise. Three call sites each spelled a third message that was
    // neither. Verified against `kotlinc` 2.4.10 on OpenJDK 21.0.12.
    let src = r#"
fun t(f: () -> Any?) {
    try { println(f()) } catch (e: Throwable) { println(e) }
}
fun main() {
    t { listOf(1, 2).chunked(0) }
    t { listOf(1, 2).windowed(0, 2) }
    t { listOf(1, 2).windowed(2, 0) }
    t { listOf(1, 2).windowed(0, 0) }
    t { "ab".chunked(0) }
    t { "ab".windowed(3, 0) }
    t { listOf(1, 2).chunked(0) { it } }
}"#;
    assert_eq!(
        stdout(src),
        "java.lang.IllegalArgumentException: size 0 must be greater than zero.\n\
         java.lang.IllegalArgumentException: Both size 0 and step 2 must be greater than zero.\n\
         java.lang.IllegalArgumentException: Both size 2 and step 0 must be greater than zero.\n\
         java.lang.IllegalArgumentException: size 0 must be greater than zero.\n\
         java.lang.IllegalArgumentException: size 0 must be greater than zero.\n\
         java.lang.IllegalArgumentException: Both size 3 and step 0 must be greater than zero.\n\
         java.lang.IllegalArgumentException: size 0 must be greater than zero.\n"
    );
}

#[test]
fn a_failed_cast_names_both_classes_in_full_and_where_they_live() {
    // The JVM's `ClassCastException` names binary class names and appends where
    // each one lives, merging the two locations when they agree — three message
    // shapes. The short Kotlin labels named neither, and a primitive receiver
    // had no label at all: `1 as Foo` said `class value cannot be cast`.
    // Verified against `kotlinc` 2.4.10 on OpenJDK 21.0.12.
    let src = r#"
class Foo
class Bar
fun t(f: () -> Any?) {
    try { println(f()) } catch (e: Throwable) { println(e) }
}
fun main() {
    t { val a: Any = "s"; a as Int }
    t { val a: Any = 1; a as Foo }
    t { val a: Any = Foo(); a as Bar }
    t { val a: Any = 1.5; a as Int }
    t { val a: Any = true; a as Int }
    t { val a: Any = 'c'; a as Int }
    t { val a: Any = Foo(); a as Int }
}"#;
    assert_eq!(
        stdout(src),
        "java.lang.ClassCastException: class java.lang.String cannot be cast to class \
         java.lang.Integer (java.lang.String and java.lang.Integer are in module java.base \
         of loader 'bootstrap')\n\
         java.lang.ClassCastException: class java.lang.Integer cannot be cast to class Foo \
         (java.lang.Integer is in module java.base of loader 'bootstrap'; Foo is in unnamed \
         module of loader 'app')\n\
         java.lang.ClassCastException: class Foo cannot be cast to class Bar (Foo and Bar are \
         in unnamed module of loader 'app')\n\
         java.lang.ClassCastException: class java.lang.Double cannot be cast to class \
         java.lang.Integer (java.lang.Double and java.lang.Integer are in module java.base \
         of loader 'bootstrap')\n\
         java.lang.ClassCastException: class java.lang.Boolean cannot be cast to class \
         java.lang.Integer (java.lang.Boolean and java.lang.Integer are in module java.base \
         of loader 'bootstrap')\n\
         java.lang.ClassCastException: class java.lang.Character cannot be cast to class \
         java.lang.Integer (java.lang.Character and java.lang.Integer are in module java.base \
         of loader 'bootstrap')\n\
         java.lang.ClassCastException: class Foo cannot be cast to class java.lang.Integer \
         (Foo is in unnamed module of loader 'app'; java.lang.Integer is in module java.base \
         of loader 'bootstrap')\n"
    );
}

#[test]
fn a_precondition_message_block_runs_only_when_the_check_fails() {
    // The lazy message is the whole point of the lambda overload: on the
    // passing path it must not run at all.
    let src = r#"
fun main() {
    var ran = false
    require(true) { ran = true; "unused" }
    check(true) { ran = true; "unused" }
    println(ran)
    try { require(false) { "bad input" } } catch (e: Exception) { println(e) }
    try { check(false) } catch (e: Exception) { println(e) }
    println(checkNotNull(5))
}"#;
    assert_eq!(
        stdout(src),
        "false\n\
         java.lang.IllegalArgumentException: bad input\n\
         java.lang.IllegalStateException: Check failed.\n\
         5\n"
    );
}

#[test]
fn todo_throws_an_error_that_catch_exception_does_not_catch() {
    // `NotImplementedError` descends from `Error`, so the `Exception` arm must
    // be skipped — the hierarchy, not just the message, has to be right.
    let src = r#"
fun main() {
    try { TODO("later") } catch (e: Exception) { println("wrong arm") } catch (e: Throwable) { println(e) }
}"#;
    assert_eq!(
        stdout(src),
        "kotlin.NotImplementedError: An operation is not implemented: later\n"
    );
}

#[test]
fn the_bulk_mutators_report_whether_the_receiver_changed() {
    // Each answers "did this change anything", NOT the argument's size — and a
    // `MutableSet` skips what it already holds where a `MutableList` appends
    // every element it is given.
    let src = r#"
fun main() {
    val xs = mutableListOf(1, 1, 2)
    println(xs.addAll(listOf<Int>()))
    println(xs.addAll(listOf(1)))
    println(xs)
    println(xs.removeAll(listOf(1)))
    println(xs)
    val s = mutableSetOf(1, 2)
    println(s.addAll(listOf(2, 3)))
    println(s)
    println(s.addAll(listOf(3)))
    println(s.retainAll(listOf(3)))
    println(s)
}"#;
    assert_eq!(
        stdout(src),
        "false\ntrue\n[1, 1, 2, 1]\ntrue\n[2]\ntrue\n[1, 2, 3]\nfalse\ntrue\n[3]\n"
    );
}

#[test]
fn a_generic_call_with_only_a_trailing_lambda_is_not_a_comparison() {
    // `buildList<Int> { }` has no parentheses at all, so the type-argument scan
    // has to accept a trailing lambda as the argument list — while `a < b` and
    // `a < b && c > d` stay comparisons.
    assert_eq!(stdout("println(buildList<Int> { })"), "[]\n");
    assert_eq!(
        stdout("val a = 1; val b = 2; val c = 4; val d = 3; println(a < b && c > d)"),
        "true\n"
    );
    assert_eq!(stdout("val a = 1; val b = 2; println(a < b)"), "true\n");
}

#[test]
fn a_generic_class_carries_its_type_argument_into_member_arithmetic() {
    // Kotlin's integer width is a property of the STATIC type, and for a generic
    // class the construction site fixes it: `Box(65536)` makes `T` an `Int`, so
    // the product wraps at 32 bits; `Box(65536L)` makes it a `Long`, which keeps
    // all 64. Both directions are asserted together on purpose — a rule that
    // always narrows a type-variable read gets the first line right and the
    // second wrong, and a rule that never narrows gets exactly the opposite. The
    // frozen parity corpus holds these same shapes against the real toolchain;
    // this pins the pair so the two cannot be traded for one another.
    let src = r#"class Box<T>(val v: T) {
    fun get(): T = v
    val once: T get() = v
}
fun main() {
    println(Box(65536).v * Box(2000000000).v)
    println(Box(65536L).v * Box(2000000000L).v)
    println(Box(65536).get() * Box(2000000000).get())
    println(Box(65536L).get() * Box(2000000000L).get())
    println(Box(65536).once * Box(2000000000).once)
    println(Box(65536L).once * Box(2000000000L).once)
}"#;
    assert_eq!(
        stdout(src),
        "-1811939328\n131072000000000\n-1811939328\n131072000000000\n\
         -1811939328\n131072000000000\n"
    );
}

#[test]
fn a_type_argument_that_is_not_an_int_is_left_alone() {
    // The other half of the rule above: only an `Int`-width type argument may
    // reach the 32-bit wrap. A `String` argument must concatenate, a `Double`
    // must divide by IEEE rules rather than truncate, and a `Char` must stay a
    // `Char` — each is a value the wrap would silently corrupt.
    let src = r#"class Box<T>(val v: T)
class Pair2<A, B>(val a: A, val b: B)
fun main() {
    println(Box("a").v + Box("b").v)
    println(Box(7.0).v / Box(2.0).v)
    println(Box(7).v / Box(2).v)
    println(Box('a').v + 1)
    println(Box(listOf(1, 2)).v)
    println(Pair2(65536L, 2000000000).a * Pair2(65536L, 2000000000).b)
    println(Pair2(65536, 2000000000).a * Pair2(65536, 2000000000).b)
}"#;
    assert_eq!(
        stdout(src),
        "ab\n3.5\n3\nb\n[1, 2]\n131072000000000\n-1811939328\n"
    );
}

#[test]
fn a_written_down_type_argument_carries_the_width_a_construction_site_never_showed() {
    // The construction site is one of two sources of a type argument; the other
    // is the annotation. Each line below reaches a source the other cannot: the
    // result of a function whose body the call site never sees, a parameter
    // whose caller is across the call, a property of another class, a supertype
    // a subclass with no type parameters of its own extends, a top-level `val`,
    // a local annotation over an opaque initializer, and a cast.
    //
    // Every one has a `Long` twin at the same magnitudes, and the twins are
    // asserted in the same run on purpose — a rule that fixes the `Int` case by
    // narrowing whatever a written annotation touches gets every odd line right
    // and every even line wrong.
    let src = r#"class Box<T>(val v: T) {
    fun get(): T = v
    val once: T get() = v
}
open class Open<T>(val v: T)
class Sub : Open<Int>(65536)
class SubL : Open<Long>(65536L)
class Hold(val b: Box<Int>, val bl: Box<Long>)
fun mkInt(): Box<Int> = Box(65536)
fun mkLong(): Box<Long> = Box(65536L)
fun takeInt(b: Box<Int>): Int = b.v * 2000000000
fun takeLong(b: Box<Long>): Long = b.v * 2000000000L
fun anyOf(x: Any): Any = Box(x)
val TOPI: Box<Int> = mkInt()
val TOPL: Box<Long> = mkLong()
fun main() {
    println(mkInt().v * 2000000000)
    println(mkLong().v * 2000000000L)
    println(mkInt().get() * mkInt().once)
    println(mkLong().get() * mkLong().once)
    println(takeInt(Box(65536)))
    println(takeLong(Box(65536L)))
    println(Hold(Box(65536), Box(1L)).b.v * 2000000000)
    println(Hold(Box(1), Box(65536L)).bl.v * 2000000000L)
    println(Sub().v * 2000000000)
    println(SubL().v * 2000000000L)
    println(TOPI.v * 2000000000)
    println(TOPL.v * 2000000000L)
    val a: Box<Int> = mkInt()
    val b: Box<Long> = mkLong()
    println(a.v * 2000000000)
    println(b.v * 2000000000L)
    println((anyOf(65536) as Box<Int>).v * 2000000000)
    println((anyOf(65536L) as Box<Long>).v * 2000000000L)
}"#;
    assert_eq!(
        stdout(src),
        "-1811939328\n131072000000000\n0\n4294967296\n\
         -1811939328\n131072000000000\n-1811939328\n131072000000000\n\
         -1811939328\n131072000000000\n-1811939328\n131072000000000\n\
         -1811939328\n131072000000000\n-1811939328\n131072000000000\n"
    );
}

#[test]
fn a_secondary_constructor_and_a_body_property_carry_the_type_argument_too() {
    // Two holes the construction-site path left. A call that selects a SECONDARY
    // constructor was matched against the PRIMARY's parameters — which take a
    // different count and mean something else — so it resolved nothing; the
    // parameters matched are now the ones the constructor selector answers. And
    // a BODY property is not a constructor parameter at all, so nothing fixed
    // its argument even though it reads the one the receiver already carries.
    //
    // The `Long` twins are what keep either fix from being "narrow it".
    let src = r#"class Box<T>(v: T) {
    val w: T = v
}
class Sec<T>(val v: T) {
    constructor(a: T, b: T) : this(a)
}
fun main() {
    println(Box(65536).w * Box(65536).w)
    println(Box(65536L).w * Box(65536L).w)
    println(Sec(65536, 46341).v * 2000000000)
    println(Sec(65536L, 46341L).v * 2000000000L)
    println(Sec(65536).v * 2000000000)
}"#;
    assert_eq!(
        stdout(src),
        "0\n4294967296\n-1811939328\n131072000000000\n-1811939328\n"
    );
}

#[test]
fn a_written_type_argument_that_is_not_an_int_is_left_alone() {
    // The written-down source obeys the same allow-list as the inferred one:
    // only an `Int`-width argument may reach the 32-bit wrap. A `String` must
    // concatenate, a `Double` must divide by IEEE rules rather than truncate,
    // and a nested argument must reach the width two levels down rather than
    // narrowing the outer read.
    let src = r#"class Box<T>(val v: T) {
    val w: T = v
}
fun mkStr(): Box<String> = Box("s")
fun mkDbl(): Box<Double> = Box(7.0)
fun mkNest(): Box<Box<Int>> = Box(Box(70000))
fun takeStr(b: Box<String>): String = b.v + b.w
fun main() {
    println(mkStr().v + mkStr().w)
    println(mkDbl().v / 2)
    println(takeStr(Box("ab")))
    println(mkNest().v.v * 46341)
    println(mkNest().v.v.toLong() * 46341L)
}"#;
    assert_eq!(stdout(src), "ss\n3.5\nabab\n-1051097296\n3243870000\n");
}

#[test]
fn a_type_argument_list_the_parser_cannot_read_is_skipped_not_rejected() {
    // The `<…>` list is now parsed rather than skipped, and Kotlin's type
    // grammar has corners this frontend has never read. A list that fails to
    // parse must rewind to the old token-depth skip and leave the annotation
    // with no arguments — narrowing nothing — rather than failing the program.
    //
    // A star projection and a variance modifier are read (as unresolved and as
    // their bare type); a function type nested in an argument must NOT publish
    // its signature as if the annotation itself were callable, which would make
    // a list-valued binding lower as a closure.
    let src = r#"fun main() {
    val a: List<*> = listOf(1, 2)
    val b: List<out Number> = listOf(3)
    val c: List<(Int) -> Int> = listOf({ x: Int -> x * 2 })
    val d: Map<String, List<Int>> = mapOf("k" to listOf(4, 5))
    println(a)
    println(b)
    println(c.size)
    println(c[0](21))
    println(d["k"])
}"#;
    assert_eq!(stdout(src), "[1, 2]\n[3]\n1\n42\n[4, 5]\n");
}

#[test]
fn a_computed_property_is_typed_by_its_declared_result() {
    // `val d: Int get() = k` is a zero-argument method wearing property syntax.
    // `compile_member` always resolved it as one, but inference did not look
    // there at all, so the read was untyped and `C(a).d + C(b).d` skipped the
    // 32-bit wrap that `C(a).f() + C(b).f()` was already getting. The `Long`
    // line is what keeps the fix from being "narrow every member read".
    let src = r#"class C(val k: Int) {
    val d: Int get() = k
    val e: Long get() = k.toLong()
}
fun main() {
    println(C(2000000000).d + C(2000000000).d)
    println(C(2000000000).e + C(2000000000).e)
}"#;
    assert_eq!(stdout(src), "-294967296\n4000000000\n");
}

#[test]
fn a_primitive_receiver_keeps_its_width_through_a_member_call() {
    // A member call on an untyped receiver is decided by runtime class tag so a
    // user override can win. A receiver whose coarse type is a PRIMITIVE is not
    // one of those: no user instance is a `Long`, so the tag dispatch can only
    // take its fallback arm — and that arm drops the static width the width-
    // sensitive members push along. `(-7L).hashCode()` is the `Long` fold, `6`;
    // it answered the 32-bit `-7` in any program where some user class happened
    // to declare `hashCode`, because that alone made the candidate list
    // non-empty. The `String` line is the same rule for a member that is not
    // width-sensitive: a user `uppercase()` must not capture `"ab".uppercase()`.
    let src = r#"class H(val a: Int) {
    override fun hashCode(): Int = a * 31
    fun uppercase(): String = "OVERRIDE"
}
fun main() {
    println((-7L).hashCode())
    println((-7).hashCode())
    println((-1L).hashCode())
    println("ab".uppercase())
    println(1L shl 32)
    println(1 shl 32)
}"#;
    assert_eq!(stdout(src), "6\n-7\n0\nAB\n4294967296\n1\n");
}

#[test]
fn an_override_still_dispatches_through_an_untyped_receiver() {
    // The other direction, and the reason the rule above is written on the
    // receiver's TYPE rather than on the member's name: a `for` variable, a
    // lambda parameter and a list element carry no static class, so the runtime
    // tag is the only thing that can reach the user's `hashCode`/`toString`.
    let src = r#"class H(val a: Int) {
    override fun hashCode(): Int = a * 31
    override fun toString(): String = "E" + a
}
fun main() {
    val xs = listOf(H(3), H(4))
    for (x in xs) println(x.hashCode())
    println(xs.map { it.hashCode() })
    println(xs.map { it.toString() })
    println(xs[0].hashCode())
    println(H(5).hashCode())
}"#;
    assert_eq!(stdout(src), "93\n124\n[93, 124]\n[E3, E4]\n93\n155\n");
}
#[test]
fn identity_asks_a_different_question_than_structural_equality() {
    // The pair that makes `===` worth having: two independently built lists are
    // `==` and not `===`, and a `data class`'s generated `equals` moves only the
    // first answer. Aliasing the SAME object flips it back.
    let src = r#"
data class P(val x: Int)
fun main() {
    val a = listOf(1, 2)
    val b = listOf(1, 2)
    println(a == b)
    println(a === b)
    println(a === a)
    val p = P(1)
    val q = P(1)
    val r = p
    println(p == q)
    println(p === q)
    println(p === r)
    println(p !== q)
}"#;
    assert_eq!(stdout(src), "true\nfalse\ntrue\ntrue\nfalse\ntrue\ntrue\n");
}

#[test]
fn identity_on_unboxed_values_is_value_comparison() {
    // Nothing here is boxed, so identity on a number, `Char`, `Boolean`,
    // `String` or `null` is value equality — which is the JVM's answer too for
    // primitives at their declared type and for interned string literals.
    assert_eq!(stdout("println(1 === 1)"), "true\n");
    assert_eq!(stdout("println(1 !== 2)"), "true\n");
    assert_eq!(stdout(r#"println("x" === "x")"#), "true\n");
    assert_eq!(stdout("println('a' === 'a')"), "true\n");
    assert_eq!(stdout("println(true === true)"), "true\n");
    assert_eq!(
        stdout("val a: String? = null; println(a === null)"),
        "true\n"
    );
}

#[test]
fn the_three_character_operators_out_lex_the_two_character_ones() {
    // `===` must be tested before the `==` it starts with: scanning greedily
    // left to right would leave a stray `=` and report `unexpected token
    // Assign`. Both spellings have to keep working side by side.
    assert_eq!(
        stdout("val a = listOf(1); val b = a; println(a == b && a === b)"),
        "true\n"
    );
    assert_eq!(
        stdout("val a = listOf(1); println(a != listOf(2) && a !== listOf(1))"),
        "true\n"
    );
    // A genuine assignment after a comparison still parses as an assignment.
    assert_eq!(
        stdout("var n = 0; if (1 === 1) { n = 7 }; println(n)"),
        "7\n"
    );
}

#[test]
fn the_from_the_end_selectors_partition_the_receiver() {
    // `takeLast(n)` and `dropLast(n)` split at the same point from the other
    // end, clamp an oversized count, and fault on a negative one with the same
    // message `take`/`drop` use.
    assert_eq!(stdout("println(listOf(1,2,3,4).takeLast(2))"), "[3, 4]\n");
    assert_eq!(stdout("println(listOf(1,2,3,4).dropLast(2))"), "[1, 2]\n");
    assert_eq!(stdout("println(listOf(1,2).takeLast(9))"), "[1, 2]\n");
    assert_eq!(stdout("println(listOf(1,2).dropLast(9))"), "[]\n");
    assert_eq!(stdout(r#"println("abcde".takeLast(2))"#), "de\n");
    assert_eq!(stdout(r#"println("abcde".dropLast(2))"#), "abc\n");

    let out = eval("println(listOf(1,2).takeLast(-1))");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("Requested element count -1 is less than zero."),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn folding_from_the_right_reverses_both_the_walk_and_the_lambda_arguments() {
    // `foldRight`'s lambda is `(element, acc)` where `fold`'s is `(acc,
    // element)`, so the same lambda gives a different answer for each — that
    // pair is the whole contract.
    assert_eq!(
        stdout(r#"println(listOf(1,2,3).foldRight("") { a, b -> "$a$b" })"#),
        "123\n"
    );
    assert_eq!(
        stdout(r#"println(listOf(1,2,3).fold("") { a, b -> "$a$b" })"#),
        "123\n"
    );
    assert_eq!(
        stdout("println(listOf(1,2,3).foldRight(0) { a, b -> a - b })"),
        "2\n"
    );
    assert_eq!(
        stdout("println(listOf(1,2,3).reduceRight { a, b -> a - b })"),
        "2\n"
    );
    assert_eq!(
        stdout("println(listOf(1,2,3).reduce { a, b -> a - b })"),
        "-4\n"
    );
}

#[test]
fn unzip_and_zip_with_next_are_the_pairing_inverses() {
    assert_eq!(
        stdout(r#"println(listOf(1 to "a", 2 to "b").unzip())"#),
        "([1, 2], [a, b])\n"
    );
    // One shorter than the receiver, so a one-element list pairs nothing.
    assert_eq!(
        stdout("println(listOf(1,2,3).zipWithNext())"),
        "[(1, 2), (2, 3)]\n"
    );
    assert_eq!(stdout("println(listOf(1).zipWithNext())"), "[]\n");
    // The lambda overload transforms each pair instead of yielding it.
    assert_eq!(
        stdout("println(listOf(1,2,3).zipWithNext { a, b -> a + b })"),
        "[3, 5]\n"
    );
}

#[test]
fn map_conversions_and_half_filters() {
    // `Map.toList()` must yield `Pair`s, which print `(1, a)` — the entries
    // themselves would print `1=a`.
    assert_eq!(
        stdout(r#"println(mapOf(1 to "a", 2 to "b").toList())"#),
        "[(1, a), (2, b)]\n"
    );
    // `filterKeys`/`filterValues` hand the lambda that HALF, not the entry.
    assert_eq!(
        stdout(r#"println(mapOf(1 to "a", 2 to "b").filterKeys { it > 1 })"#),
        "{2=b}\n"
    );
    assert_eq!(
        stdout(r#"println(mapOf(1 to "a", 2 to "b").filterValues { it == "a" })"#),
        "{1=a}\n"
    );
    // `toSortedMap` is a TreeMap: key order, whatever the receiver's was.
    assert_eq!(
        stdout(r#"println(mapOf(2 to "b", 1 to "a", 3 to "c").toSortedMap())"#),
        "{1=a, 2=b, 3=c}\n"
    );
    assert_eq!(
        stdout(r#"println(mapOf("b" to 1, "a" to 2).toSortedMap())"#),
        "{a=2, b=1}\n"
    );
}

#[test]
fn equality_on_an_untyped_operand_compares_values_not_their_numeric_coercions() {
    // `Op::NumEq` coerces, and two different strings both coerce to `0` — so
    // every comparison against a string came out `true` wherever inference
    // stopped. It stops at a declared parameter's element type and at a map
    // entry's halves, both of which can hold a string.
    assert_eq!(
        stdout(
            r#"fun f(xs: List<String>) = xs.filter { it == "a" }
fun main() { println(f(listOf("a","b","a"))) }"#
        ),
        "[a, a]\n"
    );
    assert_eq!(
        stdout(r#"println(mapOf(1 to "a", 2 to "b").mapValues { it.value == "a" })"#),
        "{1=true, 2=false}\n"
    );
    // The statically typed comparisons keep their native ops and their answers.
    assert_eq!(stdout("println(1 == 1)"), "true\n");
    assert_eq!(stdout(r#"println("a" == "b")"#), "false\n");
    assert_eq!(stdout("println('a' == 'a')"), "true\n");
    assert_eq!(stdout("val x: Int? = null; println(x == null)"), "true\n");
}

#[test]
fn get_or_else_reads_a_key_on_a_map_and_an_index_on_a_sequence() {
    // One name, two members. The map form was routed through the index form, so
    // the key was coerced to `0` and the entry at that position came back —
    // `a=1` where the fallback was due.
    assert_eq!(
        stdout(r#"println(mapOf("a" to 1).getOrElse("z") { 9 })"#),
        "9\n"
    );
    assert_eq!(
        stdout(r#"println(mapOf("a" to 1).getOrElse("a") { 9 })"#),
        "1\n"
    );
    // The sequence form still hands the missing INDEX to the lambda.
    assert_eq!(
        stdout("println(listOf(1,2).getOrElse(5) { it * 10 })"),
        "50\n"
    );
    assert_eq!(stdout("println(listOf(1,2).getOrElse(1) { 99 })"), "2\n");
}

#[test]
fn comparators_chain_by_key_and_each_key_keeps_its_own_direction() {
    let src = r#"
data class P(val name: String, val age: Int)
fun main() {
    val ps = listOf(P("b", 2), P("a", 2), P("c", 1))
    println(ps.sortedWith(compareBy<P> { it.age }.thenBy { it.name }))
    println(ps.sortedWith(compareBy<P> { it.age }.thenByDescending { it.name }))
    println(ps.sortedWith(compareBy({ it.age }, { it.name })))
}"#;
    // The tiebreak fires only on an equal first key, and `thenByDescending`
    // reverses THAT key alone — the age order stays ascending in both.
    assert_eq!(
        stdout(src),
        "[P(name=c, age=1), P(name=a, age=2), P(name=b, age=2)]\n\
         [P(name=c, age=1), P(name=b, age=2), P(name=a, age=2)]\n\
         [P(name=c, age=1), P(name=a, age=2), P(name=b, age=2)]\n"
    );
}

#[test]
fn then_by_answers_a_new_comparator_and_leaves_the_base_alone() {
    // Kotlin's comparators are immutable, so a base bound to a name must still
    // sort by its own keys after a `thenBy` has been built from it.
    let src = r#"
fun main() {
    val byLen = compareBy<String> { it.length }
    val words = listOf("cc", "a", "bb")
    println(words.sortedWith(byLen))
    println(words.sortedWith(byLen.thenBy { it }))
    println(words.sortedWith(byLen))
}"#;
    assert_eq!(stdout(src), "[a, cc, bb]\n[a, bb, cc]\n[a, cc, bb]\n");
}

#[test]
fn sorted_with_still_takes_a_plain_two_argument_lambda() {
    // `sortedWith` now accepts two different things; the lambda form is the one
    // that already worked and must not have been displaced by the comparator.
    assert_eq!(
        stdout("println(listOf(3,1,2).sortedWith { a, b -> a - b })"),
        "[1, 2, 3]\n"
    );
    assert_eq!(
        stdout("println(listOf(3,1,2).sortedWith { a, b -> b - a })"),
        "[3, 2, 1]\n"
    );
}

#[test]
fn a_local_by_lazy_runs_its_thunk_at_the_first_read_and_only_then() {
    // The whole observable difference from an ordinary initializer: the thunk
    // has not run when the declaration is passed, runs once at the first read,
    // and does not run again.
    let src = r#"
fun main() {
    var n = 0
    val v by lazy { n += 1; n }
    println(n)
    println(v)
    println(v)
    println(n)
}"#;
    assert_eq!(stdout(src), "0\n1\n1\n1\n");
    // The forced value is an ordinary value: members resolve on it.
    assert_eq!(
        stdout(r#"fun main() { val s by lazy { "hi" }; println(s.uppercase()) }"#),
        "HI\n"
    );
    assert_eq!(
        stdout("fun main() { val xs by lazy { listOf(1,2,3) }; println(xs.map { it * 2 }) }"),
        "[2, 4, 6]\n"
    );
}

#[test]
fn by_lazy_on_a_local_still_requires_a_val() {
    // `by lazy` keeps its own lowering — an unforced cell whose thunk runs at
    // the first read — and a `var` has no such shape.
    let out = eval("var x by lazy { 1 }; println(x)");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("`by lazy` requires `val`"),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_local_delegates_to_a_user_getvalue_and_setvalue() {
    // This test used to assert that a local could NOT: "a local has no property
    // object to hand a general `getValue` delegate". It has none still — both
    // arguments are null, a local having no receiver and no `KProperty` this
    // frontend can build — and that turns out to be all the protocol needs.
    // A read is `getValue`, a write is `setValue`, and a compound write is one
    // of each.
    let src = "class Holder { var v: String = \"x\"\n\
               operator fun getValue(t: Any?, p: Any?): String = v\n\
               operator fun setValue(t: Any?, p: Any?, x: String) { v = x } }\n\
               fun main() { var s: String by Holder(); println(s); s = \"y\"; \
               println(s); s += \"z\"; println(s) }";
    assert_eq!(prog(src), "x\ny\nyz\n");
}

#[test]
fn a_local_by_a_delegate_with_no_getvalue_is_rejected() {
    // The half of the old rule that still holds: a delegate whose class the
    // frontend cannot name — and so whose `getValue` it cannot call — fails
    // loudly rather than being silently mis-lowered.
    let out = eval("val x by listOf(1); println(x)");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("operator fun getValue"),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And a class that exists but declares no `getValue` — which would
    // otherwise fail as a missing subroutine, long after the declaration.
    let out = eval("class NoGet(val v: Int)\nfun main() { val x by NoGet(1); println(x) }");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("operator fun getValue"),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_endless_generator_is_bounded_by_take_not_by_materializing_it() {
    // `generateSequence(1) { it * 2 }` never ends, so every one of these
    // terminates only because the pipeline stayed lazy up to the bound.
    assert_eq!(
        stdout("println(generateSequence(1) { it * 2 }.take(5).toList())"),
        "[1, 2, 4, 8, 16]\n"
    );
    assert_eq!(
        stdout("println(generateSequence(1) { it + 1 }.map { it * it }.take(4).toList())"),
        "[1, 4, 9, 16]\n"
    );
    assert_eq!(
        stdout("println(generateSequence(1) { it + 1 }.filter { it % 2 == 0 }.take(3).toList())"),
        "[2, 4, 6]\n"
    );
    // `takeWhile` bounds it too, and `first { }` short-circuits.
    assert_eq!(
        stdout("println(generateSequence(1) { it + 1 }.takeWhile { it < 5 }.toList())"),
        "[1, 2, 3, 4]\n"
    );
    assert_eq!(
        stdout("println(generateSequence(1) { it + 1 }.first { it > 10 })"),
        "11\n"
    );
}

#[test]
fn a_generator_ends_when_its_step_answers_null() {
    // The other way a `generateSequence` terminates: no `take` at all.
    assert_eq!(
        stdout("println(generateSequence(1) { if (it < 20) it * 2 else null }.toList())"),
        "[1, 2, 4, 8, 16, 32]\n"
    );
    // A null SEED is an empty sequence, exactly as a null step ends one.
    assert_eq!(
        stdout("println(generateSequence<Int>(null) { it + 1 }.toList())"),
        "[]\n"
    );
}

#[test]
fn a_sequence_is_re_runnable_and_its_stages_do_not_leak_between_pipelines() {
    // Pulling a pipeline must not consume the base: the stage list is copied
    // into each derived sequence, so `dropWhile`'s progress and `take`'s
    // countdown are per-pull state and not shared.
    let src = r#"
fun main() {
    val seq = generateSequence(1) { it + 1 }
    println(seq.take(3).toList())
    println(seq.take(3).toList())
    println(seq.dropWhile { it < 3 }.take(2).toList())
    println(seq.take(3).toList())
}"#;
    assert_eq!(stdout(src), "[1, 2, 3]\n[1, 2, 3]\n[3, 4]\n[1, 2, 3]\n");
}

#[test]
fn split_to_sequence_matches_split() {
    assert_eq!(
        stdout(r#"println("a,b,c".splitToSequence(",").toList())"#),
        "[a, b, c]\n"
    );
    assert_eq!(
        stdout(r#"println("a,b,c".splitToSequence(",").map { it.uppercase() }.toList())"#),
        "[A, B, C]\n"
    );
}

#[test]
fn named_arguments_reach_the_stdlib_members_that_have_optional_parameters() {
    // Naming a later parameter must not shift an earlier one: each skipped
    // parameter is filled with its OWN default, not with a hole.
    assert_eq!(
        stdout(r#"println(listOf(1,2,3).joinToString(postfix = "!"))"#),
        "1, 2, 3!\n"
    );
    assert_eq!(
        stdout(
            r#"println(listOf(1,2,3).joinToString(separator = "-", prefix = "<", postfix = ">"))"#
        ),
        "<1-2-3>\n"
    );
    assert_eq!(
        stdout("println(listOf(1,2,3,4,5).joinToString(limit = 3))"),
        "1, 2, 3, ...\n"
    );
    // `windowed`'s skipped `step` must default to 1, not to the 0 a null would
    // coerce to — the case a hole-filling rewrite gets wrong.
    assert_eq!(
        stdout("println(listOf(1,2,3,4).windowed(size = 2))"),
        "[[1, 2], [2, 3], [3, 4]]\n"
    );
    // `step` is a soft keyword with its own token, and still a parameter name.
    assert_eq!(
        stdout("println(listOf(1,2,3,4).windowed(size = 2, step = 2))"),
        "[[1, 2], [3, 4]]\n"
    );
    assert_eq!(
        stdout(r#"println("ab".padStart(5, padChar = '.'))"#),
        "...ab\n"
    );
}

#[test]
fn a_named_argument_on_a_user_declaration_still_binds_from_the_declaration() {
    // The stdlib parameter table must not intercept a call a user declaration
    // owns — `copy` and a declared method both bind their own names.
    let src = r#"
data class Cfg(val a: Int, val b: String)
class Box(val n: Int) { fun scaled(by: Int) = n * by }
fun main() {
    println(Cfg(1, "q").copy(b = "r"))
    println(Box(3).scaled(by = 4))
}"#;
    assert_eq!(stdout(src), "Cfg(a=1, b=r)\n12\n");
}

#[test]
fn a_fully_qualified_name_resolves_without_an_import() {
    assert_eq!(stdout("println(kotlin.math.floor(2.7))"), "2.0\n");
    assert_eq!(stdout("println(kotlin.math.abs(-3))"), "3\n");
    assert_eq!(
        stdout(r#"println(String.format("%.2f", 3.14159))"#),
        "3.14\n"
    );
    assert_eq!(stdout(r#"println(Integer.parseInt("-7"))"#), "-7\n");
    // A binding of that name shadows the package, as it does for `Math`.
    assert_eq!(stdout(r#"val String = "s"; println(String.length)"#), "1\n");
}

#[test]
fn the_builder_range_members_slice_their_argument_not_the_receiver() {
    let src = r#"
fun main() {
    val sb = StringBuilder("xy")
    sb.appendRange("hello", 1, 3)
    println(sb)
    sb.insertRange(1, "ABCDE", 0, 2)
    println(sb)
}"#;
    assert_eq!(stdout(src), "xyel\nxAByel\n");
    // The ARGUMENT's bounds, so the length reported is the argument's (2) and
    // not the receiver's (0).
    //
    // This asserted `begin 0, end 9, length 2`, which no JVM produces: JDK 17
    // says `start 0, end 9, length 2` and JDK 21 and 26 both say the below.
    // The old text was written rather than captured. Re-verified against
    // `kotlinc` 2.4.10 on OpenJDK 21.0.12 and 26.0.2.
    let out = eval(r#"StringBuilder().appendRange("ab", 0, 9)"#);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Range [0, 9) out of bounds for length 2"),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── Round 6: what the frontend hardcoded about the reference ────────────────
//
// Every expectation below was READ OFF `kotlinc` 2.4.10 running on OpenJDK
// 21.0.12, not written from the documentation. Where a value is also pinned
// against older JVMs the comment says so; where it is NOT, the point is that no
// JVM ever produced what kotlinrs used to answer.

#[test]
fn unicode_escapes_work_in_string_literals_and_not_only_in_char_ones() {
    // The escape table was written out twice and only the `Char` copy carried
    // `\u` and `\b`, so `"A"` lexed as the five characters `u0041` while
    // `'A'` lexed as `A`. Both spellings now read one table.
    assert_eq!(stdout(r#"println("A")"#), "A\n");
    assert_eq!(stdout(r#"println('A')"#), "A\n");
    assert_eq!(stdout(r#"println("\u00a0".length)"#), "1\n");
    assert_eq!(stdout(r#"println("abc")"#), "abc\n");
    // `\b` and the escaped quote that only the other table used to accept.
    assert_eq!(stdout(r#"println("x\by".length)"#), "3\n");
    assert_eq!(stdout(r#"println("\'".length)"#), "1\n");
    assert_eq!(stdout(r#"println('\"'.code)"#), "34\n");
}

#[test]
fn negative_array_size_names_the_length_it_was_asked_for() {
    // `java.lang.NegativeArraySizeException: -3`, on every JVM measured (17,
    // 21, 26). The bare form kotlinrs used to raise is not an older dialect —
    // no JVM produces it.
    for builder in [
        "IntArray(n)",
        "DoubleArray(n)",
        "CharArray(n)",
        "BooleanArray(n)",
    ] {
        let src = format!(
            "fun main() {{ val n = -3; try {{ {builder} }} \
             catch (e: Throwable) {{ println(e) }} }}"
        );
        assert_eq!(
            prog(&src),
            "java.lang.NegativeArraySizeException: -3\n",
            "for {builder}"
        );
    }
    assert_eq!(
        prog(
            "fun main() { val n = -3; try { Array(n) { 0 } } \
             catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.NegativeArraySizeException: -3\n"
    );
    // The detail message is the bare number, not a sentence.
    assert_eq!(
        prog(
            "fun main() { val n = -3; try { IntArray(n) } \
             catch (e: NegativeArraySizeException) { println(e.message) } }"
        ),
        "-3\n"
    );
}

#[test]
fn unknown_format_conversion_always_names_a_character() {
    // Two ways to reach this fault and neither is bare. A complete spec names
    // its conversion; a spec that runs off the end names the character right
    // after the `%`, or `%` itself when the string ends there.
    let probe = |fmt: &str| {
        prog(&format!(
            "fun main() {{ try {{ println(String.format({fmt})) }} \
             catch (e: Throwable) {{ println(e) }} }}"
        ))
    };
    let unknown =
        |c: char| format!("java.util.UnknownFormatConversionException: Conversion = '{c}'\n");
    assert_eq!(probe(r#""%q", 1"#), unknown('q'));
    assert_eq!(probe(r#""%5q", 1"#), unknown('q'));
    assert_eq!(probe(r#""%-5.2q", 1"#), unknown('q'));
    assert_eq!(probe(r#""%""#), unknown('%'));
    assert_eq!(probe(r#""abc%""#), unknown('%'));
    assert_eq!(probe(r#""%5""#), unknown('5'));
    assert_eq!(probe(r#""%-""#), unknown('-'));
    assert_eq!(probe(r#""%.2""#), unknown('.'));
    assert_eq!(probe(r#""%5.2""#), unknown('5'));
    // `%%` and `%n` take no argument but DO take the width and the `-` flag.
    assert_eq!(probe(r#""%5%""#), "    %\n");
    assert_eq!(probe(r#""%-5%""#), "%    \n");
    assert_eq!(probe(r#""100%%""#), "100%\n");
    assert_eq!(probe(r#""a%nb""#), "a\nb\n");
}

#[test]
fn a_primitive_array_renders_its_primitive_descriptor() {
    // `intArrayOf(1, 2)` is an `int[]`. Its `Object.toString` is `[I@…`, and
    // `[Ljava.lang.Integer;@…` — what inferring the descriptor from the boxed
    // element values produced — is what an `Array<Int>` renders, not this.
    let desc = |expr: &str| {
        prog(&format!(
            "fun main() {{ println({expr}.toString().substringBefore(\"@\")) }}"
        ))
    };
    assert_eq!(desc("intArrayOf(1, 2)"), "[I\n");
    assert_eq!(desc("doubleArrayOf(1.0)"), "[D\n");
    assert_eq!(desc("booleanArrayOf(true)"), "[Z\n");
    assert_eq!(desc("charArrayOf('a')"), "[C\n");
    assert_eq!(desc("IntArray(2)"), "[I\n");
    assert_eq!(desc("CharArray(2)"), "[C\n");
    assert_eq!(desc("\"ab\".toCharArray()"), "[C\n");
    // `arrayOf` really is the boxed one, and `Array(n) { … }` infers from what
    // the lambda produced — so this is not "always primitive now".
    assert_eq!(desc("arrayOf(1, 2)"), "[Ljava.lang.Integer;\n");
    assert_eq!(desc("arrayOf(\"a\")"), "[Ljava.lang.String;\n");
    assert_eq!(desc("Array(1) { 0 }"), "[Ljava.lang.Integer;\n");
    // A `CharArray` slot defaults to the NUL character, not the integer 0.
    assert_eq!(prog("fun main() { println(CharArray(2)[0].code) }"), "0\n");
}

#[test]
fn a_failed_cast_names_the_receivers_real_jvm_class() {
    // `class List cannot be cast to class String` is a sentence no JVM writes.
    // The class a container handle really is depends on how it was built, and
    // that provenance is already recorded for the index diagnostics.
    let cce = |expr: &str| {
        prog(&format!(
            "fun main() {{ try {{ val a: Any = {expr}; a as String }} \
             catch (e: Throwable) {{ println(e) }} }}"
        ))
    };
    let base = |c: &str| {
        format!(
            "java.lang.ClassCastException: class {c} cannot be cast to class java.lang.String \
             ({c} and java.lang.String are in module java.base of loader 'bootstrap')\n"
        )
    };
    let app = |c: &str| {
        format!(
            "java.lang.ClassCastException: class {c} cannot be cast to class java.lang.String \
             ({c} is in unnamed module of loader 'app'; java.lang.String is in module \
             java.base of loader 'bootstrap')\n"
        )
    };
    assert_eq!(cce("listOf<Int>()"), app("kotlin.collections.EmptyList"));
    assert_eq!(
        cce("listOf(1)"),
        base("java.util.Collections$SingletonList")
    );
    assert_eq!(cce("listOf(1, 2)"), base("java.util.Arrays$ArrayList"));
    assert_eq!(cce("mutableListOf(1, 2)"), base("java.util.ArrayList"));
    assert_eq!(cce("listOf(1, 2).toList()"), base("java.util.ArrayList"));
    assert_eq!(
        cce("listOf(2, 1).sorted()"),
        base("java.util.Arrays$ArrayList")
    );
    assert_eq!(cce("intArrayOf(1)"), base("[I"));
    assert_eq!(cce("arrayOf(1)"), base("[Ljava.lang.Integer;"));
    assert_eq!(cce("1 to 2"), app("kotlin.Pair"));
    assert_eq!(cce("Triple(1, 2, 3)"), app("kotlin.Triple"));
    assert_eq!(cce("1..3"), app("kotlin.ranges.IntRange"));
    assert_eq!(cce("5 downTo 1"), app("kotlin.ranges.IntProgression"));
    assert_eq!(cce("'a'..'c'"), app("kotlin.ranges.CharRange"));
}

#[test]
fn double_ordering_is_the_total_order_and_not_ieee_comparison() {
    // `partial_cmp(…).unwrap_or(Equal)` scores every NaN comparison as a tie
    // and cannot separate the signed zeros. `Double.compare` is a TOTAL order:
    // NaN above everything, `-0.0` below `0.0`.
    let p = |e: &str| prog(&format!("fun main() {{ println({e}) }}"));
    assert_eq!(p("Double.NaN.compareTo(1.0)"), "1\n");
    assert_eq!(p("1.0.compareTo(Double.NaN)"), "-1\n");
    assert_eq!(p("Double.NaN.compareTo(Double.NaN)"), "0\n");
    assert_eq!(p("0.0.compareTo(-0.0)"), "1\n");
    assert_eq!(p("(-0.0).compareTo(0.0)"), "-1\n");
    assert_eq!(
        p("listOf(1.0, Double.NaN, 0.0, -0.0).sorted()"),
        "[-0.0, 0.0, 1.0, NaN]\n"
    );
    assert_eq!(
        p("listOf(Double.NaN, 1.0).sortedDescending()"),
        "[NaN, 1.0]\n"
    );
    // …while `==` on two `Double`-typed operands stays IEEE, because Kotlin
    // compiles that to the primitive comparison and not to `equals`.
    assert_eq!(p("Double.NaN == Double.NaN"), "false\n");
    assert_eq!(p("0.0 == -0.0"), "true\n");
    // `min`/`max` over a floating receiver take the `Math` overload, which
    // PROPAGATES NaN rather than ordering it — so `min` disagrees with the
    // comparator even though `max` happens to agree with it.
    assert_eq!(p("listOf(1.0, Double.NaN, 2.0).max()"), "NaN\n");
    assert_eq!(p("listOf(1.0, Double.NaN, 2.0).min()"), "NaN\n");
    assert_eq!(p("minOf(Double.NaN, 1.0)"), "NaN\n");
    assert_eq!(p("maxOf(1.0, Double.NaN)"), "NaN\n");
    assert_eq!(p("Math.max(0.0, -0.0)"), "0.0\n");
    assert_eq!(p("Math.min(0.0, -0.0)"), "-0.0\n");
    // A `Long` past the 53-bit mantissa must not compare as a `Double`.
    assert_eq!(p("9007199254740993L.compareTo(9007199254740992L)"), "1\n");
    assert_eq!(
        p("listOf(9007199254740993L, 9007199254740992L).sorted()"),
        "[9007199254740992, 9007199254740993]\n"
    );
}

#[test]
fn collection_membership_uses_boxed_equality_not_ieee() {
    // `Double.equals` compares `doubleToLongBits`, so it disagrees with `==` on
    // exactly two values — and every membership test goes through it.
    let p = |e: &str| prog(&format!("fun main() {{ println({e}) }}"));
    assert_eq!(p("Double.NaN.equals(Double.NaN)"), "true\n");
    assert_eq!(p("0.0.equals(-0.0)"), "false\n");
    assert_eq!(p("listOf(Double.NaN).contains(Double.NaN)"), "true\n");
    assert_eq!(p("listOf(0.0).contains(-0.0)"), "false\n");
    assert_eq!(p("listOf(-0.0).indexOf(0.0)"), "-1\n");
    assert_eq!(p("listOf(Double.NaN, Double.NaN).distinct().size"), "1\n");
    assert_eq!(p("listOf(0.0, -0.0).distinct().size"), "2\n");
    assert_eq!(p("setOf(Double.NaN, Double.NaN).size"), "1\n");
    assert_eq!(p("setOf(0.0, -0.0).size"), "2\n");
    assert_eq!(p("mapOf(0.0 to 1, -0.0 to 2).size"), "2\n");
    assert_eq!(p("listOf(Double.NaN) == listOf(Double.NaN)"), "true\n");
    assert_eq!(p("listOf(0.0) == listOf(-0.0)"), "false\n");
}

#[test]
fn math_round_is_the_jdk_algorithm_and_not_floor_of_x_plus_half() {
    // `floor(x + 0.5)` is the documentation, not the implementation. The
    // addition itself rounds `0.49999999999999994` up to `1.0`, so the floor
    // form answers 1 where every JVM since the JDK 8 fix answers 0.
    // `roundToInt` and `roundToLong` are `kotlin.math` EXTENSIONS on `Double`,
    // not members of it, so the import is part of the program: without it the
    // reference toolchain answers `unresolved reference 'roundToInt'`.
    let p = |e: &str| {
        prog(&format!(
            "import kotlin.math.roundToInt\nfun main() {{ println({e}) }}"
        ))
    };
    assert_eq!(p("Math.round(0.49999999999999994)"), "0\n");
    assert_eq!(p("0.49999999999999994.roundToInt()"), "0\n");
    assert_eq!(p("Math.round(0.5)"), "1\n");
    assert_eq!(p("Math.round(-0.5)"), "0\n");
    assert_eq!(p("Math.round(-1.5)"), "-1\n");
    assert_eq!(p("Math.round(2.5)"), "3\n");
    assert_eq!(p("Math.round(4503599627370495.5)"), "4503599627370496\n");
    // `kotlin.math.round` is a DIFFERENT function — half to even, `Double` out.
    assert_eq!(p("kotlin.math.round(2.5)"), "2.0\n");
    assert_eq!(p("kotlin.math.round(-0.5)"), "-0.0\n");
    // Kotlin's two wrappers add guards `Math.round` does not have: NaN is
    // refused rather than answered as 0, and `roundToInt` clamps to `Int`.
    assert_eq!(p("Math.round(Double.NaN)"), "0\n");
    assert_eq!(p("Double.POSITIVE_INFINITY.roundToInt()"), "2147483647\n");
    assert_eq!(p("Double.NEGATIVE_INFINITY.roundToInt()"), "-2147483648\n");
    for m in ["roundToInt", "roundToLong"] {
        assert_eq!(
            prog(&format!(
                "import kotlin.math.{m}\nfun main() {{ try {{ println(Double.NaN.{m}()) }} \
                 catch (e: Throwable) {{ println(e) }} }}"
            )),
            "java.lang.IllegalArgumentException: Cannot round NaN value.\n",
            "for {m}"
        );
    }
}

#[test]
fn coerce_in_refuses_a_crossed_pair_and_clamps_with_kotlins_comparisons() {
    let p = |e: &str| prog(&format!("fun main() {{ println({e}) }}"));
    // `f64::max` then `f64::min` quietly answered 1 here.
    assert_eq!(
        prog(
            "fun main() { try { println(5.coerceIn(3, 1)) } \
             catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.IllegalArgumentException: Cannot coerce value to an empty range: \
         maximum 1 is less than minimum 3.\n"
    );
    // Kotlin clamps with `<`/`>` and hands the receiver back when neither
    // fires, so a NaN receiver survives and `-0.0` is not below `0.0`.
    assert_eq!(p("Double.NaN.coerceIn(1.0, 3.0)"), "NaN\n");
    assert_eq!(p("(-0.0).coerceIn(0.0, 1.0)"), "-0.0\n");
    assert_eq!(p("0.0.coerceIn(-0.0, 1.0)"), "0.0\n");
    assert_eq!(p("5.coerceIn(1, 3)"), "3\n");
    assert_eq!(p("5.coerceAtMost(3)"), "3\n");
    assert_eq!(p("5.coerceAtLeast(9)"), "9\n");
}

#[test]
fn whitespace_is_javas_set_and_not_unicodes() {
    // Kotlin's predicate is `Character.isWhitespace || Character.isSpaceChar`,
    // which differs from Unicode's `White_Space` — Rust's set — at both ends.
    // Written as `\u` escapes so this file stays ASCII and no invisible
    // character can drift.
    let p = |e: &str| prog(&format!("fun main() {{ println({e}) }}"));
    // Java-only: the C0 file/group/record/unit separators.
    assert_eq!(p(r#"'\u001c'.isWhitespace()"#), "true\n");
    assert_eq!(p(r#"'\u001f'.isWhitespace()"#), "true\n");
    // Unicode-only: NEL. Rust says true, Kotlin says false.
    assert_eq!(p(r#"'\u0085'.isWhitespace()"#), "false\n");
    // The no-break spaces, which `isWhitespace` alone would drop and the union
    // keeps.
    assert_eq!(p(r#"'\u00a0'.isWhitespace()"#), "true\n");
    assert_eq!(p(r#"'\u202f'.isWhitespace()"#), "true\n");
    assert_eq!(p(r#"'\u3000'.isWhitespace()"#), "true\n");
    // Not a separator at all: zero-width space is a format character.
    assert_eq!(p(r#"'\u200b'.isWhitespace()"#), "false\n");
    // `trim` / `isBlank` key on the same predicate, so they inherit both edges.
    assert_eq!(p(r#""\u00a0x\u00a0".trim().length"#), "1\n");
    assert_eq!(p(r#""\u001cx".trim().length"#), "1\n");
    assert_eq!(p(r#""\u1680x".trim().length"#), "1\n");
    assert_eq!(p(r#""\u0085x".trim().length"#), "2\n");
    assert_eq!(p(r#""\u200bx".trim().length"#), "2\n");
    assert_eq!(p(r#""\u00a0".isBlank()"#), "true\n");
    assert_eq!(p(r#""\u200b".isBlank()"#), "false\n");
}

#[test]
fn parse_double_follows_javas_grammar_and_not_rusts() {
    let p = |e: &str| prog(&format!("fun main() {{ println({e}) }}"));
    // Java accepts a type suffix and hexadecimal floating point; Rust rejects
    // both.
    assert_eq!(p(r#""1.5d".toDouble()"#), "1.5\n");
    assert_eq!(p(r#""1.5F".toDouble()"#), "1.5\n");
    assert_eq!(p(r#""0x1p3".toDouble()"#), "8.0\n");
    assert_eq!(p(r#""0X1.8p1".toDouble()"#), "3.0\n");
    // …exactly one suffix, and the `p` exponent is mandatory.
    assert_eq!(p(r#""1.5dd".toDoubleOrNull()"#), "null\n");
    assert_eq!(p(r#""0x1".toDoubleOrNull()"#), "null\n");
    // Rust accepts short infinity spellings that Java's grammar does not.
    assert_eq!(p(r#""inf".toDoubleOrNull()"#), "null\n");
    assert_eq!(p(r#""infinity".toDoubleOrNull()"#), "null\n");
    assert_eq!(p(r#""nan".toDoubleOrNull()"#), "null\n");
    assert_eq!(p(r#""Infinity".toDouble()"#), "Infinity\n");
    assert_eq!(p(r#""-Infinity".toDouble()"#), "-Infinity\n");
    assert_eq!(p(r#""NaN".toDouble()"#), "NaN\n");
    // An empty (or all-whitespace) input has its OWN message here, unlike every
    // other numeric parse.
    for input in [r#""""#, r#""   ""#] {
        assert_eq!(
            prog(&format!(
                "fun main() {{ try {{ println({input}.toDouble()) }} \
                 catch (e: Throwable) {{ println(e) }} }}"
            )),
            "java.lang.NumberFormatException: empty String\n",
            "for {input}"
        );
    }
    assert_eq!(
        prog(r#"fun main() { try { println("".toInt()) } catch (e: Throwable) { println(e) } }"#),
        "java.lang.NumberFormatException: For input string: \"\"\n"
    );
}

#[test]
fn floor_division_and_sign_follow_the_divisor_not_the_dividend() {
    // `/` and `%` truncate toward zero; `floorDiv`/`floorMod`/`mod` round
    // toward negative infinity, which flips the remainder's sign.
    //
    // `Int.sign` is an EXTENSION in `kotlin.math`, not a member of `Int`, so
    // the import is part of the program rather than decoration: without it the
    // reference toolchain answers `unresolved reference 'sign'`.
    let p = |e: &str| {
        prog(&format!(
            "import kotlin.math.sign\nfun main() {{ println({e}) }}"
        ))
    };
    assert_eq!(p("-7 / 2"), "-3\n");
    assert_eq!(p("-7 % 2"), "-1\n");
    assert_eq!(p("Math.floorDiv(-7, 2)"), "-4\n");
    assert_eq!(p("Math.floorMod(-7, 2)"), "1\n");
    assert_eq!(p("(-7).mod(2)"), "1\n");
    assert_eq!(p("7 / -2"), "-3\n");
    assert_eq!(p("7 % -2"), "1\n");
    assert_eq!(p("Math.floorDiv(7, -2)"), "-4\n");
    assert_eq!(p("Math.floorMod(7, -2)"), "-1\n");
    // `sign` keeps the sign of a zero and answers NaN for NaN, where Rust's
    // `signum` answers ±1.0 and 1.0 respectively.
    assert_eq!(p("kotlin.math.sign(0.0)"), "0.0\n");
    assert_eq!(p("kotlin.math.sign(-0.0)"), "-0.0\n");
    assert_eq!(p("kotlin.math.sign(Double.NaN)"), "NaN\n");
    assert_eq!(p("kotlin.math.sign(-3.5)"), "-1.0\n");
    assert_eq!(p("Math.signum(-0.0)"), "-0.0\n");
    assert_eq!(p("0.sign"), "0\n");
    assert_eq!(p("(-7).sign"), "-1\n");
}

#[test]
fn build_list_and_the_one_argument_list_of_not_null_have_their_own_classes() {
    // Two rows the provenance table was missing, both re-judged this round.
    //
    // `buildList { … }` hands back a `kotlin.collections.builders.ListBuilder`,
    // which collapses at NO size and words its bounds fault lowercase and
    // comma-separated — `index: 9, size: 2` — where every other `List` says
    // `Index 9 out of bounds for length 2`.
    let idx = |expr: &str| {
        prog(&format!(
            "fun main() {{ val i = 9; try {{ {expr}[i] }} \
             catch (e: Throwable) {{ println(e) }} }}"
        ))
    };
    for n in 0..3 {
        assert_eq!(
            idx(&format!("buildList {{ for (k in 1..{n}) add(k) }}")),
            format!("java.lang.IndexOutOfBoundsException: index: 9, size: {n}\n"),
            "for buildList of {n}"
        );
    }
    // `listOfNotNull` declares TWO overloads that hand back different classes.
    // The ONE-argument one is `if (element != null) listOf(element) else
    // emptyList()`, so it collapses like a literal…
    assert_eq!(
        idx("listOfNotNull(1)"),
        "java.lang.IndexOutOfBoundsException: Index: 9, Size: 1\n"
    );
    assert_eq!(
        idx("listOfNotNull<Int>(null)"),
        "java.lang.IndexOutOfBoundsException: Empty list doesn't contain element at index 9.\n"
    );
    // …while the VARARG one is `filterNotNull()`, a plain `ArrayList`, at every
    // size including zero. Arity decides, not the surviving element count.
    assert_eq!(
        idx("listOfNotNull<Int>(null, null)"),
        "java.lang.IndexOutOfBoundsException: Index 9 out of bounds for length 0\n"
    );
    assert_eq!(
        idx("listOfNotNull(1, null)"),
        "java.lang.IndexOutOfBoundsException: Index 9 out of bounds for length 1\n"
    );
    assert_eq!(
        idx("listOfNotNull(1, null, 2)"),
        "java.lang.IndexOutOfBoundsException: Index 9 out of bounds for length 2\n"
    );
    // The values themselves are unchanged by the tagging.
    assert_eq!(
        prog("fun main() { println(buildList { add(1); add(2) }); println(listOfNotNull(1)) }"),
        "[1, 2]\n[1]\n"
    );
}

#[test]
fn the_step_method_spelling_is_the_same_call_as_the_step_infix() {
    // Kotlin declares ONE `IntProgression.step`; `1..5 step 2` and
    // `(1..5).step(2)` are the same call. Only the infix form reached the
    // parser's `Expr::Step`, so the method form answered `unresolved
    // reference: step on IntRange`. It rewrites to the same node now, which is
    // what keeps the non-positive-step fault identical between them.
    let src = r#"
class Ladder { fun step(n: Int) = "user step " + n }
fun main() {
    println(1..5 step 2)
    println((1..5).step(2))
    println((5 downTo 1).step(2))
    println(('a'..'g').step(3))
    val r = 1..9
    println(r.step(4))
    println(Ladder().step(7))
    println((1..5).step(2).toList())
}"#;
    assert_eq!(
        prog(src),
        "1..5 step 2\n1..5 step 2\n5 downTo 1 step 2\na..g step 3\n1..9 step 4\n\
         user step 7\n[1, 3, 5]\n"
    );
    // A user class declaring its OWN `step` is not hijacked by the rewrite —
    // the line above proves that — and a non-positive step still faults.
    assert_eq!(
        prog(
            "fun main() { val n = 0; try { (1..5).step(n) } catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.IllegalArgumentException: Step must be positive, was: 0.\n"
    );
}

// ─── Round 7: a fault the program cannot catch is not a fault Kotlin has ──
//
// Every expected string below was measured on kotlinc 2.4.10 running on JRE
// 21.0.12, with the program COMPILED by that same JVM and RUN under
// `java -cp out:kotlin-stdlib.jar` (not the `kotlin` launcher, whose
// URLClassLoader changes how a `ClassCastException` words itself). Both axes
// matter and both were pinned.

/// Runaway recursion through a lambda, a `toString`, or an `equals` used to
/// exhaust the RUST stack — `fatal runtime error: stack overflow, aborting`,
/// SIGABRT, exit 134. That is not a Kotlin behaviour at any depth: the JVM
/// raises `StackOverflowError`, which a program can catch and recover from.
/// Reproduced at depth 100 before the fix; every shape below aborted.
#[test]
fn runaway_recursion_raises_a_catchable_stack_overflow_error() {
    // Through a lambda-taking collection member — a re-entrant `vm.run()` per
    // level, which is what the Rust stack was paying for.
    assert_eq!(
        prog(
            "fun f(n: Int): Int = if (n == 0) 0 else listOf(n).map { f(it - 1) }[0]\n\
             fun main() { try { println(f(200000)) } catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.StackOverflowError\n"
    );
    // Through a user `toString()` that walks a cycle.
    assert_eq!(
        prog(
            "class N { var next: N? = null\n\
             \x20   override fun toString(): String = \"N(\" + next + \")\" }\n\
             fun main() { val a = N(); a.next = a\n\
             \x20   try { println(a) } catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.StackOverflowError\n"
    );
    // Through a data class's GENERATED toString, which has no self-check.
    assert_eq!(
        prog(
            "data class D(var next: Any?)\n\
             fun main() { val d = D(null); d.next = d\n\
             \x20   try { println(d) } catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.StackOverflowError\n"
    );
    // Through a user `equals`.
    assert_eq!(
        prog(
            "class E { var o: E? = null\n\
             \x20   override fun equals(other: Any?): Boolean = o == (other as E).o }\n\
             fun main() { val a = E(); a.o = a\n\
             \x20   try { println(a == a) } catch (e: Throwable) { println(e) } }"
        ),
        "java.lang.StackOverflowError\n"
    );
}

/// `StackOverflowError` is an `Error` under `VirtualMachineError`, so
/// `catch (e: Exception)` must NOT claim it — the property that separates a
/// throwable placed in the hierarchy from one that merely has the right name.
/// A class absent from `THROWABLE_PARENTS` descends straight from `Throwable`,
/// which answers this pair the same way by accident; the uncaught line below is
/// what distinguishes them, and the `Error` arm is what pins the placement.
#[test]
fn stack_overflow_error_sits_under_error_and_not_under_exception() {
    let recurse = "fun f(n: Int): Int = if (n == 0) 0 else listOf(n).map { f(it - 1) }[0]\n";
    assert_eq!(
        prog(&format!(
            "{recurse}fun main() {{ try {{ println(f(200000)) }} \
             catch (e: Exception) {{ println(\"Exception\") }} \
             catch (e: Throwable) {{ println(\"not an Exception\") }} }}"
        )),
        "not an Exception\n"
    );
    assert_eq!(
        prog(&format!(
            "{recurse}fun main() {{ try {{ println(f(200000)) }} \
             catch (e: Error) {{ println(\"Error\") }} \
             catch (e: Throwable) {{ println(\"not an Error\") }} }}"
        )),
        "Error\n"
    );
    // Uncaught, it is the JVM's own report line — messageless, so no colon.
    assert_eq!(
        uncaught_line(&format!("{recurse}fun main() {{ println(f(200000)) }}")),
        "Exception in thread \"main\" java.lang.StackOverflowError"
    );
}

/// `AbstractCollection.toString` checks each element against `this` and writes
/// `(this Collection)`; `AbstractMap` writes `(this Map)`. The check is
/// reference identity against the IMMEDIATE receiver and nothing more, so a
/// two-step cycle still overflows — which is why both halves are pinned here.
/// Every one of these eight aborted with SIGABRT before the fix.
#[test]
fn a_container_holding_itself_renders_the_jvm_placeholder() {
    assert_eq!(
        prog("fun main() { val xs = mutableListOf<Any>(); xs.add(xs); println(xs) }"),
        "[(this Collection)]\n"
    );
    assert_eq!(
        prog(
            "fun main() { val xs = mutableListOf<Any>()\n\
             \x20   xs.add(1); xs.add(xs); xs.add(2); println(xs) }"
        ),
        "[1, (this Collection), 2]\n"
    );
    assert_eq!(
        prog("fun main() { val s = mutableSetOf<Any>(); s.add(s); println(s) }"),
        "[(this Collection)]\n"
    );
    assert_eq!(
        prog("fun main() { val m = mutableMapOf<String, Any>(); m[\"k\"] = m; println(m) }"),
        "{k=(this Map)}\n"
    );
    assert_eq!(
        prog("fun main() { val m = mutableMapOf<Any, Int>(); m[m] = 1; println(m) }"),
        "{(this Map)=1}\n"
    );
    // INDIRECT cycles get no placeholder — the JVM has no cycle detection, only
    // that one identity check — so they raise, and raise catchably.
    for src in [
        "fun main() { val a = mutableListOf<Any>(); val b = mutableListOf<Any>()\n\
         \x20   a.add(b); b.add(a)\n\
         \x20   try { println(a) } catch (e: Throwable) { println(e) } }",
        "fun main() { val xs = mutableListOf<Any>(); val ys = mutableListOf<Any>(xs)\n\
         \x20   xs.add(ys)\n\
         \x20   try { println(ys) } catch (e: Throwable) { println(e) } }",
        "fun main() { val xs = mutableListOf<Any>(); xs.add(listOf(xs))\n\
         \x20   try { println(xs) } catch (e: Throwable) { println(e) } }",
    ] {
        assert_eq!(prog(src), "java.lang.StackOverflowError\n", "for {src:?}");
    }
}

/// An exhausted argument list used to become `Undef`, which every numeric
/// conversion read as `0` — so `"%d %d".format(1)` answered `1 0`, a wrong
/// answer with no diagnostic anywhere. The JVM raises, and it quotes the
/// specifier AS WRITTEN, flags and width and precision included.
#[test]
fn a_specifier_with_no_argument_left_raises_and_quotes_itself() {
    for (src, want) in [
        (
            r#"fun main() { try { println("%d %d".format(1)) } catch (e: Throwable) { println(e) } }"#,
            "java.util.MissingFormatArgumentException: Format specifier '%d'\n",
        ),
        (
            r#"fun main() { try { println("%s".format()) } catch (e: Throwable) { println(e) } }"#,
            "java.util.MissingFormatArgumentException: Format specifier '%s'\n",
        ),
        (
            r#"fun main() { try { println("%-5.2f".format()) } catch (e: Throwable) { println(e) } }"#,
            "java.util.MissingFormatArgumentException: Format specifier '%-5.2f'\n",
        ),
        (
            r#"fun main() { try { println("%,d".format()) } catch (e: Throwable) { println(e) } }"#,
            "java.util.MissingFormatArgumentException: Format specifier '%,d'\n",
        ),
        (
            r#"fun main() { try { println("a%sb%dc".format("x")) } catch (e: Throwable) { println(e) } }"#,
            "java.util.MissingFormatArgumentException: Format specifier '%d'\n",
        ),
    ] {
        assert_eq!(prog(src), want, "for {src:?}");
    }
    // EXTRA arguments are ignored, not a fault.
    assert_eq!(
        prog(r#"fun main() { println("%d".format(1, 2, 3)) }"#),
        "1\n"
    );
    // `%%` and `%n` consume no argument, so an empty list is fine.
    assert_eq!(
        prog(r#"fun main() { println("a%%b%nc".format().length) }"#),
        "5\n"
    );
}

/// A conversion is TYPED. `%d` with a `String` used to coerce through `to_int()`
/// and print `0`; `%f` with an `Int` printed `1.000000` where the JVM refuses
/// the pair. The refusal names the conversion character and the argument's
/// boxed class.
#[test]
fn a_conversion_refuses_an_argument_of_the_wrong_class() {
    for (src, want) in [
        (r#""%d".format("x")"#, "d != java.lang.String"),
        (r#""%d".format(true)"#, "d != java.lang.Boolean"),
        (r#""%d".format(1.5)"#, "d != java.lang.Double"),
        (r#""%d".format('a')"#, "d != java.lang.Character"),
        (r#""%f".format("x")"#, "f != java.lang.String"),
        (r#""%f".format(1)"#, "f != java.lang.Integer"),
        (r#""%x".format("x")"#, "x != java.lang.String"),
        (r#""%o".format(true)"#, "o != java.lang.Boolean"),
        (r#""%e".format(1)"#, "e != java.lang.Integer"),
        (r#""%c".format("x")"#, "c != java.lang.String"),
        (r#""%c".format(1.0)"#, "c != java.lang.Double"),
    ] {
        assert_eq!(
            prog(&format!(
                "fun main() {{ try {{ println({src}) }} catch (e: Throwable) {{ println(e) }} }}"
            )),
            format!("java.util.IllegalFormatConversionException: {want}\n"),
            "for {src}"
        );
    }
    // The pairs the JVM ACCEPTS must keep working — the check must not have
    // become a blanket refusal.
    for (src, want) in [
        (r#""%c".format(65)"#, "A"),
        (r#""%c".format('z')"#, "z"),
        (r#""%s".format(1)"#, "1"),
        (r#""%s".format(listOf(1, 2))"#, "[1, 2]"),
        (r#""%d".format(255)"#, "255"),
        (r#""%x".format(255)"#, "ff"),
        (r#""%f".format(5.0)"#, "5.000000"),
    ] {
        assert_eq!(
            prog(&format!("fun main() {{ println({src}) }}")),
            format!("{want}\n")
        );
    }
}

/// The Theme-C property a message audit cannot see: every `java.util.Formatter`
/// fault descends from `IllegalFormatException`, which descends from
/// `IllegalArgumentException`. kotlinrs produced the right MESSAGES for these
/// before it had the right CLASSES, so a `catch (e: IllegalArgumentException)`
/// — which the JVM answers YES to — saw nothing at all and the throwable fell
/// out to `Throwable`.
#[test]
fn a_format_fault_is_catchable_where_the_jvm_catches_it() {
    for fault in [
        r#""%q".format(1)"#,              // UnknownFormatConversionException
        r#""%d".format("x")"#,            // IllegalFormatConversionException
        r#""%d %d".format(1)"#,           // MissingFormatArgumentException
        r#""%,s".format("a")"#,           // FormatFlagsConversionMismatchException
        r#""%2147483648d".format(1)"#,    // IllegalFormatWidthException
        r#""%.2147483648f".format(1.0)"#, // IllegalFormatPrecisionException
    ] {
        for level in ["IllegalArgumentException", "RuntimeException", "Exception"] {
            assert_eq!(
                prog(&format!(
                    "fun main() {{ try {{ println({fault}) }} \
                     catch (e: {level}) {{ println(\"caught\") }} \
                     catch (e: Throwable) {{ println(\"escaped\") }} }}"
                )),
                "caught\n",
                "{fault} should be caught by {level}"
            );
        }
        // And it is NOT an `Error`.
        assert_eq!(
            prog(&format!(
                "fun main() {{ try {{ println({fault}) }} \
                 catch (e: Error) {{ println(\"Error\") }} \
                 catch (e: Throwable) {{ println(\"not an Error\") }} }}"
            )),
            "not an Error\n",
            "{fault} is not an Error"
        );
    }
}

/// A width or precision that does not fit an `int`. kotlinrs parsed the digits
/// into a `usize` and fell back to `unwrap_or(0)`, so an overflowing width was
/// silently NO width — and the values that fit a `usize` but not an `int`
/// (`%4294967296d`) tried to pad to four billion characters and hung. Java
/// parses into an `int` and rejects the negative, so every overflowing spelling
/// lands on the same number.
#[test]
fn a_width_or_precision_that_overflows_an_int_is_a_fault() {
    for spec in [
        "%2147483648d",
        "%10000000000d",
        "%4294967296d",
        "%99999999999999999999d",
    ] {
        assert_eq!(
            prog(&format!(
                "fun main() {{ try {{ println(\"{spec}\".format(1)) }} \
                 catch (e: Throwable) {{ println(e) }} }}"
            )),
            "java.util.IllegalFormatWidthException: -2147483648\n",
            "for {spec}"
        );
    }
    for spec in ["%.2147483648f", "%.99999999999999999999f"] {
        assert_eq!(
            prog(&format!(
                "fun main() {{ try {{ println(\"{spec}\".format(1.0)) }} \
                 catch (e: Throwable) {{ println(e) }} }}"
            )),
            "java.util.IllegalFormatPrecisionException: -2147483648\n",
            "for {spec}"
        );
    }
    // A width that DOES fit still pads.
    assert_eq!(
        prog(r#"fun main() { println("[%5d]".format(1)) }"#),
        "[    1]\n"
    );
}

/// `%b` is a NULL TEST, not a truth test: `false` only for `null` and for a
/// `Boolean false`. kotlinrs answered Kotlin's notion of truth, so
/// `"%b".format("x")` and `"%b".format(1)` both read `false` where the JVM
/// reads `true` — a silently wrong answer on the happy path.
#[test]
fn percent_b_is_a_null_test_and_not_a_truth_test() {
    let src = r#"fun main() {
    val n: Any? = null
    println("%b".format(n))
    println("%b".format(false))
    println("%b".format(true))
    println("%b".format("x"))
    println("%b".format(""))
    println("%b".format(1))
    println("%b".format(0))
    println("%b".format(listOf<Int>()))
}"#;
    assert_eq!(
        prog(src),
        "false\nfalse\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n"
    );
}

/// A null argument renders as the four characters `null` under every conversion
/// but `%b`, and the width and `-` flag still apply. kotlinrs coerced it
/// through the conversion instead, printing `0` for `%d`, `0.000000` for `%f`
/// and a NUL character for `%c`.
#[test]
fn a_null_argument_renders_as_null_under_every_conversion_but_b() {
    let src = r#"fun main() {
    val n: Any? = null
    println("[" + "%d".format(n) + "]")
    println("[" + "%x".format(n) + "]")
    println("[" + "%f".format(n) + "]")
    println("[" + "%c".format(n) + "]")
    println("[" + "%s".format(n) + "]")
    println("[" + "%5d".format(n) + "]")
    println("[" + "%-6s".format(n) + "]")
}"#;
    assert_eq!(
        prog(src),
        "[null]\n[null]\n[null]\n[null]\n[null]\n[ null]\n[null  ]\n"
    );
}

#[test]
fn a_lazy_sequence_is_evaluated_element_by_element_and_only_as_far_as_asked() {
    // `asSequence()` answers a LAZY pipeline, and the laziness is observable in
    // what gets EVALUATED rather than only in what comes back. A `map` over a
    // list whose second element would divide by zero is an answer when a
    // `take(1)` stops before reaching it, and a fault when the pipeline is
    // eager.
    assert_eq!(
        prog("fun main() { println(listOf(1, 0).asSequence().map { 10 / it }.take(1).toList()) }"),
        "[10]\n"
    );
    // The STEP of a `generateSequence` runs only when the next element is
    // actually wanted, so the seed alone satisfies a `take(1)` and the step
    // never runs; `first { … }` stops at the match rather than restarting the
    // pipeline once per index.
    let counted = |pipeline: &str| {
        prog(&format!(
            "fun main() {{\n    var c = 0\n    val r = generateSequence(1) {{ c++; it + 1 }}{pipeline}\n    println(\"\" + r + c)\n}}"
        ))
    };
    assert_eq!(counted(".take(1).toList()"), "[1]0\n");
    assert_eq!(counted(".first { it > 3 }"), "43\n");
    // A bounded prefix read off an ENDLESS sequence terminates, where
    // materializing the pipeline would hit the pull cap.
    assert_eq!(
        prog("fun main() { println(generateSequence(1) { it + 1 }.elementAt(4)) }"),
        "5\n"
    );
    // A sequence is RE-ITERABLE: two traversals of one pipeline each start from
    // the front rather than sharing progress.
    assert_eq!(
        prog(
            "fun main() {\n    val s = listOf(1, 2, 3).asSequence().map { it * 2 }\n\
             \x20   println(\"\" + s.toList() + s.toList())\n}"
        ),
        "[2, 4, 6][2, 4, 6]\n"
    );
}

#[test]
fn a_lazy_sequence_is_pulled_by_the_members_that_splice_or_render_one() {
    // Every member that reads a container's ELEMENTS has to pull a lazy
    // sequence rather than read it as empty. Each of these answered an empty
    // result when `asSequence` began returning a real pipeline, and each is
    // reached by a different path, so one fix for one of them proves nothing
    // about the others.
    //
    // `joinToString` is the one that hid: a program declaring a `toString()`
    // override lowers it to a DIFFERENT op than every other program does, so it
    // answered the empty string in exactly those programs. The `object` below
    // is what selects that lowering and is load-bearing, not decoration.
    assert_eq!(
        prog(
            "object O { override fun toString(): String = \"O\" }\n\
             fun main() { println(listOf(1, 2, 3).asSequence().joinToString(\"|\")) }"
        ),
        "1|2|3\n"
    );
    assert_eq!(
        prog(
            "object O { override fun toString(): String = \"O\" }\n\
             fun main() { println(generateSequence(1) { it + 1 }.take(3).joinToString(\"-\")) }"
        ),
        "1-2-3\n"
    );
    // `flatMap`/`flatMapIndexed` splice the elements of whatever the lambda
    // answered, and `zip` reads the elements of its ARGUMENT — a sequence in
    // any of those three positions has to be pulled.
    assert_eq!(
        prog(
            "fun main() { println(listOf(1, 2).asSequence()\
             .flatMap { listOf(it, -it).asSequence() }.toList()) }"
        ),
        "[1, -1, 2, -2]\n"
    );
    assert_eq!(
        prog(
            "fun main() { println(listOf(5, 6)\
             .flatMapIndexed { i, v -> listOf(i, v).asSequence().toList() }) }"
        ),
        "[0, 5, 1, 6]\n"
    );
    assert_eq!(
        prog(
            "fun main() { println(listOf(1, 2, 3).asSequence()\
             .zip(listOf(\"a\", \"b\").asSequence()).toList()) }"
        ),
        "[(1, a), (2, b)]\n"
    );
}

/// `kotlin.sequences` declares its own `first`/`last`/`single`/`reduce`/
/// `elementAt`, each wording its empty-or-exhausted fault as a SEQUENCE.
/// kotlinrs materializes a bounded sequence into a `List` and its diagnostics
/// followed that representation, so `listOf<Int>().asSequence().first()` said
/// `List is empty.` The representation is not observable; the message is.
#[test]
fn a_sequence_words_its_faults_as_a_sequence_and_not_as_a_list() {
    for (expr, want) in [
        ("listOf<Int>().asSequence().first()", "java.util.NoSuchElementException: Sequence is empty."),
        ("listOf<Int>().asSequence().last()", "java.util.NoSuchElementException: Sequence is empty."),
        ("listOf<Int>().asSequence().single()", "java.util.NoSuchElementException: Sequence is empty."),
        (
            "listOf<Int>().asSequence().reduce { a, b -> a + b }",
            "java.lang.UnsupportedOperationException: Empty sequence can't be reduced.",
        ),
        (
            "listOf(1).asSequence().elementAt(9)",
            "java.lang.IndexOutOfBoundsException: Sequence doesn't contain element at index 9.",
        ),
        (
            "listOf(1).asSequence().first { it > 9 }",
            "java.util.NoSuchElementException: Sequence contains no element matching the predicate.",
        ),
        // A DERIVED sequence is still a sequence: `drop` answers a `Sequence`,
        // so the wording must survive the step. It did not — the tag stopped
        // after one derivation.
        (
            "listOf(1).asSequence().drop(1).first()",
            "java.util.NoSuchElementException: Sequence is empty.",
        ),
        (
            "listOf(1, 2).asSequence().map { it }.filter { it > 9 }.first()",
            "java.util.NoSuchElementException: Sequence is empty.",
        ),
        // A generated one, which was never a list at any point.
        (
            "generateSequence<Int>(null) { it }.first()",
            "java.util.NoSuchElementException: Sequence is empty.",
        ),
    ] {
        assert_eq!(
            prog(&format!(
                "fun main() {{ try {{ println({expr}) }} catch (e: Throwable) {{ println(e) }} }}"
            )),
            format!("{want}\n"),
            "for {expr}"
        );
    }
    // The LIST wording must not have moved: an untagged receiver still says
    // `List`, which is what makes the two distinguishable at all.
    assert_eq!(
        prog("fun main() { try { println(listOf<Int>().first()) } catch (e: Throwable) { println(e) } }"),
        "java.util.NoSuchElementException: List is empty.\n"
    );
    // And `toList()` on a sequence answers a COLLECTION, so it goes back to the
    // list wording.
    assert_eq!(
        prog("fun main() { try { println(listOf<Int>().asSequence().toList().first()) } catch (e: Throwable) { println(e) } }"),
        "java.util.NoSuchElementException: List is empty.\n"
    );
}

/// Recursion depth BELOW the limit must still run. The guard replaced an abort
/// at a depth under 100 with a raise at 2000, so this is also the pin that the
/// usable depth went up rather than down.
#[test]
fn recursion_below_the_limit_still_completes() {
    assert_eq!(
        prog(
            "fun f(n: Int): Int = if (n == 0) 0 else listOf(n).map { f(it - 1) }[0]\n\
             fun main() { println(f(1500)) }"
        ),
        "0\n"
    );
    // Plain recursion does not nest the interpreter at all — its frames live on
    // the VM's own heap-allocated stack — so it is not subject to the limit.
    assert_eq!(
        prog(
            "fun g(n: Int): Int = if (n == 0) 0 else 1 + g(n - 1)\n\
             fun main() { println(g(50000)) }"
        ),
        "50000\n"
    );
}

/// `substringAfterLast`/`substringBeforeLast` search with Kotlin's
/// `lastIndexOf`, whose default `startIndex` is `lastIndex` and NOT the
/// receiver's length. The two rules agree for every non-empty delimiter and
/// part only for an empty one, which is therefore the case that pins it:
/// `"abc".substringAfterLast("")` is `c` on Kotlin, and the length-based search
/// a `rfind` gives answers the empty string.
#[test]
fn the_last_substring_pair_searches_from_last_index_not_from_length() {
    assert_eq!(stdout(r#"println("abc".substringAfterLast(""))"#), "c\n");
    assert_eq!(stdout(r#"println("abc".substringBeforeLast(""))"#), "ab\n");
    // The non-empty delimiter is the ordinary last occurrence.
    assert_eq!(stdout(r#"println("a.b.c".substringAfterLast("."))"#), "c\n");
    assert_eq!(
        stdout(r#"println("a.b.c".substringBeforeLast("."))"#),
        "a.b\n"
    );
    // An absent delimiter answers `missingDelimiterValue`, which DEFAULTS to
    // the whole receiver — so the one-argument form is that default showing.
    assert_eq!(stdout(r#"println("abc".substringAfterLast("z"))"#), "abc\n");
    assert_eq!(
        stdout(r#"println("abc".substringAfterLast("z", "d"))"#),
        "d\n"
    );
    assert_eq!(stdout(r#"println("abc".substringBefore("z", "d"))"#), "d\n");
}

/// `Result.getOrDefault` and `Map.getOrDefault` are different members that
/// share a name, and the host's member dispatch runs the map body for EVERY
/// heap kind. Without a receiver test the `Result` call fell into the map body,
/// found no key, and answered the map form's SECOND argument — which a `Result`
/// call never passes, so every `runCatching { … }.getOrDefault(x)` was null.
#[test]
fn get_or_default_tells_a_result_receiver_from_a_map_one() {
    assert_eq!(stdout("println(runCatching { 7 }.getOrDefault(-1))"), "7\n");
    assert_eq!(
        stdout(r#"println(runCatching { "x".toInt() }.getOrDefault(-1))"#),
        "-1\n"
    );
    assert_eq!(stdout("println(runCatching { 7 }.getOrThrow())"), "7\n");
    // The map form still reads its own two arguments.
    assert_eq!(
        stdout(r#"println(mapOf("a" to 1).getOrDefault("a", 0))"#),
        "1\n"
    );
    assert_eq!(
        stdout(r#"println(mapOf("a" to 1).getOrDefault("z", 0))"#),
        "0\n"
    );
}

/// `split`'s `limit` caps the number of parts and leaves the LAST one unsplit.
/// It can only be passed by name — the delimiters are a vararg — and the empty
/// delimiter is the case that separates a real scan from a post-hoc truncation:
/// `"abc".split("", limit = 2)` is `[, abc]`, because the empty delimiter
/// matches at position 0 before it matches anywhere else.
#[test]
fn split_limit_caps_the_parts_and_keeps_the_remainder_whole() {
    assert_eq!(
        stdout(r#"println("a,b,c".split(",", limit = 2))"#),
        "[a, b,c]\n"
    );
    assert_eq!(
        stdout(r#"println("a,b,c".split(",", limit = 1))"#),
        "[a,b,c]\n"
    );
    // Zero is "no limit", not "no parts".
    assert_eq!(
        stdout(r#"println("a,b,c".split(",", limit = 0))"#),
        "[a, b, c]\n"
    );
    assert_eq!(
        stdout(r#"println("abc".split("", limit = 2))"#),
        "[, abc]\n"
    );
    assert_eq!(
        stdout(r#"println("abc".split("", limit = 3))"#),
        "[, a, bc]\n"
    );
    // Several delimiters still race for the earliest match under a limit.
    assert_eq!(
        stdout(r#"println("a1b2c".split("1", "2", limit = 2))"#),
        "[a, b2c]\n"
    );
    // The unlimited spellings are unchanged, including the empty delimiter's
    // parts at both ends.
    assert_eq!(stdout(r#"println("abc".split(""))"#), "[, a, b, c, ]\n");
    assert_eq!(stdout(r#"println("a1b2c".split("1", "2"))"#), "[a, b, c]\n");
}

/// The infix set functions parse only when GLUED to the left operand's line.
/// The lexer drops newlines, so without that test a following statement that
/// begins with one of these names would be swallowed as an infix argument and
/// disappear — `union`, unlike `shl`, is a plausible name for a local.
#[test]
fn the_infix_set_functions_do_not_swallow_the_next_statement() {
    assert_eq!(stdout("println(setOf(1, 2) union setOf(3))"), "[1, 2, 3]\n");
    assert_eq!(stdout("println(setOf(1, 2) intersect setOf(2))"), "[2]\n");
    assert_eq!(stdout("println(setOf(1, 2) subtract setOf(1))"), "[2]\n");
    // `union` on its own line is a local read, not an infix call on the value
    // above it — both statements must survive.
    assert_eq!(
        prog(
            "fun main() {\n\
             \x20   val union = 5\n\
             \x20   println(1)\n\
             \x20   println(union)\n\
             }"
        ),
        "1\n5\n"
    );
}

/// The IN-PLACE reorderings answer `Unit` and mutate the receiver, where the
/// `sorted…`/`reversed()` members copy and leave it alone.
/// `sortByDescending` is `sortWith(compareByDescending(selector))`: the
/// COMPARISON is inverted, so ties keep their input order — reversing a stable
/// ascending sort would reverse the ties with them.
#[test]
fn the_in_place_sorts_mutate_the_receiver_and_keep_ties_in_order() {
    assert_eq!(
        prog("fun main() { val m = mutableListOf(3, 1, 2); m.sort(); println(m); m.sortDescending(); println(m); m.reverse(); println(m) }"),
        "[1, 2, 3]\n[3, 2, 1]\n[1, 2, 3]\n"
    );
    // Two elements of equal length stay in the order they were given.
    assert_eq!(
        prog(
            r#"fun main() { val t = mutableListOf("bb", "aa", "c"); t.sortByDescending { it.length }; println(t) }"#
        ),
        "[bb, aa, c]\n"
    );
    assert_eq!(
        prog(
            r#"fun main() { val l = mutableListOf("bb", "a", "ccc"); l.sortBy { it.length }; println(l) }"#
        ),
        "[a, bb, ccc]\n"
    );
    // A primitive array sorts in place through the same member.
    assert_eq!(
        prog("fun main() { val a = intArrayOf(3, 1, 2); a.sort(); println(a.contentToString()) }"),
        "[1, 2, 3]\n"
    );
}

/// `titlecaseChar` is the single-`Char` uppercase mapping EXCEPT for the two
/// families that have a titlecase form of their own, and `capitalize` prefers
/// that form when it differs. The exception list was measured against the
/// reference toolchain over all 65 536 `Char`s.
#[test]
fn titlecase_differs_from_uppercase_only_for_the_two_measured_families() {
    assert_eq!(stdout("println('a'.titlecaseChar())"), "A\n");
    assert_eq!(stdout("println('1'.titlecaseChar())"), "1\n");
    // The Latin digraphs titlecase to their MIDDLE form (U+01F2), where the
    // uppercase mapping goes all the way to U+01F1.
    assert_eq!(stdout(r"println('ǳ'.titlecaseChar().code)"), "498\n");
    assert_eq!(stdout(r"println('ǳ'.uppercaseChar().code)"), "497\n");
    assert_eq!(stdout(r"println('ǆ'.titlecaseChar().code)"), "453\n");
    // Georgian Mkhedruli has no titlecase mapping and answers itself, where
    // the uppercase one moves it into the Mtavruli block.
    assert_eq!(stdout(r"println('ა'.titlecaseChar().code)"), "4304\n");
    assert_eq!(stdout(r"println('ა'.uppercaseChar().code)"), "7312\n");
    assert_eq!(stdout(r#"println("abc".capitalize())"#), "Abc\n");
    assert_eq!(stdout(r#"println("ABC".capitalize())"#), "ABC\n");
    assert_eq!(stdout(r#"println("ABC".decapitalize())"#), "aBC\n");
    assert_eq!(stdout(r#"println("".capitalize())"#), "\n");
}

/// A progression's `step` is a PROPERTY, and it is the one range member the
/// element list cannot answer — the elements no longer say how far apart they
/// were. A downward progression reports a NEGATIVE step, which is how it knows
/// its direction.
#[test]
fn a_progression_reports_its_own_step_with_its_sign() {
    assert_eq!(stdout("println((1..10).step)"), "1\n");
    assert_eq!(stdout("println((10 downTo 1).step)"), "-1\n");
    assert_eq!(stdout("println((1..10 step 3).step)"), "3\n");
    assert_eq!(stdout("println((10 downTo 1 step 3).step)"), "-3\n");
    assert_eq!(stdout("println((1 until 10).step)"), "1\n");
}

/// `lazy { }` written as a VALUE builds the same cell `by lazy` stores, so the
/// thunk runs at most once and only when `.value` asks for it.
/// `isInitialized()` must NOT force it — asking the question would otherwise
/// answer it.
#[test]
fn a_standalone_lazy_forces_once_and_only_when_asked() {
    assert_eq!(
        prog("fun main() { val x = lazy { 42 }; println(x.isInitialized()); println(x.value); println(x.isInitialized()) }"),
        "false\n42\ntrue\n"
    );
    assert_eq!(
        prog("fun main() { var n = 0; val y = lazy { n++; 7 }; println(y.value); println(y.value); println(n) }"),
        "7\n7\n1\n"
    );
}

/// `Integer.toBinaryString`/`toHexString`/`toOctalString` render the UNSIGNED
/// 32-bit pattern, which is the whole difference from `toString(radix)` and is
/// visible only for a negative value.
#[test]
fn the_integer_radix_renderers_are_unsigned_where_to_string_is_not() {
    assert_eq!(stdout("println(Integer.toBinaryString(255))"), "11111111\n");
    assert_eq!(stdout("println(Integer.toHexString(255))"), "ff\n");
    assert_eq!(stdout("println(Integer.toOctalString(8))"), "10\n");
    assert_eq!(stdout("println(Integer.toHexString(-1))"), "ffffffff\n");
    assert_eq!(
        stdout("println(Integer.toBinaryString(-1))"),
        "11111111111111111111111111111111\n"
    );
    // The member spelling keeps the sign, so the two are not interchangeable.
    assert_eq!(stdout("println((-1).toString(2))"), "-1\n");
}

/// `getOrPut` keys on a NULL VALUE rather than on an absent key — that is what
/// the stdlib body tests — and its lambda runs only when it has to.
#[test]
fn get_or_put_computes_only_for_a_missing_key_and_stores_the_result() {
    assert_eq!(
        prog(
            r#"fun main() { val m = mutableMapOf("a" to 1); println(m.getOrPut("a") { 99 }); println(m.getOrPut("b") { 2 }); println(m) }"#
        ),
        "1\n2\n{a=1, b=2}\n"
    );
    // The lambda is not called at all for a key that is already there.
    assert_eq!(
        prog(
            r#"fun main() { var calls = 0; val m = mutableMapOf("a" to 1); m.getOrPut("a") { calls++; 9 }; println(calls) }"#
        ),
        "0\n"
    );
}

/// `eachCountTo` fills the map it was GIVEN and answers that same map, so the
/// caller's handle is the result. A fresh allocation here would leave the
/// destination empty while still printing the right counts — the one shape
/// where the return value alone cannot tell the two apart. It also
/// ACCUMULATES onto a count that is already there.
#[test]
fn each_count_to_writes_into_the_destination_it_was_handed() {
    assert_eq!(
        prog(
            r#"fun main() { val d = mutableMapOf<Char, Int>(); listOf("ax", "ay", "b").groupingBy { it.first() }.eachCountTo(d); println(d) }"#
        ),
        "{a=2, b=1}\n"
    );
    assert_eq!(
        prog(
            r#"fun main() { val d = mutableMapOf('a' to 10); println(listOf("ax", "b").groupingBy { it.first() }.eachCountTo(d)); println(d) }"#
        ),
        "{a=11, b=1}\n{a=11, b=1}\n"
    );
}

/// The vararg `trim` overloads take a CHARACTER SET and share nothing with the
/// whitespace ones but a name. Passing a `Char` used to be accepted and
/// ignored, which answered the receiver unchanged — a silent wrong answer
/// rather than a diagnostic.
#[test]
fn the_trim_family_takes_a_character_set_when_given_arguments() {
    assert_eq!(stdout(r#"println("xxhixx".trimStart('x'))"#), "hixx\n");
    assert_eq!(stdout(r#"println("xxhixx".trimEnd('x'))"#), "xxhi\n");
    assert_eq!(stdout(r#"println("xyhixy".trim('x', 'y'))"#), "hi\n");
    // A receiver with none of the named characters is left alone, and the
    // no-argument spelling is still the whitespace trim.
    assert_eq!(stdout(r#"println("hi".trimEnd('x'))"#), "hi\n");
    assert_eq!(stdout(r#"println("  hi  ".trim() + "|")"#), "hi|\n");
    assert_eq!(stdout(r#"println("".trimStart('x') + "|")"#), "|\n");
}

/// A `for` loop's `continue` targets the loop's INCREMENT, not its top — the
/// counted form must still advance, or the loop never terminates. Pinned for
/// every loop shape at once, because a jump patched to the wrong target is a
/// hang rather than a wrong answer and would take the whole suite down with it.
#[test]
fn continue_advances_every_loop_shape_rather_than_repeating_it() {
    assert_eq!(
        prog("fun main() { for (i in 1..5) { if (i == 3) continue; print(i) }; println() }"),
        "1245\n"
    );
    assert_eq!(
        prog("fun main() { for (i in 5 downTo 1 step 2) { if (i == 3) continue; print(i) }; println() }"),
        "51\n"
    );
    assert_eq!(
        prog("fun main() { for (x in listOf(1, 2, 3)) { if (x == 2) continue; print(x) }; println() }"),
        "13\n"
    );
    assert_eq!(
        prog("fun main() { var j = 0; while (j < 5) { j++; if (j == 2) continue; print(j) }; println() }"),
        "1345\n"
    );
    assert_eq!(
        prog("fun main() { var j = 0; do { j++; if (j == 2) continue; print(j) } while (j < 4); println() }"),
        "134\n"
    );
    assert_eq!(
        prog("fun main() { outer@ for (a in 1..3) { for (b in 1..3) { if (b == 2) continue@outer; print(\"\" + a + b) } }; println() }"),
        "112131\n"
    );
}

/// `ignoreCase` must CHANGE the answer, not merely be accepted.
///
/// Every member below took the flag and dropped it: `"abcabc".indexOf("B", 1,
/// true)` answered -1 where the reference answers 1, and the same for
/// `lastIndexOf`, `startsWith`, `endsWith`, `replace`, `replaceFirst`, `split`
/// and `Char.equals`. Each case is pinned twice — flag off and flag on — so a
/// member that stops reading the flag again cannot pass by keeping the
/// case-sensitive answer.
#[test]
fn ignore_case_changes_the_answer_rather_than_being_accepted_and_dropped() {
    assert_eq!(stdout(r#"println("abcabc".indexOf("B", 1, true))"#), "1\n");
    assert_eq!(stdout(r#"println("abcabc".indexOf("B", 1))"#), "-1\n");
    assert_eq!(stdout(r#"println("abcabc".indexOf('B', 1, true))"#), "1\n");
    assert_eq!(
        stdout(r#"println("abcabc".lastIndexOf("B", 5, true))"#),
        "4\n"
    );
    assert_eq!(stdout(r#"println("abcabc".lastIndexOf("B", 5))"#), "-1\n");
    assert_eq!(
        stdout(r#"println("abcabc".startsWith("ABC", true))"#),
        "true\n"
    );
    assert_eq!(stdout(r#"println("abcabc".startsWith("ABC"))"#), "false\n");
    assert_eq!(
        stdout(r#"println("abcabc".startsWith("A", 0, true))"#),
        "true\n"
    );
    assert_eq!(
        stdout(r#"println("ABCabc".endsWith("ABC", true))"#),
        "true\n"
    );
    assert_eq!(stdout(r#"println("ABCabc".endsWith("ABC"))"#), "false\n");
    assert_eq!(stdout(r#"println("abcabc".contains("B", true))"#), "true\n");
    assert_eq!(stdout(r#"println("abcabc".contains("B"))"#), "false\n");
    assert_eq!(
        stdout(r#"println("aXbXc".replace("x", "-", true))"#),
        "a-b-c\n"
    );
    assert_eq!(stdout(r#"println("aXbXc".replace("x", "-"))"#), "aXbXc\n");
    assert_eq!(
        stdout(r#"println("aXbXc".replaceFirst("x", "-", true))"#),
        "a-bXc\n"
    );
    assert_eq!(
        stdout(r#"println("aXbXc".split("x", ignoreCase = true))"#),
        "[a, b, c]\n"
    );
    assert_eq!(stdout(r#"println("aXbXc".split("x"))"#), "[aXbXc]\n");
    assert_eq!(stdout("println('a'.equals('A', true))"), "true\n");
    assert_eq!(stdout("println('a'.equals('A'))"), "false\n");
    // Naming the flag has to reach the same place as passing it positionally,
    // and it used to be rejected outright ("has no parameter `ignoreCase`").
    assert_eq!(
        stdout(r#"println("abcabc".indexOf("B", ignoreCase = true))"#),
        "1\n"
    );
    assert_eq!(
        stdout(r#"println("HELLO".endsWith("lo", ignoreCase = true))"#),
        "true\n"
    );
    assert_eq!(
        stdout(r#"println("abc".replace("B", "z", ignoreCase = true))"#),
        "azc\n"
    );
}

/// The `ignoreCase` fold goes THROUGH the uppercase, and is per character.
///
/// Two `Char` pairs separate that rule from the two it is easy to mistake it
/// for. `U+03D1`/`U+03F4` and `U+0130`/`U+0131` fold together only when the
/// key is `lowercaseChar(uppercaseChar(c))`; folding the character directly, or
/// comparing two whole `lowercase()`d strings, calls both pairs different. The
/// reference calls every one of these true, under every member that takes the
/// flag — which is why they are pinned together rather than one per member.
#[test]
fn the_ignore_case_fold_is_through_the_uppercase_and_per_character() {
    for probe in [
        "'ϑ'.equals('ϴ', true)",
        r#""ϑ".equals("ϴ", true)"#,
        r#""aϑ".contains("ϴ", true)"#,
        r#""ϑ".startsWith("ϴ", true)"#,
        r#""ϑ".endsWith("ϴ", true)"#,
        r#""ϑ".regionMatches(0, "ϴ", 0, 1, true)"#,
        "'İ'.equals('ı', true)",
        r#""İ".equals("ı", true)"#,
        r#""İ".equals("i", true)"#,
        r#""aİ".contains("ı", true)"#,
    ] {
        assert_eq!(stdout(&format!("println({probe})")), "true\n", "{probe}");
    }
    assert_eq!(stdout(r#"println("aϑ".indexOf('ϴ', 0, true))"#), "1\n");
    assert_eq!(stdout(r#"println("aϑb".replace("ϴ", "!", true))"#), "a!b\n");
    assert_eq!(
        stdout(r#"println("aϑb".split("ϴ", ignoreCase = true))"#),
        "[a, b]\n"
    );
    assert_eq!(stdout(r#"println("İx".replace("ı", "!", true))"#), "!x\n");
    // A folded character need not encode to the width of the one it came from
    // ('İ' is one UTF-16 unit and folds to 'i', which is also one, but two
    // UTF-8 bytes fold to one), so a scan that worked in BYTES moved the cut.
    assert_eq!(
        stdout(r#"println("aİb".split("i", ignoreCase = true))"#),
        "[a, b]\n"
    );
}

/// `uppercaseChar`/`lowercaseChar` are the JVM's SIMPLE case mappings, which
/// Rust's `char::to_uppercase`/`to_lowercase` (the FULL ones) are not.
///
/// Three families disagree, and all three were measured over the whole `Char`
/// range against the oracle rather than recalled:
///
///   * The 27 Greek letters with a subscript iota, whose full uppercase expands
///     past one character while the simple one is their titlecase form.
///   * `U+0130`, whose full lowercase expands to `i` and a combining dot.
///   * Every surrogate half, which is not a character at all and maps to
///     itself — these answered `U+FFFD` before.
#[test]
fn the_char_case_members_are_the_jvms_simple_mappings() {
    assert_eq!(stdout("println('ᾀ'.uppercaseChar().code)"), "8072\n");
    assert_eq!(stdout("println('ᾐ'.uppercaseChar().code)"), "8088\n");
    assert_eq!(stdout("println('ᾠ'.uppercaseChar().code)"), "8104\n");
    assert_eq!(stdout("println('ᾳ'.uppercaseChar().code)"), "8124\n");
    assert_eq!(stdout("println('ῃ'.uppercaseChar().code)"), "8140\n");
    assert_eq!(stdout("println('ῳ'.uppercaseChar().code)"), "8188\n");
    // 'ß' has no one-character uppercase at all and keeps itself.
    assert_eq!(stdout("println('ß'.uppercaseChar().code)"), "223\n");
    assert_eq!(stdout("println('İ'.lowercaseChar().code)"), "105\n");
    assert_eq!(
        prog("fun main() { val d = 55296.toChar(); println(d.uppercaseChar().code); println(d.lowercaseChar().code); println(d.titlecaseChar().code) }"),
        "55296\n55296\n55296\n"
    );
    // `titlecase()` is the STRING mapping: when the full uppercase expands, it
    // is that uppercase with everything past its first character lowercased
    // again — not the uppercase unchanged.
    assert_eq!(stdout("println('ᾀ'.titlecase())"), "Ἀι\n");
    assert_eq!(stdout("println('ß'.titlecase())"), "Ss\n");
    assert_eq!(stdout("println('և'.titlecase())"), "Եւ\n");
    // U+0149 is the one character that keeps its whole uppercase.
    assert_eq!(stdout("println('ŉ'.titlecase())"), "ʼN\n");
}

/// `compareTo(other, ignoreCase = true)` is the JVM's `CASE_INSENSITIVE_ORDER`,
/// which is neither a comparison of two lowercased strings nor a walk over
/// `Char`s.
///
/// It compares CODE POINTS, but only as far as the shorter receiver's UTF-16
/// length, so a surrogate pair straddling that bound reads as a lone surrogate.
/// Each assertion below fails under a different wrong implementation.
#[test]
fn compare_to_ignoring_case_is_the_jvms_case_insensitive_order() {
    // A whole-string lowercase calls these unequal; the fold does not.
    assert_eq!(stdout(r#"println("ϑ".compareTo("ϴ", true))"#), "0\n");
    assert_eq!(stdout(r#"println("İ".compareTo("i", true))"#), "0\n");
    // The difference is of the folding KEYS, not of the characters.
    assert_eq!(stdout(r#"println("ß".compareTo("ss", true))"#), "108\n");
    assert_eq!(stdout(r#"println("ϑ".compareTo("i", true))"#), "847\n");
    // A common prefix that folds equal falls through to the UTF-16 length.
    assert_eq!(stdout(r#"println("abc".compareTo("AB", true))"#), "1\n");
    assert_eq!(stdout(r#"println("abc".compareTo("ABD", true))"#), "-1\n");
    // A per-`Char` walk answers 40 here, because the low surrogates differ; the
    // code-point walk folds the pair together and goes on to 'b' vs 'a'.
    assert_eq!(stdout(r#"println("𐐨b".compareTo("𐐀a", true))"#), "1\n");
    // …and an UNBOUNDED code-point walk answers 66503 here, because the pair is
    // longer than the receiver it is being compared against.
    assert_eq!(stdout(r#"println("𐐨".compareTo("a", true))"#), "55200\n");
    assert_eq!(stdout(r#"println("abc".compareTo("ABC"))"#), "32\n");
}

/// `indexOfAny`/`findAnyOf` and their `…Last` pair scan POSITIONS and report
/// UTF-16 offsets, and `startIndex` is one too.
///
/// Reading `startIndex` as a character index instead answered 2 for
/// `"𐐨cbiϑ".lastIndexOfAny(listOf("c", "s"), 1, true)` where the reference
/// answers -1, and an empty receiver walked a range built from `-1 as usize`
/// and aborted the interpreter.
#[test]
fn the_any_of_searches_index_in_utf16_units_and_take_the_first_needle() {
    assert_eq!(
        stdout(r#"println("𐐨cbiϑ".lastIndexOfAny(listOf("c", "s"), 1, true))"#),
        "-1\n"
    );
    assert_eq!(
        stdout(r#"println("".findLastAnyOf(listOf("b")))"#),
        "null\n"
    );
    assert_eq!(stdout(r#"println("".indexOfAny(listOf("b")))"#), "-1\n");
    assert_eq!(
        stdout(r#"println("abc".indexOfAny(listOf("B", "c"), 0, true))"#),
        "1\n"
    );
    assert_eq!(
        stdout(r#"println("abc".lastIndexOfAny(listOf("b", "c")))"#),
        "2\n"
    );
    // `findAnyOf` answers the needle from the COLLECTION, not the text it
    // matched, so an `ignoreCase` hit reports the argument's own casing.
    assert_eq!(
        stdout(r#"println("abc".findAnyOf(listOf("B"), 0, true))"#),
        "(1, B)\n"
    );
    assert_eq!(
        stdout(r#"println("abc".findLastAnyOf(listOf("b", "c")))"#),
        "(2, c)\n"
    );
    // Two needles matching at one position are decided by the collection's
    // order, not by their lengths.
    assert_eq!(
        stdout(r#"println("abc".findAnyOf(listOf("ab", "a")))"#),
        "(0, ab)\n"
    );
    assert_eq!(
        stdout(r#"println("abc".findAnyOf(listOf("a", "ab")))"#),
        "(0, a)\n"
    );
}

/// A top-level `val` records its class, so every receiver-keyed rule reaches
/// file scope and not only function scope.
///
/// `PropMeta.class` was copied from the type ANNOTATION alone, so `val p = C()`
/// carried no class while `val p: C = C()` did — and `infer_class` reads exactly
/// that field for a global. The operator CONVENTIONS are resolved against the
/// left operand's class, so the same two objects answered `K3` held in locals
/// and failed to compile with `unresolved reference: plus on K` held in
/// top-level `val`s. Measured against `kotlinc` 2.4.10 / JRE 21.0.12: every
/// line below prints what the reference prints.
#[test]
fn a_top_level_val_resolves_the_operator_conventions_of_its_class() {
    let decl = r#"class K(val v: Int) {
    operator fun plus(o: K) = K(v + o.v)
    operator fun times(o: K) = K(v * o.v)
    operator fun compareTo(other: K) = v - other.v
    override fun toString() = "K" + v
}
"#;
    // Unannotated — the case that did not compile at all.
    assert_eq!(
        stdout(&format!(
            "{decl}val a = K(1)\nval b = K(2)\nfun main() {{ println(a + b) }}"
        )),
        "K3\n"
    );
    assert_eq!(
        stdout(&format!(
            "{decl}val a = K(3)\nval b = K(4)\nfun main() {{ println(a * b) }}"
        )),
        "K12\n"
    );
    // `<` is `compareTo`'s SIGN, and it is keyed off the same class field.
    assert_eq!(
        stdout(&format!(
            "{decl}val a = K(1)\nval b = K(2)\nfun main() {{ println(a < b) }}"
        )),
        "true\n"
    );
    // A global naming an EARLIER global inherits its class through the same
    // forward pass, in the order the initializers run.
    assert_eq!(
        stdout(&format!(
            "{decl}val a = K(5)\nval b = a\nfun main() {{ println(b + K(1)) }}"
        )),
        "K6\n"
    );
    // The annotated spelling already worked and must keep working.
    assert_eq!(
        stdout(&format!(
            "{decl}val a: K = K(7)\nfun main() {{ println(a + K(1)) }}"
        )),
        "K8\n"
    );
    // A member call on an unannotated global resolves statically too.
    assert_eq!(
        stdout(&format!(
            "{decl}val a = K(9)\nfun main() {{ println(a.v) }}"
        )),
        "9\n"
    );
}

/// A top-level `val` holding a lambda is callable as `f(x)`.
///
/// The local-slot arm of `compile_call` covered the same shape inside a
/// function; at file scope every remaining arm looks for a callable
/// DECLARATION, and a property is not one, so `f(3)` was `unresolved
/// reference: f`. Reference `kotlinc` prints `103` / `4` for the first two.
#[test]
fn a_top_level_val_holding_a_lambda_is_callable() {
    assert_eq!(
        stdout("val f = { x: Int -> x + 100 }\nfun main() { println(f(3)) }"),
        "103\n"
    );
    // The annotated function-type spelling infers `Unknown`, not `Obj`.
    assert_eq!(
        stdout("val f: (Int) -> Int = { x -> x + 1 }\nfun main() { println(f(3)) }"),
        "4\n"
    );
    assert_eq!(
        stdout("val f = { a: Int, b: Int -> a * b }\nfun main() { println(f(3, 4)) }"),
        "12\n"
    );
    // A top-level `fun` of the same name still wins: Kotlin keeps functions and
    // properties in separate namespaces and resolves a CALL against the
    // function. Reference prints `fun`.
    assert_eq!(
        stdout(
            "val f = { _: Int -> \"val\" }\nfun f(x: Int) = \"fun\"\nfun main() { println(f(1)) }"
        ),
        "fun\n"
    );
    // A local binding still shadows the global, as it did before.
    assert_eq!(
        stdout("val f = { x: Int -> x + 1 }\nfun main() { val f = { x: Int -> x + 2 }; println(f(1)) }"),
        "3\n"
    );
    // A non-callable global keeps its COMPILE-TIME diagnostic rather than
    // becoming a closure call that only fails at run time.
    let out = eval("val n = 5\nfun main() { println(n(1)) }");
    assert!(
        !out.status.success(),
        "`n(1)` on an Int global must not compile"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unresolved reference: n"),
        "stderr was {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A lambda parameter may be a DESTRUCTURING group.
///
/// `{ (k, v) -> … }` is how `Map.map`/`filter`/`forEach` are almost always
/// written, and it was a parse error (`expected RParen, found Comma`). Kotlin
/// defines the group as ONE parameter whose components are unpacked in the
/// body, which is how it lowers here — a synthetic parameter plus the same
/// `componentN` destructuring node `val (k, v) = e` produces. Every expectation
/// below was captured from `kotlinc` 2.4.10 on JRE 21.0.12.
#[test]
fn a_lambda_parameter_may_be_a_destructuring_group() {
    assert_eq!(
        stdout(r#"println(mapOf("a" to 1, "b" to 2).map { (k, v) -> "$k=$v" })"#),
        "[a=1, b=2]\n"
    );
    assert_eq!(
        stdout(r#"println(mapOf("a" to 1, "b" to 2).filter { (k, v) -> v > 1 })"#),
        "{b=2}\n"
    );
    assert_eq!(
        stdout("println(listOf(1 to 2, 3 to 4).map { (a, b) -> a + b })"),
        "[3, 7]\n"
    );
    // `_` discards a component, as it does in a `val` destructuring.
    assert_eq!(
        stdout("println(listOf(1 to 2).map { (a, _) -> a })"),
        "[1]\n"
    );
    assert_eq!(
        stdout("println(listOf(1 to 2, 3 to 1).sortedBy { (_, v) -> v })"),
        "[(3, 1), (1, 2)]\n"
    );
    // An annotated component. The annotation is only consumed — the binding's
    // type comes from `componentN`'s receiver — but the scan has to STOP at the
    // group's `)`, which is what made this spelling a parse error of its own.
    assert_eq!(
        stdout("println(listOf(1 to 2).map { (x: Int, y: Int) -> x - y })"),
        "[-1]\n"
    );
    // Three components, and a `data class`'s generated `componentN`.
    assert_eq!(
        stdout("println(listOf(Triple(1,2,3)).map { (x, y, z) -> x + y + z })"),
        "[6]\n"
    );
    assert_eq!(
        stdout(
            "data class Rec(val n: String, val q: Int)\nfun main() { println(listOf(Rec(\"r\", 7)).map { (n, q) -> \"$n$q\" }) }"
        ),
        "[r7]\n"
    );
    assert_eq!(
        stdout("println(mapOf(1 to 2).entries.map { (k, v) -> k * v })"),
        "[2]\n"
    );
    assert_eq!(
        stdout(r#"println(listOf(1 to "x").associate { (k, v) -> v to k })"#),
        "{x=1}\n"
    );
    // The scan is SPECULATIVE: a body that merely opens with a parenthesis is
    // still a body. Rolling this back wrongly would turn every such lambda into
    // a parse error.
    assert_eq!(
        stdout("val a = 1\nval b = 2\nfun main() { println(listOf(1,2,3).map { (a + b).toString() }.size) }"),
        "3\n"
    );
    assert_eq!(stdout("println(listOf(1).map { (it) })"), "[1]\n");
}

/// The callable-reference operator `::`.
///
/// `::` did not lex at all — `listOf(1).map(::inc)` was `unexpected token
/// Colon`. Kotlin's definition is that a reference denotes a FUNCTION, so it
/// lowers to the lambda that calls it and rides the closure path that already
/// existed. Every expectation below was captured from `kotlinc` 2.4.10 on
/// JRE 21.0.12.
#[test]
fn the_callable_reference_operator_denotes_a_function() {
    // `::name` — a top-level function.
    assert_eq!(
        stdout("fun inc(x: Int) = x + 1\nfun main() { println(listOf(1,2,3).map(::inc)) }"),
        "[2, 3, 4]\n"
    );
    assert_eq!(
        stdout(
            "fun add(a: Int, b: Int) = a + b\nfun main() { println(listOf(1,2,3).fold(0, ::add)) }"
        ),
        "6\n"
    );
    assert_eq!(
        stdout("fun odd(x: Int) = x % 2 == 1\nfun main() { println(listOf(1,2,3).filter(::odd)) }"),
        "[1, 3]\n"
    );
    // A zero-parameter function's reference is a zero-parameter function.
    assert_eq!(
        stdout("fun nul(): Int = 7\nfun main() { val f = ::nul; println(f()) }"),
        "7\n"
    );
    // `::println` is a reference to the built-in.
    assert_eq!(
        stdout(r#"fun main() { listOf("x","y").forEach(::println) }"#),
        "x\ny\n"
    );
    // `::C` is C's primary constructor.
    assert_eq!(
        stdout(
            "class C(val v: Int) { override fun toString() = \"C\" + v }\nfun main() { println(listOf(1,2).map(::C)) }"
        ),
        "[C1, C2]\n"
    );

    // `Type::member` — UNBOUND: the receiver becomes the first parameter.
    let c = "class C(val v: Int) { fun twice() = v * 2\n fun plusN(n: Int) = v + n }\n";
    assert_eq!(
        stdout(&format!(
            "{c}fun main() {{ println(listOf(C(1), C(2)).map(C::twice)) }}"
        )),
        "[2, 4]\n"
    );
    // A PROPERTY reference is a one-parameter function too.
    assert_eq!(
        stdout(&format!(
            "{c}fun main() {{ println(listOf(C(1), C(2)).map(C::v)) }}"
        )),
        "[1, 2]\n"
    );
    assert_eq!(
        stdout(
            "data class P(val n: String, val q: Int)\nfun main() { println(listOf(P(\"a\",1), P(\"b\",2)).sortedByDescending(P::q)) }"
        ),
        "[P(n=b, q=2), P(n=a, q=1)]\n"
    );
    // A BUILT-IN receiver type, where the frontend keeps no arity table and the
    // reference lowers to a one-parameter member access — a property and a
    // zero-argument method alike.
    assert_eq!(
        stdout(r#"println(listOf("aa","b").map(String::length))"#),
        "[2, 1]\n"
    );
    assert_eq!(
        stdout(r#"println(listOf("aa","b").map(String::uppercase))"#),
        "[AA, B]\n"
    );

    // `expr::member` — BOUND: the receiver is captured, and the function takes
    // only the member's own parameters.
    assert_eq!(
        stdout(&format!(
            "{c}fun main() {{ val c = C(5); println(listOf(1,2).map(c::plusN)) }}"
        )),
        "[6, 7]\n"
    );
    // A local of the same name SHADOWS the type, so this is a bound reference
    // on the value and not an unbound one on the class.
    assert_eq!(
        stdout(&format!(
            "{c}fun main() {{ val C = C(4); println(listOf(1).map(C::plusN)) }}"
        )),
        "[5]\n"
    );
    // A computed receiver is evaluated ONCE, where the reference is written —
    // not once per invocation of the resulting function.
    assert_eq!(
        stdout(
            "var n = 0\nfun bump(): String { n += 1; return \"s\" }\nfun main() { val f = bump()::length; println(listOf(1,2,3).map { f() }); println(n) }"
        ),
        "[1, 1, 1]\n1\n"
    );
    // A name that resolves to nothing is still a compile-time error.
    let out = eval("fun main() { println(listOf(1).map(::nope)) }");
    assert!(!out.status.success(), "`::nope` must not compile");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unresolved reference: nope"),
        "stderr was {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The map builders and the `toMap` conversions that were `unresolved
/// reference`.
///
/// `linkedMapOf`/`sortedMapOf`/`TreeMap` are the ordered `Map` builders whose
/// `Set` counterparts (`linkedSetOf`/`sortedSetOf`/`TreeSet`) already resolved;
/// each one's ITERATION order is the whole reason it exists, so that is what
/// these pin. Captured from `kotlinc` 2.4.10 on JRE 21.0.12.
#[test]
fn the_ordered_map_builders_and_to_map_conversions_resolve() {
    // Insertion order, like `mapOf`.
    assert_eq!(
        stdout(r#"println(linkedMapOf(2 to "b", 1 to "a"))"#),
        "{2=b, 1=a}\n"
    );
    // KEY order — the point of a `TreeMap`.
    assert_eq!(
        stdout(r#"println(sortedMapOf(2 to "b", 1 to "a"))"#),
        "{1=a, 2=b}\n"
    );
    assert_eq!(
        stdout(r#"println(sortedMapOf("b" to 1, "a" to 2).keys)"#),
        "[a, b]\n"
    );
    // The JVM constructor's COPY form, which takes a map rather than pairs.
    assert_eq!(
        stdout("println(TreeMap(mapOf(3 to 1, 1 to 2)))"),
        "{1=2, 3=1}\n"
    );
    // Structural `==`, which needs the builder to be typed as a heap object.
    assert_eq!(
        stdout("println(linkedMapOf(1 to 1) == linkedMapOf(1 to 1))"),
        "true\n"
    );

    // `Iterable<Pair<K, V>>.toMap()`.
    assert_eq!(stdout("println(listOf(1 to 2).toMap())"), "{1=2}\n");
    // A later pair overwrites an earlier equal key, and the map keeps the
    // FIRST insertion's position.
    assert_eq!(stdout("println(listOf(1 to 2, 1 to 3).toMap())"), "{1=3}\n");
    assert_eq!(
        stdout(r#"println(listOf("a" to 1, "b" to 2).toMap().keys)"#),
        "[a, b]\n"
    );
    assert_eq!(stdout("println(setOf(1 to 2).toMap())"), "{1=2}\n");
    assert_eq!(stdout("println(listOf<Pair<Int,Int>>().toMap())"), "{}\n");

    // `Map.toMap()` / `Map.toMutableMap()` — a COPY, in the receiver's own
    // iteration order. The copy must be independent of the receiver.
    assert_eq!(stdout("println(mapOf(1 to 2).toMap())"), "{1=2}\n");
    assert_eq!(
        stdout(
            "val m = mapOf(1 to 2).toMutableMap(); m[3] = 4; println(m); println(mapOf(1 to 2))"
        ),
        "{1=2, 3=4}\n{1=2}\n"
    );
    // A `HashMap` copy keeps the receiver's BUCKET order, not insertion order.
    assert_eq!(
        stdout("println(hashMapOf(3 to 1, 1 to 2).toMap())"),
        "{1=2, 3=1}\n"
    );
}

/// `startsWith(prefix, startIndex)` REFUSES an offset outside the string rather
/// than clamping it into range.
///
/// `java.lang.String.startsWith` — the exact-case arm Kotlin delegates to —
/// opens with `if (toffset < 0 || toffset > length() - prefix.length()) return
/// false`, and `regionMatchesImpl`, the `ignoreCase` arm, tests the same bound.
/// kotlinrs clamped the offset with `.max(0)` and never checked the upper end,
/// so `"".startsWith("", 1)` answered true where Kotlin answers false — an
/// empty prefix matched an empty string one character past its end. Found by
/// `parity-fuzz` (seed 24334); every value below was measured on kotlinc.
#[test]
fn starts_with_refuses_an_offset_outside_the_string() {
    // The offset is one past the last position an empty prefix can start at.
    assert_eq!(stdout(r#"println("".startsWith("", 1))"#), "false\n");
    assert_eq!(stdout(r#"println("".startsWith("", 0))"#), "true\n");
    assert_eq!(stdout(r#"println("abc".startsWith("", 3))"#), "true\n");
    assert_eq!(stdout(r#"println("abc".startsWith("", 4))"#), "false\n");
    // A negative offset is false, not treated as 0 — which is what the clamp
    // turned it into.
    assert_eq!(stdout(r#"println("abc".startsWith("a", -1))"#), "false\n");
    assert_eq!(stdout(r#"println("abc".startsWith("a", 0))"#), "true\n");
    // A prefix longer than what is left cannot match at any offset.
    assert_eq!(stdout(r#"println("abc".startsWith("abcd", 0))"#), "false\n");
    assert_eq!(stdout(r#"println("abc".startsWith("bc", 2))"#), "false\n");
    assert_eq!(stdout(r#"println("abc".startsWith("bc", 1))"#), "true\n");
    // The `ignoreCase` arm carries the same bound, so a folded compare cannot
    // reach a position the exact one refuses.
    assert_eq!(
        stdout(r#"println("abc".startsWith("A", -1, true))"#),
        "false\n"
    );
    assert_eq!(
        stdout(r#"println("abc".startsWith("", 4, true))"#),
        "false\n"
    );
    assert_eq!(
        stdout(r#"println("abc".startsWith("A", 0, true))"#),
        "true\n"
    );
}

// ── nested class declarations ────────────────────────────────────────────────
// A class declared inside another class's body is hoisted to the top level
// under the JVM's own binary name, `Owner$Nested`. The parity corpus pins the
// observable output of the shapes against the reference toolchain; what is
// pinned here is the NAME MACHINERY those records cannot separate — which
// spelling reaches the class, which one it prints under, and the cases the
// hoist deliberately refuses.

#[test]
fn a_nested_class_prints_its_simple_name_from_data_and_its_binary_name_otherwise() {
    // The two forms disagree on the JVM and the split is not cosmetic: the
    // generated `toString` of a data class names the class simply, while the
    // identity and throwable forms name it as the class loader does.
    assert_eq!(
        stdout("class O { data class D(val v: Int); class P }; println(O.D(1)); println(O.P().toString().substringBefore(\"@\"))"),
        "D(v=1)\nO$P\n"
    );
    assert_eq!(
        stdout(
            "class O { class Boom(m: String) : RuntimeException(m) }\n\
             try { throw O.Boom(\"bad\") } catch (e: O.Boom) { println(e) }"
        ),
        "O$Boom: bad\n"
    );
}

#[test]
fn a_nested_class_is_reached_by_both_its_qualified_and_its_bare_spelling() {
    // Qualified from outside the owner, bare from inside it — and the two must
    // land on ONE class, or `is` would answer false for a value the other
    // spelling built.
    assert_eq!(
        stdout(
            "class O {\n\
               data class D(val v: Int)\n\
               companion object { fun make(v: Int) = D(v) }\n\
             }\n\
             val d = O.make(3)\n\
             println(d)\n\
             println(d is O.D)\n\
             println(O.D(3) == d)"
        ),
        "D(v=3)\ntrue\ntrue\n"
    );
}

#[test]
fn a_bare_nested_name_two_owners_share_resolves_by_the_enclosing_class() {
    // Two owners nesting the same simple name is legal Kotlin, and inside each
    // owner the bare name means THAT owner's. The program-wide alias cannot
    // decide it; only the enclosing declaration can, so the bare lookup walks
    // outward from the class being lowered before consulting the alias.
    assert_eq!(
        stdout(
            "class A {\n\
               data class Dup(val v: Int)\n\
               companion object { fun make() = Dup(1) }\n\
             }\n\
             class B {\n\
               data class Dup(val v: Int, val w: Int)\n\
               companion object { fun make() = Dup(2, 3) }\n\
             }\n\
             println(A.make())\n\
             println(B.make())\n\
             println(A.make() is A.Dup)\n\
             println(B.make() is B.Dup)"
        ),
        "Dup(v=1)\nDup(v=2, w=3)\ntrue\ntrue\n"
    );
    // From outside either owner the bare name says nothing, and the qualified
    // spelling is the only one that does.
    let out = eval("class A { class Dup }; class B { class Dup }; println(Dup())");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unresolved reference"), "stderr was: {err}");
}

#[test]
fn a_nested_class_name_is_only_a_qualifier_until_it_is_called() {
    // `Reg.Kind` names a type; `Reg.Kind.A` reads a member THROUGH it. Reading
    // the qualifier must not construct anything, which is the bug the enum form
    // exposes first because its constants live on a companion.
    assert_eq!(
        stdout(
            "class Reg { enum class Kind { A, B }; object Cfg { val n = 4 } }\n\
             println(Reg.Kind.A)\n\
             println(Reg.Kind.valueOf(\"B\").ordinal)\n\
             println(Reg.Cfg.n)\n\
             println(Reg.Kind.values().size)"
        ),
        "A\n1\n4\n2\n"
    );
}

#[test]
fn an_inner_class_reads_and_writes_its_enclosing_instance() {
    // `inner` carries a reference to the enclosing INSTANCE: each `In` sees
    // the `Outer` it was built on, not some other one, and a write through it
    // lands on that instance. Two instances keep the receivers apart. Output
    // captured from kotlinc 2.4.20 / JRE 21.
    let src = r#"
class Outer(val name: String) {
    var count = 0
    fun bump() = ++count
    inner class In(val k: Int) {
        val tag = "$name:$k"
        init { count += 10 }
        fun g() = "$name/$k/$tag"
        fun touch() { count += k; bump() }
        fun me() = this@Outer.name + this@In.k
        inner class Deep { fun d() = "$name-$k-$count" }
    }
    fun make(k: Int) = In(k)
}
fun main() {
    val a = Outer("a"); val b = Outer("b")
    val i = a.In(3)
    println(i.g()); println(b.make(5).g())
    i.touch(); println(a.count); println(b.count)
    println(i.me()); println(i.Deep().d())
}"#;
    assert_eq!(stdout(src), "a/3/a:3\nb/5/b:5\n14\n10\na3\na-3-14\n");
    // A non-`inner` nested class has no enclosing instance to read through.
    let out = eval(
        "class O { val v = 1; class N { fun get() = v } }\nfun main() { println(O.N().get()) }",
    );
    assert!(!out.status.success());
}

#[test]
fn nesting_deeper_than_one_level_keeps_qualifying() {
    assert_eq!(
        stdout(
            "class A { class B { data class C(val k: Int) } }\n\
             val c: A.B.C = A.B.C(7)\n\
             println(c)\n\
             println(c is A.B.C)"
        ),
        "C(k=7)\ntrue\n"
    );
}

#[test]
fn logarithms_and_exp_are_fdlibm_bit_for_bit() {
    // `java.lang.Math.log`/`exp` on the reference JVM answer fdlibm's
    // `StrictMath` bits, which differ from a correctly rounded `ln` in the
    // last place: `exp(1.0)` is `2.7182818284590455`, not `…045`, and
    // `log10(1.0 / 7.0)` is `-0.8450980400142569`, where Rust's `f64::log10`
    // answers `…568`. Measured on kotlinc 2.4.20 / JDK 21.
    assert_eq!(
        prog(
            "import kotlin.math.*\n\
             fun main() {\n\
             println(ln(0.0))\n\
             println(exp(1.0))\n\
             println(log10(1000.0))\n\
             println(log10(1.0 / 7.0))\n\
             println(log2(10.0))\n\
             println(ln1p(1e-10))\n\
             println(log(8.0, 2.0))\n\
             println(log(8.0, 1.0))\n\
             println(ln(2.0f))\n\
             println(Math.log(10.0) + Math.log1p(0.5))\n\
             var s = 0.0\n\
             for (i in 1..2000) s += ln(i.toDouble()) + log10(i * 1.7) + exp(i / 300.0)\n\
             println(s)\n}"
        ),
        "-Infinity\n2.7182818284590455\n3.0\n-0.8450980400142569\n3.3219280948873626\n9.999999999500001E-11\n3.0\nNaN\n0.6931472\n2.7080502011022105\n255227.1451013387\n"
    );
}

#[test]
fn lateinit_properties_raise_until_written() {
    // A `lateinit var` holds nothing until its first write, and every read
    // before that — through the instance, inside a member, or in an `init`
    // block — is `kotlin.UninitializedPropertyAccessException` with Kotlin's
    // wording. `this::s.isInitialized` reads without raising. Measured on
    // kotlinc 2.4.20 / JDK 21.
    assert_eq!(
        prog(
            "class C {\n\
                 lateinit var s: String\n\
                 lateinit var xs: MutableList<Int>\n\
                 fun ready() = this::s.isInitialized\n\
                 fun show() = \"s=$s\"\n\
             }\n\
             class E { lateinit var s: String; init { println(s.length) } }\n\
             class D { lateinit var s: String; init { s = \"set in init\" } }\n\
             fun main() {\n\
                 val c = C()\n\
                 println(c.ready())\n\
                 try { println(c.s) } catch (e: UninitializedPropertyAccessException) { println(e.message) }\n\
                 try { println(c.show()) } catch (e: RuntimeException) { println(\"RT \" + e) }\n\
                 c.s = \"x\"; println(c.s + c.ready() + c.show())\n\
                 c.xs = mutableListOf(1); c.xs.add(2); println(c.xs)\n\
                 try { E() } catch (e: Exception) { println(e.message) }\n\
                 println(D().s)\n\
             }"
        ),
        "false\n\
         lateinit property s has not been initialized\n\
         RT kotlin.UninitializedPropertyAccessException: lateinit property s has not been initialized\n\
         xtrues=x\n\
         [1, 2]\n\
         lateinit property s has not been initialized\n\
         set in init\n"
    );
    // Kotlin refuses the modifier on a `val`, a nullable or primitive type, and
    // a property with an initializer.
    for bad in [
        "class C { lateinit val s: String }",
        "class C { lateinit var s: String? }",
        "class C { lateinit var n: Int }",
        "class C { lateinit var s: String = \"\" }",
    ] {
        let err = prog_err(&format!("{bad}\nfun main() {{ println(1) }}"));
        assert!(err.contains("lateinit"), "{bad}: stderr was: {err}");
    }
}

#[test]
fn lateinit_top_level_and_local_raise_until_written() {
    // `lateinit` on a top-level property and on a local, the two positions
    // besides a class body Kotlin allows it in. A read before the first write
    // raises from a function, a lambda, and a local `fun` alike, and
    // `::top.isInitialized` asks the top-level one. Measured on kotlinc 2.4.20.
    assert_eq!(
        prog(
            "lateinit var top: String\n\
             lateinit var names: MutableList<String>\n\
             fun setTop() { top = \"set\" }\n\
             fun main() {\n\
                 lateinit var loc: String\n\
                 try { println(top) } catch (e: UninitializedPropertyAccessException) { println(e.message) }\n\
                 println(::top.isInitialized)\n\
                 setTop(); println(top); println(::top.isInitialized)\n\
                 val r = { try { loc.length } catch (e: UninitializedPropertyAccessException) { -1 } }\n\
                 fun peek(): String = try { loc } catch (e: UninitializedPropertyAccessException) { \"unset\" }\n\
                 println(r()); println(peek())\n\
                 loc = \"abc\"\n\
                 println(r()); println(peek() + top)\n\
                 names = mutableListOf(\"a\"); names.add(\"b\"); println(names)\n\
             }"
        ),
        "lateinit property top has not been initialized\n\
         false\n\
         set\n\
         true\n\
         -1\n\
         unset\n\
         3\n\
         abcset\n\
         [a, b]\n"
    );
    let err = prog_err("fun main() { lateinit var loc: String; println(loc) }");
    assert!(
        err.contains(
            "UninitializedPropertyAccessException: lateinit property loc has not been initialized"
        ),
        "stderr was: {err}"
    );
    for bad in [
        "fun main() { lateinit val s: String; println(1) }",
        "fun main() { lateinit var n: Int; println(1) }",
        "lateinit var s: String? \nfun main() { println(1) }",
    ] {
        let err = prog_err(bad);
        assert!(err.contains("lateinit"), "{bad}: stderr was: {err}");
    }
}

#[test]
fn regex_matches_finds_replaces_and_splits_like_java_util_regex() {
    // `kotlin.text.Regex` over `java.util.regex` semantics: `Matcher.find`'s
    // empty-match iteration, `matches()` backtracking into a longer
    // alternative, `appendReplacement`'s group references, look-behind and
    // back-references, and Kotlin's own `split` (trailing empties kept).
    // Measured on kotlinc 2.4.20.
    assert_eq!(
        prog(
            "fun main() {\n\
                 println(Regex(\"[0-9]+\").findAll(\"a1b22c\").map { it.value }.toList())\n\
                 println(\"hello\".replace(Regex(\"l+\"), \"L\"))\n\
                 println(\"a1b2\".split(Regex(\"[0-9]\")))\n\
                 val r = Regex(\"(\\\\w+)@(\\\\w+)\\\\.com\")\n\
                 val m = r.find(\"mail bob@site.com and amy@web.com\")\n\
                 println(m?.value); println(m?.range); println(m?.groupValues)\n\
                 println(m?.next()?.value); println(m?.next()?.next())\n\
                 println(r.matches(\"x@y.com\")); println(r.matches(\" x@y.com\"))\n\
                 println(r.replace(\"bob@site.com, amy@web.com\", \"$2:$1\"))\n\
                 println(r.replace(\"bob@site.com, amy@web.com\") { it.groupValues[1].uppercase() })\n\
                 println(Regex(\"a*\").findAll(\"baaa\").map { it.range }.toList())\n\
                 println(\"abc\".split(Regex(\"\")))\n\
                 println(\"a,b,c\".split(Regex(\",\"), 2))\n\
                 println(Regex(\"a|ab\").matchEntire(\"ab\")?.value)\n\
                 println(Regex(\"(a)(x)?\").find(\"a\")?.groups)\n\
                 val (user, host) = r.find(\"q@w.com\")!!.destructured\n\
                 println(\"$user $host\")\n\
                 println(Regex(\"(?<y>\\\\d{4})-(?<m>\\\\d\\\\d)\").replace(\"2024-05\", \"\\${m}/\\${y}\"))\n\
                 println(Regex(\"(?<=a)b\").findAll(\"abab cb\").count())\n\
                 println(Regex(\"(\\\\w)\\\\1\").findAll(\"aabbcd\").map { it.value }.toList())\n\
                 println(Regex(\"ö\").find(\"héllo wörld\")?.range)\n\
                 println(\"A1b2\".contains(\"[0-9]\".toRegex()))\n\
                 try { Regex(\"a\").find(\"abc\", 9) } catch (e: IndexOutOfBoundsException) { println(e.message) }\n\
                 try { Regex(\"a\").findAll(\"abc\", 9) } catch (e: IndexOutOfBoundsException) { println(e.message) }\n\
                 try { Regex(\"(a\") } catch (e: IllegalArgumentException) { println(\"bad pattern\") }\n\
                 for (rep in listOf(\"$\", \"\\${-}\", \"\\${1a}\", \"\\$x\", \"$9\")) {\n\
                     try { println(Regex(\"(?<g>a)\").replace(\"ab\", rep)) } catch (e: Exception) { println(e.message) }\n\
                 }\n\
                 println(Regex(\"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)\").replace(\"abcdefghijk\", \"$11-$10-$1\"))\n\
             }"
        ),
        "[1, 22]\n\
         heLo\n\
         [a, b, ]\n\
         bob@site.com\n\
         5..16\n\
         [bob@site.com, bob, site]\n\
         amy@web.com\n\
         null\n\
         true\n\
         false\n\
         site:bob, web:amy\n\
         BOB, AMY\n\
         [0..-1, 1..3, 4..3]\n\
         [, a, b, c, ]\n\
         [a, b,c]\n\
         ab\n\
         [MatchGroup(value=a, range=0..0), MatchGroup(value=a, range=0..0), null]\n\
         q w\n\
         05/2024\n\
         2\n\
         [aa, bb]\n\
         7..7\n\
         true\n\
         Illegal start index\n\
         Start index out of bounds: 9, input length: 3\n\
         bad pattern\n\
         Illegal group reference: group index is missing\n\
         named capturing group has 0 length name\n\
         capturing group name {1a} starts with digit character\n\
         Illegal group reference\n\
         No group 9\n\
         k-j-a\n"
    );
}

#[test]
fn match_groups_resolve_a_group_name() {
    // `groups["name"]` is the named group, not whatever index a `String`
    // coerces to (it answered the whole match); an undeclared name is
    // `Matcher`'s `IllegalArgumentException`. Measured on kotlinc 2.4.20.
    assert_eq!(
        prog(
            "fun main() {\n\
                 val m = Regex(\"a(?<y>\\\\d+)(?<z>x)?\").find(\"a7\")!!\n\
                 println(m.groups[\"y\"]?.value)\n\
                 println(m.groups.get(\"y\")?.range)\n\
                 println(m.groups[\"z\"])\n\
                 println(m.groups[1]?.value)\n\
                 try { m.groups[\"nope\"] } catch (e: IllegalArgumentException) { println(e.message) }\n\
             }"
        ),
        "7\n1..1\nnull\n7\nNo group with name <nope>\n"
    );
}

#[test]
fn a_sorted_collection_built_with_a_comparator_orders_and_dedups_by_it() {
    // `sortedSetOf`/`sortedMapOf`/`TreeSet`/`TreeMap` take a leading
    // `Comparator`, and the collection then orders AND identifies its elements
    // by it: two keys it calls equal are one key (the first spelling stays, a
    // map takes the later value), and `contains`/`get`/`remove` find elements
    // through it rather than `equals`. Measured on kotlinc 2.4.20.
    assert_eq!(
        prog(
            "import java.util.TreeSet\n\
             import java.util.TreeMap\n\
             fun main() {\n\
                 println(sortedSetOf(String.CASE_INSENSITIVE_ORDER, \"b\", \"B\", \"a\"))\n\
                 println(sortedMapOf(String.CASE_INSENSITIVE_ORDER, \"b\" to 1, \"B\" to 2))\n\
                 val s = sortedSetOf(compareByDescending<Int> { it }, 3, 1, 2)\n\
                 s.add(5); s.add(0); s.add(3); println(s)\n\
                 println(s.contains(2)); println(2 in s); println(s.first()); println(s.last())\n\
                 val m = sortedMapOf(compareByDescending<String> { it }, \"a\" to 1, \"c\" to 3, \"b\" to 2)\n\
                 m[\"d\"] = 4; println(m); println(m[\"c\"]); println(m.keys)\n\
                 val t = TreeSet<String>(compareBy { it.length })\n\
                 t.add(\"ccc\"); t.add(\"a\"); t.add(\"bb\"); t.add(\"dd\"); println(t)\n\
                 val ci = sortedSetOf(compareBy<String> { it.lowercase() }, \"X\", \"y\", \"x\")\n\
                 println(ci); println(ci.contains(\"Y\")); ci.remove(\"x\"); println(ci)\n\
                 val tm = TreeMap<String, Int>(String.CASE_INSENSITIVE_ORDER)\n\
                 tm[\"Key\"] = 1; tm[\"KEY\"] = 2; tm[\"a\"] = 0\n\
                 println(tm); println(tm[\"key\"]); println(tm.containsKey(\"kEy\"))\n\
                 println(sortedSetOf(3, 1, 2))\n\
             }"
        ),
        "[a, b]\n\
         {b=2}\n\
         [5, 3, 2, 1, 0]\n\
         true\n\
         true\n\
         5\n\
         0\n\
         {d=4, c=3, b=2, a=1}\n\
         3\n\
         [d, c, b, a]\n\
         [a, bb, ccc]\n\
         [X, y]\n\
         true\n\
         [y]\n\
         {a=0, Key=2}\n\
         2\n\
         true\n\
         [1, 2, 3]\n"
    );
}

#[test]
fn a_captured_var_modified_after_capture_is_shared() {
    // Kotlin wraps a captured `var` that is modified anywhere in a `Ref`, so a
    // lambda or local `fun` that only READS it still sees a later write from
    // the declaring frame. A `for` variable is fresh per iteration and is not
    // shared. Measured on kotlinc 2.4.20.
    assert_eq!(
        prog(
            "fun main() {\n\
                 var x = 1\n\
                 val f = { x * 10 }\n\
                 x = 2\n\
                 println(f())\n\
                 var n = 0\n\
                 fun show() = \"n=$n\"\n\
                 n++\n\
                 println(show())\n\
                 val fs = mutableListOf<() -> Int>()\n\
                 var k = 0\n\
                 while (k < 3) { fs.add { k }; k++ }\n\
                 println(fs.map { it() })\n\
                 for (i in 0 until 3) { fs.add { i } }\n\
                 println(fs.map { it() })\n\
             }"
        ),
        "20\nn=1\n[3, 3, 3]\n[3, 3, 3, 0, 1, 2]\n"
    );
}

#[test]
fn descending_sorts_are_stable_and_in_place_sorts_use_compare_to() {
    // `sortedDescending()`/`sortDescending()` are `sortWith(reverseOrder())`:
    // stable, so elements that compare equal keep their order — a sort
    // followed by `reverse()` flipped them. The in-place `sort()` orders a
    // `Comparable` by its own `compareTo`, and comparing inside the heap borrow
    // used to panic. `sortedArray()` answers an array of the receiver's kind.
    // Measured on kotlinc 2.4.20 / JDK 21.
    assert_eq!(
        prog(
            "data class P(val k: Int, val t: String) : Comparable<P> {\n\
                 override fun compareTo(other: P) = k.compareTo(other.k)\n\
             }\n\
             fun main() {\n\
                 val l = listOf(P(1, \"a\"), P(2, \"b\"), P(1, \"c\"), P(2, \"d\"))\n\
                 println(l.sortedDescending())\n\
                 val m = l.toMutableList(); m.sort(); println(m)\n\
                 m.sortDescending(); println(m)\n\
                 println(arrayOf(P(1, \"a\"), P(2, \"b\"), P(1, \"c\")).sortedArrayDescending().toList())\n\
                 println(arrayOf(3, 1, 2).sortedArray().toList() + intArrayOf(5, 4).sortedArray().toList())\n\
                 println(doubleArrayOf(2.0, -1.0).sortedArrayDescending().contentToString())\n\
                 val e = arrayOf<Int>(); println(e.sortedArray() === e)\n\
             }"
        ),
        "[P(k=2, t=b), P(k=2, t=d), P(k=1, t=a), P(k=1, t=c)]\n\
         [P(k=1, t=a), P(k=1, t=c), P(k=2, t=b), P(k=2, t=d)]\n\
         [P(k=2, t=b), P(k=2, t=d), P(k=1, t=a), P(k=1, t=c)]\n\
         [P(k=2, t=b), P(k=1, t=a), P(k=1, t=c)]\n\
         [1, 2, 3, 4, 5]\n\
         [2.0, -1.0]\n\
         true\n"
    );
}

#[test]
fn a_collection_result_of_an_untracked_receiver_keeps_plus_as_a_collection_op() {
    // `arr.toList()` names a `List` by the stdlib signature even where the
    // array's own type went untracked; its `+` is the collection convention,
    // not the numeric add that answered `0.0`.
    assert_eq!(
        stdout("println(arrayOf(3, 1, 2).toList() + listOf(9)); val m = mutableListOf(1); println(m.toList() + 5)"),
        "[3, 1, 2, 9]\n[1, 5]\n"
    );
}

#[test]
fn mutable_map_remapping_defaults() {
    // `merge`/`compute`/`computeIfAbsent`/`computeIfPresent` are the JDK's
    // `Map` defaults: a null answer removes the entry, and each call answers the
    // stored value. Measured on kotlinc 2.4.20 / JDK 21.
    assert_eq!(
        stdout(
            "val m = mutableMapOf(\"a\" to 1)\n\
             m.compute(\"a\") { _, v -> (v ?: 0) + 10 }\n\
             m.computeIfAbsent(\"z\") { it.length }\n\
             m.computeIfPresent(\"a\") { _, v -> v * 2 }\n\
             println(m)\n\
             val h = hashMapOf(\"x\" to 1, \"y\" to 2)\n\
             h.merge(\"x\", 3) { a, b -> a + b }\n\
             h.merge(\"q\", 4, Int::plus)\n\
             h.merge(\"y\", 0) { _, _ -> null }\n\
             println(h)\n\
             println(h.compute(\"x\") { _, _ -> null })\n\
             println(h)\n\
             println(h.computeIfAbsent(\"q\") { 99 })\n\
             println(h.computeIfPresent(\"nope\") { _, _ -> 1 })"
        ),
        "{a=22, z=1}\n{x=4, q=4}\nnull\n{q=4}\n4\nnull\n"
    );
}

#[test]
fn case_insensitive_order_compare_and_max_with() {
    // `String.CASE_INSENSITIVE_ORDER` is `compareToIgnoreCase`; a comparator
    // value answers `compare(a, b)`; `maxWith`/`minWith` keep the FIRST of a
    // tied run and fault on an empty receiver. Measured on kotlinc 2.4.20 /
    // JDK 21.
    assert_eq!(
        stdout(
            "println(listOf(\"b\", \"A\", \"c\", \"_\", \"a\").sortedWith(String.CASE_INSENSITIVE_ORDER))\n\
             val ci = String.CASE_INSENSITIVE_ORDER\n\
             println(ci.compare(\"x\", \"X\"))\n\
             println(compareBy<String> { it.length }.compare(\"aa\", \"b\"))\n\
             println(listOf(\"b\", \"C\", \"a\").maxWith(String.CASE_INSENSITIVE_ORDER))\n\
             println(listOf(10, 10, 3).minWith(compareBy { it % 3 }))\n\
             try { listOf<Int>().maxWith(compareBy { it }) } catch (e: NoSuchElementException) { println(\"NSE \" + e.message) }"
        ),
        "[_, A, a, b, c]\n0\n1\nC\n3\nNSE null\n"
    );
}

#[test]
fn seeded_random_draws_the_stdlib_xorwow_sequence() {
    // `Random(seed)` is the stdlib's `XorWowRandom`, so a seeded program draws
    // exactly the reference's numbers — through every `next…` member, the
    // power-of-two and rejection-sampled `nextInt` paths, `shuffled`/`shuffle`
    // (the descending Fisher–Yates walk), and `random()` on each receiver
    // kind, with each kind's empty-receiver message. Measured on kotlinc
    // 2.4.20 / JDK 21.
    assert_eq!(
        prog(
            r#"
import kotlin.random.Random
fun main() {
    val r = Random(42)
    println(r.nextInt())
    println(r.nextInt(100))
    println(r.nextInt(5, 10))
    println(r.nextInt(-2000000000, 2000000000))
    println(r.nextLong())
    println(r.nextLong(1000L))
    println(r.nextLong(1L shl 40))
    println(r.nextDouble())
    println(r.nextDouble(5.0))
    println(r.nextDouble(-1.0, 1.0))
    println(r.nextFloat())
    println(r.nextBoolean())
    println(r.nextBits(7))
    println(listOf(1, 2, 3, 4, 5).shuffled(Random(7)))
    val m = mutableListOf("a", "b", "c", "d"); m.shuffle(Random(3)); println(m)
    val a = arrayOf(1, 2, 3, 4); a.shuffle(Random(3)); println(a.toList())
    println(listOf("x", "y", "z").random(Random(11)))
    println((1..100).random(Random(5)))
    println("hello".random(Random(9)))
    println(setOf(3, 4).random(Random(1)))
    println(listOf<Int>().randomOrNull(Random(1)))
    try { listOf<Int>().random(Random(1)) } catch (e: NoSuchElementException) { println(e.message) }
    try { (5..1).random(Random(1)) } catch (e: NoSuchElementException) { println(e.message) }
    try { Random(1).nextInt(5, 5) } catch (e: IllegalArgumentException) { println(e.message) }
    val big = Random(-123456789012345L)
    println(big.nextInt(1000))
    println(kotlin.random.Random(99).nextInt(10))
    println(Random(0).nextInt(1 shl 20))
}
"#
        ),
        "972016666\n40\n6\n-112774692\n4994053466947955232\n508\n202138095686\n0.5926209788406316\n1.471767405343729\n-0.7237029688720977\n0.5930778\nfalse\n10\n[5, 3, 4, 1, 2]\n[d, b, a, c]\n[4, 2, 1, 3]\nz\n42\nl\n3\nnull\nCollection is empty.\nCannot get random in empty range: 5..1\nRandom range is empty: [5, 5).\n149\n7\n576332\n"
    );
    // `kotlin.random` is not auto-imported.
    let err = prog_err("fun main() { println(Random(1).nextInt()) }");
    assert!(
        err.contains("unresolved reference: Random"),
        "stderr was: {err}"
    );
}

#[test]
fn unbound_references_to_one_argument_builtin_members() {
    // `Int::plus` is a two-parameter function (receiver, argument). Lowered as
    // a one-parameter member access it answered the receiver and dropped the
    // argument, so `reduce(Int::plus)` answered the first element and
    // `merge(k, v, Int::plus)` never combined. Measured on kotlinc 2.4.20 /
    // JDK 21.
    assert_eq!(
        stdout(
            "println(listOf(1, 2, 3).reduce(Int::times))\n\
             println(listOf(1, 2).fold(10, Int::plus))\n\
             val m = mutableMapOf(\"a\" to 1); m.merge(\"a\", 5, Int::plus); println(m)\n\
             println(listOf(\"b\", \"a\").sortedWith(String::compareTo))\n\
             println(listOf(2.5, 1.0).reduce(Double::plus))\n\
             println(listOf(\"ab\", \"c\").fold(\"\", String::plus))\n\
             println(listOf(5, 3).reduce(Int::coerceAtLeast))\n\
             println(listOf(12, 5).reduce(Int::rem))"
        ),
        "6\n13\n{a=6}\n[a, b]\n3.5\nabc\n5\n2\n"
    );
}

#[test]
fn a_default_is_evaluated_in_the_callees_frame() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12. A default naming an earlier
    // parameter, `this` or a member is not evaluable where the call is written.
    assert_eq!(
        stdout(
            "class K(val n: Int) { fun m(p: Int = n + 1, q: Int = p * n) = \"$p,$q\" }\n\
             open class B(val n: Int) { open fun f(x: Int = n + 10) = \"b\" + x }\n\
             class S(n: Int) : B(n) { override fun f(x: Int) = \"s\" + x }\n\
             fun a(x: Int, y: Int = x * 2, z: String = \"z\" + y) = \"$x/$y/$z\"\n\
             fun String.e(n: Int = length, t: String = this + n) = t\n\
             fun main() {\n\
             println(a(1)); println(a(3, z = \"k\")); println(a(z = \"q\", x = 4))\n\
             println(K(3).m()); println(K(3).m(q = 2))\n\
             println(S(1).f()); val b: B = S(2); println(b.f(5))\n\
             println(\"abc\".e()); println(\"\".e())\n\
             }"
        ),
        "1/2/z2\n3/6/k\n4/8/q\n4,12\n4,2\ns11\ns5\nabc3\n0\n"
    );
}

#[test]
fn named_arguments_run_in_written_order() {
    assert_eq!(
        stdout(
            "var log = \"\"\n\
             fun t(s: String, v: Int): Int { log += s; return v }\n\
             fun f(a: Int, b: Int, c: Int) = \"$a$b$c\"\n\
             fun main() {\n\
             println(f(c = t(\"c\", 1), a = t(\"a\", 2), b = t(\"b\", 3)) + \"|\" + log)\n\
             log = \"\"\n\
             println(f(t(\"a\", 1), c = t(\"c\", 2), b = t(\"b\", 3)) + \"|\" + log)\n\
             }"
        ),
        "231|cab\n132|acb\n"
    );
}

#[test]
fn initializers_may_call_members_and_reach_overrides() {
    // A superclass initializer's virtual call lands on the subclass override,
    // which reads the subclass's own field at its zero value: the JVM allocates
    // the whole object before any constructor runs.
    assert_eq!(
        stdout(
            "open class A { init { println(\"A \" + hello()) }; open fun hello() = \"A\" }\n\
             class B : A() { val own = 7; override fun hello() = \"B\" + own; init { println(\"B \" + hello()) } }\n\
             class C(val n: Int) { val d = twice(n); init { println(\"C \" + d + \" \" + me()) }\n\
             fun twice(x: Int) = x * 2; fun me() = \"<\" + n + \">\" }\n\
             fun main() { B(); C(4) }"
        ),
        "A B0\nB B7\nC 8 <4>\n"
    );
}

#[test]
fn an_object_initializes_on_first_use() {
    assert_eq!(
        stdout(
            "object O { init { println(\"O init\") }; val v = 5 }\n\
             fun main() { println(\"start\"); println(O.v); println(O.v) }"
        ),
        "start\nO init\n5\n5\n"
    );
}

#[test]
fn a_function_type_with_a_receiver_binds_this() {
    assert_eq!(
        stdout(
            "class H { val s = StringBuilder(); fun tag(n: String, f: H.() -> Unit) { s.append(\"<\" + n + \">\"); f(); s.append(\"</\" + n + \">\") }\n\
             fun text(t: String) { s.append(t) } }\n\
             fun h(f: H.() -> Unit): String { val x = H(); x.f(); return x.s.toString() }\n\
             fun twice(n: Int, f: Int.(Int) -> Int) = n.f(n)\n\
             fun main() {\n\
             println(h { tag(\"p\") { text(\"hi\"); tag(\"b\") { text(\"x\") } } })\n\
             println(twice(6) { this * it })\n\
             val g: Int.(Int) -> Int = { o -> this - o }\n\
             println(10.g(4)); println(g(10, 4))\n\
             }"
        ),
        "<p>hi<b>x</b></p>\n36\n6\n6\n"
    );
}

#[test]
fn return_break_and_continue_are_expressions() {
    assert_eq!(
        stdout(
            "fun len(s: String?): Int { val n = s ?: return -1; return n.length }\n\
             fun sum(xs: List<String?>): Int { var t = 0; for (x in xs) { t += x?.toIntOrNull() ?: continue }; return t }\n\
             fun upTo(xs: List<Int?>): Int { var t = 0; for (x in xs) { t += x ?: break }; return t }\n\
             fun main() {\n\
             println(len(null)); println(len(\"abc\"))\n\
             println(sum(listOf(\"1\", \"x\", null, \"4\")))\n\
             println(upTo(listOf(1, 2, null, 4)))\n\
             }"
        ),
        "-1\n3\n5\n3\n"
    );
}

#[test]
fn char_classes_come_from_the_jdks_tables() {
    // `²` and `½` are numeric but not decimal digits, `Ⅷ` is a letter-number,
    // `٣` and fullwidth `７` ARE digits, and `ᵃ` (modifier letter) is a letter.
    assert_eq!(
        stdout(
            "fun main() {\n\
             println(listOf('\\u00b2', '\\u00bd', '\\u2167', '\\u0663', '\\uff17').map { it.isDigit() })\n\
             println(listOf('\\u00b2', '\\u2167', '\\u02b0', '\\u0345').map { it.isLetter() })\n\
             println('\\u0663'.digitToInt() + '\\uff17'.digitToInt())\n\
             println('f'.digitToInt(16)); println('9'.digitToIntOrNull(8))\n\
             println(runCatching { '8'.digitToInt(8) }.exceptionOrNull()?.message)\n\
             }"
        ),
        "[false, false, false, true, true]\n[false, false, true, false]\n10\n15\nnull\nChar 8 is not a digit in the given radix=8\n"
    );
}

#[test]
fn indexed_folds_pass_the_position_first() {
    // Expected values measured on kotlinc 2.4.21 / JRE 21. A reduction's first
    // index is 1 (the seed is element 0); a right fold walks from `size - 2`
    // and takes `(index, element, acc)`; `Int` accumulators still wrap.
    assert_eq!(
        stdout(
            "fun main() {\n\
             val l = listOf(5, 3, 8)\n\
             println(l.foldIndexed(0) { i, a, v -> a + i * v })\n\
             println(l.foldRightIndexed(\"\") { i, v, a -> a + \"$i$v\" })\n\
             println(l.reduceIndexed { i, a, v -> a + i * v })\n\
             println(emptyList<Int>().reduceIndexedOrNull { i, a, v -> a + v })\n\
             println(l.reduceRightIndexed { i, v, a -> a - i * v })\n\
             println(l.runningFoldIndexed(1) { i, a, v -> a * v + i }); println(l.scanIndexed(0) { i, a, v -> a + i })\n\
             println(l.runningReduceIndexed { i, a, v -> a + i * v })\n\
             println(l.mapIndexedNotNull { i, v -> if (i > 0) v * 2 else null })\n\
             println(\"abc\".foldIndexed(\"\") { i, a, c -> a + i + c })\n\
             println(sequenceOf(1, 2, 3).mapIndexedNotNull { i, v -> if (i == 1) null else v }.toList())\n\
             println(listOf(Int.MAX_VALUE, 1).foldIndexed(0) { i, a, v -> a + v + i })\n\
             println(runCatching { emptyList<Int>().reduceIndexed { i, a, v -> a + v } }.exceptionOrNull()?.message)\n\
             println(runCatching { emptyArray<String>().reduceIndexed { i, a, v -> a } }.exceptionOrNull()?.message)\n\
             println(runCatching { emptyList<Int>().reduceRightIndexed { i, a, v -> a } }.exceptionOrNull()?.message)\n\
             }"
        ),
        "19\n281305\n24\nnull\n5\n[1, 5, 16, 130]\n[0, 0, 1, 3]\n[5, 8, 24]\n[6, 16]\n0a1b2c\n[1, 3]\n-2147483647\n\
         Empty collection can't be reduced.\nEmpty array can't be reduced.\nEmpty list can't be reduced.\n"
    );
}

#[test]
fn map_for_each_with_two_parameters_gets_key_and_value() {
    // `{ k, v -> }` is `java.util.Map.forEach(BiConsumer)`; `{ (k, v) -> }` and
    // `{ e -> }` are Kotlin's one-parameter entry form. Measured on kotlinc
    // 2.4.21 / JRE 21: before the fix `k` was the whole entry.
    assert_eq!(
        stdout(
            "fun main() {\n\
             val m = linkedMapOf(\"b\" to 2, \"a\" to 1)\n\
             m.forEach { k, v -> print(\"$k:$v \") }; println()\n\
             m.forEach { (k, v) -> print(\"$k=$v \") }; println()\n\
             m.forEach { e -> print(e.key + e.value) }; println()\n\
             val sm = sortedMapOf(3 to \"c\", 1 to \"a\"); sm.forEach { k, v -> print(k * 10); print(v) }; println()\n\
             var total = 0; mapOf(1 to 10, 2 to 20).forEach { k, v -> total += k * v }; println(total)\n\
             }"
        ),
        "b:2 a:1 \nb=2 a=1 \nb2a1\n10a30c\n50\n"
    );
}

#[test]
fn iterator_builder_suspends_between_has_next_and_next() {
    // Measured on kotlinc 2.4.21 / JRE 21. `hasNext` runs the block to its next
    // `yield` exactly once however often it is asked; an endless block is
    // pulled only as far as asked; `for` over an iterator walks what is left.
    assert_eq!(
        stdout(
            "fun main() {\n\
             val it2 = iterator { yield(1); yieldAll(listOf(2, 3)) }\n\
             while (it2.hasNext()) print(it2.next()); println()\n\
             val lazy = iterator { println(\"start\"); yield(\"a\"); println(\"mid\"); yield(\"b\"); println(\"end\") }\n\
             println(\"built\"); println(lazy.hasNext()); println(lazy.hasNext()); println(lazy.next()); println(lazy.next()); println(lazy.hasNext())\n\
             try { lazy.next() } catch (e: NoSuchElementException) { println(\"NSEE ${e.message}\") }\n\
             val nat = iterator { var n = 0; while (true) yield(n++) }\n\
             println(List(5) { nat.next() }); println(nat.next())\n\
             val li = listOf(1, 2, 3, 4).iterator(); li.next()\n\
             for (x in li) print(x); println(); println(li.hasNext())\n\
             val fin = iterator { yield(10); yield(20); yield(30) }; fin.next()\n\
             for (x in fin) print(\"$x \"); println(); println(fin.hasNext())\n\
             }"
        ),
        "123\nbuilt\nstart\ntrue\ntrue\na\nmid\nb\nend\nfalse\nNSEE null\n[0, 1, 2, 3, 4]\n5\n234\nfalse\n20 30 \nfalse\n"
    );
}

#[test]
fn array_type_tests_tell_reference_and_primitive_arrays_apart() {
    // Measured on kotlinc 2.4.21 / JRE 21: each array type is its own class, so
    // a primitive array is not `Array<*>`, and `toTypedArray()` IS an array.
    assert_eq!(
        stdout(
            "fun t(x: Any) = listOf(x is Array<*>, x is IntArray, x is LongArray, x is DoubleArray, x is CharArray, x is ByteArray).joinToString(\"\") { if (it) \"1\" else \"0\" }\n\
             fun j(vararg x: String) = x.joinToString(\"-\")\n\
             fun main() {\n\
             for (a in listOf<Any>(arrayOf(1), Array(2) { it }, intArrayOf(1), longArrayOf(1), doubleArrayOf(1.0), \"ab\".toCharArray(), byteArrayOf(1), listOf(1).toTypedArray())) println(t(a))\n\
             val x: Any = intArrayOf(1)\n\
             println(when (x) { is Array<*> -> \"array\"; is IntArray -> \"ints\"; else -> \"?\" })\n\
             val a = listOf(\"a\", \"b\").toTypedArray(); a[0] = \"z\"; println(a.contentToString()); println(j(*a))\n\
             }"
        ),
        "100000\n100000\n010000\n001000\n000100\n000010\n000001\n100000\nints\n[z, b]\nz-b\n"
    );
}

#[test]
fn yielded_long_keeps_its_width_through_sequence_stages() {
    // Measured on kotlinc 2.4.21 / JRE 21: a `Long` handed to `yield` is a
    // `java.lang.Long`, so `it == 2L` in a later stage is true. Unboxed, the
    // element read as an `Int` and every such comparison was false (an
    // infinite `filter` then ran into the element cap).
    assert_eq!(
        stdout(
            "fun main() {\n\
             val s = sequence { var a = 0L; while (true) { yield(a); a++ } }\n\
             println(s.filter { it % 2L == 0L }.take(3).toList())\n\
             val v = sequence { yield(0L); yield(2L) }\n\
             println(v.filter { it == 2L }.toList())\n\
             println(v.toList().contains(2L))\n\
             println(v.toList() == listOf(0L, 2L))\n\
             val it2 = iterator { yield(5L) }\n\
             println(it2.next() == 5L)\n\
             }"
        ),
        "[0, 2, 4]\n[2]\ntrue\ntrue\ntrue\n"
    );
}

#[test]
fn component_n_past_the_end_of_a_list_or_array_raises_the_get_fault() {
    // Measured on kotlinc 2.4.21 / JRE 21: `List.componentN` is `get(N - 1)`
    // and `Array.componentN` is the array load, so a short receiver raises the
    // same fault `get` does, catchable as an ordinary exception.
    assert_eq!(
        stdout(
            "fun t(f: () -> Unit) { try { f() } catch (e: Exception) { println(\"${e::class.simpleName}: ${e.message}\") } }\n\
             fun main() {\n\
             t { val (a, b) = listOf(1); println(\"$a $b\") }\n\
             t { val (a, b) = arrayOf(1); println(\"$a $b\") }\n\
             t { val (a, b, c) = listOf<Int>() }\n\
             t { val (a, b) = mutableListOf(1) }\n\
             t { val (a, b) = intArrayOf(1) }\n\
             t { val (a, b) = listOf(1).sorted() }\n\
             }"
        ),
        "IndexOutOfBoundsException: Index: 1, Size: 1\n\
         ArrayIndexOutOfBoundsException: Index 1 out of bounds for length 1\n\
         IndexOutOfBoundsException: Empty list doesn't contain element at index 0.\n\
         IndexOutOfBoundsException: Index 1 out of bounds for length 1\n\
         ArrayIndexOutOfBoundsException: Index 1 out of bounds for length 1\n\
         IndexOutOfBoundsException: Index: 1, Size: 1\n"
    );
}

#[test]
fn exit_process_ends_the_run_with_its_status_and_skips_finally() {
    // Measured on kotlinc 2.4.21 / JRE 21: `exitProcess(7)` inside `try` ends the
    // JVM with status 7 without running `finally`; `main`'s `args` is an empty
    // array; the `System` statics and `System.out`/`System.err` resolve with no
    // import; and a default-imported package may be spelled out in full.
    let out = eval(
        "import kotlin.system.exitProcess\n\
         import kotlin.system.measureTimeMillis\n\
         fun main(args: Array<String>) {\n\
         println(args.size)\n\
         println(args.isEmpty())\n\
         println(measureTimeMillis { var s = 0L; for (i in 1..100) s += i } >= 0L)\n\
         println(System.currentTimeMillis() > 1_600_000_000_000L)\n\
         println(System.nanoTime() - System.nanoTime() <= 0L)\n\
         println(System.lineSeparator() == \"\\n\")\n\
         println(System.getenv(\"NO_SUCH_VAR_X_Y_Z\") == null)\n\
         System.out.println(\"out \" + 1)\n\
         System.out.print(\"a\"); System.out.print('b'); System.out.println()\n\
         System.err.println(\"to stderr\")\n\
         println(kotlin.collections.listOf(1, 2).map { it * 2 })\n\
         try {\n\
         println(\"in try\")\n\
         if (args.isEmpty()) exitProcess(7)\n\
         } finally {\n\
         println(\"finally must not run\")\n\
         }\n\
         println(\"not reached\")\n\
         }",
    );
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "0\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\nout 1\nab\n[2, 4]\nin try\n"
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "to stderr\n");
}

#[test]
fn system_exit_flushes_pending_output() {
    // Measured on kotlinc 2.4.21 / JRE 21: `System.exit(3)` ends the run with
    // status 3 after a `print` that had not yet seen its newline.
    let out = eval("fun main() { print(\"partial\"); System.exit(3) }");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "partial");
}

#[test]
fn unsuffixed_literal_past_int_range_is_a_long() {
    // Measured on kotlinc 2.4.21 / JRE 21: a decimal, hex or binary literal that does
    // not fit an `Int` is a `Long`, so `i * 3_000_000_000` and `i * 0xFFFFFFFF`
    // widen instead of wrapping at 32 bits. The one exception is `-2147483648`,
    // which Kotlin folds into the `Int` it names.
    assert_eq!(
        stdout(
            "fun k(x: Any) = x.javaClass.simpleName\n\
             fun main() {\n\
                 val i = 7\n\
                 println(i * 3_000_000_000)\n\
                 println(5 * 3_000_000_000)\n\
                 println(i + 2_147_483_648)\n\
                 println(i * 0xFFFFFFFF)\n\
                 println(k(3_000_000_000)); println(k(2147483647)); println(k(2147483648))\n\
                 println(k(-2147483648)); println(-2147483648 - 1)\n\
                 println(k(0x7FFFFFFF)); println(k(0x80000000)); println(0x80000000.toInt())\n\
                 println(k(0b11111111111111111111111111111111))\n\
                 val big = 4_000_000_000\n\
                 println(big + 1); println(big.toInt()); println(big shl 2)\n\
                 for (j in 1..2) println(j * 3_000_000_000)\n\
                 println(i * 3_000_000_000 % 1_000_000_007)\n\
                 println(1_000_000_007 * 1_000_000_007)\n\
             }"
        ),
        "21000000000\n15000000000\n2147483655\n30064771065\nLong\nInteger\nLong\nInteger\n2147483647\nInteger\nLong\n-2147483648\nLong\n4000000001\n-294967296\n16000000000\n3000000000\n6000000000\n999999860\n-371520463\n"
    );
}

#[test]
fn long_keys_look_up_boxed_through_get_getordefault_and_getvalue() {
    // Measured on kotlinc 2.4.21 / JRE 21: a `Long` key handed to `get`,
    // `getOrDefault`, `getValue`, `getOrPut` or `getOrElse` is looked up as a
    // `java.lang.Long`, the shape `m[k] = v` stored it in. Unboxed, `get`
    // answered null and `getValue` threw.
    assert_eq!(
        stdout(
            "fun isL(x: Any?) = if (x is Long) \"L\" else if (x is Int) \"I\" else if (x == null) \"N\" else \"?\"\n\
             class Acc { var t = 0L; fun get(x: Long): Long = x * 2 }\n\
             fun main() {\n\
                 val mm = mutableMapOf<Long, Long>(); mm[1L] = 2L; mm.put(3L, 4L)\n\
                 println(isL(mm.get(1L)))\n\
                 println(isL(mm.getOrDefault(1L, 0L)))\n\
                 println(isL(mm.getOrDefault(7L, 0L)))\n\
                 println(isL(mm.getOrElse(8L) { 7L }))\n\
                 println(isL(mm.getOrPut(9L) { 10L }))\n\
                 println(isL(mm.getValue(1L)))\n\
                 println(mm.getValue(3L))\n\
                 val h = HashMap<Long, Int>(); h[5L] = 1\n\
                 println(h.get(5L)); println(h.getOrDefault(5L, 9)); println(h.getOrDefault(6L, 9))\n\
                 println(Acc().get(4L))\n\
             }"
        ),
        "L\nL\nL\nL\nL\nL\n4\n1\n1\n9\n8\n"
    );
}

#[test]
fn long_width_survives_seeds_equals_and_collection_operators() {
    // Measured on kotlinc 2.4.21 / JRE 21: a `Long` seed, a `Long` element handed to
    // `sequenceOf`, `equals` on either side, and a collection-valued
    // `emptyList<T>()` as the left operand of `+`/`-` all keep their width or
    // their list-ness. A `for` variable over a `Long` bound is a `Long`, and
    // `Math.abs`/`max`/`min` read a `Long` out of a collection.
    assert_eq!(
        stdout(
            "fun isL(x: Any?) = if (x is Long) \"L\" else if (x is Int) \"I\" else if (x == null) \"N\" else \"?\"\n\
             fun main() {\n\
                 println(isL(listOf(1, 2).asSequence().fold(0L) { a, b -> a + b }))\n\
                 println(listOf(1, 2).runningFold(0L) { a, b -> a + b }.map { isL(it) })\n\
                 println(listOf(1, 2).scan(0L) { a, b -> a + b }.filter { it == 3L })\n\
                 println(generateSequence(1L) { if (it < 8) it * 2 else null }.filter { it == 4L }.toList())\n\
                 println(generateSequence(1L) { it * 2 }.take(4).map { isL(it) }.toList())\n\
                 println(sequenceOf(1L, 2L).filter { it == 2L }.toList())\n\
                 println(listOf(2L).filter { it.equals(2L) })\n\
                 val y: Long = 5L\n\
                 println(5L.equals(5L)); println(y.equals(5L)); println(y.equals(5)); println(y.equals(y as Any))\n\
                 println(emptyList<Int>() + 1)\n\
                 println(mutableListOf<Int>() + 1)\n\
                 println(emptySet<Int>() + 1)\n\
                 println(emptyList<Long>() + 1L)\n\
                 println(listOf(1L) - 1L)\n\
                 for (i in 1L..2L) println(i * 2_000_000_000 * 2)\n\
                 println(listOf(1L, -2L).map { Math.abs(it) })\n\
                 println(listOf(1L, 5L).map { Math.max(it, 3L) })\n\
                 println(listOf(1L, 5L).map { Math.min(it, 3L) })\n\
                 println(listOf(1L).map { it.coerceAtMost(3) }.filter { it == 1L })\n\
                 println(listOf(1).map { it.toLong().plus(3) }.map { isL(it) })\n\
             }"
        ),
        "L\n[L, L, L]\n[3]\n[4]\n[L, L, L, L]\n[2]\n[2]\ntrue\ntrue\nfalse\ntrue\n[1]\n[1]\n[1]\n[1]\n[]\n4000000000\n8000000000\n[1, 2]\n[3, 5]\n[1, 3]\n[1]\n[L]\n"
    );
}

#[test]
fn java_deque_members_and_objects_statics() {
    // Measured on kotlinc 2.4.21 / JRE 21: under `import java.util.ArrayDeque` the
    // deque answers `offer`/`push`/`poll`/`pop`/`peek…`/`element` with the JDK's
    // messageless `NoSuchElementException`, and `java.util.Objects` supplies
    // `hash`/`equals`/`isNull`/`nonNull`/`requireNonNull`.
    assert_eq!(
        stdout(
            "import java.util.ArrayDeque\n\
             import java.util.Objects\n\
             data class Pt(val x: Int, val y: String?) {\n\
                 override fun hashCode() = Objects.hash(x, y)\n\
                 override fun equals(other: Any?) = other is Pt && Objects.equals(x, other.x) && Objects.equals(y, other.y)\n\
             }\n\
             fun main() {\n\
                 val dq = ArrayDeque<Int>()\n\
                 println(dq.offer(1)); dq.offerFirst(0); dq.offerLast(2); dq.push(-1)\n\
                 println(dq); println(dq.peek()); println(dq.peekFirst()); println(dq.peekLast())\n\
                 println(dq.poll()); println(dq.pollLast()); println(dq.pop()); println(dq)\n\
                 println(dq.size); println(dq.isEmpty()); println(dq.element())\n\
                 dq.clear()\n\
                 val n1: Int? = dq.poll(); val n2: Int? = dq.peek(); val n3: Int? = dq.pollFirst(); val n4: Int? = dq.peekLast(); val n5: Int? = dq.pollLast(); println(\"$n1 $n2 $n3 $n4 $n5\")\n\
                 try { dq.pop() } catch (e: NoSuchElementException) { println(\"pop: ${e.message}\") }\n\
                 try { dq.element() } catch (e: NoSuchElementException) { println(\"element: ${e.message}\") }\n\
                 val bfs = ArrayDeque<Int>(); bfs.add(1); val seen = mutableSetOf(1)\n\
                 while (bfs.isNotEmpty()) { val n = bfs.poll(); if (n < 20) for (m in listOf(n * 2, n * 3)) if (seen.add(m)) bfs.offer(m) }\n\
                 println(seen.sorted())\n\
                 println(Objects.hash(1, 2, 3)); println(Objects.hash(\"a\", null)); println(Objects.hash()); println(Pt(1, \"a\").hashCode())\n\
                 println(Pt(1, \"a\") == Pt(1, \"a\")); println(Objects.equals(null, null)); println(Objects.equals(\"a\", null))\n\
                 println(Objects.isNull(null)); println(Objects.nonNull(3)); println(Objects.requireNonNull(\"z\"))\n\
                 try { Objects.requireNonNull<String?>(null) } catch (e: NullPointerException) { println(\"npe\") }\n\
             }"
        ),
        "true\n[-1, 0, 1, 2]\n-1\n-1\n2\n-1\n2\n0\n[1]\n1\nfalse\n1\nnull null null null null\npop: null\nelement: null\n[1, 2, 3, 4, 6, 8, 9, 12, 16, 18, 24, 27, 32, 36, 48, 54]\n30817\n3968\n1\n1089\ntrue\ntrue\nfalse\ntrue\ntrue\nz\nnpe\n"
    );
}

#[test]
fn assert_is_a_no_op_that_still_evaluates_its_condition() {
    // Measured on kotlinc 2.4.21 / JRE 21: neither `kotlin` nor `java` passes
    // `-ea`, so `assert(false)` never fires, its condition is still evaluated,
    // and `Throwable.localizedMessage` is `message`.
    assert_eq!(
        stdout(
            "fun main() {\n\
                 val e = IllegalStateException(\"m\"); println(e.localizedMessage)\n\
                 println(RuntimeException().localizedMessage)\n\
                 var n = 0\n\
                 assert(n++ == 0) { \"never built\" }\n\
                 assert(false) { \"asrt\" }\n\
                 assert(false)\n\
                 println(\"assertions are off: $n\")\n\
             }"
        ),
        "m\nnull\nassertions are off: 1\n"
    );
}

#[test]
fn regex_options_fold_into_the_pattern() {
    assert_eq!(
        stdout(
            "println(Regex(\"a\", RegexOption.IGNORE_CASE).matches(\"A\"))\n\
             println(Regex(\"^b$\", setOf(RegexOption.IGNORE_CASE, RegexOption.MULTILINE)).findAll(\"a\\nB\\nc\").count())\n\
             println(\"A.B\".toRegex(RegexOption.LITERAL).containsMatchIn(\"xA.By\"))\n\
             println(\"A.B\".toRegex(RegexOption.LITERAL).containsMatchIn(\"xAxBy\"))\n\
             println(\"a.b\".toRegex(setOf(RegexOption.DOT_MATCHES_ALL)).matches(\"a\\nb\"))"
        ),
        "true\n1\ntrue\nfalse\ntrue\n"
    );
}

#[test]
fn data_class_array_properties_render_and_hash_by_content() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: a `data class`'s generated
    // `toString` renders an array-typed property with `Arrays.toString` and its
    // `hashCode` folds it with `Arrays.hashCode`, while `equals` keeps the
    // array's identity. A property of another declared type holding an array
    // keeps the identity `toString`.
    assert_eq!(
        stdout(
            r#"data class Q(val a: IntArray, val s: Array<String>, val d: DoubleArray, val n: IntArray?)
data class A(val l: LongArray, val f: FloatArray, val c: CharArray, val b: BooleanArray, val bl: Array<Long>, val by: ByteArray, val sh: ShortArray)
data class R(val a: Any, val l: List<IntArray>)
data class V(var x: IntArray) { val y = intArrayOf(9) }
fun main() {
    val x = intArrayOf(1, 2)
    val q1 = Q(x, arrayOf("a"), doubleArrayOf(1.5), null)
    val q2 = Q(intArrayOf(1, 2), arrayOf("a"), doubleArrayOf(1.5), null)
    println(q1)
    println(q1 == q2)
    println(q1 == q1.copy())
    println(q1.hashCode() == q2.hashCode())
    println(Q(intArrayOf(1, 2), arrayOf("a", "b"), doubleArrayOf(), intArrayOf(3)).hashCode())
    val a = A(longArrayOf(3000000000L, -1), floatArrayOf(0.1f, 1f), charArrayOf('x', 'y'), booleanArrayOf(true, false), arrayOf(5000000000L, 2L), byteArrayOf(-1, 2), shortArrayOf(300))
    println(a)
    println(a.hashCode())
    val r = R(intArrayOf(1), listOf(intArrayOf(2)))
    println(r.toString().startsWith("R(a=[I@"))
    println(r.toString().contains("l=[[I@"))
    val v = V(intArrayOf(1, 2))
    v.x = intArrayOf(7)
    println(v)
    println(v.hashCode())
}"#
        ),
        "Q(a=[1, 2], s=[a], d=[1.5], n=null)\nfalse\ntrue\ntrue\n33519745\n\
         A(l=[3000000000, -1], f=[0.1, 1.0], c=[x, y], b=[true, false], bl=[5000000000, 2], by=[-1, 2], sh=[300])\n\
         -1063542991\ntrue\ntrue\nV(x=[7])\n38\n"
    );
}

#[test]
fn named_arguments_run_in_written_order_for_copy_and_stdlib_members() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: arguments evaluate in the
    // order they are WRITTEN, for a `data class`'s `copy` and for a stdlib
    // member's named parameters alike.
    assert_eq!(
        stdout(
            r#"data class D(val a: Int, val b: Int)
fun g(n: Int): Int { println("g$n"); return n }
fun main() {
    println(D(1, 2).copy(b = g(6), a = g(7)))
    println(listOf(1, 2).joinToString(postfix = "!".also { println("pf") }, separator = "-".also { println("sep") }))
}"#
        ),
        "g6\ng7\nD(a=7, b=6)\npf\nsep\n1-2!\n"
    );
}

#[test]
fn a_user_class_extension_applies_only_to_its_own_instances() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. A receiver the frontend
    // cannot type (`3.also { }`, a lambda parameter of `reduce`) used to take
    // the program's sole extension of that name even when the value was an
    // `Int`, and an operator extension was never found for one.
    assert_eq!(
        stdout(
            r#"class Box(var v: Int)
operator fun Box.plus(o: Box) = Box(v + o.v)
fun Box.size() = 99
interface Animal { val n: String }
class Dog(override val n: String) : Animal
fun Animal.greet() = "hi $n"
class Money(val c: Int)
operator fun Money.plus(o: Money) = Money(c + o.c)
fun Money.times(k: Int) = Money(c * k)
fun Money.show() = "$" + c
fun main() {
    println(3.also { }.plus(1))
    val xs: List<Any> = listOf(1, Box(2))
    println(xs.map { if (it is Box) it.size() else it })
    println(5.plus(2))
    println(listOf(Dog("a"), Dog("b")).map { it.greet() })
    println(Money(2).also { }.plus(Money(3)).show())
    println(3.also { }.times(4))
    println(listOf(Money(1), Money(2)).reduce { a, b -> a + b }.show())
    println(listOf(1, 2).reduce { a, b -> a.plus(b) })
    println(mapOf(1 to Money(5)).getValue(1).times(2).show())
    println(mapOf(1 to 5).getValue(1).times(2))
}"#
        ),
        "4\n[1, 99]\n7\n[hi a, hi b]\n$5\n12\n$3\n3\n$10\n10\n"
    );
}

#[test]
fn an_untyped_left_operand_against_a_collection_dispatches_on_the_value() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. The native `Add` coerced the
    // left handle to a number, so these printed `0.0`.
    assert_eq!(
        stdout(
            r#"fun <T> id(x: T) = x
fun main() {
    println(id(listOf(1)) + listOf(2))
    println(id(listOf(1, 2, 3)) - listOf(2))
    println(id(setOf(1)) + setOf(2))
    println(id(mapOf(1 to 2)) + mapOf(3 to 4))
    println(id("s") + listOf(2))
    println(id(mutableListOf(1)) + arrayOf(4).toList())
}"#
        ),
        "[1, 2]\n[1, 3]\n[1, 2]\n{1=2, 3=4}\ns[2]\n[1, 4]\n"
    );
}

#[test]
fn a_read_only_list_behind_a_mutable_list_cast_faults_as_its_jvm_class() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `listOf(1)` is a
    // `SingletonList`, `listOf(1, 2)` and `sorted()` an `Arrays$ArrayList`
    // (`set` works, a size change does not), `buildList` a sealed
    // `ListBuilder`, and an empty one `EmptyList`, which is not a
    // `MutableList` at all. A `toList()` of two or more is a real `ArrayList`,
    // and stays one after a write shrinks it.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
@Suppress("UNCHECKED_CAST")
fun <T> m(l: List<T>) = l as MutableList<T>
fun main() {
    t("one.add") { m(listOf(1)).add(7) }
    t("one.removeAbsent") { m(listOf(1)).remove(9) }
    t("one.reverse") { val l = listOf(1); m(l).reverse(); l }
    t("one.removeIf") { m(listOf(1)).removeIf { it == 9 } }
    t("two.set") { val l = listOf(1, 2); m(l)[0] = 9; l }
    t("two.add") { m(listOf(1, 2)).add(7) }
    t("two.removePresent") { m(listOf(1, 2)).remove(1) }
    t("two.addAllEmpty") { m(listOf(1, 2)).addAll(listOf()) }
    t("two.plusAssign") { val q = m(listOf(1, 2)); q += 4; q }
    t("two.iterRemove") { val it = m(listOf(1, 2)).iterator(); it.next(); it.remove() }
    val a = listOf(1, 2, 3, 4)
    t("two.removeAllLambda") { m(a).removeAll { it % 2 == 1 } }
    println(a)
    t("sorted.add") { m(listOf(3, 1).sorted()).add(1) }
    t("built.sortBy") { m(buildList { add(2); add(1) }).sortBy { it } }
    t("built.removeAbsent") { m(buildList { add(1) }).remove(9) }
    t("empty.cast") { m(listOf<Int>()).add(1) }
    println(listOf<Int>() is MutableList<*>)
    t("toList.shrink") { val l = listOf(1, 2, 3).take(2); m(l).clear(); m(l).add(5); l }
    t("toList1.add") { m(listOf(5).toList()).add(2) }
}"#
        ),
        "one.add: EX java.lang.UnsupportedOperationException\n\
         one.removeAbsent: false\n\
         one.reverse: [1]\n\
         one.removeIf: EX java.lang.UnsupportedOperationException\n\
         two.set: [9, 2]\n\
         two.add: EX java.lang.UnsupportedOperationException\n\
         two.removePresent: EX java.lang.UnsupportedOperationException: remove\n\
         two.addAllEmpty: false\n\
         two.plusAssign: EX java.lang.UnsupportedOperationException\n\
         two.iterRemove: EX java.lang.UnsupportedOperationException: remove\n\
         two.removeAllLambda: EX java.lang.UnsupportedOperationException\n\
         [2, 4, 3, 4]\n\
         sorted.add: EX java.lang.UnsupportedOperationException\n\
         built.sortBy: EX java.lang.UnsupportedOperationException\n\
         built.removeAbsent: EX java.lang.UnsupportedOperationException\n\
         empty.cast: EX java.lang.ClassCastException: kotlin.collections.EmptyList cannot be cast to kotlin.collections.MutableList\n\
         false\n\
         toList.shrink: [5]\n\
         toList1.add: EX java.lang.UnsupportedOperationException\n"
    );
}

#[test]
fn a_sequence_block_entered_mid_expression_keeps_the_operands_below_it() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. The block is a lambda with a
    // `SequenceScope` receiver, and entering it pushed no value for that
    // parameter, so its prologue took the consumer's top operand instead:
    // `listOf(7, 8, s.first())` printed `[7, null, 2]` and a string
    // concatenation lost its left side.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { println("$name: ${f()}") }
fun main() {
    val s = sequence { yield(2) }
    println(listOf(7, 8, s.first()))
    println("x" + iterator { yield(5) }.next())
    t("built") { sequence { yield(1); yieldAll(listOf(2, 3)) }.toList() }
}"#
        ),
        "[7, 8, 2]\nx5\nbuilt: [1, 2, 3]\n"
    );
}

#[test]
fn sequence_operators_and_shuffled_follow_the_sequence_members() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `seq - x` lowered to the
    // numeric `Sub`, and `Sequence.shuffled` draws over a shrinking buffer —
    // not the `Iterable` walk — so the same seed permutes differently.
    assert_eq!(
        stdout(
            r#"import kotlin.random.Random
fun main() {
    println((sequenceOf(1, 2, 3) - 2).toList())
    println((generateSequence(1) { if (it < 3) it + 1 else null } + 9).toList())
    println(sequenceOf(1, 2, 3, 4).shuffled(Random(42)).toList())
    println((1..10).asSequence().shuffled(Random(5)).toList())
    val r = Random(3); println(sequenceOf(1, 2, 3).shuffled(r).toList()); println(r.nextInt(1000))
    println((1..10).shuffled(Random(3)))
}"#
        ),
        "[1, 3]\n[1, 2, 3, 9]\n[1, 4, 2, 3]\n[2, 4, 6, 10, 1, 7, 9, 5, 3, 8]\n[1, 2, 3]\n346\n\
         [3, 2, 10, 9, 6, 5, 4, 8, 7, 1]\n"
    );
}

#[test]
fn to_map_with_a_destination_fills_and_answers_that_map() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: `toMap(destination)` is
    // `destination.apply { putAll(this) }`, so the destination's own class
    // decides the iteration order.
    assert_eq!(
        stdout(
            r#"fun main() {
    println((1..20).associate { "k$it" to it }.toMap(HashMap()).keys.take(8))
    println(listOf(3 to "c", 1 to "a").toMap(java.util.TreeMap()))
    println(mapOf(5 to 1, 2 to 2).toMap(sortedMapOf(9 to 9)))
    val d = mutableMapOf(0 to 0); val r = listOf(1 to 1).toMap(d); println(r === d); println(d)
}"#
        ),
        "[k1, k2, k3, k20, k4, k5, k11, k6]\n{1=a, 3=c}\n{2=2, 5=1, 9=9}\ntrue\n{0=0, 1=1}\n"
    );
}

#[test]
fn jdk_integer_long_and_double_statics_without_a_kotlin_member() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. Each was `unresolved
    // reference: Integer` (or `java`).
    assert_eq!(
        stdout(
            r#"fun main() {
    println(listOf(Integer.reverse(1), Integer.reverse(-2), Integer.reverseBytes(0x01020304), Integer.signum(-5), Integer.signum(0), Integer.signum(9)))
    println(listOf(Integer.toUnsignedString(-1), Integer.toUnsignedLong(-1), Integer.divideUnsigned(-1, 2), Integer.remainderUnsigned(-1, 7), Integer.compareUnsigned(-1, 1), Integer.toUnsignedString(-16, 16)))
    println(listOf(java.lang.Long.reverse(1L), java.lang.Long.reverseBytes(1L), java.lang.Long.signum(-9L), java.lang.Long.toUnsignedString(-1L), java.lang.Long.divideUnsigned(-1L, 3L), java.lang.Long.remainderUnsigned(-1L, 10L), java.lang.Long.compareUnsigned(-1L, 1L)))
    println(listOf(java.lang.Double.compare(1.0, 2.0), java.lang.Double.compare(0.0, -0.0), java.lang.Double.isNaN(0.0 / 0), java.lang.Double.isInfinite(1 / 0.0), java.lang.Double.isFinite(1.0), java.lang.Double.toString(1.5), java.lang.Double.max(1.0, 3.0), java.lang.Double.min(-0.0, 0.0), java.lang.Double.sum(0.1, 0.2), java.lang.Double.hashCode(1.5)))
    println(listOf(java.lang.Float.compare(1f, 2f), java.lang.Float.isNaN(Float.NaN), java.lang.Float.toString(0.1f), java.lang.Integer.toString(5), java.lang.Integer.hashCode(7), java.lang.Long.hashCode(-1L), java.lang.Boolean.compare(true, false), java.lang.Character.isLetter('x')))
}"#
        ),
        "[-2147483648, 2147483647, 67305985, -1, 0, 1]\n\
         [4294967295, 4294967295, 2147483647, 3, 1, fffffff0]\n\
         [-9223372036854775808, 72057594037927936, -1, 18446744073709551615, 6148914691236517205, 5, 1]\n\
         [-1, 1, true, true, true, 1.5, 3.0, -0.0, 0.30000000000000004, 1073217536]\n\
         [-1, true, 0.1, 5, 7, 0, 1, true]\n"
    );
}

#[test]
fn stdlib_members_that_answered_unresolved_reference() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. Every one of these was
    // `unresolved reference` (`IntRange`, `emptySequence`, `lineSequence on
    // String`, `toBits on Double`, `Double`, `java`, `sumBy on List`).
    assert_eq!(
        stdout(
            r#"data class P(val a: Int)
@Suppress("DEPRECATION")
fun main() {
    println(listOf(IntRange.EMPTY, LongRange.EMPTY, IntRange.EMPTY.isEmpty(), CharRange.EMPTY.count()))
    println(listOf(emptySequence<Int>().toList(), emptySequence<String>().count(), "a\nb\r\nc\n".lineSequence().toList()))
    println(listOf(12.345.toBits(), Double.NaN.toRawBits(), (0.0 / 0.0).toBits(), (-0.0).toBits(), 1.5f.toBits(), 0.1f.toRawBits(), Float.NaN.toBits()))
    println(listOf(Double.fromBits(4613937818241073152L), Float.fromBits(1069547520), Float.fromBits(1036831949)))
    val n: String? = null
    println(listOf(java.lang.String.valueOf(5), java.lang.String.valueOf(n), java.lang.String.valueOf(P(1)), java.lang.String.valueOf('c')))
    println(listOf(listOf(5, 3).sumBy { it }, listOf(Int.MAX_VALUE, 1).sumBy { it }, listOf(1.5, 2.0).sumByDouble { it * 2 }))
}"#
        ),
        "[1..0, 1..0, true, 0]\n[[], 0, [a, b, c, ]]\n[4623139235229744497, 9221120237041090560, 9221120237041090560, -9223372036854775808, 1069547520, 1036831949, 2143289344]\n[3.0, 1.5, 0.1]\n[5, null, P(a=1), c]\n[8, -2147483648, 7.0]\n"
    );
}

#[test]
fn the_seedless_generate_sequence_and_constrain_once_allow_one_traversal() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `generateSequence { }` asks
    // its function for every element, lazily, and is `constrainOnce()`d: a
    // second traversal — through any pipeline built on it — throws.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("genSeedless") { var i = 0; generateSequence { if (i < 3) i++ else null }.toList() }
    t("seedlessLazy") { var n = 0; val s = generateSequence { n++; if (n <= 2) n * 10 else null }; println("built $n"); s.toList() }
    t("seedlessTwice") { var i = 0; val s = generateSequence { if (i < 3) i++ else null }; s.toList(); s.toList() }
    t("seedlessMapTwice") { var i = 0; val s = generateSequence { if (i < 9) i++ else null }; val m = s.map { it * 2 }; println(m.take(2).toList()); m.toList() }
    t("constrain") { val s = sequenceOf(1, 2).constrainOnce(); s.toList(); s.toList() }
    t("constrainOrig") { val s = sequenceOf(1, 2); val c = s.constrainOnce(); c.toList(); s.toList() }
    t("constrainGen") { val s = generateSequence(1) { if (it < 3) it + 1 else null }.constrainOnce(); println(s.toList()); s.count() }
    t("constrainBlock") { val s = sequence { yield(1) }.constrainOnce(); println(s.first()); s.first() }
    t("seedTake") { var i = 0; generateSequence { i++ }.take(4).toList() }
}"#
        ),
        "genSeedless: [0, 1, 2]\nbuilt 0\nseedlessLazy: [10, 20]\nseedlessTwice: EX java.lang.IllegalStateException: This sequence can be consumed only once.\n[0, 2]\nseedlessMapTwice: EX java.lang.IllegalStateException: This sequence can be consumed only once.\nconstrain: EX java.lang.IllegalStateException: This sequence can be consumed only once.\nconstrainOrig: [1, 2]\n[1, 2, 3]\nconstrainGen: EX java.lang.IllegalStateException: This sequence can be consumed only once.\n1\nconstrainBlock: EX java.lang.IllegalStateException: This sequence can be consumed only once.\nseedTake: [0, 1, 2, 3]\n"
    );
}

#[test]
fn top_level_overloads_resolve_by_argument_types() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. Every overload of a name
    // used to collapse onto the first declaration: `describe(1L)`,
    // `describe("s")` and `describe(1.5)` all printed `Int …`.
    assert_eq!(
        stdout(
            r#"open class Animal(val n: String)
class Dog(n: String) : Animal(n)
class Cat(n: String) : Animal(n)
fun describe(x: Int) = "Int $x"
fun describe(x: Long) = "Long $x"
fun describe(x: Double) = "Double $x"
fun describe(x: String) = "String $x"
fun describe(x: Boolean) = "Boolean $x"
fun describe(x: Char) = "Char $x"
fun describe(x: List<Int>) = "List ${x.size}"
fun describe(x: Animal) = "Animal ${x.n}"
fun describe(x: Dog) = "Dog ${x.n}"
fun describe(x: Any?) = "Any $x"
fun area(r: Double) = 3.0 * r * r
fun area(w: Double, h: Double) = w * h
fun area(w: Int, h: Int, d: Int = 1) = w * h * d
fun greet(name: String, greeting: String = "Hello") = "$greeting, $name"
fun greet(times: Int) = "hi".repeat(times)
fun sum(vararg xs: Int) = xs.sum()
fun sum(a: Int, b: Int) = a + b + 1000
fun fact(n: Int): Long = if (n <= 1) 1L else n * fact(n - 1)
fun fact(n: Long): Long = if (n <= 1L) 1L else n * fact(n - 1L)
fun main() {
    println(describe(1)); println(describe(1L)); println(describe(1.5)); println(describe("s")); println(describe(true)); println(describe('c'))
    println(describe(listOf(1, 2))); println(describe(Dog("rex"))); println(describe(Cat("tom"))); println(describe(Animal("a")))
    println(describe(null)); println(describe(2.5f)); println(describe(setOf(1)))
    val a: Animal = Dog("d"); println(describe(a))
    val n = 7; println(describe(n)); val l = 8L; println(describe(l * 2))
    println(area(2.0)); println(area(2.0, 3.0)); println(area(2, 3)); println(area(2, 3, 4))
    println(greet("Bob")); println(greet("Ann", "Hey")); println(greet(3)); println(greet(name = "Z"))
    println(sum(1, 2)); println(sum(1, 2, 3)); println(sum())
    println(fact(5)); println(fact(20L))
    println(listOf(1, 2).map { describe(it) }); println(listOf("x").map { describe(it) })
}"#
        ),
        "Int 1\nLong 1\nDouble 1.5\nString s\nBoolean true\nChar c\nList 2\nDog rex\nAnimal tom\nAnimal a\nAny null\nAny 2.5\nAny [1]\nAnimal d\nInt 7\nLong 16\n12.0\n6.0\n6\n24\nHello, Bob\nHey, Ann\nhihihi\nHello, Z\n1003\n6\n0\n120\n2432902008176640000\n[Int 1, Int 2]\n[String x]\n"
    );
}

#[test]
fn overload_selection_reads_the_static_type_and_smart_casts() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: `val a: Animal = Dog()`
    // picks `describe(Animal)` — the annotation, not the instance — until an
    // `is Dog` check smart-casts it.
    assert_eq!(
        stdout(
            r#"open class Animal(val n: String)
class Dog(n: String) : Animal(n)
fun describe(x: Animal) = "Animal ${x.n}"
fun describe(x: Dog) = "Dog ${x.n}"
fun apply(f: (Int) -> Int) = f(10)
fun apply(x: Int) = x + 1
fun <T> show(x: T) = "T $x"
fun show(x: Int) = "Int $x"
fun pick(a: Int, b: String = "d") = "IS $a$b"
fun pick(a: String, b: Int = 0) = "SI $a$b"
fun nums(xs: List<Int>) = "list ${xs.sum()}"
fun nums(xs: Set<Int>) = "set ${xs.size}"
fun nums(xs: Map<Int, Int>) = "map ${xs.size}"
fun main() {
    val a: Animal = Dog("d")
    if (a is Dog) println(describe(a))
    println(describe(a))
    println(apply { it * 2 }); println(apply(5))
    println(show("s")); println(show(3)); println(show(3L))
    println(pick(1)); println(pick("x")); println(pick(b = 2, a = "y")); println(pick(a = 4, b = "z"))
    println(nums(listOf(1, 2))); println(nums(setOf(1, 2, 3))); println(nums(mapOf(1 to 2))); println(nums(mutableListOf(5))); println(nums(hashSetOf(1)))
    val xs = listOf(Dog("q"), Animal("r")); println(xs.map { describe(it) })
}"#
        ),
        "Dog d\nAnimal d\n20\n6\nT s\nInt 3\nT 3\nIS 1d\nSI x0\nSI y2\nIS 4z\nlist 3\nset 3\nmap 1\nlist 5\nset 1\n[Animal q, Animal r]\n"
    );
}

#[test]
fn a_class_declaring_one_method_twice_resolves_the_call_by_its_argument() {
    // The second declaration used to answer for the first (`C().f(1)` ran
    // `f(a: String)`), and the set was then refused; member overloads now
    // resolve, and `C().f(1)` is the reference's `2` (kotlinc 2.4.21 / JRE
    // 21.0.12.1), as is the same set on `next`, a member the runtime calls by
    // name. Two declarations with the SAME parameters stay a compile error, as
    // `kotlinc` reports conflicting overloads.
    for name in ["f", "next"] {
        assert_eq!(
            stdout(&format!(
                "class C {{ fun {name}(a: String) = 1; fun {name}(a: Int) = 2 }}\nfun main() {{ println(C().{name}(1)) }}"
            )),
            "2\n"
        );
    }
    let out =
        eval("class C { fun f(a: Int) = 1; fun f(a: Int) = 2 }\nfun main() { println(C().f(1)) }");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("conflicting overloads"), "stderr was: {err}");
}

#[test]
fn comparator_builders_over_a_comparator_and_compare_values() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `compareBy(cmp) { sel }`,
    // `compareValues`, `compareValuesBy` and `minOfWith`/`maxOfWith` were
    // `unresolved reference`.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
data class P(val name: String, val age: Int?)
fun main() {
    val ps = listOf(P("b", 3), P("a", null), P("c", 3), P("d", 1))
    t("nullsFirst") { ps.sortedWith(compareBy(nullsFirst()) { it.age }).map { it.name } }
    t("nullsLast") { ps.sortedWith(compareBy(nullsLast()) { it.age }).map { it.name } }
    t("byCmpDesc") { ps.sortedWith(compareByDescending(nullsLast()) { it.age }).map { it.name } }
    t("byLen") { listOf("bb", "a", "ccc").sortedWith(compareBy(reverseOrder()) { it.length }) }
    t("cmpValues") { listOf(compareValues(null, 1), compareValues("b", "a"), compareValues(2, 2), compareValues<Int>(null, null), compareValues(1.5, 0.5)) }
    t("cmpBy") { listOf(compareValuesBy(P("a", 1), P("a", 2), { it.name }, { it.age }), compareValuesBy(P("b", 1), P("a", 2), { it.name }), compareValuesBy(P("a", null), P("a", 2)) { it.age }) }
    t("minOfWith") { listOf("aa", "b", "ccc").minOfWith(compareBy<String> { it.length }) { it + "!" } }
    t("maxOfWith") { listOf(3, 1, 2).maxOfWith(reverseOrder()) { it * 10 } }
    t("minOfWithEmpty") { listOf<Int>().minOfWith(naturalOrder()) { it } }
    t("minOfWithOrNull") { listOf<Int>().minOfWithOrNull(naturalOrder()) { it } }
    t("maxOfWithOrNull") { listOf(4, 9).maxOfWithOrNull(naturalOrder()) { -it } }
}"#
        ),
        "nullsFirst: [a, d, b, c]\nnullsLast: [d, b, c, a]\nbyCmpDesc: [a, b, c, d]\nbyLen: [ccc, bb, a]\ncmpValues: [-1, 1, 0, 0, 1]\ncmpBy: [-1, 1, -1]\nminOfWith: b!\nmaxOfWith: 10\nminOfWithEmpty: EX java.util.NoSuchElementException\nminOfWithOrNull: null\nmaxOfWithOrNull: -4\n"
    );
}

#[test]
fn a_for_loop_and_an_iterator_pull_a_lazy_source_one_element_at_a_time() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. The loop used to pull the
    // whole sequence at its head, so the block's side effects ran before the
    // body's (`y1 y2 b1 b2` for `y1 b1 y2 b2`); `seq.iterator()` did the same.
    assert_eq!(
        stdout(
            r#"fun main() {
    val it1 = sequence { print("y1 "); yield(1); print("y2 "); yield(2) }.iterator()
    while (it1.hasNext()) print("b${it1.next()} "); println()
    val s = generateSequence(1) { print("g$it "); if (it < 3) it + 1 else null }.map { print("m$it "); it * 10 }
    val it2 = s.iterator(); while (it2.hasNext()) print("b${it2.next()} "); println()
    val it3 = iterator { print("i1 "); yield(5); print("i2 ") }
    while (it3.hasNext()) print("b${it3.next()} "); println()
    for (x in sequence { print("y1 "); yield(1); print("y2 "); yield(2) }) print("b$x "); println()
    for (x in s) print("b$x "); println()
    for (x in iterator { print("i1 "); yield(5); print("i2 ") }) print("b$x "); println()
    val ss = sequence { print("A "); yield(1); print("B "); yield(2); print("C ") }
    for (x in ss) { if (x == 1) continue; print("b$x ") }; println()
    for (x in ss) { print("b$x "); break }; println()
    for ((i, v) in ss.withIndex()) print("$i:$v "); println()
    for (x in listOf(1, 2).asSequence().filter { print("f$it "); true }) print("b$x "); println()
    val lst = listOf(1, 2); for (x in lst.asSequence()) print("L$x "); println()
}"#
        ),
        "y1 b1 y2 b2 \nm1 b10 g1 m2 b20 g2 m3 b30 g3 \ni1 b5 i2 \ny1 b1 y2 b2 \nm1 b10 g1 m2 b20 g2 m3 b30 g3 \ni1 b5 i2 \nA B b2 C \nA b1 \nA 0:1 B 1:2 C \nf1 b1 f2 b2 \nL1 L2 \n"
    );
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun gen() = sequence { print("<a>"); yield(1); print("<b>"); yield(2); print("<c>") }
fun main() {
    for (x in gen()) print("[$x]"); println()
    for (x in gen().map { it * 10 }.filter { it > 10 }) print("[$x]"); println()
    for (x in gen().take(1)) print("[$x]"); println()
    var n = 0; for (x in generateSequence(1) { it + 1 }) { n += x; if (x == 70_000) break }; println(n)
    for ((i, v) in gen().withIndex()) print("[$i=$v]"); println()
    println(gen().withIndex().toList())
    val it = gen().iterator(); println("made"); println(it.hasNext()); println(it.hasNext()); println(it.next()); println(it.next()); println(it.hasNext())
    t("nextPastEnd") { val i2 = sequenceOf(1).iterator(); i2.next(); i2.next() }
    val once = sequenceOf(1, 2).constrainOnce(); for (x in once) print(x); println(); t("twice") { for (x in once) print(x) }
    fun numbers(): Sequence<Int> = generateSequence(1) { it + 1 }
    for (x in numbers()) { if (x > 3) break; print(x) }; println()
    val xs: Iterable<Int> = listOf(4, 5); for (x in xs) print(x); println()
    for (x in mapOf(1 to 2).entries) print(x); println()
    for (c in "ab".asSequence()) print(c); println()
    for (x in (1..3).asSequence().map { print("m"); it }) print(x); println()
}"#
        ),
        "<a>[1]<b>[2]<c>\n<a><b>[20]<c>\n<a>[1]\n-1844932296\n<a>[0=1]<b>[1=2]<c>\n<a><b><c>[IndexedValue(index=0, value=1), IndexedValue(index=1, value=2)]\nmade\n<a>true\ntrue\n1\n<b>2\n<c>false\nnextPastEnd: EX java.util.NoSuchElementException\n12\ntwice: EX java.lang.IllegalStateException: This sequence can be consumed only once.\n123\n45\n1=2\nab\nm1m2m3\n"
    );
}

#[test]
fn read_only_set_and_map_literals_behind_a_mutable_cast_fault_as_their_jvm_class() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `setOf(x)` / `mapOf(k to v)`
    // and a one-element `toSet()` / `toMap()` are the JDK singletons, which
    // refuse every write; the empty ones are `EmptySet` / `EmptyMap`, which no
    // mutable cast accepts; from two elements up they are writable.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
@Suppress("UNCHECKED_CAST") fun <T> ms(s: Set<T>) = s as MutableSet<T>
@Suppress("UNCHECKED_CAST") fun <K, V> mm(m: Map<K, V>) = m as MutableMap<K, V>
fun main() {
    for ((n, s) in listOf("s0" to setOf<Int>(), "s1" to setOf(1), "s2" to setOf(1, 2), "e" to emptySet<Int>(), "ts" to setOf(1).toSet(), "ts2" to setOf(2, 1).toSet())) {
        t("$n.add") { ms(s).add(7); s }
        t("$n.addDup") { ms(s).add(1) }
        t("$n.removeAbsent") { ms(s).remove(9) }
        t("$n.removePresent") { ms(s).remove(1); s }
        t("$n.clear") { ms(s).clear(); s }
        t("$n.retainAll") { ms(s).retainAll(listOf(1, 2)) }
        t("$n.plusAssign") { val q = ms(s); q += 5; s }
    }
    for ((n, m) in listOf("m0" to mapOf<Int, Int>(), "m1" to mapOf(1 to 1), "m2" to mapOf(1 to 1, 2 to 2), "em" to emptyMap<Int, Int>(), "tm" to mapOf(1 to 1).toMap())) {
        t("$n.put") { mm(m)[7] = 7; m }
        t("$n.putSame") { mm(m).put(1, 1) }
        t("$n.removeAbsent") { mm(m).remove(9) }
        t("$n.removePresent") { mm(m).remove(1); m }
        t("$n.clear") { mm(m).clear(); m }
        t("$n.getOrPutHit") { mm(m).getOrPut(1) { 9 } }
        t("$n.getOrPutMiss") { mm(m).getOrPut(5) { 9 } }
        t("$n.isMutable") { m is MutableMap<*, *> }
    }
}"#
        ),
        "s0.add: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns0.addDup: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns0.removeAbsent: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns0.removePresent: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns0.clear: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns0.retainAll: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns0.plusAssign: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ns1.add: EX java.lang.UnsupportedOperationException\ns1.addDup: EX java.lang.UnsupportedOperationException\ns1.removeAbsent: false\ns1.removePresent: EX java.lang.UnsupportedOperationException\ns1.clear: EX java.lang.UnsupportedOperationException\ns1.retainAll: false\ns1.plusAssign: EX java.lang.UnsupportedOperationException\ns2.add: [1, 2, 7]\ns2.addDup: false\ns2.removeAbsent: false\ns2.removePresent: [2, 7]\ns2.clear: []\ns2.retainAll: false\ns2.plusAssign: [5]\ne.add: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ne.addDup: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ne.removeAbsent: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ne.removePresent: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ne.clear: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ne.retainAll: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\ne.plusAssign: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\nts.add: EX java.lang.UnsupportedOperationException\nts.addDup: EX java.lang.UnsupportedOperationException\nts.removeAbsent: false\nts.removePresent: EX java.lang.UnsupportedOperationException\nts.clear: EX java.lang.UnsupportedOperationException\nts.retainAll: false\nts.plusAssign: EX java.lang.UnsupportedOperationException\nts2.add: [2, 1, 7]\nts2.addDup: false\nts2.removeAbsent: false\nts2.removePresent: [2, 7]\nts2.clear: []\nts2.retainAll: false\nts2.plusAssign: [5]\nm0.put: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.putSame: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.removeAbsent: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.removePresent: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.clear: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.getOrPutHit: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.getOrPutMiss: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nm0.isMutable: false\nm1.put: EX java.lang.UnsupportedOperationException\nm1.putSame: EX java.lang.UnsupportedOperationException\nm1.removeAbsent: null\nm1.removePresent: EX java.lang.UnsupportedOperationException\nm1.clear: EX java.lang.UnsupportedOperationException\nm1.getOrPutHit: 1\nm1.getOrPutMiss: EX java.lang.UnsupportedOperationException\nm1.isMutable: true\nm2.put: {1=1, 2=2, 7=7}\nm2.putSame: 1\nm2.removeAbsent: null\nm2.removePresent: {2=2, 7=7}\nm2.clear: {}\nm2.getOrPutHit: 9\nm2.getOrPutMiss: 9\nm2.isMutable: true\nem.put: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.putSame: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.removeAbsent: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.removePresent: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.clear: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.getOrPutHit: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.getOrPutMiss: EX java.lang.ClassCastException: kotlin.collections.EmptyMap cannot be cast to kotlin.collections.MutableMap\nem.isMutable: false\ntm.put: EX java.lang.UnsupportedOperationException\ntm.putSame: EX java.lang.UnsupportedOperationException\ntm.removeAbsent: null\ntm.removePresent: EX java.lang.UnsupportedOperationException\ntm.clear: EX java.lang.UnsupportedOperationException\ntm.getOrPutHit: 1\ntm.getOrPutMiss: EX java.lang.UnsupportedOperationException\ntm.isMutable: true\n"
    );
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
@Suppress("UNCHECKED_CAST") fun <T> ms(s: Set<T>) = s as MutableSet<T>
@Suppress("UNCHECKED_CAST") fun <K, V> mm(m: Map<K, V>) = m as MutableMap<K, V>
fun main() {
    t("dupToSet") { val s = listOf(1, 1).toSet(); ms(s).add(2); s }
    t("oneToSet") { val s = listOf(1).toSet(); ms(s).add(2); s }
    t("rangeToSet") { val s = (1..1).toSet(); ms(s).add(2); s }
    t("seqToSet") { val s = sequenceOf(3, 3).toSet(); ms(s).add(2); s }
    t("strToSet") { val s = "aa".toSet(); ms(s).add('b'); s }
    t("dupPairs") { val m = listOf(1 to 1, 1 to 2).toMap(); mm(m)[5] = 5; m }
    t("onePair") { val m = listOf(1 to 1).toMap(); mm(m)[5] = 5; m }
    t("mapToMap1") { val m = mapOf(1 to 1, 2 to 2).filterKeys { it == 1 }.toMap(); mm(m)[5] = 5; m }
    t("setOf2shrunk") { val s = setOf(1, 2); ms(s).remove(2); ms(s).add(3); s }
    t("emptyToSet") { ms(listOf<Int>().toSet()) }
}"#
        ),
        "dupToSet: [1, 2]\noneToSet: EX java.lang.UnsupportedOperationException\nrangeToSet: EX java.lang.UnsupportedOperationException\nseqToSet: [3, 2]\nstrToSet: [a, b]\ndupPairs: {1=2, 5=5}\nonePair: EX java.lang.UnsupportedOperationException\nmapToMap1: EX java.lang.UnsupportedOperationException\nsetOf2shrunk: [1, 3]\nemptyToSet: EX java.lang.ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet\n"
    );
}

#[test]
fn a_cast_to_a_collection_interface_names_both_jvm_classes() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. The target used to print as
    // the Kotlin name and the receiver as `value`: `class value cannot be
    // cast to class List`.
    assert_eq!(
        stdout(
            r#"data class D(val a: Int)
fun main() {
    val vals: List<Any> = listOf(1, "s", setOf(1, 2), mapOf(1 to 2, 3 to 4), D(1), listOf(1, 2), 1.5, mutableListOf(1), setOf(1), mapOf(1 to 1), emptySet<Int>())
    for (v in vals) try { println(v as List<*>) } catch (e: ClassCastException) { println(e.message) }
    for (v in vals) try { println(v as Map<*, *>) } catch (e: ClassCastException) { println(e.message) }
    for (v in vals) try { println(v as Set<*>) } catch (e: ClassCastException) { println(e.message) }
    for (v in vals) try { println(v as Collection<*>) } catch (e: ClassCastException) { println(e.message) }
    for (v in vals) try { println(v as Iterable<*>) } catch (e: ClassCastException) { println(e.message) }
}"#
        ),
        "class java.lang.Integer cannot be cast to class java.util.List (java.lang.Integer and java.util.List are in module java.base of loader 'bootstrap')\nclass java.lang.String cannot be cast to class java.util.List (java.lang.String and java.util.List are in module java.base of loader 'bootstrap')\nclass java.util.LinkedHashSet cannot be cast to class java.util.List (java.util.LinkedHashSet and java.util.List are in module java.base of loader 'bootstrap')\nclass java.util.LinkedHashMap cannot be cast to class java.util.List (java.util.LinkedHashMap and java.util.List are in module java.base of loader 'bootstrap')\nclass D cannot be cast to class java.util.List (D is in unnamed module of loader 'app'; java.util.List is in module java.base of loader 'bootstrap')\n[1, 2]\nclass java.lang.Double cannot be cast to class java.util.List (java.lang.Double and java.util.List are in module java.base of loader 'bootstrap')\n[1]\nclass java.util.Collections$SingletonSet cannot be cast to class java.util.List (java.util.Collections$SingletonSet and java.util.List are in module java.base of loader 'bootstrap')\nclass java.util.Collections$SingletonMap cannot be cast to class java.util.List (java.util.Collections$SingletonMap and java.util.List are in module java.base of loader 'bootstrap')\nclass kotlin.collections.EmptySet cannot be cast to class java.util.List (kotlin.collections.EmptySet is in unnamed module of loader 'app'; java.util.List is in module java.base of loader 'bootstrap')\nclass java.lang.Integer cannot be cast to class java.util.Map (java.lang.Integer and java.util.Map are in module java.base of loader 'bootstrap')\nclass java.lang.String cannot be cast to class java.util.Map (java.lang.String and java.util.Map are in module java.base of loader 'bootstrap')\nclass java.util.LinkedHashSet cannot be cast to class java.util.Map (java.util.LinkedHashSet and java.util.Map are in module java.base of loader 'bootstrap')\n{1=2, 3=4}\nclass D cannot be cast to class java.util.Map (D is in unnamed module of loader 'app'; java.util.Map is in module java.base of loader 'bootstrap')\nclass java.util.Arrays$ArrayList cannot be cast to class java.util.Map (java.util.Arrays$ArrayList and java.util.Map are in module java.base of loader 'bootstrap')\nclass java.lang.Double cannot be cast to class java.util.Map (java.lang.Double and java.util.Map are in module java.base of loader 'bootstrap')\nclass java.util.ArrayList cannot be cast to class java.util.Map (java.util.ArrayList and java.util.Map are in module java.base of loader 'bootstrap')\nclass java.util.Collections$SingletonSet cannot be cast to class java.util.Map (java.util.Collections$SingletonSet and java.util.Map are in module java.base of loader 'bootstrap')\n{1=1}\nclass kotlin.collections.EmptySet cannot be cast to class java.util.Map (kotlin.collections.EmptySet is in unnamed module of loader 'app'; java.util.Map is in module java.base of loader 'bootstrap')\nclass java.lang.Integer cannot be cast to class java.util.Set (java.lang.Integer and java.util.Set are in module java.base of loader 'bootstrap')\nclass java.lang.String cannot be cast to class java.util.Set (java.lang.String and java.util.Set are in module java.base of loader 'bootstrap')\n[1, 2]\nclass java.util.LinkedHashMap cannot be cast to class java.util.Set (java.util.LinkedHashMap and java.util.Set are in module java.base of loader 'bootstrap')\nclass D cannot be cast to class java.util.Set (D is in unnamed module of loader 'app'; java.util.Set is in module java.base of loader 'bootstrap')\nclass java.util.Arrays$ArrayList cannot be cast to class java.util.Set (java.util.Arrays$ArrayList and java.util.Set are in module java.base of loader 'bootstrap')\nclass java.lang.Double cannot be cast to class java.util.Set (java.lang.Double and java.util.Set are in module java.base of loader 'bootstrap')\nclass java.util.ArrayList cannot be cast to class java.util.Set (java.util.ArrayList and java.util.Set are in module java.base of loader 'bootstrap')\n[1]\nclass java.util.Collections$SingletonMap cannot be cast to class java.util.Set (java.util.Collections$SingletonMap and java.util.Set are in module java.base of loader 'bootstrap')\n[]\nclass java.lang.Integer cannot be cast to class java.util.Collection (java.lang.Integer and java.util.Collection are in module java.base of loader 'bootstrap')\nclass java.lang.String cannot be cast to class java.util.Collection (java.lang.String and java.util.Collection are in module java.base of loader 'bootstrap')\n[1, 2]\nclass java.util.LinkedHashMap cannot be cast to class java.util.Collection (java.util.LinkedHashMap and java.util.Collection are in module java.base of loader 'bootstrap')\nclass D cannot be cast to class java.util.Collection (D is in unnamed module of loader 'app'; java.util.Collection is in module java.base of loader 'bootstrap')\n[1, 2]\nclass java.lang.Double cannot be cast to class java.util.Collection (java.lang.Double and java.util.Collection are in module java.base of loader 'bootstrap')\n[1]\n[1]\nclass java.util.Collections$SingletonMap cannot be cast to class java.util.Collection (java.util.Collections$SingletonMap and java.util.Collection are in module java.base of loader 'bootstrap')\n[]\nclass java.lang.Integer cannot be cast to class java.lang.Iterable (java.lang.Integer and java.lang.Iterable are in module java.base of loader 'bootstrap')\nclass java.lang.String cannot be cast to class java.lang.Iterable (java.lang.String and java.lang.Iterable are in module java.base of loader 'bootstrap')\n[1, 2]\nclass java.util.LinkedHashMap cannot be cast to class java.lang.Iterable (java.util.LinkedHashMap and java.lang.Iterable are in module java.base of loader 'bootstrap')\nclass D cannot be cast to class java.lang.Iterable (D is in unnamed module of loader 'app'; java.lang.Iterable is in module java.base of loader 'bootstrap')\n[1, 2]\nclass java.lang.Double cannot be cast to class java.lang.Iterable (java.lang.Double and java.lang.Iterable are in module java.base of loader 'bootstrap')\n[1]\n[1]\nclass java.util.Collections$SingletonMap cannot be cast to class java.lang.Iterable (java.util.Collections$SingletonMap and java.lang.Iterable are in module java.base of loader 'bootstrap')\n[]\n"
    );
}

#[test]
fn to_char_array_takes_its_range() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: the range arguments were
    // ignored, so `"hello".toCharArray(1, 3)` answered all five characters.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("a") { "hello".toCharArray(1, 3).joinToString() }
    t("b") { "hello".toCharArray(2).joinToString() }
    t("c") { "hello".toCharArray(endIndex = 2).joinToString() }
    t("d") { "hello".toCharArray(1, 9).joinToString() }
    t("e") { "hello".toCharArray(3, 1).joinToString() }
    t("f") { "hello".toCharArray(-1, 2).joinToString() }
    t("g") { "a😀".toCharArray(1, 3).size }
}"#
        ),
        "a: e, l\nb: l, l, o\nc: h, e\nd: EX java.lang.IndexOutOfBoundsException: startIndex: 1, endIndex: 9, size: 5\ne: EX java.lang.IllegalArgumentException: startIndex: 3 > endIndex: 1\nf: EX java.lang.IndexOutOfBoundsException: startIndex: -1, endIndex: 2, size: 5\ng: 2\n"
    );
}

#[test]
fn range_arguments_of_binary_search_sort_and_copy_into_are_honoured() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `binarySearch(x, from, to)`
    // and an array's `sort(from, to)` ignored the range; `copyInto` was
    // `unresolved reference`.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    val l = listOf(1, 3, 5, 7, 9)
    t("a") { l.binarySearch(7, 0, 3) }
    t("b") { l.binarySearch(7, 2) }
    t("c") { l.binarySearch(1, 1, 4) }
    t("d") { l.binarySearch(4, fromIndex = 1, toIndex = 4) }
    t("e") { l.binarySearch(7, 3, 1) }
    t("f") { l.binarySearch(7, -1, 3) }
    t("g") { l.binarySearch(7, 0, 9) }
    val a = intArrayOf(1, 3, 5, 7, 9)
    t("aa") { a.binarySearch(7, 0, 3) }
    t("ab") { a.binarySearch(9, 2) }
    t("ae") { a.binarySearch(7, 3, 1) }
    t("af") { a.binarySearch(7, -1, 3) }
    t("ag") { a.binarySearch(7, 0, 9) }
    t("s1") { intArrayOf(5, 4, 3, 2, 1).also { it.sort(1, 4) }.toList() }
    t("s2") { intArrayOf(5, 4, 3, 2, 1).also { it.sort(2) }.toList() }
    t("s3") { intArrayOf(5, 4, 3).also { it.sort(2, 1) }.toList() }
    t("s4") { intArrayOf(5, 4, 3).also { it.sort(-1, 2) }.toList() }
    t("s5") { intArrayOf(5, 4, 3).also { it.sort(0, 7) }.toList() }
    t("s6") { arrayOf("c", "b", "a").also { it.sort(0, 2) }.toList() }
    t("s7") { intArrayOf(1, 2, 3, 4).also { it.sortDescending(1, 3) }.toList() }
    t("ci") { val d = IntArray(5); intArrayOf(1, 2, 3).copyInto(d, 1, 1, 3); d.toList() }
    t("ci2") { val d = arrayOfNulls<String>(3); arrayOf("x", "y").copyInto(d); d.toList() }
    t("ci3") { val d = IntArray(2); intArrayOf(1, 2, 3).copyInto(d).toList() }
    t("ci4") { val d = intArrayOf(1, 2, 3, 4, 5); d.copyInto(d, 1, 0, 3); d.toList() }
}"#
        ),
        "a: -4\nb: 3\nc: -2\nd: -3\ne: EX java.lang.IllegalArgumentException: fromIndex (3) is greater than toIndex (1).\nf: EX java.lang.IndexOutOfBoundsException: fromIndex (-1) is less than zero.\ng: EX java.lang.IndexOutOfBoundsException: toIndex (9) is greater than size (5).\naa: -4\nab: 4\nae: EX java.lang.IllegalArgumentException: fromIndex(3) > toIndex(1)\naf: EX java.lang.ArrayIndexOutOfBoundsException: Array index out of range: -1\nag: EX java.lang.ArrayIndexOutOfBoundsException: Array index out of range: 9\ns1: [5, 2, 3, 4, 1]\ns2: [5, 4, 1, 2, 3]\ns3: EX java.lang.IllegalArgumentException: fromIndex(2) > toIndex(1)\ns4: EX java.lang.ArrayIndexOutOfBoundsException: Array index out of range: -1\ns5: EX java.lang.ArrayIndexOutOfBoundsException: Array index out of range: 7\ns6: [b, c, a]\ns7: [1, 3, 2, 4]\nci: [0, 2, 3, 0, 0]\nci2: [x, y, null]\nci3: EX java.lang.ArrayIndexOutOfBoundsException: arraycopy: last destination index 3 out of bounds for int[2]\nci4: [1, 1, 2, 3, 5]\n"
    );
}

#[test]
fn add_all_at_an_index_and_min_of_over_nan() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `addAll(1, xs)` appended
    // nothing (the index was read as the collection), and `minOf { }` over a
    // NaN answered the smallest number where `Math.min` propagates NaN.
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("minOfNaN") { listOf(1.0, Double.NaN, 0.5).minOf { it } }
    t("maxOfNaN") { listOf(1.0, Double.NaN).maxOf { it } }
    t("minOfOrNullNaN") { listOf(Double.NaN, 2.0).minOfOrNull { it } }
    t("minOfFloat") { listOf(1f, Float.NaN).minOf { it } }
    t("minOfInt") { listOf(3, 1, 2).minOf { it } }
    t("minOfStr") { listOf("b", "a").minOf { it } }
    t("addAllIdx") { mutableListOf(1, 4).apply { addAll(1, listOf(2, 3)) } }
    t("addAllIdxRet") { mutableListOf(1).addAll(0, listOf()) }
    t("addAllIdxEnd") { mutableListOf(1).apply { addAll(1, setOf(5, 6)) } }
    t("addAllIdxBad") { mutableListOf(1).addAll(3, listOf(2)) }
    t("addAllIdxNeg") { mutableListOf(1).addAll(-1, listOf(2)) }
}"#
        ),
        "minOfNaN: NaN\nmaxOfNaN: NaN\nminOfOrNullNaN: NaN\nminOfFloat: NaN\nminOfInt: 1\nminOfStr: a\naddAllIdx: [1, 2, 3, 4]\naddAllIdxRet: false\naddAllIdxEnd: [1, 5, 6]\naddAllIdxBad: EX java.lang.IndexOutOfBoundsException: Index: 3, Size: 1\naddAllIdxNeg: EX java.lang.IndexOutOfBoundsException: Index: -1, Size: 1\n"
    );
    assert_eq!(
        stdout(
            r#"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("lastIndexOfStart") { "abcabc".lastIndexOf('b', 3) to "abcabc".lastIndexOf("bc", 2) }
    t("indexOfStart") { "abcabc".indexOf('a', 1) to "abcabc".indexOf("c", -5) }
    t("startsWithIdx") { "hello".startsWith("ll", 2) to "hello".startsWith("ll", 3) }
    t("endsWithCI") { "Hello".endsWith("LO", true) }
    t("replaceCI") { "aAbB".replace("a", "x", ignoreCase = true) }
    t("replaceChar") { "a-b-c".replace('-', '+') }
    t("replaceCharCI") { "aAa".replace('a', 'b', true) }
    t("containsChar") { "Hello".contains('h', ignoreCase = true) }
    t("splitChar") { "a.b..c".split('.') }
    t("splitCI") { "aXbxc".split("x", ignoreCase = true) }
    t("splitLimitNeg") { "a,b".split(",", limit = 0) }
    t("binSearchRange") { listOf(1, 3, 5, 7, 9).binarySearch(7, 0, 3) }
    t("fillRange") { IntArray(5).also { it.fill(9, 1, 3) }.toList() }
    t("copyOf") { intArrayOf(1, 2).copyOf(4).toList() }
    t("copyOfRange") { intArrayOf(1, 2, 3, 4).copyOfRange(1, 3).toList() }
    t("copyOfRangeBad") { intArrayOf(1, 2).copyOfRange(1, 5).toList() }
    t("sortRange") { intArrayOf(5, 4, 3, 2, 1).also { it.sort(1, 4) }.toList() }
    t("copyInto") { val d = IntArray(5); intArrayOf(1, 2, 3).copyInto(d, 1, 1, 3); d.toList() }
    t("arrCopyOfStr") { arrayOf("a", "b").copyOf(3).toList() }
    t("joinLimit") { (1..10).joinToString(limit = 3, truncated = "…") }
    t("joinTransform") { listOf(1, 2).joinToString(" + ", "= ", ";") { "($it)" } }
    t("chunkTransform") { (1..7).chunked(3) { it.sum() } }
    t("windowedPartialTransform") { (1..5).windowed(2, 2, true) { it.joinToString("") } }
    t("takeLast") { listOf(1, 2, 3).takeLast(5) }
    t("dropLastWhile") { listOf(1, 2, 3, 4).dropLastWhile { it > 2 } }
    t("elementAtOrElse") { listOf(1).elementAtOrElse(5) { it * 10 } }
    t("getOrElseList") { listOf(1).getOrElse(-1) { 99 } }
    t("indexOfLast") { listOf(1, 2, 1).indexOfLast { it == 1 } }
    t("lastOrNullPred") { listOf(1, 2, 3).lastOrNull { it < 3 } }
    t("findLast") { listOf(1, 2, 3).findLast { it % 2 == 1 } }
    t("randomSeeded") { listOf(1, 2, 3, 4, 5).random(kotlin.random.Random(5)) }
    t("toStringRadix") { (-255L).toString(16) to 255.toString(36) }
    t("toLongRadix") { "-zz".toLong(36) }
    t("toIntRadixBad") { "12".toInt(1) }
    t("strSubSeq") { "hello".subSequence(1, 3) }
    t("removeRange") { "hello".removeRange(1, 3) }
    t("replaceRange") { "hello".replaceRange(1, 3, "XYZ") }
    t("insertSB") { StringBuilder("ac").insert(1, 'b').insert(0, 12).toString() }
    t("mutableAddAllIdx") { mutableListOf(1, 4).apply { addAll(1, listOf(2, 3)) } }
    t("removeAtNeg") { mutableListOf(1).removeAt(-1) }
    t("setNeg") { mutableListOf(1).set(3, 0) }
    t("listOfNulls") { arrayOfNulls<String>(2).toList() }
    t("sumOfLong") { listOf(1, 2).sumOf { it.toLong() * Int.MAX_VALUE } }
    t("averageEmpty") { listOf<Int>().average() }
    t("maxOrNullD") { listOf(1.5, Double.NaN, 2.0).maxOrNull() }
    t("minOfNaN") { listOf(1.0, Double.NaN).minOf { it } }
}"#
        ),
        "lastIndexOfStart: (1, 1)\nindexOfStart: (3, 2)\nstartsWithIdx: (true, false)\nendsWithCI: true\nreplaceCI: xxbB\nreplaceChar: a+b+c\nreplaceCharCI: bbb\ncontainsChar: true\nsplitChar: [a, b, , c]\nsplitCI: [a, b, c]\nsplitLimitNeg: [a, b]\nbinSearchRange: -4\nfillRange: [0, 9, 9, 0, 0]\ncopyOf: [1, 2, 0, 0]\ncopyOfRange: [2, 3]\ncopyOfRangeBad: EX java.lang.IndexOutOfBoundsException: toIndex (5) is greater than size (2).\nsortRange: [5, 2, 3, 4, 1]\ncopyInto: [0, 2, 3, 0, 0]\narrCopyOfStr: [a, b, null]\njoinLimit: 1, 2, 3, …\njoinTransform: = (1) + (2);\nchunkTransform: [6, 15, 7]\nwindowedPartialTransform: [12, 34, 5]\ntakeLast: [1, 2, 3]\ndropLastWhile: [1, 2]\nelementAtOrElse: 50\ngetOrElseList: 99\nindexOfLast: 2\nlastOrNullPred: 2\nfindLast: 3\nrandomSeeded: 2\ntoStringRadix: (-ff, 73)\ntoLongRadix: -1295\ntoIntRadixBad: EX java.lang.IllegalArgumentException: radix 1 was not in valid range 2..36\nstrSubSeq: el\nremoveRange: hlo\nreplaceRange: hXYZlo\ninsertSB: 12abc\nmutableAddAllIdx: [1, 2, 3, 4]\nremoveAtNeg: EX java.lang.IndexOutOfBoundsException: Index -1 out of bounds for length 1\nsetNeg: EX java.lang.IndexOutOfBoundsException: Index 3 out of bounds for length 1\nlistOfNulls: [null, null]\nsumOfLong: 6442450941\naverageEmpty: NaN\nmaxOrNullD: NaN\nminOfNaN: NaN\n"
    );
}

#[test]
fn array_content_hashes_and_string_replace_around_a_delimiter() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `contentHashCode`,
    // `contentDeepHashCode`, the null-receiver `content…` members,
    // `replaceAfter`/`replaceBefore`(`Last`) and `chunkedSequence`/
    // `windowedSequence` over a string were `unresolved reference`.
    assert_eq!(
        stdout(
            r##"fun main() {
    println(listOf(longArrayOf(3000000000L, -1).contentHashCode(), doubleArrayOf(1.5, -0.0).contentHashCode(), floatArrayOf(0.1f, 2f).contentHashCode(), arrayOf("a", null, "bc").contentHashCode(), charArrayOf('x', 'y').contentHashCode(), booleanArrayOf(true, false).contentHashCode(), byteArrayOf(-1, 2).contentHashCode(), shortArrayOf(300).contentHashCode(), intArrayOf().contentHashCode()))
    val n: IntArray? = null
    println(n.contentHashCode())
    println(arrayOf<Any>(intArrayOf(1, 2), arrayOf("x")).contentDeepHashCode())
    println(arrayOf(listOf(1), 5).contentHashCode())
    more()
}
fun more() {
    val n: IntArray? = null
    println(n.contentToString())
    println(arrayOf<Any>(arrayOf(arrayOf(1)), 2).contentDeepHashCode())
}"##
        ),
        "[-1489279551, 1057489857, -1144205164, 126145, 4802, 40359, 932, 331, 1]\n0\n31926\n1958\nnull\n2916\n"
    );
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("indexOfFrom") { listOf(1, 2, 1, 2).subList(1, 4).indexOf(1) }
    t("strIndexOfCI") { "Hello".indexOf("L", ignoreCase = true) to "Hello".indexOf('L', 0, true) }
    t("strLastIndexOfCI") { "Hello".lastIndexOf("L", ignoreCase = true) }
    t("strCountSub") { "banana".windowed(3).count { it == "ana" } }
    t("arrFillAll") { arrayOf(1, 2, 3).also { it.fill(0) }.toList() }
    t("arrIndexOf") { intArrayOf(1, 2, 3).indexOf(3) to arrayOf("a", "b").lastIndexOf("a") }
    t("arrContains") { 2 in intArrayOf(1, 2) }
    t("arrSum") { longArrayOf(1, 2).sum() }
    t("arrMaxOrNull") { intArrayOf().maxOrNull() }
    t("arrReversed") { intArrayOf(1, 2, 3).reversedArray().toList() }
    t("arrSortedArray") { intArrayOf(3, 1).sortedArray().toList() }
    t("arrTakeDrop") { intArrayOf(1, 2, 3, 4).take(2) to intArrayOf(1, 2, 3).drop(1) }
    t("arrSlice") { intArrayOf(1, 2, 3, 4).sliceArray(1..2).toList() }
    t("arrPlus") { (intArrayOf(1) + 2 + intArrayOf(3, 4)).toList() }
    t("arrAll") { intArrayOf(2, 4).all { it % 2 == 0 } }
    t("arrMap") { intArrayOf(1, 2).map { it * 1.5 } }
    t("arrWithIndex") { arrayOf("a", "b").withIndex().toList() }
    t("arrForEachIdx") { var s = ""; charArrayOf('x', 'y').forEachIndexed { i, c -> s += "$i$c" }; s }
    t("arrEquals") { intArrayOf(1) contentEquals intArrayOf(1) }
    t("arrHash") { intArrayOf(1, 2).contentHashCode() }
    t("arr2dDeep") { arrayOf(intArrayOf(1), intArrayOf(2, 3)).contentDeepToString() }
    t("arrCountPred") { intArrayOf(1, 2, 3).count { it > 1 } }
    t("arrDistinct") { intArrayOf(1, 1, 2).distinct() }
    t("arrToSet") { intArrayOf(2, 1, 2).toSet() }
    t("arrFirstLast") { intArrayOf(5, 6).first() to intArrayOf(5, 6).last() }
    t("arrGetOrNull") { intArrayOf(5).getOrNull(3) }
    t("arrIsEmpty") { emptyArray<Int>().isEmpty() }
    t("arrOfNullsSize") { arrayOfNulls<Int>(3).size }
    t("arrAverage") { intArrayOf(1, 2).average() }
    t("arrJoin") { intArrayOf(1, 2).joinToString("-", "<", ">") }
    t("arrFold") { intArrayOf(1, 2, 3).fold(10) { a, b -> a - b } }
    t("arrReduceIdx") { intArrayOf(1, 2, 3).reduceIndexed { i, a, b -> a + i * b } }
    t("arrZip") { intArrayOf(1, 2) zip arrayOf("a", "b") }
    t("arrAsList") { arrayOf(1, 2).asList() }
    t("arrToMutable") { intArrayOf(1).toMutableList().apply { add(2) } }
    t("arrSortBy") { arrayOf("bb", "a").also { it.sortBy { s -> s.length } }.toList() }
    t("arrBinSearch") { intArrayOf(1, 3, 5).binarySearch(4) }
    t("arrFillRangeBad") { IntArray(3).also { it.fill(1, 2, 5) } }
    t("arrCopyOfNeg") { intArrayOf(1).copyOf(-1) }
    t("charArrStr") { charArrayOf('a', 'b').joinToString("") }
    t("byteArr") { byteArrayOf(127, -128).map { it + 1 } }
    t("shortArr") { shortArrayOf(1, 2).sum() }
    t("boolArr") { BooleanArray(3) { it % 2 == 0 }.toList() }
    t("floatArr") { floatArrayOf(1.5f, 2.25f).sum() }
}"##
        ),
        "indexOfFrom: 1\nstrIndexOfCI: (2, 2)\nstrLastIndexOfCI: 3\nstrCountSub: 2\narrFillAll: [0, 0, 0]\narrIndexOf: (2, 0)\narrContains: true\narrSum: 3\narrMaxOrNull: null\narrReversed: [3, 2, 1]\narrSortedArray: [1, 3]\narrTakeDrop: ([1, 2], [2, 3])\narrSlice: [2, 3]\narrPlus: [1, 2, 3, 4]\narrAll: true\narrMap: [1.5, 3.0]\narrWithIndex: [IndexedValue(index=0, value=a), IndexedValue(index=1, value=b)]\narrForEachIdx: 0x1y\narrEquals: true\narrHash: 994\narr2dDeep: [[1], [2, 3]]\narrCountPred: 2\narrDistinct: [1, 2]\narrToSet: [2, 1]\narrFirstLast: (5, 6)\narrGetOrNull: null\narrIsEmpty: true\narrOfNullsSize: 3\narrAverage: 1.5\narrJoin: <1-2>\narrFold: 4\narrReduceIdx: 9\narrZip: [(1, a), (2, b)]\narrAsList: [1, 2]\narrToMutable: [1, 2]\narrSortBy: [a, bb]\narrBinSearch: -3\narrFillRangeBad: EX java.lang.ArrayIndexOutOfBoundsException: Array index out of range: 5\narrCopyOfNeg: EX java.lang.NegativeArraySizeException: -1\ncharArrStr: ab\nbyteArr: [128, -127]\nshortArr: 3\nboolArr: [true, false, true]\nfloatArr: 3.75\n"
    );
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("splitChars") { "a,b;c d".split(',', ';', ' ') }
    t("splitCharsLimit") { "a,b,c,d".split(',', limit = 2) }
    t("splitCharCI") { "aXbxc".split('x', ignoreCase = true) }
    t("splitMultiStrLimit") { "a--b==c--d".split("--", "==", limit = 3) }
    t("trimEndChars") { "abcxyx".trimEnd('x', 'y') }
    t("trimStartChars") { "xxyab".trimStart('x', 'y') }
    t("startsWithChar") { "abc".startsWith('a') to "abc".endsWith('C', true) }
    t("removeSurroundingSame") { "\"q\"".removeSurrounding("\"") }
    t("substringAfterLast") { "a/b/c".substringAfterLast("/") to "abc".substringAfterLast("/", "none") }
    t("substringBeforeChar") { "a.b.c".substringBefore('.') to "a.b.c".substringAfter('.') }
    t("replaceAfter") { "a.b.c".replaceAfter(".", "X") to "a.b.c".replaceBefore(".", "Y") }
    t("replaceAfterLast") { "a.b.c".replaceAfterLast(".", "Z") to "a.b.c".replaceBeforeLast(".", "W") }
    t("padEndChar") { "ab".padEnd(5, '*') }
    t("chunkedSeqStr") { "abcdefg".chunkedSequence(3).toList() }
    t("takeLastStr") { "hello".takeLast(3) to "hello".dropLast(2) }
    t("takeWhileStr") { "aab".takeWhile { it == 'a' } to "aab".dropWhile { it == 'a' } }
    t("takeLastWhileStr") { "abb".takeLastWhile { it == 'b' } }
    t("filterIndexedStr") { "abcd".filterIndexed { i, _ -> i % 2 == 0 } }
    t("mapIndexedStr") { "ab".mapIndexed { i, c -> "$i$c" } }
    t("anyStr") { "abc".any { it == 'b' } to "abc".none { it == 'z' } }
    t("firstOrNullStr") { "abc".firstOrNull { it > 'a' } to "".firstOrNull() }
    t("singleStr") { "a".single() }
    t("singleStrBad") { "ab".single() }
    t("reduceStr") { "abc".reduce { acc, c -> maxOf(acc, c) } }
    t("charRangeStep") { ('a'..'k' step 3).joinToString("") }
    t("stringCompareNull") { (null as String?).equals(null) }
    t("toUpperChar") { "ß".uppercase() to 'ß'.uppercaseChar() }
    t("lowercaseTurkish") { "İ".lowercase().length }
    t("isLetterDigit") { listOf('٣'.isDigit(), '٣'.digitToIntOrNull(), 'Ⅻ'.isLetter()) }
    t("ifBlank") { "  ".ifBlank { "dflt" } to "x".ifEmpty { "e" } }
    t("orEmptyStr") { (null as String?).orEmpty().length }
    t("strLinesSeq") { "a\r\nb\rc".lines() }
    t("indent") { "a\nb".prependIndent("> ") }
    t("replaceIndent") { "  a\n    b".replaceIndent("|") }
    t("trimMarginCustom") { "#a\n  #b".trimMargin("#") }
    t("format0") { String.format("%3$2s %1$2s %2$2s", "a", "b", "c") }
    extra4()
    extra5()
}
fun extra4() {
    t("replaceAfterChar") { "a.b".replaceAfter('.', "Q") }
    t("replaceAfterMissing") { "abc".replaceAfter("/", "Q") to "abc".replaceBefore("/", "Q", "miss") }
    t("replaceBeforeLastMulti") { "x::y::z".replaceBeforeLast("::", "W") }
}
fun extra5() {
    t("windowedSeqStr") { "abcde".windowedSequence(2, 2, true).toList() }
    t("chunkedSeqTransform") { "abcdef".chunkedSequence(2) { it.reversed() }.toList() }
}"##
        ),
        "splitChars: [a, b, c, d]\nsplitCharsLimit: [a, b,c,d]\nsplitCharCI: [a, b, c]\nsplitMultiStrLimit: [a, b, c--d]\ntrimEndChars: abc\ntrimStartChars: ab\nstartsWithChar: (true, true)\nremoveSurroundingSame: q\nsubstringAfterLast: (c, none)\nsubstringBeforeChar: (a, b.c)\nreplaceAfter: (a.X, Y.b.c)\nreplaceAfterLast: (a.b.Z, W.c)\npadEndChar: ab***\nchunkedSeqStr: [abc, def, g]\ntakeLastStr: (llo, hel)\ntakeWhileStr: (aa, b)\ntakeLastWhileStr: bb\nfilterIndexedStr: ac\nmapIndexedStr: [0a, 1b]\nanyStr: (true, true)\nfirstOrNullStr: (b, null)\nsingleStr: a\nsingleStrBad: EX java.lang.IllegalArgumentException: Char sequence has more than one element.\nreduceStr: c\ncharRangeStep: adgj\nstringCompareNull: true\ntoUpperChar: (SS, ß)\nlowercaseTurkish: 2\nisLetterDigit: [true, 3, false]\nifBlank: (dflt, x)\norEmptyStr: 0\nstrLinesSeq: [a, b, c]\nindent: > a\n> b\nreplaceIndent: |a\n|  b\ntrimMarginCustom: a\nb\nformat0:  c  a  b\nreplaceAfterChar: a.Q\nreplaceAfterMissing: (abc, miss)\nreplaceBeforeLastMulti: W::z\nwindowedSeqStr: [ab, cd, e]\nchunkedSeqTransform: [ba, dc, fe]\n"
    );
}

#[test]
fn when_in_a_literal_range_tests_membership_not_magnitude_for_an_untyped_subject() {
    // Measured on kotlinc 2.4.21 / JDK 25. A subject of no primitive static
    // type is tested with `IntRange.contains`, so only an `Int` can be a
    // member: `2.0`, a boxed `Long`, `"2"` and `null` all fall to `!in 1..100`.
    // A `Double` subject still compares by magnitude against a `Double` range,
    // and a statically `Int` one keeps the native comparison.
    assert_eq!(
        stdout(
            r#"
fun d(x: Any?) = when (x) {
    in 1..3 -> "small"
    !in 1..100 -> "out"
    else -> "big"
}
fun main() {
    println(listOf<Any?>(2, 2.0, 5L, "2", null, 50).map { d(it) })
    val y = 2.5
    println(when (y) { in 1.0..3.0 -> "in"; else -> "out" })
    val n = 3
    println(when (n) { in 1..3 -> "edge"; else -> "past" })
}"#
        ),
        "[small, out, out, out, out, big]\nin\nedge\n"
    );
}

#[test]
fn the_to_destination_family_and_the_array_list_capacity_constructor() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `filterTo`, `mapTo`,
    // `associateTo`, `groupByTo` and the rest of the `…To(destination)`
    // family, and `toCollection`, were `unresolved reference`;
    // `ArrayList(10)` was read as a copy of the Int `10`.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    val xs = listOf(3, 1, 4, 1, 5, 9, 2, 6)
    t("filterTo") { val d = mutableListOf(0); xs.filterTo(d) { it > 4 }; d }
    t("mapTo") { xs.mapTo(mutableSetOf()) { it % 3 } }
    t("toCollection") { xs.toCollection(sortedSetOf()) }
    t("associateTo") { xs.associateTo(sortedMapOf()) { it to it * it } }
    t("associateByTo") { listOf("a", "bb").associateByTo(mutableMapOf(0 to "z")) { it.length } }
    t("groupByTo") { xs.groupByTo(sortedMapOf()) { it % 2 } }
    t("flatMapTo") { listOf(1, 2).flatMapTo(mutableListOf(-1)) { listOf(it, it) } }
    t("mapNotNullTo") { listOf("1", "x").mapNotNullTo(mutableListOf()) { it.toIntOrNull() } }
    t("filterIndexedTo") { xs.filterIndexedTo(mutableListOf()) { i, _ -> i < 2 } }
    t("mapIndexedNotNull") { xs.mapIndexedNotNull { i, v -> if (i % 3 == 0) v else null } }
    t("filterNotTo") { xs.filterNotTo(hashSetOf()) { it < 5 } }
    t("zipTransform") { listOf(1, 2).zip(listOf(10, 20)) { a, b -> a + b } }
    t("foldRight") { listOf("a", "b").foldRight("!") { s, acc -> s + acc } }
    t("foldIndexed") { xs.foldIndexed(0) { i, acc, v -> acc + i * v } }
    t("scan") { listOf(1, 2, 3).scan(0) { a, b -> a + b } }
    t("runningFoldIndexed") { listOf(1, 2).runningFoldIndexed(10) { i, a, b -> a + i + b } }
    t("reduceRightIndexed") { listOf(1, 2, 3).reduceRightIndexed { i, v, acc -> acc - v * i } }
    t("reduceOrNull") { listOf<Int>().reduceOrNull { a, b -> a + b } }
    t("associateByKV") { listOf("a", "bb").associateBy({ it.length }, { it.uppercase() }) }
    t("groupByKV") { listOf("a", "bb", "c").groupBy({ it.length }, { it.uppercase() }) }
    t("mapKeys") { mapOf(1 to "a").mapKeys { it.key * 10 } }
    t("mapValuesTo") { mapOf(1 to 2).mapValuesTo(mutableMapOf(0 to 0)) { it.value + 1 } }
    t("mapKeysTo") { mapOf(1 to 2).mapKeysTo(sortedMapOf(5 to 5)) { -it.key } }
    t("mapFilterMap") { mapOf(1 to 2, 3 to 4).filter { (k, v) -> k + v > 3 }.map { (k, v) -> k * v } }
    t("mapMinBy") { mapOf("a" to 3, "b" to 1).minByOrNull { it.value }?.key }
    t("mapForEachBi") { val sb = StringBuilder(); mapOf(1 to 2).forEach { k, v -> sb.append(k + v) }; sb }
    t("mapToSorted") { mapOf(3 to "c", 1 to "a").toSortedMap(compareByDescending { it }) }
    t("mapEntriesSorted") { mapOf("b" to 1, "a" to 2).entries.sortedBy { it.key }.map { it.value } }
    t("mapFlatMap") { mapOf(1 to listOf("a", "b")).flatMap { (k, vs) -> vs.map { "$k$it" } } }
    t("mapNotNullMap") { mapOf(1 to null, 2 to "b").mapNotNull { it.value } }
    t("mapAll") { mapOf(1 to 2).all { it.value > it.key } }
    t("mapAsIterable") { mapOf(1 to 2).asIterable().map { it.key } }
    t("mapFilterNot") { mapOf(1 to 2, 3 to 4).filterNot { it.key == 1 } }
    t("mapMaxOf") { mapOf(1 to 2, 3 to 4).maxOf { it.value } }
    t("mapSumOf") { mapOf(1 to 2, 3 to 4).entries.sumOf { it.key * it.value } }
    t("mapToMutable") { mapOf(1 to 2).toMutableMap().apply { put(3, 4) } }
    t("mapPlusMap") { mapOf(1 to 2) + mapOf(1 to 3, 4 to 5) }
    t("mapPlusList") { mapOf(1 to 2) + listOf(5 to 6) }
    t("mapMinusList") { mapOf(1 to 2, 3 to 4, 5 to 6) - listOf(1, 5) }
    t("listOfPairsToMap") { listOf(1 to "a", 1 to "b").toMap() }
    t("zipToMap") { listOf("a", "b").zip(listOf(1, 2)).toMap() }
    t("MutableListInit") { MutableList(3) { it * 2 }.apply { add(9) } }
    t("ListInit") { List(3) { "x$it" } }
    t("arrayListCap") { ArrayList<Int>(10).apply { add(1) } }
    t("linkedListLike") { ArrayDeque<Int>().apply { addFirst(1); addFirst(2); addLast(3) }.toList() }
    extra6()
}
fun extra6() {
    t("arrayListNeg") { ArrayList<String>(-1) }
    t("arrayListCopy") { ArrayList(listOf(1, 2)).apply { add(3) } }
    t("toCollectionList") { listOf(2, 1, 2).toCollection(mutableListOf(9)) }
    t("groupByToExisting") { val d = mutableMapOf(1 to mutableListOf(100)); listOf(1, 2, 3).groupByTo(d) { it % 2 }; d }
    t("mapToSeq") { sequenceOf(1, 2).mapTo(mutableListOf()) { it * 3 } }
    t("filterToStr") { "a1b2".filterTo(StringBuilder()) { it.isDigit() }.toString() }
    t("associateWithTo") { listOf("x").associateWithTo(sortedMapOf("a" to 0)) { it.length } }
}"##
        ),
        "filterTo: [0, 5, 9, 6]\nmapTo: [0, 1, 2]\ntoCollection: [1, 2, 3, 4, 5, 6, 9]\nassociateTo: {1=1, 2=4, 3=9, 4=16, 5=25, 6=36, 9=81}\nassociateByTo: {0=z, 1=a, 2=bb}\ngroupByTo: {0=[4, 2, 6], 1=[3, 1, 1, 5, 9]}\nflatMapTo: [-1, 1, 1, 2, 2]\nmapNotNullTo: [1]\nfilterIndexedTo: [3, 1]\nmapIndexedNotNull: [3, 1, 2]\nfilterNotTo: [5, 6, 9]\nzipTransform: [11, 22]\nfoldRight: ab!\nfoldIndexed: 131\nscan: [0, 1, 3, 6]\nrunningFoldIndexed: [10, 11, 14]\nreduceRightIndexed: 1\nreduceOrNull: null\nassociateByKV: {1=A, 2=BB}\ngroupByKV: {1=[A, C], 2=[BB]}\nmapKeys: {10=a}\nmapValuesTo: {0=0, 1=3}\nmapKeysTo: {-1=2, 5=5}\nmapFilterMap: [12]\nmapMinBy: b\nmapForEachBi: 3\nmapToSorted: {3=c, 1=a}\nmapEntriesSorted: [2, 1]\nmapFlatMap: [1a, 1b]\nmapNotNullMap: [b]\nmapAll: true\nmapAsIterable: [1]\nmapFilterNot: {3=4}\nmapMaxOf: 4\nmapSumOf: 14\nmapToMutable: {1=2, 3=4}\nmapPlusMap: {1=3, 4=5}\nmapPlusList: {1=2, 5=6}\nmapMinusList: {3=4}\nlistOfPairsToMap: {1=b}\nzipToMap: {a=1, b=2}\nMutableListInit: [0, 2, 4, 9]\nListInit: [x0, x1, x2]\narrayListCap: [1]\nlinkedListLike: [2, 1, 3]\narrayListNeg: EX java.lang.IllegalArgumentException: Illegal Capacity: -1\narrayListCopy: [1, 2, 3]\ntoCollectionList: [9, 2, 1, 2]\ngroupByToExisting: {1=[100, 1, 3], 0=[2]}\nmapToSeq: [3, 6]\nfilterToStr: 12\nassociateWithTo: {a=0, x=1}\n"
    );
}

#[test]
fn a_user_class_implementing_iterable_gets_the_iterable_extensions() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `: Iterable<T>` was an
    // unresolved supertype; an `object : Iterator<T>` handed back through an
    // `Iterator`-typed value could not be called; and `a..b` over a class
    // declaring `rangeTo` typed as an `IntRange`, so `x in r` skipped the
    // user `contains`.
    assert_eq!(
        stdout(
            r##"import kotlin.reflect.KProperty
class Ver(val major: Int, val minor: Int) : Comparable<Ver> {
    override fun compareTo(other: Ver) = compareValuesBy(this, other, { it.major }, { it.minor })
    operator fun rangeTo(o: Ver) = VerRange(this, o)
    override fun toString() = "$major.$minor"
}
class VerRange(val start: Ver, val end: Ver) : Iterable<Ver> {
    operator fun contains(v: Ver) = v >= start && v <= end
    override fun iterator(): Iterator<Ver> = object : Iterator<Ver> {
        var cur = start
        override fun hasNext() = cur <= end
        override fun next(): Ver { val r = cur; cur = Ver(cur.major, cur.minor + 1); return r }
    }
}
class Countdown(val from: Int) { operator fun iterator() = (from downTo 1).iterator() }
class Logged<T>(var v: T) { operator fun getValue(r: Any?, p: KProperty<*>): T { println("get ${p.name}"); return v }
    operator fun setValue(r: Any?, p: KProperty<*>, nv: T) { println("set ${p.name}=$nv"); v = nv } }
class Holder { var x: Int by Logged(1) }
class Key(val k: String) { override fun equals(other: Any?) = other is Key && other.k.lowercase() == k.lowercase(); override fun hashCode() = k.lowercase().hashCode() }
data class Cell(val r: Int, val c: Int)
class Grid { operator fun get(c: Cell) = c.r * 10 + c.c; operator fun invoke(r: Int) = "row $r" }
fun main() {
    val r = Ver(1, 0)..Ver(1, 3)
    println(Ver(1, 2) in r); println(Ver(2, 0) in r); println(r.map { it.toString() })
    for (v in r) print("$v "); println()
    for (i in Countdown(3)) print(i); println()
    val h = Holder(); h.x = 5; println(h.x + 1)
    println(setOf(Key("A"), Key("a"), Key("b")).size); println(mapOf(Key("X") to 1)[Key("x")])
    val g = Grid(); println(g[Cell(2, 3)]); println(g(7))
    val cells = listOf(Cell(1, 2), Cell(3, 4)); for ((a, b) in cells) print("${a + b} "); println()
    println(listOf(Ver(1, 5), Ver(1, 2), Ver(0, 9)).sorted()); println(listOf(Ver(1, 5), Ver(0, 9)).maxOrNull())
    println(Ver(1, 1) == Ver(1, 1)); println(Ver(1, 1).compareTo(Ver(1, 1)) == 0)
}"##
        ),
        "true\nfalse\n[1.0, 1.1, 1.2, 1.3]\n1.0 1.1 1.2 1.3 \n321\nset x=5\nget x\n6\n2\n1\n23\nrow 7\n3 7 \n[0.9, 1.2, 1.5]\n1.5\nfalse\ntrue\n"
    );
    assert_eq!(
        stdout(
            r##"class Bag(private val xs: List<Int>) : Iterable<Int> {
    override fun iterator() = xs.iterator()
    fun total() = sum()
    fun evens() = this.filter { it % 2 == 0 }
}
class Fib(val n: Int) : Iterable<Long> {
    override fun iterator(): Iterator<Long> = object : Iterator<Long> {
        var a = 0L; var b = 1L; var i = 0
        override fun hasNext() = i < n
        override fun next(): Long { val r = a; a = b; b += r; i++; return r }
    }
}
fun main() {
    val b = Bag(listOf(3, 1, 4, 1, 5))
    println(b.map { it * 2 }); println(b.sum()); println(b.total()); println(b.evens())
    println(b.toList()); println(b.sorted()); println(b.count { it > 1 }); println(b.first()); println(b.joinToString("-"))
    println(4 in b); println(9 in b); println(b.contains(5)); println(b.any { it > 4 }); println(b.maxOrNull())
    println(b.toSet()); println(b.groupBy { it % 2 }); println(b.zip(listOf("a", "b"))); println(b.withIndex().toList())
    var s = 0; b.forEach { s += it }; println(s)
    for (x in b) print(x); println()
    println(Fib(10).toList()); println(Fib(10).sum()); println(Fib(5).map { it * it })
    println(Fib(90).last()); println(Fib(3).joinToString())
    val it2: Iterable<Int> = b; println(it2.map { it + 1 })
    println(b.asSequence().filter { it > 2 }.toList())
    println(b.chunked(2)); println(b.reversed()); println(b.distinct()); println(b.indexOf(4)); println(b.elementAt(2))
}"##
        ),
        "[6, 2, 8, 2, 10]\n14\n14\n[4]\n[3, 1, 4, 1, 5]\n[1, 1, 3, 4, 5]\n3\n3\n3-1-4-1-5\ntrue\nfalse\ntrue\ntrue\n5\n[3, 1, 4, 5]\n{1=[3, 1, 1, 5], 0=[4]}\n[(3, a), (1, b)]\n[IndexedValue(index=0, value=3), IndexedValue(index=1, value=1), IndexedValue(index=2, value=4), IndexedValue(index=3, value=1), IndexedValue(index=4, value=5)]\n14\n31415\n[0, 1, 1, 2, 3, 5, 8, 13, 21, 34]\n88\n[0, 1, 1, 4, 9]\n1779979416004714189\n0, 1, 1\n[4, 2, 5, 2, 6]\n[3, 4, 5]\n[[3, 1], [4, 1], [5]]\n[5, 1, 4, 1, 3]\n[3, 1, 4, 5]\n2\n4\n"
    );
    assert_eq!(
        stdout(
            r##"class C : Iterator<Int> { var n = 0; override fun hasNext() = n < 2; override fun next() = n++ }
fun mk(): Iterator<Int> = object : Iterator<Int> { var n = 0; override fun hasNext() = n < 2; override fun next() = n++ }
fun main() {
    val a: Iterator<Int> = C(); println(a.hasNext())
    val b = mk(); println(b.hasNext()); println(b.next())
    val c: Any = C(); println((c as Iterator<*>).hasNext())
}"##
        ),
        "true\ntrue\n0\ntrue\n"
    );
}

#[test]
fn an_object_initializer_that_throws_is_an_exception_in_initializer_error() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. The first access raises
    // `ExceptionInInitializerError` (its cause the exception; an `Error`
    // passes through), every later one `NoClassDefFoundError`. An object whose
    // property initializer called a stdlib function was built eagerly at
    // program start, so its fault surfaced at an unrelated access.
    assert_eq!(
        stdout(
            r##"object O { init { println("init O"); if (true) throw IllegalStateException("in init") }; val x = 1 }
object E { val y: Int = error("boom") }
object S { init { throw StackOverflowError() } ; val z = 2 }
class C { companion object { init { println("comp init"); throw IllegalArgumentException("comp") }; val k = 3 } }
fun main() {
    try { println(O.x) } catch (e: Throwable) { println("1 $e / ${e.cause} / ${e.message}") }
    try { println(O.x) } catch (e: Throwable) { println("2 $e / ${e.cause}") }
    try { println(E.y) } catch (e: Throwable) { println("3 $e / ${e.cause}") }
    try { println(S.z) } catch (e: Throwable) { println("4 $e / ${e.cause}") }
    try { println(C.k) } catch (e: Throwable) { println("5 $e / ${e.cause}") }
    try { println(C.k) } catch (e: Throwable) { println("6 $e / ${e.cause}") }
    try { println(O.x) } catch (e: ExceptionInInitializerError) { println("7 eiie") } catch (e: Throwable) { println("7 other $e") }
}"##
        ),
        "init O\n1 java.lang.ExceptionInInitializerError / java.lang.IllegalStateException: in init / null\n2 java.lang.NoClassDefFoundError: Could not initialize class O / java.lang.ExceptionInInitializerError: Exception java.lang.IllegalStateException: in init [in thread \"main\"]\n3 java.lang.ExceptionInInitializerError / java.lang.IllegalStateException: boom\n4 java.lang.StackOverflowError / null\ncomp init\n5 java.lang.ExceptionInInitializerError / java.lang.IllegalArgumentException: comp\n6 java.lang.NoClassDefFoundError: Could not initialize class C / java.lang.ExceptionInInitializerError: Exception java.lang.IllegalArgumentException: comp [in thread \"main\"]\n7 other java.lang.NoClassDefFoundError: Could not initialize class O\n"
    );
    assert_eq!(
        stdout(
            r##"object A { init { println("A init") }; val v = B.w + 1 }
object B { init { println("B init") }; val w = 10 * 2 }
object Cfg { val list = listOf(1, 2).map { it * 2 }; val n = list.size }
object Plain { val k = 5; val s = "x" }
fun main() {
    println("main")
    println(Cfg.n)
    println(A.v)
    println(B.w)
    println(Plain.k)
}"##
        ),
        "main\n2\nA init\nB init\n21\n20\n5\n"
    );
    let out = eval("object O { val x: Int = error(\"boom\") }\nfun main() { println(\"start\"); println(O.x) }");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "start\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("java.lang.ExceptionInInitializerError"));
}

#[test]
fn a_write_through_a_map_view_reaches_the_map() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `keys`/`values`/`entries`
    // were detached copies, so a removal through one left the map whole.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("keysRemove") { val m = mutableMapOf(1 to 2, 3 to 4); println(m.keys.remove(1)); m }
    t("keysRemoveAbsent") { val m = mutableMapOf(1 to 2); println(m.keys.remove(9)); m }
    t("valuesRemove") { val m = mutableMapOf(1 to 4, 2 to 4, 3 to 5); println(m.values.remove(4)); m }
    t("entriesIter") { val m = mutableMapOf(1 to 2, 3 to 4); val it = m.entries.iterator(); it.next(); it.remove(); m }
    t("keysIter") { val m = mutableMapOf("a" to 1, "b" to 2, "c" to 3); val it = m.keys.iterator(); while (it.hasNext()) if (it.next() == "b") it.remove(); m }
    t("valuesIter") { val m = linkedMapOf(1 to "x", 2 to "y"); val it = m.values.iterator(); it.next(); it.remove(); m }
    t("keysRemoveAll") { val m = mutableMapOf(1 to 1, 2 to 2, 3 to 3); m.keys.removeAll(listOf(1, 3)); m }
    t("keysRetainAll") { val m = mutableMapOf(1 to 1, 2 to 2, 3 to 3); m.keys.retainAll(setOf(2)); m }
    t("valuesRemoveIf") { val m = mutableMapOf(1 to 10, 2 to 20, 3 to 30); m.values.removeIf { it > 15 }; m }
    t("entriesRemoveIf") { val m = mutableMapOf(1 to 10, 2 to 20); m.entries.removeIf { it.key == 1 }; m }
    t("keysRemoveAllLambda") { val m = mutableMapOf(1 to 1, 2 to 2); m.keys.removeAll { it == 2 }; m }
    t("keysClear") { val m = mutableMapOf(1 to 1); m.keys.clear(); m }
    t("valuesClear") { val m = mutableMapOf(1 to 1); m.values.clear(); m.size }
    t("keysAdd") { mutableMapOf(1 to 1).keys.add(5) }
    t("viewAfter") { val m = mutableMapOf(1 to 2, 3 to 4); val k = m.keys; k.remove(1); k }
    t("hashMapKeys") { val m = hashMapOf("x" to 1, "y" to 2, "z" to 3); m.keys.remove("y"); m }
    t("entriesRemove") { val m = mutableMapOf(1 to 2, 3 to 4); val e = m.entries.first(); m.entries.remove(e); m }
}"##
        ),
        "true\nkeysRemove: {3=4}\nfalse\nkeysRemoveAbsent: {1=2}\ntrue\nvaluesRemove: {2=4, 3=5}\nentriesIter: {3=4}\nkeysIter: {a=1, c=3}\nvaluesIter: {2=y}\nkeysRemoveAll: {2=2}\nkeysRetainAll: {2=2}\nvaluesRemoveIf: {1=10}\nentriesRemoveIf: {2=20}\nkeysRemoveAllLambda: {1=1}\nkeysClear: {}\nvaluesClear: 0\nkeysAdd: EX java.lang.UnsupportedOperationException\nviewAfter: [3]\nhashMapKeys: {x=1, z=3}\nentriesRemove: {3=4}\n"
    );
}

#[test]
fn a_for_loop_over_a_collection_the_body_resizes_follows_the_jdk_iterator() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: a size change raises
    // `ConcurrentModificationException` at the next pass, unless the cursor
    // now sits at the end — removing the second-to-last element ends the loop
    // quietly. The indexed walk used to shift the next element under the
    // index instead.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("removeFirst") { val l = mutableListOf(1, 2, 3); for (x in l) l.remove(x); l }
    t("removeSecondToLast") { val l = mutableListOf(1, 2, 3); for (x in l) if (x == 2) l.remove(x); l }
    t("removeLast") { val l = mutableListOf(1, 2, 3); for (x in l) if (x == 3) l.remove(x); l }
    t("addDuring") { val l = mutableListOf(1, 2); for (x in l) if (x == 1) l.add(9); l }
    t("addAtLast") { val l = mutableListOf(1, 2); for (x in l) if (x == 2) l.add(9); l }
    t("setDuring") { val l = mutableListOf(1, 2); for (x in l) l[0] = 5; l }
    t("mapPut") { val m = mutableMapOf(1 to 1, 2 to 2); for ((k, _) in m) m[k + 10] = 0; m }
    t("setRemove") { val s = mutableSetOf(1, 2, 3); for (x in s) if (x == 1) s.remove(x); s }
    t("indexLoop") { val l = mutableListOf(1, 2, 3); for (i in l.indices) if (i == 0) l.removeAt(0); l }
    t("noChange") { var s = 0; for (x in mutableListOf(1, 2, 3)) s += x; s }
    t("arr") { val a = intArrayOf(1, 2); var s = 0; for (x in a) { a[1] = 7; s += x }; s }
    t("breakAfter") { val l = mutableListOf(1, 2, 3); for (x in l) { l.add(4); break }; l }
}"##
        ),
        "removeFirst: EX java.util.ConcurrentModificationException\nremoveSecondToLast: [1, 3]\nremoveLast: EX java.util.ConcurrentModificationException\naddDuring: EX java.util.ConcurrentModificationException\naddAtLast: EX java.util.ConcurrentModificationException\nsetDuring: [5, 2]\nmapPut: EX java.util.ConcurrentModificationException\nsetRemove: EX java.util.ConcurrentModificationException\nindexLoop: [2, 3]\nnoChange: 6\narr: 8\nbreakAfter: [1, 2, 3, 4]\n"
    );
}

#[test]
fn a_write_through_a_sub_list_reaches_the_backing_list() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `subList` was a detached
    // copy, so `xs.subList(1, 3).clear()` — the idiomatic range removal — and
    // every other write through the view left `xs` unchanged.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
@Suppress("UNCHECKED_CAST") fun <T> m(l: List<T>) = l as MutableList<T>
fun main() {
    t("clearRange") { val l = mutableListOf(1, 2, 3, 4, 5); l.subList(1, 3).clear(); l }
    t("reverseRange") { mutableListOf(1, 2, 3, 4).also { it.subList(1, 3).reverse() } }
    t("sortRange") { mutableListOf(5, 4, 3, 2, 1).also { it.subList(0, 3).sort() } }
    t("setThrough") { val l = mutableListOf(1, 2, 3); val s = l.subList(1, 3); s[0] = 9; l }
    t("addThrough") { val l = mutableListOf(1, 2, 3); val s = l.subList(0, 1); s.add(7); println(s); l }
    t("removeIfThrough") { val l = mutableListOf(1, 2, 3, 4); l.subList(1, 4).removeIf { it % 2 == 0 }; l }
    t("fillThrough") { val l = MutableList(5) { 0 }; l.subList(1, 3).fill(8); l }
    t("twoWrites") { val l = mutableListOf(1, 2, 3, 4); val s = l.subList(1, 3); s.add(5); s.removeAt(0); println(s); l }
    t("roSubAdd") { m(listOf(1, 2, 3).subList(0, 2)).add(3) }
    t("roSubSet") { val base = listOf(1, 2, 3); m(base.subList(0, 2))[0] = 9; base }
    t("sortWithThrough") { mutableListOf(1, 3, 2, 0).also { it.subList(1, 4).sortWith(compareByDescending { x -> x }) } }
    t("shuffleNoop") { val l = mutableListOf(1, 2); l.subList(0, 1).shuffle(); l }
    t("readOnlyView") { val l = mutableListOf(1, 2, 3); l.subList(0, 2).sum() }
}"##
        ),
        "clearRange: [1, 4, 5]\nreverseRange: [1, 3, 2, 4]\nsortRange: [3, 4, 5, 2, 1]\nsetThrough: [1, 9, 3]\n[1, 7]\naddThrough: [1, 7, 2, 3]\nremoveIfThrough: [1, 3]\nfillThrough: [0, 8, 8, 0, 0]\n[3, 5]\ntwoWrites: [1, 3, 5, 4]\nroSubAdd: EX java.lang.UnsupportedOperationException\nroSubSet: [9, 2, 3]\nsortWithThrough: [1, 3, 2, 0]\nshuffleNoop: [1, 2]\nreadOnlyView: 3\n"
    );
}

#[test]
fn unbounded_plain_recursion_raises_stack_overflow_error() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. Direct, mutual and method
    // recursion without a base case grew the frame stack until the process
    // was killed; the JVM raises a catchable `StackOverflowError` (near 45 000
    // frames there, a million here). A deep FINITE recursion still completes.
    assert_eq!(
        stdout(
            r##"var depth = 0
fun f(n: Int): Int { depth = n; return f(n + 1) + 1 }
fun even(n: Int): Boolean = if (n == 0) true else odd(n - 1)
fun odd(n: Int): Boolean = if (n == 0) false else even(n - 1)
class Node { fun walk(): Int = walk() + 1 }
fun main() {
    try { f(0) } catch (e: StackOverflowError) { println("caught ${e}") }
    println(depth > 40000)
    try { println(even(-1)) } catch (e: StackOverflowError) { println("mutual $e") }
    try { Node().walk() } catch (e: Throwable) { println("method $e") }
    println(even(10000))
    println("after")
}"##
        ),
        "caught java.lang.StackOverflowError\ntrue\nmutual java.lang.StackOverflowError\nmethod java.lang.StackOverflowError\ntrue\nafter\n"
    );
    let out = eval("fun f(n: Int): Int = f(n + 1) + 1\nfun main() { println(f(0)) }");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("java.lang.StackOverflowError"));
}

#[test]
fn unit_is_a_value_that_prints_kotlin_unit_and_is_not_null() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `Unit` and `null` were one
    // runtime value, so a lambda's or a function's `Unit` result printed
    // `null` wherever its static type was lost, compared equal to `null`, and
    // `x?.let { println(it) }` read as null.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun u() {}
fun early(x: Int) { if (x > 0) return; println("neg") }
class C { fun m() {} }
fun main() {
    t("lambda") { val f = { x: Int -> println(x) }; f(1) }
    t("anon") { val g = fun(s: String) { s.length }; g("a") }
    t("letNonNull") { val x: Int? = 5; val r = x?.let { println(it) }; r == null }
    t("elvis") { val x: Int? = 5; x?.let { println(it) } ?: "other" }
    t("clear") { mutableListOf(1).clear() }
    t("forEach") { listOf(1).forEach { } }
    t("early") { early(1) }
    t("method") { C().m() }
    t("anyUnit") { val a: Any = u(); a.toString() }
    t("eqUnit") { u() == Unit }
    t("runUnit") { run { println("r") } }
    t("applyUnit") { "x".also { } }
    t("mapUnit") { listOf(1, 2).map { } }
    t("mapNotNullUnit") { listOf(1).mapNotNull { println(it) } }
    t("unitHash") { Unit.hashCode() == Unit.hashCode() }
    t("isUnit") { val a: Any = u(); a is Unit }
    t("unitString") { "${u()}" + Unit }
    t("result") { runCatching { println("x") } }
    t("lazyUnit") { val l by lazy { println("l") }; l }
    t("ifUnit") { if (true) println("a") else println("b") }
    t("whenUnit") { when (1) { 1 -> println("one"); else -> {} } }
    t("assignLambda") { var v = 0; val set = { v = 5 }; set() }
    t("unitList") { listOf(Unit, null) }
    more()
}
fun more() {
    t("sortUnit") { mutableListOf(2, 1).sort() }
    t("addIdx") { mutableListOf(1).add(0, 5) }
    t("putAllUnit") { mutableMapOf(1 to 1).putAll(mapOf(2 to 2)) }
    t("fillUnit") { IntArray(2).fill(3) }
}"##
        ),
        "1\nlambda: kotlin.Unit\nanon: kotlin.Unit\n5\nletNonNull: false\n5\nelvis: kotlin.Unit\nclear: kotlin.Unit\nforEach: kotlin.Unit\nearly: kotlin.Unit\nmethod: kotlin.Unit\nanyUnit: kotlin.Unit\neqUnit: true\nr\nrunUnit: kotlin.Unit\napplyUnit: x\nmapUnit: [kotlin.Unit, kotlin.Unit]\n1\nmapNotNullUnit: [kotlin.Unit]\nunitHash: true\nisUnit: true\nunitString: kotlin.Unitkotlin.Unit\nx\nresult: Success(kotlin.Unit)\nl\nlazyUnit: kotlin.Unit\na\nifUnit: kotlin.Unit\none\nwhenUnit: kotlin.Unit\nassignLambda: kotlin.Unit\nunitList: [kotlin.Unit, null]\nsortUnit: kotlin.Unit\naddIdx: kotlin.Unit\nputAllUnit: kotlin.Unit\nfillUnit: kotlin.Unit\n"
    );
}

#[test]
fn as_list_is_an_arrays_array_list_over_the_array() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: a size change through
    // `array.asList()` is refused, a `set` or `sort` writes the array, and an
    // out-of-range index is the ARRAY subclass at every size.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
@Suppress("UNCHECKED_CAST") fun <T> m(l: List<T>) = l as MutableList<T>
fun main() {
    t("add") { m(arrayOf(1, 2).asList()).add(3) }
    t("add1") { m(arrayOf(1).asList()).add(3) }
    t("set") { val a = arrayOf(1, 2); m(a.asList())[0] = 9; a.toList() }
    t("set1") { val a = arrayOf(1); m(a.asList())[0] = 9; a.toList() }
    t("sortThrough") { val a = arrayOf(3, 1, 2); m(a.asList()).sort(); a.toList() }
    t("remove") { m(arrayOf(1, 2).asList()).remove(1) }
    t("clear") { m(arrayOf(1, 2).asList()).clear() }
    t("idx") { arrayOf(1).asList()[5] }
}"##
        ),
        "add: EX java.lang.UnsupportedOperationException\nadd1: EX java.lang.UnsupportedOperationException\nset: [9, 2]\nset1: [9]\nsortThrough: [1, 2, 3]\nremove: EX java.lang.UnsupportedOperationException: remove\nclear: EX java.lang.UnsupportedOperationException\nidx: EX java.lang.ArrayIndexOutOfBoundsException: Index 5 out of bounds for length 1\n"
    );
}

#[test]
fn a_same_size_structural_change_during_a_for_loop_is_detected() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: an `add` and a `remove` in
    // one pass, or a `sort`/`replaceAll` of two or more elements, bump the
    // JDK's `modCount` without changing the size; a `set`, a `put` over an
    // existing key and a `shuffle` do not.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("addRemove") { val l = mutableListOf(1, 2, 3); for (x in l) if (x == 1) { l.add(9); l.removeAt(3) }; l }
    t("sortDuring") { val l = mutableListOf(3, 1, 2); for (x in l) if (x == 3) l.sort(); l }
    t("sortOne") { val l = mutableListOf(1); for (x in l) l.sort(); l }
    t("replaceAllDuring") { val l = mutableListOf(1, 2); for (x in l) l.replaceAll { it * 2 }; l }
    t("setDuring") { val l = mutableListOf(1, 2); for (x in l) l[1] = 5; l }
    t("putExisting") { val m = mutableMapOf(1 to 1, 2 to 2); for ((k, v) in m) m[k] = v + 1; m }
    t("putNewRemove") { val m = mutableMapOf(1 to 1, 2 to 2, 3 to 3); for ((k, _) in m) if (k == 1) { m[9] = 9; m.remove(9) }; m }
    t("shuffleDuring") { val l = mutableListOf(1, 2, 3); var n = 0; for (x in l) { l.shuffle(java.util.Random(1)); n++ }; n }
    t("removeIfNone") { val l = mutableListOf(1, 2); for (x in l) l.removeIf { it > 9 }; l }
    t("addRemoveSet") { val s = mutableSetOf(1, 2, 3); for (x in s) if (x == 1) { s.add(9); s.remove(9) }; s }
    t("sortByDuring") { val l = mutableListOf("b", "a"); for (x in l) l.sortBy { it }; l }
}"##
        ),
        "addRemove: EX java.util.ConcurrentModificationException\nsortDuring: EX java.util.ConcurrentModificationException\nsortOne: [1]\nreplaceAllDuring: EX java.util.ConcurrentModificationException\nsetDuring: [1, 5]\nputExisting: {1=2, 2=3}\nputNewRemove: EX java.util.ConcurrentModificationException\nshuffleDuring: 3\nremoveIfNone: [1, 2]\naddRemoveSet: EX java.util.ConcurrentModificationException\nsortByDuring: EX java.util.ConcurrentModificationException\n"
    );
}

#[test]
fn a_class_implements_a_collection_interface_by_a_delegate() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `: MutableList<String> by
    // inner` was rejected; every member the class does not declare now
    // reaches the delegate, `+=` is the virtual `add`, and an override wins.
    assert_eq!(
        stdout(
            r##"class Logger(private val inner: MutableList<String> = mutableListOf()) : MutableList<String> by inner {
    override fun add(element: String): Boolean { println("adding $element"); return inner.add(element.uppercase()) }
}
class Registry(private val m: MutableMap<String, Int> = mutableMapOf()) : MutableMap<String, Int> by m
class Names(names: List<String>) : List<String> by names { fun shout() = map { it.uppercase() } }
fun main() {
    val lg = Logger(); lg.add("a"); lg += "b"; lg.addAll(listOf("c"))
    println(lg.size); println(lg[0]); println(lg.joinToString()); println("A" in lg); println(lg.map { it.lowercase() })
    for (x in lg) print(x); println()
    lg.removeAt(0); println(lg.size); println(lg.isEmpty())
    val r = Registry(); r["x"] = 1; r.put("y", 2); println(r.size); println(r["x"]); println(r.keys); println(r.getOrDefault("z", 0))
    val n = Names(listOf("ann", "bo")); println(n.shout()); println(n.first()); println(n.size); println(n.indexOf("bo"))
    println(lg.toString().startsWith("Logger@"))
}"##
        ),
        "adding a\nadding b\n3\nA\nA, B, c\ntrue\n[a, b, c]\nABc\n2\nfalse\n2\n1\n[x, y]\n0\n[ANN, BO]\nann\n2\n1\ntrue\n"
    );
}

#[test]
fn an_iterable_user_object_without_a_static_class_reaches_the_extensions() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1: `b.let { it.toList() }`, a
    // generic `id(b)`, an `Any` cast to `Iterable<*>` and a list of
    // `Iterable<Int>` all walk the object's own iterator.
    assert_eq!(
        stdout(
            r##"class Bag(private val xs: List<Int>) : Iterable<Int> { override fun iterator() = xs.iterator() }
class Fib(val n: Int) : Iterable<Long> {
    override fun iterator(): Iterator<Long> = object : Iterator<Long> {
        var a = 0L; var b = 1L; var i = 0
        override fun hasNext() = i < n
        override fun next(): Long { val r = a; a = b; b += r; i++; return r }
    }
}
fun <T> id(x: T) = x
fun main() {
    val b = Bag(listOf(3, 1, 2))
    println(b.let { it.toList().size }); println(b.also { }.count())
    println(id(b).sorted()); println(id(b).map { it * 10 }); println(id(Fib(6)).toList()); println(id(Fib(5)).filter { it % 2 == 0L })
    val any: Any = b; println((any as Iterable<*>).joinToString())
    val xs: List<Iterable<Int>> = listOf(b, Bag(listOf(9))); println(xs.map { it.sum() })
    println(xs.flatMap { it.toList() })
}"##
        ),
        "3\n3\n[1, 2, 3]\n[30, 10, 20]\n[0, 1, 1, 2, 3, 5]\n[0, 2]\n3, 1, 2\n[6, 9]\n[3, 1, 2, 9]\n"
    );
}

#[test]
fn char_category_answers_from_the_jdk_type_data() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `Char.category` and the
    // `CharCategory` constants were `unresolved reference`.
    assert_eq!(
        stdout(
            r##"fun main() {
    println("aZ5 _-(é €«ǅ̀".map { it.category })
    println('a'.category.code); println('A'.category.value); println('1'.category.ordinal)
    println('a'.category == CharCategory.LOWERCASE_LETTER); println('a'.category == CharCategory.UPPERCASE_LETTER)
    println(CharCategory.MATH_SYMBOL); println('+'.category == CharCategory.MATH_SYMBOL)
    println(listOf('b', 'B', '9').sortedBy { it.category }); println('͸'.category)
    println((0..0xFFFF).count { it.toChar().category == CharCategory.OTHER_LETTER })
    println(when ('x'.category) { CharCategory.LOWERCASE_LETTER -> "lower"; else -> "other" })
}"##
        ),
        "[LOWERCASE_LETTER, UPPERCASE_LETTER, DECIMAL_DIGIT_NUMBER, SPACE_SEPARATOR, CONNECTOR_PUNCTUATION, DASH_PUNCTUATION, START_PUNCTUATION, LOWERCASE_LETTER, LINE_SEPARATOR, CURRENCY_SYMBOL, INITIAL_QUOTE_PUNCTUATION, TITLECASE_LETTER, NON_SPACING_MARK, PRIVATE_USE]\nLl\n1\n9\ntrue\nfalse\nMATH_SYMBOL\ntrue\n[B, b, 9]\nUNASSIGNED\n46126\nlower\n"
    );
}

#[test]
fn long_elements_and_results_keep_their_width_in_erased_positions() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. A `LongRange`'s elements,
    // `sum()`, `random()`, `LongRange(a, b)`, `absoluteValue` and `Int.mod(Long)`
    // read as `Int`s once erased, so `is Long` was false and `== 2L` missed.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("rangeFirst") { (1L..3L).first() is Long }
    t("rangeFilter") { (1L..3L).filter { it == 2L } }
    t("rangeSum") { (1L..3L).sum() is Long }
    t("rangeToList") { (1L..3L).toList().map { it is Long } }
    t("rangeMap") { (1L..3L).map { it * 3000000000L } }
    t("forRange") { var s = 0L; for (x in 1L..3L) s += x * 3000000000L; s }
    t("listOfLongLit") { listOf(1L, 2L)[0] is Long }
    t("absValue") { kotlin.math.abs(5L) is Long }
    t("modIntLong") { 5.mod(3L) is Long }
    t("modLongInt") { 5L.mod(3) is Int }
    t("eqAnyLong") { val a: Any = 5L; a == 5L }
    t("eqAnyInt") { val a: Any = 5; a == 5L }
    t("setLong") { setOf(1L, 2L).contains(1L) }
    t("mapKeyLong") { mapOf(1L to "a")[1L] }
    t("longStep") { (1L..10L step 3L).toList() }
    t("downTo") { (5L downTo 1L).last() is Long }
    t("untilLong") { (0L until 3L).count() }
    t("genLong") { generateSequence(1L) { if (it < 3L) it + 1 else null }.toList().map { it is Long } }
    t("longRangeContains") { 2L in 1L..3L }
}"##
        ),
        "rangeFirst: true\nrangeFilter: [2]\nrangeSum: true\nrangeToList: [true, true, true]\nrangeMap: [3000000000, 6000000000, 9000000000]\nforRange: 18000000000\nlistOfLongLit: true\nabsValue: true\nmodIntLong: true\nmodLongInt: true\neqAnyLong: true\neqAnyInt: false\nsetLong: true\nmapKeyLong: a\nlongStep: [1, 4, 7, 10]\ndownTo: true\nuntilLong: 3\ngenLong: [true, true, true]\nlongRangeContains: true\n"
    );
    assert_eq!(
        stdout(
            r##"import kotlin.math.absoluteValue
fun main() {
    println((1L..3L).random() is Long)
    println((1L..3L).asSequence().map { it is Long }.toList())
    println(LongRange(1L, 3L).filter { it == 2L })
    println(5L.absoluteValue is Long)
}"##
        ),
        "true\n[true, true, true]\n[2]\ntrue\n"
    );
}

#[test]
fn more_jvm_throwables_and_suppressed_exceptions() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.12.1. `AssertionError`,
    // `OutOfMemoryError`, `ConcurrentModificationException` (also as thrown by
    // a `for` loop), `InterruptedException`, `ArrayStoreException`, and
    // `addSuppressed`/`suppressed` were `unresolved reference`.
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("ae") { try { throw AssertionError("ae") } catch (e: Error) { e.toString() } }
    t("aeNull") { AssertionError().message }
    t("aeAny") { AssertionError(42).message }
    t("oom") { try { throw OutOfMemoryError("oom") } catch (e: VirtualMachineError) { e.message } }
    t("cme") { try { val l = mutableListOf(1, 2); for (x in l) l.add(3) } catch (e: ConcurrentModificationException) { "caught ${e.message}" } }
    t("cmeNew") { ConcurrentModificationException("x").toString() }
    t("interrupted") { try { throw InterruptedException("i") } catch (e: Exception) { e.toString() } }
    t("arrayStore") { ArrayStoreException("s").toString() }
    t("aeCatchException") { try { throw AssertionError("x") } catch (e: Exception) { "exc" } catch (e: Throwable) { "thr" } }
}"##
        ),
        "ae: java.lang.AssertionError: ae\naeNull: null\naeAny: 42\noom: oom\ncme: caught null\ncmeNew: java.util.ConcurrentModificationException: x\ninterrupted: java.lang.InterruptedException: i\narrayStore: java.lang.ArrayStoreException: s\naeCatchException: thr\n"
    );
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun main() {
    t("size") { val e = RuntimeException("x"); e.addSuppressed(Exception("s")); e.addSuppressed(Error("t")); e.suppressed.size }
    t("list") { val e = RuntimeException("x"); e.addSuppressed(Exception("s")); e.suppressedExceptions.map { it.message } }
    t("first") { val e = RuntimeException("x"); e.addSuppressed(IllegalStateException("s")); e.suppressed[0].toString() }
    t("none") { RuntimeException().suppressed.size }
    t("self") { val e = RuntimeException("x"); e.addSuppressed(e) }
    t("ret") { RuntimeException().addSuppressed(Exception()) }
}"##
        ),
        "size: 2\nlist: [s]\nfirst: java.lang.IllegalStateException: s\nnone: 0\nself: kotlin.Unit\nret: kotlin.Unit\n"
    );
    assert_eq!(
        stdout(
            r##"fun t(name: String, f: () -> Any?) { try { println("$name: ${f()}") } catch (e: Throwable) { println("$name: EX $e") } }
fun f1(): String { try { throw RuntimeException("a") } catch (e: Exception) { return "caught ${e.message}" } finally { println("fin1") } }
@Suppress("RETURN_IN_FINALLY") fun f2(): Int { try { return 1 } finally { return 2 } }
fun f3(): Int { var x = 0; try { x = 1; throw IllegalStateException() } catch (e: IllegalStateException) { x += 10 } finally { x += 100 }; return x }
fun f4() { try { try { throw IllegalArgumentException("inner") } finally { println("inner fin") } } catch (e: IllegalArgumentException) { println("outer caught ${e.message}") } }
fun f5(): String = try { "x".toInt().toString() } catch (e: NumberFormatException) { "nfe" } finally { println("fin5") }
fun f6(xs: List<Int>): Int { xs.forEach { if (it > 2) return it }; return -1 }
fun f7() { try { error("boom") } catch (e: Exception) { throw RuntimeException("wrapped", e) } }
class Res(val n: String) : AutoCloseable { override fun close() { println("close $n") } }
fun main() {
    t("f1") { f1() }; t("f2") { f2() }; t("f3") { f3() }; f4(); t("f5") { f5() }; t("f6") { f6(listOf(1, 3, 5)) }
    t("f7") { try { f7() } catch (e: RuntimeException) { "${e.message} <- ${e.cause?.message}" } }
    t("use") { Res("a").use { r -> println("using ${r.n}"); 42 } }
    t("useThrow") { Res("b").use { throw IllegalStateException("in use") } }
    t("typed") { try { listOf(1)[3] } catch (e: IndexOutOfBoundsException) { e::class.simpleName } }
    t("multiCatch") { try { "a".toInt() } catch (e: IllegalStateException) { "ise" } catch (e: IllegalArgumentException) { "iae:${e is NumberFormatException}" } }
    t("rethrowFin") { var log = ""; try { try { throw Exception("q") } finally { log += "f" } } catch (e: Exception) { log += "c" }; log }
    t("nothing") { val x: String? = null; x ?: throw IllegalArgumentException("null!") }
}"##
        ),
        "fin1\nf1: caught a\nf2: 2\nf3: 111\ninner fin\nouter caught inner\nfin5\nf5: nfe\nf6: 3\nf7: wrapped <- boom\nusing a\nclose a\nuse: 42\nclose b\nuseThrow: EX java.lang.IllegalStateException: in use\ntyped: IndexOutOfBoundsException\nmultiCatch: iae:true\nrethrowFin: fc\nnothing: EX java.lang.IllegalArgumentException: null!\n"
    );
}

/// The depth guard a recursive function carries is ONE op on its ordinary
/// path: the entry is `Extended(KT_DEPTH_GUARD)` and the body follows at once,
/// with the unwinding return (`LoadUndef; ReturnValue`) parked just before the
/// entry where only the guard's jump reaches it. It used to be the guard plus a
/// `JumpIfFalse` over that stub, two dispatched ops per call — about 10% of
/// `fib`'s instructions. A function that cannot recurse carries no guard.
#[test]
fn the_depth_guard_is_one_op_ahead_of_the_body() {
    let out = Command::new(env!("CARGO_BIN_EXE_kotlin"))
        .arg("--dump-bytecode")
        .arg("-e")
        .arg("fun leaf(n: Int) = n + 1\nfun fib(n: Int): Int = if (n < 2) leaf(n) else fib(n - 1) + fib(n - 2)\nfun main() { println(fib(5)) }")
        .output()
        .unwrap();
    let asm = String::from_utf8(out.stdout).unwrap();
    let entry = |name: &str| -> usize {
        let tag = format!(";   {name} @ ");
        let line = asm.lines().find(|l| l.starts_with(&tag)).unwrap();
        line[tag.len()..].trim().parse().unwrap()
    };
    let op = |ip: usize| -> &str {
        let at = format!("{ip:04} ");
        let line = asm.lines().find(|l| l.starts_with(&at)).unwrap();
        line.split_whitespace().nth(2).unwrap()
    };
    let fib = entry("fib");
    assert_eq!(op(fib), "Extended(157,", "fib's entry in:\n{asm}");
    assert!(!op(fib + 1).starts_with("JumpIf"), "fib's body in:\n{asm}");
    assert_eq!((op(fib - 2), op(fib - 1)), ("LoadUndef", "ReturnValue"));
    assert!(
        !op(entry("leaf")).starts_with("Extended(157"),
        "leaf in:\n{asm}"
    );
}

/// `mutableListOf<Long>(1)`, `ArrayDeque<Long>()`, `mutableMapOf<Long, Long>()` and their
/// annotated spellings (`val xs: MutableList<Long> = mutableListOf(1)`) type every
/// integer literal passed as an element, key or value as a `Long` — Kotlin types a
/// literal by its expected type. Each element below answered `is Long` false
/// before, and a `Map<String, Int>`'s `m[k]!! + m[k]!!` skipped the 32-bit wrap.
#[test]
fn explicit_and_annotated_element_types_make_literal_elements_long() {
    assert_eq!(
        prog(
            r##"fun main() {
    val a = mutableListOf<Long>(1)
    println(a[0] is Long)
    a.add(2)
    println(a)
    println(a.sum())
    val d = ArrayDeque<Long>()
    d.addLast(5)
    println(d.first() is Long)
    val s = setOf<Long>(1, 2)
    println(s.first() is Long)
    val m = mutableMapOf<Long, Long>(1L to 2L)
    m[3] = 4
    println(m[3] is Long)
    println(m.keys.last() is Long)
    val l = listOf<Double>(1.0, 2.0)
    println(l.sum())
    val x = arrayListOf<Long>(7)
    println(x[0] is Long)
    val e = emptyList<Long>()
    println(e.size)
    val ml = mutableListOf<Byte>(1, 2)
    println(ml[0] is Byte)
    val sh = mutableListOf<Short>(1)
    println(sh[0] is Short)
    val big = mutableListOf<Long>(3000000000)
    println(big[0] + 1)
    println(listOf<Long>(1, 2).map { it * 3000000000 })
}"##
        ),
        "true\n[1, 2]\n3\ntrue\ntrue\ntrue\ntrue\n3.0\ntrue\n0\ntrue\ntrue\n3000000001\n[3000000000, 6000000000]\n"
    );
    assert_eq!(
        prog(
            r##"fun sumAll(xs: List<Long>): Long = xs.sum()
fun main() {
    val m = mutableMapOf<Long, String>()
    m[1] = "a"; m.put(2, "b")
    println(m.keys.map { it is Long })
    println(m[1] + m.get(2) + m.containsKey(2) + m.getOrDefault(3, "z"))
    val n = HashMap<String, Long>()
    n["x"] = 5
    n["x"] = n["x"]!! + 1
    println(n["x"] is Long)
    println(n)
    val q: MutableMap<Long, Long> = mutableMapOf()
    q[7] = 8
    q[7] = q.getValue(7) * 3000000000
    println(q)
    println(q.keys.first() is Long)
    val c = mutableMapOf(1L to 2L)
    c[5] = 6
    println(c.entries.map { it.key is Long && it.value is Long })
    val l = mutableListOf<Long>()
    l.add(1); l.add(0, 2); l += 3
    println(l.map { it is Long })
    println(l.contains(3))
    println(l.indexOf(1))
    l.remove(2)
    println(l)
    println(sumAll(listOf(1, 2, 3)))
    val s = mutableSetOf<Long>(1, 2)
    s.add(3)
    println(s.map { it is Long })
    println(3 in s)
    val arr = arrayOf<Long>(1, 2)
    println(arr[0] is Long)
    val e = emptyList<Long>()
    println(e + 4L)
    val str = listOf<String>("a")
    println(str)
    val nested = listOf<List<Long>>(listOf(1L))
    println(nested[0][0] is Long)
}"##
        ),
        "[true, true]\nabtruez\ntrue\n{x=6}\n{7=24000000000}\ntrue\n[true, true]\n[true, true, true]\ntrue\n1\n[1, 3]\n6\n[true, true, true]\ntrue\ntrue\n[4]\n[a]\ntrue\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val big: MutableMap<String, Int> = mutableMapOf()
    big["k"] = 2000000000
    println(big["k"]!! + big["k"]!!)
    println(big["zz"])
    println(big["zz"] ?: 7)
    println((big["zz"] ?: 2000000000) + 2000000000)
    val x = big["zz"]
    println(x == null)
    val m = mutableMapOf("a" to 2000000000)
    println(m["a"]!! * 2)
    println("v=${m["a"]} ${m["q"]}")
    val d = mapOf("a" to 1.5)
    println(d["a"]!! / 2)
    val i = mapOf(1 to 7)
    println(i[1]!! / 2)
    val s = mapOf("k" to "v")
    println(s["k"] + "!")
    println(s["q"] + "!")
    val l = mapOf("k" to 3000000000L)
    println(l["k"]!! + 1)
    val cc = mapOf('a' to 'b')
    println(cc['a'])
    println(cc['z'])
}"##
        ),
        "-294967296\nnull\n7\n-294967296\ntrue\n-294967296\nv=2000000000 null\n0.75\n3\nv!\nnull!\n3000000001\nb\nnull\n"
    );
}

/// `subList`, `asList` (object and primitive arrays), `asReversed` and a map's
/// `keys`/`values`/`entries` read their backing object live, as the JDK's views
/// do, and a `subList` whose backing list changed structurally raises
/// `ConcurrentModificationException` on every later use. Each was a snapshot
/// before. The two loops pin linear cost: a write to the backing inside a loop
/// over the view must not re-copy the view per step.
#[test]
fn collection_views_read_their_backing_live() {
    assert_eq!(
        prog(
            r##"fun main() {
    val a = mutableListOf(1, 2, 3, 4)
    val s = a.subList(1, 3)
    a[1] = 20
    println(s)
    val arr = arrayOf(1, 2, 3)
    val v = arr.asList()
    arr[0] = 10
    println(v)
    val ia = intArrayOf(3, 1, 2)
    val il = ia.asList()
    ia.sort()
    println(il)
    val m = mutableMapOf("a" to 1)
    val ks = m.keys
    val vs = m.values
    val es = m.entries
    m["b"] = 2
    m["a"] = 10
    println("$ks $vs $es ${ks.size}")
    val rev = a.asReversed()
    a[0] = 100
    println(rev)
    a.add(5)
    try { println(s) } catch (e: ConcurrentModificationException) { println("CME print") }
    try { s.add(7) } catch (e: ConcurrentModificationException) { println("CME add") }
    try { println(listOf(s)) } catch (e: ConcurrentModificationException) { println("CME nested") }
    val h = HashMap<Int, Int>()
    for (i in 0 until 3000) h[i] = i
    for (k in h.keys) h[k] = k + 1
    for (k in h.keys) h.put(k, h.getValue(k) + 1)
    println(h.values.sum())
    val big = (1..3000).toMutableList()
    val bs = big.subList(0, 3000)
    var acc = 0L
    for (i in 0 until bs.size) { acc += bs[i]; big[0] = i }
    println(acc)
}"##
        ),
        "[20, 3]\n[10, 2, 3]\n[1, 2, 3]\n[a, b] [10, 2] [a=10, b=2] 2\n[4, 3, 20, 100]\nCME print\nCME add\nCME nested\n4504500\n4501500\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val a = mutableListOf(1, 2, 3, 4, 5, 6)
    val s = a.subList(1, 5)
    val t = s.subList(1, 3)
    a[2] = 30
    println(t)
    t[0] = 300
    println(a)
    println(s)
    s.removeAt(0)
    println(a)
    println(s)
    println(s.size)
    for (x in s) print("$x ")
    println()
    println(s.sum())
    val m = mutableMapOf("a" to 1, "b" to 2)
    val vs = m.values
    m["a"] = 10
    println(vs)
    println(vs.sum())
    m.remove("b")
    println(vs.size)
    for (k in m.keys) print(k)
    println()
    val ks = m.keys
    m["z"] = 26
    println(ks.contains("z"))
    println("z" in ks)
    val es = m.entries
    m["y"] = 25
    println(es.map { it.key })
    val arr = intArrayOf(3, 1, 2)
    val al = arr.asList()
    arr.sort()
    println(al)
    val r = a.asReversed()
    a[0] = -1
    println(r)
    println(r.size)
    println(listOf(s, r))
    println(s == listOf(300, 4, 5))
    val big = (1..2000).toMutableList()
    val bs = big.subList(0, 2000)
    var acc = 0L
    for (i in 0 until bs.size) acc += bs[i]
    println(acc)
    val out = mutableListOf<Int>()
    for (i in 0 until bs.size) out.add(bs[i])
    println(out.size)
}"##
        ),
        "[30, 4]\n[1, 2, 300, 4, 5, 6]\n[2, 300, 4, 5]\n[1, 300, 4, 5, 6]\n[300, 4, 5]\n3\n300 4 5 \n309\n[10, 2]\n12\n1\na\ntrue\ntrue\n[a, z, y]\n[1, 2, 3]\n[6, 5, 4, 300, -1]\n5\n[[300, 4, 5], [6, 5, 4, 300, -1]]\ntrue\n2001000\n2000\n"
    );
}

/// `MutableList.asReversed()` is a `MutableList` view: `set`, `add` and `removeAt`
/// through it reach the backing list at the mirrored position. It was a
/// read-only copy whose writes were dropped silently (`[1, 2, 3]` stayed).
#[test]
fn a_mutable_lists_reversed_view_writes_through() {
    assert_eq!(
        prog(
            r##"fun main() {
    val b = mutableListOf(1, 2, 3)
    val r = b.asReversed()
    r[0] = 9
    println(b)
    r.add(0)
    println(b)
    r.removeAt(1)
    println("$b $r")
    b.add(7)
    println(r)
}"##
        ),
        "[1, 2, 9]\n[0, 1, 2, 9]\n[0, 1, 9] [9, 1, 0]\n[7, 9, 1, 0]\n"
    );
}

/// `Any()` is a bare `java.lang.Object` (equal only to itself), `synchronized(lock)`
/// runs its block, and a throwable user code constructs carries the frames live
/// at construction as its `stackTrace`: `Cls.method(file.kt:line)`, the facade
/// class `MainKt` for top-level functions, the throwable's own `<init>` frames
/// left out, inlined lambdas folded into their caller, and the bridge frame of a
/// parameterless `main`. Each was `unresolved reference`. Expected output is the
/// reference compiling the program as `main.kt`, the name a `-e` program takes.
#[test]
fn any_synchronized_and_user_stack_traces() {
    assert_eq!(
        prog(
            r##"fun main() {
    val a = Any()
    val b = Any()
    println(a == a)
    println(a == b)
    println(a.equals(b))
    println(a.hashCode() == a.hashCode())
    println(a.toString().startsWith("java.lang.Object@"))
    println(a is Any)
    val lock = Any()
    synchronized(lock) { println("in") }
    val s = setOf(a, b, a)
    println(s.size)
    println(generateSequence { 5L }.take(3).toList())
    var n = 0
    println(generateSequence { if (n < 3) n++ else null }.toList())
    println(generateSequence { 5L }.take(2).map { it is Long }.toList())
    val t = Throwable("x")
    println(t.stackTrace is Array<StackTraceElement>)
    println(t.stackTrace.isNotEmpty())
    val e = RuntimeException("boom")
    println(e.stackTrace.size > 0)
    println(e.stackTrace[0].methodName)
}"##
        ),
        "true\nfalse\nfalse\ntrue\ntrue\ntrue\nin\n2\n[5, 5, 5]\n[0, 1, 2]\n[true, true]\ntrue\ntrue\ntrue\nmain\n"
    );
    assert_eq!(
        prog(
            r##"class MyEx(m: String) : Exception(m)
class Box {
    fun make(): Throwable = IllegalStateException("in box")
}
fun Int.boom(): RuntimeException = RuntimeException("ext $this")
fun deep(n: Int): Throwable = if (n == 0) Exception("deep") else deep(n - 1)
fun main() {
    val t = Throwable("x")
    for (e in t.stackTrace) println(e)
    println(MyEx("m").stackTrace.joinToString())
    println(Box().make().stackTrace.joinToString())
    println(3.boom().stackTrace.joinToString())
    println(deep(2).stackTrace.joinToString())
    val l = run { Exception("in run") }
    println(l.stackTrace.joinToString())
    val f = t.stackTrace[0]
    println("${f.className} ${f.methodName} ${f.fileName} ${f.lineNumber}")
}"##
        ),
        "MainKt.main(main.kt:8)\nMainKt.main(main.kt)\nMainKt.main(main.kt:10), MainKt.main(main.kt)\nBox.make(main.kt:3), MainKt.main(main.kt:11), MainKt.main(main.kt)\nMainKt.boom(main.kt:5), MainKt.main(main.kt:12), MainKt.main(main.kt)\nMainKt.deep(main.kt:6), MainKt.deep(main.kt:6), MainKt.deep(main.kt:6), MainKt.main(main.kt:13), MainKt.main(main.kt)\nMainKt.main(main.kt:14), MainKt.main(main.kt)\nMainKt main main.kt 8\n"
    );
}

/// `UByte`/`UShort`/`UInt`/`ULong`: `u`/`uL` literals, wrapping arithmetic at
/// the type's width, unsigned division, remainder and comparison, conversions to
/// and from the signed types and `String`, companions' constants, bitwise members,
/// ranges, arrays, `sum`, and `is`/`equals`/`hashCode` by type. Every program here
/// was a parse error (`expected RParen, found Ident("u")`) before.
#[test]
fn unsigned_integer_types() {
    assert_eq!(
        prog(
            r##"fun main() {
    val a: UInt = 5u
    val b = 3u
    println(a + b)
    println(b - a)
    println(a * 1000000000u)
    println(UInt.MAX_VALUE)
    println(UInt.MAX_VALUE + 1u)
    println(ULong.MAX_VALUE)
    println(ULong.MAX_VALUE / 3uL)
    println((-1).toUInt())
    println((-1).toULong())
    println(4000000000u.toInt())
    println(7u / 2u)
    println(7u % 3u)
    println(0u - 1u > 5u)
    println(a is UInt)
    println(listOf(1u, 2u).map { it * 2u })
    val x: UByte = 255u
    println(x)
    println((x + 1u).toUByte())
    val s: UShort = 65535u
    println(s.toInt())
    println(200.toUByte())
    println(300.toUByte())
    println(0xFFFFFFFFu)
    println(5u.toLong() + 1)
    println(18446744073709551615uL.toString())
    println("x=${2u * 3u}")
    println(1u.compareTo(2u))
    println(UInt.MIN_VALUE)
    println(5u shl 30)
    println(5u.toString(2))
    println("42".toUInt() + 1u)
    println("4294967296".toUIntOrNull())
}"##
        ),
        "8\n4294967294\n705032704\n4294967295\n0\n18446744073709551615\n6148914691236517205\n4294967295\n18446744073709551615\n-294967296\n3\n1\ntrue\ntrue\n[2, 4]\n255\n0\n65535\n200\n44\n4294967295\n6\n18446744073709551615\nx=6\n-1\n0\n1073741824\n101\n43\nnull\n"
    );
    assert_eq!(
        prog(
            r##"fun half(x: UInt): UInt = x / 2u
fun big(x: ULong) = x * 2uL
fun main() {
    val x: UByte = 255u
    println(x is UByte)
    val y: ULong = 5u
    println(y is ULong)
    var i = 0u
    i++
    i += 2u
    println(i)
    var j = 10u
    j--
    println(j)
    println(half(9u))
    println(big(ULong.MAX_VALUE / 2uL + 1uL))
    val xs = listOf(3u, 1u, 2u)
    println(xs.sorted())
    println(xs.maxOrNull())
    println(xs.sum())
    println(xs.contains(2u))
    println(setOf(1u, 1u, 2u).size)
    println(mapOf(1u to "a")[1u])
    println(1u == 1u)
    println(when (j) { 9u -> "nine"; else -> "?" })
    val n: UInt? = null
    println(n ?: 7u)
    println(5u.hashCode())
    println(UByte.MAX_VALUE.hashCode())
    println((1u as Any) == (1 as Any))
    println(UInt.SIZE_BITS)
    println(255u.toUByte().toInt())
    println(3000000000u.toLong())
    println(1.5.toUInt())
    println((-1.5).toUInt())
    println(xs.map { it + 1u })
    println(xs.filter { it > 1u })
    println(5u.toString().length)
    println(0u.inv())
    println(maxOf(3u, 7u))
    println(0xFFu and 0x0Fu)
    println(1uL shl 63)
}"##
        ),
        "true\ntrue\n3\n9\n4\n0\n[1, 2, 3]\n3\n6\ntrue\n2\na\ntrue\nnine\n7\n5\n-1\nfalse\n32\n255\n3000000000\n1\n0\n[4, 2, 3]\n[3, 2]\n1\n4294967295\n7\n15\n9223372036854775808\n"
    );
    assert_eq!(
        prog(
            r##"fun takeByte(b: UByte) = b is UByte
fun main() {
    for (i in 1u..3u) print("$i ")
    println()
    for (i in 0u until 3u) print("$i ")
    println()
    println((1u..5u).toList())
    println(5u in 1u..10u)
    val arr = uintArrayOf(1u, 2u)
    println(arr.size)
    println(arr[1])
    println(takeByte(5u))
    println(ubyteArrayOf(1u).toList())
    println(UIntArray(2).toList())
    println((10u downTo 8u).toList())
    println(listOf(2u, 1u).minOrNull())
    println(1u.coerceIn(2u, 5u))
    val m = mutableMapOf<UInt, Int>()
    m[3u] = 1
    println(m)
}"##
        ),
        "1 2 3 \n0 1 2 \n[1, 2, 3, 4, 5]\ntrue\n2\n2\ntrue\n[1]\n[0, 0]\n[10, 9, 8]\n1\n2\n{3=1}\n"
    );
    assert_eq!(
        prog(
            r##"data class P(val a: UInt, val b: ULong)
fun main() {
    val p = P(1u, 2uL)
    println(p)
    println(p == P(1u, 2uL))
    var h = 2166136261u
    for (c in "ab") { h = h xor c.code.toUInt(); h *= 16777619u }
    println(h)
    println(255u.toString(16))
    println(ULong.MAX_VALUE.toString(36))
    try { println(1u / 0u) } catch (e: ArithmeticException) { println(e.message) }
    try { "-1".toUInt() } catch (e: NumberFormatException) { println(e.message) }
    println("ff".toUInt(16))
    println("256".toUByteOrNull())
    println((1u as Any) is UInt)
    println((1u as Any) is Int)
    println(listOf(3u, 10u, 2u).sortedDescending())
    println(10000000000uL * 10000000000uL)
    println((-1L).toULong())
    println(Long.MAX_VALUE.toULong() + 1uL)
    println(5u.toByte())
    println(200u.toByte())
    var x = 7u
    x *= 3u
    x -= 1u
    x /= 2u
    println(x)
    println(x shr 1)
    println(when (x) { in 0u..9u -> "low"; else -> "high" })
    println(x.toInt() + 1)
    println(UShort.MAX_VALUE.toInt())
    println(UByte.MIN_VALUE)
    println(0u.countLeadingZeroBits())
    println("v=$x ${x + 1u}")
    println(x.compareTo(10u))
    println(listOf(1u, 2u).joinToString("-"))
    println(mutableListOf(1u).apply { add(5u) })
}"##
        ),
        "P(a=1, b=2)\ntrue\n1294271946\nff\n3w5e11264sgsf\n/ by zero\nInvalid number format: '-1'\n255\nnull\ntrue\nfalse\n[10, 3, 2]\n7766279631452241920\n18446744073709551615\n9223372036854775808\n5\n-56\n10\n5\nhigh\n11\n65535\n0\n32\nv=10 11\n0\n1-2\n[1, 5]\n"
    );
}

/// An unsigned value's `::class` and `javaClass` name `kotlin.UInt`/`kotlin.ULong`, `%d`
/// refuses one as `java.util.Formatter` does, and unsigned ranges are
/// `UIntRange`/`ULongRange`/`UIntProgression` — a `UByte` range a `UIntRange` of
/// `UInt`s.
#[test]
fn unsigned_classes_formats_and_range_kinds() {
    assert_eq!(
        prog(
            r##"fun main() {
    println(5u::class.simpleName)
    println(5u::class.qualifiedName)
    println(5u.javaClass)
    println(5uL.javaClass.name)
    try { println("%d".format(5u)) } catch (e: Exception) { println(e) }
}"##
        ),
        "UInt\nkotlin.UInt\nclass kotlin.UInt\nkotlin.ULong\njava.util.IllegalFormatConversionException: d != kotlin.UInt\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    println((1u..3u).javaClass.name)
    println((1uL..3uL).javaClass.name)
    println((3u downTo 1u).javaClass.name)
    println((1u..3u))
    println(1u..9u step 2)
}"##
        ),
        "kotlin.ranges.UIntRange\nkotlin.ranges.ULongRange\nkotlin.ranges.UIntProgression\n1..3\n1..9 step 2\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val r = 1.toUByte()..3.toUByte()
    println(r.first() is UInt)
    println(r.javaClass.name)
}"##
        ),
        "true\nkotlin.ranges.UIntRange\n"
    );
}

/// A class may declare one method name several times: each declaration is
/// renamed by its parameter types, and a call picks one by its arguments the way
/// top-level overloads are picked — through a subclass override, an interface
/// default, a named argument and a default, an implicit-`this` call, and a
/// receiver of no static class (where the value decides between the user overload
/// and a stdlib member of the same name). Every program was refused with
/// `member overloads are not supported`.
#[test]
fn member_overloads_resolve_by_argument_types() {
    assert_eq!(
        prog(
            r##"interface Shape
class Sq(val s: Int) : Shape
class Circ(val r: Int) : Shape
open class Printer {
    open fun put(x: Int) = "int $x"
    open fun put(x: String) = "str $x"
    fun put(x: Long) = "long $x"
    fun put(x: Shape) = "shape"
    fun put(x: Sq) = "square ${x.s}"
    fun both() = put(1) + "/" + put("a") + "/" + put(Circ(1))
    fun pad(x: Int, width: Int = 3) = x.toString().padStart(width, '0')
    fun pad(x: String) = "[$x]"
}
class Loud : Printer() {
    override fun put(x: Int) = "INT $x"
}
interface Greeter {
    fun greet(name: String): String
    fun greet(n: Int): String = "hi #$n"
}
class G : Greeter {
    override fun greet(name: String) = "hi $name"
}
fun main() {
    val p = Printer()
    println(p.put(1))
    println(p.put("x"))
    println(p.put(5L))
    println(p.put(3000000000))
    println(p.put(Sq(2)))
    println(p.put(Circ(2)))
    val sh: Shape = Sq(9)
    println(p.put(sh))
    println(p.both())
    println(p.pad(7))
    println(p.pad(7, width = 5))
    println(p.pad("q"))
    val l: Printer = Loud()
    println(l.put(1))
    println(l.put("y"))
    println(l.both())
    val g: Greeter = G()
    println(g.greet("bob"))
    println(g.greet(4))
}"##
        ),
        "int 1\nstr x\nlong 5\nlong 3000000000\nsquare 2\nshape\nshape\nint 1/str a/shape\n007\n00007\n[q]\nINT 1\nstr y\nINT 1/str a/shape\nhi bob\nhi #4\n"
    );
    assert_eq!(
        prog(
            r##"class Printer {
    fun put(x: Int) = "int $x"
    fun put(x: String) = "str $x"
}
fun <T> id(x: T): T = x
fun main() {
    println(listOf(Printer()).map { it.put(1) })
    val a: Any = Printer()
    println((a as Printer).put("z"))
    println(id(Printer()).put(2))
    val ps = mutableListOf(Printer())
    for (q in ps) println(q.put("w"))
}"##
        ),
        "[int 1]\nstr z\nint 2\nstr w\n"
    );
    assert_eq!(
        prog(
            r##"class Printer {
    fun put(x: Int) = "int $x"
    fun put(x: String) = "str $x"
}
fun <T> id(x: T): T = x
fun main() {
    val p = Printer()
    val xs: List<Any> = listOf(1, "a")
    for (x in xs) {
        when (x) {
            is Int -> println(p.put(x))
            is String -> println(p.put(x))
        }
    }
    println(p.put(id(5)))
}"##
        ),
        "int 1\nstr a\nint 5\n"
    );
}

/// `fun String.Companion.hello()`, `val String.Companion.x` and
/// `fun Foo.Companion.f()` are called on the type name (`String.hello()`); a user
/// class's companion is `this` inside. They were a parse error
/// (`expected LParen, found Dot`).
#[test]
fn extensions_on_a_types_companion() {
    assert_eq!(
        prog(
            r##"class Foo
data class D(val x: Int)
object Obj
enum class E { A }
class WithComp { companion object { val base = 10 } }
fun String.Companion.hello() = "hello"
fun Int.Companion.answer() = 42
val String.Companion.EMPTY_ISH: String get() = "e"
fun WithComp.Companion.plusBase(n: Int) = base + n
fun main() {
    println(Foo().javaClass.name)
    println(Foo().javaClass.simpleName)
    println(D(1).javaClass)
    println(Obj.javaClass.name)
    println(E.A.javaClass.name)
    println(1.javaClass)
    println("s".javaClass.name)
    println(listOf(1).javaClass.name)
    println(mutableListOf(1).javaClass.simpleName)
    println(Foo::class.java.name)
    println(String.hello())
    println(Int.answer())
    println(String.EMPTY_ISH)
    println(WithComp.plusBase(5))
}"##
        ),
        "Foo\nFoo\nclass D\nObj\nE\nint\njava.lang.String\njava.util.Collections$SingletonList\nArrayList\nFoo\nhello\n42\ne\n15\n"
    );
}

/// An unsigned array prints as its value class does — `UIntArray(storage=[1, 2])`
/// — with a `javaClass` of `kotlin.UIntArray`; it printed `[Ljava.lang.Object;@…`.
/// A `ULongRange` above `Long.MAX_VALUE` iterates, tests membership, prints and
/// reverses in unsigned order, and `x until 0u` is the type's `EMPTY` (`MAX..0`);
/// both answered in signed order before.
#[test]
fn unsigned_arrays_print_their_storage_and_ulong_ranges_order_unsigned() {
    assert_eq!(
        prog(
            r##"fun main() {
    val a = uintArrayOf(1u, 2u)
    println(a.toString())
    println(a.javaClass.name)
    println(UIntArray(2).toString())
    println(ulongArrayOf(1uL).toString())
    println(a.toList())
    println(a.size)
    println(a.contentToString())
    println(ubyteArrayOf(1u).toString())
}"##
        ),
        "UIntArray(storage=[1, 2])\nkotlin.UIntArray\nUIntArray(storage=[0, 0])\nULongArray(storage=[1])\n[1, 2]\n2\n[1, 2]\nUByteArray(storage=[1])\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    println(uintArrayOf(5u))
    println("${uintArrayOf(5u, 6u)}")
    println(listOf(uintArrayOf(1u)))
}"##
        ),
        "UIntArray(storage=[5])\nUIntArray(storage=[5, 6])\n[UIntArray(storage=[1])]\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val top = ULong.MAX_VALUE
    println((top - 1uL..top).toList())
    println(top - 1uL..top)
    println(top in (top - 5uL)..top)
    println(5uL in (top - 5uL)..top)
    for (x in (top - 2uL)..top) print("$x ")
    println()
    println((top downTo top - 2uL).toList())
    println(0uL until 0uL)
    println((0uL until 0uL).toList())
    println((9223372036854775807uL..9223372036854775809uL).toList())
    println((1uL..10uL step 4).toList())
    println((0uL..3uL).reversed().toList())
}"##
        ),
        "[18446744073709551614, 18446744073709551615]\n18446744073709551614..18446744073709551615\ntrue\nfalse\n18446744073709551613 18446744073709551614 18446744073709551615 \n[18446744073709551615, 18446744073709551614, 18446744073709551613]\n18446744073709551615..0\n[]\n[9223372036854775807, 9223372036854775808, 9223372036854775809]\n[1, 5, 9]\n[3, 2, 1, 0]\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    println(0u until 0u)
    println((0u until 0u).toList())
    println((3u..1u).toList())
    println((0u..2u).reversed())
}"##
        ),
        "4294967295..0\n[]\n[]\n2 downTo 0 step 1\n"
    );
}

/// An overload set of a member the runtime calls by name — `toString`, `equals`,
/// `compareTo`, an iterator's `next`, `invoke` — keeps the declaration it calls
/// under its plain name and resolves the rest by arguments; a callable reference
/// to an overloaded member (`Printer::put`, `p::put`) takes the declaration its
/// expected function type selects. Each was refused or unresolved.
#[test]
fn overloads_of_host_called_members_and_overloaded_references() {
    assert_eq!(
        prog(
            r##"class V(val x: Int) : Comparable<V> {
    override fun toString() = "V($x)"
    fun toString(radix: Int) = x.toString(radix)
    override fun equals(other: Any?) = other is V && other.x == x
    fun equals(other: V, slack: Int) = kotlin.math.abs(other.x - x) <= slack
    override fun hashCode() = x
    override fun compareTo(other: V) = x.compareTo(other.x)
    fun compareTo(other: Int) = x.compareTo(other)
}
class Cnt(val n: Int) : Iterator<Int> {
    var i = 0
    override fun hasNext() = i < n
    override fun next() = i++
    fun next(skip: Int): Int { i += skip; return i }
}
fun main() {
    val v = V(10)
    println(v)
    println("$v")
    println(v.toString(16))
    println(v == V(10))
    println(v.equals(V(12), 3))
    println(setOf(V(1), V(1)).size)
    println(listOf(V(3), V(1)).sorted())
    println(v > V(2))
    println(v.compareTo(11))
    val c = Cnt(5)
    println(c.next(2))
    for (x in c) print("$x ")
    println()
}"##
        ),
        "V(10)\nV(10)\na\ntrue\ntrue\n1\n[V(1), V(3)]\ntrue\n-1\n2\n2 3 4 \n"
    );
    assert_eq!(
        prog(
            r##"class Printer {
    fun put(x: Int) = "int $x"
    fun put(x: String) = "str $x"
}
fun main() {
    val f: (Printer, Int) -> String = Printer::put
    println(f(Printer(), 3))
    val g: (Printer, String) -> String = Printer::put
    println(g(Printer(), "a"))
    val p = Printer()
    val h: (Int) -> String = p::put
    println(h(9))
    println(listOf(1, 2).map(p::put))
    println(listOf("x").map(p::put))
}"##
        ),
        "int 3\nstr a\nint 9\n[int 1, int 2]\n[str x]\n"
    );
    assert_eq!(
        prog(
            r##"class C { operator fun invoke(a: String) = 1; operator fun invoke(a: Int) = 2; operator fun invoke() = 0 }
fun main() { val c = C(); println(c(1)); println(c("s")); println(c()); println(C()(5)); val l = listOf(C()); println(l[0](3)) }"##
        ),
        "2\n1\n0\n2\n2\n"
    );
    assert_eq!(
        prog(
            r##"class C { operator fun invoke(a: String) = 1; operator fun invoke(a: Int) = 2 }
fun <T> id(x: T) = x
fun main() { val l = listOf(C()); println(l[0](3)); val f = id { x: Int -> x * 10 }; println(f(4)); val fs = listOf({ x: Int -> x + 1 }); println(fs[0](1)) }"##
        ),
        "2\n40\n2\n"
    );
}

/// A sorted set's or map's `headSet`/`tailMap`/`subSet`/`descendingSet`/… is a\nview: it is recomputed from its source after the source is written, and a\nwrite through it reaches the source, refusing a key outside the range with\n`key out of range` as `java.util.TreeMap` does. Each was a snapshot taken when\nthe member was called.
#[test]
fn sorted_collection_ranges_are_live_views() {
    assert_eq!(
        prog(
            r##"fun main() {
    val t = sortedSetOf(1, 5, 9)
    val h = t.headSet(6)
    t.add(2)
    println(h)
    println(h.size)
    println(3 in h)
    t.add(3)
    println(3 in h)
    for (x in h) print("$x ")
    println()
    val d = t.descendingSet()
    t.add(7)
    println(d)
    val m = sortedMapOf("b" to 2, "d" to 4)
    val tm = m.tailMap("c")
    m["e"] = 5
    m["c"] = 3
    println(tm)
    println(tm.keys)
    val byLen = sortedSetOf(compareBy<String> { it.length }, "aaa", "b")
    val small = byLen.headSet("cc")
    byLen.add("dd")
    byLen.add("e")
    println(small)
    println(h.map { it * 10 })
    val sub = t.subSet(2, 8)
    t.remove(5)
    println(sub)
    println("$h")
}"##
        ),
        "[1, 2, 5]\n3\nfalse\ntrue\n1 2 3 5 \n[9, 7, 5, 3, 2, 1]\n{c=3, d=4, e=5}\n[c, d, e]\n[b]\n[10, 20, 30, 50]\n[2, 3, 7]\n[1, 2, 3]\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val t = sortedSetOf(1, 5, 9)
    val h = t.headSet(6)
    h.add(2)
    println(t)
    try { h.add(7) } catch (e: IllegalArgumentException) { println(e.message) }
    println(t)
    println(h.remove(9))
    println(h.remove(5))
    println(t)
    h.clear()
    println(t)
    val m = sortedMapOf(1 to "a", 4 to "d", 8 to "h")
    val tail = m.tailMap(3)
    tail[5] = "e"
    println(m)
    try { tail[2] = "b" } catch (e: IllegalArgumentException) { println(e.message) }
    tail.remove(8)
    println(m)
    val d = t.descendingSet()
    d.add(4)
    println(t)
}"##
        ),
        "[1, 2, 5, 9]\nkey out of range\n[1, 2, 5, 9]\nfalse\ntrue\n[1, 2, 9]\n[9]\n{1=a, 4=d, 5=e, 8=h}\nkey out of range\n{1=a, 4=d, 5=e}\n[4, 9]\n"
    );
}

/// A throwable the runtime raises carries the frames live at the fault as its\n`stackTrace`, after the JDK/stdlib frames its JVM trace starts with where the\nraising operation is a measured one (`ArrayList.get`, `Integer.parseInt`,\n`String.charAt`, `CollectionsKt.first`, …). A lambda passed to a non-inline\nfunction is its own frame, named as `kotlinc` names it (`main$lambda$3$0`), and\none run by an inlined stdlib call is not. `printStackTrace` and\n`stackTraceToString` write the trace with its suppressed and `Caused by` parts.\nRuntime-raised traces were refused and lambda frames were missing.
#[test]
fn runtime_raised_and_lambda_stack_traces() {
    assert_eq!(
        prog(
            r##"fun t(name: String, f: () -> Unit) {
    try { f() } catch (e: Throwable) {
        println("$name: $e")
        for (el in e.stackTrace) println("  $el")
    }
}
fun div(a: Int, b: Int) = a / b
fun main() {
    t("div") { div(1, 0) }
    t("idx") { listOf(1, 2)[5] }
    t("npe") { val s: String? = null; s!!.length }
    t("cast") { val a: Any = "x"; a as Int }
    t("req") { require(false) { "bad" } }
    t("chk") { check(false) }
    t("err") { error("boom") }
    t("nfe") { "x".toInt() }
    t("first") { emptyList<Int>().first() }
    t("arr") { IntArray(1)[3] }
    t("map") { mapOf(1 to 2).getValue(3) }
}"##
        ),
        "div: java.lang.ArithmeticException: / by zero\n  MainKt.div(main.kt:7)\n  MainKt.main$lambda$0(main.kt:9)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:9)\n  MainKt.main(main.kt)\nidx: java.lang.ArrayIndexOutOfBoundsException: Index 5 out of bounds for length 2\n  java.base/java.util.Arrays$ArrayList.get(Arrays.java:4266)\n  MainKt.main$lambda$1(main.kt:10)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:10)\n  MainKt.main(main.kt)\nnpe: java.lang.NullPointerException\n  MainKt.main$lambda$2(main.kt:11)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:11)\n  MainKt.main(main.kt)\ncast: java.lang.ClassCastException: class java.lang.String cannot be cast to class java.lang.Integer (java.lang.String and java.lang.Integer are in module java.base of loader 'bootstrap')\n  MainKt.main$lambda$3(main.kt:12)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:12)\n  MainKt.main(main.kt)\nreq: java.lang.IllegalArgumentException: bad\n  MainKt.main$lambda$4(main.kt:13)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:13)\n  MainKt.main(main.kt)\nchk: java.lang.IllegalStateException: Check failed.\n  MainKt.main$lambda$5(main.kt:14)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:14)\n  MainKt.main(main.kt)\nerr: java.lang.IllegalStateException: boom\n  MainKt.main$lambda$6(main.kt:15)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:15)\n  MainKt.main(main.kt)\nnfe: java.lang.NumberFormatException: For input string: \"x\"\n  java.base/java.lang.NumberFormatException.forInputString(NumberFormatException.java:67)\n  java.base/java.lang.Integer.parseInt(Integer.java:662)\n  java.base/java.lang.Integer.parseInt(Integer.java:778)\n  MainKt.main$lambda$7(main.kt:16)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:16)\n  MainKt.main(main.kt)\nfirst: java.util.NoSuchElementException: List is empty.\n  kotlin.collections.CollectionsKt___CollectionsKt.first(_Collections.kt:222)\n  MainKt.main$lambda$8(main.kt:17)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:17)\n  MainKt.main(main.kt)\narr: java.lang.ArrayIndexOutOfBoundsException: Index 3 out of bounds for length 1\n  MainKt.main$lambda$9(main.kt:18)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:18)\n  MainKt.main(main.kt)\nmap: java.util.NoSuchElementException: Key 3 is missing in the map.\n  kotlin.collections.MapsKt__MapWithDefaultKt.getOrImplicitDefaultNullable(MapWithDefault.kt:25)\n  kotlin.collections.MapsKt__MapsKt.getValue(Maps.kt:437)\n  MainKt.main$lambda$10(main.kt:19)\n  MainKt.t(main.kt:2)\n  MainKt.main(main.kt:19)\n  MainKt.main(main.kt)\n"
    );
    assert_eq!(
        prog(
            r##"fun t(f: () -> Unit) { try { f() } catch (e: Exception) { println(e.stackTrace.take(3).joinToString(" | ")) } }
class K { fun m() { t { throw Exception() } } }
fun main() {
    println(listOf(1).map { it + 1 })
    t { throw Exception() }
    val g = { x: Int -> x + 1 }
    t { t { throw Exception("in") }; throw Exception() }
    K().m()
    run { t { throw Exception() } }
    listOf(1).forEach { t { throw Exception() } }
}"##
        ),
        "[2]\nMainKt.main$lambda$1(main.kt:5) | MainKt.t(main.kt:1) | MainKt.main(main.kt:5)\nMainKt.main$lambda$3$0(main.kt:7) | MainKt.t(main.kt:1) | MainKt.main$lambda$3(main.kt:7)\nMainKt.main$lambda$3(main.kt:7) | MainKt.t(main.kt:1) | MainKt.main(main.kt:7)\nK.m$lambda$0(main.kt:2) | MainKt.t(main.kt:1) | K.m(main.kt:2)\nMainKt.main$lambda$4$0(main.kt:9) | MainKt.t(main.kt:1) | MainKt.main(main.kt:9)\nMainKt.main$lambda$5$0(main.kt:10) | MainKt.t(main.kt:1) | MainKt.main(main.kt:10)\n"
    );
    assert_eq!(
        prog(
            r##"fun t(name: String, f: () -> Unit) {
    try { f() } catch (e: Throwable) {
        println("$name: $e")
        for (el in e.stackTrace) { if (el.className.startsWith("MainKt")) break; println("  $el") }
    }
}
fun main() {
    t("mut") { mutableListOf(1)[5] }
    t("single") { listOf(1)[5] }
    t("two") { listOf(1, 2)[5] }
    t("empty") { emptyList<Int>()[0] }
    t("toInt") { "x".toInt() }
    t("toLong") { "x".toLong() }
    t("toDouble") { "x".toDouble() }
    t("first") { emptyList<Int>().first() }
    t("last") { emptyList<Int>().last() }
    t("getValue") { mapOf(1 to 2).getValue(3) }
    t("str") { "abc"[5] }
    t("arr") { arrayOf(1)[3] }
    t("single2") { listOf(1, 2).single() }
    t("next") { emptyList<Int>().iterator().next() }
    t("removeAt") { mutableListOf(1).removeAt(3) }
    t("mutset") { mutableListOf(1)[2] = 3 }
    t("sub") { "abc".substring(5) }
    t("max") { emptyList<Int>().max() }
    t("reduce") { emptyList<Int>().reduce { a, b -> a + b } }
    t("ro") { (listOf(1, 2) as MutableList<Int>).add(3) }
}"##
        ),
        "mut: java.lang.IndexOutOfBoundsException: Index 5 out of bounds for length 1\n  java.base/jdk.internal.util.Preconditions.outOfBounds(Preconditions.java:100)\n  java.base/jdk.internal.util.Preconditions.outOfBoundsCheckIndex(Preconditions.java:106)\n  java.base/jdk.internal.util.Preconditions.checkIndex(Preconditions.java:302)\n  java.base/java.util.Objects.checkIndex(Objects.java:385)\n  java.base/java.util.ArrayList.get(ArrayList.java:427)\nsingle: java.lang.IndexOutOfBoundsException: Index: 5, Size: 1\n  java.base/java.util.Collections$SingletonList.get(Collections.java:5180)\ntwo: java.lang.ArrayIndexOutOfBoundsException: Index 5 out of bounds for length 2\n  java.base/java.util.Arrays$ArrayList.get(Arrays.java:4266)\nempty: java.lang.IndexOutOfBoundsException: Empty list doesn't contain element at index 0.\n  kotlin.collections.EmptyList.get(Collections.kt:37)\n  kotlin.collections.EmptyList.get(Collections.kt:25)\ntoInt: java.lang.NumberFormatException: For input string: \"x\"\n  java.base/java.lang.NumberFormatException.forInputString(NumberFormatException.java:67)\n  java.base/java.lang.Integer.parseInt(Integer.java:662)\n  java.base/java.lang.Integer.parseInt(Integer.java:778)\ntoLong: java.lang.NumberFormatException: For input string: \"x\"\n  java.base/java.lang.NumberFormatException.forInputString(NumberFormatException.java:67)\n  java.base/java.lang.Long.parseLong(Long.java:709)\n  java.base/java.lang.Long.parseLong(Long.java:832)\ntoDouble: java.lang.NumberFormatException: For input string: \"x\"\n  java.base/jdk.internal.math.FloatingDecimal.readJavaFormatString(FloatingDecimal.java:2054)\n  java.base/jdk.internal.math.FloatingDecimal.parseDouble(FloatingDecimal.java:110)\n  java.base/java.lang.Double.parseDouble(Double.java:792)\nfirst: java.util.NoSuchElementException: List is empty.\n  kotlin.collections.CollectionsKt___CollectionsKt.first(_Collections.kt:222)\nlast: java.util.NoSuchElementException: List is empty.\n  kotlin.collections.CollectionsKt___CollectionsKt.last(_Collections.kt:431)\ngetValue: java.util.NoSuchElementException: Key 3 is missing in the map.\n  kotlin.collections.MapsKt__MapWithDefaultKt.getOrImplicitDefaultNullable(MapWithDefault.kt:25)\n  kotlin.collections.MapsKt__MapsKt.getValue(Maps.kt:437)\nstr: java.lang.StringIndexOutOfBoundsException: Index 5 out of bounds for length 3\n  java.base/jdk.internal.util.Preconditions$1.apply(Preconditions.java:55)\n  java.base/jdk.internal.util.Preconditions$1.apply(Preconditions.java:52)\n  java.base/jdk.internal.util.Preconditions$4.apply(Preconditions.java:213)\n  java.base/jdk.internal.util.Preconditions$4.apply(Preconditions.java:210)\n  java.base/jdk.internal.util.Preconditions.outOfBounds(Preconditions.java:98)\n  java.base/jdk.internal.util.Preconditions.outOfBoundsCheckIndex(Preconditions.java:106)\n  java.base/jdk.internal.util.Preconditions.checkIndex(Preconditions.java:302)\n  java.base/java.lang.String.checkIndex(String.java:4822)\n  java.base/java.lang.StringLatin1.charAt(StringLatin1.java:46)\n  java.base/java.lang.String.charAt(String.java:1544)\narr: java.lang.ArrayIndexOutOfBoundsException: Index 3 out of bounds for length 1\nsingle2: java.lang.IllegalArgumentException: List has more than one element.\n  kotlin.collections.CollectionsKt___CollectionsKt.single(_Collections.kt:621)\nnext: java.util.NoSuchElementException\n  kotlin.collections.EmptyIterator.next(Collections.kt:21)\n  kotlin.collections.EmptyIterator.next(Collections.kt:16)\nremoveAt: java.lang.IndexOutOfBoundsException: Index 3 out of bounds for length 1\n  java.base/jdk.internal.util.Preconditions.outOfBounds(Preconditions.java:100)\n  java.base/jdk.internal.util.Preconditions.outOfBoundsCheckIndex(Preconditions.java:106)\n  java.base/jdk.internal.util.Preconditions.checkIndex(Preconditions.java:302)\n  java.base/java.util.Objects.checkIndex(Objects.java:385)\n  java.base/java.util.ArrayList.remove(ArrayList.java:551)\nmutset: java.lang.IndexOutOfBoundsException: Index 2 out of bounds for length 1\n  java.base/jdk.internal.util.Preconditions.outOfBounds(Preconditions.java:100)\n  java.base/jdk.internal.util.Preconditions.outOfBoundsCheckIndex(Preconditions.java:106)\n  java.base/jdk.internal.util.Preconditions.checkIndex(Preconditions.java:302)\n  java.base/java.util.Objects.checkIndex(Objects.java:385)\n  java.base/java.util.ArrayList.set(ArrayList.java:470)\nsub: java.lang.StringIndexOutOfBoundsException: Range [5, 3) out of bounds for length 3\n  java.base/jdk.internal.util.Preconditions$1.apply(Preconditions.java:55)\n  java.base/jdk.internal.util.Preconditions$1.apply(Preconditions.java:52)\n  java.base/jdk.internal.util.Preconditions$4.apply(Preconditions.java:213)\n  java.base/jdk.internal.util.Preconditions$4.apply(Preconditions.java:210)\n  java.base/jdk.internal.util.Preconditions.outOfBounds(Preconditions.java:98)\n  java.base/jdk.internal.util.Preconditions.outOfBoundsCheckFromToIndex(Preconditions.java:112)\n  java.base/jdk.internal.util.Preconditions.checkFromToIndex(Preconditions.java:349)\n  java.base/java.lang.String.checkBoundsBeginEnd(String.java:4855)\n  java.base/java.lang.String.substring(String.java:2823)\n  java.base/java.lang.String.substring(String.java:2796)\nmax: java.util.NoSuchElementException\n  kotlin.collections.CollectionsKt___CollectionsKt.maxOrThrow(_Collections.kt:2272)\nreduce: java.lang.UnsupportedOperationException: Empty collection can't be reduced.\nro: java.lang.UnsupportedOperationException\n  java.base/java.util.AbstractList.add(AbstractList.java:155)\n  java.base/java.util.AbstractList.add(AbstractList.java:113)\n"
    );
    assert_eq!(
        prog(
            r##"fun t(name: String, f: () -> Unit) {
    try { f() } catch (e: Throwable) {
        println("$name: $e")
        for (el in e.stackTrace) { if (el.className.startsWith("MainKt")) break; println("  $el") }
    }
}
fun main() {
    t("sub2") { "abc".substring(5, 7) }
    t("min") { emptyList<Int>().min() }
    t("mutnext") { mutableListOf<Int>().iterator().next() }
    t("removeFirst") { mutableListOf<Int>().removeFirst() }
    t("removeLast") { mutableListOf<Int>().removeLast() }
    t("ro2") { (listOf(1) as MutableList<Int>).add(3) }
    t("roempty") { (emptyList<Int>() as MutableList<Int>).add(3) }
    t("firstStr") { "".first() }
    t("toIntRadix") { "z".toInt(10) }
    t("lastIdx") { mutableListOf(1).last { it > 5 } }
    t("firstPred") { listOf(1).first { it > 5 } }
}"##
        ),
        "sub2: java.lang.StringIndexOutOfBoundsException: Range [5, 7) out of bounds for length 3\n  java.base/jdk.internal.util.Preconditions$1.apply(Preconditions.java:55)\n  java.base/jdk.internal.util.Preconditions$1.apply(Preconditions.java:52)\n  java.base/jdk.internal.util.Preconditions$4.apply(Preconditions.java:213)\n  java.base/jdk.internal.util.Preconditions$4.apply(Preconditions.java:210)\n  java.base/jdk.internal.util.Preconditions.outOfBounds(Preconditions.java:98)\n  java.base/jdk.internal.util.Preconditions.outOfBoundsCheckFromToIndex(Preconditions.java:112)\n  java.base/jdk.internal.util.Preconditions.checkFromToIndex(Preconditions.java:349)\n  java.base/java.lang.String.checkBoundsBeginEnd(String.java:4855)\n  java.base/java.lang.String.substring(String.java:2823)\nmin: java.util.NoSuchElementException\n  kotlin.collections.CollectionsKt___CollectionsKt.minOrThrow(_Collections.kt:2696)\nmutnext: java.util.NoSuchElementException\n  java.base/java.util.ArrayList$Itr.next(ArrayList.java:1052)\nremoveFirst: java.util.NoSuchElementException\n  java.base/java.util.ArrayList.removeFirst(ArrayList.java:568)\nremoveLast: java.util.NoSuchElementException\n  java.base/java.util.ArrayList.removeLast(ArrayList.java:586)\nro2: java.lang.UnsupportedOperationException\n  java.base/java.util.AbstractList.add(AbstractList.java:155)\n  java.base/java.util.AbstractList.add(AbstractList.java:113)\nroempty: java.lang.ClassCastException: kotlin.collections.EmptyList cannot be cast to kotlin.collections.MutableList\nfirstStr: java.util.NoSuchElementException: Char sequence is empty.\n  kotlin.text.StringsKt___StringsKt.first(_Strings.kt:77)\ntoIntRadix: java.lang.NumberFormatException: For input string: \"z\"\n  java.base/java.lang.NumberFormatException.forInputString(NumberFormatException.java:67)\n  java.base/java.lang.Integer.parseInt(Integer.java:662)\nlastIdx: java.util.NoSuchElementException: List contains no element matching the predicate.\nfirstPred: java.util.NoSuchElementException: Collection contains no element matching the predicate.\n"
    );
    assert_eq!(
        prog(
            r##"fun f() { throw IllegalStateException("x") }
fun g() { try { f() } catch (e: Exception) { throw RuntimeException("wrapped", e) } }
fun main() {
    try { f() } catch (e: Exception) { println(e.stackTraceToString()) }
    try { g() } catch (e: Exception) { println(e.stackTraceToString()) }
    val a = Exception("a"); a.addSuppressed(IllegalArgumentException("s"))
    println(a.stackTraceToString())
    try { listOf(1, 2)[5] } catch (e: Exception) { println(e.stackTraceToString()) }
}"##
        ),
        "java.lang.IllegalStateException: x\n	at MainKt.f(main.kt:1)\n	at MainKt.main(main.kt:4)\n	at MainKt.main(main.kt)\n\njava.lang.RuntimeException: wrapped\n	at MainKt.g(main.kt:2)\n	at MainKt.main(main.kt:5)\n	at MainKt.main(main.kt)\nCaused by: java.lang.IllegalStateException: x\n	at MainKt.f(main.kt:1)\n	... 3 more\n\njava.lang.Exception: a\n	at MainKt.main(main.kt:6)\n	at MainKt.main(main.kt)\n	Suppressed: java.lang.IllegalArgumentException: s\n		... 2 more\n\njava.lang.ArrayIndexOutOfBoundsException: Index 5 out of bounds for length 2\n	at java.base/java.util.Arrays$ArrayList.get(Arrays.java:4266)\n	at MainKt.main(main.kt:8)\n	at MainKt.main(main.kt)\n\n"
    );
}

/// An exception that reaches the top of `main` is reported as the JVM reports
/// it — the `Exception in thread "main"` line and the trace, with no `kotlin:`
/// prefix — including the JDK frame of a measured operation. Measured on
/// kotlinc 2.4.21 / JRE 21.0.12.1 with the program compiled as `main.kt`.
#[test]
fn an_uncaught_exception_prints_its_stack_trace() {
    assert_eq!(
        prog_err("fun f(n: Int): Int = 10 / n\nfun main() {\n    println(\"start\")\n    println(f(0))\n}"),
        "Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n\tat MainKt.f(main.kt:1)\n\tat MainKt.main(main.kt:4)\n\tat MainKt.main(main.kt)\n"
    );
    assert_eq!(
        prog_err("fun main() { listOf(1, 2)[5] }"),
        "Exception in thread \"main\" java.lang.ArrayIndexOutOfBoundsException: Index 5 out of bounds for length 2\n\tat java.base/java.util.Arrays$ArrayList.get(Arrays.java:4266)\n\tat MainKt.main(main.kt:1)\n\tat MainKt.main(main.kt)\n"
    );
}

/// An operator EXTENSION on the left operand's known type (`operator fun\nPair<Int, Int>.plus`) is the operator's convention, and `val (a, b, c) = v` calls\na user class's own `operator fun component3` beside its data components. They\nwere `unresolved reference: plus on Pair` and `no component3 on V`.
#[test]
fn operator_extensions_and_user_component_functions() {
    assert_eq!(
        prog(
            r##"sealed class Shape {
    data class Circle(val r: Double) : Shape()
    data class Rect(val w: Double, val h: Double) : Shape()
    object Empty : Shape()
}
sealed interface Expr
data class Num(val v: Int) : Expr
data class Add(val l: Expr, val r: Expr) : Expr
fun eval(e: Expr): Int = when (e) { is Num -> e.v; is Add -> eval(e.l) + eval(e.r) }
fun area(s: Shape) = when (s) {
    is Shape.Circle -> 3.0 * s.r * s.r
    is Shape.Rect -> s.w * s.h
    Shape.Empty -> 0.0
}
inline fun <reified T> List<Any>.only(): List<T> = filter { it is T }.map { it as T }
inline fun <reified T> typeName() = T::class.simpleName
operator fun Pair<Int, Int>.plus(o: Pair<Int, Int>) = Pair(first + o.first, second + o.second)
data class V(val x: Int, val y: Int) {
    operator fun plus(o: V) = V(x + o.x, y + o.y)
    operator fun unaryMinus() = V(-x, -y)
    operator fun times(k: Int) = V(x * k, y * k)
    operator fun get(i: Int) = if (i == 0) x else y
    operator fun contains(k: Int) = k == x || k == y
    operator fun compareTo(o: V) = (x + y) - (o.x + o.y)
    operator fun inc() = V(x + 1, y + 1)
    operator fun rangeTo(o: V) = listOf(this, o)
    operator fun component3() = x * y
}
class Grid { val m = mutableMapOf<Pair<Int, Int>, Int>(); operator fun get(a: Int, b: Int) = m[a to b] ?: 0; operator fun set(a: Int, b: Int, v: Int) { m[a to b] = v } }
fun main() {
    println(listOf(Shape.Circle(1.0), Shape.Rect(2.0, 3.0), Shape.Empty).map { area(it) })
    println(eval(Add(Num(1), Add(Num(2), Num(3)))))
    println(Shape.Circle(2.0))
    println(listOf<Any>(1, "a", 2, 'c').only<Int>())
    println(typeName<String>())
    println((1 to 2) + (3 to 4))
    var v = V(1, 2)
    println(v + V(3, 4)); println(-v); println(v * 3); println(v[1]); println(2 in v); println(v > V(0, 0))
    v++
    println(v)
    println(V(1, 1)..V(2, 2))
    val (a, b, c) = V(2, 5)
    println("$a $b $c")
    val g = Grid(); g[1, 2] = 7; g[1, 2] += 1; println(g[1, 2])
    val (k, w) = "x" to 9
    println("$k=$w")
    for ((i, s) in listOf("p", "q").withIndex()) print("$i$s ")
    println()
    val m = mapOf("a" to 1, "b" to 2)
    for ((key, value) in m) print("$key->$value ")
    println()
    val (_, second) = listOf(1, 2)
    println(second)
}"##
        ),
        "[3.0, 6.0, 0.0]\n6\nCircle(r=2.0)\n[1, 2]\nString\n(4, 6)\nV(x=4, y=6)\nV(x=-1, y=-2)\nV(x=3, y=6)\n2\ntrue\ntrue\nV(x=2, y=3)\n[V(x=1, y=1), V(x=2, y=2)]\n2 5 10\n8\nx=9\n0p 1q \na->1 b->2 \n2\n"
    );
}

/// A `when` that covers a sealed hierarchy — through a sealed intermediate,\nobjects named or tested with `is`, every enum constant, both `Boolean`s — needs\nno `else`, in expression and statement position.
#[test]
fn an_exhaustive_when_over_a_sealed_hierarchy_runs() {
    assert_eq!(
        prog(
            r##"sealed class T
sealed class Mid : T()
class L1 : Mid()
class L2 : Mid()
object O : T()
data class D(val v: Int) : T()
enum class Dir { N, S }
fun a(t: T) = when (t) { is Mid -> "mid"; O -> "o"; is D -> "d${t.v}" }
fun b(t: T) = when (t) { is L1 -> 1; is L2 -> 2; is O -> 3; is D -> 4 }
fun c(d: Dir) = when (d) { Dir.N -> "n"; Dir.S -> "s" }
fun e(x: Boolean) = when (x) { true -> 1; false -> 0 }
fun f(d: Dir) = when (d) { Dir.N, Dir.S -> "any" }
fun main() {
    println(listOf(L1(), L2(), O, D(4)).map { a(it) + b(it) })
    println(Dir.values().map { c(it) + f(it) })
    println(e(false))
    val t: T = L2()
    when (t) { is Mid -> println("m"); O -> println("o"); is D -> println("d") }
}"##
        ),
        "[mid1, mid2, o3, d44]\n[nany, sany]\n0\nm\n"
    );
}

/// A `when` over a `sealed` type, an enum or a `Boolean` that misses a case and
/// has no `else` is a compile error — in statement position too — naming what
/// is missing as `kotlinc` 2.4.21 does. Each program used to run and answer
/// `null` (or skip the arm silently).
#[test]
fn a_non_exhaustive_when_over_a_closed_type_is_refused() {
    for (src, missing) in [
        ("sealed class S\nclass A : S()\nclass B : S()\nfun f(s: S) = when (s) { is A -> 1 }\nfun main() { println(f(B())) }", "the 'is B' branch"),
        ("enum class E { X, Y }\nfun f(e: E): Int = when (e) { E.X -> 1 }\nfun main() { println(f(E.Y)) }", "the 'Y' branch"),
        ("fun f(b: Boolean): Int = when (b) { true -> 1 }\nfun main() { println(f(false)) }", "the 'false' branch"),
        ("sealed interface S\nobject A : S\nobject B : S\nfun main() { val s: S = B\n when (s) { A -> println(\"a\") }\n println(\"done\") }", "the 'B' branch"),
        ("sealed class S\nclass A : S()\nclass B : S()\nobject C : S()\nfun f(s: S) = when (s) { is A -> 1 }\nfun main() {}", "the 'is B', 'C' branches"),
    ] {
        let err = prog_err(src);
        assert!(
            err.contains(&format!("'when' expression must be exhaustive. Add {missing} or an 'else' branch.")),
            "for {src:?}: stderr was {err}"
        );
    }
}

/// `class C private constructor(val c: Double)`, `class A constructor(…)` and\n`internal constructor` declare the primary constructor; they were a parse\nerror (`expected Class, found Ident("constructor")`). The visibility is not\nenforced.
#[test]
fn a_primary_constructor_spelled_with_its_keyword() {
    assert_eq!(
        prog(
            r##"interface Named { val name: String; fun greet() = "Hi $name" }
abstract class Animal(override val name: String) : Named { abstract fun sound(): String; open fun describe() = "$name says ${sound()}" }
class Dog(name: String) : Animal(name) { override fun sound() = "woof"; override fun describe() = super.describe() + "!" }
class Box<out T>(val v: T) { fun <R> map(f: (T) -> R) = Box(f(v)); override fun toString() = "Box($v)" }
class Stack<T> { private val items = ArrayDeque<T>(); fun push(x: T) = items.addLast(x); fun pop(): T? = items.removeLastOrNull(); val size get() = items.size }
object Registry { private val m = mutableMapOf<String, Int>(); fun add(k: String) { m[k] = (m[k] ?: 0) + 1 }; override fun toString() = m.toString() }
class Temp private constructor(val c: Double) { companion object { fun ofF(f: Double) = Temp((f - 32) * 5 / 9) }; override fun toString() = "%.1f".format(c) }
fun <T : Comparable<T>> maxOf3(a: T, b: T, c: T): T = maxOf(a, maxOf(b, c))
fun main() {
    val d = Dog("Rex"); println(d.describe()); println(d.greet())
    println(Box(3).map { it * 2 }.map { "s$it" })
    val s = Stack<String>(); s.push("a"); s.push("b"); println("${s.pop()} ${s.size} ${s.pop()} ${s.pop()}")
    Registry.add("x"); Registry.add("x"); Registry.add("y"); println(Registry)
    println(Temp.ofF(212.0))
    println(maxOf3("pear", "apple", "zoo")); println(maxOf3(3, 9, 1))
    val words = "the quick brown fox jumps over the lazy dog".split(" ")
    println(words.groupingBy { it.length }.eachCount().toSortedMap())
    println(words.associateWith { it.length }.filterValues { it > 4 })
    println(words.windowed(3, 3).map { it.joinToString("-") })
    println(words.zipWithNext { a, b -> a.first() == b.first() }.count { it })
    println(words.sortedWith(compareBy({ it.length }, { it })).take(4))
    println(words.fold(StringBuilder()) { acc, w -> acc.append(w[0]) })
    val (short, long) = words.partition { it.length <= 3 }
    println("$short | $long")
    println(words.chunked(4).map { it.size })
    println((1..10).toList().reduceRight { x, acc -> x - acc })
    println(words.flatMap { it.toList() }.distinct().size)
    println(buildString { repeat(3) { append(it) } })
    println(buildList { add(1); addAll(listOf(2, 3)) })
    println(buildMap { put("k", 1) })
}"##
        ),
        "Rex says woof!\nHi Rex\nBox(s6)\nb 1 a null\n{x=2, y=1}\n100.0\nzoo\n9\n{3=4, 4=2, 5=3}\n{quick=5, brown=5, jumps=5}\n[the-quick-brown, fox-jumps-over, the-lazy-dog]\n0\n[dog, fox, the, the]\ntqbfjotld\n[the, fox, the, dog] | [quick, brown, jumps, over, lazy]\n[4, 4, 1]\n-5\n26\n012\n[1, 2, 3]\n{k=1}\n"
    );
    assert_eq!(
        prog(
            r##"class A constructor(val x: Int)
class B internal constructor(val y: Int) { constructor() : this(7) }
fun main() { println(A(1).x + B(2).y + B().y) }"##
        ),
        "10\n"
    );
}

/// `fun <T> pick(a: T, b: T) where T : Comparable<T> = …` and `class Holder<T>(…)\nwhere T : Comparable<T> { … }` declare bounds the coarse typing does not keep;\nthey were refused (`top-level fun pick needs a body`, `found Ident("where")`).\nThe rest of the program pins typealiases, infix and extension properties,\nlabels, local functions and smart casts beside them.
#[test]
fn where_clauses_on_functions_and_classes() {
    assert_eq!(
        prog(
            r##"typealias Pred<T> = (T) -> Boolean
typealias Matrix = List<List<Int>>
infix fun Int.pow(e: Int): Long { var r = 1L; repeat(e) { r *= this }; return r }
val String.vowels get() = count { it in "aeiou" }
fun <T> List<T>.firstMatch(p: Pred<T>): T? = firstOrNull(p)
fun <T> pick(a: T, b: T, cond: Boolean) where T : Comparable<T> = if (cond) maxOf(a, b) else minOf(a, b)
fun len(s: String?): Int = s?.length ?: return -1
fun need(s: String?): String = s ?: throw IllegalArgumentException("null!")
fun main() {
    println(2 pow 10)
    println("education".vowels)
    println(listOf(3, 8, 11).firstMatch { it > 5 })
    println(pick("b", "a", true))
    val m: Matrix = listOf(listOf(1, 2), listOf(3, 4))
    println(m.map { r -> r.sum() })
    println(len(null)); println(len("abc"))
    try { need(null) } catch (e: IllegalArgumentException) { println(e.message) }
    outer@ for (i in 1..3) { for (j in 1..3) { if (j == 2) continue@outer; if (i == 3) break@outer; print("$i$j ") } }
    println()
    fun local(n: Int): Int = if (n <= 1) 1 else n * local(n - 1)
    println(local(5))
    val x: Any = "str"
    if (x is String && x.length > 2) println(x.uppercase())
    val y: Any = 42
    val desc = when (y) { is Int -> "int ${y + 1}"; is String -> "s"; else -> "?" }
    println(desc)
    println((10 downTo 1 step 3).toList()); println((0 until 10 step 4).toList())
    println(('a'..'e').joinToString("")); println('a' + 2); println('z' - 'a'); println('A'.code)
    for (ch in "hi") print(ch.uppercaseChar())
    println()
    val nested: String? = listOf<String?>(null, "v").last()
    println(nested?.let { "got $it" } ?: "none")
    listOf(1, 2, 3).forEach lit@{ if (it == 2) return@lit; print(it) }
    println()
    println(run { listOf(1, 2).map { v -> v * 3 } })
}"##
        ),
        "1024\n5\n8\nb\n[3, 7]\n-1\n3\nnull!\n11 21 \n120\nSTR\nint 43\n[10, 7, 4, 1]\n[0, 4, 8]\nabcde\nc\n25\n65\nHI\ngot v\n13\n[3, 6]\n"
    );
    assert_eq!(
        prog(
            r##"class Holder<T>(val v: T) where T : Comparable<T> { fun bigger(o: T) = if (v > o) v else o }
fun main() { println(Holder(3).bigger(5)) }"##
        ),
        "5\n"
    );
}

/// A class implementing `Sequence<T>` (`override fun iterator()`) is a sequence:\n`for`, `toList`, `filter`/`map`/`take`/`first` over it, lazily — an infinite one\nstops at its `take`. It was `unresolved supertype Sequence`. The rest pins\ninit order through a super call, user `equals`/`hashCode` in hash containers,\ncomparator objects and destructuring lambda parameters.
#[test]
fn a_user_class_implementing_sequence() {
    assert_eq!(
        prog(
            r##"open class Base(val tag: String) {
    init { println("Base init $tag") }
    open val size: Int get() = 1
    open fun show() = "Base($tag, $size)"
}
class Derived(tag: String, val extra: Int) : Base(tag.uppercase()) {
    init { println("Derived init $extra") }
    override val size get() = extra * 2
    override fun show() = "D:" + super.show()
}
class Pt(val x: Int, val y: Int) {
    override fun equals(other: Any?) = other is Pt && other.x == x && other.y == y
    override fun hashCode() = 31 * x + y
    override fun toString() = "($x,$y)"
}
class Countdown(val from: Int) : Sequence<Int> { override fun iterator() = object : Iterator<Int> { var n = from; override fun hasNext() = n > 0; override fun next() = n-- } }
fun main() {
    println(Derived("d", 3).show())
    println(setOf(Pt(1, 2), Pt(1, 2), Pt(2, 1)).size)
    println(mapOf(Pt(0, 0) to "o")[Pt(0, 0)])
    println(listOf(Pt(1, 2), Pt(0, 5)).sortedWith(Comparator { a, b -> a.x - b.x }))
    println(listOf(Pt(1, 2), Pt(0, 5)).sortedWith(compareByDescending<Pt> { it.y }.thenBy { it.x }))
    println(listOf(1 to "a", 2 to "b").map { (n, s) -> "$s$n" })
    println(Countdown(4).toList()); println(Countdown(5).filter { it % 2 == 1 }.joinToString())
    val cmp = object : Comparator<String> { override fun compare(a: String, b: String) = b.compareTo(a) }
    println(listOf("b", "c", "a").sortedWith(cmp))
    println(listOf("bb", "a", "ccc").sortedBy { it.length }.reversed())
    val lz by lazy { "lazy!" }
    println(lz)
    println(Pt(3, 4) == Pt(3, 4)); println(Pt(3, 4) === Pt(3, 4))
}"##
        ),
        "Base init D\nDerived init 3\nD:Base(D, 6)\n2\no\n[(0,5), (1,2)]\n[(0,5), (1,2)]\n[a1, b2]\n[4, 3, 2, 1]\n5, 3, 1\n[c, b, a]\n[ccc, bb, a]\nlazy!\ntrue\nfalse\n"
    );
    assert_eq!(
        prog(
            r##"class Nat : Sequence<Int> { override fun iterator() = object : Iterator<Int> { var n = 0; override fun hasNext() = true; override fun next() = n++ } }
fun main() {
    println(Nat().take(5).toList())
    println(Nat().map { it * it }.filter { it % 2 == 0 }.take(3).toList())
    println(Nat().first { it > 10 })
    for (x in Nat()) { if (x > 3) break; print(x) }
    println()
    val s: Sequence<Int> = Nat()
    println(s.take(2).toList())
    println(Nat() is Sequence<*>)
}"##
        ),
        "[0, 1, 2, 3, 4]\n[0, 4, 16]\n11\n0123\n[0, 1]\ntrue\n"
    );
}

/// `Result.mapCatching` and `recoverCatching` run their transform inside\n`runCatching`, so an exception it throws becomes the failure; both were\n`unresolved reference`. The rest pins custom exception classes, `finally`\nordering, a `try` expression, causes and `runCatching` idioms.
#[test]
fn result_map_catching_and_recover_catching() {
    assert_eq!(
        prog(
            r##"class AppError(msg: String, val code: Int) : Exception(msg)
class NotFound(what: String) : RuntimeException("$what not found")
fun find(k: String): Int = mapOf("a" to 1)[k] ?: throw NotFound(k)
fun parse(s: String): Result<Int> = runCatching { s.toInt() }
fun main() {
    val r = try { find("b") } catch (e: NotFound) { println(e.message); -1 } finally { println("finally") }
    println(r)
    try { throw AppError("bad", 42) } catch (e: AppError) { println("${e.message} ${e.code} ${e is Exception}") }
    try { try { error("inner") } finally { println("inner finally") } } catch (e: IllegalStateException) { println("caught ${e.message}") }
    try { try { 1 / 0 } catch (e: ArithmeticException) { throw IllegalStateException("wrapped", e) } } catch (e: Exception) { println("${e.message} <- ${e.cause?.message}") }
    println(listOf("1", "x", "3").map { parse(it) }.map { it.getOrElse { 0 } })
    println(parse("7").mapCatching { it * 2 }.getOrThrow())
    println(parse("q").exceptionOrNull()?.javaClass?.simpleName)
    val results = listOf("4", "z").map(::parse)
    println(results.filter { it.isSuccess }.map { it.getOrNull() })
    println(runCatching { require(false) { "nope" } }.exceptionOrNull()?.message)
    fun f(): Nothing = throw UnsupportedOperationException("never")
    println(runCatching { f() }.isFailure)
    val x = try { "5".toInt() } catch (e: Exception) { 0 }
    println(x)
    println(kotlin.runCatching { listOf<Int>().first() }.onFailure { println("fail: ${it.message}") }.getOrDefault(9))
}"##
        ),
        "b not found\nfinally\n-1\nbad 42 true\ninner finally\ncaught inner\nwrapped <- / by zero\n[1, 0, 3]\n14\nNumberFormatException\n[4]\nnope\ntrue\n5\nfail: List is empty.\n9\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val r = runCatching { 4 }
    println(r.mapCatching { it / 0 })
    println(r.mapCatching { it + 1 })
    val f = runCatching<Int> { error("x") }
    println(f.recoverCatching { 9 })
    println(f.recoverCatching { throw IllegalArgumentException("y") }.exceptionOrNull())
    println(r.recoverCatching { 0 })
}"##
        ),
        "Failure(java.lang.ArithmeticException: / by zero)\nSuccess(5)\nSuccess(9)\njava.lang.IllegalArgumentException: y\nSuccess(4)\n"
    );
}

/// Prefix `+x` (`unaryPlus`, the identity on a number), `a..<b` (`rangeUntil`)\nand `expr op= v` on an operand no assignment can rebind (`m[k]!! += 2`,\n`mk() += 4`, the `…Assign` convention) parse and run; each was a parse error\n(`unexpected token Plus`, `unexpected token Lt`, `invalid assignment\ntarget`). The rest pins a user `Comparable` through `sorted`/`max`/ranges/\n`when`.
#[test]
fn unary_plus_range_until_and_compound_assign_on_values() {
    assert_eq!(
        prog(
            r##"data class Ver(val major: Int, val minor: Int) : Comparable<Ver> {
    override fun compareTo(other: Ver) = compareValuesBy(this, other, { it.major }, { it.minor })
    operator fun rangeTo(o: Ver) = VerRange(this, o)
    operator fun unaryPlus() = Ver(major + 1, 0)
}
class VerRange(val lo: Ver, val hi: Ver) { operator fun contains(v: Ver) = v >= lo && v <= hi }
class Counter { var n = 0; operator fun inc(): Counter { n++; return this } }
class Holder { var c = 0 }
fun main() {
    val vs = listOf(Ver(1, 2), Ver(0, 9), Ver(1, 0))
    println(vs.sorted()); println(vs.max()); println(vs.min())
    println(Ver(1, 1) in Ver(1, 0)..Ver(2, 0)); println(Ver(3, 0) !in Ver(1, 0)..Ver(2, 0))
    println(+Ver(1, 5))
    for (i in 0..<3) print(i)
    println()
    val h = Holder(); h.c++; h.c += 5; println(h.c)
    val m = mutableMapOf("a" to mutableListOf(1)); m["a"]!! += 2; m.getOrPut("b") { mutableListOf() } += 3; println(m)
    val arr = intArrayOf(1, 2, 3); arr[1] *= 10; arr[2]++; println(arr.toList())
    val v = Ver(2, 3)
    println(when (v) { in Ver(0, 0)..Ver(1, 9) -> "old"; in Ver(2, 0)..Ver(2, 9) -> "two"; else -> "new" })
    var cnt = Counter(); cnt++; cnt++; println(cnt.n)
    println(Ver(1, 2) == Ver(1, 2)); println(Ver(1, 2) < Ver(1, 3)); println(maxOf(Ver(0, 1), Ver(0, 2)))
    println(vs.sortedDescending().first()); println(vs.minOrNull()); println(vs.sortedWith(reverseOrder()))
}"##
        ),
        "[Ver(major=0, minor=9), Ver(major=1, minor=0), Ver(major=1, minor=2)]\nVer(major=1, minor=2)\nVer(major=0, minor=9)\ntrue\ntrue\nVer(major=2, minor=0)\n012\n6\n{a=[1, 2], b=[3]}\n[1, 20, 4]\ntwo\n2\ntrue\ntrue\nVer(major=0, minor=2)\nVer(major=1, minor=2)\nVer(major=0, minor=9)\n[Ver(major=1, minor=2), Ver(major=1, minor=0), Ver(major=0, minor=9)]\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    val x = 5
    println(+x * 2)
    println(+3)
    println(-(+x))
    println(+2.5)
    println(+Int.MAX_VALUE + 1)
}"##
        ),
        "10\n3\n-5\n2.5\n-2147483648\n"
    );
    assert_eq!(
        prog(
            r##"fun main() {
    for (i in 0..<3) print(i)
    println()
    println((1..<4).toList())
    val n = 5
    println(when (n) { in 0..<5 -> "lo"; else -> "hi" })
    println((0..<0).toList())
    println(2 in 0..<2)
}"##
        ),
        "012\n[1, 2, 3]\nhi\n[]\nfalse\n"
    );
    assert_eq!(
        prog(
            r##"class Money(val c: Int) { operator fun plusAssign(o: Int) { println("pa $o") } }
fun mk() = Money(1)
fun main() {
    mk() += 4
    val m = mutableMapOf("k" to mutableSetOf(1))
    m["k"]!! -= 1
    m["k"]!! += setOf(7, 8)
    println(m)
}"##
        ),
        "pa 4\n{k=[7, 8]}\n"
    );
}

/// An erased `==` is the boxed class's `equals`, which checks the class before
/// the value: no integral box equals a floating-point one, and a `Long` box
/// equals neither an `Int` nor a `Double`. Measured against kotlinc 2.4.21 /
/// JDK 25.
#[test]
fn erased_numbers_of_different_classes_are_never_equal() {
    assert_eq!(
        prog(
            r##"fun main() {
    val x: Any? = 2.0
    val y: Any = 2
    val l: Any = 2L
    val f: Any = 2.0f
    println(listOf(x == 2, x in listOf(1, 2, 3), y in listOf(2.0), x == 2.0))
    println(listOf(l == x, x == l, f == y, y == f, l == f, y == 2))
    println(listOf(1, 2.0, 3L).indexOf(2))
    println(listOf<Any>(2, 2.0, 2L).distinct())
    println(mapOf<Any, String>(1 to "i", 1.0 to "d", 1L to "l").size)
    println(listOf<Any>(3.0 / 1.5).contains(2))
}"##
        ),
        "[false, false, false, true]\n[false, false, false, false, false, true]\n-1\n[2, 2.0, 2]\n3\nfalse\n"
    );
}

/// `1L..10L` is a `LongRange`: its elements are `Long`s, it is its own class
/// with its own `equals`/`hashCode`, and `contains` takes either width from a
/// typed value but only a `Long` from an erased one. Measured against kotlinc
/// 2.4.21 / JDK 25.
#[test]
fn long_ranges_keep_their_width() {
    assert_eq!(
        prog(
            r##"fun main() {
    val i = 5
    val l = 5L
    val b: Any = 5
    val a: Any = 5L
    println(listOf(i in 1L..10L, l in 1..10, b in 1L..10L, a in 1L..10L, b in 1..10, a in 1..10))
    println(listOf((1L..10L).contains(i), (1..10).contains(l)))
    val r: Any = 1L..3L
    println(listOf(r is LongRange, r is IntRange, r is LongProgression, (1..3) is IntProgression))
    println((1L..3L).toList() == listOf(1L, 2L, 3L))
    println((1L..3L).toList().contains(2L))
    val xs: List<Any> = (1L..2L).toList()
    println(listOf(xs.contains(1), xs.contains(1L), xs[0] is Long))
    println((1..4L).toList().contains(4L))
    println(listOf((1..3) == (1L..3L) as Any, LongRange(1, 3) == 1L..3L))
    println(listOf((1L..3L).hashCode(), (1L..10L step 3).hashCode(), (-5L..5_000_000_000L).hashCode()))
    println((1L until 4L).reversed().javaClass.name)
    val z: Any = 3
    println(when (z) { in 1L..5L -> "in"; else -> "out" })
    println(listOf((1L..9L).start, (1L..9L).endInclusive, (1..9).endInclusive))
}"##
        ),
        "[true, true, false, true, true, false]\n[true, true]\n[true, false, true, true]\ntrue\ntrue\n[false, true, true]\ntrue\n[false, true]\n[34, 1274, 705032829]\nkotlin.ranges.LongProgression\nout\n[1, 9, 9]\n"
    );
}

/// `Byte` and `Short` are types of their own: a value of either boxes to
/// `java.lang.Byte`/`Short` in an erased position, so `is Byte` is false for an
/// `Int`, `is Int` false for a `Byte`, and `1.toByte() as Any` equals no `1`.
/// Arithmetic promotes both to `Int`; `toByte`/`toShort`/`inc`/`++` narrow
/// back. Overloads, extensions and `javaClass` see the type. Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn byte_and_short_are_their_own_runtime_types() {
    assert_eq!(
        prog(
            r##"fun f(x: Any) = when (x) { is Byte -> "B$x"; is Short -> "S$x"; is Int -> "I$x"; is Long -> "L$x"; else -> "?" }
fun g(b: Byte) = "byte"
fun g(s: Short) = "short"
fun g(i: Int) = "int"
fun main() {
    println((listOf(1) as List<Any>)[0] is Byte)
    val b: Byte = 10
    val s: Short = 300
    println(f(b)); println(f(s)); println(f(1)); println(f(1L))
    println(b + b); println(f(b + b)); println(f(-b)); println(f(b.inc()))
    val xs = listOf<Byte>(1, 2, 3)
    println(xs); println(xs.map { f(it) })
    println(xs[0] + 1)
    println(f(xs[0])); println(xs.sum()); println(f(xs.sum()))
    println(200.toByte()); println(f(200.toByte())); println(70000.toShort()); println(f(70000.toShort()))
    println((1.toByte() as Any) == (1 as Any))
    println(listOf<Any>(1.toByte(), 1, 1.toShort(), 1L).distinct())
    println(b.hashCode()); println(b.javaClass); println(b::class)
    println(g(b)); println(g(s)); println(g(1))
    var c: Byte = 127; c++; println(c); println(f(c))
    val ba = ByteArray(3) { (it * 100).toByte() }
    println(ba.toList()); println(f(ba[1])); println(ba.contentToString())
    val sa = shortArrayOf(1, 2, 40000.toShort())
    println(sa.toList()); println(f(sa[2]))
    println(b.toString() + s); println("$b $s")
    println(b.compareTo(s)); println(b < s)
    println(b == 10.toByte())
    val m = mapOf<Any, String>(1.toByte() to "b", 1 to "i")
    println(m.size); println(m[1.toByte()]); println(m[1])
    println(Byte.MAX_VALUE); println(f(Byte.MAX_VALUE)); println(f(Short.MIN_VALUE))
    println((b * 20).toByte())
    println(b / 3); println(b % 3); println(f(b / 3))
    val any: Any = b
    println((any as Byte) + 1)
    println(any as? Int)
    println(b.toInt() + b.toLong())
}
"##
        ),
        "false\nB10\nS300\nI1\nL1\n20\nI20\nI-10\nB11\n[1, 2, 3]\n[B1, B2, B3]\n2\nB1\n6\nI6\n-56\nB-56\n4464\nS4464\nfalse\n[1, 1, 1, 1]\n10\nbyte\nclass kotlin.Byte\nbyte\nshort\nint\n-128\nB-128\n[0, 100, -56]\nB100\n[0, 100, -56]\n[1, 2, -25536]\nS-25536\n10300\n10 300\n-1\ntrue\ntrue\n2\nb\ni\n127\nB127\nS-32768\n-56\n3\n1\nI3\n11\nnull\n20\n"
    );
}

/// `Byte`/`Short` through the positions that keep no type: `%x` at the
/// value's own width, a `data class` property, a generic `id`, a `List<Byte>`
/// literal, `Map<Byte, _>` keys, `when` over a `Byte`, `ByteArray` members,
/// and `setOf<Any>(1.toByte(), 1.toShort(), 1, 1L)` holding four. Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn byte_and_short_in_collections_formats_and_data_classes() {
    assert_eq!(
        prog(
            r##"data class D(val b: Byte, val s: Short)
fun <T> id(x: T) = x
fun r(): Any = 5.toByte()
fun main() {
    val b: Byte = -1
    val s: Short = -1
    println("%x %x %d %d".format(b, s, b, s))
    println(D(1, 2)); println(D(1, 2) == D(1, 2)); println(D(1, 2).hashCode())
    println(id(b) is Byte); println(id(s) is Short); println(r() is Byte); println(r())
    val xs = mutableListOf<Byte>(3, 1, 2)
    xs.add(9); xs.sort(); println(xs); println(xs.contains(9)); println(xs.indexOf(2))
    println(xs.max()); println(xs.maxOf { it }); println(xs.map { it * 2 })
    println(xs.filter { it > 1 }); println(xs.sumOf { it.toInt() })
    val m = mutableMapOf<Byte, String>()
    m[1] = "a"; m[2] = "b"; println(m); println(m[1]); println(m.containsKey(2))
    val t: Byte = 5
    when (t) { 5.toByte() -> println("five") else -> println("other") }
    when (t) { in 1..10 -> println("in") else -> println("out") }
    println(b.equals(s)); println(b.equals(-1)); println(b.equals((-1).toByte()))
    println(b.hashCode()); println(s.hashCode()); println(b.toString())
    println(b.compareTo(1)); println(maxOf(b, 3.toByte()))
    val ba = byteArrayOf(1, 2, 127)
    println(ba.sum()); println(ba.map { it + 1 }); println(ba.toList().map { it is Byte })
    println(ba.joinToString()); println(ba.reversed()); println(ba.size)
    var acc: Short = 0
    for (x in ba) acc = (acc + x).toShort()
    println(acc)
    println(Byte.MAX_VALUE + 1); println((Byte.MAX_VALUE + 1).toByte())
    println(300.toShort().toByte()); println((-129).toByte()); println(65535.toShort())
    println(1.5.toInt().toByte()); println(b.toInt() and 0xFF); println(b.toLong()); println(b.toDouble()); println(b.toFloat())
    println(listOf<Any>(1.toByte(), 1.toShort(), 1).map { it.javaClass.simpleName })
    println(listOf(1.toByte(), 2.toByte()) + 3.toByte())
    val anyS: Any = s
    println(anyS == (-1).toShort()); println(anyS == -1)
    println(setOf<Any>(1.toByte(), 1.toShort(), 1, 1L).size)
}
"##
        ),
        "ff ffff -1 -1\nD(b=1, s=2)\ntrue\n33\ntrue\ntrue\ntrue\n5\n[1, 2, 3, 9]\ntrue\n1\n9\n9\n[2, 4, 6, 18]\n[2, 3, 9]\n15\n{1=a, 2=b}\na\ntrue\nfive\nin\nfalse\nfalse\ntrue\n-1\n-1\n-1\n-1\n3\n130\n[2, 3, 128]\n[true, true, true]\n1, 2, 127\n[127, 2, 1]\n3\n130\n128\n-128\n44\n127\n-1\n1\n255\n-1\n-1.0\n-1.0\n[Byte, Short, Integer]\n[1, 2, 3]\ntrue\nfalse\n4\n"
    );
}

/// Overload and extension choice by `Byte`/`Short`, the `Int` a `Byte`
/// operator answers, `--` wrapping at `Short.MIN_VALUE`, and `List<Short>`
/// folds and reductions. Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn byte_and_short_overloads_extensions_and_steps() {
    assert_eq!(
        prog(
            r##"fun Byte.twice() = this * 2
fun Short.name() = "short"
fun Int.name() = "int"
fun h(x: Byte) = "byte $x"
fun h(x: Long) = "long $x"
fun k(x: Short): Short = (x * 2).toShort()
fun main() {
    val b: Byte = -128
    println(b.twice()); println(5.toShort().name()); println(5.name())
    println(h(b)); println(k(20000)); println(k(20000) is Short)
    println(-b); println((-b).toByte())
    val z: Byte = 0
    try { println(b / z) } catch (e: ArithmeticException) { println(e.message) }
    val xs = listOf<Short>(100, 200)
    println(xs[0] + xs[1]); println(xs[0] * xs[1]); println((xs[0] * xs[1]).toShort())
    println(xs.reduce { a, c -> (a + c).toShort() })
    println(xs.fold(0) { a, c -> a + c })
    val sa = ShortArray(2); sa[0] = 7; sa[1] = (sa[0] * 3).toShort(); println(sa.toList())
    println(sa.sorted()); println(sa.average())
    var c: Short = Short.MIN_VALUE; c--; println(c)
    var d: Byte = 0; d = d.dec(); println(d); println(++d); println(d--); println(d)
    println(b.toString(16)); println(b.toUByte())
    println("${xs[0]}-${b}")
    println(b.coerceIn(-5, 5))
    println(xs.sorted().reversed()); println(xs.groupBy { it > 150 })
}
"##
        ),
        "-256\nshort\nint\nbyte -128\n-25536\ntrue\n128\n-128\n/ by zero\n300\n20000\n20000\n300\n300\n[7, 21]\n[7, 21]\n14.0\n32767\n-1\n0\n0\n-1\n-80\n128\n100--128\n-5\n[200, 100]\n{false=[100], true=[200]}\n"
    );
}

/// An integer literal takes a `Byte`/`Short` declaration's type only when it
/// fits; one that does not is an `Int`, which the declaration refuses — as
/// `kotlinc` does (`initializer type mismatch: expected 'Byte', actual 'Int'.`).
/// Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn an_out_of_range_literal_does_not_initialize_a_byte() {
    for (src, want) in [
        (
            "val b: Byte = 200; println(b)",
            "expected 'Byte', actual 'Int'.",
        ),
        (
            "val s: Short = -32769; println(s)",
            "expected 'Short', actual 'Int'.",
        ),
    ] {
        let err = prog_err(src);
        assert!(
            err.contains(&format!("initializer type mismatch: {want}")),
            "{src}: {err}"
        );
    }
    assert_eq!(
        stdout("val b: Byte = -128; val s: Short = 32767; println(\"$b $s\")"),
        "-128 32767\n"
    );
}

/// A `when` with no `else` over an open type is a STATEMENT in the tail of a
/// lambda (and of an `if`/`try` statement), so the lambda answers `Unit`
/// whichever arm ran; only where its value is used must it be exhaustive.
/// Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn a_non_exhaustive_when_in_a_lambda_tail_is_a_statement() {
    assert_eq!(
        prog(
            r##"fun f(x: Int) { println("f"); when (x) { 1 -> println("a") } }
fun main() {
    val x = 3
    val a = { y: Int -> when (y) { 1 -> 2 } }
    println(a(1))
    println(run { when (x) { 1 -> "a" } })
    println(listOf(1, 2).map { when (it) { 1 -> "a" } })
    println({ try { when (x) { 1 -> "a" } } finally { } }())
    println({ when (x) { 1 -> when (x) { 2 -> "q" } } }())
    println({ when (x) { 3 -> "three"; else -> "z" } }())
    val t: () -> Any = { when (x) { 1 -> "a" } }
    println(t())
    println(x.let { when (it) { 1 -> 9 } })
    f(2)
    if (x > 0) { when (x) { 1 -> println("a") } }
    try { when (x) { 1 -> println("a") } } finally { println("fin") }
    when (x) { 1 -> println("a") }
    println(when (x) { 3 -> "three"; else -> "z" })
}
"##
        ),
        "kotlin.Unit\nkotlin.Unit\n[kotlin.Unit, kotlin.Unit]\nkotlin.Unit\nkotlin.Unit\nthree\nkotlin.Unit\nkotlin.Unit\nf\nfin\nthree\n"
    );
}

/// A `when` with no `else` over an open type whose VALUE is used — a function
/// expression body, a `return`, an initializer, an argument, an `if`
/// expression's branch, a compound assignment, an explicitly `Unit` body — is
/// refused, as `kotlinc` refuses it. Each program below is a compile error on
/// kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn a_used_non_exhaustive_when_is_refused() {
    for src in [
        "fun g(x: Int) = when (x) { 1 -> 2 }\nfun main() { println(g(1)) }",
        "fun k(x: Int) { return when (x) { 1 -> println(\"a\") } }\nfun main() { k(1) }",
        "fun h(x: Int): Unit = when (x) { 1 -> println(\"a\") }\nfun main() { h(1) }",
        "fun main() { val x = 3; val y = when (x) { 1 -> \"a\" }; println(y) }",
        "fun main() { val x = 3; println(when (x) { 1 -> \"a\" }) }",
        "fun main() { val x = 1; val y = when { x > 0 -> \"p\" }; println(y) }",
        "fun main() { val x = 5; var s = \"\"; s += when (x) { 1 -> \"a\" }; println(s) }",
        "fun main() { val x = 3; val c = true; val y = if (c) { when (x) { 1 -> \"a\" } } else \"b\"; println(y) }",
        "fun main() { val x: Any = 3; val y = if (x is Int) when (x) { 1 -> \"a\" } else \"b\"; println(y) }",
    ] {
        let err = prog_err(src);
        assert!(
            err.contains("'when' expression must be exhaustive. Add an 'else' branch."),
            "{src}: {err}"
        );
    }
}

/// Which lambdas own a stack-trace frame is decided by how `kotlinc` compiles
/// them: one passed to a user `inline fun` is copied into the caller with the
/// function, so neither appears; a lambda held in a `val` and called keeps its
/// `main$lambda$N` frame even when its own body calls it; an unannotated
/// `val k = { throw … }` is `() -> Nothing`, compiled to a class whose
/// bridge and typed `invoke` both appear and which takes no `$lambda$N`
/// index. Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn lambda_frames_follow_the_jvm_back_end() {
    assert_eq!(
        prog(
            r##"inline fun twice(f: () -> Unit) { f(); f() }
fun h(f: () -> Unit) = f()
fun tr(e: Throwable) = println(e.stackTrace.joinToString(" | "))
class C { fun m() { val q = { throw Exception("c") }; try { q() } catch (e: Exception) { tr(e) } } }
fun main() {
    try { twice { throw IllegalStateException("x") } } catch (e: Exception) { tr(e) }
    try { run { twice { error("r") } } } catch (e: Exception) { tr(e) }
    try { h { throw Exception("a") } } catch (e: Exception) { tr(e) }
    val g = { x: Int -> if (x > 0) throw Exception("b") else x }
    try { g(1) } catch (e: Exception) { tr(e) }
    val k = { throw Exception("k") }
    try { k() } catch (e: Exception) { tr(e) }
    C().m()
    val n: () -> Int = { throw Exception("n") }
    try { n() } catch (e: Exception) { tr(e) }
    try { listOf(1).map { throw Exception("m") } } catch (e: Exception) { tr(e) }
}
"##
        ),
        "MainKt.main(main.kt:6) | MainKt.main(main.kt)\nMainKt.main(main.kt:7) | MainKt.main(main.kt)\nMainKt.main$lambda$2(main.kt:8) | MainKt.h(main.kt:2) | MainKt.main(main.kt:8) | MainKt.main(main.kt)\nMainKt.main$lambda$3(main.kt:9) | MainKt.main(main.kt:10) | MainKt.main(main.kt)\nMainKt$main$k$1.invoke(main.kt:11) | MainKt$main$k$1.invoke(main.kt:11) | MainKt.main(main.kt:12) | MainKt.main(main.kt)\nC$m$q$1.invoke(main.kt:4) | C$m$q$1.invoke(main.kt:4) | C.m(main.kt:4) | MainKt.main(main.kt:13) | MainKt.main(main.kt)\nMainKt.main$lambda$4(main.kt:14) | MainKt.main(main.kt:15) | MainKt.main(main.kt)\nMainKt.main(main.kt:16) | MainKt.main(main.kt)\n"
    );
}

/// A `throw` that reaches the top of `main` prints its trace after the
/// `Exception in thread "main"` line, as a runtime-raised fault's already
/// did; it printed the line alone. Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn an_uncaught_throw_prints_its_stack_trace() {
    let src = r##"fun twice(f: () -> Unit) { f(); f() }
fun main() {
    println("start")
    twice { throw IllegalStateException("x") }
}
"##;
    assert_eq!(eval(src).status.code(), Some(1));
    assert_eq!(prog_err(src), "Exception in thread \"main\" java.lang.IllegalStateException: x\n\tat MainKt.main$lambda$0(main.kt:4)\n\tat MainKt.twice(main.kt:1)\n\tat MainKt.main(main.kt:4)\n\tat MainKt.main(main.kt)\n");
}

/// A fault raised by a stdlib member's own argument or emptiness check has
/// that member's frames (and the `java.util` class behind a list) in front of
/// the user's, as on the JVM. `listIterator(5)` reports the size as well.
/// Measured on kotlinc 2.4.21 / JRE 21.0.13.
#[test]
fn stdlib_argument_checks_have_their_stdlib_frames() {
    assert_eq!(
        prog(
            r##"fun t(f: () -> Any?) = try { f(); "ok" } catch (e: Exception) { e.stackTrace.dropLast(2).joinToString(" | ") }
fun main() {
    println(t { ArrayDeque<Int>().removeFirst() })
    println(t { ArrayDeque<Int>().removeLast() })
    println(t { ArrayDeque<Int>().first() })
    println(t { ArrayDeque<Int>().last() })
    println(t { setOf<Int>().first() })
    println(t { setOf<Int>().last() })
    println(t { setOf<Int>().single() })
    println(t { setOf(1, 2).single() })
    println(t { listOf<Int>().single() })
    println(t { emptyList<Int>().random() })
    println(t { sequenceOf<Int>().first() })
    println(t { sequenceOf<Int>().last() })
    println(t { "".last() })
    println(t { "abc".take(-1) })
    println(t { "abc".takeLast(-1) })
    println(t { "abc".drop(-1) })
    println(t { listOf(1).take(-1) })
    println(t { listOf(1).takeLast(-1) })
    println(t { listOf(1).drop(-1) })
    println(t { listOf(1).dropLast(-1) })
    println(t { "ab".padStart(-1) })
    println(t { "ab".padStart(-1, 'x') })
    println(t { "ab".padEnd(-1) })
    println(t { listOf(1, 2).chunked(0) })
    println(t { listOf(1, 2).windowed(0) })
    println(t { "abc".chunked(0) })
    println(t { 'x'.digitToInt() })
    println(t { 'a'.digitToInt(37) })
    println(t { "x".toInt(1) })
    println(t { mutableListOf(1).add(5, 1) })
    println(t { listOf(1).elementAt(4) })
    println(t { listOf(1, 2).subList(0, 5) })
    println(t { mutableListOf(1, 2).subList(0, 5) })
    println(t { listOf(1).listIterator(5) })
    println(t { mutableListOf(1).listIterator(5) })
    try { mutableListOf(1).listIterator(5) } catch (e: Exception) { println(e.message) }
}
"##
        ),
        "kotlin.collections.ArrayDeque.removeFirst(ArrayDeque.kt:147) | MainKt.main$lambda$0(main.kt:3) | MainKt.t(main.kt:1)\nkotlin.collections.ArrayDeque.removeLast(ArrayDeque.kt:168) | MainKt.main$lambda$1(main.kt:4) | MainKt.t(main.kt:1)\nkotlin.collections.ArrayDeque.first(ArrayDeque.kt:102) | MainKt.main$lambda$2(main.kt:5) | MainKt.t(main.kt:1)\nkotlin.collections.ArrayDeque.last(ArrayDeque.kt:112) | MainKt.main$lambda$3(main.kt:6) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.first(_Collections.kt:209) | MainKt.main$lambda$4(main.kt:7) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.last(_Collections.kt:413) | MainKt.main$lambda$5(main.kt:8) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.single(_Collections.kt:605) | MainKt.main$lambda$6(main.kt:9) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.single(_Collections.kt:608) | MainKt.main$lambda$7(main.kt:10) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.single(_Collections.kt:619) | MainKt.main$lambda$8(main.kt:11) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.random(_Collections.kt:573) | MainKt.main$lambda$9(main.kt:12) | MainKt.t(main.kt:1)\nkotlin.sequences.SequencesKt___SequencesKt.first(_Sequences.kt:117) | MainKt.main$lambda$10(main.kt:13) | MainKt.t(main.kt:1)\nkotlin.sequences.SequencesKt___SequencesKt.last(_Sequences.kt:249) | MainKt.main$lambda$11(main.kt:14) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt___StringsKt.last(_Strings.kt:194) | MainKt.main$lambda$12(main.kt:15) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt___StringsKt.take(_Strings.kt:581) | MainKt.main$lambda$13(main.kt:16) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt___StringsKt.takeLast(_Strings.kt:606) | MainKt.main$lambda$14(main.kt:17) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt___StringsKt.drop(_Strings.kt:350) | MainKt.main$lambda$15(main.kt:18) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.take(_Collections.kt:906) | MainKt.main$lambda$16(main.kt:19) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.takeLast(_Collections.kt:930) | MainKt.main$lambda$17(main.kt:20) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.drop(_Collections.kt:693) | MainKt.main$lambda$18(main.kt:21) | MainKt.t(main.kt:1)\nkotlin.collections.CollectionsKt___CollectionsKt.dropLast(_Collections.kt:732) | MainKt.main$lambda$19(main.kt:22) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt__StringsKt.padStart(Strings.kt:217) | kotlin.text.StringsKt__StringsKt.padStart(Strings.kt:238) | kotlin.text.StringsKt__StringsKt.padStart$default(Strings.kt:237) | MainKt.main$lambda$20(main.kt:23) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt__StringsKt.padStart(Strings.kt:217) | kotlin.text.StringsKt__StringsKt.padStart(Strings.kt:238) | MainKt.main$lambda$21(main.kt:24) | MainKt.t(main.kt:1)\nkotlin.text.StringsKt__StringsKt.padEnd(Strings.kt:252) | kotlin.text.StringsKt__StringsKt.padEnd(Strings.kt:273) | kotlin.text.StringsKt__StringsKt.padEnd$default(Strings.kt:272) | MainKt.main$lambda$22(main.kt:25) | MainKt.t(main.kt:1)\nkotlin.collections.SlidingWindowKt.checkWindowSizeStep(SlidingWindow.kt:9) | kotlin.collections.CollectionsKt___CollectionsKt.windowed(_Collections.kt:3765) | kotlin.collections.CollectionsKt___CollectionsKt.chunked(_Collections.kt:3556) | MainKt.main$lambda$23(main.kt:26) | MainKt.t(main.kt:1)\nkotlin.collections.SlidingWindowKt.checkWindowSizeStep(SlidingWindow.kt:9) | kotlin.collections.CollectionsKt___CollectionsKt.windowed(_Collections.kt:3765) | kotlin.collections.CollectionsKt___CollectionsKt.windowed$default(_Collections.kt:3764) | MainKt.main$lambda$24(main.kt:27) | MainKt.t(main.kt:1)\nkotlin.collections.SlidingWindowKt.checkWindowSizeStep(SlidingWindow.kt:9) | kotlin.text.StringsKt___StringsKt.windowed(_Strings.kt:2453) | kotlin.text.StringsKt___StringsKt.windowed(_Strings.kt:2431) | kotlin.text.StringsKt___StringsKt.chunked(_Strings.kt:2319) | MainKt.main$lambda$25(main.kt:28) | MainKt.t(main.kt:1)\nkotlin.text.CharsKt__CharKt.digitToInt(Char.kt:23) | MainKt.main$lambda$26(main.kt:29) | MainKt.t(main.kt:1)\nkotlin.text.CharsKt__CharJVMKt.checkRadix(CharJVM.kt:312) | kotlin.text.CharsKt__CharKt.digitToIntOrNull(Char.kt:74) | kotlin.text.CharsKt__CharKt.digitToInt(Char.kt:42) | MainKt.main$lambda$27(main.kt:30) | MainKt.t(main.kt:1)\nkotlin.text.CharsKt__CharJVMKt.checkRadix(CharJVM.kt:312) | MainKt.main$lambda$28(main.kt:31) | MainKt.t(main.kt:1)\njava.base/java.util.ArrayList.rangeCheckForAdd(ArrayList.java:838) | java.base/java.util.ArrayList.add(ArrayList.java:510) | MainKt.main$lambda$29(main.kt:32) | MainKt.t(main.kt:1)\njava.base/java.util.Collections$SingletonList.get(Collections.java:5180) | MainKt.main$lambda$30(main.kt:33) | MainKt.t(main.kt:1)\njava.base/java.util.AbstractList.subListRangeCheck(AbstractList.java:509) | java.base/java.util.AbstractList.subList(AbstractList.java:499) | MainKt.main$lambda$31(main.kt:34) | MainKt.t(main.kt:1)\njava.base/java.util.AbstractList.subListRangeCheck(AbstractList.java:509) | java.base/java.util.ArrayList.subList(ArrayList.java:1190) | MainKt.main$lambda$32(main.kt:35) | MainKt.t(main.kt:1)\njava.base/java.util.AbstractList.rangeCheckForAdd(AbstractList.java:634) | java.base/java.util.AbstractList.listIterator(AbstractList.java:340) | MainKt.main$lambda$33(main.kt:36) | MainKt.t(main.kt:1)\njava.base/java.util.ArrayList.rangeCheckForAdd(ArrayList.java:838) | java.base/java.util.ArrayList.listIterator(ArrayList.java:1005) | MainKt.main$lambda$34(main.kt:37) | MainKt.t(main.kt:1)\nIndex: 5, Size: 1\n"
    );
}

/// `kotlin.properties.Delegates` spelled out needs no import, and is the same
/// object the import names: `observable`, `vetoable` and `notNull` through
/// it. Bare `Delegates` without the import is `unresolved reference`, since
/// `kotlin.properties` is not a default import. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn the_delegates_object_may_be_spelled_out() {
    assert_eq!(
        prog(
            r##"var obs: Int by kotlin.properties.Delegates.observable(0) { _, o, n -> println("$o -> $n") }
var vet: Int by kotlin.properties.Delegates.vetoable(0) { _, _, n -> n >= 0 }
class C { var nn: String by kotlin.properties.Delegates.notNull() }
fun main() {
    obs = 3; obs = 7; vet = 5; vet = -1; println(vet)
    val c = C()
    try { println(c.nn) } catch (e: IllegalStateException) { println(e.message) }
    c.nn = "set"; println(c.nn)
}
"##
        ),
        "0 -> 3\n3 -> 7\n5\nProperty nn should be initialized before get.\nset\n"
    );
    let err = prog_err(
        "var obs: Int by Delegates.observable(0) { _, o, n -> println(\"$o -> $n\") }\nfun main() { obs = 1 }",
    );
    assert!(err.contains("unresolved reference: Delegates"), "{err}");
}

/// A `lazy` initializer that throws leaves the value uninitialized, so the
/// next read runs it again — a `by lazy` binding, a class property and a
/// `Lazy` read through `value` alike — and `isInitialized()` stays false.
/// It cached the throwing read's `null`. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn a_lazy_initializer_that_throws_runs_again() {
    assert_eq!(
        prog(
            r##"var n = 0
val x: Int by lazy { n++; if (n < 3) throw IllegalStateException("boom $n"); n * 10 }
class C { val y by lazy { n++; if (n < 5) error("c $n"); "ok $n" } }
fun main() {
    repeat(3) { try { println(x) } catch (e: Exception) { println(e.message) } }
    println(x)
    val c = C()
    repeat(3) { try { println(c.y) } catch (e: Exception) { println(e.message) } }
    val l = lazy { n++; if (n < 7) throw RuntimeException("v $n"); n }
    repeat(2) { try { println(l.value) } catch (e: Exception) { println(e.message + " " + l.isInitialized()) } }
    println(l.isInitialized())
}
"##
        ),
        "boom 1\nboom 2\n30\n30\nc 4\nok 5\nok 5\nv 6 false\n7\ntrue\n"
    );
}

/// `toSortedSet(comparator)` on an `Iterable` is a `TreeSet` ordered by the
/// comparator; `String` has no such overload. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn to_sorted_set_takes_a_comparator() {
    assert_eq!(
        prog(
            r##"data class P(val name: String, val age: Int)
fun main() {
    val ps = listOf(P("b", 3), P("a", 1))
    println(ps.toSortedSet(compareBy { it.name }))
    println(listOf(3, 1, 2).toSortedSet())
    println(listOf(3, 1, 2).toSortedSet(reverseOrder()))
    println(setOf("b", "a").toSortedSet())
    extra()
}
fun extra() {
    val s = listOf("ccc", "a", "bb").toSortedSet(compareBy { it.length })
    println(s); println(s.first()); println(s.javaClass.name)
}
"##
        ),
        "[P(name=a, age=1), P(name=b, age=3)]\n[1, 2, 3]\n[3, 2, 1]\n[a, b]\n[a, bb, ccc]\na\njava.util.TreeSet\n"
    );
    // `too many arguments for 'fun CharSequence.toSortedSet(): SortedSet<Char>'`
    // on the reference.
    let err = prog_err(r#"println("hello".toSortedSet(reverseOrder()))"#);
    assert!(err.contains("toSortedSet"), "{err}");
}

/// A class or interface that inherits two different declarations of one
/// member, at least one with a body, and declares none itself must override
/// it. A diamond's single declaration and two abstract ones need nothing.
/// Each refused program is a compile error on kotlinc 2.4.21 / JRE 21.0.13.0.2
/// with the wording asserted.
#[test]
fn conflicting_inherited_members_must_be_overridden() {
    for (src, want) in [
        (
            "interface A { fun f() = \"A\" }\ninterface B { fun f() = \"B\" }\nclass C : A, B\nfun main() { println(C().f()) }",
            "class 'C' must override 'f' because it inherits multiple interface methods for it.",
        ),
        (
            "interface A { fun f() = \"A\" }\ninterface B { fun f(): String }\nclass C : A, B\nfun main() { println(C().f()) }",
            "class 'C' must override 'f' because it inherits multiple interface methods for it.",
        ),
        (
            "open class P { open fun f() = \"P\" }\ninterface I { fun f() = \"I\" }\nclass C : P(), I\nfun main() { println(C().f()) }",
            "class 'C' must override 'f' because it inherits multiple implementations for it.",
        ),
        (
            "interface A { fun f() = \"A\" }\ninterface B { fun f() = \"B\" }\ninterface D : A, B\nfun main() { println(1) }",
            "interface 'D' must override 'f' because it inherits multiple interface methods for it.",
        ),
        (
            "interface A { val p: Int get() = 1 }\ninterface B { val p: Int get() = 2 }\nclass C : A, B\nfun main() { println(C().p) }",
            "class 'C' must override 'p' because it inherits multiple interface methods for it.",
        ),
    ] {
        let err = prog_err(src);
        assert!(err.contains(want), "{src}: {err}");
    }
    assert_eq!(
        prog("interface A { fun f() = \"A\" }\ninterface B : A\ninterface E : A\nclass C : B, E\nfun main() { println(C().f()) }"),
        "A\n"
    );
    assert_eq!(
        prog("interface A { fun f(): String }\ninterface B { fun f(): String }\nabstract class C : A, B\nclass D : C() { override fun f() = \"D\" }\nfun main() { println(D().f()) }"),
        "D\n"
    );
    assert_eq!(
        prog("interface A { fun f() = \"A\" }\ninterface B { fun f() = \"B\" }\nclass C : A, B { override fun f() = super<A>.f() + super<B>.f() }\nfun main() { println(C().f()) }"),
        "AB\n"
    );
}

/// A numeric declaration, assignment or argument takes only its own type —
/// Kotlin converts no number implicitly — while an integer literal adopts any
/// integral type it fits, branch by branch. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn numeric_declarations_take_only_their_own_type() {
    assert_eq!(
        prog(
            r##"fun f(b: Byte, l: Long, d: Double, i: Int = 3) = "$b $l $d $i"
fun main() {
    val c = true; val x = 5; var bb: Byte = 1; var ll = 2L; var dd = 1.5
    val b: Byte = if (c) 1 else 2; println(b)
    ll += 1; println(ll)
    dd += 1; println(dd)
    bb = 3; println(bb)
    bb = (bb + 1).toByte(); println(bb)
    val w: Byte = when (x) { 5 -> 1; else -> 2 }; println(w)
    val big: Long = 2147483648; println(big)
    val d: Double = x.toDouble(); println(d)
    val n: Number = x; println(n)
    println(f(1, 2, 2.5)); println(f(x.toByte(), 2L, x.toDouble(), x))
}
"##
        ),
        "1\n3\n2.5\n3\n4\n1\n2147483648\n5.0\n5\n1 2 2.5 3\n5 2 5.0 5\n"
    );
}

/// The numeric mismatches `kotlinc` refuses, each with its diagnostic. Every
/// program is a compile error on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn numeric_type_mismatches_are_refused() {
    let pre = "val c = true; val x = 5; var bb: Byte = 1; var ll = 2L; var ii = 3; ";
    for (body, want) in [
        (
            "val b: Byte = x",
            "initializer type mismatch: expected 'Byte', actual 'Int'.",
        ),
        (
            "val b: Byte = 1 + 1",
            "initializer type mismatch: expected 'Byte', actual 'Int'.",
        ),
        (
            "val l: Long = x",
            "initializer type mismatch: expected 'Long', actual 'Int'.",
        ),
        (
            "val l: Long = x * 2",
            "initializer type mismatch: expected 'Long', actual 'Int'.",
        ),
        (
            "val s: Short = -x",
            "initializer type mismatch: expected 'Short', actual 'Int'.",
        ),
        (
            "val i: Int = 5L",
            "initializer type mismatch: expected 'Int', actual 'Long'.",
        ),
        (
            "val i: Int = 2147483648",
            "initializer type mismatch: expected 'Int', actual 'Long'.",
        ),
        (
            "val d: Double = 1",
            "initializer type mismatch: expected 'Double', actual 'Int'.",
        ),
        (
            "val f: Float = 1.0",
            "initializer type mismatch: expected 'Float', actual 'Double'.",
        ),
        (
            "val l: Long = bb",
            "initializer type mismatch: expected 'Long', actual 'Byte'.",
        ),
        (
            "bb += 1",
            "assignment type mismatch: actual type is 'Int', but 'Byte' was expected.",
        ),
        (
            "ii += 1L",
            "assignment type mismatch: actual type is 'Long', but 'Int' was expected.",
        ),
        (
            "ii = 5L",
            "assignment type mismatch: actual type is 'Long', but 'Int' was expected.",
        ),
        (
            "ll = x",
            "assignment type mismatch: actual type is 'Int', but 'Long' was expected.",
        ),
    ] {
        let err = prog_err(&format!("fun main() {{ {pre}{body}; println(c) }}"));
        assert!(err.contains(want), "{body}: {err}");
    }
    for (src, want) in [
        (
            "fun f(b: Byte) = b\nfun main() { val x = 3; println(f(x)) }",
            "argument type mismatch: actual type is 'Int', but 'Byte' was expected.",
        ),
        (
            "fun f(l: Long) = l\nfun main() { val x = 3; println(f(x)) }",
            "argument type mismatch: actual type is 'Int', but 'Long' was expected.",
        ),
        (
            "fun f(d: Double) = d\nfun main() { println(f(1)) }",
            "argument type mismatch: actual type is 'Int', but 'Double' was expected.",
        ),
    ] {
        let err = prog_err(src);
        assert!(err.contains(want), "{src}: {err}");
    }
}

/// A range is an `Iterable`, not a `List`: the members only `List` declares
/// are `unresolved reference` on it, named with the range's own class. Each
/// refused program is a compile error on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn list_only_members_are_not_on_ranges() {
    for (expr, want) in [
        (
            "(1..4).reduceRightIndexed { i, a, b -> a + b }",
            "'reduceRightIndexed' on receiver of type 'IntRange'",
        ),
        (
            "(1..4).reduceRight { a, b -> a + b }",
            "'reduceRight' on receiver of type 'IntRange'",
        ),
        (
            "(1..4).foldRight(0) { a, b -> a + b }",
            "'foldRight' on receiver of type 'IntRange'",
        ),
        (
            "(1..4).takeLast(1)",
            "'takeLast' on receiver of type 'IntRange'",
        ),
        (
            "(1..4).getOrNull(1)",
            "'getOrNull' on receiver of type 'IntRange'",
        ),
        (
            "('a'..'d').takeLast(1)",
            "'takeLast' on receiver of type 'CharRange'",
        ),
        (
            "(1L..9L step 2).dropLast(1)",
            "'dropLast' on receiver of type 'LongProgression'",
        ),
    ] {
        let err = prog_err(&format!("fun main() {{ println({expr}) }}"));
        assert!(
            err.contains(&format!("unresolved reference {want}.")),
            "{expr}: {err}"
        );
    }
}
/// The `Iterable` members a range does have, and the `List` members on a
/// list built from one. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn range_members_iterables_do_have() {
    assert_eq!(
        prog(
            r##"fun main() {
    val r = 1..4
    println(r.toList().takeLast(2)); println((1..4 step 2).toList().reduceRight { a, b -> a + b }); println(('a'..'d').last())
    println(r.reversed()); println(r.lastIndexOf(3)); println(listOf(1, 2).foldRight(0) { a, b -> a + b })
}
"##
        ),
        "[3, 4]\n4\nd\n4 downTo 1 step 1\n2\n3\n"
    );
}
/// A `ByteArray`/`ShortArray` element that leaves the array — `toList`,
/// `asList`, `toSet`, `sorted`, `first` — is still a `Byte`/`Short`, so it
/// equals no `Int`. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn byte_and_short_arrays_hold_their_element_type() {
    assert_eq!(
        prog(
            r##"fun main() {
    val l: List<Any> = byteArrayOf(1, 2).toList()
    println(l.contains(1)); println(l[0] is Byte); println(l.map { it is Byte })
    val s: List<Any> = shortArrayOf(1).asList()
    println(s[0] is Short)
    val g: List<Any> = longArrayOf(1L).toList()
    println(g.contains(1)); println(g[0] is Long)
    val f: List<Any> = floatArrayOf(1.5f).toList()
    println(f[0] is Float); println(f)
    val bs: Set<Any> = byteArrayOf(1, 1, 2).toSet(); println(bs.contains(1)); println(bs.size)
    val bl: Any = byteArrayOf(3).first(); println(bl is Byte)
    val sm: List<Any> = byteArrayOf(3, 1).sorted(); println(sm[0] is Byte)
    val mp: List<Any> = byteArrayOf(3).map { it }; println(mp[0] is Byte)
}
"##
        ),
        "false\ntrue\n[true, true]\ntrue\nfalse\ntrue\ntrue\n[1.5]\nfalse\n2\ntrue\ntrue\ntrue\n"
    );
}
/// `next()` past the end of a `Set`'s or `Map`'s iterator has the frames of
/// the JDK iterator behind the collection: `LinkedHashMap`'s, `HashMap`'s,
/// `EmptyIterator`'s or `Collections$1`'s. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn set_and_map_iterators_have_their_jdk_frames() {
    assert_eq!(
        prog(
            r##"fun t(f: () -> Any?) = try { f(); "ok" } catch (e: Exception) { e.toString() + " @ " + e.stackTrace.dropLast(2).joinToString(" | ") }
fun main() {
    println(t { mutableMapOf<Int, Int>().iterator().next() })
    println(t { mutableSetOf<Int>().iterator().next() })
    println(t { mutableMapOf<Int, Int>().keys.iterator().next() })
    println(t { mapOf<Int, Int>().iterator().next() })
    println(t { setOf<Int>().iterator().next() })
    println(t { setOf(1).iterator().let { it.next(); it.next() } })
    println(t { setOf(1, 2).iterator().let { it.next(); it.next(); it.next() } })
    println(t { mapOf(1 to 2).iterator().let { it.next(); it.next() } })
    println(t { mapOf(1 to 2, 3 to 4).iterator().let { it.next(); it.next(); it.next() } })
    println(t { hashMapOf<Int, Int>().iterator().next() })
    println(t { hashSetOf<Int>().iterator().next() })
}
"##
        ),
        "java.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedEntryIterator.next(LinkedHashMap.java:1058) | java.base/java.util.LinkedHashMap$LinkedEntryIterator.next(LinkedHashMap.java:1055) | MainKt.main$lambda$0(main.kt:3) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedKeyIterator.next(LinkedHashMap.java:1046) | MainKt.main$lambda$1(main.kt:4) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedKeyIterator.next(LinkedHashMap.java:1046) | MainKt.main$lambda$2(main.kt:5) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ kotlin.collections.EmptyIterator.next(Collections.kt:21) | kotlin.collections.EmptyIterator.next(Collections.kt:16) | MainKt.main$lambda$3(main.kt:6) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ kotlin.collections.EmptyIterator.next(Collections.kt:21) | kotlin.collections.EmptyIterator.next(Collections.kt:16) | MainKt.main$lambda$4(main.kt:7) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.Collections$1.next(Collections.java:5038) | MainKt.main$lambda$5(main.kt:8) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedKeyIterator.next(LinkedHashMap.java:1046) | MainKt.main$lambda$6(main.kt:9) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.Collections$1.next(Collections.java:5038) | MainKt.main$lambda$7(main.kt:10) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedEntryIterator.next(LinkedHashMap.java:1058) | java.base/java.util.LinkedHashMap$LinkedEntryIterator.next(LinkedHashMap.java:1055) | MainKt.main$lambda$8(main.kt:11) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.HashMap$HashIterator.nextNode(HashMap.java:1607) | java.base/java.util.HashMap$EntryIterator.next(HashMap.java:1638) | java.base/java.util.HashMap$EntryIterator.next(HashMap.java:1636) | MainKt.main$lambda$9(main.kt:12) | MainKt.t(main.kt:1)\njava.util.NoSuchElementException @ java.base/java.util.HashMap$HashIterator.nextNode(HashMap.java:1607) | java.base/java.util.HashMap$KeyIterator.next(HashMap.java:1628) | MainKt.main$lambda$10(main.kt:13) | MainKt.t(main.kt:1)\n"
    );
}
/// A fault in a lambda body whose ops carry no line names the body's own line
/// for the lambda's frame, not the next statement's. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn a_lambda_frame_names_the_lambdas_line() {
    assert_eq!(
        prog(
            r##"fun tr(e: Throwable) { println(e.stackTrace.joinToString(" | ")) }
fun main() {
    println("start")
    val lz = { y: Int -> y / 0 }
    try { lz(1) } catch (e: Exception) { tr(e) }
    val h = { listOf(1)[3] }
    try { h() } catch (e: Exception) { tr(e) }
}
"##
        ),
        "start\nMainKt.main$lambda$0(main.kt:4) | MainKt.main(main.kt:5) | MainKt.main(main.kt)\njava.base/java.util.Collections$SingletonList.get(Collections.java:5180) | MainKt.main$lambda$1(main.kt:6) | MainKt.main(main.kt:7) | MainKt.main(main.kt)\n"
    );
}
/// A pattern `java.util.regex.Pattern` refuses raises `PatternSyntaxException`
/// with `Pattern`'s description, index, pattern and caret, for unclosed and
/// unmatched groups, classes and ranges, dangling quantifiers, counted
/// repetitions, a trailing backslash, inline modifiers and named groups. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn regex_syntax_errors_use_pattern_wording() {
    assert_eq!(
        prog(
            r##"fun t(p: String) = try { Regex(p); "ok" } catch (e: IllegalArgumentException) { "[" + e.javaClass.simpleName + "] " + e.message!!.replace("\n", "\\n") }
fun main() {
    for (p in listOf("(", "a)", "[a", "*a", "a{2,1}", "\\", "a**", "(?<n>a)(?<n>b)", "x{", "[b-a]", "(?<1a>x)", "a{99999999999}", "\\k<zz>", "(?z)", "+", "?", "a|*", "((a)", "[]", "a{,3}"))
        println(p + " => " + t(p))
}
"##
        ),
        "( => [PatternSyntaxException] Unclosed group near index 1\\n(\na) => [PatternSyntaxException] Unmatched closing ')' near index 0\\na)\\n^\n[a => [PatternSyntaxException] Unclosed character class near index 1\\n[a\\n ^\n*a => [PatternSyntaxException] Dangling meta character '*' near index 0\\n*a\\n^\na{2,1} => [PatternSyntaxException] Illegal repetition range near index 5\\na{2,1}\\n     ^\n\\ => [PatternSyntaxException] Unescaped trailing backslash near index 1\\n\\\na** => [PatternSyntaxException] Dangling meta character '*' near index 2\\na**\\n  ^\n(?<n>a)(?<n>b) => [PatternSyntaxException] Named capturing group <n> is already defined near index 11\\n(?<n>a)(?<n>b)\\n           ^\nx{ => [PatternSyntaxException] Illegal repetition near index 2\\nx{\n[b-a] => [PatternSyntaxException] Illegal character range near index 3\\n[b-a]\\n   ^\n(?<1a>x) => [PatternSyntaxException] capturing group name does not start with a Latin letter near index 3\\n(?<1a>x)\\n   ^\na{99999999999} => [PatternSyntaxException] Illegal repetition range near index 11\\na{99999999999}\\n           ^\n\\k<zz> => [PatternSyntaxException] named capturing group <zz> does not exist near index 5\\n\\k<zz>\\n     ^\n(?z) => [PatternSyntaxException] Unknown inline modifier near index 2\\n(?z)\\n  ^\n+ => [PatternSyntaxException] Dangling meta character '+' near index 0\\n+\\n^\n? => [PatternSyntaxException] Dangling meta character '?' near index 0\\n?\\n^\na|* => [PatternSyntaxException] Dangling meta character '*' near index 2\\na|*\\n  ^\n((a) => [PatternSyntaxException] Unclosed group near index 4\\n((a)\n[] => [PatternSyntaxException] Unclosed character class near index 1\\n[]\\n ^\na{,3} => [PatternSyntaxException] Illegal repetition near index 2\\na{,3}\\n  ^\n"
    );
}
/// An initializer may read a property declared above it, a constructor
/// parameter, a function, or — inside a lambda, which runs later — anything.
/// Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn initializers_read_what_is_already_initialized() {
    assert_eq!(
        prog(
            r##"object O { val a = { b + 1 }; val b = 2 }
class K(val y: Int) { val x = y * 2; val z = f(); val w = 3; fun f() = w }
class L { val p = 1; init { println("p=$p") }; val q = p + 1; init { val r = 5; println(q + r) } }
fun main() { println(O.a()); println(K(1).x); println(K(1).z); L() }
"##
        ),
        "3\n2\n0\np=1\n7\n"
    );
}

/// An initializer (or `init` block) that reads a property declared below it
/// is refused: `variable 'w' must be initialized.` Each program is a compile
/// error on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn reading_a_property_before_its_initializer_is_refused() {
    for (src, name) in [
        ("class K { init { println(y) }; val y = 3 }\nfun main() { K() }", "y"),
        ("val a = b + 1\nval b = 2\nfun main() { println(a) }", "b"),
        ("class K(val y: Int) { val x = y * 2; val z = w; val w = 1 }\nfun main() { println(K(1).x) }", "w"),
        ("class K { val x = this.y * 2; val y = 3 }\nfun main() { println(K().x) }", "y"),
        ("class K { var x = 1; val z = x + w; var w = 2 }\nfun main() { println(K().z) }", "w"),
    ] {
        let err = prog_err(src);
        assert!(err.contains(&format!("variable '{name}' must be initialized.")), "{src}: {err}");
    }
}
/// A parameterless lambda held in a property with no written type is a
/// `() -> R` value, and `obj.prop()` invokes it. It printed the closure.
/// Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn a_lambda_held_in_a_property_is_invoked() {
    assert_eq!(
        prog(
            r##"val top = { 7 }
val tg: (Int) -> Int = { it + 1 }
class C { val z = { "z" }; val w: () -> String = { "w" } }
object O { val a = { 5 }; val g: (Int) -> Int = { it * 2 }; val l by lazy { 9 } }
fun main() { println(top()); println(tg(1)); println(C().z()); println(C().w()); println(O.a()); println(O.g(4)); println(O.l) }
"##
        ),
        "7\n2\nz\nw\n5\n8\n9\n"
    );
}

/// A boxed `Float` equals only a boxed `Float`: `Float.equals` checks the
/// class before the value, so no `Double` of the same magnitude matches it —
/// whether the `Double` is erased, a collection element or a `when` arm. The
/// same-width comparisons through the same positions stay `true`, since every
/// erased `Float` operand is boxed. Measured on kotlinc 2.4.21 / JDK 25.0.5.
#[test]
fn a_boxed_float_never_equals_a_double() {
    assert_eq!(
        prog(
            r##"fun main() {
    val f: Any = 2.0f
    val d: Any = 2.0
    println(listOf(f == d, d == f, f.equals(d), d.equals(f)))
    println(listOf(listOf(2.0).contains(f), listOf<Any>(2.0f).indexOf(2.0), setOf<Any>(1f, 1.0).size))
    println(when (f) { 2.0 -> "double"; 2.0f -> "float"; else -> "none" })
    val x = 2.0f
    println(listOf(f == x, x == f, listOf(x).contains(2.0f), listOf(1f / 3f).contains(1f / 3f)))
    println(when (f) { x -> "same"; else -> "diff" })
}
"##
        ),
        "[false, false, false, false]\n[false, -1, 2]\nfloat\n[true, true, true, true]\nsame\n"
    );
}

/// `constructor(vararg v: T)` binds an array, so a call with any number of
/// trailing arguments — none, or a spread — selects it, and a primitive
/// argument never selects a primary whose parameter is a collection.
/// Measured on kotlinc 2.4.21 / JDK 25.0.5.
#[test]
fn a_secondary_constructor_takes_a_vararg() {
    assert_eq!(
        prog(
            r##"class P(val xs: List<Int>) { constructor(vararg v: Int) : this(v.toList()) }
class N(val tag: String, val xs: List<String>) {
    constructor(tag: String, vararg v: String) : this(tag, v.toList())
}
fun main() {
    println(listOf(P(1, 2, 3).xs, P().xs, P(7).xs, P(listOf(9)).xs))
    val arr = arrayOf("x", "y")
    println(listOf(N("t", "a").xs, N("u").xs, N("w", *arr).xs, N("v", listOf("z")).xs))
}
"##
        ),
        "[[1, 2, 3], [], [7], [9]]\n[[a], [], [x, y], [z]]\n"
    );
}

/// `withSign`, `nextUp`, `nextDown`, `nextTowards` and `ulp` step through the
/// lattice of the RECEIVER's width: `1f.nextUp()` is the next `Float`, not the
/// next `Double` narrowed back to `1.0`. `nextTowards` answers the direction
/// when the two are equal, so `0.0` towards `-0.0` is `-0.0`. Measured on
/// kotlinc 2.4.21 / JDK 25.0.5.
#[test]
fn ieee_neighbours_keep_the_receivers_width() {
    assert_eq!(
        prog(
            r##"import kotlin.math.*
fun main() {
    println(listOf(1.5.withSign(-1), (-0.0).withSign(1), 1.0.nextUp(), 0.0.nextDown(), 0.0.nextTowards(-0.0)))
    println(listOf(Double.MAX_VALUE.nextUp(), Double.MAX_VALUE.ulp, 0.0.ulp, 1.0.ulp))
    println(listOf(1f.nextUp(), 1f.nextDown(), 0f.nextUp(), 1f.ulp, 2f.withSign(-0f)))
    println(1f.nextUp() - 1f)
}
"##
        ),
        "[-1.5, 0.0, 1.0000000000000002, -4.9E-324, -0.0]\n\
         [Infinity, 1.99584030953472E292, 4.9E-324, 2.220446049250313E-16]\n\
         [1.0000001, 0.99999994, 1.4E-45, 1.1920929E-7, -2.0]\n\
         1.1920929E-7\n"
    );
    // They are `kotlin.math` extensions, unresolved without the import.
    let err = prog_err("fun main() { println(1.0.nextUp()) }");
    assert!(err.contains("unresolved reference"), "{err}");
}

/// A companion `var` is written through the companion singleton, both
/// qualified (`C.count = 5`, `C.Factory.made += 1`) and unqualified from the
/// owning class's own members and `init`. It was `unresolved reference`.
/// Measured on kotlinc 2.4.21 / JDK 25.0.5.
#[test]
fn a_companion_var_is_assignable() {
    assert_eq!(
        prog(
            r##"class C {
    companion object { var count = 0 }
    init { count++ }
    fun bump() { count += 10 }
}
class F { companion object Factory { var made = 0 } }
fun main() {
    C.count = 5
    C(); C().bump()
    C.count++
    println(C.count)
    F.made += 100
    F.Factory.made -= 1
    println(F.made)
}
"##
        ),
        "18\n99\n"
    );
}

/// `var Recv.name` with a getter and a setter: the write calls the setter,
/// compound and `++` writes read through the getter first, and a member
/// property of the same name wins over the extension for both the read and
/// the write. Measured on kotlinc 2.4.21 / JDK 25.0.5.
#[test]
fn an_extension_var_has_a_setter() {
    assert_eq!(
        prog(
            r##"class Box(var n: Int)
var Box.double: Int
    get() = n * 2
    set(value) { n = value / 2 }
var MutableList<Int>.head: Int get() = this[0]; set(v) { this[0] = v }
class Q(var v: Int)
var Q.v: Int
    get() = -1
    set(x) { println("ext $x") }
fun main() {
    val b = Box(3)
    b.double = 10
    b.double += 4
    println(listOf(b.n, b.double))
    val l = mutableListOf(1, 2)
    l.head = 9
    l.head++
    println(l)
    val q = Q(1)
    q.v = 5
    println(q.v)
}
"##
        ),
        "[7, 14]\n[10, 2]\n5\n"
    );
}

/// Declaration-site variance: a class member whose written type puts an
/// `out` type parameter in an `in` or invariant position, or an `in` one in an
/// `out` or invariant position, is refused with `kotlinc`'s message. `private`
/// members are exempt. Each message measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn declaration_site_variance_violations_are_refused() {
    for (src, msg) in [
        ("class Box<out T>(var v: T)", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'T (of class Box<out T>)'."),
        ("class Sink<in T> { fun get(): T? = null }", "type parameter 'T' is declared as 'in' but occurs in 'out' position in type 'T? (of class Sink<in T>)'."),
        ("class P<out T> { fun put(x: T) {} }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of class P<out T>)'."),
        ("class Q<in T>(val t: T)", "type parameter 'T' is declared as 'in' but occurs in 'out' position in type 'T (of class Q<in T>)'."),
        ("class R<out T> { fun f(g: (T) -> Unit) {} ; fun h(): List<T> = listOf(); fun k(x: MutableList<T>) {} }", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'MutableList<T (of class R<out T>)>'."),
        ("class S<in T> { fun f(g: (T) -> Unit) {} }", "type parameter 'T' is declared as 'in' but occurs in 'out' position in type '(T (of class S<in T>)) -> Unit'."),
        ("class A<out T>(private var v: T, x: T) { private fun f(y: T) {}; protected fun g(y: T) {} }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of class A<out T>)'."),
        ("class B<out T : Number, in U>(val u: U)", "type parameter 'U' is declared as 'in' but occurs in 'out' position in type 'U (of class B<out T : Number, in U>)'."),
        ("class C<out T> { fun <T> f(x: T) {} ; internal fun h(x: T) {} }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of class C<out T>)'."),
        ("interface I<out T> { fun f(x: T) }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of interface I<out T>)'."),
        ("sealed interface Si<in T> { val x: T }", "type parameter 'T' is declared as 'in' but occurs in 'out' position in type 'T (of interface Si<in T>)'."),
        ("class E<out T> { fun T.ext() {} }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of class E<out T>)'."),
        ("class E<out T> { val p: T get() = TODO(); var q: T get() = TODO(); set(v) {} }", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'T (of class E<out T>)'."),
        ("class F<in T> { private val p: T? = null; fun f(x: (T) -> T) {} }", "type parameter 'T' is declared as 'in' but occurs in 'out' position in type '(T (of class F<in T>)) -> T (of class F<in T>)'."),
        ("abstract class G<out T> { abstract var x: T }", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'T (of class G<out T>)'."),
        ("class H<out T> { fun f(): ArrayList<List<T>> = TODO() }", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'ArrayList<List<T (of class H<out T>)>>'."),
        ("class H<out T> { fun g(): Pair<ArrayList<T>, Int> = TODO() }", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in abbreviated type 'Pair<ArrayList<T (of class H<out T>)>, Int>'."),
        ("class X<out T> { fun r(): Comparable<T> = TODO() }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'Comparable<T (of class X<out T>)>'."),
        ("class X<out T> { fun r(): Array<in T> = TODO() }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'Array<in T (of class X<out T>)>'."),
        ("class X<out T> { fun p(x: Map<String, T>) {} }", "type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'Map<String, T (of class X<out T>)>'."),
        ("data class Dc<out T>(var x: T)", "type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'T (of class Dc<out T>)'."),
    ] {
        let err = prog_err(&format!("{src}\nfun main() {{}}"));
        assert_eq!(err, format!("kotlin: {msg}\n"), "{src}");
    }
}

/// The valid side of variance: covariant producers, contravariant consumers,
/// `@UnsafeVariance`, `private` members, a method's own type parameter
/// shadowing the class's, use-site projections read and written where Kotlin
/// allows it, and subtyping through declared variance. Output measured on
/// kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn valid_variance_programs_compile_and_run() {
    assert_eq!(
        prog(
            r##"class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
class Producer<out T>(private val items: List<T>) {
    fun first(): T = items[0]
    fun all(): List<T> = items
    fun each(f: (T) -> Unit) { items.forEach(f) }
    fun <R> map(f: (T) -> R): Producer<R> = Producer(items.map(f))
    fun contains(x: @UnsafeVariance T): Boolean = x in items
    private var cache: T? = null
}
class Consumer<in T> {
    fun take(x: T) { println("took $x") }
    fun takeAll(xs: List<T>) { xs.forEach { take(it) } }
    fun sink(f: () -> T) { take(f()) }
}
interface Source<out T> { fun next(): T }
fun copy(from: Array<out Any>, to: Array<Any>) { for (i in from.indices) to[i] = from[i] }
fun fill(dest: Array<in String>, v: String) { dest[0] = v }
fun main() {
    val p: Producer<Any> = Producer(listOf("a", "b"))
    println(p.first()); println(p.contains("b")); p.each { print("$it ") }; println()
    println(p.map { it.toString().length }.all())
    val c: Consumer<String> = Consumer<Any>()
    c.take("x"); c.takeAll(listOf("y")); c.sink { "s" }
    val b: Box<out Number> = Box<Int>(1)
    println(b.get()); println(b.v)
    val bi: Box<in Int> = Box<Number>(2.5)
    bi.put(3); val a: Any? = bi.get(); println(a)
    val star: Box<*> = Box("q"); println(star.get())
    val xs: MutableList<out Number> = mutableListOf(1, 2)
    println(xs[0]); println(xs.contains(1))
    val ys: MutableList<in Int> = mutableListOf<Number>(1.5)
    ys.add(3); println(ys)
    val l: List<Any> = listOf<String>("a")
    val m: Map<String, Any> = mapOf<String, Int>("k" to 1)
    val bo: Box<Any> = Box("inferred")
    val bb = Box<String>("s"); val b4: Box<out Any> = bb
    val arr = arrayOf<Any>(1, 2); copy(arrayOf(3, 4), arr); println(arr.toList())
    val sa = arrayOfNulls<String>(1); fill(sa, "f"); println(sa.toList())
    val src = object : Source<String> { override fun next() = "n" }; val s2: Source<Any> = src
    println(s2.next()); println(l); println(m); println(bo.get()); println(b4.get())
}
"##
        ),
        "a\ntrue\na b \n[1, 1]\ntook x\ntook y\ntook s\n1\n1\n3\nq\n1\ntrue\n[1.5, 3]\n[3, 4]\n[f]\nn\n[a]\n{k=1}\ninferred\ns\n"
    );
}

/// Use-site variance: an initializer whose generic type is certainly not a
/// subtype of the declaration, a call through an `out`/star projection to a
/// member taking the projected parameter, a read through an `in`/star
/// projection into anything but `Any?`, and a write of a projected `var`.
/// Each message measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn use_site_variance_violations_are_refused() {
    let class = "class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v; fun d(x: T, y: Int = 2): Int = 1; fun n(): T? = null }";
    for (body, msg) in [
        (r#"val b: Box<Any> = Box<String>("s")"#, "initializer type mismatch: expected 'Box<Any>', actual 'Box<String>'."),
        (r#"val xs: MutableList<Any> = mutableListOf<String>("a")"#, "initializer type mismatch: expected 'MutableList<Any>', actual 'MutableList<String>'."),
        (r#"val bb: Box<String> = Box("s"); val b2: Box<Any> = bb"#, "initializer type mismatch: expected 'Box<Any>', actual 'Box<String>'."),
        ("val b: Box<Int?> = Box<Int>(1)", "initializer type mismatch: expected 'Box<Int?>', actual 'Box<Int>'."),
        ("val b: List<String> = listOf<Int>(1)", "initializer type mismatch: expected 'List<String>', actual 'List<Int>'."),
        ("val b: Pair<Int, String> = Pair<Int, Int>(1, 2)", "initializer type mismatch: expected 'Pair<Int, String>', actual 'Pair<Int, Int>'."),
        ("val ys: MutableList<out Number> = mutableListOf(1, 2); ys.add(3)", "receiver type 'MutableList<out Number>' contains out projection which prohibits the use of 'fun add(element: E): Boolean'."),
        ("val ys: MutableList<out Number> = mutableListOf(1, 2); ys[0] = 5", "receiver type 'MutableList<out Number>' contains out projection which prohibits the use of 'fun set(index: Int, element: E): E'."),
        ("val ys: MutableList<out Number> = mutableListOf(1, 2); ys.add(0, 3)", "receiver type 'MutableList<out Number>' contains out projection which prohibits the use of 'fun add(index: Int, element: E): Unit'."),
        ("val st: MutableList<*> = mutableListOf(1); st.add(1)", "receiver type 'MutableList<*>' contains star projection which prohibits the use of 'fun add(element: E): Boolean'."),
        ("val s: ArrayList<out Int> = arrayListOf(1); s.add(0, 1)", "receiver type 'ArrayList<out Int>' contains out projection which prohibits the use of 'fun add(p0: Int, p1: E): Unit'."),
        ("val c: Array<out Int> = arrayOf(1); c[0] = 2", "receiver type 'Array<out Int>' contains out projection which prohibits the use of 'fun set(index: Int, value: T): Unit'."),
        (r#"val m: MutableMap<String, out Number> = mutableMapOf("a" to 1); m["b"] = 2"#, "receiver type 'MutableMap<String, out Number>' contains out projection which prohibits the use of 'fun <K, V> MutableMap<K, V>.set(key: K, value: V): Unit'."),
        (r#"val m: HashMap<String, out Int> = hashMapOf("a" to 1); m.put("x", 1)"#, "receiver type 'HashMap<String, out Int>' contains out projection which prohibits the use of 'fun put(p0: K, p1: V): V?'."),
        ("val b: Box<out Number> = Box(1); b.put(2)", "receiver type 'Box<out Number>' contains out projection which prohibits the use of 'fun put(x: T): Unit'."),
        ("val b: Box<*> = Box(1); b.d(2)", "receiver type 'Box<*>' contains star projection which prohibits the use of 'fun d(x: T, y: Int = ...): Int'."),
        ("val c: Box<in Int> = Box<Number>(1); val x: Int = c.get()", "initializer type mismatch: expected 'Int', actual 'CapturedType(in Int)'."),
        ("val c: Box<in Int> = Box<Number>(1); val x: Number = c.v", "initializer type mismatch: expected 'Number', actual 'CapturedType(in Int)'."),
        ("val c: Box<in Int> = Box<Number>(1); val x: Int = c.n()", "initializer type mismatch: expected 'Int', actual 'CapturedType(in Int)?'."),
        ("val zs: MutableList<in Int> = mutableListOf<Number>(1, 2.0); val n: Int = zs[0]", "initializer type mismatch: expected 'Int', actual 'CapturedType(in Int)'."),
        ("val s: MutableList<*> = mutableListOf(1); val n: Int = s[0]", "initializer type mismatch: expected 'Int', actual 'CapturedType(*)'."),
        ("val b: Box<out Number> = Box(1); b.v = 3", "assignment type mismatch: actual type is 'Int', but 'Nothing' was expected."),
        (r#"val b: Box<*> = Box(1); b.v = "s""#, "assignment type mismatch: actual type is 'String', but 'Nothing' was expected."),
    ] {
        let err = prog_err(&format!("{class}\nfun main() {{ {body} }}"));
        assert_eq!(err, format!("kotlin: {msg}\n"), "{body}");
    }
}

/// Valid `Pattern` syntax the engine reads differently: comments mode (whitespace
/// and `#` comments dropped everywhere, scoped by group, from `(?x)` or
/// `RegexOption.COMMENTS`), octal `\0n`/`\0nn`/`\0mnn` escapes, and `\cX`
/// control escapes — plus `Pattern`'s refusals of a bad octal or control
/// escape. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn regex_comments_mode_octal_and_control_escapes() {
    assert_eq!(
        prog(
            r##"fun t(p: String, s: String) = try { val r = Regex(p); "" + r.containsMatchIn(s) + " " + r.find(s)?.value?.map { it.code } + " " + r.matches(s) } catch (e: Exception) { e.javaClass.simpleName + ": " + e.message!!.replace("\n", "|") }
fun main() {
    println(t("\\08", "x"))
    println(t("x\\0", "x"))
    println(t("\\c", "x"))
    println(t("(?x)[a b]", " "))
    println(t("(?x)a\tb\nc", "abc"))
    println(t("(?x)a#c\rb", "ab"))
    println(t("(?x)\\Q a \\E", " a "))
    println(t("(?x:a b)c d", "abc d"))
    println(t("(?x)a(?-x) b", "a b"))
    println(t("(?x)a(?-x: b) c", "a bc"))
    println(t("(?ix)A b", "AB"))
    println(t("a b(?x) c d", "a bcd"))
    println(t("(?x)a{1, 2}", "aa"))
    println(t("(?x)a {2}", "aa"))
    println(t("(?x)a+ +", "aa"))
    println(t("(?x)\\c A", "\u0001 A"))
    println(t("[\\0101]", "A"))
    println(t("[\\cA]", "\u0001"))
    println(t("\\0101{2}", "AA"))
    println(t("(?x)( a ) \\1", "aa"))
    println(Regex("a b # c", RegexOption.COMMENTS).matches("ab"))
    println(Regex("a b # c", setOf(RegexOption.COMMENTS, RegexOption.IGNORE_CASE)).matches("AB"))
}
"##
        ),
        "PatternSyntaxException: Illegal octal escape sequence near index 2|\\08|  ^\nPatternSyntaxException: Illegal octal escape sequence near index 3|x\\0\nPatternSyntaxException: Illegal control escape sequence near index 1|\\c| ^\nfalse null false\ntrue [97, 98, 99] true\ntrue [97, 98] true\ntrue [32, 97, 32] true\ntrue [97, 98, 99, 32, 100] true\ntrue [97, 32, 98] true\ntrue [97, 32, 98, 99] true\ntrue [65, 66] true\ntrue [97, 32, 98, 99, 100] true\ntrue [97, 97] true\ntrue [97, 97] true\ntrue [97, 97] true\ntrue [1] false\ntrue [65] true\ntrue [1] true\ntrue [65, 65] true\ntrue [97, 97] true\ntrue\ntrue\n"
    );
}

/// `\cX` takes any operand (a `[` is not a class), and an octal escape reads
/// three digits only when the first is at most 3. Measured on kotlinc 2.4.21 /
/// JRE 21.0.13.0.2.
#[test]
fn regex_control_operand_and_octal_width() {
    assert_eq!(
        prog(
            r##"fun t(p: String, s: String) = try { val r = Regex(p); "" + r.containsMatchIn(s) + " " + r.find(s)?.value?.map { it.code } + " " + r.matches(s) } catch (e: Exception) { e.javaClass.simpleName + ": " + e.message!!.replace("\n", "|") }
fun main() {
    println(t("\\c[", "\u001b"))
    println(t("\\0377", "ÿ"))
    println(t("\\0400", " 0"))
    println(t("\\c@x", "\u0000x"))
}
"##
        ),
        "true [27] true\ntrue [255] true\ntrue [32, 48] true\ntrue [0, 120] true\n"
    );
}

/// Unsigned arrays print their SIGNED storage, an unsigned literal stored into
/// one takes the array's element width, the signed arrays convert with
/// `toUByteArray()` and its siblings (and back with `toByteArray()`), and a
/// parenthesized expression is invocable: `(O.a)()`. Measured on kotlinc
/// 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn unsigned_array_storage_and_parenthesized_callee() {
    assert_eq!(
        prog(
            r##"@file:OptIn(ExperimentalUnsignedTypes::class)
object O { val a = { 5 }; val f: (Int) -> Int = { it * 2 } }
class C { val g = { "g" } }
fun main() {
    val a = UByteArray(2); a[0] = 255u
    println(a[0] + 1u); println(a[0]); println(a[0]::class.simpleName); println(a)
    println(UByteArray(3).also { it[1] = 200u })
    println(byteArrayOf(-1, 2).toUByteArray())
    println(byteArrayOf(-1, 2).toUByteArray().toList())
    println(ubyteArrayOf(255u, 1u).toByteArray().toList())
    println(UShortArray(2).also { it[0] = 65535u })
    println(UIntArray(2).also { it[0] = 4000000000u })
    println(ULongArray(1).also { it[0] = 18446744073709551615u })
    println(shortArrayOf(-1).toUShortArray())
    println(intArrayOf(-1, 7).toUIntArray().toList())
    println(longArrayOf(-1).toULongArray())
    println((O.a)())
    println((O.f)(4))
    println((C().g)())
    val h = { x: Int -> x + 1 }
    println((h)(1))
    println(("ab")
        .length)
    println("a\nb\r\nc".lineSequence().toList())
}
"##
        ),
        "256\n255\nUByte\nUByteArray(storage=[-1, 0])\nUByteArray(storage=[0, -56, 0])\nUByteArray(storage=[-1, 2])\n[255, 2]\n[-1, 1]\nUShortArray(storage=[-1, 0])\nUIntArray(storage=[-294967296, 0])\nULongArray(storage=[-1])\nUShortArray(storage=[-1])\n[4294967295, 7]\nULongArray(storage=[-1])\n5\n8\ng\n2\n2\n[a, b, c]\n"
    );
}

/// A read of a non-`private` `lateinit` property before its first write
/// raises inside the property's GETTER, whose frame heads the trace: the
/// declaring class (an inherited one, a companion, an `object`, or the file
/// facade for a top-level property) and the declaration line. A `private` one
/// and a local have no getter. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn lateinit_reads_run_through_the_getter_frame() {
    assert_eq!(
        prog(
            r##"class L {
    private lateinit var p: String
    lateinit var s: String
    fun f() = p.length
    fun g() = this.s.length
}
lateinit var top: String
open class B { lateinit var q: String }
class D : B() { fun h() = q }
object Ob { lateinit var w: String }
class M { companion object { lateinit var c: String } }
lateinit var isOn: String
fun t(b: () -> Unit) = try { b() } catch (e: Exception) { println(e.stackTrace.take(3).joinToString(" | ")) }
fun main() {
    t { L().f() }
    t { L().g() }
    t { println(top) }
    t { D().h() }
    t { Ob.w }
    t { val x = L(); x.s }
    t { M.c }
    t { isOn }
    lateinit var loc: String
    t { loc }
    val l = L(); l.s = "x"; println(l.s)
    top = "y"; println(top)
}
"##
        ),
        "L.f(main.kt:4) | MainKt.main$lambda$0(main.kt:15) | MainKt.t(main.kt:13)\nL.getS(main.kt:3) | L.g(main.kt:5) | MainKt.main$lambda$1(main.kt:16)\nMainKt.getTop(main.kt:7) | MainKt.main$lambda$2(main.kt:17) | MainKt.t(main.kt:13)\nB.getQ(main.kt:8) | D.h(main.kt:9) | MainKt.main$lambda$3(main.kt:18)\nOb.getW(main.kt:10) | MainKt.main$lambda$4(main.kt:19) | MainKt.t(main.kt:13)\nL.getS(main.kt:3) | MainKt.main$lambda$5(main.kt:20) | MainKt.t(main.kt:13)\nM$Companion.getC(main.kt:11) | MainKt.main$lambda$6(main.kt:21) | MainKt.t(main.kt:13)\nMainKt.isOn(main.kt:12) | MainKt.main$lambda$7(main.kt:22) | MainKt.t(main.kt:13)\nMainKt.main$lambda$8(main.kt:24) | MainKt.t(main.kt:13) | MainKt.main(main.kt:24)\nx\ny\n"
    );
    let out = eval(
        "class L { lateinit var s: String }\nfun main() {\n    val l = L()\n    println(l.s)\n}\n",
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Exception in thread \"main\" kotlin.UninitializedPropertyAccessException: lateinit property s has not been initialized\n\tat L.getS(main.kt:1)\n\tat MainKt.main(main.kt:4)\n\tat MainKt.main(main.kt)\n"
    );
}

/// A fault raised by an op that carries no line of its own — a division —
/// is attributed to the STATEMENT it is in, as the JVM's line table does,
/// not to the statement before it: in a function, a `try` block and a
/// multi-line lambda, whether the statement is the last one or not.
/// Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn a_fault_takes_its_statements_line() {
    assert_eq!(
        prog(
            r##"fun t(b: () -> Unit) = try { b() } catch (e: Exception) { println(e.stackTrace.take(2).joinToString(" | ")) }
fun main() {
    val f = { x: Int ->
        println("in")
        10 / x
    }
    t { f(0) }
    val g = { x: Int ->
        println("g")
        val y = 10 / x
        y
    }
    t { g(0) }
    val h: (Int) -> Int = {
        println("h")
        listOf(1)[it]
    }
    t { h(5) }
    t {
        println("t")
        10 / 0
    }
}
"##
        ),
        "in\nMainKt.main$lambda$0(main.kt:5) | MainKt.main$lambda$1(main.kt:7)\ng\nMainKt.main$lambda$2(main.kt:10) | MainKt.main$lambda$3(main.kt:13)\nh\njava.base/java.util.Collections$SingletonList.get(Collections.java:5180) | MainKt.main$lambda$4(main.kt:16)\nt\nMainKt.main$lambda$6(main.kt:21) | MainKt.t(main.kt:1)\n"
    );
    assert_eq!(
        prog(
            r##"fun f(x: Int): Int {
    println("a")
    val y = 10 / x
    return y
}
fun g(x: Int): Int {
    println("b")
    return 10 /
        x
}
fun main() {
    try { f(0) } catch (e: Exception) { println(e.stackTrace[0]) }
    try { g(0) } catch (e: Exception) { println(e.stackTrace[0]) }
    try {
        println("c")
        10 / 0
    } catch (e: Exception) { println(e.stackTrace[0]) }
}
"##
        ),
        "a\nMainKt.f(main.kt:3)\nb\nMainKt.g(main.kt:8)\nc\nMainKt.main(main.kt:16)\n"
    );
}

/// `next()` past the end of a map's `keys`/`values`/`entries` iterator raises
/// inside the MAP's iterator class for that projection — `LinkedHashMap`,
/// `HashMap`, a singleton map, an empty map, a `TreeMap` — whose JDK frames head
/// the trace. Measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn map_view_iterators_raise_in_the_maps_iterator_class() {
    assert_eq!(
        prog(
            r##"fun t(name: String, b: () -> Unit) = try { b() } catch (e: Exception) { println(name + ": " + e + " @ " + e.stackTrace.take(4).joinToString(" | ")) }
fun main() {
    t("vals") { mutableMapOf<Int, Int>().values.iterator().next() }
    t("keys") { mutableMapOf<Int, Int>().keys.iterator().next() }
    t("ents") { mutableMapOf<Int, Int>().entries.iterator().next() }
    t("hvals") { hashMapOf<Int, Int>().values.iterator().next() }
    t("hkeys") { hashMapOf<Int, Int>().keys.iterator().next() }
    t("hents") { hashMapOf<Int, Int>().entries.iterator().next() }
    t("ivals") { mapOf(1 to 2).values.iterator().also { it.next() }.next() }
    t("ikeys") { mapOf(1 to 2).keys.iterator().also { it.next() }.next() }
    t("ients") { mapOf(1 to 2).entries.iterator().also { it.next() }.next() }
    t("lvals") { mapOf(1 to 2, 3 to 4).values.iterator().also { it.next(); it.next() }.next() }
    t("full") { mutableMapOf(1 to 2).values.iterator().also { it.next() }.next() }
}
"##
        ),
        "vals: java.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedValueIterator.next(LinkedHashMap.java:1052) | MainKt.main$lambda$0(main.kt:3) | MainKt.t(main.kt:1)\nkeys: java.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedKeyIterator.next(LinkedHashMap.java:1046) | MainKt.main$lambda$1(main.kt:4) | MainKt.t(main.kt:1)\nents: java.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedEntryIterator.next(LinkedHashMap.java:1058) | java.base/java.util.LinkedHashMap$LinkedEntryIterator.next(LinkedHashMap.java:1055) | MainKt.main$lambda$2(main.kt:5)\nhvals: java.util.NoSuchElementException @ java.base/java.util.HashMap$HashIterator.nextNode(HashMap.java:1607) | java.base/java.util.HashMap$ValueIterator.next(HashMap.java:1633) | MainKt.main$lambda$3(main.kt:6) | MainKt.t(main.kt:1)\nhkeys: java.util.NoSuchElementException @ java.base/java.util.HashMap$HashIterator.nextNode(HashMap.java:1607) | java.base/java.util.HashMap$KeyIterator.next(HashMap.java:1628) | MainKt.main$lambda$4(main.kt:7) | MainKt.t(main.kt:1)\nhents: java.util.NoSuchElementException @ java.base/java.util.HashMap$HashIterator.nextNode(HashMap.java:1607) | java.base/java.util.HashMap$EntryIterator.next(HashMap.java:1638) | java.base/java.util.HashMap$EntryIterator.next(HashMap.java:1636) | MainKt.main$lambda$5(main.kt:8)\nivals: java.util.NoSuchElementException @ java.base/java.util.Collections$1.next(Collections.java:5038) | MainKt.main$lambda$6(main.kt:9) | MainKt.t(main.kt:1) | MainKt.main(main.kt:9)\nikeys: java.util.NoSuchElementException @ java.base/java.util.Collections$1.next(Collections.java:5038) | MainKt.main$lambda$7(main.kt:10) | MainKt.t(main.kt:1) | MainKt.main(main.kt:10)\nients: java.util.NoSuchElementException @ java.base/java.util.Collections$1.next(Collections.java:5038) | MainKt.main$lambda$8(main.kt:11) | MainKt.t(main.kt:1) | MainKt.main(main.kt:11)\nlvals: java.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedValueIterator.next(LinkedHashMap.java:1052) | MainKt.main$lambda$9(main.kt:12) | MainKt.t(main.kt:1)\nfull: java.util.NoSuchElementException @ java.base/java.util.LinkedHashMap$LinkedHashIterator.nextNode(LinkedHashMap.java:1025) | java.base/java.util.LinkedHashMap$LinkedValueIterator.next(LinkedHashMap.java:1052) | MainKt.main$lambda$10(main.kt:13) | MainKt.t(main.kt:1)\n"
    );
    assert_eq!(
        prog(
            r##"fun t(name: String, b: () -> Unit) = try { b() } catch (e: Exception) { println(name + ": " + e + " @ " + e.stackTrace.take(4).joinToString(" | ")) }
fun main() {
    t("evals") { emptyMap<Int, Int>().values.iterator().next() }
    t("ekeys") { emptyMap<Int, Int>().keys.iterator().next() }
    t("eents") { mapOf<Int, Int>().entries.iterator().next() }
    t("svals") { sortedMapOf(1 to 2).values.iterator().also { it.next() }.next() }
    t("skeys") { sortedMapOf(1 to 2).keys.iterator().also { it.next() }.next() }
}
"##
        ),
        "evals: java.util.NoSuchElementException @ kotlin.collections.EmptyIterator.next(Collections.kt:21) | kotlin.collections.EmptyIterator.next(Collections.kt:16) | MainKt.main$lambda$0(main.kt:3) | MainKt.t(main.kt:1)\nekeys: java.util.NoSuchElementException @ kotlin.collections.EmptyIterator.next(Collections.kt:21) | kotlin.collections.EmptyIterator.next(Collections.kt:16) | MainKt.main$lambda$1(main.kt:4) | MainKt.t(main.kt:1)\neents: java.util.NoSuchElementException @ kotlin.collections.EmptyIterator.next(Collections.kt:21) | kotlin.collections.EmptyIterator.next(Collections.kt:16) | MainKt.main$lambda$2(main.kt:5) | MainKt.t(main.kt:1)\nsvals: java.util.NoSuchElementException @ java.base/java.util.TreeMap$PrivateEntryIterator.nextEntry(TreeMap.java:1519) | java.base/java.util.TreeMap$ValueIterator.next(TreeMap.java:1566) | MainKt.main$lambda$3(main.kt:6) | MainKt.t(main.kt:1)\nskeys: java.util.NoSuchElementException @ java.base/java.util.TreeMap$PrivateEntryIterator.nextEntry(TreeMap.java:1519) | java.base/java.util.TreeMap$KeyIterator.next(TreeMap.java:1575) | MainKt.main$lambda$4(main.kt:7) | MainKt.t(main.kt:1)\n"
    );
}

/// A fault in a constructor keeps the constructor's ONE `<init>` frame, on
/// the line of the initializer that raised — a property initializer, an
/// `init` block, a superclass's initializer, a recursive construction, a
/// secondary constructor — and only a throwable's OWN constructors are left
/// out. An `object`'s initializer is `<clinit>`. Measured on kotlinc 2.4.21 /
/// JRE 21.0.13.0.2.
#[test]
fn constructor_faults_keep_one_init_frame() {
    assert_eq!(
        prog(
            r##"class Z(val d: Int) {
    val q = 10 / d
}
class Y(val d: Int) {
    init { check(d > 0) }
}
class E2(m: String) : Exception(m)
class W(val d: Int) {
    init { if (d == 0) throw RuntimeException("x") else throw E2("y") }
}
open class Base(val d: Int) { val r = 5 / d }
class Sub(d: Int) : Base(d)
class N(val d: Int) {
    val c: Int = if (d > 0) N(d - 1).c else 1 / d
}
class S2(val d: Int) {
    constructor() : this(0) { println(1 / d) }
}
fun t(b: () -> Unit) = try { b() } catch (e: Exception) { println(e.stackTrace.take(4).joinToString(" | ")) }
fun main() {
    t { Z(0) }
    t { Y(0) }
    t { W(0) }
    t { W(1) }
    t { Sub(0) }
    t { N(2) }
    t { S2() }
    t { throw E2("z") }
}
"##
        ),
        "Z.<init>(main.kt:2) | MainKt.main$lambda$0(main.kt:21) | MainKt.t(main.kt:19) | MainKt.main(main.kt:21)\nY.<init>(main.kt:5) | MainKt.main$lambda$1(main.kt:22) | MainKt.t(main.kt:19) | MainKt.main(main.kt:22)\nW.<init>(main.kt:9) | MainKt.main$lambda$2(main.kt:23) | MainKt.t(main.kt:19) | MainKt.main(main.kt:23)\nW.<init>(main.kt:9) | MainKt.main$lambda$3(main.kt:24) | MainKt.t(main.kt:19) | MainKt.main(main.kt:24)\nBase.<init>(main.kt:11) | Sub.<init>(main.kt:12) | MainKt.main$lambda$4(main.kt:25) | MainKt.t(main.kt:19)\nN.<init>(main.kt:14) | N.<init>(main.kt:14) | N.<init>(main.kt:14) | MainKt.main$lambda$5(main.kt:26)\nS2.<init>(main.kt:17) | MainKt.main$lambda$6(main.kt:27) | MainKt.t(main.kt:19) | MainKt.main(main.kt:27)\nMainKt.main$lambda$7(main.kt:28) | MainKt.t(main.kt:19) | MainKt.main(main.kt:28) | MainKt.main(main.kt)\n"
    );
    let out = eval(
        r##"open class E2(m: String) : Exception(m) { val tag = 1 }
class E3(m: String) : E2(m) { init { println("e3") } }
class L {
    lateinit var s: String
    init { try { println(s) } catch (e: Exception) { println(e.stackTrace.take(2).joinToString(" | ")) } }
}
class Q(val d: Int) {
    val q = 10 / d
}
object Ob { val v = 1 / zero() }
fun zero() = 0
fun main() {
    try { throw E3("z") } catch (e: Exception) { println(e.stackTrace.take(2).joinToString(" | ")) }
    L()
    try { Ob.v } catch (e: Throwable) { println(e.toString() + " " + e.cause?.stackTrace?.take(2)?.joinToString(" | ")) }
    Q(0)
}
"##,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "e3\nMainKt.main(main.kt:13) | MainKt.main(main.kt)\nL.getS(main.kt:4) | L.<init>(main.kt:5)\njava.lang.ExceptionInInitializerError Ob.<clinit>(main.kt:10) | MainKt.main(main.kt:15)\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "Exception in thread \"main\" java.lang.ArithmeticException: / by zero\n\tat Q.<init>(main.kt:8)\n\tat MainKt.main(main.kt:16)\n\tat MainKt.main(main.kt)\n");
    assert_eq!(out.status.code(), Some(1));
}

/// The syntax check reads comments mode as `Pattern` does: a `#` comment and
/// whitespace are skipped everywhere — inside a class, between `(` and `?`,
/// between a quantifier and its `+`/`?` — so a `(` or `[` in a comment opens
/// nothing, `(?x)[#a]` is an unclosed class, and an inline-flag group is the
/// start of a sequence for a dangling quantifier. Measured on kotlinc 2.4.21 /
/// JRE 21.0.13.0.2.
#[test]
fn regex_syntax_check_in_comments_mode() {
    assert_eq!(
        prog(
            r##"fun t(p: String, s: String) = try { val r = Regex(p); "" + r.containsMatchIn(s) + " " + r.find(s)?.value + " " + r.matches(s) } catch (e: Exception) { e.javaClass.simpleName + ": " + e.message!!.replace("\n", "|") }
fun main() {
    println(t("(?x)[#a]", "#"))
    println(t("(?x)a # (", "a"))
    println(t("(?x)a # )", "a"))
    println(t("(?x)[a # ]\n]", "a"))
    println(t("(?x)a\\ b", "a b"))
    println(t("(?x)( a ) # [", "a"))
    println(t("(?x)a #c\n)", "a"))
    println(t("(?x:a #(\n)b", "ab"))
    println(t("(?x)a{2 }", "aa"))
    println(t("(?x) * a", "a"))
    println(t("(?x)[ ]", " "))
}
"##
        ),
        "PatternSyntaxException: Unclosed character class near index 7|(?x)[#a]|       ^\ntrue a true\ntrue a true\ntrue a true\ntrue a b true\ntrue a true\nPatternSyntaxException: Unmatched closing ')' near index 8|(?x)a #c|)|        ^\ntrue ab true\ntrue aa true\nPatternSyntaxException: Dangling meta character '*' near index 6|(?x) * a|      ^\nPatternSyntaxException: Unclosed character class near index 6|(?x)[ ]|      ^\n"
    );
    assert_eq!(
        prog(
            r##"fun t(p: String) = try { Regex(p); "ok" } catch (e: Exception) { e.message!!.replace("\n", "|") }
fun main() {
    for (p in listOf("(?x)*a", "(?x) *a", "(?x)  *a", "(?x)a|  *", "(?x)a| *", "(?x)(  ?", "(?x)[  ", "(?x)[a", "(?x)a{2,1}", "(?x)a{ 2}", "(?x) ( ", "(?x)a)", "(?x)a )", "(?x)  )", "(?x)\\k<q>", "(?x)a{", "(?x)[b-a]"))
        println(p + " => " + t(p))
}
"##
        ),
        "(?x)*a => Dangling meta character '*' near index 4|(?x)*a|    ^\n(?x) *a => Dangling meta character '*' near index 5|(?x) *a|     ^\n(?x)  *a => Dangling meta character '*' near index 6|(?x)  *a|      ^\n(?x)a|  * => Dangling meta character '*' near index 8|(?x)a|  *|        ^\n(?x)a| * => Dangling meta character '*' near index 7|(?x)a| *|       ^\n(?x)(  ? => Unknown inline modifier near index 8|(?x)(  ?\n(?x)[   => Unclosed character class near index 6|(?x)[  |      ^\n(?x)[a => Unclosed character class near index 5|(?x)[a|     ^\n(?x)a{2,1} => Illegal repetition range near index 9|(?x)a{2,1}|         ^\n(?x)a{ 2} => Illegal repetition near index 6|(?x)a{ 2}|      ^\n(?x) (  => Unclosed group near index 7|(?x) ( \n(?x)a) => Unmatched closing ')' near index 4|(?x)a)|    ^\n(?x)a ) => Unmatched closing ')' near index 5|(?x)a )|     ^\n(?x)  ) => Unmatched closing ')' near index 5|(?x)  )|     ^\n(?x)\\k<q> => named capturing group <q> does not exist near index 8|(?x)\\k<q>|        ^\n(?x)a{ => Illegal repetition near index 6|(?x)a{\n(?x)[b-a] => Illegal character range near index 7|(?x)[b-a]|       ^\n"
    );
    assert_eq!(
        prog(
            r##"fun t(p: String) = try { Regex(p); "ok" } catch (e: Exception) { e.message!!.replace("\n", "|") }
fun main() {
    for (p in listOf("(?i)*a", "a(?i)*", "(?", "a(?", "(?i)+", "^*", "a$*", "(?i:a)*", "(?x)[a\n#c\n]"))
        println(p.replace("\n", "\\n") + " => " + t(p))
}
"##
        ),
        "(?i)*a => Dangling meta character '*' near index 4|(?i)*a|    ^\na(?i)* => Dangling meta character '*' near index 5|a(?i)*|     ^\n(? => Unknown inline modifier near index 2|(?\na(? => Unknown inline modifier near index 3|a(?\n(?i)+ => Dangling meta character '+' near index 4|(?i)+|    ^\n^* => ok\na$* => ok\n(?i:a)* => ok\n(?x)[a\\n#c\\n] => ok\n"
    );
    assert_eq!(
        prog(
            r##"fun t(p: String, s: String) = try { val r = Regex(p); "" + r.containsMatchIn(s) + " " + r.find(s)?.value + " " + r.matches(s) } catch (e: Exception) { e.javaClass.simpleName + ": " + e.message!!.replace("\n", "|") }
fun main() {
    println(t("(?x)[ ]a]", "]"))
    println(t("(?x)[ ^ a]", "b"))
    println(t("(?x)[a\n#c\n]", "c"))
    println(t("(?x)[a b c]+", "cab"))
    println(t("(?x)[a #]", "a"))
}
"##
        ),
        "true ] true\nfalse null false\nfalse null false\ntrue cab true\nPatternSyntaxException: Unclosed character class near index 8|(?x)[a #]|        ^\n"
    );
}

/// Variance reaches further: an inner class's members (a `private` one's too)
/// against the outer type parameters, supertypes as `out` positions, a
/// parameter's written type, a call to a top-level function with a concrete
/// written result, and `also`/`apply` on a local. Messages and the valid
/// program's output measured on kotlinc 2.4.21 / JRE 21.0.13.0.2.
#[test]
fn variance_reaches_inner_classes_supertypes_parameters_and_calls() {
    for (src, msg) in [
        (
            r##"class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
fun f(b: Box<String>) { val x: Box<Any> = b }
fun main() {}
"##,
            "kotlin: initializer type mismatch: expected 'Box<Any>', actual 'Box<String>'.",
        ),
        (
            r##"class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
fun mk(): Box<String> = Box("s")
fun main() { val c: Box<Any> = mk() }
"##,
            "kotlin: initializer type mismatch: expected 'Box<Any>', actual 'Box<String>'.",
        ),
        (
            r##"class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
fun main() { val b: Box<Number> = Box(1); val c: Box<Int> = b.also { } }
"##,
            "kotlin: initializer type mismatch: expected 'Box<Int>', actual 'Box<Number>'.",
        ),
        (
            r##"class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
fun g(b: Box<out Number>) { b.put(1) }
fun main() {}
"##,
            "kotlin: receiver type 'Box<out Number>' contains out projection which prohibits the use of 'fun put(x: T): Unit'.",
        ),
        (
            r##"class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
fun g(c: Box<in Int>) { val x: Int = c.get() }
fun main() {}
"##,
            "kotlin: initializer type mismatch: expected 'Int', actual 'CapturedType(in Int)'.",
        ),
        (
            r##"class J<out T> { inner class K { fun f(x: T) {} } }
fun main() {}
"##,
            "kotlin: type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of class J<out T>)'.",
        ),
        (
            r##"class D<out T> : Comparable<T> { override fun compareTo(other: T): Int = 0 }
fun main() {}
"##,
            "kotlin: type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'Comparable<T (of class D<out T>)>'.",
        ),
        (
            r##"interface Src<out T> { fun next(): T }
abstract class M<in T> : Src<T>
fun main() {}
"##,
            "kotlin: type parameter 'T' is declared as 'in' but occurs in 'out' position in type 'Src<T (of class M<in T>)>'.",
        ),
        (
            r##"class J<out T> { private inner class PK { fun f(x: T) {} } }
fun main() {}
"##,
            "kotlin: type parameter 'T' is declared as 'out' but occurs in 'in' position in type 'T (of class J<out T>)'.",
        ),
    ] {
        let err = prog_err(src);
        assert_eq!(err.lines().next(), Some(msg), "{src}");
    }
    assert_eq!(
        prog(
            r##"interface Src<out T> { fun next(): T }
interface Snk<in T> { fun put(x: T) }
class Box<T>(var v: T) { fun put(x: T) { v = x }; fun get(): T = v }
class J<out T>(val t: T) {
    inner class K { fun get(): T = t; private fun p(x: T) {} }
}
class Rep<out T>(private val x: T) : Src<T> { override fun next(): T = x }
class Eat<in T> : Snk<T>, Comparable<T> { override fun put(x: T) { println("ate $x") }; override fun compareTo(other: T) = 0 }
abstract class Wrap<out T> : Iterable<T>
fun use(b: Box<String>): Box<String> { val c: Box<String> = b; val d: Box<out Any> = b; return c }
fun mk(): Box<String> = Box("s")
fun <T> gen(x: T): Box<T> = Box(x)
fun main() {
    val j = J("a"); println(j.K().get())
    val s: Src<Any> = Rep("r"); println(s.next())
    val e: Snk<String> = Eat<Any>(); e.put("x")
    println(use(Box("u")).get())
    val m: Box<String> = mk(); val m2: Box<out CharSequence> = mk(); println(m.get() + m2.get())
    val g: Box<Any> = gen("q"); println(g.get())
    val n: Box<Number> = Box(1); val n2: Box<Number> = n.also { }; println(n2.get())
}
"##
        ),
        "a\nr\nate x\nu\nss\nq\n1\n"
    );
}

#[test]
fn require_and_check_answer_the_unit_object() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13: a passing `require`/`check`
    // returns `Unit` itself, so it compares equal (and identical) to `Unit`.
    // Before, it rendered as `kotlin.Unit` but compared unequal.
    assert_eq!(
        stdout(
            "fun main() {\n\
             \x20   val u = check(true)\n\
             \x20   println(u == Unit)\n\
             \x20   println(u === Unit)\n\
             \x20   println(require(1 > 0) == Unit)\n\
             \x20   println(check(true))\n\
             }"
        ),
        "true\ntrue\ntrue\nkotlin.Unit\n"
    );
}

#[test]
fn an_enum_initializes_on_first_use_with_its_companion() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13. The constants are built by
    // the enum's class initializer, on first access — after `start`, not
    // before `main` — and the companion's own properties and `init` blocks
    // run in that same initializer, after the constants. Before, the
    // constants were built at program start and the companion `init` block
    // never ran. A throwing initializer fails the first access with
    // `ExceptionInInitializerError` and every later one with
    // `NoClassDefFoundError`.
    assert_eq!(
        stdout(
            "enum class F(val v: Int) {\n\
             \x20   A(1), B(2);\n\
             \x20   init { println(\"F init $name\") }\n\
             \x20   companion object {\n\
             \x20       val y = 5.also { println(\"comp y\") }\n\
             \x20       init { println(\"F comp ${A.v}\") }\n\
             \x20   }\n\
             }\n\
             enum class Bad(val x: Int) { ONE(1); init { if (x == 1) throw IllegalStateException(\"boom\") } }\n\
             fun main() {\n\
             \x20   println(\"start\")\n\
             \x20   println(F.A.v)\n\
             \x20   println(F.B.v)\n\
             \x20   println(F.y)\n\
             \x20   try { println(Bad.ONE) } catch (e: Throwable) { println(e::class.simpleName + \" \" + e.cause) }\n\
             \x20   try { println(Bad.ONE) } catch (e: Throwable) { println(e::class.simpleName) }\n\
             }"
        ),
        "start\nF init A\nF init B\ncomp y\nF comp 1\n1\n2\n5\n\
         ExceptionInInitializerError java.lang.IllegalStateException: boom\nNoClassDefFoundError\n"
    );
}

#[test]
fn a_constructor_reference_selects_by_the_expected_function_type() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13. With several constructors,
    // `::P` names the one the expected function type selects — from a `val`
    // annotation, a function-typed parameter, or a collection member's lambda.
    assert_eq!(
        stdout(
            "class P(val a: Int) {\n\
             \x20   var tag = \"p\"\n\
             \x20   constructor(s: String) : this(s.length) { tag = \"s\" }\n\
             \x20   override fun toString() = \"P($a,$tag)\"\n\
             }\n\
             fun mk(f: (String) -> P) = f(\"xyz\")\n\
             fun main() {\n\
             \x20   val f: (String) -> P = ::P\n\
             \x20   println(f(\"abcd\"))\n\
             \x20   val g: (Int) -> P = ::P\n\
             \x20   println(g(7))\n\
             \x20   println(listOf(1, 2).map(::P))\n\
             \x20   println(mk(::P))\n\
             }"
        ),
        "P(4,s)\nP(7,p)\n[P(1,p), P(2,p)]\nP(3,s)\n"
    );
    // kotlinc: `overload resolution ambiguity between candidates` — with no
    // expected type there is nothing to choose by. Before, the primary was
    // taken silently.
    let out = eval(
        "class P(val a: Int) { constructor(s: String) : this(s.length) }\n\
         fun main() { val f = ::P; println(f) }",
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("overload resolution ambiguity"),
        "stderr was: {err}"
    );
}

#[test]
fn a_read_only_lists_reversed_view_is_not_a_mutable_list() {
    // Measured on kotlinc 2.4.21 / JRE 21.0.13: `asReversed()` of a read-only
    // list is `ReversedListReadOnly`, which is not a `MutableList` — the cast
    // throws and the type test is false. A `MutableList`'s view still is one.
    // Before, the cast succeeded and the write went through.
    assert_eq!(
        stdout(
            "fun main() {\n\
             \x20   val r = listOf(1, 2).asReversed()\n\
             \x20   println(r is MutableList<*>)\n\
             \x20   println((r as? MutableList<Int>) == null)\n\
             \x20   try { (r as MutableList<Int>)[0] = 5 } catch (e: ClassCastException) { println(e.message) }\n\
             \x20   val m = mutableListOf(1, 2).asReversed() as MutableList<Int>; m[0] = 9; println(m)\n\
             \x20   println(emptyList<Int>().asReversed() is MutableList<*>)\n\
             }"
        ),
        "false\ntrue\n\
         kotlin.collections.ReversedListReadOnly cannot be cast to kotlin.collections.MutableList\n\
         [9, 1]\nfalse\n"
    );
}
