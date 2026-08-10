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
//! The later modes target the constructs whose LOWERING differs from a nearby
//! one that already works, which is where a frontend quietly reuses the wrong
//! path: `dowhile` (the body precedes the test and `continue` targets the test),
//! `strfmt` (`%f` is HALF_UP over the shortest decimal form, where Rust's
//! formatter is half-to-even), `bitwise` (32-bit shifts and `inv` under a 64-bit
//! integer representation), `safecall` (`?.` must reach every routing `.` does,
//! and must still print `null`), `mapcoll` (a `Map` iterates as entries and
//! `filter` re-wraps into a `Map`), and `finexit` (a `break`/`continue` leaving
//! a `try` has to run its `finally` first).
//!
//! The newest modes target the two shapes a frontend gets wrong SILENTLY rather
//! than loudly. First, an argument or a lambda that a shorter overload does not
//! have, which a name-only dispatch drops on the floor and answers the shorter
//! overload's result for: `collarg` (`joinToString`'s affixes and limit,
//! `windowed`'s step and `partialWindows`, the transform-taking `chunked`/
//! `windowed`/`zip`), `predicate` (`first`/`last`/`single`/`find` — each shares
//! its name with a no-argument member, so running the wrong one answers the
//! first ELEMENT rather than the first MATCH), and `strsearch` (`indexOf`'s
//! `startIndex`, `compareTo`'s `ignoreCase`). Second, a value whose type is not
//! written at the point it is used: `width` (Kotlin decides integer width from
//! the STATIC type, so an `Int` overflow inside a lambda has a right answer only
//! if the receiver's element type reaches the parameter — and `Long` receivers
//! are mixed in, so always narrowing fails too), `hash` (the JVM's exact
//! `hashCode` contract, where `Int` and `Long` fold differently from the same
//! runtime representation), and `entry` (a `Map.Entry` is not a `Pair`, and
//! `keys`/`entries` are `Set`s — three silent differences in display, hashing,
//! and equality).
//!
//! Scope + determinism invariants (mirroring the javars/scalars harnesses):
//!   * Only constructs kotlinrs actually implements are emitted — an unsupported
//!     construct would be a known gap, not a parity signal.
//!   * No nondeterministic output (no `Random`, no time, no identity hashes, no
//!     unordered collections). Every probe's output is a pure function of source.
//!     This is why an array is never printed directly: a JVM array inherits
//!     `Object.toString`, so `println(arrayOf(1))` emits an identity hash. Array
//!     probes read `size`/elements/`sum()`/`joinToString()` instead. The same
//!     rule keeps the identity-HASHED kinds out of `hash` (a non-`data` class,
//!     an array, a lambda) and `Map.values` out of `entry`: `values` is a plain
//!     `Collection` whose hash and equality the JVM leaves as identity, and
//!     whose answer even changes with the map implementation `mapOf` picked.
//!   * Integer operands stay well inside range EXCEPT in `width`, whose whole
//!     subject is 32-bit overflow, and integer divisors are never zero
//!     (Kotlin throws there; that is a fault-path test, not a value test).
//!     Likewise ranges are never empty where a probe calls `max()`/`min()`,
//!     which throw on an empty sequence.
//!   * Every exception a probe raises is caught by that same probe. The probes
//!     share one `main`, so an escaping throw would truncate the whole program
//!     and test the harness instead of the frontend.
//!
//! WHAT THIS HARNESS STRUCTURALLY CANNOT REPORT. Every generator invariant
//! above is also a blind spot, and so is every field the comparison drops.
//! Naming them is the only thing that keeps a clean run from reading as
//! "no divergences exist" rather than "none in what is looked at":
//!
//!   * **stderr.** [`capture`] pipes it and then returns only `stdout`, and
//!     [`differs`] compares `stdout` and the success bool. So the TEXT of an
//!     uncaught exception, the stack shape under it, and any warning either
//!     side prints are invisible. Only probes that CATCH and `println` a
//!     message put a fault's wording in front of the comparison — which is why
//!     [`g_exc`] and friends always catch.
//!   * **The exit code's value.** `ok` is `status.success()`, one bit. An
//!     oracle exiting 1 and a frontend exiting 134 agree here.
//!   * **Interleaving.** stdout and stderr are captured on separate pipes, so
//!     the order a terminal would show them in is not compared at all.
//!   * **Anything the generators never emit** — the determinism rules above bar
//!     `Random`, the clock, identity hashes, unordered containers, printing an
//!     array directly, and integer overflow outside `width`. Those are not
//!     "known good"; they are unlooked-at.
//!   * **A hang.** [`capture`]'s timeout kills the child and reports
//!     `ok = false` with whatever it had printed, which reads as an abort.
//!
//! The two axes that USED to be blind here are now gated: the oracle's JVM
//! (both of them — see [`check_oracle_jvms`]) and the run step's locale and
//! console charset (see [`ORACLE_JVM_PINS`]).
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

const INTS: &[&str] = &[
    "0", "1", "2", "3", "7", "10", "42", "100", "-1", "-7", "-42",
];
const DIVS: &[&str] = &["1", "2", "3", "4", "5", "7", "-2", "-3"];
const DBLS: &[&str] = &[
    "0.0",
    "1.0",
    "0.5",
    "2.5",
    "3.14",
    "-1.5",
    "100.0",
    "1e3",
    "1e-3",
    "0.1",
    "1234567.0",
    "1.0e7",
    "1.0e-7",
    "123456789.0",
    "9.999e-4",
];
const ZDIVS: &[&str] = &["0.0", "-0.0"];
const STRS: &[&str] = &[
    "\"\"",
    "\"a\"",
    "\"abc\"",
    "\"Hello\"",
    "\" x \"",
    "\"AbC\"",
];
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
        3 => {
            format!("val q{idx} = DoubleArray({n}) {{ it * 1.5 }}; println(q{idx}.joinToString())")
        }
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
    match r.below(9) {
        0 => format!("for (c{idx} in {s}) println(c{idx})"),
        1 => format!("var t{idx} = 0; for (c{idx} in {s}) t{idx} += c{idx}.code; println(t{idx})"),
        2 => format!("var u{idx} = \"\"; for (c{idx} in {s}) u{idx} += c{idx}; println(u{idx})"),
        3 => format!("for (c{idx} in {s}) print(c{idx}); println()"),
        4 => format!(
            "var w{idx} = 0; val z{idx} = {s}; for (c{idx} in z{idx}) w{idx}++; println(w{idx})"
        ),
        // `s[i]` — indexed by UTF-16 code unit; the index stays in range because
        // an out-of-range one throws (a fault-path test, not a value test).
        5 => format!("println({s}[0])"),
        6 => format!("val q{idx} = {s}; println(q{idx}[q{idx}.length - 1])"),
        7 => format!("val q{idx} = {s}; println(q{idx}[0].code)"),
        _ => format!(
            "var v{idx} = \"\"; val q{idx} = {s};              for (i{idx} in 0 until q{idx}.length) v{idx} += q{idx}[i{idx}]; println(v{idx})"
        ),
    }
}

/// `Char` as a runtime type of its own: its display in every position (bare, in
/// a collection, in a template), `Char` arithmetic and ordering — including in
/// the statically untyped positions a lambda creates — the `Char` members, `is
/// Char` vs `is Int`, and `CharRange`.
fn g_char(r: &mut Rng, idx: usize) -> String {
    let c = pick(r, &["'a'", "'z'", "'A'", "'0'", "'m'", "' '", "'~'"]);
    let d = pick(r, &["'a'", "'q'", "'B'", "'9'"]);
    let n = pick(r, &["0", "1", "2", "5", "-1"]);
    match r.below(24) {
        0 => format!("println({c})"),
        1 => format!("println(listOf({c}, {d}))"),
        2 => format!("println(setOf({c}, {d}, {c}))"),
        3 => format!("println(mapOf({c} to 1, {d} to 2))"),
        4 => format!("println({c} to {d})"),
        5 => format!("println({c} + {n})"),
        6 => format!("println({c} - {d})"),
        7 => format!("println({c}.code)"),
        8 => format!("println({}.toChar())", pick(r, &["65", "97", "48", "122"])),
        9 => format!("println(\"[${c}]\")"),
        10 => format!("println({c} == {d})"),
        11 => format!(
            "println({c} {} {d})",
            pick(r, &["<", ">", "<=", ">=", "!="])
        ),
        12 => format!("println(listOf({c}, {d}).map {{ it + 1 }})"),
        13 => format!("println(listOf({c}, {d}).filter {{ it > {d} }})"),
        14 => format!("println(listOf({c}, {d}).sorted())"),
        15 => format!("println(listOf({c}, {d}).joinToString(\"\"))"),
        16 => format!("println(listOf({c}, {d}).fold(\"\") {{ a, b -> a + b }})"),
        17 => format!("val a{idx}: Any = {c}; println(a{idx} is Char); println(a{idx} is Int)"),
        18 => format!(
            "println({c}.{})",
            pick(
                r,
                &[
                    "isDigit()",
                    "isLetter()",
                    "isLetterOrDigit()",
                    "isWhitespace()",
                    "isUpperCase()",
                    "isLowerCase()",
                    "uppercaseChar()",
                    "lowercaseChar()",
                    "uppercase()",
                    "lowercase()",
                    "toString()",
                    "hashCode()",
                ]
            )
        ),
        19 => format!("println({c}.compareTo({d}))"),
        20 => "println(('a'..'e').toList())".to_string(),
        21 => format!("println({c} in 'a'..'z')"),
        22 => format!("for (k{idx} in 'a'..'d') print(k{idx}); println()"),
        _ => format!("var m{idx} = {c}; m{idx}++; println(m{idx})"),
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

/// `do { … } while (cond)`, whose body always runs once and whose `continue`
/// targets the CONDITION rather than the loop top — the two things that make it
/// a different lowering from `while`, not a rewrite of it. Every generated loop
/// has a bounded counter so a mis-lowered exit shows up as a divergence rather
/// than a hang.
fn g_dowhile(r: &mut Rng, idx: usize) -> String {
    let n = 1 + r.below(5);
    let m = 1 + r.below(4);
    match r.below(6) {
        0 => format!(
            "var d{idx} = 0; do {{ d{idx}++ }} while (d{idx} < {n}); println(d{idx})"
        ),
        // A condition that is already false on entry: the body still runs once.
        1 => format!(
            "var d{idx} = {n}; do {{ d{idx}++; println(d{idx}) }} while (d{idx} < 0)"
        ),
        2 => format!(
            "var d{idx} = 0; do {{ d{idx}++; if (d{idx} % {m} == 0) continue; println(d{idx}) }} while (d{idx} < {n})"
        ),
        3 => format!(
            "var d{idx} = 0; do {{ d{idx}++; if (d{idx} == {m}) break; println(d{idx}) }} while (d{idx} < 9); println(\"e$d{idx}\")"
        ),
        4 => format!(
            "var d{idx} = 0; var t{idx} = 0; do {{ var j{idx} = 0; do {{ j{idx}++; t{idx}++ }} while (j{idx} < {m}); d{idx}++ }} while (d{idx} < {n}); println(t{idx})"
        ),
        _ => format!(
            "var d{idx} = 0; L{idx}@ do {{ d{idx}++; for (k{idx} in 1..3) {{ if (k{idx} == {m}) continue@L{idx}; println(\"$d{idx}/$k{idx}\") }} }} while (d{idx} < {n})"
        ),
    }
}

/// `String.format` — the JVM `Formatter` conversions and their flags. `%f`
/// rounds HALF_UP over the value's shortest decimal form, which differs from
/// Rust's half-to-even at every tie, so the tie values are generated on purpose.
fn g_strfmt(r: &mut Rng) -> String {
    const TIES: &[&str] = &[
        "0.5", "1.5", "2.5", "-0.5", "-1.5", "0.15", "0.25", "2.45", "9.95", "1.005", "2.675",
        "3.14159", "1e10", "0.0",
    ];
    match r.below(8) {
        0 => p(format!("\"%.{}f\".format({})", r.below(4), pick(r, TIES))),
        1 => p(format!("\"%f\".format({})", pick(r, TIES))),
        2 => p(format!(
            "\"%{}{}d|\".format({})",
            pick(r, &["", "-", "0", "+"]),
            1 + r.below(8),
            pick(r, INTS)
        )),
        3 => p(format!(
            "\"%{}{}s|\".format({})",
            pick(r, &["", "-"]),
            1 + r.below(8),
            pick(r, STRS)
        )),
        4 => p(format!(
            "\"%{}\".format({})",
            pick(r, &["x", "X", "o"]),
            pick(r, &["0", "1", "8", "10", "255", "4095"])
        )),
        5 => p(format!(
            "\"%s=%d\".format({}, {})",
            pick(r, STRS),
            pick(r, INTS)
        )),
        6 => p(format!(
            "\"%0{}.{}f\".format({})",
            2 + r.below(8),
            r.below(3),
            pick(r, TIES)
        )),
        _ => p(format!("\"%b/%%\".format({})", pick(r, BOOLS))),
    }
}

/// The bitwise infix member functions. `and`/`or`/`xor` are width-agnostic over
/// these operands; `shl`/`shr`/`ushr`/`inv` are `Int` (32-bit) operations, which
/// is where a naive 64-bit lowering would show.
fn g_bitwise(r: &mut Rng) -> String {
    const BITS: &[&str] = &[
        "0", "1", "0xF", "0xFF", "0x10", "0b1010", "255", "-1", "-256",
    ];
    let a = pick(r, BITS);
    let b = pick(r, BITS);
    match r.below(6) {
        0 => p(format!("{a} and {b}")),
        1 => p(format!("{a} or {b}")),
        2 => p(format!("{a} xor {b}")),
        3 => p(format!("({a}).inv()")),
        4 => p(format!("{a} shl {}", r.below(8))),
        _ => p(format!("{a} {} {}", pick(r, &["shr", "ushr"]), r.below(8))),
    }
}

/// A safe call `?.` on a receiver that may be null, over the member kinds the
/// lowering routes differently: a stdlib member, a collection higher-order
/// function, and an `it`-form scope function. A null receiver must short-circuit
/// in every one of them.
fn g_safecall(r: &mut Rng, idx: usize) -> String {
    let s = pick(r, &["null", "\"abc\"", "\"\"", "\"Hi\""]);
    let l = pick(r, &["null", "listOf(1, 2, 3)", "listOf(7)", "emptyList()"]);
    match r.below(8) {
        0 => format!("val q{idx}: String? = {s}; println(q{idx}?.uppercase())"),
        1 => format!("val q{idx}: String? = {s}; println(q{idx}?.let {{ it + \"!\" }})"),
        2 => format!("val q{idx}: String? = {s}; println(q{idx}?.take(2)?.length)"),
        3 => format!("val q{idx}: List<Int>? = {l}; println(q{idx}?.map {{ it * 2 }})"),
        4 => format!("val q{idx}: List<Int>? = {l}; println(q{idx}?.filter {{ it > 1 }})"),
        5 => format!("val q{idx}: List<Int>? = {l}; println(q{idx}?.sum())"),
        6 => {
            format!("val q{idx}: List<Int>? = {l}; println(q{idx}?.sorted()?.joinToString(\"|\"))")
        }
        _ => format!("val q{idx}: List<Int>? = {l}; println(q{idx}?.fold(10) {{ a, b -> a + b }})"),
    }
}

/// `Map` as an iterable: its higher-order functions see one `Map.Entry` per
/// element, `filter` re-wraps into a `Map` (where `map` yields a `List`), and
/// `for ((k, v) in m)` destructures the entry. Map literals keep insertion
/// order on both sides, so the printed forms are deterministic.
fn g_mapcoll(r: &mut Rng, idx: usize) -> String {
    let m = pick(
        r,
        &[
            "mapOf(\"a\" to 1, \"b\" to 2)",
            "mapOf(1 to \"x\", 2 to \"y\")",
            "mapOf(\"k\" to 9)",
        ],
    );
    match r.below(9) {
        0 => p(format!("{m}.map {{ it.key }}")),
        1 => p(format!("{m}.map {{ it.value }}")),
        2 => p(format!("{m}.entries.size")),
        3 => p(format!("{m}.keys.toList()")),
        4 => p(format!("{m}.values.toList()")),
        5 => p(format!("{m}.any {{ true }}")),
        6 => p(format!("{m}.count {{ true }}")),
        7 => format!("for ((k{idx}, v{idx}) in {m}) {{ println(\"$k{idx}=$v{idx}\") }}"),
        _ => format!("for (e{idx} in {m}) {{ println(e{idx}.key) }}"),
    }
}

/// A `break`/`continue` that leaves a `try` owning a `finally`: the finalizer
/// has to run on the way out, and every finalizer between the jump and its
/// target has to run innermost-first. The labeled form whose `break` sits in a
/// loop NESTED inside the `try` is included because it crosses the `try`
/// without appearing in its body.
fn g_finexit(r: &mut Rng, idx: usize) -> String {
    let n = 2 + r.below(3);
    let m = 1 + r.below(3);
    match r.below(6) {
        0 => format!(
            "for (x{idx} in 1..{n}) {{ try {{ if (x{idx} == {m}) break; println(\"b$x{idx}\") }} finally {{ println(\"f$x{idx}\") }} }}"
        ),
        1 => format!(
            "for (x{idx} in 1..{n}) {{ try {{ if (x{idx} == {m}) continue; println(\"c$x{idx}\") }} finally {{ println(\"g$x{idx}\") }} }}"
        ),
        2 => format!(
            "O{idx}@ for (x{idx} in 1..{n}) {{ try {{ for (y{idx} in 1..3) {{ if (y{idx} == {m}) break@O{idx} }} }} finally {{ println(\"n$x{idx}\") }} }}"
        ),
        3 => format!(
            "for (x{idx} in 1..{n}) {{ try {{ try {{ if (x{idx} == {m}) continue }} finally {{ println(\"i$x{idx}\") }} }} finally {{ println(\"o$x{idx}\") }} ; println(\"t$x{idx}\") }}"
        ),
        4 => format!(
            "var z{idx} = 0; do {{ z{idx}++; try {{ if (z{idx} == {m}) break; println(\"d$z{idx}\") }} finally {{ println(\"h$z{idx}\") }} }} while (z{idx} < {n})"
        ),
        _ => format!(
            "O{idx}@ for (x{idx} in 1..{n}) {{ for (y{idx} in 1..3) {{ try {{ if (y{idx} == {m}) continue@O{idx}; println(\"$x{idx}.$y{idx}\") }} finally {{ println(\"F$x{idx}$y{idx}\") }} }} }}"
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
    match r.below(18) {
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
        13 => format!(
            "println(try {{ throw KtErr({s}); \"no\" }} catch (e: Exception) {{ \"c:\" + e.message }})"
        ),
        // `super<T>.m()`: two supertypes implement `m`, so the qualifier is what
        // decides which body runs — and Kotlin *requires* it here, which makes
        // this the only spelling that can be tested against the oracle.
        14 => p(format!("Both({k}).pick()")),
        15 => p(format!("Both({k}).only()")),
        16 => format!("val bt{idx}: Left = Both({k}); println(bt{idx}.pick()); println(bt{idx}.only())"),
        _ => p(format!("Sub({k}).chain()")),
    }
}

/// A `data class` that inherits stored properties: Kotlin derives its
/// `toString`/`equals`/`hashCode`/`componentN`/`copy` from the primary
/// constructor *alone*, while the inherited field is still readable.
fn g_datainherit(r: &mut Rng, idx: usize) -> String {
    let k = pick(r, &["0", "1", "2", "7", "-3"]);
    let j = pick(r, &["1", "2", "9"]);
    let s = pick(r, &["\"a\"", "\"abc\"", "\"\"", "\"Hi\""]);
    match r.below(14) {
        0 => p(format!("Lf({k})")),
        1 => p(format!("Br({s}, {j})")),
        2 => p(format!("Lf({k}).d")),
        3 => p(format!("Lf({k}).depth()")),
        4 => p(format!("Lf({k}) == Lf({k})")),
        5 => p(format!("Lf({k}) == Lf({j})")),
        6 => p(format!("Lf({k}).hashCode() == Lf({k}).hashCode()")),
        7 => p(format!("Lf({k}).copy({j})")),
        8 => p(format!("Br({s}, {j}).copy({s}, {k})")),
        9 => format!("val (a{idx}, b{idx}) = Br({s}, {j}); println(\"$a{idx}|$b{idx}\")"),
        10 => p(format!("listOf(Lf({k}), Br({s}, {j}))")),
        11 => p(format!("setOf(Lf({k}), Lf({k}), Lf({j}))")),
        // `copy` re-runs the superclass header, so the base field follows the
        // NEW constructor argument, not the receiver's old one.
        12 => format!("val w{idx} = Wd({s}).copy(\"zzzz\"); println(w{idx}); println(w{idx}.d)"),
        _ => format!(
            "val n{idx}: Nd = Lf({k}); println(n{idx} is Lf); println(n{idx} is Nd); println(n{idx})"
        ),
    }
}

/// `Set` and the collection operations layered on an `Iterable`: the ordering a
/// `LinkedHashSet` preserves, the de-duplication `setOf`/`toSet`/`distinct`
/// perform, the order-insensitive `Set` equality, the set operators, and the
/// lambda-taking `associate`/`sorted*`/`flatMap` family.
///
/// Determinism: `setOf` is a `LinkedHashSet`, so iteration and display follow
/// insertion order and are reproducible; a `HashSet` would not be. Element
/// counts stay small so a printed collection stays short.
fn g_coll(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, &["0", "1", "2", "3", "5", "7"]);
    let b = pick(r, &["1", "2", "3", "5", "8"]);
    let c = pick(r, &["0", "2", "3", "9"]);
    let set = format!("setOf({a}, {b}, {c}, {a})");
    let list = format!("listOf({a}, {b}, {c}, {b})");
    match r.below(18) {
        0 => p(set),
        1 => p(format!("{set}.size")),
        2 => p(format!("{b} in {set}")),
        3 => p(format!("{set}.toList()")),
        4 => p(format!("{list}.toSet()")),
        5 => p(format!("{list}.distinct()")),
        6 => p(format!("setOf({a}, {b}) == setOf({b}, {a})")),
        7 => p(format!("setOf({a}, {b}) == setOf({a}, {c})")),
        8 => p(format!("{set}.union(setOf({c}, {b}))")),
        9 => p(format!("{set}.intersect(setOf({c}, {b}))")),
        10 => p(format!("{set}.subtract(setOf({b}))")),
        11 => p(format!("{list}.sorted()")),
        12 => p(format!("{list}.sortedDescending()")),
        13 => p(format!("{list}.take({a})")),
        14 => p(format!("{list}.drop({a})")),
        15 => p(format!("{list}.associate {{ it to it * 2 }}")),
        16 => p(format!("{list}.flatMap {{ listOf(it, it + 1) }}")),
        _ => format!(
            "val cl{idx} = {list}; println(cl{idx}.mapIndexed {{ i, x -> i * x }}); \
             println(cl{idx}.filterNot {{ it > {b} }}); println(cl{idx}.none {{ it > 99 }}); \
             println(cl{idx}.minByOrNull {{ it }}); println(cl{idx}.sortedByDescending {{ it }}); \
             println(cl{idx}.associateBy {{ it % 2 }})"
        ),
    }
}

/// Magnitudes that STRADDLE `Int` range: a product or sum of two of these
/// overflows 32 bits, so the probe answers a wrapped value on Kotlin and a
/// 64-bit one on any frontend that skipped the narrowing.
const BIGINTS: &[&str] = &[
    "100000",
    "70000",
    "2000000000",
    "-2000000000",
    "65536",
    "46341",
];

/// The `Int` wraparound that reaches an operand whose type is not written down:
/// a lambda parameter, a `for` variable, an element read back out of a
/// sequence. Kotlin decides arithmetic width from the STATIC type, so every
/// probe here has a right answer the source spells out — but only if the
/// frontend propagates the receiver's element type into the lambda instead of
/// treating an untyped parameter as possibly-`Long`.
///
/// The `Long` receivers are deliberately mixed in: they must NOT narrow, so a
/// frontend that fixes the `Int` case by always narrowing fails these instead.
fn g_width(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, BIGINTS);
    let b = pick(r, BIGINTS);
    let op = pick(r, AOPS);
    match r.below(16) {
        0 => p(format!("listOf({a}).map {{ it {op} {b} }}")),
        1 => p(format!("listOf({a}, {b}).map {{ it * it }}")),
        2 => p(format!("listOf({a}L).map {{ it {op} {b} }}")),
        3 => p(format!("listOf({a}).sumOf {{ it {op} {b} }}")),
        4 => p(format!("listOf({a}).filter {{ it {op} {b} > 0 }}")),
        5 => p(format!("listOf({a}).fold(1) {{ acc, x -> acc {op} x }}")),
        6 => p(format!("listOf({a}).fold(1L) {{ acc, x -> acc {op} x }}")),
        7 => p(format!("listOf({a}, {b}).reduce {{ x, y -> x {op} y }}")),
        8 => p(format!("listOf({a}).mapIndexed {{ i, x -> x {op} x + i }}")),
        9 => p(format!("(1..3).map {{ it * {a} }}")),
        10 => format!(
            "var w{idx} = 0; for (x in listOf({a}, {b})) w{idx} = w{idx} {op} x; println(w{idx})"
        ),
        11 => format!(
            "val wl{idx} = listOf({a}); println(wl{idx}.map {{ it {op} {b} }}); println(wl{idx}.first() * wl{idx}.first())"
        ),
        12 => format!(
            "val wf{idx}: (Int) -> Int = {{ it {op} {b} }}; println(wf{idx}({a}))"
        ),
        13 => format!(
            "val wg{idx}: (Long) -> Long = {{ it {op} {b}L }}; println(wg{idx}({a}L))"
        ),
        14 => p(format!("listOf(listOf({a})).map {{ g -> g.map {{ it * it }} }}")),
        _ => p(format!("listOf({a}).windowed(1) {{ it.first() * it.first() }}")),
    }
}

/// `hashCode()` across the whole value model. Every kind here has a hash the
/// JVM specifies EXACTLY — `Int` is the value, `Long` folds its halves,
/// `String` is the `31`-polynomial, a `List` folds, a `Set` and a `Map` sum —
/// so the expected value is reproducible rather than an identity.
///
/// The identity-hashed kinds (a non-`data` class, an array, a lambda) are
/// excluded: the JVM's own answer varies per run, so no frontend can match one.
fn g_hash(r: &mut Rng, _idx: usize) -> String {
    let a = pick(r, INTS);
    let b = pick(r, STRS);
    let d = pick(r, DBLS);
    let c = pick(r, &["'a'", "'Z'", "'0'", "' '"]);
    match r.below(16) {
        0 => p(format!("{b}.hashCode()")),
        1 => p(format!("({a}).hashCode()")),
        2 => p(format!("({a}L).hashCode()")),
        3 => p(format!("({d}).hashCode()")),
        4 => p(format!("{c}.hashCode()")),
        5 => p(format!("{}.hashCode()", pick(r, BOOLS))),
        6 => p(format!("listOf({a}, {b}).hashCode()")),
        7 => p("listOf<Int>().hashCode()".to_string()),
        8 => p(format!("setOf({a}, {}).hashCode()", pick(r, INTS))),
        9 => p(format!("mapOf({a} to {b}).hashCode()")),
        10 => p(format!("({a} to {b}).hashCode()")),
        11 => p(format!("Pt({a}, {b}).hashCode()")),
        12 => p(format!(
            "Pt({a}, {b}).hashCode() == Pt({a}, {b}).hashCode()"
        )),
        13 => p(format!(
            "({}..{}).hashCode()",
            pick(r, RINTS),
            pick(r, RINTS)
        )),
        14 => p(format!(
            "({}..{} step {}).hashCode()",
            pick(r, RINTS),
            pick(r, RINTS),
            pick(r, STEPS)
        )),
        _ => p(format!(
            "({} downTo {}).hashCode()",
            pick(r, RINTS),
            pick(r, RINTS)
        )),
    }
}

/// `Map.Entry` — the type a `Map` iterates as. It is NOT a `Pair`, and the
/// three places that differ are all silent when one is used for the other: an
/// entry renders `k=v` where a pair renders `(k, v)`, its hash is `key xor
/// value` where a pair's folds, and the two are never equal.
fn g_entry(r: &mut Rng, idx: usize) -> String {
    let k = pick(r, INTS);
    let v = pick(r, STRS);
    let k2 = pick(r, INTS);
    let v2 = pick(r, STRS);
    let m = format!("mapOf({k} to {v}, {k2} to {v2})");
    match r.below(16) {
        0 => p(format!("{m}.entries")),
        1 => p(format!("{m}.entries.first()")),
        2 => p(format!("{m}.entries.first().hashCode()")),
        3 => p(format!("{m}.entries.first() == ({k} to {v})")),
        4 => p(format!("{m}.entries.joinToString()")),
        5 => p(format!("{m}.entries.map {{ it.key }}")),
        6 => p(format!("{m}.entries.map {{ it.value }}")),
        7 => p(format!("{m}.map {{ \"${{it.key}}=${{it.value}}\" }}")),
        8 => p(format!("{m}.mapValues {{ it.value + \"!\" }}")),
        9 => p(format!("{m}.mapKeys {{ it.key * 2 }}")),
        // `keys` and `entries` are SETS: their hash is the SUM of the element
        // hashes and their equality ignores order, neither of which a list-like
        // result reproduces. (`values` is deliberately absent — it is a plain
        // `Collection` whose hash and equality the JVM leaves as identity, so no
        // frontend can match it.)
        10 => p(format!("{m}.keys.hashCode()")),
        11 => p(format!("{m}.entries.hashCode()")),
        12 => p(format!("{m}.keys == setOf({k2}, {k})")),
        13 => p(format!("{m}.keys")),
        14 => format!("for (e{idx} in {m}) print(e{idx}); println()"),
        _ => format!("for ((ek{idx}, ev{idx}) in {m}) print(\"$ek{idx}/$ev{idx} \"); println()"),
    }
}

/// The `String` search and comparison members in their OVERLOADED spellings —
/// the `startIndex` and `ignoreCase` arguments. Each has a shorter form that
/// already worked, so dropping the extra argument re-runs the default silently:
/// `"abc".indexOf("b", 2)` answers 1 instead of -1.
fn g_strsearch(r: &mut Rng, _idx: usize) -> String {
    let hay = pick(
        r,
        &["\"abcabc\"", "\"aaa\"", "\"\"", "\"Hello\"", "\"abc\""],
    );
    let needle = pick(
        r,
        &["\"a\"", "\"bc\"", "\"\"", "\"z\"", "\"ABC\"", "\"abc\""],
    );
    let at = pick(r, &["0", "1", "2", "3", "9", "-1"]);
    match r.below(10) {
        0 => p(format!("{hay}.indexOf({needle})")),
        1 => p(format!("{hay}.indexOf({needle}, {at})")),
        2 => p(format!("{hay}.lastIndexOf({needle})")),
        3 => p(format!("{hay}.lastIndexOf({needle}, {at})")),
        4 => p(format!("{hay}.startsWith({needle})")),
        5 => p(format!("{hay}.startsWith({needle}, {at})")),
        6 => p(format!("{hay}.compareTo({needle})")),
        7 => p(format!("{hay}.compareTo({needle}, true)")),
        8 => p(format!("{hay}.equals({needle}, true)")),
        _ => p(format!("{hay}.replaceFirst({needle}, \"-\")")),
    }
}

/// The collection members whose LATER arguments are easy to drop: an affix, a
/// step, a `partialWindows` flag, a transform. Every one of them has a shorter
/// overload that behaves differently, which is what makes a dropped argument a
/// wrong answer rather than an error.
fn g_collarg(r: &mut Rng, _idx: usize) -> String {
    let xs = pick(
        r,
        &[
            "listOf(1, 2, 3, 4, 5)",
            "listOf(1)",
            "listOf<Int>()",
            "listOf(3, 1, 2)",
        ],
    );
    let n = pick(r, &["1", "2", "3", "5"]);
    let step = pick(r, &["1", "2", "3"]);
    match r.below(14) {
        0 => p(format!("{xs}.joinToString(\"-\")")),
        1 => p(format!("{xs}.joinToString(\"-\", \"<\")")),
        2 => p(format!("{xs}.joinToString(\"-\", \"<\", \">\")")),
        3 => p(format!(
            "{xs}.joinToString(\"-\", \"<\", \">\", {n}, \"~\")"
        )),
        4 => p(format!("{xs}.joinToString(\"-\") {{ \"v$it\" }}")),
        5 => p(format!(
            "{xs}.joinToString(\"-\", \"<\", \">\") {{ \"v$it\" }}"
        )),
        6 => p(format!("{xs}.chunked({n})")),
        7 => p(format!("{xs}.chunked({n}) {{ it.sum() }}")),
        8 => p(format!("{xs}.windowed({n})")),
        9 => p(format!("{xs}.windowed({n}, {step})")),
        10 => p(format!("{xs}.windowed({n}, {step}, true)")),
        11 => p(format!("{xs}.windowed({n}) {{ it.sum() }}")),
        12 => p(format!("{xs}.zip(listOf(9, 8, 7)) {{ a, b -> a * b }}")),
        // `slice` is given a receiver long enough for the range it is handed.
        // Drawn from the shared pool it could be `listOf(1)` or an empty list,
        // and `slice(0..1)` on those THROWS — which aborts the reference run
        // partway and made the whole 40-probe batch barren, every probe after
        // it uncompared. The throwing path belongs in `exc` mode, where a
        // `try`/`catch` keeps the batch alive.
        _ => p(format!("listOf(3, 1, 2).slice(0..{})", r.below(3))),
    }
}

/// The searching predicates. Each shares its NAME with a no-argument member, so
/// a frontend that routes on the name alone runs the wrong one and answers the
/// first/last element rather than the first/last MATCH — silently, and only for
/// inputs where the two differ.
///
/// The receivers always contain a match for the `first`/`last`/`single` forms,
/// which throw on none; the `…OrNull` forms are probed with a predicate that
/// matches nothing so the null path is covered too.
fn g_predicate(r: &mut Rng, _idx: usize) -> String {
    let xs = "listOf(1, 2, 3, 4)";
    match r.below(12) {
        0 => p(format!("{xs}.first {{ it > 1 }}")),
        1 => p(format!("{xs}.last {{ it < 4 }}")),
        2 => p(format!("{xs}.find {{ it > 2 }}")),
        3 => p(format!("{xs}.find {{ it > 99 }}")),
        4 => p(format!("{xs}.findLast {{ it < 3 }}")),
        5 => p(format!("{xs}.single {{ it == 3 }}")),
        6 => p(format!("{xs}.singleOrNull {{ it > 2 }}")),
        7 => p(format!("{xs}.indexOfFirst {{ it > 2 }}")),
        8 => p(format!("{xs}.indexOfLast {{ it < 3 }}")),
        9 => p(format!("{xs}.indexOfFirst {{ it > 99 }}")),
        10 => p(format!("{xs}.filterIndexed {{ i, x -> i == x }}")),
        _ => p(format!("{xs}.maxOf {{ -it }}")),
    }
}

/// `body` — properties declared in a class BODY rather than the primary
/// constructor, and the `companion object` that reaches a class's members
/// without an instance.
///
/// The silent difference this hunts is what a `data class`'s generated members
/// see: Kotlin derives `toString`/`equals`/`hashCode`/`componentN` from the
/// PRIMARY CONSTRUCTOR alone, so a body property is stored, readable, and
/// printable — and still absent from every derived member. A frontend that
/// appends body properties to the same field record answers `DBody(a=1, extra=2)`
/// and compares `extra` too, which is wrong in a way nothing else reveals.
fn g_body(r: &mut Rng, _idx: usize) -> String {
    let k = pick(r, RINTS);
    match r.below(14) {
        0 => p(format!("Acc({k}).bump()")),
        1 => p(format!("Acc({k}).doubled")),
        2 => p(format!("Acc({k}).tot()")),
        // Two bumps on ONE instance: a body property is per-instance state, so a
        // frontend that built it once (as an `object`'s is) answers 1 twice.
        3 => format!("run {{ val a = Acc({k}); a.bump(); println(a.bump()) }}"),
        4 => format!("run {{ val a = Acc({k}); a.bump(); println(a.tot()) }}"),
        5 => p("Acc.ZERO".to_string()),
        6 => p(format!("Acc.of({k}).doubled")),
        7 => p(format!("Acc.of({k}).n")),
        8 => p("Acc.describe()".to_string()),
        9 => p(format!("DBody({k})")),
        10 => p(format!("DBody({k}) == DBody({k})")),
        11 => p(format!("DBody({k}).hashCode()")),
        12 => p(format!("DBody({k}).extra")),
        _ => p(format!("DBody({k}).copy(a = {}).extra", pick(r, RINTS))),
    }
}

/// `ext` — extension functions. A `fun Int.dbl()` is dispatched by the
/// receiver's STATIC type, so `Int` and `Long` versions of one name must stay
/// apart even though both receivers are one `i64` at runtime — and the `Int`
/// one's arithmetic still has to wrap at 32 bits, which is only knowable from
/// the declared receiver.
fn g_ext(r: &mut Rng, _idx: usize) -> String {
    match r.below(12) {
        0 => p(format!("{}.dbl()", pick(r, INTS))),
        // The width probe: `Int.dbl` wraps, `Long.dbl` does not, from the same
        // written body.
        1 => p("2000000000.dbl()".to_string()),
        2 => p("2000000000L.dbl()".to_string()),
        3 => p(format!("{}L.dbl()", pick(r, INTS))),
        4 => p(format!("{}.shout()", pick(r, STRS))),
        5 => p(format!("{}.rep({})", pick(r, STRS), pick(r, STEPS))),
        6 => p(format!("Pt({}, {}).label()", pick(r, INTS), pick(r, STRS))),
        7 => p(format!("{}.half()", pick(r, DBLS))),
        8 => p(format!("{}.plusN()", pick(r, INTS))),
        9 => p(format!("{}.plusN({})", pick(r, INTS), pick(r, INTS))),
        // An extension calling another on the same receiver, unqualified.
        10 => p(format!("{}.quad()", pick(r, INTS))),
        _ => p(format!("{}.shout().length", pick(r, STRS))),
    }
}

/// `scope` — the scope functions. They split into the `it`-form
/// (`let`/`also`/`takeIf`/`takeUnless`) and the `this`-form (`run`/`apply`/
/// `with`), and the two differ in exactly one place a frontend can get wrong
/// silently: whether an unqualified name inside the block reads a member of the
/// receiver or an enclosing binding. `apply` additionally yields the RECEIVER
/// where `run` yields the block, so confusing them answers a plausible value of
/// the wrong thing.
fn g_scope(r: &mut Rng, _idx: usize) -> String {
    let s = pick(r, STRS);
    let n = pick(r, INTS);
    let k = pick(r, RINTS);
    match r.below(16) {
        0 => p(format!("{s}.run {{ length }}")),
        1 => p(format!("{s}.run {{ uppercase() + length }}")),
        2 => p(format!("with({s}) {{ length * 2 }}")),
        3 => p(format!("with({s}) {{ this + \"!\" }}")),
        // The receiver is parenthesized because `-1.takeIf { … }` parses as
        // `-(1.takeIf { … })` in Kotlin, and negating the resulting `Int?` is a
        // type error there — a program the reference toolchain rejects is not a
        // parity signal.
        4 => p(format!("({n}).let {{ it * it }}")),
        5 => p(format!("({n}).also {{ it }}")),
        6 => p(format!("({n}).takeIf {{ it > 0 }}")),
        7 => p(format!("({n}).takeUnless {{ it > 0 }}")),
        8 => p("run { 1 + 2 }".to_string()),
        9 => p(format!("Acc({k}).apply {{ n = {} }}.tot()", pick(r, RINTS))),
        10 => p(format!("Acc({k}).run {{ tot() + doubled }}")),
        11 => p(format!("Acc({k}).also {{ it.bump() }}.n")),
        12 => p(format!("listOf({k}, {}).run {{ size }}", pick(r, RINTS))),
        13 => p(format!(
            "listOf({k}, {}).let {{ it.sum() }}",
            pick(r, RINTS)
        )),
        // Width inside a receiver block: the receiver's declared type has to
        // reach the block's arithmetic.
        14 => p("2000000000.let { it + it }".to_string()),
        _ => p("2000000000L.let { it + it }".to_string()),
    }
}

/// `params` — default, named, and `vararg` parameters. Each is a way for the
/// ARGUMENT LIST at a call site not to match the parameter list, and the wrong
/// answers are quiet: a dropped default binds nothing, a named argument bound
/// positionally swaps two values of the same type, and a `vararg` that collects
/// the wrong tail changes a sum rather than failing.
fn g_params(r: &mut Rng, _idx: usize) -> String {
    let a = pick(r, RINTS);
    let b = pick(r, RINTS);
    let c = pick(r, RINTS);
    match r.below(14) {
        0 => p(format!("pad({})", pick(r, STRS))),
        1 => p(format!("pad({}, {})", pick(r, STRS), pick(r, STEPS))),
        2 => p(format!("pad({}, {}, \"*\")", pick(r, STRS), pick(r, STEPS))),
        3 => p(format!("pad(sep = \"*\", s = {})", pick(r, STRS))),
        4 => p(format!("pad({}, sep = \"+\")", pick(r, STRS))),
        5 => p("total()".to_string()),
        6 => p(format!("total({a})")),
        7 => p(format!("total({a}, {b}, {c})")),
        8 => p(format!("mixed({a})")),
        9 => p(format!("mixed({a}, {b}, {c})")),
        10 => p("Cfg().b".to_string()),
        11 => p(format!("Cfg({a}).a")),
        12 => p(format!("Cfg(b = \"z\").a + Cfg({a}).a")),
        _ => p(format!("Cfg({a}, \"q\")")),
    }
}

/// `tuple` — `Pair` and `Triple`. Both are `data class`es whose display is
/// `(a, b)` rather than `Name(x=…)`, and whose `hashCode` is the `data class`
/// fold — three separate places a frontend that reuses another heap kind for
/// them answers something plausible and wrong.
fn g_tuple(r: &mut Rng, _idx: usize) -> String {
    let a = pick(r, RINTS);
    let b = pick(r, RINTS);
    let s = pick(r, STRS);
    match r.below(14) {
        0 => p(format!("Pair({a}, {s})")),
        1 => p(format!("Triple({a}, {b}, {s})")),
        2 => p(format!("Pair({a}, {b}).first + Pair({a}, {b}).second")),
        3 => p(format!("Triple({a}, {b}, {a}).third")),
        4 => p(format!("Pair({a}, {s}) == Pair({a}, {s})")),
        5 => p(format!("Triple({a}, {b}, {s}) == Triple({a}, {b}, {s})")),
        6 => p(format!("Triple({a}, {b}, {s}) == Triple({b}, {a}, {s})")),
        7 => p(format!("Pair({a}, {b}).hashCode()")),
        8 => p(format!("Triple({a}, {b}, {a}).hashCode()")),
        // A `Pair` and a `Triple` are NOT interchangeable with the `to` form or
        // with a `Map.Entry`.
        9 => p(format!("({a} to {b}) == Pair({a}, {b})")),
        10 => format!("run {{ val (x, y, z) = Triple({a}, {b}, {s}); println(\"\" + x + y + z) }}"),
        11 => p(format!("listOf(Pair({a}, {b}), Pair({b}, {a}))")),
        12 => p(format!("Triple({a}, {b}, {s}).component2()")),
        _ => p(format!("listOf(Triple({a}, {b}, {a})).first().second")),
    }
}

/// `capture` — a `var` of the enclosing frame that a lambda ASSIGNS to.
///
/// A closure copies its captures by value, so the write has to reach shared
/// storage or the enclosing frame keeps the pre-lambda value — a wrong number,
/// not an error. The `Int` cases additionally require the boxed value to keep
/// its declared width, so the accumulation still wraps at 32 bits.
fn g_capture(r: &mut Rng, _idx: usize) -> String {
    let xs = format!(
        "listOf({}, {}, {})",
        pick(r, RINTS),
        pick(r, RINTS),
        pick(r, RINTS)
    );
    match r.below(10) {
        0 => format!("run {{ var n = 0; {xs}.forEach {{ n += it }}; println(n) }}"),
        1 => format!("run {{ var n = 1; {xs}.forEach {{ n *= it }}; println(n) }}"),
        2 => format!("run {{ var s = \"\"; {xs}.forEach {{ s = s + it }}; println(s) }}"),
        3 => format!("run {{ var c = 0; {xs}.forEach {{ if (it > 0) c++ }}; println(c) }}"),
        4 => format!("run {{ var d = 0.0; {xs}.forEach {{ d += it }}; println(d) }}"),
        5 => format!("run {{ var n = 2000000000; {xs}.forEach {{ n += n }}; println(n) }}"),
        6 => format!("run {{ var n = 2000000000L; {xs}.forEach {{ n += n }}; println(n) }}"),
        7 => format!("run {{ var n = 0; {xs}.map {{ n += it; n }}.forEach {{ println(it) }} }}"),
        8 => format!(
            "run {{ var n = 0; for (i in 1..3) {{ {xs}.forEach {{ n += it }} }}; println(n) }}"
        ),
        _ => {
            format!("run {{ var n = 0; {xs}.let {{ l -> l.forEach {{ n += it }} }}; println(n) }}")
        }
    }
}

/// `cast` — `x as T` and `x as? T`. The cast changes no representation; what it
/// supplies is the STATIC type, which then decides `/` dispatch and integer
/// width downstream. The failure paths differ (`ClassCastException` vs null),
/// and `as? String` yielding null has to PRINT as `null` rather than as the
/// empty string a non-null String coercion would give.
fn g_cast(r: &mut Rng, _idx: usize) -> String {
    let i = r.below(3);
    let any = format!("anyAt({i})");
    match r.below(12) {
        0 => p(format!("({any} as? Int)")),
        1 => p(format!("({any} as? String)")),
        2 => p(format!("({any} as? Double)")),
        3 => p("(anyAt(0) as Int) + 1".to_string()),
        4 => p("anyAt(0) as Int / 2".to_string()),
        5 => p("(anyAt(2) as Double) / 2".to_string()),
        6 => p("(anyAt(1) as String).length".to_string()),
        7 => p("(anyAt(1) as String) + \"!\"".to_string()),
        8 => p("(bigAny() as Int) + (bigAny() as Int)".to_string()),
        9 => p("((bigAny() as Int).toLong()) + (bigAny() as Int)".to_string()),
        10 => "try { println(anyAt(1) as Int) } catch (e: ClassCastException) { println(\"cce\") }"
            .to_string(),
        _ => p(format!("({any} as? Int) ?: -1")),
    }
}

/// `lazyprop` — top-level properties and `by lazy`.
///
/// A top-level `val` initializes before `main`; a `by lazy` one does NOT — its
/// thunk runs at the first READ and caches. Evaluating it eagerly gives the same
/// value and a different program: the observable difference is WHEN the thunk's
/// output appears, and whether it appears twice.
fn g_lazyprop(r: &mut Rng, _idx: usize) -> String {
    let k = pick(r, RINTS);
    match r.below(10) {
        0 => p("GK".to_string()),
        1 => p("GK * 2".to_string()),
        2 => p("GNAME.uppercase()".to_string()),
        3 => p("GDERIVED".to_string()),
        4 => format!("run {{ GC = {k}; println(GC) }}"),
        5 => format!("run {{ GC = {k}; GC += 1; println(GC) }}"),
        6 => p(format!("Lz({k}).v")),
        // The forcing-order probe: a fresh instance per probe keeps it local,
        // and the thunk's print must land between the two markers.
        7 => format!(
            "run {{ val z = Lz({k}); println(\"a\"); println(z.v); println(z.v); println(\"b\") }}"
        ),
        8 => p(format!("Lz({k}).v + Lz({k}).v")),
        _ => p(format!("Lz({k}).doubled()")),
    }
}

/// `result` — `runCatching` and the `Result` it yields.
///
/// `Result` is a union that renders as `Success(v)` / `Failure(<throwable>)`,
/// and every reader of it is total: `getOrNull` is null on failure,
/// `exceptionOrNull` is null on success, `map` transforms only a success. A
/// frontend that lets the exception escape, or that reports success for a
/// caught throw, answers the wrong branch rather than failing.
fn g_result(r: &mut Rng, _idx: usize) -> String {
    let n = pick(r, RINTS);
    let block = if r.below(2) == 0 {
        format!("rboom({n})")
    } else {
        "rboom(-1)".to_string()
    };
    match r.below(12) {
        0 => p(format!("runCatching {{ {block} }}")),
        1 => p(format!("runCatching {{ {block} }}.isSuccess")),
        2 => p(format!("runCatching {{ {block} }}.isFailure")),
        3 => p(format!("runCatching {{ {block} }}.getOrNull()")),
        4 => p(format!("runCatching {{ {block} }}.exceptionOrNull()")),
        5 => p(format!("runCatching {{ {block} }}.getOrElse {{ -1 }}")),
        6 => p(format!("runCatching {{ {block} }}.map {{ it + 1 }}")),
        7 => p(format!("runCatching {{ {block} }}.getOrNull() ?: -7")),
        8 => p("runCatching { 1 / 0 }.isFailure".to_string()),
        9 => p("runCatching { listOf(1).first { it > 9 } }.isFailure".to_string()),
        10 => format!("runCatching {{ {block} }}.onSuccess {{ println(\"s\" + it) }}"),
        _ => format!("runCatching {{ {block} }}.onFailure {{ println(\"f\") }}"),
    }
}

/// `localfn` — a `fun` declared inside another function's body. It is a
/// subroutine rather than a closure value, which is what lets it recurse; the
/// probes exercise the recursion, the defaults it still gets, and the shadowing
/// of a top-level function of the same name.
fn g_localfn(r: &mut Rng, _idx: usize) -> String {
    let n = pick(r, STEPS);
    match r.below(8) {
        0 => p(format!("withLocal({n})")),
        1 => p(format!("localFact({n})")),
        2 => p(format!("localFib({n})")),
        3 => p(format!("localDefault({n})")),
        4 => p("localDefaultBare()".to_string()),
        5 => p(format!("localShadow({n})")),
        6 => p(format!("localInLambda({n})")),
        _ => p(format!("localNested({n})")),
    }
}

/// `ctor` — secondary constructors and `init`-block ORDERING.
///
/// The gap this hunts is not "does an object come out" but WHEN each piece
/// runs: Kotlin interleaves the property initializers with the `init` blocks in
/// declaration order, and a secondary constructor's body runs only after the
/// constructor it delegates to has finished — including that constructor's own
/// body when the delegation chains. Every probe therefore PRINTS from inside
/// the initializers, so the ordering is in the output rather than only in the
/// final field values, and the chained forms (`constructor() : this(9)`) are
/// generated as often as the direct ones.
fn g_ctor(r: &mut Rng, _idx: usize) -> String {
    let n = pick(r, STEPS);
    // Never `println` a bare instance: `Ord` is not a `data class`, so the JVM
    // gives it `Object.toString` and the output carries an identity hash, which
    // is exactly the nondeterminism this harness excludes. Each probe reads a
    // field or calls a method instead — the `init`/constructor bodies still
    // print, so the ORDERING under test is unaffected.
    match r.below(9) {
        0 => p(format!("Ord({n}).sum")),
        1 => p(format!("Ord({n}, {n}).sum")),
        2 => p("Ord().sum".to_string()),
        3 => p(format!("Ord({n}).seen")),
        4 => p(format!("NoPrim({n}).total")),
        5 => p("NoPrim().total".to_string()),
        6 => p(format!("SubOrd({n}).tag")),
        7 => p(format!("SubOrd({n}, {n}).tag")),
        _ => p(format!("Pick({n}).show()")),
    }
}

/// `deleg` — interface delegation, exercised through the DELEGATED CALLS.
///
/// Constructing a `class C(x: I) : I by x` proves nothing; the divergence lives
/// in which implementation a call reaches. A default method of the delegated
/// interface runs on the DELEGATE, so it calls the delegate's implementation of
/// an abstract member even when the delegating class overrides it — the one
/// result a "forward only the abstract members" lowering gets wrong. Probes
/// therefore call the plain member, the defaulted member, and the overridden
/// member on the same shapes, and also through a supertype-typed binding.
fn g_deleg(r: &mut Rng, _idx: usize) -> String {
    let n = pick(r, STEPS);
    match r.below(8) {
        0 => p(format!("Fwd(Base1({n})).one()")),
        1 => p(format!("Fwd(Base1({n})).both()")),
        2 => p(format!("Over(Base1({n})).one()")),
        3 => p(format!("Over(Base1({n})).both()")),
        4 => p(format!("Two(Base1({n}), TwoB({n})).one()")),
        5 => p(format!("Two(Base1({n}), TwoB({n})).two()")),
        6 => p(format!("asOne(Fwd(Base1({n}))).both()")),
        _ => p(format!("asOne(Over(Base1({n}))).both()")),
    }
}

/// `invoke` — invoking the result of a call, `f()()`.
///
/// The lambda a probe invokes CAPTURES wherever it can: a non-capturing
/// `{ 42 }` survives a lowering that loses the closure environment, so it would
/// hide the bug a capturing one exposes. Three-deep chains, invocation off an
/// index and off a lambda literal, and a class with `operator fun invoke` are
/// all generated, because each reaches the postfix `(` from a different
/// preceding form.
fn g_invoke(r: &mut Rng, _idx: usize) -> String {
    let n = pick(r, STEPS);
    match r.below(9) {
        0 => p(format!("adder({n})({n})")),
        1 => p(format!("mulBy({n})({n})")),
        2 => p(format!("twice({n})()()")),
        3 => p(format!("fnList({n})[0]({n})")),
        4 => p(format!("fnList({n})[1]({n})")),
        5 => p(format!("{{ x: Int -> x - {n} }}({n})")),
        6 => p(format!("Boxed({n})({n})")),
        7 => p(format!("Boxed({n}).invoke({n})")),
        _ => p(format!("adder({n}).invoke({n})")),
    }
}

/// `strcoll` — the collection API on a `String` receiver.
///
/// The divergence here is the RESULT TYPE, not whether the call resolves:
/// `kotlin.text` gives `"abc".filter { … }` a `String` where the `Iterable`
/// overload would give a `List<Char>`, while `"abc".map { … }` keeps the
/// `List`. A lowering that materializes the characters and reuses the list
/// implementation wholesale is right for `map` and wrong for `filter`, and only
/// printing the result tells them apart. The empty and single-character
/// receivers are generated as often as the longer ones, because that is where
/// `first`/`reduce`/`windowed` change answer or throw.
fn g_strcoll(r: &mut Rng, _idx: usize) -> String {
    let subj = pick(r, &["\"\"", "\"a\"", "\"abc\"", "\"aabbc\"", "\"hello\""]);
    match r.below(20) {
        0 => p(format!("{subj}.map {{ it }}")),
        1 => p(format!("{subj}.map {{ it.code }}")),
        2 => p(format!("{subj}.filter {{ it != 'l' }}")),
        3 => p(format!("{subj}.filterNot {{ it == 'a' }}")),
        4 => p(format!("{subj}.filterIndexed {{ i, c -> i % 2 == 0 }}")),
        5 => p(format!("{subj}.sumOf {{ it.code }}")),
        6 => p(format!("{subj}.groupBy {{ it }}")),
        7 => p(format!("{subj}.count {{ it > 'a' }}")),
        8 => p(format!("{subj}.any {{ it == 'b' }}")),
        9 => p(format!("{subj}.all {{ it > 'A' }}")),
        10 => p(format!("{subj}.takeWhile {{ it < 'c' }}")),
        11 => p(format!("{subj}.dropWhile {{ it < 'c' }}")),
        12 => p(format!("{subj}.partition {{ it < 'b' }}")),
        13 => p(format!("{subj}.chunked(2)")),
        14 => p(format!("{subj}.windowed(2)")),
        15 => p(format!("{subj}.zip(\"xy\")")),
        16 => p(format!("{subj}.withIndex().toList()")),
        17 => p(format!("{subj}.associateWith {{ it.code }}")),
        18 => p(format!("{subj}.mapIndexed {{ i, c -> \"\" + i + c }}")),
        _ => p(format!("{subj}.onEach {{ }}")),
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
    Char,
    NullSafe,
    DataWhen,
    Class,
    DataInherit,
    Coll,
    DoWhile,
    StrFmt,
    Bitwise,
    SafeCall,
    MapColl,
    FinExit,
    Width,
    Hash,
    Entry,
    StrSearch,
    CollArg,
    Predicate,
    Body,
    Ext,
    Scope,
    Params,
    Tuple,
    Capture,
    Cast,
    LazyProp,
    Result,
    LocalFn,
    Ctor,
    Deleg,
    Invoke,
    StrColl,
    Equality,
    Operator,
    Generic,
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
    Mode::Char,
    Mode::NullSafe,
    Mode::DataWhen,
    Mode::Class,
    Mode::DataInherit,
    Mode::Coll,
    Mode::DoWhile,
    Mode::StrFmt,
    Mode::Bitwise,
    Mode::SafeCall,
    Mode::MapColl,
    Mode::FinExit,
    Mode::Width,
    Mode::Hash,
    Mode::Entry,
    Mode::StrSearch,
    Mode::CollArg,
    Mode::Predicate,
    Mode::Body,
    Mode::Ext,
    Mode::Scope,
    Mode::Params,
    Mode::Tuple,
    Mode::Capture,
    Mode::Cast,
    Mode::LazyProp,
    Mode::Result,
    Mode::LocalFn,
    Mode::Ctor,
    Mode::Deleg,
    Mode::Invoke,
    Mode::StrColl,
    Mode::Equality,
    Mode::Operator,
    Mode::Generic,
];

/// The **operator conventions** on a collection receiver, plus the iteration
/// order of the JVM collections and the `Grouping` terminal operations.
///
/// The generator reached NONE of this: it produced the arithmetic operators
/// only between numbers and strings, and named
/// `hashMapOf`/`hashSetOf`/`sortedSetOf`/`groupingBy` not at all. So the
/// harness stayed clean at 240 probes while `listOf(1, 2, 3) - 2` evaluated to
/// `-2.0` and `hashMapOf` answered in insertion order. A clean number over a
/// surface the generator never visits proves nothing about that surface, which
/// is the reason this mode exists.
///
/// A user class declaring its own conventions is NOT generated here — the
/// `extra_declarations` preamble would have to carry an operator class per
/// probe shape. Those are pinned by the `lang` tests and the frozen corpus
/// instead.
///
/// Determinism: every probe prints a value whose order is fixed by the
/// collection's own discipline (bucket order for the hash kinds, ascending for
/// `sortedSetOf`, insertion for the rest), so repeated runs agree.
/// Calls whose result type is a TYPE VARIABLE — a generic function, a generic
/// class property, a generic method.
///
/// The generator reached none of this: it declared no `fun <T>` at all, so a
/// clean run said nothing about the surface while `gid(1) + gid(2)` failed
/// outright with an unresolved `plus`, and `gid(7) / gid(2)` answered `3.5`.
/// Kotlin resolves the type argument from the CALL SITE, so every probe here
/// has an answer the source spells out — the width, the division discipline and
/// the display all follow the argument that was passed in.
///
/// The `Double`, `Long` and `String` instantiations are deliberately mixed with
/// the `Int` ones: they must NOT truncate or wrap, so a frontend that fixes the
/// `Int` case by always narrowing fails these instead.
///
/// A generic CLASS is covered in every role its type argument can reach, because
/// each role resolves the argument from a different place: a stored property and
/// a `var` property off the CONSTRUCTION site, a method result and a computed
/// property off the RECEIVER, a nested instantiation off the argument's own
/// arguments, and a second type parameter off a different position of the same
/// argument list. A generator that declared only `class GBox<T>(val v: T)` said
/// nothing about any of the others.
///
/// The second SOURCE of a type argument is the one written down in SOURCE, and
/// it reaches places no construction site can: the result of a function whose
/// body the call site never sees (`fun mkInt(): GBox<Int>`), a parameter whose
/// caller is on the other side of the call (`fun takeInt(b: GBox<Int>)`), a
/// property of another class (`class GHold(val b: GBox<Int>)`), a supertype a
/// subclass with no type parameters of its own extends (`class GSub :
/// GOpen<Int>()`), a top-level `val`, a local annotation, and a cast. The
/// generator wrote NONE of them: every generic probe read its argument off a
/// construction, so a clean run said nothing about the annotation path at all.
///
/// Two further shapes the construction path itself did not reach: a SECONDARY
/// constructor's parameters (the primary's were the only ones matched, so a call
/// selecting a secondary resolved nothing) and a BODY property declared with a
/// type variable (`class GBox<T>(v: T) { val w: T = v }`), which is not a
/// constructor parameter.
///
/// Every one of them has a `Long` twin written the same way and at the same
/// magnitudes, which must NOT narrow — and a `String`/`Double` instantiation
/// that must reach neither the wrap nor the integer division — so a rule that
/// fixes the `Int` case by narrowing everything a written annotation touches
/// fails the twin instead of passing.
fn g_generic(r: &mut Rng, idx: usize) -> String {
    let big = pick(r, BIGINTS);
    let op = pick(r, AOPS);
    // The `Long` spelling of the same magnitudes: identical arithmetic that must
    // NOT wrap, so a probe pair over the two catches a rule that got the `Int`
    // case right by narrowing everything.
    let lbig = format!("{}L", pick(r, BIGINTS));
    let lbig2 = format!("{}L", pick(r, BIGINTS));
    match r.below(45) {
        0 => format!("println(gid({}) {op} gid({}))", pick(r, BIGINTS), big),
        1 => format!("println(gid({}) / gid({}))", pick(r, INTS), pick(r, DIVS)),
        2 => format!("println(gid({}) % gid({}))", pick(r, INTS), pick(r, DIVS)),
        3 => format!(
            "println(gid({}) {op} gid({}))",
            pick(r, DBLS),
            pick(r, DBLS)
        ),
        4 => format!("println(gid({}) + gid({}))", pick(r, STRS), pick(r, STRS)),
        5 => format!("println(-gid({}))", pick(r, BIGINTS)),
        // A stored `T`-typed property, read straight off the construction site.
        // `BIGINTS` on both sides, so the product leaves the `Int` range and the
        // answer differs between a frontend that carries the type argument and
        // one that does not.
        6 => format!("println(GBox({big}).v {op} GBox({}).v)", pick(r, BIGINTS)),
        7 => format!(
            "println(GBox({}).get() / GBox({}).get())",
            pick(r, BIGINTS),
            pick(r, DIVS)
        ),
        // The same two shapes at `Long` width. Nothing here may narrow, and the
        // magnitudes are the ones that WOULD change value if it did.
        8 => format!("println(GBox({lbig}).v {op} GBox({lbig2}).v)"),
        9 => format!("println(GBox({lbig}).get() {op} GBox({lbig2}).get())"),
        // A method result and a computed property: both read the type argument
        // off the RECEIVER rather than off an argument of their own.
        10 => format!(
            "println(GBox({big}).get() {op} GBox({}).get())",
            pick(r, BIGINTS)
        ),
        11 => format!(
            "println(GBox({big}).once {op} GBox({}).once)",
            pick(r, BIGINTS)
        ),
        12 => format!("println(GBox({lbig}).once {op} GBox({lbig2}).once)"),
        // A `var` property, whose type argument the construction site fixes even
        // though a later write is what supplies the value.
        13 => format!(
            "val gm{idx} = GMut({big}); gm{idx}.v = {}; println(gm{idx}.v {op} gm{idx}.v)",
            pick(r, BIGINTS)
        ),
        14 => format!(
            "val gl{idx} = GMut({lbig}); gl{idx}.v = {lbig2}; println(gl{idx}.v {op} gl{idx}.v)"
        ),
        // Two type parameters: each position of the argument list supplies a
        // different one, so a frontend that resolves only the first is wrong on
        // the second — and the mixed `Int`/`Long` pair below is wrong for a
        // frontend that resolves both to whatever the first argument was.
        15 => format!(
            "println(GTwo({big}, {}).a {op} GTwo({big}, {}).b)",
            pick(r, BIGINTS),
            pick(r, BIGINTS)
        ),
        16 => format!("println(GTwo({lbig}, {big}).a {op} GTwo({lbig}, {big}).b)"),
        // A nested instantiation — the inner argument is itself a generic type,
        // so the width lives two levels down.
        17 => format!(
            "println(GBox(GBox({big})).v.v {op} GBox({}).v)",
            pick(r, BIGINTS)
        ),
        18 => format!(
            "val gv{idx} = gfirst({}, {}); println(gv{idx} {op} {})",
            pick(r, INTS),
            pick(r, INTS),
            pick(r, INTS)
        ),
        19 => format!(
            "println(\"\" + gid({}) + \"|\" + gid({}))",
            pick(r, DBLS),
            pick(r, INTS)
        ),
        // ── the type argument WRITTEN DOWN, one shape per source ──
        //
        // A function RESULT. The call site passes nothing, and the body is a
        // separate declaration, so the annotation is the only place the width
        // is stated.
        20 => format!("println(gmkInt().v {op} {big})"),
        21 => format!("println(gmkLong().v {op} {lbig})"),
        // …read through the method and the computed property as well, which
        // resolve off the receiver rather than off the annotation directly.
        22 => format!("println(gmkInt().get() {op} gmkInt().once)"),
        23 => format!("println(gmkLong().get() {op} gmkLong().once)"),
        // A PARAMETER. The construction the caller wrote is on the other side
        // of the call; inside the body only the annotation says `Int`.
        24 => format!("println(gtakeInt(GBox({big})))"),
        25 => format!("println(gtakeLong(GBox({lbig})))"),
        // A PROPERTY of another class, whose own type is written out in full.
        26 => format!("println(GHold(GBox({big}), GBox(1L)).b.v {op} {big})"),
        27 => format!("println(GHold(GBox(1), GBox({lbig})).bl.v {op} {lbig})"),
        // A SUPERTYPE. `GSub` declares no type parameters at all, so nothing but
        // `: GOpen<Int>(…)` fixes the inherited property's width.
        28 => format!("println(GSub().v {op} {big})"),
        29 => format!("println(GSubL().v {op} {lbig})"),
        // A top-level `val` and a LOCAL annotation, whose initializers are
        // opaque calls — so the annotation, not the initializer, is the source.
        30 => format!("println(GTOPI.v {op} {big})"),
        31 => format!("println(GTOPL.v {op} {lbig})"),
        32 => format!("val ga{idx}: GBox<Int> = gmkInt(); println(ga{idx}.v {op} {big})"),
        33 => format!("val gb{idx}: GBox<Long> = gmkLong(); println(gb{idx}.v {op} {lbig})"),
        // A CAST. The JVM erases the argument — `kotlinc` warns the cast is
        // unchecked — but the static type it produces still decides the width.
        34 => format!("println((gany({big}) as GBox<Int>).v {op} {big})"),
        35 => format!("println((gany({lbig}) as GBox<Long>).v {op} {lbig})"),
        // A SECONDARY constructor supplies the type argument from ITS OWN
        // parameters; the primary's take a different count and mean nothing
        // here.
        36 => format!("println(GSec({big}, {}).v {op} {big})", pick(r, BIGINTS)),
        37 => format!("println(GSec({lbig}, {lbig2}).v {op} {lbig})"),
        // A BODY property declared with the class's type variable, which is not
        // a constructor parameter and so was left untyped by everything above.
        38 => format!("println(GBox({big}).w {op} GBox({}).w)", pick(r, BIGINTS)),
        39 => format!("println(GBox({lbig}).w {op} GBox({lbig2}).w)"),
        // The written argument at a NON-integer type, through the same
        // annotation paths: neither may reach the 32-bit wrap or the integer
        // division. The leading `""` is required, not cosmetic: `+` is resolved
        // against the LEFT operand, and `Double.plus` has no `String` overload —
        // so `gmkDbl().v + "|"` is a compile error in Kotlin, not a parity probe.
        40 => "println(gmkStr().v + \"|\" + gmkStr().w)".to_string(),
        41 => format!("println(gmkDbl().v / {})", pick(r, DIVS)),
        42 => format!("println(\"\" + (gmkDbl().get() {op} 0.0))"),
        43 => format!("println(gtakeStr(GBox({})))", pick(r, STRS)),
        // A NESTED written argument, where the width lives two levels down.
        _ => format!("println(gmkNest().v.v {op} {big})"),
    }
}

fn g_operator(r: &mut Rng) -> String {
    const INTKEYS: &[&str] = &["1", "3", "7", "10", "25", "42"];
    const STRKEYS: &[&str] = &["\"apple\"", "\"banana\"", "\"cherry\"", "\"zebra\""];
    let a = pick(r, INTKEYS);
    let b = pick(r, INTKEYS);
    let s = pick(r, STRKEYS);
    let t = pick(r, STRKEYS);
    match r.below(24) {
        // Collection `plus`/`minus`, in both the operator and the member form,
        // and across the element/Iterable overload pair.
        0 => p(format!("listOf({a}, {b}, {a}) - {a}")),
        1 => p(format!("listOf({a}, {b}) + {a}")),
        2 => p(format!("listOf({a}, {b}, {a}) - listOf({a})")),
        3 => p(format!("listOf({a}, {b}) + listOf({a}, {b})")),
        4 => p(format!("setOf({a}, {b}) - {a}")),
        5 => p(format!("setOf({a}, {b}) + {a}")),
        6 => p(format!("listOf({a}, {b}).plus({a})")),
        7 => p(format!("listOf({a}, {b}, {a}).minus({a})")),
        8 => p(format!("listOf({a}, {b}).plusElement(listOf({a}))")),
        9 => p(format!("mapOf({s} to {a}, {t} to {b}) - {s}")),
        10 => p(format!("mapOf({s} to {a}) + ({t} to {b})")),
        11 => p(format!("mapOf({s} to {a}) + mapOf({t} to {b})")),
        12 => p(format!("(1..3) + {a}")),
        13 => p(format!("listOf({s}) + {t}")),
        // The compound forms: `plus` rebinding a `var` against `plusAssign`
        // mutating a `val`, which differ only through an alias.
        14 => p(format!(
            "run {{ var l = listOf({a}); val k = l; l += {b}; \"$l/$k\" }}"
        )),
        15 => p(format!(
            "run {{ val m = mutableListOf({a}); val k = m; m += {b}; \"$m/$k\" }}"
        )),
        16 => p(format!(
            "run {{ val m = mutableMapOf({s} to {a}); m += ({t} to {b}); m }}"
        )),
        // Iteration order of the JVM collections.
        17 => p(format!("hashSetOf({a}, {b}, 7, 25, 10)")),
        18 => p(format!("hashMapOf({s} to {a}, {t} to {b}, \"kiwi\" to 3)")),
        19 => p(format!("sortedSetOf({s}, {t}, \"kiwi\")")),
        20 => p(format!("linkedSetOf({s}, {t}, \"kiwi\")")),
        21 => p(format!(
            "run {{ val h = HashMap<Int, Int>(); for (i in listOf({a}, {b}, 7, 25, 10)) h[i] = i; h }}"
        )),
        22 => p(format!("HashSet(listOf({s}, {t}, \"kiwi\"))")),
        // `Grouping`, whose keys come out in first-encounter order.
        _ => p(format!(
            "listOf({s}, {t}, \"kiwi\").groupingBy {{ it.length }}.eachCount()"
        )),
    }
}

/// Instance **equality** — `==` between class instances, and the three rules
/// Kotlin picks between.
///
/// The generator had no probe of this shape at all, which is why the fuzzer
/// stayed clean while `Eqp(1) == Eqp(1)` answered `true`: a class that declares
/// neither `data` nor `equals` inherits `Any.equals`, i.e. **reference
/// identity**, so two separate constructions are NOT equal. The three families
/// are emitted together because they only differ from each other:
///
/// * `EqPlain` — no override: identity.
/// * `EqEq` — `equals` only: `List` members see it, but a `Set`/`Map` key does
///   not, because the hash buckets never meet.
/// * `EqBoth` — `equals` + `hashCode`: every container sees it.
fn g_equality(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, &[0, 1, 2, 7]);
    let b = pick(r, &[0, 1, 2, 7]);
    let cls = pick(r, &["EqPlain", "EqEq", "EqBoth"]);
    match r.below(22) {
        0 => p(format!("{cls}({a}) == {cls}({b})")),
        1 => p(format!("{cls}({a}) != {cls}({b})")),
        2 => p(format!("listOf({cls}({a})).contains({cls}({b}))")),
        3 => p(format!("{cls}({a}) in listOf({cls}({b}))")),
        4 => p(format!(
            "listOf({cls}({a}), {cls}({b})).indexOf({cls}({b}))"
        )),
        5 => p(format!("setOf({cls}({a}), {cls}({b})).size")),
        6 => p(format!("listOf({cls}({a}), {cls}({b})).distinct().size")),
        7 => p(format!(
            "mapOf({cls}({a}) to 1, {cls}({b}) to 2)[{cls}({a})]"
        )),
        8 => p(format!(
            "mapOf({cls}({a}) to 1, {cls}({b}) to 2).containsKey({cls}({b}))"
        )),
        9 => p(format!("listOf({cls}({a})) == listOf({cls}({b}))")),
        // Two elements, not one: a 1-element `setOf`/`mapOf` is
        // `java.util.Collections.singleton*`, whose `contains`/`get` consult
        // `equals` ALONE — no hash gate, no identity check. That corner is a
        // documented exclusion (see README), so probes stay off it.
        10 => p(format!(
            "setOf({cls}({a}), {cls}(9)) == setOf({cls}({b}), {cls}(9))"
        )),
        11 => p(format!("({cls}({a}) to 1) == ({cls}({b}) to 1)")),
        12 => p(format!(
            "listOf(listOf({cls}({a}))) == listOf(listOf({cls}({b})))"
        )),
        13 => p(format!("{cls}({a}).equals({cls}({b}))")),
        // A self-comparison. `==` is `Intrinsics.areEqual`, which has NO
        // identity short-circuit, so a declared `equals` DOES run here.
        // The local is named per class as well as per operand: two probes that
        // named by the probe INDEX, which is the only thing unique within a
        // batch: keying on the class and operands still collided whenever the
        // same triple was drawn twice, and a collision is `conflicting
        // declarations`, which makes the whole 40-probe batch barren.
        14 => format!("    val se{idx} = {cls}({a})\n    println(se{idx} == se{idx})"),
        15 => p(format!(
            "mutableListOf({cls}({a}), {cls}({b})).remove({cls}({b}))"
        )),
        // Only `EqBoth` has a REPRODUCIBLE hashCode. `EqPlain`/`EqEq` inherit
        // the JVM's identity hash, which differs run to run, so a probe folding
        // one could never agree with anything and would be noise, not signal.
        16 => p("listOf(EqBoth(1)).hashCode()".to_string()),
        17 => p("setOf(EqBoth(1)).hashCode()".to_string()),
        18 => p("(EqBoth(1) to 0).hashCode()".to_string()),
        19 => p(format!(
            "mapOf({cls}({a}) to 1).values.toList() == listOf(1)"
        )),
        20 => p(format!(
            "listOf({cls}({a}), {cls}({b})).count {{ it == {cls}({a}) }}"
        )),
        _ => p(format!(
            "listOf({cls}({a})).containsAll(listOf({cls}({b})))"
        )),
    }
}

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
        Mode::Char => "char",
        Mode::NullSafe => "nullsafe",
        Mode::DataWhen => "datawhen",
        Mode::Class => "class",
        Mode::DataInherit => "datainherit",
        Mode::Coll => "coll",
        Mode::DoWhile => "dowhile",
        Mode::StrFmt => "strfmt",
        Mode::Bitwise => "bitwise",
        Mode::SafeCall => "safecall",
        Mode::MapColl => "mapcoll",
        Mode::FinExit => "finexit",
        Mode::Width => "width",
        Mode::Hash => "hash",
        Mode::Entry => "entry",
        Mode::StrSearch => "strsearch",
        Mode::CollArg => "collarg",
        Mode::Predicate => "predicate",
        Mode::Body => "body",
        Mode::Ext => "ext",
        Mode::Scope => "scope",
        Mode::Params => "params",
        Mode::Tuple => "tuple",
        Mode::Capture => "capture",
        Mode::Cast => "cast",
        Mode::LazyProp => "lazyprop",
        Mode::Result => "result",
        Mode::LocalFn => "localfn",
        Mode::Ctor => "ctor",
        Mode::Deleg => "deleg",
        Mode::Invoke => "invoke",
        Mode::StrColl => "strcoll",
        Mode::Equality => "equality",
        Mode::Operator => "operator",
        Mode::Generic => "generic",
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
        Mode::Char => g_char(r, idx),
        Mode::NullSafe => g_nullsafe(r, idx),
        Mode::DataWhen => g_datawhen(r, idx),
        Mode::Class => g_class(r, idx),
        Mode::DataInherit => g_datainherit(r, idx),
        Mode::Coll => g_coll(r, idx),
        Mode::DoWhile => g_dowhile(r, idx),
        Mode::StrFmt => g_strfmt(r),
        Mode::Bitwise => g_bitwise(r),
        Mode::SafeCall => g_safecall(r, idx),
        Mode::MapColl => g_mapcoll(r, idx),
        Mode::FinExit => g_finexit(r, idx),
        Mode::Width => g_width(r, idx),
        Mode::Hash => g_hash(r, idx),
        Mode::Entry => g_entry(r, idx),
        Mode::StrSearch => g_strsearch(r, idx),
        Mode::CollArg => g_collarg(r, idx),
        Mode::Predicate => g_predicate(r, idx),
        Mode::Body => g_body(r, idx),
        Mode::Ext => g_ext(r, idx),
        Mode::Scope => g_scope(r, idx),
        Mode::Params => g_params(r, idx),
        Mode::Tuple => g_tuple(r, idx),
        Mode::Capture => g_capture(r, idx),
        Mode::Cast => g_cast(r, idx),
        Mode::LazyProp => g_lazyprop(r, idx),
        Mode::Result => g_result(r, idx),
        Mode::LocalFn => g_localfn(r, idx),
        Mode::Ctor => g_ctor(r, idx),
        Mode::Deleg => g_deleg(r, idx),
        Mode::Invoke => g_invoke(r, idx),
        Mode::StrColl => g_strcoll(r, idx),
        Mode::Equality => g_equality(r, idx),
        Mode::Operator => g_operator(r),
        Mode::Generic => g_generic(r, idx),
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
/// The top-level declarations the newer modes need, each emitted only when a
/// probe names it — the same rule the class blocks follow, so `minimize` keeps
/// producing programs the reference toolchain still compiles.
///
/// A top-level `val`/`var` is emitted whenever ANY probe reads one, because the
/// `lazyprop` probes share them; every mutation probe writes and reads inside
/// one `run { … }`, so the answer does not depend on which other probes survive
/// a reduction.
fn extra_declarations(probes: &[String]) -> String {
    let mut out = String::new();
    let named = |m: &str| probes.iter().any(|p| p.contains(m));
    // The three equality families — see `g_equality`.
    if named("EqPlain(") {
        out.push_str("class EqPlain(val a: Int)\n");
    }
    if named("EqEq(") {
        out.push_str(
            "class EqEq(val a: Int) {\n\
             \x20   override fun equals(other: Any?): Boolean = other is EqEq && other.a == a\n\
             }\n",
        );
    }
    if named("EqBoth(") {
        out.push_str(
            "class EqBoth(val a: Int) {\n\
             \x20   override fun equals(other: Any?): Boolean = other is EqBoth && other.a == a\n\
             \x20   override fun hashCode(): Int = a\n\
             }\n",
        );
    }
    if named("Acc(") || named("Acc.") {
        out.push_str(
            "class Acc(val base: Int) {\n\
             \x20   var n = 0\n\
             \x20   val doubled = base * 2\n\
             \x20   companion object {\n\
             \x20       val ZERO = 0\n\
             \x20       fun of(k: Int): Acc = Acc(k)\n\
             \x20       fun describe(): String = \"acc/\" + ZERO\n\
             \x20   }\n\
             \x20   fun bump(): Int { n = n + 1; return n }\n\
             \x20   fun tot(): Int = n + doubled\n\
             }\n",
        );
    }
    // The generic helpers — see `g_generic`. Each is emitted only when a probe
    // names it, so a reduced program still compiles under the reference
    // toolchain.
    if named("gid(") {
        out.push_str("fun <T> gid(x: T): T = x\n");
    }
    if named("gfirst(") {
        out.push_str("fun <T> gfirst(a: T, b: T): T = a\n");
    }
    // `once` is a COMPUTED property: a zero-argument method wearing property
    // syntax, whose declared result is the class's type variable. It resolves
    // its width from the receiver exactly as `get()` does, and it is a separate
    // lowering path — reads of it go through the member node, not the call one.
    // `w` is a BODY property declared with the class's type variable. It is not
    // a constructor parameter, so nothing the construction site does fixes it
    // directly — it reads the argument off the receiver, as `once` does.
    //
    // The gate lists every marker that TRANSITIVELY needs the class, not just
    // the probes that spell it: `println(GTOPL.v - 1L)` names no `GBox` at all,
    // yet it pulls in `val GTOPL: GBox<Long>` and `fun gmkLong(): GBox<Long>`
    // below. Missing one leaves the reference toolchain with an unresolved
    // reference, which is a BARREN program — the fuzzer reported exactly that on
    // seed 24323 before this line listed `GTOP`.
    if named("GBox")
        || named("GHold(")
        || named("gmk")
        || named("gtake")
        || named("gany(")
        || named("GTOP")
    {
        out.push_str(
            "class GBox<T>(val v: T) {\n\
             \x20   fun get(): T = v\n\
             \x20   val once: T get() = v\n\
             \x20   val w: T = v\n\
             }\n",
        );
    }
    // The written-down sources, each in its own declaration so a reduction to a
    // single probe still emits only what that probe names.
    if named("gmkInt(") || named("GTOPI") || named("GBox<Int>") {
        out.push_str("fun gmkInt(): GBox<Int> = GBox(65536)\n");
    }
    if named("gmkLong(") || named("GTOPL") || named("GBox<Long>") {
        out.push_str("fun gmkLong(): GBox<Long> = GBox(65536L)\n");
    }
    if named("gmkStr(") {
        out.push_str("fun gmkStr(): GBox<String> = GBox(\"gs\")\n");
    }
    if named("gmkDbl(") {
        out.push_str("fun gmkDbl(): GBox<Double> = GBox(2.5)\n");
    }
    if named("gmkNest(") {
        out.push_str("fun gmkNest(): GBox<GBox<Int>> = GBox(GBox(70000))\n");
    }
    if named("gtakeInt(") {
        out.push_str("fun gtakeInt(b: GBox<Int>): Int = b.v * 2000000000\n");
    }
    if named("gtakeLong(") {
        out.push_str("fun gtakeLong(b: GBox<Long>): Long = b.v * 2000000000L\n");
    }
    if named("gtakeStr(") {
        out.push_str("fun gtakeStr(b: GBox<String>): String = b.v + b.w\n");
    }
    if named("gany(") {
        out.push_str("fun gany(x: Any): Any = GBox(x)\n");
    }
    if named("GHold(") {
        out.push_str("class GHold(val b: GBox<Int>, val bl: GBox<Long>)\n");
    }
    if named("GSub(") || named("GSubL(") {
        out.push_str(
            "open class GOpen<T>(val v: T)\n\
             class GSub : GOpen<Int>(65536)\n\
             class GSubL : GOpen<Long>(65536L)\n",
        );
    }
    if named("GTOPI") {
        out.push_str("val GTOPI: GBox<Int> = gmkInt()\n");
    }
    if named("GTOPL") {
        out.push_str("val GTOPL: GBox<Long> = gmkLong()\n");
    }
    // A class whose SECONDARY constructor is the one a two-argument call
    // selects; its parameters, not the primary's, name the type variable.
    if named("GSec(") {
        out.push_str(
            "class GSec<T>(val v: T) {\n\
             \x20   constructor(a: T, b: T) : this(a)\n\
             }\n",
        );
    }
    if named("GMut(") {
        out.push_str("class GMut<T>(var v: T)\n");
    }
    if named("GTwo(") {
        out.push_str("class GTwo<A, B>(val a: A, val b: B)\n");
    }
    if named("DBody(") {
        out.push_str("data class DBody(val a: Int) {\n\x20   val extra = a + 1\n}\n");
    }
    if named(".dbl()")
        || named(".shout()")
        || named(".rep(")
        || named(".label()")
        || named(".half()")
        || named(".plusN(")
        || named(".quad()")
    {
        out.push_str(
            "fun Int.dbl(): Int = this * 2\n\
             fun Long.dbl(): Long = this * 2\n\
             fun Int.plusN(n: Int = 3): Int = this + n\n\
             fun Int.quad(): Int = dbl().dbl()\n\
             fun String.shout(): String = uppercase() + \"!\"\n\
             fun String.rep(n: Int): String {\n\
             \x20   var s = \"\"\n\
             \x20   for (i in 1..n) s += this\n\
             \x20   return s\n\
             }\n\
             fun Double.half(): Double = this / 2\n",
        );
    }
    // Kept separate from the block above so a reduction down to a probe that
    // names no `Pt` does not emit an extension on an undeclared type — the
    // reduced program has to stay compilable for the oracle.
    if named(".label()") {
        out.push_str("fun Pt.label(): String = x.toString() + \"/\" + y\n");
    }
    if named("pad(") {
        out.push_str(
            "fun pad(s: String, n: Int = 2, sep: String = \"-\"): String {\n\
             \x20   var out = s\n\
             \x20   for (i in 1..n) out = out + sep\n\
             \x20   return out\n\
             }\n",
        );
    }
    if named("total(") {
        out.push_str(
            "fun total(vararg xs: Int): Int {\n\
             \x20   var t = 0\n\
             \x20   for (x in xs) t += x\n\
             \x20   return t\n\
             }\n",
        );
    }
    if named("mixed(") {
        out.push_str(
            "fun mixed(a: Int, vararg rest: Int): Int {\n\
             \x20   var t = a * 100\n\
             \x20   for (x in rest) t += x\n\
             \x20   return t\n\
             }\n",
        );
    }
    if named("Cfg(") {
        out.push_str("data class Cfg(val a: Int = 1, val b: String = \"x\")\n");
    }
    if named("anyAt(") {
        out.push_str("fun anyAt(i: Int): Any = listOf<Any>(7, \"ab\", 2.5)[i]\n");
    }
    if named("bigAny(") {
        out.push_str("fun bigAny(): Any = 2000000000\n");
    }
    if named("GK") || named("GNAME") || named("GDERIVED") || named("GC") {
        out.push_str(
            "val GK = 7\n\
             val GNAME = \"kt\"\n\
             val GDERIVED = GK * 3\n\
             var GC = 0\n",
        );
    }
    if named("Lz(") {
        out.push_str(
            "class Lz(val k: Int) {\n\
             \x20   val v: Int by lazy { println(\"force\" + k); k * 10 }\n\
             \x20   fun doubled(): Int = v + v\n\
             }\n",
        );
    }
    if named("rboom(") {
        out.push_str(
            "fun rboom(n: Int): Int {\n\
             \x20   if (n < 0) throw IllegalStateException(\"neg\")\n\
             \x20   return n * 2\n\
             }\n",
        );
    }
    // ── ctor: secondary constructors + `init` ordering ──
    if named("Ord(") {
        out.push_str(
            "class Ord(val a: Int, val b: Int) {\n\
             \x20   val sum: Int = a + b\n\
             \x20   init { println(\"ord.i1 sum=\" + sum) }\n\
             \x20   val seen: String = \"s\" + a\n\
             \x20   init { println(\"ord.i2 seen=\" + seen) }\n\
             \x20   constructor(a: Int) : this(a, 0) { println(\"ord.c1 \" + a) }\n\
             \x20   constructor() : this(9) { println(\"ord.c0\") }\n\
             }\n",
        );
    }
    if named("NoPrim(") {
        out.push_str(
            "class NoPrim {\n\
             \x20   var total: Int = 1\n\
             \x20   init { total += 10; println(\"np.init \" + total) }\n\
             \x20   constructor(n: Int) { total += n; println(\"np.c1 \" + total) }\n\
             \x20   constructor() : this(3) { total += 100; println(\"np.c0 \" + total) }\n\
             }\n",
        );
    }
    if named("SubOrd(") {
        out.push_str(
            "open class OrdBase(val bv: Int) {\n\
             \x20   init { println(\"base.init \" + bv) }\n\
             }\n\
             class SubOrd(x: Int) : OrdBase(x * 2) {\n\
             \x20   val tag: String = \"t\" + x + \"/\" + bv\n\
             \x20   init { println(\"sub.init \" + tag) }\n\
             \x20   constructor(x: Int, y: Int) : this(x + y) { println(\"sub.c2\") }\n\
             }\n",
        );
    }
    if named("Pick(") {
        out.push_str(
            "class Pick(val a: Int, val b: Int = 5) {\n\
             \x20   constructor(s: String) : this(s.length) { println(\"pick.str \" + s) }\n\
             \x20   fun show(): String = \"\" + a + \":\" + b\n\
             }\n",
        );
    }
    // ── deleg: interface delegation ──
    if named("Fwd(") || named("Over(") || named("Two(") || named("asOne(") {
        out.push_str(
            "interface One {\n\
             \x20   fun one(): String\n\
             \x20   fun both(): String { return one() + \"|\" + one() }\n\
             }\n\
             interface TwoI { fun two(): Int }\n\
             class Base1(val k: Int) : One { override fun one(): String = \"b1-\" + k }\n\
             class TwoB(val k: Int) : TwoI { override fun two(): Int = k * 3 }\n\
             class Fwd(x: One) : One by x\n\
             class Over(x: One) : One by x { override fun one(): String = \"over\" }\n\
             class Two(x: One, y: TwoI) : One by x, TwoI by y\n\
             fun asOne(x: One): One = x\n",
        );
    }
    // ── invoke: calling the result of a call ──
    if named("adder(") {
        out.push_str("fun adder(n: Int): (Int) -> Int = { it + n }\n");
    }
    if named("mulBy(") {
        out.push_str("fun mulBy(n: Int): (Int) -> Int { return { x: Int -> x * n } }\n");
    }
    if named("twice(") {
        out.push_str("fun twice(n: Int): () -> (() -> Int) = { { n + n } }\n");
    }
    if named("fnList(") {
        out.push_str(
            "fun fnList(n: Int): List<(Int) -> Int> = listOf<(Int) -> Int>({ it + n }, { it * n })\n",
        );
    }
    if named("Boxed(") {
        out.push_str("class Boxed(val k: Int) { operator fun invoke(x: Int): Int = x * 10 + k }\n");
    }
    if named("withLocal(") {
        out.push_str(
            "fun withLocal(n: Int): Int {\n\
             \x20   fun sq(x: Int): Int = x * x\n\
             \x20   return sq(n) + 1\n\
             }\n",
        );
    }
    if named("localFact(") {
        out.push_str(
            "fun localFact(n: Int): Int {\n\
             \x20   fun f(k: Int): Int = if (k <= 1) 1 else k * f(k - 1)\n\
             \x20   return f(n)\n\
             }\n",
        );
    }
    if named("localFib(") {
        out.push_str(
            "fun localFib(n: Int): Int {\n\
             \x20   fun fib(k: Int): Int = if (k < 2) k else fib(k - 1) + fib(k - 2)\n\
             \x20   return fib(n)\n\
             }\n",
        );
    }
    if named("localDefault(") || named("localDefaultBare(") {
        out.push_str(
            "fun localDefault(n: Int): String {\n\
             \x20   fun tag(k: Int, sep: String = \"-\"): String = sep + k\n\
             \x20   return tag(n) + tag(n, \"+\")\n\
             }\n\
             fun localDefaultBare(): String {\n\
             \x20   fun tag(k: Int = 4): String = \"t\" + k\n\
             \x20   return tag()\n\
             }\n",
        );
    }
    if named("localShadow(") {
        out.push_str(
            "fun shadowed(n: Int): Int = n * 1000\n\
             fun localShadow(n: Int): Int {\n\
             \x20   fun shadowed(k: Int): Int = k + 1\n\
             \x20   return shadowed(n)\n\
             }\n",
        );
    }
    if named("localInLambda(") {
        out.push_str(
            "fun localInLambda(n: Int): Int {\n\
             \x20   fun step(x: Int): Int = x * 2 + 1\n\
             \x20   return listOf(n, n + 1).map { step(it) }.sum()\n\
             }\n",
        );
    }
    if named("localNested(") {
        out.push_str(
            "fun localNested(n: Int): Int {\n\
             \x20   fun outer(x: Int): Int {\n\
             \x20       fun inner(y: Int): Int = y + 3\n\
             \x20       return inner(x) * 2\n\
             \x20   }\n\
             \x20   return outer(n)\n\
             }\n",
        );
    }
    out
}

/// Top-level declarations a probe may reference are emitted only when some probe
/// actually names them, so an unrelated program stays minimal and [`minimize`]
/// keeps producing compilable reductions: the `Pt` `data class`, and one
/// `boom<i>` helper per cross-frame `throw` probe. The helper id is read back
/// out of the probe text rather than from its position, because `minimize` drops
/// probes and would otherwise renumber them.
fn declarations(probes: &[String]) -> String {
    let mut out = String::new();
    out.push_str(&extra_declarations(probes));
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
        ("KtErr(", "class KtErr(msg: String) : Exception(msg)\n"),
        // `super<T>.m()`. `Both` implements two interfaces that both supply
        // `pick`, which is exactly when Kotlin *requires* the qualifier — so the
        // oracle accepts only the qualified spelling, and the two arms it can
        // resolve to differ. `only` is implemented by one supertype and `chain`
        // by a superclass, covering the qualified forms that have an unqualified
        // equivalent.
        (
            "Both(",
            "interface Left {\n\
             \x20   fun pick(): String = \"L\"\n\
             \x20   fun only(): String = \"only-L\"\n\
             }\n\
             interface Right {\n\
             \x20   fun pick(): String = \"R\"\n\
             }\n\
             class Both(val k: Int) : Left, Right {\n\
             \x20   override fun pick(): String = super<Left>.pick() + super<Right>.pick() + k\n\
             \x20   override fun only(): String = super<Left>.only() + \"/\" + k\n\
             }\n",
        ),
        (
            "Sub(",
            "open class Sup(val k: Int) {\n\
             \x20   open fun chain(): String = \"sup$k\"\n\
             }\n\
             class Sub(k: Int) : Sup(k) {\n\
             \x20   override fun chain(): String = \"sub[\" + super<Sup>.chain() + \"]\"\n\
             }\n",
        ),
        // A `data class` under a field-carrying supertype. `Wd`'s superclass
        // argument is written in terms of its own constructor parameter, which
        // is what makes `copy` observable: Kotlin's generated `copy` calls the
        // primary constructor, so the base field is recomputed rather than
        // carried over.
        (
            "Lf(",
            "open class Nd(val d: Int) {\n\
             \x20   fun depth(): Int = d\n\
             }\n\
             data class Lf(val v: Int) : Nd(1)\n\
             data class Br(val l: String, val r: Int) : Nd(2)\n\
             data class Wd(val s: String) : Nd(s.length)\n",
        ),
    ] {
        // `Sq(`/`Ci(` also need the `Shp` block, so the shape marker is the bare
        // type prefix rather than a constructor call.
        let named = match marker {
            "Shp" => probes
                .iter()
                .any(|p| p.contains("Shp") || p.contains("Sq(") || p.contains("Ci(")),
            // `Br(`/`Wd(` share the `Nd` block with `Lf(`, and a batch can draw
            // one without the other. Keying the block on `Lf(` alone emitted
            // `Br(...)` with no declaration, which `kotlinc` rejects — so those
            // batches were never compared at all, and scored as clean runs
            // until the barren gate started reporting them.
            "Lf(" => probes
                .iter()
                .any(|p| p.contains("Lf(") || p.contains("Br(") || p.contains("Wd(")),
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
    /// How far this side got. See [`Stage`].
    stage: Stage,
}

/// How far a side got before it stopped.
///
/// Only the ORACLE can stop at `Rejected`: it is a two-command toolchain
/// (`kotlinc`, then `kotlin`), and which of the two failed is the difference
/// between two unrelated defects — a generated program the reference compiler
/// REJECTS is a bug in this generator, while one it compiles and then aborts on
/// is a run-time difference worth reading. Collapsed into a single count the two
/// read identically in the summary, which is how a whole mode's worth of them
/// goes unread. Our frontend is one command, so it always reports `Ran`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    /// The reference compiler rejected the program; nothing ran.
    Rejected,
    /// The program was compiled (or, for our side, accepted) and executed.
    Ran,
}

/// Why the reference toolchain produced no answer to compare against, or `None`
/// when it did produce one — exit 0 and non-empty stdout.
///
/// A run that produced none is **barren**, not a pass. Two failing sides compare
/// equal, so a `kotlinc` that rejected the generated program (or a `kotlin` that
/// timed out) scores as agreement under [`differs`] and quietly inflates the
/// clean count. Sibling frontends have been burned by exactly this: a whole fuzz
/// session reporting `divergences: 0` across hundreds of oracle timeouts. Barren
/// programs are counted and reported separately, and they fail the run — split
/// by WHICH of the three ways they were barren, because those are three
/// different defects.
///
/// Pure in its input so the classification can be tested without a toolchain on
/// PATH; the inputs themselves are what `run_oracle` observes.
fn barren_reason(oracle: &RunOut) -> Option<&'static str> {
    match (oracle.stage, oracle.ok, oracle.stdout.is_empty()) {
        (Stage::Rejected, _, _) => Some("kotlinc REJECTED the program (a generator bug)"),
        (Stage::Ran, true, false) => None,
        (Stage::Ran, true, true) => Some("the program ran and printed nothing"),
        (Stage::Ran, false, _) => Some("the program compiled and then ABORTED"),
    }
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
        Some((stdout, ok)) => RunOut {
            stdout,
            ok,
            stage: Stage::Ran,
        },
        None => RunOut {
            stdout: Vec::new(),
            ok: false,
            stage: Stage::Ran,
        },
    }
}

/// The oldest JVM whose answers this harness treats as the reference, matching
/// `scripts/capture-parity.sh`'s floor. Below it the oracle speaks an older
/// dialect — `Double.toString` took the shortest-representation algorithm in
/// JDK 19 and the index faults moved onto `Preconditions` in JDK 21 — so a
/// divergence report would name the JDK rather than this frontend.
const ORACLE_JVM_FLOOR: u32 = 21;

/// The properties that pin the run step's locale and console charset, the same
/// set `scripts/capture-parity.sh` freezes the corpus under.
///
/// `%f`/`%e`/`%,d` read their separators from `Locale.getDefault()`, and from
/// JDK 19 on the console streams take their charset from `stdout.encoding` /
/// `stderr.encoding` rather than from `file.encoding` — so `LANG=C` alone turns
/// `println("café")` into `caf?`. kotlinrs has no locale and always writes
/// UTF-8 the `en_US` way; without these the `strfmt` mode reports the AMBIENT
/// locale as a frontend bug.
const ORACLE_JVM_PINS: &[&str] = &[
    "-J-Duser.language=en",
    "-J-Duser.country=US",
    "-J-Dfile.encoding=UTF-8",
    "-J-Dstdout.encoding=UTF-8",
    "-J-Dstderr.encoding=UTF-8",
];

/// The feature release named by a `(JRE 21.0.12)` / `(JRE 17.0.4.1+9-LTS)`
/// parenthesis, which both Kotlin launchers print in their `-version` banner.
///
/// Pure so the parse can be tested without a toolchain installed.
fn jre_feature(banner: &str) -> Option<u32> {
    let tail = banner.split("(JRE ").nth(1)?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The JVM a launcher resolved for ITSELF, measured rather than inferred.
///
/// There are two JVMs in the oracle and `JAVA_HOME` is only *expected* to steer
/// both: a `const val` is folded into the class file under the COMPILER's
/// `Double.toString` while an identical literal read at run time renders under
/// the RUNTIME's, so the same source compiled and run across 17/21 gives four
/// distinct answer pairs. Each launcher is therefore asked separately.
fn launcher_jre(tool: &Path) -> Option<u32> {
    let out = Command::new(tool).arg("-version").output().ok()?;
    let banner =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    jre_feature(&banner)
}

/// Refuse an oracle older than [`ORACLE_JVM_FLOOR`] on either side.
fn check_oracle_jvms(kotlinc: &Path, kotlin: &Path) {
    for (label, tool) in [("kotlinc", kotlinc), ("kotlin", kotlin)] {
        match launcher_jre(tool) {
            Some(v) if v >= ORACLE_JVM_FLOOR => {
                eprintln!("parity-fuzz: {label} runs on JRE {v}");
            }
            other => {
                let seen = other.map_or("unknown".to_string(), |v| v.to_string());
                eprintln!(
                    "parity-fuzz: {label} runs on JRE {seen}; the oracle needs \
                     {ORACLE_JVM_FLOOR} or newer"
                );
                eprintln!("parity-fuzz: export JAVA_HOME=/path/to/jdk{ORACLE_JVM_FLOOR}+");
                std::process::exit(2);
            }
        }
    }
}

/// Run through the reference toolchain: compile with `kotlinc`, then run the
/// generated `TKt` class. A compile failure reports `Stage::Rejected`, which is
/// a different defect from a program that compiled and then aborted.
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
            stage: Stage::Rejected,
        };
    }

    let mut r = Command::new(kotlin);
    r.args(ORACLE_JVM_PINS)
        .arg("-classpath")
        .arg(&out)
        .arg("TKt")
        .current_dir(&dir);
    let res = capture(&mut r, timeout);
    let _ = std::fs::remove_dir_all(&dir);
    match res {
        Some((stdout, ok)) => RunOut {
            stdout,
            ok,
            stage: Stage::Ran,
        },
        None => RunOut {
            stdout: Vec::new(),
            ok: false,
            stage: Stage::Ran,
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

/// Run one program through both sides and hand back (oracle, ours).
fn compare(probes: &[String], t: &Tools, timeout: Duration) -> (RunOut, RunOut) {
    let src = build_program(probes);
    let a = run_oracle(&t.kotlinc, &t.kotlin, &src, timeout);
    let b = run_ours(&t.ours, &src, timeout);
    (a, b)
}

fn diverges(probes: &[String], t: &Tools, timeout: Duration) -> bool {
    let (a, b) = compare(probes, t, timeout);
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
    /// Print the generated program instead of running it — the only way to
    /// see what a BARREN seed actually handed the reference toolchain.
    dump: bool,
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
        dump: false,
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
            "--dump" => a.dump = true,
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

    check_oracle_jvms(&kotlinc, &kotlin);

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
    let mut probes_compared = 0usize;
    let mut barren = 0usize;
    // The two halves of `barren`, which are unrelated defects — see `Stage`.
    let mut rejected = 0usize;
    let mut aborted = 0usize;

    for k in 0..iters {
        let seed = if args.once {
            base
        } else {
            base.wrapping_add(k as u64)
        };
        let probes = gen_probes(seed, args.mode, args.probes);
        probes_run += probes.len();
        if args.dump {
            print!("{}", build_program(&probes));
            continue;
        }
        let (oracle, ours) = compare(&probes, &t, args.timeout);
        if let Some(why) = barren_reason(&oracle) {
            // Never scored as a pass — see `oracle_answered`.
            barren += 1;
            match oracle.stage {
                Stage::Rejected => rejected += 1,
                Stage::Ran => aborted += 1,
            }
            eprintln!(
                "seed {seed}: BARREN — {why} (ok={}, {} byte(s)); {} probe(s) NOT compared",
                oracle.ok,
                oracle.stdout.len(),
                probes.len()
            );
            continue;
        }
        if !differs(&oracle, &ours) {
            if args.verbose {
                eprintln!("seed {seed}: ok ({} probes)", probes.len());
            }
            probes_compared += probes.len();
            continue;
        }
        probes_compared += probes.len();
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

    eprintln!(
        "parity-fuzz: {iters} program(s), {probes_run} probe(s) generated, \
         {probes_compared} compared, {failures} divergence(s), \
         {barren} barren ({rejected} rejected by kotlinc, {aborted} aborted at run time)"
    );
    // A barren program is a hole in the measurement, not a pass, so it fails the
    // run just as a divergence does.
    if failures > 0 || barren > 0 {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three ways the oracle can fail to answer are three different
    /// defects, and a single `barren` count spells them the same. A program
    /// `kotlinc` REJECTS means this generator emitted something Kotlin does not
    /// accept; one that compiles and then ABORTS means the program ran and
    /// died; one that ran and printed nothing means the probes produced no
    /// output at all. Only the last two can ever be a frontend's fault, and the
    /// first is the one a clean-looking summary hides.
    #[test]
    fn barren_reasons_are_told_apart() {
        let rejected = RunOut {
            stdout: Vec::new(),
            ok: false,
            stage: Stage::Rejected,
        };
        let aborted = RunOut {
            stdout: b"partial\n".to_vec(),
            ok: false,
            stage: Stage::Ran,
        };
        let silent = RunOut {
            stdout: Vec::new(),
            ok: true,
            stage: Stage::Ran,
        };
        let answered = RunOut {
            stdout: b"7\n".to_vec(),
            ok: true,
            stage: Stage::Ran,
        };
        let why = |o: &RunOut| barren_reason(o).unwrap_or("answered");
        assert_eq!(
            why(&rejected),
            "kotlinc REJECTED the program (a generator bug)"
        );
        assert_eq!(why(&aborted), "the program compiled and then ABORTED");
        assert_eq!(why(&silent), "the program ran and printed nothing");
        assert_eq!(barren_reason(&answered), None);
        // …and none of the three is ever scored as agreement, which is the
        // whole point of counting them apart from a pass.
        for o in [&rejected, &aborted, &silent] {
            assert!(barren_reason(o).is_some());
        }
    }

    /// The JVM floor is only enforceable if the banner parse is, and both
    /// launchers spell their JRE the same way in two different shapes: a bare
    /// `21.0.12` and a vendor-suffixed `17.0.4.1+9-LTS`. Every string here is a
    /// real banner observed from `kotlinc -version` / `kotlin -version`, plus
    /// the shapes a missing or unparseable banner takes — those must answer
    /// `None` so the caller refuses rather than reading them as a pass.
    #[test]
    fn a_launcher_banner_names_its_jre() {
        let cases: &[(&str, Option<u32>)] = &[
            ("info: kotlinc-jvm 2.4.10 (JRE 21.0.12)", Some(21)),
            ("info: kotlinc-jvm 2.4.10 (JRE 17.0.4.1+9-LTS)", Some(17)),
            ("Kotlin version 2.4.10-release-377 (JRE 21.0.12)", Some(21)),
            (
                "Kotlin version 2.4.10-release-377 (JRE 17.0.4.1+9-LTS)",
                Some(17),
            ),
            ("Kotlin version 2.4.10-release-377 (JRE 8u402-b06)", Some(8)),
            // A three-digit feature release has to survive the parse too, or
            // the floor starts rejecting the newest JVMs in 2100.
            ("info: kotlinc-jvm 9.9.9 (JRE 127.0.1)", Some(127)),
            ("", None),
            ("info: kotlinc-jvm 2.4.10", None),
            ("info: kotlinc-jvm 2.4.10 (JRE unknown)", None),
        ];
        assert!(cases.len() >= 9, "the banner corpus lost cases");
        for (banner, want) in cases {
            assert_eq!(jre_feature(banner), *want, "banner: {banner:?}");
        }
        // The floor itself is what the parse feeds, so pin the comparison too:
        // every banner below it must be rejected and every one at or above it
        // accepted.
        assert!(jre_feature(cases[1].0).unwrap() < ORACLE_JVM_FLOOR);
        assert!(jre_feature(cases[0].0).unwrap() >= ORACLE_JVM_FLOOR);
    }

    /// The run step's pins are what make a `%f` probe measure the FRONTEND
    /// rather than the capturing machine's `LANG`. Losing any of the five puts
    /// an ambient axis back into the comparison: the first two decide `%f`'s
    /// decimal separator and `%,d`'s grouping, and from JDK 19 on the last two
    /// decide the console charset, which `file.encoding` no longer does.
    #[test]
    fn the_run_step_pins_every_ambient_axis() {
        for want in [
            "-J-Duser.language=en",
            "-J-Duser.country=US",
            "-J-Dfile.encoding=UTF-8",
            "-J-Dstdout.encoding=UTF-8",
            "-J-Dstderr.encoding=UTF-8",
        ] {
            assert!(
                ORACLE_JVM_PINS.contains(&want),
                "the oracle stopped pinning {want}"
            );
        }
        assert_eq!(ORACLE_JVM_PINS.len(), 5);
        // Every pin has to reach the JVM rather than the program, or it becomes
        // a stray argv entry the class never sees.
        for p in ORACLE_JVM_PINS {
            assert!(p.starts_with("-J-D"), "{p} is not a JVM property flag");
        }
    }

    /// A run that aborts AFTER printing is the case a plain
    /// `stdout != stdout` comparison scores as agreement whenever our side
    /// aborts too — which is why it is barren rather than compared.
    #[test]
    fn an_abort_with_output_is_barren_not_a_pass() {
        let oracle = RunOut {
            stdout: b"1\n".to_vec(),
            ok: false,
            stage: Stage::Ran,
        };
        let ours = RunOut {
            stdout: b"1\n".to_vec(),
            ok: false,
            stage: Stage::Ran,
        };
        assert!(!differs(&oracle, &ours));
        assert!(barren_reason(&oracle).is_some());
    }
}
