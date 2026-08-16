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
| `listOf(1).iterator()` | `unresolved reference: iterator on List` | (works; `next()` past the end is a bare `java.util.NoSuchElementException`) |
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
| `fun main() { data class P(val a: Int) ; println(P(1)) }` | `unexpected token Class (line 1)` | `P(a=1)` |
| `listOf(1, 2, 3).forEach lit@{ if (it == 2) return@lit }` | `a label must precede a loop (`for`/`while`/`do`), found LBrace` | (runs; the label names the lambda) |

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
