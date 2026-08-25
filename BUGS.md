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
| `class C { lateinit var s: String }` then `C().s` | parse error: `expected \`fun\` or a property, found Ident("lateinit")` | `kotlin.UninitializedPropertyAccessException: lateinit property s has not been initialized` |
| `mapOf("a" to 1).getValue("z")` | `unresolved reference: getValue on Map` | `java.util.NoSuchElementException: Key z is missing in the map.` |
| `throw RuntimeException("outer", IllegalStateException("inner"))` | `unresolved reference: RuntimeException` | `java.lang.RuntimeException: outer` |
| `e.cause` | `unresolved reference: cause on <class>` | `null`, or the chained throwable |
| `kotlin.math.ln(0.0)` | `unresolved reference: kotlin.math.ln` | `-Infinity` |
| `val café = 1` | `unexpected character 'Ã'` | `1` |
| `` val `odd name` = 1 `` | `` unexpected character '`' `` | `1` |
| `"\uD83D".length` (a lone surrogate) | `invalid unicode scalar in literal` | `1` |
| `"%1\$s".format("a")` (positional) | `UnknownFormatConversionException: Conversion = '$'` | `a` |

`cause` and the two-argument `(message, cause)` constructor are the one entry
here that belongs to this round's theme by subject. They are declined on size,
not on relevance: `HeapObj::Exc` carries `{ class, msg }` and threading a
`cause` through it touches the constructor selection, the property read and the
`toString` chain. The measurement above is the whole specification for whoever
takes it.

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

## Collection VIEWS are snapshots

`asReversed()`, `Map.keys`/`Map.values` and `subList()` are declared to be LIVE
VIEWS of their receiver: Kotlin's documentation for each says a change to the
backing collection shows through. Every one of them is a copy here, taken when
the member is called, so the view stops tracking at that moment. Measured on
`kotlinc` 2.4.10 / JRE 21.0.12:

| program | kotlinrs | reference |
| --- | --- | --- |
| `val b = mutableListOf(1, 2, 3); val v = b.asReversed(); b.add(4); println(v)` | `[3, 2, 1]` | `[4, 3, 2, 1]` |
| `val m = mutableMapOf("a" to 1); val k = m.keys; m["b"] = 2; println(k)` | `[a]` | `[a, b]` |
| `val l = mutableListOf(1, 2); val s = l.subList(0, 2); l[0] = 9; println(s)` | `[1, 2]` | `[9, 2]` |

Every read that does not outlive a mutation of the backing collection agrees, so
this is invisible until a program holds a view across a write. Closing it needs a
heap representation for a view — a `HeapObj` that carries a handle and a mapping
rather than its own elements — which every member that snapshots elements
(`list_snapshot`, the display path, indexing, `size`) would then have to
understand. That is the whole change; the measurements above are its
specification.

## Still missing, with the measurement

Each of these fails LOUDLY — an `unresolved reference` or a parse error — so
none is a wrong answer. They are recorded with what the reference produces so
the next round starts from a measurement rather than a guess.

| program | kotlinrs | reference |
| --- | --- | --- |
| `Regex("[0-9]+").findAll("a1b22c").map { it.value }.toList()` | `unresolved reference: Regex` | `[1, 22]` |
| `"hello".replace(Regex("l+"), "L")` | `unresolved reference: Regex` | `heLo` |
| `"a1b2".split(Regex("[0-9]"))` | `unresolved reference: Regex` | `[a, b, ]` |
| `"a".toRegex()` | `unresolved reference: toRegex on String` | `a` |
| `fun main() { data class P(val a: Int) ; println(P(1)) }` | `unexpected token Class (line 1)` | `P(a=1)` |
| `listOf(1, 2, 3).forEach lit@{ if (it == 2) return@lit }` | `a label must precede a loop (`for`/`while`/`do`), found LBrace` | (runs; the label names the lambda) |
| `listOf("a", "B").sortedWith(String.CASE_INSENSITIVE_ORDER)` | `unresolved reference: String` | `[a, B]` |
| `listOf(1, 2, 3).random(kotlin.random.Random(1))` | `unresolved reference: kotlin` | `1` |

`Regex` is the largest of the three by far: the class carries `find`/`findAll`/
`matches`/`containsMatchIn`/`replace`/`split` plus `MatchResult`'s
`value`/`groupValues`/`range`, and matching itself needs an engine this crate has
no dependency for. The other two are parser gaps — a class declaration inside a
function body, and a label on a lambda literal rather than on a loop.

`Double.MIN_VALUE` is absent deliberately and for a different reason: it is the
shortest decimal that round-trips a subnormal, and this frontend carries every
floating value as an `f64`, so it would print `5.0E-324` where Kotlin prints
`4.9E-324`. Leaving it unresolved keeps that divergence out of running programs
(see `primitive_const` in `src/compiler.rs`).

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

## `Float` loses its width in an ERASED position

`Float` is a static type now: a literal with the `f` suffix, a `Float`
annotation, `toFloat()`, `floatArrayOf`, the `Float` companion constants and
`kotlin.math`'s `Float` overloads all carry 32-bit width, arithmetic on them
rounds once at 32 bits, and they render through `Float.toString`. What is left
is every position where the value outlives the static type — a `List`, a `Map`,
an `Any` parameter, a `%s` conversion — because a `Float` and a `Double` are the
same `Value::Float` at run time and only the compiler knew which.

| program | kotlinrs | reference |
| --- | --- | --- |
| `println(listOf(1.0f / 3.0f))` | `[0.3333333432674408]` | `[0.33333334]` |
| `println(mapOf("k" to 1.0f / 3.0f))` | `{k=0.3333333432674408}` | `{k=0.33333334}` |
| `fun id(x: Any) = x; println(id(1.0f / 3.0f))` | `0.3333333432674408` | `0.33333334` |
| `"%s".format(1.0f / 3.0f)` | `0.3333333432674408` | `0.33333334` |

Note what is NOT in the table: `1.0f / 3.0f`, `(0.1f).toDouble()`,
`16777217.0f`, `Float.MAX_VALUE`/`MIN_VALUE`, `1.0e-45f`, `floatArrayOf(…)[0]`,
a `Float` parameter and a `Float` return all answer exactly what the reference
does. The arithmetic is genuinely single-precision — `16777217.0f * 0.2f` is
`3355443.2`, which computing in `f64` and narrowing afterwards cannot produce.

Closing the rest means carrying the width on the VALUE rather than on the static
type — a `Float` tag in the reserved handle region the way `Char` has one — so
that a value read back out of a `List` still knows it is 32-bit. That is also
what the `IllegalFormatConversionException` operand name above needs: `%d` of a
`Float` still says `d != java.lang.Double` where the reference says
`d != java.lang.Float`, for the same reason.

## `Map` members and key widths, newly measured

Found while pinning `Map` behaviour around the key index. Each is a feature gap
or an erasure limit, not a wrong answer produced silently — except the first,
which is.

| program | kotlinrs | reference |
| --- | --- | --- |
| `m[1] = "int"; m[1L] = "long"` on a `MutableMap<Any, String>` | one entry, `1=long` | two entries, `1=int, 1=long` |
| `fun main() { data class K(val a: Int); … }` (a LOCAL data class) | `unexpected token Class` | compiles |

This is the `Int`/`Long` counterpart of the `Float` erasure above: both widths
are `Value::Int` at run time, so `1` and `1L` are one key here and two on the
JVM.

## More `unresolved reference`, newly measured

Each fails loudly with a diagnostic rather than answering wrongly. Recorded so
the next round has the measurement rather than a guess.

| program | kotlinrs | reference |
| --- | --- | --- |
| `x::class` / `Int::class` | `expected a name after \`::\`, found Class` | a `KClass`; `.simpleName` is `Int` |
| `class Op(val v: Int) : Comparable<Op>` | `unresolved supertype Comparable of class Op` | compiles; `<` is the `compareTo` override |
| `enumValues<E>().size` | `unresolved reference: enumValues` | `2` (`E.values()` DOES work) |
| `e.javaClass.name` | `unresolved reference: javaClass on Exception` | `java.lang.Exception` |
| `1.javaClass` / `"x".javaClass` | `unresolved reference: javaClass on Int` | `int` / `class java.lang.String` |

`javaClass` needs a class-object value this frontend has no representation for,
and its answer for a primitive is the JVM's boxing rule rather than the
language's — `1.javaClass` is `int` where `"x".javaClass` is
`class java.lang.String`. `::class`, `enumValues<E>()` and a `Comparable<T>`
supertype are still open; the rest of what this table used to hold —
`orEmpty`/`isNullOrEmpty`/`isNullOrBlank`, `IntRange(a, b)`, `iterator()`,
`Result.success`/`failure`, `putAll` and `clear` — is implemented and pinned in
the corpus.

## `"%b".format(null)` reads a missing argument as `false`

```
"%b".format(null)   kotlinrs: false   reference: java.util.MissingFormatArgumentException: Format specifier '%b'
```

Not a `%b` bug. `format`'s parameter is `vararg args: Any?`, so a literal `null`
is the ARRAY and not an element: the reference sees a call with no arguments at
all and faults on the first specifier. kotlinrs packs the `null` into a
one-element array and `%b` then renders an absent value, which is `false` —
Java's `%b` answer for a null argument, and the right answer for the call this
was mistaken for. Closing it means telling a `null` spread from a `null`
element at the vararg packing site.

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

