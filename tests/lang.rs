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
    let out = eval("val z = 0; println(10 / z)");
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("ArithmeticException"), "stderr was: {err}");
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
    // Non-exhaustive `when` with no matching arm and no `else` yields null.
    assert_eq!(stdout(r#"println(when (9) { 1 -> "a" })"#), "null\n");
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
    assert_eq!(stdout("println(('a'..'e').reversed())"), "e downTo a step 1\n");
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
    // `!!` on null raises an uncaught NullPointerException.
    let out = eval("val s: String? = null; println(s!!)");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("NullPointerException"),
        "stderr was: {}",
        String::from_utf8_lossy(&out.stderr)
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
        assert!(err.contains(want), "expected {want:?} for {src:?}, got {err}");
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
    let err = prog_err("fun main() { val xs = listOf(1, 2); println(xs[5]) }");
    assert!(err.contains("IndexOutOfBounds"), "stderr was: {err}");
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
    let err = prog_err("fun main() { val d = { a: Int, b: Int -> a / b }; println(d(10, 0)) }");
    assert!(err.contains("ArithmeticException"), "stderr was: {err}");
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
    let out = eval("fun main() {\n    println(\"before\")\n    throw IllegalStateException(\"dead\")\n}");
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
    assert_eq!(
        prog(src),
        "close 3\n6\nclose -1\n-1\ninner\nouter\ndeep\n"
    );
}

#[test]
fn a_break_out_of_a_try_with_a_finally_is_rejected() {
    // Unlike `return`, a `break` has no path that could run the finalizer here;
    // refusing the program beats silently skipping a cleanup block.
    let err = prog_err(
        "fun main() { for (i in 1..3) { try { break } finally { println(\"fin\") } } }",
    );
    assert!(
        err.contains("out of a `try` with a `finally` is not supported"),
        "stderr was: {err}"
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
    assert_eq!(
        prog(src),
        "java.lang.RuntimeException: m\nm\nnull\nrte\n"
    );
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
    assert_eq!(prog(src), "0, 2, 4, 6\n12\n0.0, 1.5, 3.0\n[0, 1, 4]\n0, 0\n");
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
    assert!(prog_err("abstract class S(val n: Int)\nfun main() { println(S(1)) }")
        .contains("cannot construct abstract class S"));
    assert!(prog_err("sealed class S\nfun main() { println(S()) }")
        .contains("cannot construct abstract class S"));
    assert!(
        prog_err("interface I { fun f(): Int }\nfun main() { println(I()) }")
            .contains("cannot construct interface I")
    );
    assert!(
        prog_err("class D : Missing()\nfun main() { println(1) }")
            .contains("unresolved supertype Missing")
    );
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
