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
