//! Differential parity fuzzer: the real Kotlin toolchain vs this frontend.
//!
//! Generates grammar-driven, deterministic-output Kotlin programs, runs each
//! through a real `kotlinc` + `kotlin` pair and through our `kotlin`, and reports
//! every case whose stdout OR success/failure diverges. Each program is produced
//! from a per-index seed so any divergence replays exactly:
//! `parity-fuzz --seed <N> --once`.
//!
//! The oracle is two steps — `kotlinc T.kt -d <out>` then `kotlin -classpath
//! <out> TKt` — because the `kotlin` launcher cannot run a `.kt` carrying
//! `fun main` directly. That compile dominates the runtime, so every program
//! packs many independent probe statements (`--probes`, default 40) into one
//! `main`; a single compile therefore exercises dozens of probes. On divergence,
//! [`minimize`] bisects the probe list down to the single offending probe.
//!
//! The generator is biased toward where a from-scratch Kotlin frontend goes
//! wrong: `Int`-vs-`Double` division dispatch (`7/2==3`, `7/2.0==3.5`), IEEE
//! division by zero, `Double.toString` notation (the decimal/scientific
//! threshold), string templates, `String` member dispatch, and — since a VM with
//! no unwind opcode has to fake one — exception control flow: which handler
//! claims a throw, what a `finally` runs, what a partially-evaluated statement
//! is allowed to print, and what a `try` evaluates to.
//!
//! Scope + determinism invariants (mirroring the javars/scalars harnesses):
//!   * Only constructs kotlinrs actually implements are emitted — an unsupported
//!     construct would be a known gap, not a parity signal.
//!   * No nondeterministic output (no `Random`, no time, no identity hashes, no
//!     unordered collections). Every probe's output is a pure function of source.
//!     This is why an array is never printed directly: a JVM array inherits
//!     `Object.toString`, so `println(arrayOf(1))` emits an identity hash. Array
//!     probes read `size`/elements/`sum()`/`joinToString()` instead.
//!   * Integer operands stay well inside range so 32-bit overflow — a documented
//!     gap — is never the thing under test, and integer divisors are never zero
//!     (Kotlin throws there; that is a fault-path test, not a value test).
//!     Likewise ranges are never empty where a probe calls `max()`/`min()`,
//!     which throw on an empty sequence.
//!   * Every exception a probe raises is caught by that same probe. The probes
//!     share one `main`, so an escaping throw would truncate the whole program
//!     and test the harness instead of the frontend.
//!
//! Subprocess-only: this binary never links the kotlinrs library.
//!
//! Build:  cargo build --bin parity-fuzz
//! Run:    ./target/debug/parity-fuzz --iters 20
//!         ./target/debug/parity-fuzz --seed 12345 --once

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn pick<'a, T>(rng: &mut Rng, xs: &'a [T]) -> &'a T {
    &xs[rng.below(xs.len())]
}

const INTS: &[&str] = &["0", "1", "2", "3", "7", "10", "42", "100", "-1", "-7", "-42"];
const DIVS: &[&str] = &["1", "2", "3", "4", "5", "7", "-2", "-3"];
const DBLS: &[&str] = &[
    "0.0", "1.0", "0.5", "2.5", "3.14", "-1.5", "100.0", "1e3", "1e-3", "0.1", "1234567.0",
    "1.0e7", "1.0e-7", "123456789.0", "9.999e-4",
];
const ZDIVS: &[&str] = &["0.0", "-0.0"];
const STRS: &[&str] = &["\"\"", "\"a\"", "\"abc\"", "\"Hello\"", "\" x \"", "\"AbC\""];
const BOOLS: &[&str] = &["true", "false"];
const AOPS: &[&str] = &["+", "-", "*"];
const CMPOPS: &[&str] = &["==", "!=", "<", ">", "<=", ">="];
const LOGOPS: &[&str] = &["&&", "||"];
/// Range endpoints — small enough that a materialized range stays short, and
/// spanning both orientations so ascending, descending, and empty ranges all
/// occur.
const RINTS: &[&str] = &["0", "1", "2", "3", "5", "8", "-2", "-5"];
const STEPS: &[&str] = &["1", "2", "3"];

fn p(body: String) -> String {
    format!("println({body})")
}

fn g_arith(r: &mut Rng) -> String {
    p(format!(
        "({} {} {}) {} {}",
        pick(r, INTS),
        pick(r, AOPS),
        pick(r, INTS),
        pick(r, AOPS),
        pick(r, INTS)
    ))
}

/// `Int` division/modulo truncate toward zero; divisors are never 0.
fn g_intdiv(r: &mut Rng) -> String {
    let op = if r.below(2) == 0 { "/" } else { "%" };
    p(format!("{} {op} {}", pick(r, INTS), pick(r, DIVS)))
}

fn g_doublearith(r: &mut Rng) -> String {
    p(format!(
        "{} {} {}",
        pick(r, DBLS),
        pick(r, &["+", "-", "*", "/"]),
        pick(r, DBLS)
    ))
}

/// Mixed operands — the promotion that decides `/` dispatch.
fn g_mixeddiv(r: &mut Rng) -> String {
    if r.below(2) == 0 {
        p(format!("{} / {}", pick(r, INTS), pick(r, DBLS)))
    } else {
        p(format!("{} / {}", pick(r, DBLS), pick(r, INTS)))
    }
}

/// IEEE division by zero: signed infinities and NaN, never a fault.
fn g_divzero(r: &mut Rng) -> String {
    p(format!("{} / {}", pick(r, DBLS), pick(r, ZDIVS)))
}

/// `Double.toString` notation — the decimal/scientific threshold.
fn g_doublefmt(r: &mut Rng) -> String {
    p((*pick(r, DBLS)).to_string())
}

fn g_concat(r: &mut Rng) -> String {
    let s = pick(r, STRS);
    match r.below(4) {
        0 => p(format!("{s} + {}", pick(r, INTS))),
        1 => p(format!("{s} + {}", pick(r, DBLS))),
        2 => p(format!("{s} + {}", pick(r, BOOLS))),
        _ => p(format!("{s} + {} + {}", pick(r, INTS), pick(r, STRS))),
    }
}

/// String templates — `$name` and `${expr}`. A declared name carries the probe
/// index so probes stay independent when packed into one `main`.
fn g_template(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, INTS);
    let b = pick(r, INTS);
    match r.below(3) {
        0 => format!("val t{idx} = {a}; println(\"v=$t{idx}\")"),
        1 => p(format!("\"sum=${{{a} + {b}}}\"")),
        _ => format!("val t{idx} = {}; println(\"s=$t{idx}!\")", pick(r, STRS)),
    }
}

fn g_compare(r: &mut Rng) -> String {
    let op = pick(r, CMPOPS);
    if r.below(2) == 0 {
        p(format!("{} {op} {}", pick(r, INTS), pick(r, INTS)))
    } else {
        p(format!("{} {op} {}", pick(r, DBLS), pick(r, DBLS)))
    }
}

fn g_bool(r: &mut Rng) -> String {
    p(format!(
        "({} < {}) {} ({} > {})",
        pick(r, INTS),
        pick(r, INTS),
        pick(r, LOGOPS),
        pick(r, INTS),
        pick(r, INTS)
    ))
}

/// `if` as an expression.
fn g_ifexpr(r: &mut Rng) -> String {
    p(format!(
        "if ({} < {}) {} else {}",
        pick(r, INTS),
        pick(r, INTS),
        pick(r, INTS),
        pick(r, INTS)
    ))
}

/// `String` members kotlinrs dispatches (ASCII receivers only).
fn g_strmember(r: &mut Rng) -> String {
    let s = pick(r, STRS);
    match r.below(9) {
        0 => p(format!("{s}.length")),
        1 => p(format!("{s}.uppercase()")),
        2 => p(format!("{s}.lowercase()")),
        3 => p(format!("{s}.trim()")),
        4 => p(format!("{s}.isEmpty()")),
        5 => p(format!("{s}.contains(\"b\")")),
        6 => p(format!("{s}.startsWith(\"a\")")),
        7 => p(format!("{s}.indexOf(\"b\")")),
        _ => p(format!("{s}.replace(\"b\", \"-\")")),
    }
}

/// A counted loop with an accumulator, exercising the loop lowering.
///
fn g_loop(r: &mut Rng, idx: usize) -> String {
    let n = 2 + r.below(5);
    let step = pick(r, &["1", "2", "3"]);
    format!(
        "var a{idx} = 0; var i{idx} = 0; while (i{idx} < {n}) {{ a{idx} += i{idx} * {step}; i{idx}++ }}; println(a{idx})"
    )
}

/// `listOf` and a member read; list printing is order-deterministic.
fn g_list(r: &mut Rng) -> String {
    let a = pick(r, INTS);
    let b = pick(r, INTS);
    if r.below(2) == 0 {
        p(format!("listOf({a}, {b})"))
    } else {
        p(format!("listOf({a}, {b}).size"))
    }
}

/// Ranges: the `toString` forms (an `IntRange` prints `a..b`, an
/// `IntProgression` prints `a..b step n` / `a downTo b step n`), the aggregate
/// members, membership, and range-driven `for` loops. Endpoints are small so a
/// materialized range stays short.
fn g_range(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, RINTS);
    let b = pick(r, RINTS);
    let s = pick(r, STEPS);
    match r.below(14) {
        0 => p(format!("{a}..{b}")),
        1 => p(format!("{a} until {b}")),
        2 => p(format!("{a} downTo {b}")),
        3 => p(format!("({a}..{b} step {s})")),
        4 => p(format!("({a} downTo {b} step {s})")),
        5 => p(format!("({a}..{b}).sum()")),
        6 => p(format!("({a}..{b}).count()")),
        7 => p(format!("({a}..{b}).toList()")),
        8 => p(format!("({a}..{b}).joinToString()")),
        9 => p(format!("{} in {a}..{b}", pick(r, RINTS))),
        10 => p(format!("{} !in {a}..{b} step {s}", pick(r, RINTS))),
        11 => p(format!("({a}..{b}).map {{ it * 2 }}")),
        12 => format!("var g{idx} = 0; for (i in {a}..{b}) g{idx} += i; println(g{idx})"),
        _ => format!(
            "var g{idx} = 0; for (i in {a} downTo {b} step {s}) g{idx} += i; println(g{idx})"
        ),
    }
}

/// Arrays. Never printed directly (see the identity-hash note above) — every
/// probe reads a deterministic projection.
fn g_array(r: &mut Rng, idx: usize) -> String {
    let (a, b, c) = (pick(r, INTS), pick(r, INTS), pick(r, INTS));
    let decl = format!("val z{idx} = arrayOf({a}, {b}, {c})");
    match r.below(10) {
        0 => format!("{decl}; println(z{idx}.size)"),
        1 => format!("{decl}; println(z{idx}[{}])", r.below(3)),
        2 => format!("{decl}; println(z{idx}.sum())"),
        3 => format!("{decl}; println(z{idx}.joinToString())"),
        4 => format!("{decl}; z{idx}[0] = {}; println(z{idx}[0])", pick(r, INTS)),
        5 => format!("{decl}; println(z{idx}.toList())"),
        6 => format!("{decl}; println({} in z{idx})", pick(r, INTS)),
        7 => format!(
            "val z{idx} = IntArray({}); println(z{idx}.size + z{idx}.sum())",
            1 + r.below(4)
        ),
        8 => format!(
            "val z{idx} = intArrayOf({a}, {b}); var q{idx} = 0; for (x in z{idx}) q{idx} += x; println(q{idx})"
        ),
        _ => format!("{decl}; println(z{idx}.indexOf({}))", pick(r, INTS)),
    }
}

/// `kotlin.math` (via the harness's `import kotlin.math.*`) and the
/// `java.lang.Math` statics. `round` and `Math.round` differ in Kotlin — one is
/// half-to-even returning `Double`, the other half-up returning `Long` — so both
/// are probed.
fn g_math(r: &mut Rng) -> String {
    let i = pick(r, INTS);
    let d = pick(r, DBLS);
    match r.below(12) {
        0 => p(format!("abs({i})")),
        1 => p(format!("abs({d})")),
        2 => p(format!("max({i}, {})", pick(r, INTS))),
        3 => p(format!("min({d}, {})", pick(r, DBLS))),
        4 => p(format!("sqrt({d})")),
        5 => p(format!("floor({d})")),
        6 => p(format!("ceil({d})")),
        7 => p(format!("round({d})")),
        8 => p(format!("Math.abs({i})")),
        9 => p(format!("Math.round({d})")),
        10 => p(format!("maxOf({i}, {})", pick(r, INTS))),
        _ => p(format!("abs({i}) / {}", pick(r, DIVS))),
    }
}

/// `++`/`--` in expression position (the value is the pre-update one for the
/// postfix forms, the post-update one for the prefix forms) as well as in
/// statement position.
fn g_incdec(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, INTS);
    match r.below(6) {
        0 => format!("var n{idx} = {a}; println(n{idx}++); println(n{idx})"),
        1 => format!("var n{idx} = {a}; println(n{idx}--); println(n{idx})"),
        2 => format!("var n{idx} = {a}; println(++n{idx})"),
        3 => format!("var n{idx} = {a}; println(--n{idx})"),
        4 => format!("var n{idx} = {a}; println(n{idx}++ + n{idx}++); println(n{idx})"),
        _ => format!(
            "val e{idx} = intArrayOf({a}, {}); println(e{idx}[0]++); println(e{idx}[0])",
            pick(r, INTS)
        ),
    }
}

/// `try`/`catch`/`finally`/`throw`. Every probe catches what it throws — the
/// probes share one `main`, so an escaping exception would truncate the run and
/// test the harness rather than the frontend. Both throw sources are covered:
/// an explicit `throw` and a *runtime* fault the host raises (integer `/ 0`, `!!`
/// on null, an out-of-range index), which Kotlin reports as the same catchable
/// JVM exceptions.
fn g_exc(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, INTS);
    let d = pick(r, DIVS);
    let msg = pick(r, &["boom", "bad input", "x"]);
    match r.below(15) {
        0 => p(format!(
            "try {{ {a} / 0 }} catch (e: ArithmeticException) {{ -1 }}"
        )),
        1 => format!(
            "try {{ throw RuntimeException(\"{msg}\") }} catch (e: Exception) {{ println(e.message) }}"
        ),
        2 => format!(
            "var v{idx} = {a}; try {{ v{idx} += {a} / 0 }} catch (e: Exception) {{ v{idx} += 100 }}; println(v{idx})"
        ),
        3 => format!("try {{ println({a}) }} finally {{ println(\"fin{idx}\") }}"),
        4 => format!(
            "try {{ throw IllegalStateException(\"{msg}\") }} catch (e: IllegalStateException) {{ println(\"c\" + e.message) }} finally {{ println(\"f{idx}\") }}"
        ),
        // A subclass thrown, a supertype caught — the hierarchy walk.
        5 => format!(
            "try {{ throw IllegalArgumentException(\"{msg}\") }} catch (e: RuntimeException) {{ println(e) }}"
        ),
        6 => p(format!(
            "try {{ listOf({a}, {d})[{}] }} catch (e: IndexOutOfBoundsException) {{ -2 }}",
            2 + r.below(3)
        )),
        7 => format!(
            "val n{idx}: String? = null; println(try {{ n{idx}!!.length }} catch (e: NullPointerException) {{ -3 }})"
        ),
        // An exception raised inside a lambda, unwinding out of the nested run.
        8 => format!(
            "try {{ listOf(1, 2, 3).forEach {{ if (it == 2) throw RuntimeException(\"{msg}\") else println(it) }} }} catch (e: Exception) {{ println(\"L\" + e.message) }}"
        ),
        // Nested `try`: the inner one has no matching arm, so the outer takes it.
        9 => format!(
            "try {{ try {{ throw IllegalStateException(\"{msg}\") }} catch (e: ArithmeticException) {{ println(\"never\") }} }} catch (e: Exception) {{ println(\"outer \" + e.message) }}"
        ),
        // A `throw` crossing a call frame, and a `return` that has to run a
        // `finally` before leaving one.
        10 => format!(
            "try {{ println(boom{idx}({a})) }} catch (e: Exception) {{ println(\"fn \" + e.message) }}"
        ),
        14 => format!(
            "println(try {{ guard{idx}({a}) }} catch (e: Exception) {{ -1 }})"
        ),
        // Unwinding out of a loop, and continuing from a handler.
        11 => format!(
            "var s{idx} = 0; for (i in 1..4) {{ try {{ if (i == 3) throw RuntimeException(\"skip\") ; s{idx} += i }} catch (e: Exception) {{ s{idx} += 10 }} }}; println(s{idx})"
        ),
        // `try` as an expression, with a `finally` that runs on the value path.
        12 => format!(
            "val t{idx} = try {{ {a} * 2 }} finally {{ println(\"tf{idx}\") }}; println(t{idx})"
        ),
        _ => format!(
            "println(try {{ if ({a} > 0) {a} else throw IllegalArgumentException(\"{msg}\") }} catch (e: Exception) {{ e.message }})"
        ),
    }
}

/// A helper `fun` for the [`g_exc`] cross-frame probe: throws for a
/// non-positive argument so the caller's `catch` sees an exception raised one
/// frame down.
fn exc_helper(idx: usize) -> String {
    format!(
        "fun boom{idx}(n: Int): Int {{ if (n <= 0) throw IllegalStateException(\"neg\") ; return n * 2 }}"
    )
}

/// A helper `fun` whose `return` sits inside a `try` that owns a `finally`: the
/// finalizer has to run before the frame is left, on the value path and the
/// throwing one alike.
fn guard_helper(idx: usize) -> String {
    format!(
        "fun guard{idx}(n: Int): Int {{ try {{ if (n < 0) throw IllegalStateException(\"neg\") ; return n + 1 }} finally {{ println(\"g{idx}\") }} }}"
    )
}

/// The array lambda-initializer constructors. Arrays are never printed directly
/// (identity hash), so every probe reads a deterministic projection.
fn g_arrayinit(r: &mut Rng, idx: usize) -> String {
    let n = 1 + r.below(4);
    let k = pick(r, &["1", "2", "3"]);
    match r.below(6) {
        0 => format!("val q{idx} = IntArray({n}) {{ it * {k} }}; println(q{idx}.joinToString())"),
        1 => format!("val q{idx} = IntArray({n}) {{ it + {k} }}; println(q{idx}.sum())"),
        2 => format!("val q{idx} = IntArray({n}) {{ it }}; println(q{idx}.size)"),
        3 => format!(
            "val q{idx} = DoubleArray({n}) {{ it * 1.5 }}; println(q{idx}.joinToString())"
        ),
        4 => format!("val q{idx} = Array({n}) {{ it * it }}; println(q{idx}.toList())"),
        _ => format!(
            "val q{idx} = IntArray({n}) {{ it * {k} }}; println(q{idx}[{}])",
            r.below(n)
        ),
    }
}

/// `for (c in "…")` over a String's characters.
fn g_strchars(r: &mut Rng, idx: usize) -> String {
    let s = pick(r, &["\"abc\"", "\"Hello\"", "\"x\"", "\"AbC\""]);
    match r.below(5) {
        0 => format!("for (c{idx} in {s}) println(c{idx})"),
        1 => format!("var t{idx} = 0; for (c{idx} in {s}) t{idx} += c{idx}.code; println(t{idx})"),
        2 => format!("var u{idx} = \"\"; for (c{idx} in {s}) u{idx} += c{idx}; println(u{idx})"),
        3 => format!("for (c{idx} in {s}) print(c{idx}); println()"),
        _ => format!(
            "var w{idx} = 0; val z{idx} = {s}; for (c{idx} in z{idx}) w{idx}++; println(w{idx})"
        ),
    }
}

/// Null safety: `?.`, `?:`, `!!`, `== null`, and a nullable value's display.
fn g_nullsafe(r: &mut Rng, idx: usize) -> String {
    let s = pick(r, &["null", "\"abc\"", "\"\"", "\"Hi\""]);
    match r.below(9) {
        0 => format!("val n{idx}: String? = {s}; println(n{idx}?.length)"),
        1 => format!("val n{idx}: String? = {s}; println(n{idx}?.length ?: -1)"),
        2 => format!("val n{idx}: String? = {s}; println(n{idx} ?: \"dflt\")"),
        3 => format!("val n{idx}: String? = {s}; println(n{idx} == null)"),
        4 => format!("val n{idx}: String? = {s}; println(n{idx} != null)"),
        5 => format!("val n{idx}: String? = {s}; println(\"v=$n{idx}\")"),
        6 => format!("val n{idx}: String? = {s}; println(\"c\" + n{idx})"),
        7 => format!("val n{idx}: String? = {s}; println(n{idx}?.uppercase() ?: \"none\")"),
        _ => format!(
            "val n{idx}: Int? = {}; println(n{idx}?.plus(1) ?: 0)",
            pick(r, &["null", "1", "7", "-2"])
        ),
    }
}

/// `data class` members and `when` as an expression, over the `Pt` declaration
/// [`build_program`] always emits.
fn g_datawhen(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, INTS);
    let b = pick(r, STRS);
    match r.below(10) {
        0 => p(format!("Pt({a}, {b})")),
        1 => p(format!("Pt({a}, {b}) == Pt({a}, {b})")),
        2 => p(format!("Pt({a}, {b}).copy({})", pick(r, INTS))),
        3 => format!("val (dx{idx}, dy{idx}) = Pt({a}, {b}); println(\"$dx{idx}|$dy{idx}\")"),
        4 => p(format!("Pt({a}, {b}).x")),
        5 => p(format!("listOf(Pt({a}, {b}))")),
        6 => p(format!(
            "when ({a}) {{ 0, 1 -> \"lo\"; in 2..9 -> \"mid\"; !in -99..99 -> \"far\"; else -> \"hi\" }}"
        )),
        7 => p(format!(
            "when {{ {a} > 0 -> \"pos\"; {a} < 0 -> \"neg\"; else -> \"zero\" }}"
        )),
        8 => format!(
            "val any{idx}: Any = {}; println(when (any{idx}) {{ is Int -> \"i\"; is String -> \"s\"; else -> \"?\" }})",
            if r.below(2) == 0 { a } else { b }
        ),
        _ => format!(
            "val k{idx} = when ({a} % 3) {{ 0 -> 10; 1 -> 20; else -> 30 }}; println(k{idx} + 1)"
        ),
    }
}

/// Class inheritance: virtual dispatch through a supertype-typed binding, an
/// `override` that calls `super`, an `interface` default member, an `abstract`
/// member reached from a base-class method, `is` against every level of the
/// hierarchy, and a user class extending `Exception`.
///
/// Every probe here renders through a `toString()` override or a primitive, so
/// no identity hash (which no reimplementation can reproduce) is ever printed.
/// The declarations these reference live in [`declarations`].
fn g_class(r: &mut Rng, idx: usize) -> String {
    let k = pick(r, &["0", "1", "2", "3", "5"]);
    let j = pick(r, &["1", "2", "4"]);
    let s = pick(r, STRS);
    match r.below(14) {
        0 => p(format!("Sq({k}).area()")),
        1 => p(format!("Ci({k}).area()")),
        2 => p(format!("Sq({k}).tag()")),
        3 => p(format!("Sq({k})")),
        4 => p(format!("Ci({k})")),
        // Dispatch through a supertype-typed binding: the runtime class decides.
        5 => format!(
            "val sh{idx}: Shp = {}; println(sh{idx}.area()); println(sh{idx}.tag()); println(sh{idx})",
            pick(r, &["Sq", "Ci", "Shp"]).to_string() + "(" + k + ")"
        ),
        6 => format!(
            "val sl{idx} = listOf(Shp({k}), Sq({j}), Ci({k})); println(sl{idx}); println(sl{idx}.map {{ it.area() }}); println(sl{idx}.joinToString(\"/\"))"
        ),
        7 => format!(
            "val si{idx}: Shp = {}({k}); println(si{idx} is Shp); println(si{idx} is Sq); println(si{idx} is Ci)",
            pick(r, &["Sq", "Ci", "Shp"])
        ),
        // An interface default member calling the implementor's override.
        8 => p(format!("Yell({s}).twice()")),
        9 => format!("val ld{idx}: Loud = Yell({s}); println(ld{idx}.shout()); println(ld{idx}.twice()); println(ld{idx} is Yell)"),
        // A concrete method on an abstract base calling the abstract member.
        10 => p(format!("D2({k}, {j}).g()")),
        11 => format!("val ab{idx}: Base2 = D2({k}, {j}); println(ab{idx}.g()); println(ab{idx}.f()); println(ab{idx}.b)"),
        // A user class extending Exception: message, display, catch matching.
        12 => format!(
            "try {{ throw KtErr({s}) }} catch (e: KtErr) {{ println(e.message); println(e) }}"
        ),
        _ => format!(
            "println(try {{ throw KtErr({s}); \"no\" }} catch (e: Exception) {{ \"c:\" + e.message }})"
        ),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    All,
    Arith,
    IntDiv,
    DoubleArith,
    MixedDiv,
    DivZero,
    DoubleFmt,
    Concat,
    Template,
    Compare,
    Bool,
    IfExpr,
    StrMember,
    Loop,
    List,
    Range,
    Array,
    Math,
    IncDec,
    Exc,
    ArrayInit,
    StrChars,
    NullSafe,
    DataWhen,
    Class,
}

const CONCRETE: &[Mode] = &[
    Mode::Arith,
    Mode::IntDiv,
    Mode::DoubleArith,
    Mode::MixedDiv,
    Mode::DivZero,
    Mode::DoubleFmt,
    Mode::Concat,
    Mode::Template,
    Mode::Compare,
    Mode::Bool,
    Mode::IfExpr,
    Mode::StrMember,
    Mode::Loop,
    Mode::List,
    Mode::Range,
    Mode::Array,
    Mode::Math,
    Mode::IncDec,
    Mode::Exc,
    Mode::ArrayInit,
    Mode::StrChars,
    Mode::NullSafe,
    Mode::DataWhen,
    Mode::Class,
];

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::All => "all",
        Mode::Arith => "arith",
        Mode::IntDiv => "intdiv",
        Mode::DoubleArith => "doublearith",
        Mode::MixedDiv => "mixeddiv",
        Mode::DivZero => "divzero",
        Mode::DoubleFmt => "doublefmt",
        Mode::Concat => "concat",
        Mode::Template => "template",
        Mode::Compare => "compare",
        Mode::Bool => "bool",
        Mode::IfExpr => "ifexpr",
        Mode::StrMember => "strmember",
        Mode::Loop => "loop",
        Mode::List => "list",
        Mode::Range => "range",
        Mode::Array => "array",
        Mode::Math => "math",
        Mode::IncDec => "incdec",
        Mode::Exc => "exc",
        Mode::ArrayInit => "arrayinit",
        Mode::StrChars => "strchars",
        Mode::NullSafe => "nullsafe",
        Mode::DataWhen => "datawhen",
        Mode::Class => "class",
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    if s == "all" {
        return Some(Mode::All);
    }
    CONCRETE.iter().copied().find(|m| mode_name(*m) == s)
}

fn gen_probe(r: &mut Rng, mode: Mode, idx: usize) -> String {
    let m = if mode == Mode::All {
        *pick(r, CONCRETE)
    } else {
        mode
    };
    match m {
        Mode::Arith => g_arith(r),
        Mode::IntDiv => g_intdiv(r),
        Mode::DoubleArith => g_doublearith(r),
        Mode::MixedDiv => g_mixeddiv(r),
        Mode::DivZero => g_divzero(r),
        Mode::DoubleFmt => g_doublefmt(r),
        Mode::Concat => g_concat(r),
        Mode::Template => g_template(r, idx),
        Mode::Compare => g_compare(r),
        Mode::Bool => g_bool(r),
        Mode::IfExpr => g_ifexpr(r),
        Mode::StrMember => g_strmember(r),
        Mode::Loop => g_loop(r, idx),
        Mode::List => g_list(r),
        Mode::Range => g_range(r, idx),
        Mode::Array => g_array(r, idx),
        Mode::Math => g_math(r),
        Mode::IncDec => g_incdec(r, idx),
        Mode::Exc => g_exc(r, idx),
        Mode::ArrayInit => g_arrayinit(r, idx),
        Mode::StrChars => g_strchars(r, idx),
        Mode::NullSafe => g_nullsafe(r, idx),
        Mode::DataWhen => g_datawhen(r, idx),
        Mode::Class => g_class(r, idx),
        Mode::All => unreachable!("resolved above"),
    }
}

fn gen_probes(seed: u64, mode: Mode, n: usize) -> Vec<String> {
    let mut r = Rng::new(seed);
    (0..n).map(|i| gen_probe(&mut r, mode, i)).collect()
}

/// Probes are emitted straight into `main`. Any probe that declares a name
/// suffixes it with the probe index, so packing many into one `main` keeps them
/// independent for [`minimize`] without a block wrapper — kotlinrs has no
/// `run { }` scope function, so wrapping would test the harness, not the
/// frontend.
///
/// The `kotlin.math` import is unconditional: Kotlin does not auto-import that
/// package, so the math probes need it, and an unused import is only a warning
/// (never a compile failure) for the programs that contain no math probe.
/// Top-level declarations a probe may reference are emitted only when some probe
/// actually names them, so an unrelated program stays minimal and [`minimize`]
/// keeps producing compilable reductions: the `Pt` `data class`, and one
/// `boom<i>` helper per cross-frame `throw` probe. The helper id is read back
/// out of the probe text rather than from its position, because `minimize` drops
/// probes and would otherwise renumber them.
fn declarations(probes: &[String]) -> String {
    let mut out = String::new();
    if probes.iter().any(|p| p.contains("Pt(")) {
        out.push_str("data class Pt(val x: Int, val y: String)\n");
    }
    // The class hierarchy the `class` mode probes: a three-level `open`/`override`
    // chain with a `super` call and a `toString()` override, an interface with a
    // default member, an abstract class whose concrete method calls its abstract
    // one, and a user throwable. Each block is emitted only when a probe names it,
    // so `minimize` keeps producing compilable reductions.
    for (marker, decl) in [
        (
            "Shp",
            "open class Shp(val k: Int) {\n\
             \x20   open fun area(): Int = k\n\
             \x20   open fun tag(): String = \"shp$k\"\n\
             \x20   override fun toString(): String = \"Shp[\" + tag() + \"=\" + area() + \"]\"\n\
             }\n\
             class Sq(k: Int) : Shp(k) {\n\
             \x20   override fun area(): Int = k * k\n\
             \x20   override fun tag(): String = \"sq/\" + super.tag()\n\
             }\n\
             class Ci(k: Int) : Shp(k) {\n\
             \x20   override fun area(): Int = 3 * k * k\n\
             \x20   override fun toString(): String = \"Ci[\" + area() + \"]\"\n\
             }\n",
        ),
        (
            "Yell(",
            "interface Loud {\n\
             \x20   fun shout(): String\n\
             \x20   fun twice(): String = shout() + \"-\" + shout()\n\
             }\n\
             class Yell(val w: String) : Loud {\n\
             \x20   override fun shout(): String = w.uppercase()\n\
             }\n",
        ),
        (
            "D2(",
            "abstract class Base2(val b: Int) {\n\
             \x20   abstract fun f(): Int\n\
             \x20   fun g(): Int = f() + b\n\
             }\n\
             class D2(b: Int, val m: Int) : Base2(b) {\n\
             \x20   override fun f(): Int = m * 2\n\
             }\n",
        ),
        (
            "KtErr(",
            "class KtErr(msg: String) : Exception(msg)\n",
        ),
    ] {
        // `Sq(`/`Ci(` also need the `Shp` block, so the shape marker is the bare
        // type prefix rather than a constructor call.
        let named = match marker {
            "Shp" => probes
                .iter()
                .any(|p| p.contains("Shp") || p.contains("Sq(") || p.contains("Ci(")),
            _ => probes.iter().any(|p| p.contains(marker)),
        };
        if named {
            out.push_str(decl);
        }
    }
    for (marker, decl) in [
        ("boom", exc_helper as fn(usize) -> String),
        ("guard", guard_helper as fn(usize) -> String),
    ] {
        let mut ids: Vec<usize> = Vec::new();
        for probe in probes {
            let mut rest = probe.as_str();
            while let Some(at) = rest.find(marker) {
                rest = &rest[at + marker.len()..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(id) = digits.parse::<usize>() {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        ids.sort_unstable();
        for id in ids {
            out.push_str(&decl(id));
            out.push('\n');
        }
    }
    out
}

fn build_program(probes: &[String]) -> String {
    let mut s = String::from("import kotlin.math.*\n\n");
    s.push_str(&declarations(probes));
    s.push_str("\nfun main() {\n");
    for probe in probes {
        s.push_str("    ");
        s.push_str(probe);
        s.push('\n');
    }
    s.push_str("}\n");
    s
}

struct RunOut {
    stdout: Vec<u8>,
    ok: bool,
}

static TMP_CTR: AtomicU64 = AtomicU64::new(0);

fn workdir() -> PathBuf {
    let n = TMP_CTR.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("kotlinrs_parity_{}_{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn capture(cmd: &mut Command, timeout: Duration) -> Option<(Vec<u8>, bool)> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };
    let out = child.wait_with_output().ok()?;
    Some((out.stdout, status.map(|s| s.success()).unwrap_or(false)))
}

/// Run through our frontend: one command, the `.kt` straight in.
fn run_ours(ours: &Path, src: &str, timeout: Duration) -> RunOut {
    let dir = workdir();
    let path = dir.join("T.kt");
    let _ = std::fs::write(&path, src);
    let mut cmd = Command::new(ours);
    cmd.arg(&path);
    let r = capture(&mut cmd, timeout);
    let _ = std::fs::remove_dir_all(&dir);
    match r {
        Some((stdout, ok)) => RunOut { stdout, ok },
        None => RunOut {
            stdout: Vec::new(),
            ok: false,
        },
    }
}

/// Run through the reference toolchain: compile with `kotlinc`, then run the
/// generated `TKt` class. A compile failure reports `ok == false` with no
/// stdout — exactly how a parse error surfaces on our side too.
fn run_oracle(kotlinc: &Path, kotlin: &Path, src: &str, timeout: Duration) -> RunOut {
    let dir = workdir();
    let path = dir.join("T.kt");
    let out = dir.join("out");
    let _ = std::fs::write(&path, src);

    let mut c = Command::new(kotlinc);
    c.arg(&path).arg("-d").arg(&out).current_dir(&dir);
    let compiled = matches!(capture(&mut c, timeout), Some((_, true)));
    if !compiled {
        let _ = std::fs::remove_dir_all(&dir);
        return RunOut {
            stdout: Vec::new(),
            ok: false,
        };
    }

    let mut r = Command::new(kotlin);
    r.arg("-classpath").arg(&out).arg("TKt").current_dir(&dir);
    let res = capture(&mut r, timeout);
    let _ = std::fs::remove_dir_all(&dir);
    match res {
        Some((stdout, ok)) => RunOut { stdout, ok },
        None => RunOut {
            stdout: Vec::new(),
            ok: false,
        },
    }
}

struct Tools {
    ours: PathBuf,
    kotlinc: PathBuf,
    kotlin: PathBuf,
}

fn differs(oracle: &RunOut, ours: &RunOut) -> bool {
    oracle.stdout != ours.stdout || oracle.ok != ours.ok
}

fn render(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\n', "\\n")
}

fn diverges(probes: &[String], t: &Tools, timeout: Duration) -> bool {
    let src = build_program(probes);
    let a = run_oracle(&t.kotlinc, &t.kotlin, &src, timeout);
    let b = run_ours(&t.ours, &src, timeout);
    differs(&a, &b)
}

fn minimize(probes: &[String], t: &Tools, timeout: Duration) -> Vec<String> {
    let mut cur = probes.to_vec();
    let mut chunk = cur.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        while i < cur.len() {
            let mut trial = cur.clone();
            let end = (i + chunk).min(trial.len());
            trial.drain(i..end);
            if !trial.is_empty() && diverges(&trial, t, timeout) {
                cur = trial;
            } else {
                i += chunk;
            }
        }
        if chunk == 1 {
            break;
        }
        chunk /= 2;
    }
    cur
}

fn ours_bin() -> PathBuf {
    if let Ok(p) = std::env::var("KOTLINRS_BIN") {
        return PathBuf::from(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let c = root.join("target").join(profile).join("kotlin");
        if c.exists() {
            return c;
        }
    }
    root.join("target/debug/kotlin")
}

/// Find a reference tool on PATH, skipping our own binary of the same name.
fn on_path(name: &str, skip: &Path) -> Option<PathBuf> {
    let skip = skip.canonicalize().ok();
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let cand = Path::new(dir).join(name);
        if cand.exists() && cand.canonicalize().ok() != skip {
            return Some(cand);
        }
    }
    None
}

struct Args {
    iters: usize,
    probes: usize,
    seed: Option<u64>,
    once: bool,
    mode: Mode,
    timeout: Duration,
    verbose: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        iters: 20,
        probes: 40,
        seed: None,
        once: false,
        mode: Mode::All,
        timeout: Duration::from_secs(300),
        verbose: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].clone();
        let take = |i: &mut usize| -> String {
            *i += 1;
            argv.get(*i).cloned().unwrap_or_default()
        };
        match arg.as_str() {
            "--iters" => a.iters = take(&mut i).parse().unwrap_or(a.iters),
            "--probes" => a.probes = take(&mut i).parse().unwrap_or(a.probes),
            "--seed" => a.seed = take(&mut i).parse().ok(),
            "--once" => a.once = true,
            "--verbose" | "-v" => a.verbose = true,
            "--timeout" => a.timeout = Duration::from_secs(take(&mut i).parse().unwrap_or(300)),
            "--mode" => {
                let m = take(&mut i);
                match parse_mode(&m) {
                    Some(m) => a.mode = m,
                    None => {
                        eprintln!("parity-fuzz: unknown mode `{m}`");
                        std::process::exit(2);
                    }
                }
            }
            "--help" | "-h" => {
                println!("parity-fuzz [--iters N] [--probes N] [--seed N] [--once] [--mode M] [--timeout SECS] [-v]");
                println!(
                    "modes: all {}",
                    CONCRETE
                        .iter()
                        .map(|m| mode_name(*m))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("parity-fuzz: unknown option `{other}`");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    a
}

fn main() {
    let args = parse_args();
    let ours = ours_bin();
    if !ours.exists() {
        eprintln!("parity-fuzz: {} not built (cargo build)", ours.display());
        std::process::exit(2);
    }
    let kotlinc = match std::env::var("KOTLINC")
        .map(PathBuf::from)
        .ok()
        .or_else(|| on_path("kotlinc", &ours))
    {
        Some(p) => p,
        None => {
            eprintln!("parity-fuzz: no `kotlinc` on PATH (set KOTLINC=/path/to/kotlinc)");
            std::process::exit(2);
        }
    };
    let kotlin = match std::env::var("KOTLIN_ORACLE")
        .map(PathBuf::from)
        .ok()
        .or_else(|| on_path("kotlin", &ours))
    {
        Some(p) => p,
        None => {
            eprintln!("parity-fuzz: no reference `kotlin` on PATH (set KOTLIN_ORACLE=...)");
            std::process::exit(2);
        }
    };

    let t = Tools {
        ours,
        kotlinc,
        kotlin,
    };
    eprintln!(
        "parity-fuzz: oracle={} + {} ours={} mode={} probes={}",
        t.kotlinc.display(),
        t.kotlin.display(),
        t.ours.display(),
        mode_name(args.mode),
        args.probes
    );

    let iters = if args.once { 1 } else { args.iters };
    let base = args.seed.unwrap_or(0x5EED);
    let mut failures = 0usize;
    let mut probes_run = 0usize;

    for k in 0..iters {
        let seed = if args.once {
            base
        } else {
            base.wrapping_add(k as u64)
        };
        let probes = gen_probes(seed, args.mode, args.probes);
        probes_run += probes.len();
        if !diverges(&probes, &t, args.timeout) {
            if args.verbose {
                eprintln!("seed {seed}: ok ({} probes)", probes.len());
            }
            continue;
        }
        failures += 1;
        let minimal = minimize(&probes, &t, args.timeout);
        let src = build_program(&minimal);
        let a = run_oracle(&t.kotlinc, &t.kotlin, &src, args.timeout);
        let b = run_ours(&t.ours, &src, args.timeout);
        println!("=== DIVERGENCE seed {seed} (replay: --seed {seed} --once) ===");
        for probe in &minimal {
            println!("  {probe}");
        }
        println!("  oracle: ok={} out={}", a.ok, render(&a.stdout));
        println!("  ours  : ok={} out={}", b.ok, render(&b.stdout));
    }

    eprintln!("parity-fuzz: {iters} program(s), {probes_run} probe(s), {failures} divergence(s)");
    if failures > 0 {
        std::process::exit(1);
    }
}
