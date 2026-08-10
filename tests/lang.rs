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
fn a_reified_type_test_is_rejected_rather_than_answered() {
    // The coarse type system carries no type arguments, so `x is T` inside a
    // generic function has no answer it could give — and answering `false` (the
    // shape a name-based lookup falls into) would be silently wrong. It is a
    // compile error instead.
    let err = prog_err(
        "inline fun <reified T> isA(x: Any): Boolean = x is T\nfun main() { println(isA<Int>(5)) }",
    );
    assert!(
        err.contains("type parameter `T`") && err.contains("reified"),
        "unexpected diagnostic: {err}"
    );
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
fn property_delegate_without_a_resolvable_class_is_rejected() {
    // `by Delegates.observable(…)` names no class whose `getValue` could be
    // called. Left unchecked the property becomes a plain stored field and
    // printing it shows the DELEGATE instead of the value — a silent wrong
    // answer, which is exactly what this rejection prevents.
    let out = eval(
        "import kotlin.properties.Delegates\n\
         class O { var o: Int by Delegates.observable(1) { p, a, b -> } }\n\
         fun main() { println(O().o) }",
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("getValue"), "stderr was: {err}");
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

    // A settable computed property has no lowering here, and dropping the
    // writes silently would be worse than refusing the program.
    let out = eval("class C { var x: Int get() = 1\nset(v) {} }\nfun main() { println(1) }");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`set` accessor"), "stderr was: {err}");
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
    // `insert` says "offset" where the rest say "index", and `delete` CLAMPS
    // its end instead of throwing.
    let src = r#"
fun main() {
    try { StringBuilder("abc").deleteCharAt(9) } catch (e: Exception) { println(e) }
    try { StringBuilder("abc").insert(9, "x") } catch (e: Exception) { println(e) }
    try { println(StringBuilder("abc")[9]) } catch (e: Exception) { println(e) }
    println(StringBuilder("abc").delete(1, 99))
}"#;
    assert_eq!(
        stdout(src),
        "java.lang.StringIndexOutOfBoundsException: index 9, length 3\n\
         java.lang.StringIndexOutOfBoundsException: offset 9, length 3\n\
         java.lang.StringIndexOutOfBoundsException: index 9, length 3\n\
         a\n"
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
