# Known divergences

Open gaps between kotlinrs and the reference toolchain, each with the program
that shows it and the answer that was measured. The reference throughout is
`kotlinc` 2.4.10 on **JRE 21.0.12** for both the compile step and the run step.

A gap listed here is one that has been MEASURED. Anything not measured is not
listed, however plausible.

## Declined this round, with reasons

### Missing members and syntax — they fail loudly, so they are not this round's shape

Each of these is an `unresolved reference` or a parse error, which stops the
program with a diagnostic. That is a feature gap, not a wrong answer and not an
uncatchable abort, so none was in scope for a round about panics and error
shape. They are recorded here so the next round has the measurements.

| program | kotlinrs | reference |
| --- | --- | --- |
| `"\uD83D".length` (a lone surrogate) | `invalid unicode scalar in literal` | `1` |

### `"%2147483647d".format(1)` — the oracle has no stable answer

Run alone, the reference answered
`java.lang.OutOfMemoryError: Requested array size exceeds VM limit`. Run in a
batch minutes later, the same program **timed out at 25 s** on the same JVM, and
`%2147483646d` timed out too while `%2147483640d` succeeded and printed
`2147483640`. The boundary moves with heap and timing, so there is nothing here
that can honestly be frozen. kotlinrs is slow on the same input rather than
wrong. Widths that overflow an `int` ARE now a deterministic fault and are
pinned — see CHANGELOG.

### `IllegalFormatConversionException` names the wrong box for `Long` and `Float`

```
"%f".format(5L)    kotlinrs: f != java.lang.Integer   reference: f != java.lang.Long
"%d".format(5.0f)  kotlinrs: d != java.lang.Double    reference: d != java.lang.Float
```

`Long` and `Int` share `Value::Int` at run time and `Float` and `Double` share
`Value::Float`, so the width is not there to read. The throwable's CLASS, its
place in the hierarchy and the conversion character are all exact; only that one
operand name is approximate, and only for the two widths kotlinrs does not
carry. Fixing it means carrying the declared width onto the value, which is the
same substrate the 32-bit narrowing work has been building — not a format-string
change.

### `catch (e: IllegalFormatConversionException)` is accepted where kotlinc rejects it

Kotlin's default imports cover `java.lang.*` but not `java.util.*`, so the
unqualified name needs an `import`. Adding the seven `java.util.IllegalFormat…`
classes to `BUILTIN_THROWABLES` — which is what makes them catchable at
`IllegalArgumentException`, the defect this round fixed — also makes kotlinrs
resolve the unqualified spelling that `kotlinc` reports as unresolved. Correct
catch semantics for the case that occurs beat exact rejection of the case that
does not; recorded rather than hidden.

### `f(10000000)` — plain recursion is slow, not wrong

Ten million levels of ordinary (non-lambda) recursion exceeded a 25 s timeout
here; the reference raises `StackOverflowError` quickly because its frames are
on a bounded JVM stack. kotlinrs keeps call frames on the VM's own
heap-allocated stack, so it does not overflow at all — it just takes a long
time. Depth 50000 completes and is pinned. Making the VM's frame stack bounded
to imitate the JVM's would trade a slow correct answer for a fast wrong one at
every depth in between.

The UNBOUNDED case is closed since round 21: a function that can reach
itself by name tests its depth on entry (`KT_DEPTH_GUARD`) and raises
`StackOverflowError` past a million frames, twenty times the ~45 000 at which
the reference overflows a one-parameter recursion, so no depth the reference
completes is cut short. The test is one dispatched op per call and costs
about 4.4% of a call-bound benchmark's retired instructions (`fib(27)`, debug
build: 10.80 G unguarded, 11.28 G guarded; it was 11.93 G, 10.4%, when the
guard was two ops).

### `"ab".repeat(2000000000)` and `"a".padStart(2000000000)`

Both exceeded the 25 s timeout in kotlinrs. The reference answers
`java.lang.NegativeArraySizeException: -294967296` for the first (its own `int`
overflow) and succeeds for the second. Same category as the `%2147483647d` entry
— an allocation-bound answer, not a language one.

## Structural limits recorded rather than fixed

- **The nesting limit is a count, not a stack.** `NESTED_RUN_LIMIT` is 2000
  levels of re-entrant interpretation. Kotlin's limit is a JVM stack, so no
  fixed number matches it exactly: a program that recurses 1900 deep succeeds on
  both and one that recurses forever raises `StackOverflowError` on both, but a
  program that sits between the two answers differently. What must not differ —
  and no longer does — is the KIND of failure.

- **`RENDER_DEPTH_LIMIT` is 512.** A structure legitimately nested more than 512
  deep would raise `StackOverflowError` where the JVM might still render it.

- **`sequence_preserving` is a name list.** Which members answer another
  `Sequence` is stated once, in one predicate, but it is a list and a member
  added to the stdlib surface without being added there loses the sequence
  wording for everything downstream of it.

## Collection views

`subList()`, `asList()`, `asReversed()`, a map's `keys`/`values`/`entries`
(CHANGELOG, "Collection views read live") and a sorted collection's
`headMap`/`tailMap`/`subMap`/`headSet`/`tailSet`/`subSet`/`descending…` ranges
(CHANGELOG, "Sorted-collection ranges are live views") read their backing
object live. One divergence remains, measured on `kotlinc` 2.4.21 /
JRE 21.0.12.1:

| program | kotlinrs | reference |
| --- | --- | --- |
| `(listOf(1, 2).asReversed() as MutableList<Int>)[0] = 5` | succeeds | `ClassCastException: kotlin.collections.ReversedListReadOnly cannot be cast to kotlin.collections.MutableList` |

A live view is a heap copy re-taken whenever its backing object was written
since the last copy; reads that walk a view (`for`, indexing) read the backing
directly, so a loop over a view stays linear while the backing is written. A
sorted range is recomputed at the next member call, render, `in` or `for`,
since its comparator may be user code that needs the VM.

## Still missing, with the measurement

Each of these fails LOUDLY — an `unresolved reference` or a parse error — so
none is a wrong answer. They are recorded with what the reference produces so
the next round starts from a measurement rather than a guess.

| program | kotlinrs | reference |
| --- | --- | --- |
| `infix fun Int.pw(n: Int) = …; 2 pw 10` | `expected RParen, found Ident("pw")` | `1024` |

The `infix` row is a parser gap.

## `Regex` — what differs

`Regex` runs on `fancy-regex`, not `java.util.regex.Pattern`. `RegexOption`
arguments are folded into inline flags at compile time, so only constant
options (a single `RegexOption.X`, or `setOf`/`listOf` of them) are accepted; an
option held in a variable is refused as `unresolved reference: RegexOption`.
`CANON_EQ` is accepted and has no effect. A refused pattern raises
`java.util.regex.PatternSyntaxException` (catchable as the
`IllegalArgumentException` it is). For the syntax errors `Pattern` reports in
the constructs measured — unclosed and unmatched groups, unclosed classes and
reversed ranges, dangling quantifiers, malformed and reversed counted
repetitions, a trailing backslash, unknown inline modifiers, named groups and
back-references — the pattern is checked first and the message is `Pattern`'s,
`… near index N`, the pattern and the caret line. Any other refusal carries the
Rust engine's description instead.

## A lone surrogate has no `String` to live in

Kotlin's `Char` is a UTF-16 code unit and a `String` is a sequence of them, so
both can hold an unpaired surrogate. A Rust `String` is a sequence of Unicode
SCALARS and cannot, so every place kotlinrs would have to produce one it
produces `U+FFFD` instead. Measured on `kotlinc` 2.4.10 / JRE 21.0.12 — the
reference's own output is shown as its console renders it, which is a `?`
substitution, so none of these can be frozen in the corpus either:

| program | kotlinrs | reference |
| --- | --- | --- |
| `println(55296.toChar())` | `\u{FFFD}` | a lone `U+D800` (`?` on the console) |
| `println("𐐨"[0].titlecase())` | `\u{FFFD}` | a lone `U+D801` |
| `println("𐐨a".filter { true })` | `\u{FFFD}\u{FFFD}a` | `𐐨a` |
| `println("𐐨a".takeWhile { true })` | `\u{FFFD}\u{FFFD}a` | `𐐨a` |
| `println("i𐐀".split("", ignoreCase = true))` | `[, i, 𐐀, ]` | `[, i, ?, ?, ]` |

The `Char` members were the one part of this that was fixable without changing
the representation, and they are fixed: `uppercaseChar`, `lowercaseChar`,
`titlecaseChar` and the `code` of a surrogate now answer the code unit itself
rather than `U+FFFD`'s. What remains is every path that has to put a surrogate
INSIDE a string — the `CharSequence` collection members, which decompose a
receiver into `Char`s and rejoin them, and the empty-delimiter `split`, which
Kotlin cuts between code UNITS and kotlinrs between characters.

Closing it means carrying a `String` as `Vec<u16>` rather than as a Rust
`String`, which every member in `src/host.rs` would then have to understand.
The measurements above are the whole specification.

## Accepted where `kotlinc` rejects

Each of these compiles and runs here and is a compile error on the reference, so
a program that uses one is not portable back. None is a wrong answer — the value
is what Kotlin's own semantics would give if the spelling were legal — and each
is the cost of one dispatch rule that is otherwise right.

| program | kotlinrs | `kotlinc` |
| --- | --- | --- |
| `(-5).absoluteValue` | `5` | `unresolved reference 'absoluteValue'` (needs `import kotlin.math.absoluteValue`) |
| `listOf(1, 2, 3).scanReduce { a, b -> a + b }` | `[1, 3, 6]` | `unresolved reference 'scanReduce'` (removed in Kotlin 2.x) |
| `"ABC".toLowerCase()` | `abc` | `'fun String.toLowerCase(): String' is deprecated. Use lowercase() instead.` |
| `"abc".toUpperCase()` | `ABC` | same, for `uppercase()` |
| `"abc".distinct()` / `.sorted()` / `.sortedDescending()` | `[a, b, c]` | `unresolved reference` — the `CharSequence` overload does not exist |

The `String` collection-member rule is the reason for the last row: a
`CharSequence` receiver is decomposed into its `Char`s and handed to the
`Iterable` members, which gives the right answer for the two dozen that Kotlin
does declare and three it does not.

## `Float` in an erased position, closed

`Float` carries its width everywhere now. A value whose static type is thrown
away — a `List`/`Set`/`Map` element, a `Map` key, an `Any` binding or parameter
or return, a `data class` property, a lambda's result, a `%s` conversion — is
boxed as a `HeapObj::F32`, the same point the JVM boxes into `java.lang.Float`,
and the box is what the display, equality, ordering, hashing and arithmetic
paths read the width off.

What is left is narrower and stated exactly: an operator whose operands are
BOTH statically untyped and neither of which is a box computes in `f64`. That
needs a value that was never in a `Float`-typed position to begin with, so
nothing that starts from a `Float` literal, parameter, property or collection
element reaches it.

The `Int`/`Long` erasure is closed the same way, and the reason given here for
not closing it was WRONG: boxing a `Long` was said to cost the common integer
path, but nothing on that path passes through the boxing hook. `listOf(1, 2, 3)`
holds `Int`s and boxes nothing, and every native `Op::Add` still sees a
`Value::Int`. Only a `Long` entering a position that keeps no type boxes, which
is the same short list a `Float` does.

## `Map` members and key widths, newly measured

Found while pinning `Map` behaviour around the key index. Each is a feature gap
or an erasure limit, not a wrong answer produced silently — except the first,
which is.

| program | kotlinrs | reference |
| --- | --- | --- |
| `fun main() { data class K(val a: Int); … }` (a LOCAL data class) | `unexpected token Class` | compiles |

Everything this table used to hold is closed. `m[1]` and `m[1L]` are two keys
now, as they are on the JVM.

## More `unresolved reference`, newly measured

Each fails loudly with a diagnostic rather than answering wrongly. Recorded so
the next round has the measurement rather than a guess.

Every row this table held is closed: `enumValues<E>()` and a `Comparable<T>`
supertype (re-measured in round 16: `2` and `true`, as the reference answers),
and before them `orEmpty`/`isNullOrEmpty`/`isNullOrBlank`, `IntRange(a, b)`,
`iterator()`, `Result.success`/`failure`, `putAll`, `clear`, `::class` and
`javaClass`, all implemented and pinned in the corpus.

## `Delegates.observable` before the write, `vetoable` deciding it

Both work. The one difference from the JVM that a program could observe is the
`KProperty` a delegate is handed: this frontend passes `null` for it, having no
reflection object to build, so a delegate that reads `property.name` gets a null
receiver rather than the property's name. Every delegate that only reads and
writes the value — which is what `observable`, `vetoable` and the
`getValue`/`setValue` pair in the wild do — is unaffected.

## An identity hash is a small heap index

`Object.toString` is `<class>@<hex identity hash>`, and neither side's number is
reproducible — the reference's varies per run — so nothing here can be frozen in
the corpus. The shapes still differ: kotlinrs prints the heap slot, so a first
object is `C@0` where the reference prints a wide value like `C@6d9c638`. Only
programs that PRINT a bare object without a `toString` override are affected,
and no such program has a stable expectation on either side.

## A function VALUE prints as its arity, not as its signature

```
println(::f)                 kotlinrs: (lambda arity=2)   reference: fun f(kotlin.Int, kotlin.Int): kotlin.Int
println(String::length)      kotlinrs: (lambda arity=1)   reference: val kotlin.String.length: kotlin.Int
```

A closure carries its arity and nothing else, so there is no declaration to
render. The reference's form is a reflection surface — the callee's name, its
parameter and return types, and for a property reference its `val`/`var` — none
of which survives the lowering to a closure. Calling the value is unaffected;
only printing it is.

A REFERENCE is the whole gap; a plain lambda literal is not comparable at all.
`println({ x: Int -> x })` answers `BKt$$Lambda/0x00000070010118c8@497470ed` on
the reference — a synthetic class name carrying the loader's address and an
identity hash, both of which change from run to run — so there is nothing there
to match and nothing that could be frozen in the corpus.

## Round 16: measured and still missing

Each fails loudly. Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `java.util.LinkedList<Int>()` | `unresolved reference: java` | the JDK list |
| `(-1).toUInt()` / `5u + 3u` | `unresolved reference: toUInt on Int` / parse error | `4294967295` / `8` |


## `sin` and `cos` — the reference is a platform intrinsic

Measured on the reference JVM (JDK 21.0.12.1 on macOS aarch64, 400 000
sampled inputs per function): `Math.sin` differs from `StrictMath.sin` — fdlibm
— on 18 960 of them and `Math.cos` on 17 150, in the last place. Every other
`kotlin.math` function agreed with `StrictMath` on every sample, and those are
the ones ported (`tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`,
`tanh`, `cbrt`, `hypot`, `expm1`, `pow`), each then compared against the JVM
over 250 000 further inputs with no difference.

| program | kotlinrs | reference |
| --- | --- | --- |
| `import kotlin.math.*; println(sin(1.0))` | `unresolved reference: sin` | `0.8414709848078965` |

`kotlin.math.sin` IS `Math.sin`, and HotSpot answers it with a stub of its own,
which on x86_64 is a different algorithm again — so there is no
platform-independent answer for a port to reproduce, and an fdlibm port would
be wrong in the last digit on about one input in twenty here. Both stay loud.

## Measured this round and not closed

Each fails loudly. Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `fun m() { val k = 2; class L { fun f() = k }; println(L().f()) }` | `unresolved reference: k` | `2` |
| `println(check(true) == Unit)` | `unresolved reference: Unit` | `true` |

The first is the capture limit of a LOCAL class, which is hoisted to the top
level with no link back to its construction sites. Object expressions, hoisted
the same way, capture since round 20 (see CHANGELOG).

## Round 17: measured and still missing

Each fails loudly. Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `mutableMapOf("a" to 1).withDefault { 0 }.getValue("zz")` | `unresolved reference: withDefault on Map` | `0` |

`withDefault` is a VIEW over the receiver, and maps here have no view
representation (see "Collection views").

## Round 19: `sum()` over an `Int` collection the frontend cannot type

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1. The total of an `Int` collection
wraps at 32 bits; kotlinrs applies that width from the receiver's STATIC
element type, because a `List` holding plain integers cannot say at run time
whether they are `Int`s or unboxed `Long`s (`generateSequence(3000000000L) { …
}`, a `Long` range's `toList()`, `scan` over `Long`s all hold the latter). Where
the element type is not visible statically the total is computed at 64 bits:

| program | kotlinrs | reference |
| --- | --- | --- |
| `println(mapOf("a" to Int.MAX_VALUE, "b" to 1).values.sum())` | `2147483648` | `-2147483648` |

A literal, an annotated binding or parameter (`xs: List<Int>`), an `IntArray`,
and the results of `map`/`mapIndexed`/`mapNotNull`/`List(n) { … }`/`sumOf { … }`
whose lambda result is an `Int` all carry the width.

## Round 19: measured and not closed

Each fails loudly. Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `println(UInt.MAX_VALUE)` (any unsigned type or `u` literal) | `expected RParen, found Ident("u")` | `4294967295` |

## Round 20: measured and not closed

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `val a = 5u; println(a - 7u)` (any unsigned literal or type) | `expected RParen, found Ident("u")` | `4294967294` |
| `println(ULong.MAX_VALUE)` | `kotlin: unresolved reference: ULong` | `18446744073709551615` |
| `println(listOf(1, 2).stream().findFirst())` | `unresolved reference: Stream.findFirst (its java.util.Optional result is not modelled)` | `Optional[1]` |

Unsigned types need a design decision rather than a member port. `UInt`,
`UShort` and `UByte` values fit a non-negative `i64` and would only need new
static types (32/16/8-bit masking on `+ - *`, `toInt()` reinterpretation), but
`ULong` values at or above 2^63 share their bit pattern with negative `Long`s:
printing, comparing and dividing one correctly after it has passed through an
untyped collection needs the width carried on the value at run time, which is
the same substrate as consistent `Long` boxing.

`java.util.Optional` is not modelled, so the stream terminals that answer one
(`findFirst`, `findAny`, `max`, `min`, `average`, one-argument `reduce`) are
refused at compile time: lowered onto the sequence they would print the bare
value.

## Round 8: measured and not closed

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `open class A { init { println("A " + name()) }; abstract fun name(): String }` with `class B : A() { val nm = "b"; override fun name() = nm }` | `A ` | `A null` |

**A `String`-typed field read before its initializer.** A field the JVM has not
yet written is `null`, and a value statically typed `String` concatenates as
written, so the first row prints `A ` where Kotlin prints `A null`. A nullable
declaration (`String?`) renders correctly, as does every non-`String` type
(`Int` reads `0`). Making the concatenation test its operand costs an op on every
`String` `+`, for a program that is already outside what Kotlin promises.

**Which classes are built in two steps.** A class whose chain holds a throwable,
an enum constant with a body, or an object expression that captures keeps the
older build-then-extend construction, because the message, the cause and the
captured values are assembled around the finished instance. Its `init` blocks
may still not call a member of their own class, and a virtual call from a
superclass initializer lands on the superclass's method.

**Unsigned types** are not modelled at all (`5u`, `UInt`, `toUInt()`); see Round
20 for the representation question.

## A lazy pipeline materializes at its first non-lazy intermediate

A `for` loop and `seq.iterator()` pull a pipeline one element at a time since
round 21, through the lazy stages `map`, `filter`/`filterNot`, `onEach`,
`take`/`drop`, `takeWhile`/`dropWhile` and `withIndex`. Every other
intermediate (`mapNotNull`, `mapIndexed`, `flatMap`, `distinct`, `zip`,
`chunked`, `windowed`, …) pulls the pipeline into a list where it is applied,
so a block's side effects run ahead of the consumer's from that point on —
the elements and their order still agree. Measured on `kotlinc` 2.4.21 /
JRE 21.0.12.1:

| program | kotlinrs | reference |
| --- | --- | --- |
| `for (x in sequence { print("a "); yield(1); print("b "); yield(2) }.mapNotNull { it }) print("[$x] ")` | `a b [1] [2]` | `a [1] b [2]` |

## Round 10: measured and not closed

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| a runtime-raised throwable from an operation not in `jdk_prefix`'s table (e.g. `mutableListOf(1).iterator()` past its end through a view) | the user frames only | the JDK frames in front |
| `import java.util.*` then `kotlin.collections.ArrayDeque` `.push(0)` | `unresolved reference: push` | `java.util.ArrayDeque` wins the star import |
| `java.util.LinkedList`, `java.util.BitSet`, `java.util.Optional`, `System.getProperty` | `unresolved reference` | the JDK member |

The typed-builder row this table opened with (`mutableListOf<Long>(1)[0] is
Long`) is closed: a written or annotated element, key or value type now types
the literal arguments. Its `Byte`/`Short` mirror, the `inline fun` and
held-lambda frame rows, and much of the `jdk_prefix` row are closed by the
parity fan-out's sixth round (CHANGELOG); what remains of them is listed there.

## Round 21: measured and not closed

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | kotlinrs | reference |
| --- | --- | --- |
| `fun <T> id(x: T) = x`; `println(id(listOf(1)) + 5)` | `5.0` | `[1, 5]` |
| `open class M(val c: Int)`; `class N(c: Int) : M(c)`; `operator fun M.plus(o: M)` and `operator fun N.plus(o: M)`; `listOf<M>(N(1), M(2)).reduce { a, b -> a + b }` | `unresolved reference: plus on N` | `M.plus`, chosen by the static type `M` |
| `val a = arrayOf(1, 2); val l = a.asList(); a[0] = 7; println(l)` | `[1, 2]` | `[7, 2]` — a write to the ARRAY shows through the view; writes through the view reach the array since round 21 |

The first is an untyped left operand against a PRIMITIVE right one: the native
`Add` coerces a heap handle to a number. Routing every untyped arithmetic site
through the runtime operator would close it at the cost of a builtin call on
each one; an untyped left operand against a heap right one is already routed.

The second is extension choice by static type. A receiver of no static type
picks among user-class extensions by the value's runtime class, which is only
sound when no one instance could match two candidates; where it could (an
extension on a class and on its subclass), the chain declines and the call
fails loudly, because the runtime class does not say which the source meant.

A write through a `subList` or an `asList` view reaches its backing store
since round 21; reads through either are live since the views round. A `for` loop over a collection raises `ConcurrentModificationException`
from a size change or a modification-count change since round 21; a
structural write through a `subList` view does not bump the BACKING list's
count.

Member overloads resolve since the parity fan-out's fourth round, including
overload sets of the members the runtime calls by name and callable references
to an overloaded member (CHANGELOG, "Member overloads" and "Overloads of
host-called members"). A reference with no expected function type to select
by (`val f = C::f`) stays unresolved, as `kotlinc` reports it ambiguous.

## Round 22: measured and not closed

Measured on `kotlinc` 2.4.21 / JDK 25.0.5, compiled and run on the same JVM.

Both rows this section recorded — a boxed `Float` equal to a `Double`, and
`vararg` in a secondary constructor — are closed (CHANGELOG, "Boxed `Float`
equality, secondary-constructor `vararg`, IEEE neighbours, companion and
extension `var` writes").

## Round 23: measured and not closed

Measured on `kotlinc` 2.4.21 / JDK 25.0.5, compiled and run on the same JVM.

| program | kotlinrs | reference |
| --- | --- | --- |
| `data class V(val x: Int) { operator fun times(k: Int) = V(x * k) }`, `operator fun Int.times(v: V) = v * this`; `V(2) * 3` | `unresolved reference: x` | `V(x=6)` — the member operator, not the `Int` extension |
| `mapOf("x" to 1).withDefault { it.length * 10 }.getValue("yyy")` | `unresolved reference: withDefault on Map` | `30` |
| `class K { val y = 1; var Int.ext: Int get() = this + y; set(v) { println("set $v") }; fun go() { 5.ext = 3; println(5.ext) } }` — a MEMBER extension `var` with a setter | `member extension property ext with a setter is not supported (line 3)` | `set 3`, `6` |

Without the `Int.times` extension the member call answers `V(x=6)`; the cause
inside the operator lowering has not been traced. The member extension row
fails loudly by design: only the top-level form lowers its setter.

## Parity fan-out rounds 6–7: measured and not closed

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2, compiled and run on the same
JVM.

| program | kotlinrs | reference |
| --- | --- | --- |
| `inline fun boom(): Int { throw … }`; `boom()` caught in `main` | `MainKt.main(main.kt:<boom's line>)` | `MainKt.main(main.kt:<a line past the end of the file>)` |
| `val lz = { y: Int -> y / 0 }` on line 15, `listOf(1).map(lz)` on line 16 | `main(…:16)` for the caller | `main(…:25)` |
| `emptyList<Int>().reduce { a, b -> a + b }`, `listOf(1, 2).first { it > 5 }`, `last { it > 5 }` on line 2 | `main(main.kt:2)` | `main(main.kt:6)`, `6`, `10` |
| a lambda `{ x: Int ->` on line 3, `println("in")` on 4, `10 / x` on 5 | `main$lambda$0(main.kt:4)` | `main$lambda$0(main.kt:5)` |
| `mutableMapOf<Int, Int>().values.iterator().next()` | `ArrayList$Itr.next` in front | `LinkedHashMap$LinkedValueIterator.next` and `nextNode` |
| `class L { lateinit var s: String }`; `L().s` read from outside `L` | `MainKt.main(…)` | `L.getS(main.kt:1)` in front: the read goes through the getter |
| `Regex("(?x) a b # c")`, `Regex("\\0101")`, `Regex("\\cA")` | `PatternSyntaxException` (the engine's) | compiles |
| `UByteArray(3).also { it[1] = 200u }.toString()`; `byteArrayOf(-1).toUByteArray()` | `UByteArray(storage=[0, 200, 0])`; `unresolved reference` | `UByteArray(storage=[0, -56, 0])`; `[255]` |
| `(O.a)()` — a parenthesized callee | `expected RParen, found LParen` | invokes it |
| `"a\nb".lineSequence()` | `unresolved reference: lineSequence on String` | the lines |

The numeric declaration checks apply where the frontend can tell a type for
certain — a literal, a numeric local, arithmetic of those, a conversion — and
stay silent elsewhere, so a mismatch hidden behind an untyped call is still
accepted. The `lateinit` row needs a frame the VM never runs: the getter is not
a sub here. Variance is checked since round 8 (CHANGELOG); what it
does not reach is listed under "Variance — what is not checked". The three inline line rows
are the JVM's inline-function line mapping: a body copied into its caller is
numbered past the end of the file (the SMAP section), and a stack trace prints
those numbers as they are. The multi-line lambda row is a line attribution
miss: the body's tail expression carries no line of its own, so the frame takes
the nearest earlier one. The `(?x)` row is the engine's anchored second
compilation, whose closing group the trailing comment swallows.

## Variance — what is not checked

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2. Declaration-site and use-site
variance are checked since round 8, each only where the answer is certain, so
every gap below is a program ACCEPTED that `kotlinc` refuses — none refuses a
valid one.

| program | kotlinrs | reference |
| --- | --- | --- |
| `class J<out T> { inner class K { fun f(x: T) {} } }` | compiles | `type parameter 'T' is declared as 'out' but occurs in 'in' position …` — an inner class's members are checked against the outer parameters |
| `class D<out T> : Comparable<T>` | compiles | `… occurs in 'in' position in type 'Comparable<T (of class D<out T>)>'` — supertypes are not checked |
| `fun f(b: Box<String>) { val x: Box<Any> = b }` | compiles | `initializer type mismatch …` — only a LOCAL's written type (its annotation, or an explicit `Box<String>(…)` construction) is tracked, not a parameter's |
| `val b: Box<Number> = Box(1)` then `val c: Box<Int> = b.also { }` | compiles | refused — a projection or mismatch is seen only through a bare local name or an explicit construction |

The use-site checks name standard-library members only for the receivers
measured: `add`, `set` and `put` on `MutableList`/`MutableSet`/
`MutableCollection`/`MutableMap`, their `java.util` aliases and `Array`, and the
indexed read of a list or array.
