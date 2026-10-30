# Changelog

Every expected value quoted here was measured, not recalled. The reference is
`kotlinc` 2.4.10 running on **JRE 21.0.12**, with the program **compiled** by
that JVM and **run** under `java -cp out:kotlin-stdlib.jar` on the same one.
Both axes are stated because they are separate: a `const val` is folded into the
class file under the COMPILER's `Double.toString`, while an identical literal
read at run time renders under the RUNTIME's, and the two disagree from JDK 19
on.

Earlier rounds are recorded in [README.md](README.md) under
`[0x06] STATUS & ROADMAP`; this file starts at the round that introduced it.

## `withDefault`

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `mapOf("x" to 1).withDefault { it.length * 10 }.getValue("yyy")` | `unresolved reference: withDefault on Map` | `30` |
| `val m = base.withDefault { 0 }; m["b"] = 2; println(base)` | refused | `{a=1, b=2}` |

`withDefault` answers a view over the map: `getValue` of an absent key asks the
default, and every other member, index read and write is made on the map it
wraps, whose later writes the view shows. A second `withDefault` replaces the
default over the same map, as `MapWithDefault` does.

## A built-in member operator beats a user extension of the same name

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `data class V(val x: Int) { operator fun times(k: Int) = V(x * k) }`, `operator fun Int.times(v: V) = v * this`; `V(2) * 3` | `unresolved reference: x` | `V(x=6)` |

An operator extension on a built-in type is consulted only where the built-in
member operator does not take the operand types, as Kotlin's members-first rule
has it.

## An untyped left operand under `+`/`-` is decided by its value

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `fun <T> id(x: T) = x`; `id(listOf(1)) + 5` | `5.0` | `[1, 5]` |
| `fun <T> first(xs: List<T>) = xs[0]`; `first(listOf(listOf(1, 2))) - 1` | `-1.0` | `[2]` |
| `fun <T> id(x: T) = x`; `id(2000000000) + id(2000000000)` | `4000000000` | `-294967296` |

An expression-bodied function whose body is a parameter declared with a type
variable answers that variable, so the call site reads the type off the
argument as it does for a written `: T`. A left operand that is still untyped,
against a primitive right one, goes through the runtime operator for `plus` and
`minus` — the two conventions a collection also answers — instead of a native
op that coerced a collection handle to a number.

## A map's `keys` and `values` carry its type arguments

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `println(mapOf("a" to Int.MAX_VALUE, "b" to 1).values.sum())` | `2147483648` | `-2147483648` |
| `val w: Map<String, Int> = mapOf("a" to Int.MAX_VALUE, "b" to Int.MAX_VALUE)`; `w.values.sum()` | `4294967294` | `-2` |

The element type of `m.keys` / `m.values` is the map's key / value type
argument, from its annotation or its `k to v` pairs, so the members that take
their width from the element type (`sum`, the lambda parameters of `map` and
friends) see it.

## A read-only list's `asReversed()` is not a `MutableList`

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `(listOf(1, 2).asReversed() as MutableList<Int>)[0] = 5` | succeeds | `ClassCastException: kotlin.collections.ReversedListReadOnly cannot be cast to kotlin.collections.MutableList` |
| `listOf(1, 2).asReversed() is MutableList<*>` | `true` | `false` |

## A constructor reference selects by the expected function type

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `class P(val a: Int) { constructor(s: String) : this(s.length) }`; `val f = ::P` | compiles, the primary | `overload resolution ambiguity between candidates` |
| the same `P`; `fun mk(f: (String) -> P) = f("xyz")`; `mk(::P).a` | the primary, holding `"xyz"` | `3` — the `String` secondary |

`::C` of a class with several constructors names the one whose arity and
parameter types the expected function type fits. A lambda or callable
reference passed to a function-typed parameter of a user function now takes
that type's parameters as its expected ones, as an annotated `val` already did.

## An enum initializes on first use, with its companion

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `enum class F { A; init { println("F init") } }`; `println("start"); F.A` | `F init` before `start` | `start`, then `F init` |
| `enum class F { A; companion object { init { println("comp") } } }`; `F.A` | nothing | `comp` |

The companion that holds an enum's constants is built on first use like any
other object with an effectful initializer, so the constants, then the
companion's own properties and `init` blocks, run at the enum's first access
— the JVM's class initialization. A companion `init` block keeps its place
among the companion's own properties rather than being displaced by the
constants prepended ahead of them.

## A passing `require`/`check` answers `Unit`

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `println(check(true) == Unit)` | `false` | `true` |

## Enum secondary constructors, enum constant defaults and named arguments, `String` member result types

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.

| program | before | reference |
| --- | --- | --- |
| `enum class F(val v: Int) { A(1), B("abc"); constructor(s: String) : this(s.length) }` | `class F: secondary constructor delegates to itself` | `B.v` is `3` |
| `enum class G { P(1), R; val k: String; constructor(i: Int) { k = "int$i" }; constructor() { k = "none" } }` | `enum constant G.P takes 0 argument(s), but 1 were given` | `P.k` is `int1`, `R.k` is `none` |
| `enum class E(val a: Int = 5, val b: String = "z") { X(), Z(b = "q") }` | `expected RParen, found Assign` | `X.a` is `5`, `Z.b` is `q` |
| `class K(val v: Int) { constructor(s: String) : this(s.length) }`; `K("ab".padStart(5)).v`, `K("x".repeat(2)).v` | `   ab`, `xx` — the `Int` primary | `5`, `2` — the `String` secondary |

A constant passes `name` and `ordinal` as NAMED arguments, and every secondary
constructor of an enum carries the same two as trailing parameters and forwards
them through its delegation, so a constant selects among the primary and the
secondaries, fills defaults and binds named arguments exactly as any other
construction site does. Constructor selection binds named arguments by name
before checking their types. An enum that writes no primary constructor has
none, as any other class: its implicit primary takes only the two `Enum`
properties and is reached by delegation alone. The `kotlin.text` members that answer a
`String` for a `String` receiver (`repeat`, `padStart`, `substring`, …) are
typed `String`, so the argument they produce selects a `String` constructor.

## Constructor `<init>` frames, comments-mode syntax checks, variance through inner classes, supertypes, parameters and calls

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `class Z(val d: Int) { val q = 10 / d }`; `Z(0)`, `stackTrace[0]` | `MainKt.main-e(main.kt:21)` | `Z.<init>(main.kt:2)` |
| `class N(val d: Int) { val c: Int = if (d > 0) N(d - 1).c else 1 / d }`; `N(2)` | no `<init>` frame | three `N.<init>(main.kt:14)` frames |
| `object Ob { val v = 1 / zero() }`; the cause of `Ob.v`'s `ExceptionInInitializerError` | `MainKt.main(main.kt:10)` | `Ob.<clinit>(main.kt:10)` |
| `Regex("(?x)a # (")`, `Regex("(?x:a #(\n)b")` | `Unclosed group …` | compile |
| `Regex("(?x)[#a]")`, `Regex("(?x)[ ]")` | the engine's message | `Unclosed character class near index 7`, `… near index 6` |
| `Regex("(?i)*a")`, `Regex("(?")` | the engine's message | `Dangling meta character '*' near index 4`, `Unknown inline modifier near index 2` |
| `Regex("(?x)[ ^ a]").containsMatchIn("b")` | `true` | `false` — a `^` after whitespace is a literal |
| `class J<out T> { inner class K { fun f(x: T) {} } }`; `class D<out T> : Comparable<T>` | compile | `type parameter 'T' is declared as 'out' but occurs in 'in' position …` |
| `fun f(b: Box<String>) { val x: Box<Any> = b }`; `fun mk(): Box<String>` then `val c: Box<Any> = mk()` | compile | `initializer type mismatch: expected 'Box<Any>', actual 'Box<String>'.` |

A construction is two subroutines — the allocating `C#$init` and the
initializing `C#$into` — and the JVM has one `<init>` frame, so a wrapper
frame directly outside its own class's INTO frame is folded into it instead of
every leading `<init>` being stripped; only a throwable's own constructors
are left out. A property initializer carries its declaration line. The
syntax checker tracks comments mode by group as the rewrite does. Variance is
checked on an inner class's members and on supertypes, and the written type of
a parameter, a call to a top-level function with a concrete written result, and
`also`/`apply` on a local reach the use-site checks.

## Regex comments mode and escapes, unsigned array storage, `(expr)()`, `lateinit` getter frames, statement lines, map-view iterator frames

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `Regex("(?x) a b # c").matches("ab")`, `Regex("a b # c", RegexOption.COMMENTS)` | `PatternSyntaxException` (the engine's) | `true` |
| `Regex("(?x)[a b]").containsMatchIn(" ")` | `true` | `false` — comments mode drops whitespace inside a class too |
| `Regex("\\0101")`, `Regex("\\cA")`, `Regex("\\c[")` | `PatternSyntaxException` | `A`, `\u0001`, `\u001b` |
| `Regex("\\08")`, `Regex("\\c")` | the engine's message | `Illegal octal escape sequence near index 2`, `Illegal control escape sequence near index 1` |
| `UByteArray(3).also { it[1] = 200u }` | `UByteArray(storage=[0, 200, 0])` | `UByteArray(storage=[0, -56, 0])` |
| `byteArrayOf(-1, 2).toUByteArray()`, `ubyteArrayOf(255u, 1u).toByteArray().toList()` | `unresolved reference` | `UByteArray(storage=[-1, 2])`, `[-1, 1]` |
| `(O.a)()`, `(h)(1)` | `expected RParen, found LParen` | invokes the value |
| `class L { lateinit var s: String }`; `L().s` | `MainKt.main(…)` heads the trace | `L.getS(main.kt:1)` heads it |
| `println("a")` on line 2, `val y = 10 / x` on line 3, in a function or lambda | `…(main.kt:2)` | `…(main.kt:3)` |
| `mutableMapOf<Int, Int>().values.iterator().next()` | `ArrayList$Itr.next` in front | `LinkedHashMap$LinkedHashIterator.nextNode`, `LinkedHashMap$LinkedValueIterator.next` |

A pattern is rewritten into the engine's dialect before it compiles: comments
mode is applied here, flag scope by group included, so neither the engine's
own comments mode (which keeps whitespace in a class) nor its anchored
`\A(?:…)\z` compilation sees a comment; `\0` octal and `\cX` control escapes
become code points; a quoted literal's whitespace survives comments mode. An
unsigned array element is stored at the array's width and rendered as its
signed storage. A non-`private` `lateinit` property's getter frame — declaring
class, a companion, an `object` or the file facade, and the declaration line —
is registered before `main` and put at the head of the trace of a read that
faults. Every statement's first op carries the statement's line, so an op that
has none (a division) is attributed to its own statement. A map's
`keys`/`values`/`entries` iterator names the map's own iterator class for that
projection (`LinkedHashMap`, `HashMap`, `TreeMap`).

## Variance: declaration-site positions, use-site projections, invariant initializers

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `class Box<out T>(var v: T)` | compiles | `type parameter 'T' is declared as 'out' but occurs in 'invariant' position in type 'T (of class Box<out T>)'.` |
| `class Sink<in T> { fun get(): T? = null }` | compiles | `… declared as 'in' but occurs in 'out' position in type 'T? (of class Sink<in T>)'.` |
| `class H<out T> { fun g(): Pair<ArrayList<T>, Int> = TODO() }` | compiles | `… 'invariant' position in abbreviated type 'Pair<ArrayList<T (of class H<out T>)>, Int>'.` |
| `val b: Box<Any> = Box<String>("s")` (invariant `Box`) | compiles | `initializer type mismatch: expected 'Box<Any>', actual 'Box<String>'.` |
| `val ys: MutableList<out Number> = …; ys.add(3)` | compiles | `receiver type 'MutableList<out Number>' contains out projection which prohibits the use of 'fun add(element: E): Boolean'.` |
| `val c: Box<in Int> = Box<Number>(1); val x: Int = c.get()` | compiles | `initializer type mismatch: expected 'Int', actual 'CapturedType(in Int)'.` |
| `val b: Box<out Number> = Box(1); b.v = 3` | compiles | `assignment type mismatch: actual type is 'Int', but 'Nothing' was expected.` |
| `fun contains(x: @UnsafeVariance T)` | `expected identifier, found At` | compiles |

The parser keeps every annotation AS WRITTEN beside the coarse type — names,
projections, nullability, function types (`TypeExpr`) — together with each
class's declared variances and its members' written signatures. The
declaration check walks every non-`private` member type with the position
composition `kotlinc` uses (a function parameter flips, an invariant argument
pins) against the declared variance of user classes and of the measured
standard-library generics; an unknown generic contributes nothing. The use-site
checks read the written type of a local — its annotation, or the explicit type
arguments of the construction that initialized it — and decide only where the
answer is certain: equal heads with known variances, and the final builtins.
`<in T>` also records `T` as a type parameter now: `in` lexes as the keyword,
and the parameter-list scan had skipped the name after it, leaving a `T`
annotation to name a heap class `T` rather than the type variable. A type annotation (`@UnsafeVariance T`) is accepted and
turns the variance check off for that type, as it does on the reference.

What these checks do not reach is listed in BUGS.md, "Variance — what is not
checked".

## Boxed `Float` equality, secondary-constructor `vararg`, IEEE neighbours, companion and extension `var` writes

Measured on `kotlinc` 2.4.21 / JDK 25.0.5.

| program | before | reference |
| --- | --- | --- |
| `val f: Any = 2.0f; val d: Any = 2.0`; `f == d`, `f.equals(d)`, `listOf(2.0).contains(f)` | `true` | `false` |
| `val a: Any = 3f`; `when (a) { 3.0 -> "d"; 3f -> "f"; else -> "?" }` | `d` | `f` |
| `class P(val xs: List<Int>) { constructor(vararg v: Int) : this(v.toList()) }`; `P(1, 2, 3).xs`, `P(7).xs` | `expected Colon, found Ident("v")` | `[1, 2, 3]`, `[7]` |
| `import kotlin.math.*`; `1.0.nextUp()`, `1f.nextUp()`, `1.5.withSign(-1)`, `0.0.nextTowards(-0.0)`, `Double.MAX_VALUE.ulp` | `unresolved reference: withSign on Double` | `1.0000000000000002`, `1.0000001`, `-1.5`, `-0.0`, `1.99584030953472E292` |
| `class C { companion object { var count = 0 }; init { count++ } }`; `C.count = 5` | `unresolved reference: count` / `C` | assigns the companion's property |
| `var Box.double: Int get() = n * 2; set(v) { n = v / 2 }`; `b.double = 10`, `b.double += 4` | `expected Assign, found Dot` | calls the setter |
| `class Q(var v: Int)` beside `var Q.v: Int get() = -1 …`; `Q(1).v` after `q.v = 5` | `-1` | `5` — a member property wins over an extension |

A boxed `Float` now equals only another boxed `Float`, as `Float.equals`
checks the class first; the same-width comparisons stay `true` because every
erased `Float` operand — `==` against `Any`, a collection element or argument,
and now a `when` arm against an `Any` subject — is boxed before it reaches the
comparison. `withSign`, `nextUp`, `nextDown`, `nextTowards` and `ulp` take a
`Float` receiver's 32-bit lattice through `#f32` runtime names, since the bare
value does not carry its width. A `vararg` secondary constructor is selected
for any trailing count and for a spread, and a primitive argument no longer
selects a primary whose parameter names a collection or user class.

## Numeric type checks, range members, typed arrays, iterator frames, `Pattern` messages, initialization order

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `val x = 5; val b: Byte = x`; `val l: Long = x`; `val d: Double = 1`; `val i: Int = 5L` | runs | `initializer type mismatch: expected 'Byte', actual 'Int'.` (and so on) |
| `var b: Byte = 1; b += 1`; `var i = 3; i = 5L` | runs | `assignment type mismatch: actual type is 'Int', but 'Byte' was expected.` |
| `fun f(b: Byte) = b; f(x)` | runs | `argument type mismatch: actual type is 'Int', but 'Byte' was expected.` |
| `(1..10).reduceRightIndexed { … }`, `(1..4).takeLast(1)`, `('a'..'d').takeLast(1)` | runs | `unresolved reference 'reduceRightIndexed' on receiver of type 'IntRange'.` |
| `val l: List<Any> = byteArrayOf(1, 2).toList()`; `l.contains(1)`, `l[0] is Byte` | `true`, `false` | `false`, `true` |
| `mutableMapOf<Int, Int>().iterator().next()`, a set's, `hashMapOf`'s, `setOf()`'s | user frames only | the `LinkedHashMap`/`HashMap`/`EmptyIterator`/`Collections$1` frames in front |
| `val lz = { y: Int -> y / 0 }` on line 4 called on line 5 | `main$lambda$0(main.kt:5)` | `main$lambda$0(main.kt:4)` |
| `Regex("a)")`, `Regex("[b-a]")`, `Regex("a{2,1}")`, `Regex("x{")`, `Regex("(?<n>a)(?<n>b)")` | the engine's description, or accepted | `Unmatched closing ')' near index 0` + pattern + caret, … |
| `class K { init { println(y) }; val y = 3 }`, `val a = b + 1; val b = 2` | runs | `variable 'y' must be initialized.` |
| `object O { val a = { 5 } }`; `O.a()` | `(lambda arity=1)` | `5` |

A `ByteArray`/`ShortArray` keeps its elements boxed, so they leave the array as
`Byte`s/`Short`s; a 300 000-read byte-array loop retires the same instructions
as before (12.96–12.99 G against 12.96–12.98 G, debug build).

## Qualified `Delegates`, retried `lazy`, `toSortedSet(comparator)`, conflicting inherited members

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `var o: Int by kotlin.properties.Delegates.observable(0) { … }` | `unresolved reference: properties` | runs |
| `Delegates.observable(…)` with no `import kotlin.properties.Delegates` | runs | `unresolved reference 'Delegates'.` |
| `val x by lazy { …throw… }` read twice, the first read throwing | `null` the second time | the initializer runs again |
| `listOf(P("b", 3), P("a", 1)).toSortedSet(compareBy { it.name })` | `unresolved reference: toSortedSet on List` | `[P(name=a, age=1), P(name=b, age=3)]` |
| `interface A { fun f() = "A" }`, `interface B { fun f() = "B" }`, `class C : A, B` | `A` | `class 'C' must override 'f' because it inherits multiple interface methods for it.` |
| `open class P { open fun f() = "P" }`, `interface I { fun f() = "I" }`, `class C : P(), I` | `P` | `… because it inherits multiple implementations for it.` |

## `Byte` and `Short` are types of their own

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `(listOf(1) as List<Any>)[0] is Byte` | `true` | `false` |
| `fun f(x: Any) = when (x) { is Byte -> …; is Short -> …; is Int -> … }`; `f(300.toShort())`, `f(b + b)` | the `Byte` arm for both | `S300`, `I20` |
| `(1.toByte() as Any) == (1 as Any)`; `listOf<Any>(1.toByte(), 1, 1.toShort(), 1L).distinct()` | `true`; `[1, 1]` | `false`; `[1, 1, 1, 1]` |
| `fun g(b: Byte)` / `g(s: Short)` / `g(i: Int)`; `g(s)`, `g(1)` | `byte`, `byte` | `short`, `int` |
| `var c: Byte = 127; c++` | `128` | `-128` |
| `b.javaClass`; `mapOf<Any, String>(1.toByte() to "b", 1 to "i")[1.toByte()]` | `class kotlin.Int`; `i` | `class kotlin.Byte`; `b` |
| `"%x".format((-1).toByte())`; `listOf<Byte>(1, 2).sum()` boxed into `Any` | `ffffffff`; a `Byte` | `ff`; an `Int` |
| `val b: Byte = 200` | `200` | `initializer type mismatch: expected 'Byte', actual 'Int'.` |

`Type::Byte`/`Type::Short` are static types that promote to `Int` at every
operator, and box to a `Byte`/`Short` heap value (`KT_BOX_NARROW`) at the
erased positions a `Long` and a `Float` already box at. `toByte()`/`toShort()`
on an `Int` is a native `Shl`/`Shr` pair, so a loop stepping a `Byte` stays in
the JIT: 3 000 000 iterations of `s += b * 3; b++` retire 0.19 G instructions
(debug build), and an `Int` loop's bytecode is unchanged (one through the host's `%` retires 0.6% more).

## A `when` whose value is used must be exhaustive

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `fun g(x: Int) = when (x) { 1 -> 2 }`, `val y = when (x) { 1 -> "a" }`, `println(when (x) { 1 -> "a" })`, `when { x > 0 -> "p" }` as a value | `null` for an unmatched value | `'when' expression must be exhaustive. Add an 'else' branch.` |
| `val f = { x: Int -> when (x) { 1 -> 2 } }; f(1)`; `listOf(1, 2).map { when (it) { 1 -> "a" } }` | `2`; `[a, null]` | `kotlin.Unit`; `[kotlin.Unit, kotlin.Unit]` |

The tail of a lambda — and of an `if`/`try` statement — is statement position:
a non-exhaustive `when` there runs for effect and the lambda answers `Unit`.

## Stack-trace frames the JVM back end gives

Measured on `kotlinc` 2.4.21 / JRE 21.0.13.0.2.

| program | before | reference |
| --- | --- | --- |
| `inline fun twice(f: () -> Unit)`; `twice { throw … }` | `main$lambda$0`, `twice`, `main` | `main` only |
| `val g = { x: Int -> … throw … }; g(1)` | `main` only | `main$lambda$1`, then `main` |
| `val k = { throw Exception("k") }; k()` | `main` only | `MainKt$main$k$1.invoke` twice, then `main` |
| an uncaught `throw` in a program | the `Exception in thread "main"` line alone | the line and the trace |
| `ArrayDeque().removeFirst()`, `setOf<Int>().first()`, `"abc".take(-1)`, `"ab".padStart(-1)`, `listOf(1, 2).chunked(0)`, `'x'.digitToInt()`, `mutableListOf(1).add(5, 1)`, `listOf(1, 2).subList(0, 5)`, `sequenceOf<Int>().last()`, … | the user frames only | the stdlib / `java.util` frames in front |
| `listOf(1).listIterator(5)` | `Index: 5` | `Index: 5, Size: 1` |

A lambda's sub name carries how `kotlinc` compiles it: a value held in a `val`
(its own frame), an argument of a user `inline fun` (none), or a `() ->
Nothing` class lambda (two). An `inline fun` is marked in the chunk's names.

## Erased numbers compare by class; `LongRange` keeps its width

Measured on `kotlinc` 2.4.21 / JDK 25.0.5.

| program | before | reference |
| --- | --- | --- |
| `val x: Any? = 2.0`; `x == 2`, `x in listOf(1, 2, 3)`; `val y: Any = 2`; `y in listOf(2.0)` | `true`, `true`, `true` | `false`, `false`, `false` |
| `(2L as Any) == (2.0 as Any)`, `(2.0f as Any) == (2 as Any)` | `true`, `true` | `false`, `false` |
| `(1L..3L).toList().contains(2L)`, `(1L..3L).toList() == listOf(1L, 2L, 3L)` | `false`, `false` | `true`, `true` |
| `val l = 5L`; `(1L..10L).contains(l)`, `(1..10).contains(l)` | `false`, `false` | `true`, `true` |
| `val b: Any = 5; b in 1L..10L`; `val a: Any = 5L; a in 1L..10L` | `true`, `false` | `false`, `true` |
| `val r: Any = 1L..3L; r is LongRange`, `(1L..3L).javaClass.name` | `false`, `kotlin.ranges.IntRange` | `true`, `kotlin.ranges.LongRange` |
| `(1..3) == (1L..3L) as Any`, `LongRange(1, 3) == 1L..3L` | `true`, `true` | `false`, `true` |
| `(-5L..5_000_000_000L).hashCode()` | `705032549` | `705032829` |
| `(1..9).start`, `(1..9).endInclusive` | `unresolved reference: start on IntRange` | `1`, `9` |

`value_eq`'s `Int`/`Double` arm compared numerically, on the stated worry that
a `Double`-typed result could still be a `Value::Int` at run time. Probing 55
`Double`-producing expressions (literals, conversions, `kotlin.math`, `op=`,
folds, array reads, loops) found none, and every `==` that reaches `value_eq`
across the two kinds is an erased one, since `kotlinc` refuses a typed `2.0 ==
2` and `1L == 1.0`. So the arm now answers `false`, as `Double.equals(Integer)`
does, and so do a boxed `Long` against any floating-point value and a boxed
`Float` against an integral one. A boxed `Float` against a bare floating-point
value still compares by value (see BUGS.md, round 22).

A `RangeObj` now records a `Long` width: a `Long` endpoint is boxed on its way
into `KT_RANGE` (the way a `Float` one already was), which makes the range a
`LongRange`/`LongProgression` whose elements are boxed `Long`s, with its own
class name, `equals` and `hashCode`. Membership follows the overload Kotlin
picks: a value statically `Int` or `Long` takes the numeric `contains` of
either width (`KT_IN_VM` carries that as a third argument), while an erased one
is `equals` against the elements. `r.contains(x)` answers by bounds rather than
by building every element first.

## `+x`, `a..<b`, and `op=` on a value

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `operator fun unaryPlus() = …`; `+Ver(1, 5)`; `+x * 2`, `+Int.MAX_VALUE + 1` | `unexpected token Plus` | the convention; the identity on a number, at its width |
| `for (i in 0..<3)`, `(1..<4).toList()`, `in 0..<5` in a `when` | `unexpected token Lt` | `0 1 2`, `[1, 2, 3]`, … |
| `m["a"]!! += 2`, `m.getOrPut("b") { mutableListOf() } += 3`, `mk() += 4` (a user `plusAssign`) | `invalid assignment target` | the `…Assign` convention on the value |

## `Result.mapCatching` / `recoverCatching`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `runCatching { 4 }.mapCatching { it / 0 }` | `unresolved reference: mapCatching on Result` | `Failure(java.lang.ArithmeticException: / by zero)` |
| `runCatching<Int> { error("x") }.recoverCatching { 9 }` | `unresolved reference` | `Success(9)` |

Each is a `fold` whose transforming half runs inside `runCatching`.

## A user class implementing `Sequence`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `class Countdown(val from: Int) : Sequence<Int> { override fun iterator() = … }`; `toList()`, `filter { }`, `for` | `unresolved supertype Sequence of class Countdown` | `[4, 3, 2, 1]`, … |
| an infinite `class Nat : Sequence<Int>`; `Nat().map { it * it }.filter { … }.take(3).toList()`, `first { it > 10 }` | refused | `[0, 4, 16]`, `11` |

A sequence member called on such an instance runs over `generateSequence`
pulling its iterator, so it stays lazy.

## `where` clauses

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `fun <T> pick(a: T, b: T, cond: Boolean) where T : Comparable<T> = …` | `top-level fun pick needs a body` | runs |
| `class Holder<T>(val v: T) where T : Comparable<T> { … }` | `found Ident("where")` | runs |

The bounds are consumed; the coarse typing keeps no type-variable bounds.

## A primary constructor spelled with its keyword

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `class Temp private constructor(val c: Double) { companion object { fun ofF(f: Double) = … } }` | `expected Class, found Ident("constructor")` | runs |
| `class A constructor(val x: Int)`, `class B internal constructor(val y: Int)` | parse error | runs |

The visibility modifier is accepted and not enforced.

## A non-exhaustive `when` over a closed type is a compile error

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `sealed class S`; `A`, `B : S()`; `fun f(s: S) = when (s) { is A -> 1 }`; `f(B())` | `null` | `'when' expression must be exhaustive. Add the 'is B' branch or an 'else' branch.` |
| `enum class E { X, Y }`; `when (e) { E.X -> 1 }` | `null` | `… Add the 'Y' branch …` |
| `when (b) { true -> 1 }` on a `Boolean` | `null` | `… Add the 'false' branch …` |
| a `when` STATEMENT over a sealed interface missing object `B` | ran, skipping the arm | `… Add the 'B' branch …` |
| two missing (`is B` and object `C`) | `null` | `… Add the 'is B', 'C' branches …` |

The subject's DECLARED type decides (`val s: S = B` is a `when` over `S`); an
intermediate sealed class is covered by an `is` of it or by covering all its
subclasses. A `when` over any other type without `else` is still accepted in
expression position (BUGS.md).

## Operator extensions on a known type; user `componentN` in destructuring

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `operator fun Pair<Int, Int>.plus(o: Pair<Int, Int>)`; `(1 to 2) + (3 to 4)` | `unresolved reference: plus on Pair` | `(4, 6)` |
| `data class V(val x: Int, val y: Int) { operator fun component3() = x * y }`; `val (a, b, c) = V(2, 5)` | `no component3 on V` | `2 5 10` |

A destructuring declaration now calls each `componentN()` as a member call on
a named stand-in, so a user class's own component functions are found.

## Stack traces: runtime-raised throwables, lambda frames, `printStackTrace`, uncaught reports

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, the program compiled as `main.kt`.

| program | before | reference |
| --- | --- | --- |
| `div(1, 0)` / `s!!` / `a as Int` / `require(false)` / `IntArray(1)[3]` caught, then `e.stackTrace` | `unresolved reference: stackTrace of a … the runtime raised` | the user frames |
| `listOf(1, 2)[5]`, `mutableListOf(1)[5]`, `"x".toInt()`, `"abc"[5]`, `emptyList<Int>().first()`, `mapOf(1 to 2).getValue(3)`, … | refused | `java.base/java.util.Arrays$ArrayList.get(Arrays.java:4266)` … then the user frames |
| a lambda passed to a non-inline `t(f)`; one nested in it; one in a method of `K` | no lambda frame, `t`'s line wrong | `MainKt.main$lambda$1(main.kt:5)`, `MainKt.main$lambda$3$0(…)`, `K.m$lambda$0(…)` |
| `e.printStackTrace()`, `e.stackTraceToString()` with a cause and a suppressed exception | `unresolved reference` | `Caused by: …`, `\tSuppressed: …`, `... n more` |
| an exception reaching the top of `main` | `kotlin: Exception in thread "main" …` | `Exception in thread "main" …` then `\tat …` lines (1024 at most, as the JVM keeps) |

A lambda's sub name now carries the JVM name `kotlinc` gives it (every literal
in a body numbered in order, nested ones after their parent) and the body it
was written in; a lambda run by that same body was run by an inlined stdlib
call and has no frame. The JDK and Kotlin-stdlib frames in front of a
runtime-raised throwable come from a measured table keyed by the raising
operation and the throwable's message (`jdk_prefix`). Limits are in BUGS.md.

## Sorted-collection ranges are live views

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `val h = t.headSet(6); t.add(2); println(h)`, `3 in h`, `for (x in h)` | `[1, 5]` (snapshot) | `[1, 2, 5]` |
| `m.tailMap("c")` after `m["e"] = 5; m["c"] = 3`; a comparator-ordered `headSet` | snapshot | the current range |
| `h.add(2)` / `h.remove(5)` / `h.clear()` / `tail[5] = "e"` through a range | changed the copy only | reach the source |
| `h.add(7)` / `tail[2] = "b"` outside the range | accepted into the copy | `IllegalArgumentException: key out of range` |

## Overloads of host-called members; references to overloaded members

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `override fun toString()` beside `fun toString(radix: Int)`; `equals(o)` beside `equals(o, slack)`; `compareTo(V)` beside `compareTo(Int)`; an iterator's `next()` beside `next(skip)` | `member overloads are not supported` | each call its own declaration; `println(v)`, `==`, `sorted()`, `for` use the plain one |
| `operator fun invoke(a: String)` / `invoke(a: Int)` / `invoke()`; `c(1)`, `listOf(C())[0](3)` | refused | `2`, `2` |
| `val f: (Printer, Int) -> String = Printer::put`; `p::put` as `(Int) -> String`; `listOf("x").map(p::put)` | `unresolved reference: put on Printer` | the declaration the function type selects |

In an overload set of a member the runtime calls by name, the declaration it
calls (the `override` of that arity, else the first) keeps its plain name and
the others are renamed by signature. The test that pinned the `next` overload
refusal is updated to the reference's `2` in its own commit.

## Unsigned arrays print their storage; `ULongRange` in unsigned order

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `println(uintArrayOf(1u, 2u))`, `UIntArray(2)`, `ulongArrayOf(1uL)` | `[Ljava.lang.Object;@…` | `UIntArray(storage=[1, 2])`, `UIntArray(storage=[0, 0])`, `ULongArray(storage=[1])` |
| `uintArrayOf(1u).javaClass.name` | `[Ljava.lang.Object;` | `kotlin.UIntArray` |
| `(ULong.MAX_VALUE - 1uL..ULong.MAX_VALUE).toList()`, `in`, `for`, `downTo`, `reversed()` | signed order: empty or wrong | `[18446744073709551614, 18446744073709551615]` … |
| `0uL until 0uL`, `0u until 0u` | `1..0`, `0..18446744073709551615` | `18446744073709551615..0`, `4294967295..0` (`EMPTY`) |

A `ULongRange` stores its endpoints with the sign bit flipped, which maps
unsigned order onto the signed comparisons the range code makes.

## Member overloads; extensions on a companion

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `class C { fun f(a: String) = 1; fun f(a: Int) = 2 }`; `C().f(1)` | `member overloads are not supported` | `2` |
| `put(Int)`/`put(String)`/`put(Long)`/`put(Shape)`/`put(Sq)` on one class; a subclass overriding `put(Int)`; an interface default `greet(Int)` beside an abstract `greet(String)` | refused | each call its own declaration |
| `listOf(Printer()).map { it.put(1) }`, `id(Printer()).put(2)` | refused | `int 1`, `int 2` |
| `fun String.Companion.hello() = "hello"`; `String.hello()`; `val String.Companion.E: String get() = "e"`; `fun C.Companion.plusBase(n: Int) = base + n` | `expected LParen, found Dot` | `hello`, `e`, the companion's `base` |

Each declaration of a method name some class repeats is renamed by its
parameter types (`put$ov(Int)`), in every class, so overrides agree; a call
selects by the arguments' static types as top-level overloads do. A receiver of
no static class tests the value: the selected overload on an instance of a
class declaring it, the stdlib member of the same name otherwise. The existing
test that pinned the refusal is updated to the reference's `2` in its own
commit.

## Unsigned integer types

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Every program was a parse error
(`expected RParen, found Ident("u")`) or `unresolved reference`.

| program | reference |
| --- | --- |
| `3u - 5u`, `5u * 1000000000u`, `UInt.MAX_VALUE + 1u` | `4294967294`, `705032704`, `0` |
| `ULong.MAX_VALUE / 3uL`, `10000000000uL * 10000000000uL` | `6148914691236517205`, `7766279631452241920` |
| `(-1).toUInt()`, `4000000000u.toInt()`, `(-1.5).toUInt()`, `200u.toByte()` | `4294967295`, `-294967296`, `0`, `-56` |
| `0u - 1u > 5u`, `listOf(3u, 10u, 2u).sortedDescending()` | `true`, `[10, 3, 2]` |
| `val x: UByte = 255u; (x + 1u).toUByte()`, `x is UByte` | `0`, `true` |
| `var i = 0u; i++; i += 2u`, FNV-1a over `"ab"` with `xor`/`*=` | `3`, `1294271946` |
| `for (i in 0u until 3u)`, `(10u downTo 8u).toList()`, `5u in 1u..10u` | `0 1 2`, `[10, 9, 8]`, `true` |
| `"ff".toUInt(16)`, `"256".toUByteOrNull()`, `"-1".toUInt()` | `255`, `null`, `NumberFormatException: Invalid number format: '-1'` |
| `1u / 0u` | `ArithmeticException: / by zero` |
| `(1u as Any) == (1 as Any)`, `UByte.MAX_VALUE.hashCode()`, `5u.javaClass` | `false`, `-1`, `class kotlin.UInt` |

A value is a heap box (`HeapObj::Unsigned`) of its width and bits; the
compiler types unsigned expressions (`unsigned_class`) so arithmetic reaches the
host's operator dispatch, and a literal takes a declared unsigned type from a
binding or parameter. A `ULongRange` above `Long.MAX_VALUE` and an unsigned array's printed form
are closed in the next round (see above).

## `Any()`, `synchronized`, `Throwable.stackTrace`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Each was `unresolved reference`.

| program | reference |
| --- | --- |
| `val a = Any(); a == Any(); a.toString()` | `false`, `java.lang.Object@…` |
| `synchronized(lock) { println("in") }` | `in` |
| `for (e in Throwable("x").stackTrace) println(e)` in `main` of `st.kt` | `StKt.main(st.kt:8)`, `StKt.main(st.kt)` |
| `Box().make().stackTrace`, an extension, a recursion, a `run { }` body, a user `Exception` subclass | the user frames, innermost first |

`stackTrace` is recorded when user code constructs the throwable: one element
per kotlinrs frame that has a JVM frame (`Cls.method(file.kt:line)`; top-level
and extension functions under the file's `…Kt` class), without the throwable's
own `<init>` frames, with lambda frames folded into their caller as an inline
`run`/`let` is on the JVM, and with the line-less bridge frame of a
parameterless `main`. The file name comes from the script path (`main.kt` for
`-e`). A throwable the runtime raised has JDK frames this frontend does not
have, so its `stackTrace` is refused loudly (BUGS.md).

## Collection views read live

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `val s = a.subList(1, 3); a[1] = 20; println(s)` | `[2, 3]` | `[20, 3]` |
| `arr.asList()` / `intArrayOf(3, 1, 2).asList()` after `arr[0] = 10` / `ia.sort()` | the old elements | the array's |
| `m.keys`, `m.values`, `m.entries` after `m["b"] = 2` | the old entries | the map's |
| `a.asReversed()` after `a[0] = 100` | the old elements | reversed live |
| `b.asReversed()[0] = 9` on a `MutableList` | dropped silently | writes `b`'s last element |
| `t[0] = 300` with `t = a.subList(1, 5).subList(1, 3)` | reached the middle view only | reaches `a` |
| any use of a `subList` after its backing list changed size | answered | `ConcurrentModificationException` |

A view keeps its own heap copy and is re-copied by the next read after its
backing object is written; `for` loops and indexing over a view read the
backing directly, so a loop over a view that writes the backing stays linear. A
`HashMap` write over an existing key no longer marks the map for re-ordering,
which kept such a loop quadratic. Sorted-collection ranges (`headSet`, …) are
still snapshots (BUGS.md).

## Explicit and annotated collection type arguments

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Kotlin gives an integer literal
the type it is passed as, so a written element, key or value type of `Long`
makes every literal element a `Long`.

| program | before | reference |
| --- | --- | --- |
| `mutableListOf<Long>(1)[0] is Long`, `setOf<Long>(1, 2)`, `arrayOf<Long>(1)` | `false` | `true` |
| `ArrayDeque<Long>()` then `addLast(5)`; `mutableListOf<Long>()` then `add(1)`, `add(0, 2)`, `+= 3`, `xs[0] = 4` | the elements were `Int` | `Long` |
| `mutableMapOf<Long, String>()` then `m[1] = "a"`, `m.put(2, "b")`, `m[1]`, `m.containsKey(2)` | `Int` keys, lookups missed | `Long` keys |
| `val xs: MutableList<Long> = mutableListOf(1)`, `val m: MutableMap<Long, Long> = mutableMapOf()` | `Int` | `Long` |
| `3 in mutableSetOf<Long>(3)`, `2L in mutableListOf(1L, 2L)`, `ArrayDeque(listOf(1L)).addFirst(7L)` | `false`, `false`, unboxed | `true`, `true`, `Long` |
| `val m: MutableMap<String, Int>`; `m["k"] = 2000000000`; `m["k"]!! + m["k"]!!` | `4000000000` | `-294967296` |

The parser now keeps a call's several type arguments (`<Long, Long>`) as well as
one; a map's key and value types also come from its `a to b` literals. A map
index read has the value type (nullable), which gives `m[k]!! + 1` its width.

## The recursion depth guard is one op

`KT_DEPTH_GUARD` was the entry op of a recursive function followed by a
`JumpIfFalse` over a `LoadUndef; ReturnValue` stub: two dispatched ops on every
call. The stub now sits just before the entry, where nothing falls into it, and
the guard resumes there itself (`ip -= 3`) only when a frame is entered past
`MAX_CALL_DEPTH` while `StackOverflowError` unwinds. `StackOverflowError`
behaviour is unchanged.

Retired instructions, `fib(27)`, debug build (`/usr/bin/time -l`):

| build | instructions | over unguarded |
| --- | --- | --- |
| no guard | 10.80 G | — |
| two-op guard | 11.93 G | 10.4% |
| one-op guard | 11.28 G | 4.4% |

## More JVM throwables; suppressed exceptions

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Each was `unresolved reference`.

| program | reference |
| --- | --- |
| `throw AssertionError("ae")` caught as `Error`, `AssertionError(42).message`, not caught by `catch (e: Exception)` | `java.lang.AssertionError: ae`, `42`, the `Throwable` arm |
| `OutOfMemoryError("oom")` caught as `VirtualMachineError` | `oom` |
| `catch (e: ConcurrentModificationException)` around a `for` that resizes its list | caught, `message` `null` |
| `InterruptedException`, `ArrayStoreException` | constructed, caught, printed |
| `e.addSuppressed(x)`, `e.suppressed.size`, `e.suppressedExceptions`, `e.addSuppressed(e)` | in order; a self-suppression is ignored (Kotlin's extension skips it) |

## `Long` values keep their width once erased

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `(1L..3L).first() is Long`, `.sum() is Long`, `.random() is Long`, `(5L downTo 1L).last() is Long` | `false` | `true` |
| `LongRange(1L, 3L).filter { it == 2L }` | `[]` | `[2]` |
| `(1L..3L).asSequence().map { it is Long }` | `[false, false, false]` | `[true, true, true]` |
| `5L.absoluteValue is Long`, `5.mod(3L) is Long`, `5L.mod(3) is Int` | `false`, `false`, `false` | `true`, `true`, `true` |

An erased position now boxes a value that `infer` knows is a `Long` even
where the member's lowering answered untyped; a `Long` range's `sum()`, the
`LongRange(a, b)` constructor, `asSequence()` and `absoluteValue` carry the
width, and `mod` answers the divisor's width. A generic constructor's
explicit type argument (`mutableListOf<Long>(1)`) is still not read (BUGS.md).

## `Char.category`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Each was `unresolved reference`.

| program | reference |
| --- | --- |
| `"aZ5 _-(é…".map { it.category }` | `[LOWERCASE_LETTER, UPPERCASE_LETTER, DECIMAL_DIGIT_NUMBER, SPACE_SEPARATOR, …]` |
| `'a'.category.code`, `'A'.category.value`, `'1'.category.ordinal` | `Ll`, `1`, `9` |
| `'a'.category == CharCategory.LOWERCASE_LETTER`, `when (c.category) { CharCategory.X -> … }` | compared as enum constants |
| `(0..0xFFFF).count { it.toChar().category == CharCategory.OTHER_LETTER }` | the JDK's count |

The categories come from `Character.getType` evaluated over the whole BMP on
the reference JVM and run-length encoded (`unicode_jdk::CATEGORY_RUNS`, 2 892
runs). `CharCategory` constants are enum instances with `name`, `ordinal`,
`value` and `code`, one per category, ordered by ordinal.

## An `Iterable` user object with no static class

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `class Bag(…) : Iterable<Int>`; `b.let { it.toList().size }`, `id(b).sorted()`, `id(b).map { … }` | `unresolved reference: toList on Bag` | the elements' list |
| `val any: Any = b; (any as Iterable<*>).joinToString()` | `ClassCastException: class Bag cannot be cast to class java.lang.Iterable` | `3, 1, 2` |
| `listOf<Iterable<Int>>(b, Bag(listOf(9))).map { it.sum() }` | `unresolved reference` | `[6, 9]` |

The compiler now publishes every class's `iterator()`/`hasNext()`/`next()` to
the host's member registry, so a collection member that reaches an `Iterable`
user object with no static type drains it through its own iterator and runs
over the elements; `is`/`as Iterable` accept such an object.

## A collection interface implemented `by` a delegate

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `class Logger(private val inner: MutableList<String> = mutableListOf()) : MutableList<String> by inner { override fun add(element: String): Boolean { … } }` | `` `by` delegation requires an interface supertype; MutableList is not one `` | compiles |
| `lg += "b"`, `lg.addAll(listOf("c"))` | — | `+=` reaches the OVERRIDDEN `add`; `addAll` is the delegate's |
| `lg.size`, `lg[0]`, `"A" in lg`, `lg.map { … }`, `for (x in lg)` | — | the delegate's elements |
| `class Registry(m: MutableMap<String, Int>) : MutableMap<String, Int> by m`; `r["x"] = 1`, `r.keys` | — | the delegate map |
| `class Names(names: List<String>) : List<String> by names { fun shout() = map { … } }` | — | an unqualified extension over the delegate |

The parser keeps the delegate field for a stdlib collection interface (it has
no declaration to forward from), and the compiler forwards every member,
index and `in` the class does not declare to that field; an `override` of one
of the interface's members is accepted on trust.

## Same-size modifications during a `for` loop

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `for (x in l) if (x == 1) { l.add(9); l.removeAt(3) }` | ran to the end | `ConcurrentModificationException` |
| `for (x in l) if (x == 3) l.sort()` over three elements, `l.replaceAll { … }`, `l.sortBy { … }` | ran to the end | `ConcurrentModificationException` |
| a `put` of a new key then its `remove`, over a `MutableMap` | ran to the end | `ConcurrentModificationException` |
| `l[1] = 5`, `m[k] = v + 1` over an existing key, `l.shuffle(…)`, `l.sort()` of one element | ran | ran — not structural |

Each collection carries a modification count (`MOD_COUNTS`), bumped by any
member call, operator or index write that changes its size, and by the
in-place sorts and `replaceAll` of a list of two or more — what `ArrayList`
counts. A `for` loop records it at its head (`KT_MODCOUNT`) and `KT_ITER_MORE`
compares it on every pass.

## `array.asList()` is an `Arrays$ArrayList` over the array

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `(arrayOf(1, 2).asList() as MutableList<Int>).add(3)`, `.remove(1)`, `.clear()` | the write happened on a copy | `UnsupportedOperationException` (`remove` with `: remove`) |
| `val a = arrayOf(1, 2); (a.asList() as MutableList<Int>)[0] = 9; a.toList()`, `.sort()` through the view | the array unchanged | `[9, 2]`, the array sorted |
| `arrayOf(1).asList()[5]` | `IndexOutOfBoundsException` | `ArrayIndexOutOfBoundsException: Index 5 out of bounds for length 1` |

The view is a `subList`-style view (see below) whose backing store is the
array, refused any size change at every length. A write to the ARRAY is not
yet seen through an earlier view (BUGS.md).

## `kotlin.Unit` is a value

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `val f = { x: Int -> println(x) }; println(f(1))`, `val g = fun(s: String) { s.length }; println(g("a"))` | `null` | `kotlin.Unit` |
| `val x: Int? = 5; println(x?.let { println(it) } == null)` | `true` | `false` |
| `fun u() {}`; `val a: Any = u(); println(a)`, `listOf(u())`, `"${u()}"` | `null`, `[null]`, `null` | `kotlin.Unit`, `[kotlin.Unit]`, `kotlin.Unit` |
| `listOf(1).mapNotNull { println(it) }` | `[]` | `[kotlin.Unit]` |
| `println(mutableListOf(1).clear())`, `.sort()`, `.add(0, 5)`, `putAll`, `fill` | `null` | `kotlin.Unit` |

`Unit` and `null` were one runtime value, with `kotlin.Unit` printed only
where the static type said `Unit`. It is now a heap singleton (`KT_UNIT`): a
`Unit` function's fallthrough and bare `return`, a lambda whose body ends in a
`Unit` expression, the name `Unit`, and the collection members declared to
answer `Unit` all produce it.

## Unbounded recursion raises `StackOverflowError`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `fun f(n: Int): Int = f(n + 1) + 1; println(f(0))` | the process grew until killed (exit 137) | `Exception in thread "main" java.lang.StackOverflowError` |
| the same inside `try { … } catch (e: StackOverflowError)`, mutual recursion, a method calling itself | never returned | caught |

A function that can reach itself through calls by name (computed over every
top-level, extension, member and local function) carries a `KT_DEPTH_GUARD`
test on entry: past `MAX_CALL_DEPTH` (1 000 000) frames it raises the error,
and every frame entered while it unwinds returns at once. The reference
overflows near 45 000 frames, so no finite depth it completes is affected.
Measured cost: `fib(30)` (2.7 M guarded calls) 3.71 s → 4.18 s in a debug
build, about 0.17 µs per call. Functions that cannot recurse pay nothing.

## A write through `subList` reaches the backing list

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `val l = mutableListOf(1, 2, 3, 4, 5); l.subList(1, 3).clear(); l` | `[1, 2, 3, 4, 5]` | `[1, 4, 5]` |
| `mutableListOf(1, 2, 3, 4).also { it.subList(1, 3).reverse() }`, `.sort()`, `.fill(8)`, `s[0] = 9`, `removeIf { }`, `sortWith(…)` | the backing list unchanged | the range rewritten |
| `val s = l.subList(0, 1); s.add(7)` | `l` unchanged | `7` inserted into `l` after the view's range |
| `(listOf(1, 2, 3).subList(0, 2) as MutableList<Int>).add(3)` | the write happened | `UnsupportedOperationException` — the backing `Arrays$ArrayList` refuses; `set` goes through |

Each `subList` records its backing list and range. A write through it re-reads
the view from that range, runs on the view, and splices the result back,
moving the range's end by the size change; a read-only backing list refuses the
write as its own class does.

## `ConcurrentModificationException` from a `for` loop

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `val l = mutableListOf(1, 2, 3); for (x in l) l.remove(x)` | `[2]` | `ConcurrentModificationException` |
| the same removing only `2` (the second-to-last) | `[1, 3]` | `[1, 3]` — the cursor sits at the end, so the loop ends |
| removing the LAST element, or `add` on the last pass | the loop ends | `ConcurrentModificationException` |
| `for ((k, _) in m) m[k + 10] = 0` over a `MutableMap` | the new entries iterated | `ConcurrentModificationException` |

A `for` over a collection (anything but a string, an array or a range) tests
with the new `KT_ITER_MORE` op, which reads the current size each pass and
follows the JDK iterator: `hasNext` is `cursor != size`, and a size change
under a cursor that is not at the end is what `next()` refuses. The test is
one host op per pass where the native compare was.

## A write through `keys`/`values`/`entries` reaches the map

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `val m = mutableMapOf(1 to 2, 3 to 4); m.keys.remove(1); m` | `{1=2, 3=4}` | `{3=4}` |
| `mutableMapOf(1 to 4, 2 to 4, 3 to 5).values.remove(4)` | the map unchanged | `{2=4, 3=5}` — the first entry holding the value |
| `m.entries.iterator()`, `next()`, `remove()` / the same over `keys`, `values` | the map unchanged | the entry removed |
| `m.keys.removeAll(…)`, `.retainAll(…)`, `m.values.removeIf { … }`, `m.keys.clear()` | the map unchanged | the selected entries removed |
| `m.keys.add(5)` | the element added to the copy | `UnsupportedOperationException` |

Each view records the map it projects; a write through it is applied to the
map entry by entry and the view re-read from it. Reads through a view held
across a later write to the MAP stay snapshots (BUGS.md).

## A throwing `object` initializer

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `object O { init { throw IllegalStateException("in init") }; val x = 1 }`; `O.x` | `IllegalStateException: in init` | `ExceptionInInitializerError`, its `cause` the `IllegalStateException` |
| the same `O.x` a second time | `0` — the half-built object | `NoClassDefFoundError: Could not initialize class O` (cause `ExceptionInInitializerError: Exception java.lang.IllegalStateException: in init [in thread "main"]`) |
| `object E { val y: Int = error("boom") }` beside `O`; `O.x` | `IllegalStateException: boom` — `E` was built at program start | only `O`'s own initializer runs |
| a companion's `init` throwing; `C.k` twice | the exception, then `0` | `ExceptionInInitializerError`, then `Could not initialize class C` |
| `object S { init { throw StackOverflowError() } }` | `StackOverflowError` | `StackOverflowError` — an `Error` is not wrapped |
| uncaught | `Exception in thread "main" java.lang.IllegalStateException: boom` | `Exception in thread "main" java.lang.ExceptionInInitializerError` |

Every `object` but one of literal constants is now built on first use, since
any other initializer may throw. The first-use call is bracketed by
`KT_OBJ_INIT_ENTER`/`KT_OBJ_INIT_EXIT`, which wrap an in-flight `Exception`,
record the object as failed, and clear its half-built global.

## User classes implementing `Iterable`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1.

| program | before | reference |
| --- | --- | --- |
| `class Bag(private val xs: List<Int>) : Iterable<Int> { override fun iterator() = xs.iterator() }`; `b.map { it * 2 }`, `b.sum()`, `4 in b`, `b.sorted()` | `unresolved supertype Iterable of class Bag` | the `Iterable` extensions over its elements |
| `fun total() = sum()` inside `Bag` | `unresolved reference: sum` | the elements' total |
| `fun mk(): Iterator<Int> = object : Iterator<Int> { … }`; `mk().hasNext()` | `unresolved reference: hasNext on <anonymous>$1` | dispatched to the object |
| `operator fun rangeTo(o: Ver) = VerRange(this, o)`; `Ver(1, 2) in Ver(1, 0)..Ver(1, 3)` | `false` — the range typed as an `IntRange` | `true` — the user `contains` |

An `Iterable` extension on a receiver whose static class implements
`Iterable` and does not declare the member is lowered over
`run { val it = recv.iterator(); val l = mutableListOf(); while (it.hasNext())
l.add(it.next()); l }`, once per call as Kotlin walks the iterator. A value
typed by a marker interface (`Iterator`, `Comparable`, …) dispatches to every
implementor at run time.

## The `…To(destination)` family; `ArrayList(initialCapacity)`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Each `…To` member was
`unresolved reference`.

| program | reference |
| --- | --- |
| `val d = mutableListOf(0); xs.filterTo(d) { it > 4 }; d` | `[0, 5, 9, 6]` |
| `xs.mapTo(mutableSetOf()) { it % 3 }`, `xs.toCollection(sortedSetOf())` | `[0, 1, 2]`, the sorted distinct elements |
| `xs.associateTo(sortedMapOf()) { it to it * it }`, `listOf("x").associateWithTo(sortedMapOf("a" to 0)) { it.length }` | the destination's order: `{1=1, 2=4, …}`, `{a=0, x=1}` |
| `listOf(1, 2, 3).groupByTo(mutableMapOf(1 to mutableListOf(100))) { it % 2 }` | `{1=[100, 1, 3], 0=[2]}` — an existing group is appended to |
| `"a1b2".filterTo(StringBuilder()) { it.isDigit() }` | `12` |
| `ArrayList<Int>(10).apply { add(1) }`, `ArrayList<String>(-1)` | `[1]`, `IllegalArgumentException: Illegal Capacity: -1` (was `unresolved reference: toMutableList on Int`) |

Each `…To` form runs its base member and adds (puts) the result into the
destination element by element, so a `Set` or a sorted destination applies
its own rules.
## `in` on an untyped subject, floating-point ranges, `codePointCount`

Measured on `kotlinc` 2.4.21 / JDK 25.0.5, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `fun d(x: Any?) = when (x) { in 1..3 -> "small"; in 4..10 -> "mid"; !in 1..100 -> "out"; else -> "big" }`; `d(2.0)`, `d(5L)`, `d("2")`, `d(null)`, `d(2.5f)` | `small`, `mid`, `small`, `out`, `small` | `out` for every one |
| `3.5 in 1.0..3.0` | `true` | `false` |
| `println(1.0..3.0)`, `(1.0..3.0).start`, `.endInclusive`, `(3.0..1.0).isEmpty()` | `1..3`, `unresolved reference: start on IntRange` | `1.0..3.0`, `1.0`, `3.0`, `true` |
| `Double.NaN in 1.0..3.0`, `-0.0 in 0.0..1.0`, `(0.0..1.0) == (-0.0..1.0)` | not measured | `false`, `true`, `true` |
| `(1.0..3.0).hashCode()`, `(1.0f..3.0f).hashCode()`, `(3.0..1.0).hashCode()` | not measured | `-31981568`, `-255852544`, `-1` |
| `(1.0..3.0)::class.simpleName`, `(1.0f..3.0f)::class.simpleName` | not measured | `ClosedDoubleRange`, `ClosedFloatRange` |
| `"a😀b😀".codePointCount(0, 6)`, `.codePointCount(1, 2)`, `"abc".codePointCount(2, 5)` | `unresolved reference: codePointCount on String` | `4`, `1`, `IndexOutOfBoundsException: Range [2, 5) out of bounds for length 3` |

`when (x) { in a..b -> }` lowered to two native comparisons whatever the
subject's type, so any number between the bounds matched. It keeps that
lowering only for a subject statically `Int`, `Long`, `Float`, `Double` or
`Char`. Any other subject builds the range and tests `contains`, and an
`IntRange` admits only an `Int` (or a `Char` for a `CharRange`): through an
erased position the JVM's test is `equals` per element, which no `Double`,
boxed `Long`, `String` or `null` passes.

`a..b` with a floating-point endpoint used to truncate both into an `IntRange`.
It now builds a `ClosedFloatingPointRange` that keeps its endpoints at their
own width. A `Float` endpoint is boxed for this, so the class, the rendering
and the hash follow it. The range has `start`, `endInclusive`, `contains`
(IEEE `<=` at both ends) and `isEmpty()`, plus structural `equals`/`hashCode`.
`until` and `downTo` over a `Double` are refused, as `kotlinc` refuses them.

`codePointCount(begin, end)` counts over UTF-16 indices: a surrogate pair inside
the window counts once, and a pair the window cuts counts per unit.

Three records were added to the parity corpus, and its floor raised by the same three.

## Array content hashes; `replaceAfter`/`replaceBefore`; string sequences

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Each was `unresolved reference`.

| program | reference |
| --- | --- |
| `intArrayOf(1, 2).contentHashCode()`, `longArrayOf(3000000000L, -1).contentHashCode()`, `arrayOf("a", null, "bc").contentHashCode()` | `994`, `-1489279551`, `126145` — `java.util.Arrays.hashCode` |
| `arrayOf<Any>(intArrayOf(1, 2), arrayOf("x")).contentDeepHashCode()` | `31926` — nested arrays by content |
| `(null as IntArray?).contentHashCode()`, `.contentToString()` | `0`, `null` |
| `"a.b.c".replaceAfter(".", "X")`, `.replaceBefore(".", "Y")`, `.replaceAfterLast(".", "Z")`, `"abc".replaceBefore("/", "Q", "miss")` | `a.X`, `Y.b.c`, `a.b.Z`, `miss` |
| `"abcdefg".chunkedSequence(3).toList()`, `"abcde".windowedSequence(2, 2, true).toList()` | `[abc, def, g]`, `[ab, cd, e]` |

## Arguments that were ignored: ranges, `addAll(index, …)`, `toCharArray(…)`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `listOf(1, 3, 5, 7, 9).binarySearch(7, 0, 3)` | `3` | `-4` |
| `listOf(…).binarySearch(7, 3, 1)` / an array's `(7, -1, 3)` | the whole list searched | `IllegalArgumentException: fromIndex (3) is greater than toIndex (1).` / `ArrayIndexOutOfBoundsException: Array index out of range: -1` |
| `intArrayOf(5, 4, 3, 2, 1).also { it.sort(1, 4) }` | `[1, 2, 3, 4, 5]` | `[5, 2, 3, 4, 1]` |
| `intArrayOf(1, 2, 3).copyInto(IntArray(5), 1, 1, 3)` | `unresolved reference: copyInto on Array` | `[0, 2, 3, 0, 0]`; a short destination is `arraycopy: last destination index 3 out of bounds for int[2]` |
| `mutableListOf(1, 4).apply { addAll(1, listOf(2, 3)) }` | `[1, 4]` | `[1, 2, 3, 4]` |
| `"hello".toCharArray(1, 3)` | all five characters | `[e, l]`; `(3, 1)` is `IllegalArgumentException: startIndex: 3 > endIndex: 1` |
| `listOf(1.0, Double.NaN, 0.5).minOf { it }` | `0.5` | `NaN` — the `Double` selector folds with `Math.min` |

## Read-only `Set`/`Map` writes; casts to a collection interface

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `(setOf(1) as MutableSet<Int>).add(2)`, `.remove(1)`, `.clear()` | the write happened | `UnsupportedOperationException` (`SingletonSet`); `remove(9)` is `false` |
| `(mapOf(1 to 1) as MutableMap<Int, Int>)[7] = 7`, `.getOrPut(5) { 9 }` | the write happened | `UnsupportedOperationException` (`SingletonMap`); `getOrPut(1) { 9 }` is `1` |
| `setOf<Int>() as MutableSet<Int>`, `emptyMap<Int, Int>() is MutableMap<*, *>` | the cast passed, `true` | `ClassCastException: kotlin.collections.EmptySet cannot be cast to kotlin.collections.MutableSet`, `false` |
| `listOf(1).toSet()` / `listOf(1 to 1).toMap()` written through a cast | the write happened | `UnsupportedOperationException` — `toSet`/`toMap` collapse by the RECEIVER's size, so `listOf(1, 1).toSet()` stays a writable `LinkedHashSet` |
| `val a: Any = 1; a as List<*>` | `class value cannot be cast to class List` | `class java.lang.Integer cannot be cast to class java.util.List (java.lang.Integer and java.util.List are in module java.base of loader 'bootstrap')` |

The read-only literal tag a `Set`/`Map` already carried for its JVM class name
now also gates the writes, exactly as the `List` provenance tag does, and is
dropped at the first write to a literal of two or more elements. A cast's
target `List`/`Set`/`Map`/`Collection`/`Iterable` names its JDK interface.

## `for` over a lazy sequence pulls one element per pass

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `for (x in sequence { print("y1 "); yield(1); print("y2 "); yield(2) }) print("b$x ")` | `y1 y2 b1 b2` | `y1 b1 y2 b2` |
| `for (x in iterator { print("i1 "); yield(5); print("i2 ") }) print("b$x ")` | `i1 i2 b5` | `i1 b5 i2` |
| `val it = s.iterator(); while (it.hasNext()) print("b${it.next()} ")` over `generateSequence(1) { … }.map { … }` | the whole pipeline first | one element per `next` |
| `for (x in ss) { print("b$x "); break }` over `sequence { print("A "); yield(1); print("B "); … }` | `A B C b1` | `A b1` |
| `var n = 0; for (x in generateSequence(1) { it + 1 }) { n += x; if (x == 70_000) break }` | `IllegalStateException: sequence produced more than 1000000 elements …` — the head pulled the endless source | `-1844932296` — `x` is an `Int` |
| `for ((i, v) in seq.withIndex())` | the pipeline pulled first | interleaved |

A `for` header the compiler cannot prove indexable — anything but a `String`,
an array, a range or an eager container — lowers to `KT_ITER_SRC_LAZY` /
`KT_ITER_PULL`: a lazy source becomes an iterator pulled once per pass, and the
loop ends at the end marker `KT_ITER_END` tests for. The pipeline traversal is
one `GenCursor` either way, which the terminal operations drive in a loop and
an iterator holds between pulls; `withIndex` is a lazy stage of its own. The
element type of `generateSequence(seed) { … }` is the seed's, so the step's
`it` and the loop variable have its width. A pipeline still materializes at
any other intermediate; see BUGS.md.

## Comparator builders over a comparator, `compareValues`, `minOfWith`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1. Each was `unresolved reference`.

| program | reference |
| --- | --- |
| `ps.sortedWith(compareBy(nullsFirst()) { it.age })`, `compareByDescending(nullsLast()) { it.age }` | the selector's results ordered by the comparator (descending swaps the operands) |
| `compareValues(null, 1)`, `compareValues("b", "a")`, `compareValues<Int>(null, null)` | `-1`, `1`, `0` |
| `compareValuesBy(P("a", 1), P("a", 2), { it.name }, { it.age })` | `-1` — the first non-zero selector decides |
| `listOf("aa", "b", "ccc").minOfWith(compareBy<String> { it.length }) { it + "!" }` | `b!` |
| `listOf<Int>().minOfWith(naturalOrder()) { it }`, `…minOfWithOrNull(…)` | `NoSuchElementException`, `null` |

`compareBy(cmp) { sel }` is `Comparator { a, b -> cmp.compare(sel(a), sel(b)) }`
with both evaluated once; `compareValues` is `nullsFirst()`'s comparator;
`minOfWith(cmp) { sel }` and its siblings are `map(sel)` reduced by
`minWith`/`maxWith`.

## Top-level function overloads

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `fun describe(x: Int)`, `(x: Long)`, `(x: String)`, `(x: Any?)`; `describe(1L)`, `describe("s")`, `describe(2.5f)` | `Int 1`, `Int s`, `Int 2.5` — every call ran the first declaration | `Long 1`, `String s`, `Any 2.5` |
| `fun area(r: Double)`, `fun area(w: Double, h: Double)`; `area(2.0, 3.0)` | `function area has no argument for …` / the wrong body | `6.0` |
| `fun describe(x: Animal)`, `fun describe(x: Dog)`; `val a: Animal = Dog("d"); describe(a)`, then `if (a is Dog) describe(a)` | `Dog d`, `Dog d` | `Animal d`, `Dog d` |
| `fun nums(xs: List<Int>)`, `(xs: Set<Int>)`, `(xs: Map<Int, Int>)` | the `List` body for all three | each its own |
| `class C { fun f(a: String) = "s"; fun f(a: Int) = "i" }`; `C().f(1)` | `s` (silently) | `i` — now a compile error here, recorded in BUGS.md |

Each declaration of a repeated top-level name is renamed `name$ov<k>` before
the signature table is built, and a call scores every candidate that can bind
its arguments: 3 for the parameter's own type, 2 for a subtype or an integer
literal taking a `Long`, 1 where the static type does not say, out where it
cannot be passed; the highest total wins, the earlier declaration on a tie.
The static type is a binding's annotation where it named a user class, and an
`is` smart cast narrows it for the branch.

## Members that answered `unresolved reference`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.
Each failed loudly before; each now answers the reference's value.

| program | reference |
| --- | --- |
| `IntRange.EMPTY`, `LongRange.EMPTY`, `CharRange.EMPTY.count()` | `1..0`, `1..0`, `0` |
| `emptySequence<Int>().toList()`, `"a\nb\r\nc\n".lineSequence().toList()` | `[]`, `[a, b, c, ]` |
| `12.345.toBits()`, `Double.NaN.toRawBits()`, `1.5f.toBits()`, `Float.NaN.toBits()` | `4623139235229744497`, `9221120237041090560`, `1069547520`, `2143289344` |
| `Double.fromBits(4613937818241073152L)`, `Float.fromBits(1036831949)` | `3.0`, `0.1` |
| `var i = 0; generateSequence { if (i < 3) i++ else null }.toList()` | `[0, 1, 2]`; a second traversal throws `IllegalStateException: This sequence can be consumed only once.` |
| `sequenceOf(1, 2).constrainOnce()` traversed twice | the same `IllegalStateException`; the receiver stays re-iterable |
| `java.lang.String.valueOf(5)`, `java.lang.String.valueOf(null as String?)` | `5`, `null` |
| `listOf(Int.MAX_VALUE, 1).sumBy { it }`, `sumByDouble { … }` | `-2147483648` (an `Int` total), the `Double` total |
| `Integer.reverse(1)`, `Integer.signum(-5)`, `Integer.toUnsignedString(-1)`, `Integer.divideUnsigned(-1, 2)` | `-2147483648`, `-1`, `4294967295`, `2147483647` |
| `java.lang.Long.reverseBytes(1L)`, `java.lang.Long.toUnsignedString(-1L)` | `72057594037927936`, `18446744073709551615` |
| `java.lang.Double.compare(0.0, -0.0)`, `java.lang.Double.isNaN(0.0 / 0)`, `java.lang.Double.hashCode(1.5)` | `1`, `true`, `1073217536` |

The seedless `generateSequence` carries the lateinit marker in its seed
position, which no program value can be, so the traversal knows to ask the
function for the first element too. A once-only sequence carries a token in a
side table that every pipeline derived from it shares, so traversing the
pipeline spends the source.

## `sequence { }` mid-expression, sequence operators, `Sequence.shuffled`, `toMap(destination)`

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `val s = sequence { yield(2) }; println(listOf(7, 8, s.first()))` | `[7, null, 2]` | `[7, 8, 2]` |
| `println("x" + iterator { yield(5) }.next())` | `5` | `x5` |
| `(sequenceOf(1, 2, 3) - 2).toList()`, `(generateSequence(…) + 9).toList()` | `unresolved reference: toList on Double` | `[1, 3]`, `[1, 2, 3, 9]` |
| `sequenceOf(1, 2, 3, 4).shuffled(Random(42)).toList()` | `[3, 2, 4, 1]` | `[1, 4, 2, 3]` |
| `(1..20).associate { "k$it" to it }.toMap(HashMap()).keys.take(8)` | `unresolved reference: toMap on Map` | `[k1, k2, k3, k20, k4, k5, k11, k6]` |

A `sequence { }`/`iterator { }` block is a lambda with a `SequenceScope`
receiver, so its prologue pops one parameter; entering the coroutine pushed
none, and the prologue took the consumer's operand below instead.
`Sequence.shuffled` is its own algorithm — `j = nextInt(buffer.size)` over a
shrinking buffer, the last element moved to `j` — not the `Iterable` walk.
`toMap(destination)` is `destination.apply { putAll(this) }`, so the
destination's class decides the order.

## A read-only `List` written through a `MutableList` cast

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.
`fun <T> m(l: List<T>) = l as MutableList<T>`:

| program | before | reference |
| --- | --- | --- |
| `m(listOf(1)).add(7)`, `.set(0, 9)`, `.clear()` | the write happened | `UnsupportedOperationException` (`SingletonList`) |
| `val l = listOf(1, 2); m(l)[0] = 9; l` | `[9, 2]` | `[9, 2]` — `Arrays$ArrayList` allows `set` |
| `m(listOf(1, 2)).add(7)`, `m(listOf(3, 1).sorted()).add(1)` | the write happened | `UnsupportedOperationException` |
| `m(listOf(1, 2)).remove(1)`, an iterator's `remove()` | `true` | `UnsupportedOperationException: remove` |
| `m(listOf(1, 2)).remove(9)`, `.addAll(listOf())`, `m(listOf(1)).reverse()` | as before | succeed: nothing changes, so nothing is refused |
| `val a = listOf(1, 2, 3, 4); m(a).removeAll { it % 2 == 1 }; a` | `true`, `[2, 4]` | `UnsupportedOperationException`, then `[2, 4, 3, 4]` |
| `m(buildList { add(1) }).remove(9)`, `m(buildList { add(2); add(1) }).sortBy { it }` | succeeded | `UnsupportedOperationException` (`ListBuilder`) |
| `m(listOf<Int>())`, `listOf<Int>() is MutableList<*>` | the cast passed, `true` | `ClassCastException: kotlin.collections.EmptyList cannot be cast to kotlin.collections.MutableList`, `false` |

The class behind a read-only list was already tracked for its index-fault
wording (`ListImpl`); the mutating members, `xs[i] = v`, `+=`/`-=`, the
iterator's `remove`, `removeIf`/`removeAll { }`/`retainAll { }`,
`replaceAll` and the in-place sorts now consult it. `removeAll { }` over an
`Arrays$ArrayList` reproduces Kotlin's `filterInPlace`: the kept elements are
compacted forward with `set` before the first tail `removeAt` faults. A
`toList()` of two or more elements is a real `ArrayList` and keeps accepting
writes after one shrinks it.

## Array properties of a `data class`, written-order named arguments, untyped extension receivers

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `data class Q(val a: IntArray, val s: Array<String>)`; `println(Q(intArrayOf(1, 2), arrayOf("a")))` | `Q(a=[I@0, s=[Ljava.lang.String;@1)` | `Q(a=[1, 2], s=[a])` |
| `data class V(var x: IntArray)`; `V(intArrayOf(7)).hashCode()`, and two instances over equal arrays | an identity hash; the two hashes differ | `38`; the hashes agree (`equals` stays identity on the array) |
| `D(1, 2).copy(b = g(6), a = g(7))` | `g7` before `g6` | `g6` before `g7` |
| `listOf(1, 2).joinToString(postfix = p(), separator = s())` | `s` before `p` | `p` before `s` |
| `operator fun Box.plus(o: Box)`; `3.also { }.plus(1)` | `unresolved reference: v` (the extension ran on the `Int`) | `4` |
| `operator fun Money.plus(o: Money)`; `listOf(Money(1), Money(2)).reduce { a, b -> a + b }` | `unresolved reference: plus on Money` | `Money(3)` |
| `fun <T> id(x: T) = x`; `id(listOf(1)) + listOf(2)`, `id("s") + listOf(2)` | `0.0`, `unresolved reference` | `[1, 2]`, `s[2]` |

An array-typed property is marked in the class metadata the compiler already
publishes for `Long` fields, so the generated `toString`/`hashCode` follow the
DECLARED type, as Kotlin's do. A `copy` call and a stdlib member with named
parameters now bind their arguments through the same written-order hoist user
calls use. A call on a receiver of no static type, in a program that declares
extensions of that name on user classes, tests the value's class against each
candidate at run time and falls back to the ordinary member dispatch; the
binary operators take the same path for an untyped left operand.

## Secondary constructors that call `super(args)`

Measured on `kotlinc` 2.4.21 / JRE 25.0.4.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `open class Base(val n: Int)`; `class Kid : Base { val tag: String; constructor(n: Int, t: String) : super(n) { tag = t } }`; `Kid(1, "x").tag` | `superclass constructor Base expects 1 argument(s), got 0` | `x` |
| `class E2 : RuntimeException { constructor(m: String, c: Throwable) : super(m, c) }`; `e.cause?.message` | `: super(2 argument(s))` from a secondary constructor is not supported | `c` |
| `open class AppError : Exception { val code: Int; constructor(m: String, code: Int) : super(m) { this.code = code } }`; `class NotFound(w: String) : AppError("$w not found", 404)` — a header selecting a SECONDARY of the superclass | `: super(1 argument(s))` from a secondary constructor is not supported | `user not found 404` |
| `open class Shape(val name: String) { constructor() : this("anon") }`; `class Circle : Shape { constructor() : super() }`; `Circle().name` | `superclass constructor Shape expects 1 argument(s), got 0` | `anon` |
| `open class Base(val a: Int, val b: Int = a * 2)`; `class Sub : Base { constructor() : super(7) }`; `Sub().b` — a default filled in the superclass call | `superclass constructor Base expects 2 argument(s), got 0` | `14` |
| `class C(val v: Int) { constructor(s: String) : super() }` | `constructor C has no argument for v` | rejected: `primary constructor call expected` |

A class with no primary constructor now gets one constructor of the primary's
shape PER `: super(args)` secondary, over that secondary's own parameters, so
the superclass arguments evaluate against them and the property initializers
and `init` blocks still run exactly once. The superclass constructor is selected
the way a `Super(args)` call site selects one — primary or secondary, by arity
then parameter type, defaults filled — in both the `: Super(args)` header and
the `: super(args)` clause. On the in-place initialization scheme every
secondary constructor also has an `into` form (`Class#$ctorN$into`) that runs
over an already-allocated instance, which is what a subclass chains through.

## Regex options

`Regex(p, RegexOption.X)`, `Regex(p, setOf(...))` and `p.toRegex(option)` now
compile (constant options only), as does the infix `input matches regex`;
the `IGNORE_CASE`, `MULTILINE`, `DOT_MATCHES_ALL`, `COMMENTS`, `UNIX_LINES` and
`LITERAL` options are honoured.

## Round 9 — indexed folds and other missing stdlib members

Measured on `kotlinc` 2.4.21 / JRE 21.0.12.1, compiled and run on the same JVM.

| program | before | reference |
| --- | --- | --- |
| `listOf(5, 3, 8).foldIndexed(0) { i, a, v -> a + i * v }` | `unresolved reference: foldIndexed on List` | `19` |
| `l.reduceIndexed { i, a, v -> a + i * v }`, `reduceIndexedOrNull`, `runningReduceIndexed` | unresolved | `24`, the first index is 1 |
| `l.foldRightIndexed("") { i, v, a -> a + "$i$v" }`, `reduceRightIndexed(OrNull)` | unresolved | `281305` |
| `l.runningFoldIndexed(1) { … }` / `scanIndexed` | unresolved | `[1, 5, 16, 130]` |
| `l.mapIndexedNotNull { i, v -> if (i > 0) v * 2 else null }` | unresolved | `[6, 16]` |
| `mapOf("b" to 20, "c" to 3).forEach { k, v -> print(k) }` | `b=20c=3` — `k` was the entry | `bc` |
| `val i = iterator { yield(1) }; println(i.hasNext())` | `unresolved reference: hasNext on List` | `true`, the block suspended between `hasNext` and `next` |
| `val li = listOf(1, 2).iterator(); for (x in li) print(x)` | `for-in over a non-iterable value (Iterator)` | `12`, the iterator left exhausted |
| `val x: Any = intArrayOf(1); println(x is Array<*>)` | `true` — every array matched every array type | `false` |
| `listOf(1).toTypedArray() is Array<*>` | `false` — it was a `List` | `true` |

## Round 8 — constructors that have an object, defaults in the callee, receiver function types

Measured on `kotlinc` 2.4.21 / JRE 21.0.12, compiled and run on the same JVM.
Eight harness modes were added (`calldflt`, `callorder`, `initorder`, `recvfn`,
`jumpexpr`, `charclass`, `splitmulti`, `numeq`) and twelve frozen records
captured from the reference toolchain.

### Wrong or failing, now answering what the JVM answers

| program | before | reference |
| --- | --- | --- |
| `class P(val a: Int) { val d = twice(a); fun twice(x: Int) = x * 2 }` | `unresolved reference: twice` | constructs |
| `open class A { init { println(who()) }; open fun who() = "A" }` with `class B : A() { override fun who() = "B" }` | `A` | `B` |
| `fun f(a: Int, b: Int = a * 2)`; `f(1)` | `unresolved reference: a` | `1/2` |
| `class K(val n: Int) { fun m(p: Int = n) = p }`; `K(5).m()` | `unresolved reference: n` | `5` |
| `f(c = g(), a = h())` | `h` ran before `g` | `g` then `h` |
| `fun build(f: StringBuilder.() -> Unit)` | parse error at the `.` | declares |
| `val n = s ?: return -1` | `unexpected token Return` | `-1` for a null `s` |
| `return` then a newline then a call | the call was the return value | `return` with no operand |
| `when (x) { null -> … }` for the object whose heap handle is `0` | the `null` arm matched | the `null` arm is for `null` |
| `object O { init { println("i") } }` | never ran the block | runs it at first use |
| `"a--b".split("--", "-")` | `[a, , b]` | `[a, b]` |
| `'\u0663'.isDigit()`, `'²'.isDigit()` | Rust's `is_numeric` | the JDK's `Nd` table |
| `'f'.digitToInt(16)` | `Char f is not a decimal digit` | `15` |
| `1.0.equals(1)`, `1.equals(1L)` | `true` | `false` |
| `5.coerceIn(1..3)` | `5.0` | `3` |
| `String.format("%s", null)` | `MissingFormatArgumentException` | `null` |
| `println(Unit)`, `Unit.toString()` | `unresolved reference` | `kotlin.Unit` |
| `Lg.Factory.make()` for `companion object Factory` | `unresolved reference: Factory` | resolves |
| `@JvmStatic fun f()` inside a class body | parse error | accepted |
| `fun List<Int>.e() = filter { it > 1 } + reversed()` | `0.0` | the list |
| `class O { var o: Int by Delegates.observable(1) { p, a, b -> } }` | compile error naming `getValue` | `1` |
| `Delegates.notNull()`, `val n: String by map` | `unresolved`/rejected | the delegate reads and writes by property name |
| `groupingBy { }.fold`/`reduce`/`aggregate`, `reduceOrNull`, `Regex.escape` | `unresolved reference` | the results |
| `list.iterator().remove()`, `listIterator()` | `unresolved reference` | removes / walks both ways |

### How

- **Constructors.** A class on the chained scheme is built in two steps
  (`Class#$init` allocates every field of every class up the chain at its zero
  value, `Class#$into` runs the constructors over it). Field writes are by
  POSITION, so a property a subclass redeclares keeps the superclass's own field.
  Throwables, object expressions and enums-with-bodies keep the build-then-extend
  form; BUGS.md lists what that leaves.
- **Defaults.** A default that names an earlier parameter, `this` or a member is
  filled by the callee's prologue; the caller passes the unset marker.
- **Character classes.** `isLetter`, `isDigit`, `isUpperCase`, `isLowerCase` and
  `digitToInt` read tables produced by evaluating the predicates over the BMP on
  the reference JVM (`src/unicode_jdk.rs`). Rust's `char` predicates disagreed on
  hundreds of characters, partly by Unicode version.

## Round 7 — a fault the program cannot catch is not a fault Kotlin has

### The oracle

Three ways of resolving the toolchain give three different JVMs on this machine,
so the resolution is now stated by every harness rather than assumed:

| how `kotlinc` is invoked | JRE it actually ran on |
| --- | --- |
| ambient `JAVA_HOME=…/.jenv/versions/17` | 17.0.4.1+9-LTS |
| `env -u JAVA_HOME` | 26.0.2 |
| `JAVA_HOME=/opt/homebrew/opt/openjdk@21` | 21.0.12 |

`scripts/capture-parity.sh` already refused below 21 and still does — run under
the ambient environment it exits 2 with
`capture-parity: /Users/…/.jenv/versions/17/bin/java is JDK 17; the corpus needs
21 or newer`. Bare `java` on `PATH` is a broken jenv shim and is never used.

### Panic instead of error

A Rust stack overflow is `SIGABRT` — `fatal runtime error: stack overflow,
aborting`, exit 134. Nothing unwinds and no `catch` can see it, so it is a
parity divergence even where the value on the happy path would have matched.
**Eight programs aborted; all eight now answer what the JVM answers.**

Two causes, two fixes.

**Re-entrant interpretation had no floor and no ceiling.** A closure body, a
`toString`/`equals`/`hashCode` override and a lambda-taking collection member
each run through a nested `vm.run()` (`host::run_sub`), so one level of Kotlin
recursion costs one Rust frame of the whole dispatch loop — measured at roughly
100 KB in a `cargo build` binary. On the platform default 8 MiB main-thread
stack that aborted at a depth between 50 and 100: `f(50)` printed `0` and
`f(100)` aborted. The run now happens on a thread with a 512 MiB reserved stack
(a virtual reservation; only touched pages commit), and `run_sub` raises
`java.lang.StackOverflowError` at a nesting depth of 2000. Measured after:
depth 1999 prints `0`, depth 2001 raises. The usable depth went UP by roughly
30×, and the failure that remains is catchable.

`StackOverflowError` and `VirtualMachineError` joined the throwable tables, so
the placement is real and not just the name: `catch (e: Error)` claims it and
`catch (e: Exception)` does not — both pinned, both matching the reference.

**Rendering a container recursed in Rust with no guard.** Four more shapes
aborted here, and the JVM does not answer them all the same way:

| program | before | reference (JDK 21.0.12) |
| --- | --- | --- |
| `xs.add(xs); println(xs)` | SIGABRT | `[(this Collection)]` |
| `s.add(s)` (a `Set`) | SIGABRT | `[(this Collection)]` |
| `m["k"] = m` | SIGABRT | `{k=(this Map)}` |
| `m[m] = 1` | SIGABRT | `{(this Map)=1}` |
| `a.add(b); b.add(a)` | SIGABRT | `java.lang.StackOverflowError` |
| `data class D(var next: Any?)` holding itself | SIGABRT | `java.lang.StackOverflowError` |

`AbstractCollection.toString` compares each element against `this` — reference
identity, the immediate receiver, one level, no cycle detection of any kind — so
direct self-containment gets a placeholder and an indirect cycle still
overflows. Reproducing only the first half is what makes the two differ here the
way they differ there. Both halves are implemented and both are pinned; a
generated `data class` `toString` has no such check and raises, which is why it
sits in the second group.

### Error shape, not error text

Round 6 audited message strings. These are defects a string audit cannot see,
because the string was already right.

**Every `java.util.Formatter` fault was catchable only as `Throwable`.**
`"%q".format(1)` produced exactly
`java.util.UnknownFormatConversionException: Conversion = 'q'` — the correct
message — but the class was in no hierarchy table, so it descended straight from
`Throwable` and `catch (e: IllegalArgumentException)`, which the JVM answers YES
to, saw nothing. `IllegalFormatException` and its six subclasses are now
declared under `IllegalArgumentException`; all six faults are pinned as caught
at `IllegalArgumentException`, `RuntimeException` and `Exception`, and as not
caught at `Error`.

**Three format faults were not raised at all — they were wrong answers.**

| program | before | reference |
| --- | --- | --- |
| `"%d %d".format(1)` | `1 0` | `MissingFormatArgumentException: Format specifier '%d'` |
| `"%-5.2f".format()` | `0.00 ` | `MissingFormatArgumentException: Format specifier '%-5.2f'` |
| `"%d".format("x")` | `0` | `IllegalFormatConversionException: d != java.lang.String` |
| `"%f".format(1)` | `1.000000` | `IllegalFormatConversionException: f != java.lang.Integer` |
| `"%e".format(1)` | `1.000000e+00` | `IllegalFormatConversionException: e != java.lang.Integer` |
| `"%c".format("x")` | a NUL character | `IllegalFormatConversionException: c != java.lang.String` |

An exhausted argument list became `Undef`, which `to_int()` reads as `0`; the
specifier is now quoted AS WRITTEN, flags and width and precision included,
because that is what the JVM quotes. A conversion is typed: `%d`/`%x`/`%X`/`%o`
take an integral, `%f`/`%e`/`%E` take a floating value and REFUSE an `Int`,
`%c` takes a `Char` or an `Int`, and `%s`/`%b` take anything. Extra arguments
are ignored rather than faulting, and `%%`/`%n` consume none — both pinned so
the new check cannot have become a blanket refusal.

**`%b` was a truth test where the JVM has a null test.** `"%b".format("x")` and
`"%b".format(1)` both read `false`; the JVM reads `true`. `%b` is `false` only
for `null` and for a `Boolean false`. Eight operands pinned.

**A null argument was coerced through the conversion.** `%d` printed `0`, `%f`
printed `0.000000`, `%c` printed a NUL. The JVM prints the four characters
`null` under every conversion but `%b`, with the width and `-` flag still
applied (`%5d` of a null is ` null`).

**A width or precision that overflows an `int` was silently no width.** The
digits were parsed into a `usize` with `unwrap_or(0)`, so `%99999999999999999999d`
printed `1` — and the values that fit a `usize` but not an `int`
(`%4294967296d`) tried to pad to four billion characters and never returned.
Java parses into an `int` and rejects the negative, so every overflowing
spelling lands on the same number: `IllegalFormatWidthException: -2147483648`,
and `IllegalFormatPrecisionException: -2147483648` for the precision. Four
width spellings and two precision spellings pinned.

**A `Sequence` wore a `List`'s diagnostics.** kotlinrs materializes a bounded
sequence into a `List`, and the diagnostics followed that representation rather
than the type the source wrote: `listOf<Int>().asSequence().first()` said
`List is empty.` where Kotlin says `Sequence is empty.` The representation is
not observable; the message is. A `SEQ_VIEW` tag now marks a handle as a
sequence view, deliberately NOT as a `ListImpl` row — that table names which JVM
`List` class a handle is, and a `Sequence` is not a `List` of any class.

The tag has to survive derivation, because `Sequence.map`/`filter`/`drop`/… are
declared to answer a `Sequence`, and it must NOT survive `toList`, which answers
a collection. Nine expressions pinned, `toList` included.

Finding the missing wording also turned up the reason it was missing: the
dispatch site for the shared ordered members re-derived the receiver kind with a
`with_obj` match of its own instead of asking `seq_kind_of`, and the two answers
had already drifted. There is one derivation now.

### Tests that can pass vacuously

Every test in `tests/` was censused mechanically — 224 in `tests/lang.rs`, 6 in
`tests/builtin_ids.rs`, 1 in `tests/parity.rs` — for a body that can execute
zero assertions. `tests/lang.rs` came back clean: the only two tests whose
assertions all sit inside a loop iterate over inline array literals, which
cannot be empty. **One vacuous pass was found, and it was demonstrated rather
than argued.**

`compiler_emits_each_id_through_its_own_table` reads `src/compiler.rs` as text
and matches the literal `Op::CallBuiltin(KT_`. Renaming that needle to
`Op::CallBuiltin(RENAMED_KT_` left the test **green** while the other five in
the file still ran: both of its loops iterated zero times and it reported PASS
having compared nothing. A guard that goes silent exactly when the thing it
guards has moved is the worst failure a guard can have — and every other parser
in that file already asserted its own yield (`declared_ids` > 50,
`coercion_arms` non-empty, `coll_hof_arms` > 30). This one now floors both emit
tables at 40 (measured: 51 and 54 sites). With the floor in place the same
rename fails loudly: `parsed only 0 Op::CallBuiltin(KT_…) emit site(s)`.

`throwable_hierarchy_is_closed` had a narrower version of the same hole:
`table_rows` panics when the DECLARATION is gone, but a declaration that is
still there whose rows no longer parse is silent, and both loops are driven by
those rows. Floored at 15 rows each.

No test was deleted or weakened.

### Provenance

Every one of the 635 frozen records was re-minted from the live toolchain and
compared byte for byte. **0 fabricated pins.** One record was flagged by the
fast batched stage and cleared by the second: a user throwable prints its own
qualified name, so batching it into its own package turns `NotFound: missing q`
into `p87.NotFound: missing q`. Re-captured alone in the default package through
`capture-parity.sh` it matches byte for byte.

That audit is now `scripts/reverify-parity.sh` rather than an ad-hoc step. It is
the only check for a fabricated pin — `tests/parity.rs` never runs the oracle,
so a line written from memory passes there forever — and it names the JVM it
resolved for the compile step and the run step before it compares anything.

31 new records were captured and the corpus floor moved 604 → 635.

## Round 8 — the members that were missing, and the five that answered wrongly

### How the gaps were found

The corpus is curated, so it cannot report a construct nobody captured. This
round went looking for those instead of extending what was already covered: the
member names appearing in `src/host.rs`/`src/compiler.rs` were diffed against
the identifiers the corpus programs use, and every name on neither side of that
diff became a probe. Six batches — 284 probe programs, some deliberately
re-covering earlier ground — were run against the oracle and against the built
frontend and compared byte for byte on stdout, stderr and exit status. The two
broad batches diverged on 24 of 51 and on 42 of 124 the first time they ran;
49 distinct members or behaviours were closed.

Probes are MICRO-probes — one expression each — because an `unresolved
reference` is a COMPILE error here: a single missing member kills a whole
program, and a probe that exercises eight members reports only the first.

### Five silent wrong answers

These are the ones that matter most: each ran to completion and printed
something plausible, so no diagnostic would ever have surfaced them.

| program | was | reference |
| --- | --- | --- |
| `"xxhixx".trimStart('x')` | `xxhixx` | `hixx` |
| `"xyhixy".trim('x', 'y')` | `xyhixy` | `hi` |
| `"abc".substringAfter("z", "def")` | `abc` | `def` |
| `"abc".substringAfterLast("")` | `` | `c` |
| `runCatching { 7 }.getOrDefault(-1)` | `null` | `7` |

The first three are one cause: the vararg `Char` overload of the `trim` family
and the `missingDelimiterValue` of the `substring…` family were both accepted
and then ignored. The fourth is subtler — `substringAfterLast` searches with
Kotlin's `lastIndexOf`, whose default `startIndex` is `lastIndex` and not the
receiver's length, which shows only for an empty delimiter.

The fifth was a dispatch collision, not a missing member. `Result` and `Map`
both declare `getOrDefault`, and the host's mutating-member match runs for every
heap kind: a `Result` receiver fell into the map body, found no key, and
answered the map form's SECOND argument — which a `Result` call never passes.
The arm is receiver-guarded now.

### Members and syntax added

`String`: the vararg-`Char` `trim`/`trimStart`/`trimEnd`, `substringBeforeLast`/
`substringAfterLast` and `missingDelimiterValue` on all four, `capitalize`/
`decapitalize`, `replaceFirstChar { }`, `ifEmpty`/`ifBlank`, and `split`'s named
`limit` (which the positional default table could not express — `split`'s first
parameter is a vararg, so `split("a", "b")` would have bound `"b"` to
`ignoreCase`; the two optional parameters are lifted out by name and told apart
from a delimiter by TYPE).

Collections: `distinctBy`, `firstNotNullOf`/`firstNotNullOfOrNull`, `ifEmpty`,
`asReversed`, `orEmpty`, `contentToString`, the in-place `sort`/`sortDescending`/
`reverse`/`sortBy`/`sortByDescending`, `Map.getOrPut`/`getOrDefault`, and
`eachCountTo` — which used to build a fresh map and answer it, so the
DESTINATION it was handed stayed empty while the printed counts looked right.

Values and types: `sequenceOf`, `longArrayOf`/`floatArrayOf`, `lazy { }` as a
value (`.value`/`.isInitialized()`), `Result.getOrDefault`/`getOrThrow`,
`Pair.toList`/`Triple.toList`, `Double.isNaN`/`isInfinite`/`isFinite`,
`Char.MIN_VALUE`/`MAX_VALUE`, `Char.titlecaseChar`/`titlecase`,
`Integer.toBinaryString`/`toHexString`/`toOctalString` (unsigned 32-bit, so
`-1` is `ffffffff` where `(-1).toString(2)` is `-1`), the progression `step`
property, and the infix spelling of `union`/`intersect`/`subtract`.

The infix set functions parse only when GLUED to their left operand's line. The
lexer drops newlines, and `union` — unlike `shl` — is a plausible name for a
local, so without that test

    val x = 1
    union(2)

would have parsed as `1 union (2)` and the second statement would have
disappeared. Kotlin's grammar has the rule (`infixFunctionCall` admits no
newline before the identifier), so the gate is the spec.

### The titlecase table was measured, not recalled

`Char.titlecaseChar()` is the single-`Char` uppercase mapping except where
Unicode gives a character a titlecase form of its own. Rather than reconstruct
that list, a reference program compared `titlecaseChar()` against
`uppercaseChar()` over all 65 536 `Char`s on the oracle and printed the pairs
that differ. There are exactly 58, in two families: the four Latin digraphs
(all three case forms titlecase to the middle one) and Georgian Mkhedruli
(no titlecase mapping at all — it answers itself, where `uppercaseChar` moves
it into the Mtavruli block). The Greek iota-subscript letters do NOT differ,
because their single-`Char` uppercase already is the titlecase form.

### Not closed

`Regex` in every spelling, a `data class` declared inside a function body, and a
label on a lambda literal (`forEach lit@{ … }`). All three fail loudly. They are
recorded with the reference's answer in [BUGS.md](BUGS.md), along with a
divergence this round MEASURED rather than introduced: `asReversed()`,
`Map.keys`/`values` and `subList()` are declared to be live views and are
snapshots here, so a program that holds one across a write to the backing
collection sees the wrong thing.

### Provenance

20 new records, each minted through `scripts/capture-parity.sh` — the same
script that minted the corpus, one `kotlinc` per program in the default package —
under `kotlinc` 2.4.10 / JRE 21.0.12. The corpus floor moved 635 → 655. 13 new
`tests/lang.rs` tests, 235 → 248. No test was deleted or weakened.

## Round 9 — a flag that was accepted and thrown away

### What the round looked for

The same diff round 8 used: the member names the frontend implements against
the identifiers the frozen corpus actually exercises, with everything on
neither side turned into a one-expression probe. The vein it opened this time
was not missing NAMES but a missing ARGUMENT. `ignoreCase` is a parameter on a
dozen `String` members; kotlinrs accepted it on eight of them positionally,
rejected it by name on all of them, and honoured it on none.

That is the worst shape a gap can take: `"abcabc".indexOf("B", 1, true)`
compiled, ran, and answered -1 — a plausible number, silently wrong.

### The silent wrong answers

| program | reference | kotlinrs was |
| --- | --- | --- |
| `"abcabc".indexOf("B", 1, true)` | `1` | `-1` |
| `"abcabc".indexOf('B', 1, true)` | `1` | `-1` |
| `"abcabc".lastIndexOf("B", 5, true)` | `4` | `-1` |
| `"abcabc".startsWith("ABC", true)` | `true` | `false` |
| `"abcabc".startsWith("A", 0, true)` | `true` | `false` |
| `"ABCabc".endsWith("ABC", true)` | `true` | `false` |
| `"abcabc".contains("B", true)` | `true` | `false` |
| `"aXbXc".replace("x", "-", true)` | `a-b-c` | `aXbXc` |
| `"aXbXc".replaceFirst("x", "-", true)` | `a-bXc` | `aXbXc` |
| `"aXbXc".split("x", ignoreCase = true)` | `[a, b, c]` | `[aXbXc]` |
| `'a'.equals('A', true)` | `true` | `false` |
| `"ϑ".compareTo("ϴ", true)` | `0` | `847` |

The last row is a different defect with the same cause: `equals` and `compareTo`
DID read the flag, but implemented it by lowercasing both whole strings, which
is not the rule.

Naming the flag was rejected outright on every one of them —
`startsWith has no parameter `ignoreCase`` — because the parameter tables in
`builtin_params` stopped one slot short.

### What the rule actually is, measured

Two characters are equal under `ignoreCase` exactly when they share the key
`lowercaseChar(uppercaseChar(c))`. Folding the character directly instead —
which is what comparing two `lowercase()`d strings amounts to — disagrees on
exactly two `Char` pairs in the whole range, `U+03D1`/`U+03F4` and
`U+0130`/`U+0131`. Both were put to the oracle under every member that takes the
flag, and it answers `true` for both every time. The two pairs are now pinned.

The fold is per CHARACTER, and the scan therefore runs over characters rather
than bytes: `'İ'` folds to `'i'`, two UTF-8 bytes to one, so a byte-offset scan
would have moved the cut. Positions in and out stay UTF-16 offsets, which is
what Kotlin specifies them in.

`compareTo(other, ignoreCase = true)` is the JVM's `CASE_INSENSITIVE_ORDER`, and
it is none of the three things it resembles. It walks CODE POINTS, but only as
far as the shorter receiver's UTF-16 length, so a surrogate pair straddling that
bound reads as the lone high surrogate it starts with. Three measurements fix
it: `"𐐨b".compareTo("𐐀a", true)` is 1 (a per-`Char` walk says 40),
`"İ".compareTo("i", true)` is 0 (a whole-string lowercase says 1), and
`"𐐨".compareTo("a", true)` is 55200 (an unbounded code-point walk says 66503).

### The case mappings themselves were wrong for 2 076 characters

`uppercaseChar`/`lowercaseChar` are the JVM's SIMPLE case mappings. Rust exposes
the FULL ones, and the two differ. A reference program printed every mapping for
all 65 536 `Char`s and the answers were diffed:

| family | count | was | is |
| --- | --- | --- | --- |
| surrogate halves | 2 048 | `U+FFFD` | themselves |
| Greek letters with subscript iota (`U+1F80`…, `U+1FB3`, `U+1FC3`, `U+1FF3`) | 27 | themselves | their titlecase form |
| `U+0130` under `lowercaseChar` | 1 | itself | `i` |

`Char.titlecase()` was wrong for 77 more: when the full uppercase expands, the
result is that uppercase with everything past its FIRST character lowercased
again — `'ᾀ'` titlecases to `"Ἀι"`, not to the `"ἈΙ"` its uppercase is, and
`'ß'` to `"Ss"`, not `"SS"`. `U+0149` is the one character that keeps its whole
uppercase. The rule was checked against the oracle's answer for every
non-surrogate `Char`: 0 mismatches, where taking the uppercase unchanged missed
77.

`uppercaseChar`, `lowercaseChar`, `titlecaseChar`, `titlecase()`, `uppercase()`
and `lowercase()` now agree with the oracle byte for byte over the entire `Char`
range.

### Members that were simply absent

Measured, then added: `regionMatches`, `removeSurrounding`, `commonPrefixWith`,
`commonSuffixWith`, `indexOfAny`, `lastIndexOfAny`, `findAnyOf`,
`findLastAnyOf`, `coerceAtLeast`/`coerceAtMost` on `String`, and
`takeLastWhile`/`dropLastWhile` on both a `List` and a `CharSequence`.

The `…AnyOf` family scans POSITIONS from one end and takes the FIRST needle in
the collection's own order at each — `"abc".findAnyOf(listOf("ab", "a"))` is
`(0, ab)` and `"abc".findAnyOf(listOf("a", "ab"))` is `(0, a)`, so length is not
the tie-break. Its `startIndex` and its answer are both UTF-16 offsets; a
randomized differential run against the oracle caught the first draft reading
`startIndex` as a character index, and caught an empty receiver building a range
from `-1 as usize` and aborting the interpreter.

### Verification

Three randomized differential batches (1 122 probes) over an alphabet chosen for
the awkward cases — `İ ı ϑ ϴ ß ẞ K k ǅ Ǆ ǆ ſ σ ς Σ 𐐨 𐐀 ᾀ` — diffed live against
`kotlinc`. Everything the frontend can represent agrees; the two that remain are
the lone-surrogate limit already recorded in [BUGS.md](BUGS.md).

### Not closed

`Regex`, a `data class` inside a function body, and a label on a lambda literal
are all still open and still fail loudly. Newly measured and recorded rather
than fixed: `String.CASE_INSENSITIVE_ORDER`, `"…".toRegex()`,
`kotlin.random.Random`, and five spellings kotlinrs ACCEPTS that `kotlinc`
rejects.

### Provenance

20 new records, each minted through `scripts/capture-parity.sh` under
`kotlinc` 2.4.10 / JRE 21.0.12 — and re-captured under JRE 26.0.2, which
produced a byte-identical file, so no record here depends on the capturing JVM.
The corpus floor moved 655 → 675. 5 new `tests/lang.rs` tests, 248 → 253. One
pre-existing doctest failure in `src/parser.rs` is fixed (an indented Kotlin
snippet in a doc comment was being compiled as Rust, so `cargo test` was red on
`main`). No test was deleted or weakened.

## Round 10 — the spellings that did not parse, and two loops that were quadratic

### The oracle

`kotlinc-jvm 2.4.10 (JRE 21.0.12.1)`, as `scripts/capture-parity.sh` reported
it while minting this round's records. Every value below came from a run of it.

### Three resolution gaps at file scope and in a lambda's parameter list

**A top-level `val` did not record its class.** `PropMeta.class` was copied from
the type ANNOTATION alone, so `val p = C()` carried no class while
`val p: C = C()` did — and `infer_class` reads exactly that field for a global.
Everything keyed off a receiver's class therefore stopped at file scope while
working inside a function, where the local binding table records it. The
operator conventions resolved `K(1) + K(2)` and a local `val a = K(1); a + b`,
and failed to compile the same two objects held in top-level `val`s with
`unresolved reference: plus on K`. A forward pass over the properties in
declaration order — the order their initializers run in — backfills it.

**A top-level `val` holding a lambda was not callable.** `f(3)` for a file-scope
`val f = { x: Int -> x + 1 }` was `unresolved reference: f`: the local-slot arm
of `compile_call` covers the same shape inside a function, and at file scope
every remaining arm looks for a callable DECLARATION, which a property is not.
The new arm sits after the free-function lookup, so a top-level `fun f` still
wins — Kotlin keeps functions and properties in separate namespaces and resolves
a call against the function, and the reference prints `fun` for the pair. It is
gated on the property's type, so `val n = 5; n(1)` keeps its compile-time
diagnostic rather than becoming a closure call that only fails at run time.

**`{ (k, v) -> … }` was a parse error** — `expected RParen, found Comma` — which
is the spelling `Map.map`/`filter`/`forEach`/`sortedBy` are almost always
written with. Kotlin defines the group as ONE parameter whose components are
unpacked in the body, and that is how it lowers: a synthetic parameter, named
unspellably so it cannot shadow anything the program wrote, plus the leading
`StmtKind::Destructure` that `val (k, v) = e` already produced. No new runtime —
`componentN` on a `Map.Entry`, a `Pair`, a `Triple` and a `data class` all
already answered.

`skip_lambda_param_type` needed a matching stop. It terminated only on `,` or
`->` at depth 0, so an annotated last component — `{ (x: Int, y: Int) -> … }` —
swallowed the group's `)`, drove the depth negative, missed the `->` and rolled
the whole parameter list back. An UNBALANCED `)` closes something the annotation
did not open, so it ends it; a function type's own parens are balanced and never
reach depth 0 there. The scan stays speculative: `{ (a + b).toString() }` and
`{ (it) }` still parse as bodies, which the reference agrees they are.

### `::` did not lex

Every spelling of the callable-reference operator was `unexpected token Colon`.
Kotlin's definition is that a reference denotes a FUNCTION, so it lowers to the
lambda that calls it — arity from the callee's signature, body one call with the
synthesized parameters forwarded — and capture, dispatch and passing it to
`map`/`filter`/`fold`/`sortedBy` are then the closure path that already existed.

| spelling | what it denotes |
| --- | --- |
| `::inc`, `::C`, `::println` | a top-level function, a primary constructor, a built-in |
| `C::twice`, `C::v`, `String::length` | UNBOUND — the receiver becomes the first parameter |
| `c::plusN`, `bump()::length` | BOUND — the receiver is captured |

A computed bound receiver is pinned to a temporary, so `bump()::length` calls
`bump()` once where the reference is written and not once per invocation; the
reference answers `1` for the counter and so does this. A local shadows a type
in receiver position, which kotlinc also accepts: `val C = C(4)` makes
`C::plusN` the bound reference on both.

A built-in receiver type has no arity table here, so `String::length` lowers to
a one-parameter member ACCESS — which covers a property and a zero-argument
method alike. A built-in member that takes arguments is not expressible that way
and fails with its own `unresolved reference` rather than binding a wrong arity
silently. `Type::class` is not covered and is recorded in [BUGS.md](BUGS.md).

### Six members that were `unresolved reference`

`linkedMapOf` / `sortedMapOf` / `TreeMap` are the ordered `Map` builders whose
`Set` counterparts already resolved, and each one's ITERATION order is why it
exists, so it travels through the same trailing order spec. `TreeMap` also joins
the JVM-constructor list, so `TreeMap(other)` is the COPY form.
`Iterable<Pair<K, V>>.toMap()` shares `associate`'s duplicate-key rule —
`listOf(1 to 2, 1 to 3).toMap()` is `{1=3}` — and `Map.toMap()` /
`Map.toMutableMap()` copy the receiver in its own iteration order.

Two spellings were measured and deliberately NOT added, because kotlinc rejects
them: `List.toMutableMap()` and `List<Map.Entry>.toMap()`. Both were in a first
draft; the probe against the reference is what took them out.

### Two O(1) operations that cost O(n)

Growing a collection one element at a time was quadratic on both of the two ways
a Kotlin program does it. `/usr/bin/time -p` user seconds, `cargo build` (dev)
binary, same machine and session.

`MutableList.add` ran a full membership probe whose result it discarded. Only a
`Set` needs it — it decides both the answer and whether the collection changes
at all — while a `List` always appends and always answers `true`. The probe is a
whole-collection scan that re-enters the VM for a user `equals`.

| `xs.add(i)` | before | after |
| --- | --- | --- |
| n = 20 000 | 15.13 s | 0.04 s |
| n = 40 000 | 59.28 s | 0.07 s |
| n = 80 000 | 234.30 s | 0.14 s |

`StringBuilder` dispatch cloned the builder's whole content before looking at
the member name, so `append` copied the entire string it was appending to on
every call. Only `toString` and the delegation of the inherited `CharSequence`
members need the content; every mutating member reaches the buffer through
`edit_builder`, which borrows it, and reads nothing but the length.

| `sb.append("abcdefghij")` | before | after |
| --- | --- | --- |
| n = 25 000 | 0.17 s | 0.08 s |
| n = 50 000 | 0.53 s | 0.15 s |
| n = 100 000 | 2.04 s | 0.31 s |
| n = 200 000 | 8.10 s | 0.61 s |

Four times the work for twice the input in both tables before, two after.
Behaviour is unchanged and was checked against the reference rather than
assumed: `Set.add` still dedupes, including through a user `equals`/`hashCode`;
`add(index, element)`, the `hashSetOf`/`sortedSetOf`/`linkedSetOf` orderings and
the whole `append`/`reverse`/`setCharAt`/`insert`/`deleteCharAt`/`capacity`/
surrogate-length surface all still answer byte-for-byte what kotlinc answers.

### Not closed

`m[k] = v` on a `MutableMap` is still quadratic — 20 000 puts, 14.03 s. That one
is the data structure rather than a redundant call: a `Map` is an association
`Vec` so that it can preserve iteration order and route equality through a user
`equals`, and `index_set` already skips the scan for every non-`Map` receiver.
Newly measured and recorded rather than fixed: `Float` is `Double` (so
`1.0f / 3.0f` is `0.33333334` on the reference and `0.3333333333333333` here),
`Type::class`, a `Comparable<T>` supertype, the nullable-receiver extensions,
`enumValues<E>()`, `Float`/`Double` companion constants, and what a function
VALUE prints. All in [BUGS.md](BUGS.md).

### Provenance

47 new records, each minted by `scripts/capture-parity.sh` from
`kotlinc-jvm 2.4.10 (JRE 21.0.12.1)` in this run; 0 rejected. The corpus floor
moved 675 → 722. 5 new `tests/lang.rs` tests, 253 → 258. No test was deleted or
weakened, and no audit or report script was touched.

## Round 10 — nullability is part of a cast, not decoration on it

### The bug

`is_type()` consumed the `?` of a written type and discarded it, so `String?`
and `String` reached the compiler as one string. Three answers followed from
that single loss, two of them wrong values rather than refusals:

| program | kotlinrs | reference |
| --- | --- | --- |
| `null as String?` | `ClassCastException` | `null` |
| `null as String` | `ClassCastException` | `NullPointerException` |
| `null is String?` | `false` | `true` |

The comment on `KT_AS` asserted the opposite — that dropping the `?` left a null
"passing a safe cast and failing an unsafe one exactly as the JVM's would" —
which is true of `as?` and of nothing else.

### The fix

`is_type()` now reports the `?`, and `Expr::As`, `Expr::Is` and `WhenCond::Is`
carry it. It reaches the runtime in each op's inline operand: `KT_IS` takes it
as its only bit, `KT_AS` as bit 1 beside the existing `as?` bit 0. A null
operand is then decided by the target's nullability rather than by its class —
null for `as T?` and for `as? T`, and a `NullPointerException` naming the type
for `as T`, which is the JVM's own wording and not the `ClassCastException` a
wrong class still gets.

`cast_type` had to learn it too. It already mapped `as? String` to
`NullableString`; `as String?` produced a plain `String`, and a null under that
static type prints as the empty string rather than as `null` — so the cast
answered correctly and then rendered wrongly. Both spellings now produce the
nullable type, which is what `println(null as String?)` needs.

### Newly measured, recorded rather than fixed

An infix CALL does not parse, though an `infix fun` DECLARATION does:
`2 pw 10` is `expected RParen, found Ident("pw")` where the reference answers
`1024`. In [BUGS.md](BUGS.md).

One row of that table was stale and is gone: a local `data class` inside `fun
main()` was recorded as `unexpected token Class (line 1)`, and both the
single-line and the multi-line spelling now print `P(a=1)` as the reference
does.

### Provenance

8 new records, minted by `scripts/capture-parity.sh` from
`kotlinc-jvm 2.4.10 (JRE 26.0.2.1)`; 0 rejected. The corpus floor moved
956 → 964. No test was deleted or weakened, and no audit or report script was
touched.

The JDK gate that had kept this frontend unmeasured was a mistake in the
measurement, not a missing toolchain: `/usr/libexec/java_home` lists only the
JVMs registered under JavaVirtualMachines, and Homebrew's openjdk 21, 25 and 26
are not among them. `JAVA_HOME=/opt/homebrew/opt/openjdk` is enough.

## Round 11 — a sequence that is lazy is a sequence that must be pulled

### The oracle

`kotlinc-jvm 2.4.10 (JRE 21.0.12.1)`, with `JAVA_HOME` pinned to
`/opt/homebrew/opt/openjdk@21`. The pin is not optional: unset, the launchers
resolve through `/usr/libexec/java_home`, which on this machine registers only
Corretto 17 and 11, and the ambient `JAVA_HOME` is Corretto 17. `1.0e23`
printing `9.999999999999999E22` is the tell for a pre-19 JVM; under the pin it
prints `1.0E23`.

### `asSequence()` became lazy, and the laziness is observable

A `Sequence` was represented as a `List` with a tag, which answers correctly for
every terminal that only inspects the result and wrongly for every one that can
observe HOW MUCH was evaluated:

| program | before | reference |
| --- | --- | --- |
| `listOf(1, 0).asSequence().map { 10 / it }.take(1).toList()` | `ArithmeticException: / by zero` | `[10]` |
| `generateSequence(1) { c++; it + 1 }.take(1)`, then `c` | `2` | `0` |
| `generateSequence(1) { c++; it + 1 }.first { it > 3 }`, then `c` | `10` | `3` |
| `generateSequence(1) { it + 1 }.elementAt(4)` | pull-cap `IllegalStateException` | `5` |

The source is now one of three — a `sequence { … }` block, a list, or the
seed/step pair — and the source is advanced when the next element is WANTED
rather than after the previous one was taken, which is what makes `take(1)` run
no step at all. A search (`first`/`any`/`indexOfFirst`) drives the pipeline with
a callback instead of asking for one more element per round, which had restarted
the pipeline from its seed each time and so ran the step a quadratic number of
times.

### The members that read a container's elements had to learn to pull one

Making `asSequence` answer a real pipeline broke every path that read elements
out of a materialized container. `sequence_items` answers the empty vector for a
lazy sequence, so each of these silently produced nothing:

| program | before | reference |
| --- | --- | --- |
| `listOf(1, 2).asSequence().flatMap { listOf(it, -it).asSequence() }.toList()` | `[]` | `[1, -1, 2, -2]` |
| `listOf(1, 2, 3).asSequence().zip(listOf("a", "b").asSequence()).toList()` | `[]` | `[(1, a), (2, b)]` |
| `listOf(1, 2, 3).asSequence().joinToString("|")` — in a program declaring a `toString()` override | `` | `1|2|3` |

The last one is the reason a whole-program differential harness earns its keep.
`joinToString` lowers to a different op in a program that overrides `toString()`
than in one that does not, so it answered the empty string in exactly those
programs and the right answer in every isolated probe. It survived every
targeted test written for the sequence work and fell to one 2700-probe fuzz run.

### `kotlin.math` is an import, and an alias MOVES a name

`roundToInt`, `roundToLong`, `pow`, `absoluteValue` and `sign` are extensions
declared in `kotlin.math`, not members of the numeric types, so a call with no
import line is an unresolved reference exactly as a bare `abs` is. `Int.mod` is
a `kotlin` package member and needs no import.

An aliased import moves the spelling rather than adding one, and it wins over a
star import of the same package — which holds for all three kinds the star
opens:

| program | before | reference |
| --- | --- | --- |
| `import kotlin.math.roundToInt as rti` + `3.7.rti()` | `unresolved reference: rti` | `4` |
| `import kotlin.math.*` + `import kotlin.math.abs as A` + `abs(-3)` | `3` | `unresolved reference 'abs'` |
| `import kotlin.math.*` + `import kotlin.math.PI as Tau` + `PI` | `3.141592653589793` | `unresolved reference 'PI'` |

An explicit import of the original spelling alongside the alias puts it back, so
`import kotlin.math.abs` and `import kotlin.math.abs as A` together leave both
callable.

### `Map` write members answer three different things

`put` and `putIfAbsent` answer the PREVIOUS value; `remove(key)` answers the
removed value while `remove(key, value)` answers a `Boolean` and removes only
when the mapping is the one named; `getValue` throws where `get` answers null.
A key mapped to null counts as absent for `putIfAbsent`. `containsValue` scans
the values. All measured; all previously wrong or unresolved.

### The frontend, profiled for the first time

Both earlier rounds on this frontend delivered harness-side wins only. Lowering
itself is now measured, `--dump-bytecode` isolating lex+parse+compile, minimum of
110 interleaved runs against a baseline built from the previous commit, with an
A/A control:

| program | baseline | after | A/A control |
| --- | --- | --- | --- |
| 1600 probes, one accumulating scope | 416.28ms | 82.27ms | 132.60ms |
| 800 probes, one accumulating scope | 77.13ms | 50.71ms | 0.51ms |
| 200 probes, one accumulating scope | 27.71ms | 28.44ms | 1.20ms |
| 1600 probes, each in its own `run { … }` | 125.41ms | 128.49ms | 3.84ms |
| 800 probes, each in its own `run { … }` | 69.65ms | 70.61ms | 0.86ms |

A lambda captured its whole visible scope, so a function holding n bindings and
n lambdas was quadratic. Filtering the captures to the names the body mentions
removes that — but the filter walks the body, which is pure overhead when the
scope was small enough to copy for nothing, and it cost about 12% on the
`run { … }` shape where every scope stays tiny. Both answers are correct, so the
choice is made on scope size: above `CAPTURE_SCAN_FLOOR` bindings the captures
are filtered, below it they are taken whole. The two bottom rows are the
control that the gate works; the 200-probe row is where the quadratic has not
yet bitten and nothing should move.

### Provenance

21 new records, minted by `scripts/capture-parity.sh` from
`kotlinc-jvm 2.4.10 (JRE 21.0.12.1)`; 0 rejected. The corpus floor moved
973 → 994. Seven fuzz modes were added — `strops`, `numconv`, `seq`, `destr`,
`nullcoll`, `mapops`, `contract` — and one of them shipped a generator bug that
cost half its probes: `-255.toString(16)` parses as `-(255.toString(16))`, which
kotlinc rejects, so its negative literals are now parenthesized. No test was
deleted or weakened, and no audit or report script was touched.

### Newly measured, recorded rather than fixed

A local `fun` does not capture the enclosing frame: `val k = 5; fun h(x: Int) =
x + k` inside `main` is `unresolved reference: k` where the reference answers
`6`. Local functions lower to top-level subroutines reached by a direct call,
which have no upvalue mechanism, so the fix is a closure-conversion pass with a
transitive fixpoint — a local `fun` calling another inherits its captures, and a
lambda calling one inherits them too. Not attempted here rather than attempted
badly.

## Round 12 — three answers that were wrong, and the spellings that stopped a program

### The oracle

`kotlinc-jvm 2.4.20` on **JRE 25.0.4.1** (`CAPTURE_JAVA_HOME` set to the
Microsoft 25 build) for both the compile and the run step, through
`scripts/capture-parity.sh`. Every record below was captured there and replays
byte-identically; the corpus went from 1009 to 1035 records, 0 rejected. No test
was deleted or weakened, and no audit or report script was touched.

### Wrong answers

| program | before | reference and now |
| --- | --- | --- |
| `fun <T : Comparable<T>> ge(a: T, b: T) = a >= b; ge(V(1), V(3))` | `true` | `false` |
| `maxOf("q", "r")` through a `String` function | `0.0` | `r` |
| `maxOf(V(1), V(3))` on a user `Comparable` | `V(n=1)` | `V(n=3)` |
| `listOf(0) + listOf(1) + listOf(2)` | `0.0` | `[0, 1, 2]` |
| `listOf("a") + "b" + "c"` | `(obj:10)c` | `[a, b, c]` |
| `V(1) + V(2) + V(3)` with `operator fun plus` | `0.0` | `V(x=6)` |
| `b.onClick()` on `class Btn(val onClick: () -> String)` | `(lambda arity=1)` | the lambda's result |

The ordering operators on two operands whose static types pick no native
compare lowered to the coercing numeric ops, which read two strings as `0 >= 0`;
they now reach `compareTo` at run time through `KT_COMPARE_VM`, and `maxOf`/
`minOf` over non-numeric arguments go through `KT_EXTREMUM_VM`. Both are
builtins because a user `compareTo` re-enters the VM, which an extension op
cannot host. The operator chains were an inference gap: the result of an
operator on a heap receiver typed as a number, so the next operator in the chain
was a native add; it now types as a heap object, or as the declaring class's
method result, and an unannotated expression-bodied function returning a
constructor call carries that class.

### Spellings that stopped the program

Each was an `unresolved reference` or a parse error before this round:
identifiers of any script (`val café`) and backquoted names; `%2$s` and `%<s`
in `format`; `xs.indices` and `xs.lastIndex`; a trailing lambda after named
arguments, and one that binds the last parameter over defaulted ones
(`f { … }` against `fun f(n: Int = 1, g: (Int) -> String)`); `val size get() =
…` and extension properties; `Int.floorDiv`; `List(n) { … }`/`MutableList(n)
{ … }`; a positional argument after named ones in their own positions; the
spread operator into a `vararg`, a stdlib factory, or `format`; and a
throwable's `cause`, through `(message, cause)`, `(cause)` and `.cause`.

### Recorded in Round 11, since closed

A local `fun` capturing an enclosing `val` (`val k = 5; fun h(x: Int) = x + k`)
now answers `6`; the fix landed as `77ba084947` after Round 11 was written.

## Round 13 — the stdlib members that were missing, and three answers that were wrong

### The oracle

`kotlinc-jvm 2.4.20` on **JRE 21.0.12.1** (`/opt/homebrew/opt/openjdk@21`) for
both the compile and the run step, through `scripts/capture-parity.sh`. The
corpus went from 1035 to 1039 records; one candidate was rejected by the script
because it deliberately ends in an uncaught exception (exit 1). No test was
deleted or weakened, and no audit or report script was touched.

### Wrong answers

| program | before | reference and now |
| --- | --- | --- |
| `listOf(1, 2, 3).reduce(Int::plus)` | `1` | `6` |
| `listOf(P(1, "a"), P(2, "b"), P(1, "c"), P(2, "d")).sortedDescending()` (compared by `k`) | `[P(k=2, t=d), P(k=2, t=b), P(k=1, t=c), P(k=1, t=a)]` | `[P(k=2, t=b), P(k=2, t=d), P(k=1, t=a), P(k=1, t=c)]` |
| `mutableListOf(P(2, "b"), P(1, "a")).sort()` on a user `Comparable` | Rust panic, `RefCell already mutably borrowed` | `[P(k=1, t=a), P(k=2, t=b)]` |
| `arrayOf(3, 1, 2).toList() + listOf(9)` | `0.0` | `[3, 1, 2, 9]` |

An unbound reference to a built-in member was lowered as a one-parameter member
ACCESS, so `Int::plus` answered its receiver and dropped the argument; the
members whose only overloads take one argument now lower to a two-parameter
call. `sortedDescending`/`sortDescending` sorted and then reversed, which flips
elements that compare equal; they are now a stable sort by the reversed order,
as `sortWith(reverseOrder())` is. The in-place `sort` compared with `value_cmp`
inside the heap borrow, which ignored a user `compareTo` and, for an instance
receiver, borrowed the heap twice. A member whose stdlib result is a collection
kept `+` numeric when its receiver's own type went untracked.

### Missing surface, now present

- `lateinit var` with `kotlin.UninitializedPropertyAccessException` on every
  read before the first write, and `this::p.isInitialized`.
- `kotlin.math` `ln`/`log10`/`log2`/`ln1p`/`log(x, base)`/`exp`, plus
  `Math.log1p`. They run on a port of fdlibm (`src/fdlibm.rs`), because the
  reference JVM's `Math.log`/`exp` answer `StrictMath`'s bits: over 60,025 inputs
  (random magnitudes across the full exponent range, subnormals, the
  thresholds, the specials) the port and `java.lang.Math` agree on every bit,
  where Rust's `f64::ln` differs on 218 of 20,000 and `exp(1.0)` is
  `2.718281828459045` against the JVM's `2.7182818284590455`.
- `kotlin.random.Random(seed)` as the stdlib's `XorWowRandom` — `nextInt`,
  `nextLong`, `nextDouble`, `nextFloat`, `nextBoolean`, `nextBits`, with their
  bounds — `Random.Default`, and `shuffled`/`shuffle`/`random`/`randomOrNull`.
  A seeded program draws the reference's exact sequence.
- `MutableMap.merge`/`compute`/`computeIfAbsent`/`computeIfPresent`.
- `String.CASE_INSENSITIVE_ORDER`, `Comparator.compare(a, b)` on a comparator
  value, and `maxWith`/`minWith`.
- `sortedArray`/`sortedArrayDescending`.

### Still open

`Regex` (no engine dependency yet), a lone surrogate in a string literal, and a
comparator passed to `sortedSetOf`/`sortedMapOf` — the last is a wrong answer and
is recorded with its measurement in [BUGS.md](BUGS.md).

## Round 14 — `Regex`, a comparator the sorted collections keep, and a captured `var` that is shared

Measured against `kotlinc` 2.4.20 on the ambient JRE 17.0.4.1. The `Matcher`
diagnostics quoted below are the same strings in the JDK 27 sources
(`java.util.regex.Matcher.appendExpandedReplacement`, `Matcher.find(int)`).

### Wrong answers, now right

| program | before | reference |
| --- | --- | --- |
| `var x = 1; val f = { x }; x = 2; println(f())` | `1` | `2` |
| `println(sortedSetOf(String.CASE_INSENSITIVE_ORDER, "b", "B", "a"))` | `[(lambda arity=2), B, a, b]` | `[a, b]` |
| `println(sortedMapOf(String.CASE_INSENSITIVE_ORDER, "b" to 1, "B" to 2))` | `{B=2, b=1}` | `{b=2}` |

A lambda or local `fun` receives its captures by value, and only a `var` the
lambda itself ASSIGNED was moved into a shared cell. Kotlin shares every
captured `var` that is modified anywhere, so a lambda that only reads one saw
the value at capture time. The cell now covers any `var` a lambda or local `fun`
mentions and the frame writes; a `for` variable is still fresh per iteration.

A sorted collection now keeps the `Comparator` it was built with
(`sortedSetOf(cmp, …)`, `sortedMapOf(cmp, …)`, `TreeSet(cmp)`, `TreeMap(cmp)`):
it orders by it, and — as `java.util.TreeMap` does — identifies elements by it,
so `add`/`put`/`contains`/`get`/`remove` treat two elements the comparator calls
equal as one. A later `put` keeps the first key's spelling and takes the new
value (`{a=0, Key=2}` after `Key`, then `KEY`).

### Missing surface, now present

- `kotlin.text.Regex` on the `fancy-regex` engine (look-around,
  back-references, named groups): `Regex(p)`/`toRegex()`, `find`/`findAll`
  (with `startIndex`), `matchEntire`/`matches`/`containsMatchIn`,
  `replace`/`replaceFirst` with a replacement string or a transform lambda,
  `split` (with `limit`), and `MatchResult`'s `value`/`range`/`groupValues`/
  `groups` (by index or group name)/`destructured`/`next()`; `String.replace`/`replaceFirst`/`split`/
  `matches`/`contains` with a `Regex` argument. `src/regex.rs` adapts the three
  places `java.util.regex.Matcher` defines behaviour of its own: empty-match
  iteration (`Regex("a*").findAll("baaa")` ranges `[0..-1, 1..3, 4..3]`),
  whole-input matching that backtracks into a longer alternative (`a|ab`
  matches `ab`), and `appendReplacement`'s greedy `$n`, `${name}` and `\`
  rules with its exact diagnostics (`Illegal group reference: group index is
  missing`, `named capturing group has 0 length name`, `No group 9`, …).
- `lateinit var` at the top level and on a local, raising on a read before the
  first write from a function, a lambda, or a local `fun`, with
  `::top.isInitialized` on a top-level one.

### Still open

`RegexOption`, `Regex.escape`, and the infix spelling
`input matches regex` (infix calls do not parse). A pattern the engine refuses
raises `java.util.regex.PatternSyntaxException` with the Rust engine's
description; see [BUGS.md](BUGS.md).

## Round 15 — the common spellings that stopped a program

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1 through `scripts/capture-parity.sh`.
Each item below was a parse error or an `unresolved reference` at the start of
the round; eleven corpus records cover them, and every one of the eleven failed
on the previous release and passes now.

- **Raw strings.** `"""…"""` lexed as an empty string followed by another
  literal. It now takes no escapes, spans lines, keeps `$` templates, and closes
  at the LAST three quotes of a run. `trimIndent`, `trimMargin`,
  `replaceIndent` and `replaceIndentByMargin` are ports of `Indent.kt`;
  `lines()` also splits at a lone `\r`, which it did not.
- **Property accessors with a backing field.** `var x = 0 get() = … set(v) { field
  = … }` was a parse error. It desugars to a delegated property over a holder
  for `field`, with the accessor bodies compiled as methods of the owner.
- **Object expressions.** `object : T { … }` was a parse error. It is an
  anonymous class constructed fresh per evaluation; it does not capture locals.
- **`typealias`**, plain and generic, expanded before parsing.
- **`fun interface`**: the SAM constructor, and a lambda or function reference
  passed where the interface is expected.
- **Operator extensions** on a user class and on `String`. Resolving them
  exposed a wrong answer: an extension lookup for a receiver of KNOWN type fell
  back to the sole program-wide extension of that name, so `v.compareTo(o.v)`
  inside `operator fun Node.compareTo` resolved `Int.compareTo` to the `Node`
  extension. A known receiver now only falls back to an extension on a type the
  frontend cannot name.
- **`ArrayDeque`**, with its own wording: `ArrayDeque is empty.` and
  `index: 5, size: 2`, and `kotlin.collections.ArrayDeque` as its class.
- **`MutableList.removeFirst`/`removeLast`** and the `OrNull` pair. On JDK 21
  the throwing pair resolves to `java.util.List`'s own members, so an empty list
  faults with a messageless `java.util.NoSuchElementException`, not the
  extension's `List is empty.`
- **Collection members:** `clear()` on a list or set, `removeIf`/
  `removeAll { }`/`retainAll { }`, `replaceAll` on a list and a map, `sortWith`,
  `set(i, v)` (answering the replaced element), `lastIndexOf`, `fill` (with
  `Arrays.fill`'s range faults), `addFirst`/`addLast`, `contentEquals`,
  `elementAtOrElse`, `concatToString` and `String(chars)`,
  `toSortedMap(comparator)`, `naturalOrder()`/`reverseOrder()`,
  `Int.digitToChar(radix)`. The in-place mutators print `kotlin.Unit`.
- A bare `apply { }`/`also { }`/`let { }` inside a member applies to `this`.

## Round 16 — a member that shadows a stdlib name, and the spellings that still stopped a program

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1 through `scripts/capture-parity.sh`.
The baseline was a clean corpus replay and a clean 480-probe `parity-fuzz` run
(seed 4242001), so the round worked from 272 hand-written one-line probes compared one
by one against the reference, then from 161 whole small programs in five
more batches. Thirty-eight corpus records (1050 → 1088) cover what changed; each
fails on the round's starting commit (`9ffc7c959b`) and passes now.

- **A user method named like a stdlib extension was never called.** A class
  declaring `fun <R> map(f: (T) -> R)` — or `filter`, `let`, `thenBy`, any of
  the names the collection, `Result` and scope-function lowerings intercept —
  had the call routed to the stdlib lowering instead, which answered
  `unresolved reference: map on Box`. A member of the receiver's static class
  now wins over all of them, as it does in Kotlin.
- **A settable property with no backing field.** `var f: Double get() = …
  set(v) { … }` with no initializer was refused, and the accessors could not be
  separated by `;` on one line. Both now go through the backing-field
  desugaring, whose holder simply goes unread.
- **The operator conventions in method form.** `5.inc()`, `x.dec()`,
  `y.unaryMinus()`, `unaryPlus()`, `flag.not()`, `1.rangeTo(3)`,
  `1.rangeUntil(3)` lower to the operator node itself, so the 32-bit `Int` wrap
  and `Char` arithmetic agree with the symbol; `Int::inc`, `Char::inc`,
  `Boolean::not` and friends work as references.
- **`Char(code)`**, with the constructor's own range check
  (`IllegalArgumentException: Invalid Char code: 70000`) where `toChar()`
  truncates; **`emptyArray<T>()`**; **`reversedArray()`**.
- **`Pair.copy` / `Triple.copy`**, positional or named, on a typed receiver and
  inside an untyped lambda (`xs.map { it.copy(second = …) }`).
- **`use { }`** on a class implementing `AutoCloseable`/`Closeable`: the block's
  result, `close()` after it whether it returned or threw.
- **`java.lang.Character` statics** (`isDigit`, `isLetter`, `toUpperCase`, …;
  `isWhitespace` keeps the JDK's exclusion of the three non-breaking spaces that
  Kotlin's member includes) and the **box-class constants** `Integer.MAX_VALUE`,
  `java.lang.Long.MIN_VALUE`, `Character.MAX_VALUE`, typed so that
  `Integer.MAX_VALUE + 1` wraps.
- **`inner class`** was refused. It is now hoisted like any nested class, with
  the enclosing instance as a hidden first constructor property: built on an
  instance (`outer.In(…)`, or a bare `In(…)` inside the outer class, which
  passes `this`), reading and writing the outer class's members unqualified from
  its methods, lambdas, property initializers and `init` blocks, nesting more
  than one level deep, and naming the outer instance `this@Outer`. Two more
  corpus records cover it.

Wrong answers and refusals found by a third probe batch, each byte-checked
against the reference and covered by one of five more corpus records:

- **`b += x` on a `val` collection** was `val cannot be reassigned` whenever
  the frontend had not tracked the binding's type (`val b =
  a.toMutableList()`), and always for a class property or a top-level `val`. A
  `val` of any non-primitive type now takes the `plusAssign`/`minusAssign`
  convention, as Kotlin resolves it; a `val` `Int` or `String` is still the
  reassignment error.
- **`String.slice(…)` answered a `List<Char>`** (`[o, t, l]` for `otl`). It
  answers a `String`: an `IntRange` is `substring`, with its
  `StringIndexOutOfBoundsException`, and any other index sequence picks code
  units one at a time.
- **Smart casts to a primitive.** `if (x is Int) x + 1`, a `when (x) { is
  Double -> x * 2 }` arm, a subjectless `when { x is Int -> … }` arm and the
  right operand of `x is Int && x > 3` (or `x !is Int || …`) now see `x` as the
  tested type; arithmetic on an `Any` there was `unresolved reference: plus on
  value`. A subjectless `when` arm written `x is T ->` no longer parses as the
  next subject-form arm.
- **`2.5 is Float` was `true`.** A bare floating value is a `Double`; only a
  boxed `Float` answers `is Float`, and a `when` subject whose static type is
  `Float` or `Long` is now boxed for its `is` arms as an `is` operand already
  was.
- **`(1..Int.MAX_VALUE).asSequence()`** snapshotted every element before the
  first `take` could stop it, and timed out. A range's `asSequence()` is now a
  lazy pipeline stepping the range on demand.
- **`kotlin.math`'s trigonometric, hyperbolic and power functions.** `tan`,
  `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `cbrt`, `hypot` and
  `expm1` were `unresolved reference`, and `pow` answered Rust's `powf`, which
  differs from the JVM in the last place on about one input in thirty-six and
  answered `1.0` for `1.0.pow(Double.POSITIVE_INFINITY)` where the JVM answers
  `NaN`. All twelve are now ports of fdlibm's routines as JDK 21's `StrictMath`
  runs them — `tan` with the full `pi/2` reduction for huge arguments — which
  the reference JVM's `Math` answers bit for bit: zero differences over 250 000
  inputs each. `Float.pow` narrows once to a `Float`. `sin`/`cos` stay out:
  BUGS.md has the measurement.
- **A bare `return` inside a lambda returned from the lambda.** In Kotlin it is
  a NON-LOCAL return out of the enclosing function (the lambda having been
  inlined into it): `xs.forEach { if (it == 2) return 10; println(it) }`
  printed `1`, `3` and fell through where the reference prints `1` and returns
  `10`. It now raises a marker the exception unwind carries out of the lambda,
  through `forEach`/`map`/`let`/`apply`/`repeat`/`runCatching`/a nested
  lambda and any `finally`, to the frame boundary of the function it names,
  which returns the carried value. No `catch` — not even `catch (e:
  Throwable)` — and no `runCatching` intercepts it. `return xs.map { … return
  … }` returns the non-local value, not the half-built list.
- **Anonymous functions**, `fun(x: Int): Int { … }` and `fun(x: Int) = …`, were
  a parse error. A bare `return` in one leaves it; a block body answers `Unit`.
- **The width-dependent bit members** `countOneBits`/`countLeadingZeroBits`/
  `countTrailingZeroBits`/`takeHighestOneBit`/`takeLowestOneBit`/`rotateLeft`/
  `rotateRight` on `Int` and `Long`, and the `java.lang.Integer`/`Long` statics
  that are the same operations (`bitCount`, `numberOfLeadingZeros`,
  `highestOneBit`, …), plus `Integer.toString(i[, radix])`,
  `Integer.parseInt(s, radix)`, `Integer.compare`/`max`/`min`/`sum`.
- **`copyOf()`/`copyOf(n)`/`copyOfRange(from, to)`** on every array kind,
  padding with the kind's default and raising the stdlib's, `Arrays`' and
  `System.arraycopy`'s faults in the reference's order.

Six more corpus records cover the last four items.

A fourth probe batch, of whole small programs rather than one-liners, found
these; eight more corpus records cover them:

- **`val x: Long = 7L; x / 2` answered `0.0`.** A `Long`-annotated binding
  stored its value BOXED (the box is for erased positions such as `Any`), and
  `/` and `%` read the box's handle as a number. The binding no longer boxes,
  and `/`/`%` — which, unlike `+`, bypassed the operators' box handling —
  divide a boxed `Long` (a parameter, a field, a collection element)
  integrally, faulting on zero.
- **`a + b` on a user class with no static type** — `xs.fold(P(0)) { a, b ->
  a + b }` — was `unresolved reference: plus on P`. The arithmetic operator
  conventions are now published per class and dispatched at run time, with
  numbers, `Char`s and `String`s keeping their own arithmetic there.
- **`m[i, j]` / `m[i, j] = v` / `m[i, j] += v`**, the `get`/`set` operators
  with several indices, were a parse error.
- **A property named `data`** (`data[r][c] = v`) parsed as the start of a
  `data class` declaration.
- **`it` inside a `() -> R` lambda** (`runCatching { … }`, `run { … }`,
  `lazy`, `getOrPut`, `ifEmpty`, `require`/`check` messages) was a fresh,
  unbound implicit parameter; it is the enclosing lambda's `it`, as in Kotlin.
- **`filterIsInstance<T>()`**, **`toSortedSet()`**, the primitive-array
  conversions **`toIntArray()`** … **`toByteArray()`**, a **`vararg` that is
  not the last parameter**, and the `Result` members `onSuccess`/`onFailure`/
  `getOrElse`/`map` on a receiver the compiler could not type.

From a fifth and sixth probe batch, four more corpus records:

- **`StringBuilder.replace(regex, replacement)`** answered the builder
  unchanged: only the three-argument `replace(start, end, str)` is the
  builder's own; the two-argument forms are the `CharSequence` extensions and
  answer a new `String`.
- **`groupBy(keySelector, valueTransform)` / `associateBy(keySelector,
  valueTransform)`** used the TRANSFORM as the key.
- **`x?.let { println(it) } ?: other`** ran `other` for a non-null `x`: the
  block's `Unit` and `null` are one runtime value. A safe `let`/`run` whose
  block ends in a statically `Unit` statement now decides the elvis by the
  receiver.
- **`java.util.PriorityQueue`** — natural order, a comparator lambda or
  `compareBy` chain, a source collection (heapified), `add`/`offer`/`addAll`,
  `poll`/`remove()`/`peek`/`element`, `remove(x)` — a port of the JDK's
  `siftUp`/`siftDown`/`removeAt`/`heapify`, so iteration and `toString` show
  the same heap order. Resolved under `import java.util.PriorityQueue` /
  `java.util.*` or written qualified.
- **`for (x in obj)` over a user class** taking part in the iterator protocol
  — one declaring `operator fun iterator()`, or one implementing `Iterator`
  (now accepted as a supertype) — was `for-in over a non-iterable value`. It
  is lowered to Kotlin's `iterator()`/`hasNext()`/`next()` loop, with
  destructuring, labels, `break` and `continue`.
- **`java.util.Random`** (imported or qualified): the JDK's 48-bit LCG, with
  `nextInt()`/`nextInt(bound)` (its rejection loop included)/`nextLong`/
  `nextDouble`/`nextFloat`/`nextBoolean`, and as the generator
  `shuffle`/`shuffled` draw from — the same order `Collections.shuffle`
  produces. Three more corpus records.
- **An overridden property read through the base answered the base's
  value.** `open val kind = "vehicle"` overridden by `override val kind =
  "bike"` printed `vehicle` from the base's own methods and even from
  `Bike().kind`: the record holds the base's field first and the subclass's
  after it, and every read took the first. Reads and writes now take the
  most-derived field. An override by a GETTER (`override val kind: String
  get() = …`) was refused as overriding nothing; it is accepted, and a read
  through the base dispatches on the runtime class. Two more corpus records.

## Round 17 — virtual properties both ways, `data` members over a supertype, UTF-16 order

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1. Each item below was a silent
wrong answer or a stopped program; ten corpus records cover them, captured with
`scripts/capture-parity.sh`.

- **A stored `override val` over a GETTER** (`open val g: String get() = "G"`
  overridden by `override val g = "H"`, or an interface's default getter
  overridden by an implementor's stored property) still dispatched to the
  getter: the subclass inherited the getter as a method and the dispatch index
  sent every read there. A stored property now shadows a supertype getter of
  the same name, and a call site whose static class has such a subtype keeps
  the runtime class test with its field-read fallback.
- **A `data class` under a supertype that overrides `toString`/`equals`/
  `hashCode`** inherited them. Kotlin regenerates the three unless the
  supertype's is `final`; `final override fun` is now parsed as such, kept by
  a data subclass, and refused as an override target.
- **`class Sub(x: Int) : Base(x * 2) { val y = x }`** read the base's stored
  `x` instead of the constructor parameter: an inherited property that the
  subclass shadows with a parameter or its own property is no longer bound
  over it in the constructor.
- **`String` `<`/`>`/`<=`/`>=`, `sorted`, `maxOrNull`, `sortedSetOf`**
  compared UTF-8 bytes; they compare UTF-16 code units, as `String.compareTo`
  does (`"￿" < "😀"` is `false`).
- **`"%x"`/`"%o"`/`"%X"` of a negative `Int`** printed the 64-bit pattern
  (`ffffffffffffffff` for `-1`); an `Int` prints at 32 bits, a `Long` at 64.
- **`slice(range)` on a `List` or an array** faulted with a per-index message;
  it is one `subList`/`copyOfRange` of the span, as the stdlib writes it, and
  raises that call's fault. A per-index `slice` faults as the list's or array's
  own `get` does.
- **`sliceArray`**, **`prependIndent`**, **`Double.mod`**, and
  **`SIZE_BITS`/`SIZE_BYTES`** on every primitive were unresolved.
- **`Long.MIN_VALUE.floorDiv(-1L)`** panicked on overflow; it wraps, as the
  JVM's `long` division does.

### Round 17, second batch

Ten more corpus records, each from a probe diffed against `kotlinc`:

- **`when (x) { in xs -> … }`** accepted only a literal `a..b`/`until`/
  `downTo`; any container is now accepted (a collection, a string, a range
  value, `1..9 step 2`, a type declaring `operator fun contains`).
- **An `in`/`is` opening a line** was read as an operator on the previous
  line's expression, so a `when` arm `in listOf(…) ->` after an expression arm
  was a parse error. Kotlin's grammar admits a newline only after `in`/`is`.
- **`Formatter`**: `%g`/`%G`, `%a`/`%A` (no precision), the `(` and `#`
  flags; `%e` now rounds HALF_UP over the shortest decimal form as `%f`
  already did (`"%.0e".format(2.5)` is `3e+00`), and prints `NaN`/`Infinity`.
- **`"%b %b".format(null, "x")`** resolves to the `format(locale, …)`
  overload, as Kotlin does, so the `null` is the locale.
- **A `Long` lambda result** is boxed like every erased `Long`
  (`"%x".format({ -1L }())` is sixteen `f`s), and `==`/`!=` that reaches
  object equality boxes a statically `Long` operand, so `{ -1L }() == -1L`
  is `true`.
- **`ByteArray`/`ShortArray`/`LongArray`/`FloatArray`** constructors and
  `byteArrayOf`/`shortArrayOf`; `String.toByteArray()`/`encodeToByteArray()`,
  `ByteArray.decodeToString()` and `String(bytes)` (UTF-8).

## Round 18 — user infix calls, delegates outside a class

Each record below was captured with `scripts/capture-parity.sh` (kotlinc 2.4.20
on JRE 21.0.12.1) from a probe that diverged:

- **`a pow b` over a user `infix fun`** was a parse error; any `infix fun` the
  program declares (member or extension, before or after its use) is now an
  infix call when its name sits on the left operand's line.
- **`fun Int.pow(…)`** was `unresolved reference: pow` without a
  `kotlin.math` import: the import gate for the math extensions now lets a
  program's own extension of that name resolve.
- **A top-level `var x by D()`** stored the delegate and never called
  `getValue`/`setValue` — `x = 5; x + 1` printed `6` with no accessor run.
  Reads and writes now go through the delegate with a null `thisRef`.
- **A local or top-level delegate's `property`** was null, so `property.name`
  failed to compile; it is the property's `KProperty`, as a class property's
  already was, and `Delegates.observable`/`vetoable` handlers receive it too.
- **A top-level `by Delegates.observable`/`vetoable`** silently became a plain
  variable: the handler never ran and the veto never applied.

### Round 18, second batch

- **`Map.count()`, `Map.asSequence()`, `Map.asIterable()`** were unresolved;
  they walk the entries as the `kotlin.collections` extensions on `Map` do.
- **The no-predicate `any()`/`none()`** were unresolved on every collection,
  array, range, sequence and `Map`.
- **`arrayOfNulls<T>(n)`** was unresolved.
- **`contentDeepToString()`** was unresolved; it is `Arrays.deepToString`,
  nested primitive arrays and the `[...]` self-reference included.
- **`fun ((Int) -> Int).name(…)`** — an extension on a function type — was a
  parse error, and so was an `infix fun` declared on one.
- **`{ f: (Int) -> Int, x: Int -> … }`** — a lambda parameter annotated with a
  function type — ended the parameter list at the type's own arrow.

### Round 18, third batch

- **`Result` members after a `map`** (`runCatching { … }.map { … }.getOrElse
  { … }`) lost the receiver's type and resolved as collection members, which
  answered `unresolved reference: getOrElse on Result`.
- **`Result.fold(onSuccess, onFailure)`** and **`Result.recover { }`** were
  unresolved.
- **`%h`/`%H`** were `UnknownFormatConversionException`; they are
  `Integer.toHexString(arg.hashCode())`, a user `hashCode` included, with the
  precision capping the length.

### Round 19, first batch

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

- **`return@label` naming an enclosing lambda** — `run outer@{ xs.forEach {
  return@outer "early" }; "late" }` — returned from the INNER lambda only, so the
  program silently answered `late` where the reference answers `early`. The
  label now resolves against every enclosing lambda up to the nearest `fun`,
  explicit labels and the implicit one a trailing lambda takes from its callee
  (`return@run`, `return@map`, `return@with`) alike; the return unwinds to the
  named lambda's frame, which claims it.
- **A nullable extension receiver** — `fun String?.safeLen() = this?.length
  ?: 0`, `val String?.lenOr get() = …`, `fun <T> List<T>?.sizeOrMinus()` — was
  a parse error (`expected LParen, found Question`). It parses, and the
  extension is called on `null` with `this` bound to it.
- **A generic extension property** — `val <T> List<T>.second: T get() =
  this[1]` — was `expected identifier, found Lt`.
- **A body property assigned in `init`** — `class C(val a: Int) { val b: Int;
  init { b = a * 2 } }` — was `unresolved reference: b`: with no initializer,
  the declaration was taken for an abstract one with no storage. In a concrete
  class it is now stored, holds the JVM default until the `init` block writes
  it, and stays a `val` to every write outside the constructor.
- **`Map.Entry.toPair()`** was `unresolved reference: toPair on Map.Entry`.
- **`kotlin.math.truncate`** was unresolved; it rounds toward zero, `Float`
  overload included, and is import-gated like the rest of `kotlin.math`.
- **`Int` totals did not wrap.** `listOf(Int.MAX_VALUE, 1).sum()` answered
  `2147483648` where the reference answers `-2147483648`, and so did `sumOf`
  with an `Int` selector, an `IntArray`, a `List<Int>` parameter and a `map`
  result. The width now comes from the receiver's static element type (an
  annotation `List<Int>` included) and from the selector's result type for
  `sumOf`. **`longArrayOf(Long.MAX_VALUE, 1).sum()` panicked** with `attempt to
  add with overflow`; integral totals now wrap at 64 bits as the JVM's `long`
  addition does.
- **`"abcdef".substring(2..4)`** answered the whole string; the `IntRange`
  overload now reads `start` and `endInclusive`.
- **`"abc".contentEquals("abc")`** answered `false` — the array member compared
  the string as a non-array. `CharSequence.contentEquals` now compares
  characters against a `String` or `StringBuilder`, with `ignoreCase`, and a
  `null` receiver equals only `null`.
- **`String.replaceRange`/`removeRange`** were unresolved. Both overloads are
  ported with the stdlib's fault sequence: the order check, `removeRange`'s
  `NegativeArraySizeException` from the builder capacity, then the JDK's
  `Range [a, b) out of bounds` from each copy.
- **`zip`, `contentEquals` and `matches` in infix spelling** (`a zip b`,
  `x contentEquals y`, `s matches re`) were parse errors.
- **A user `fun` named like a stdlib one** — `fun check(a: Int)`, then
  `check(5)` or `xs.map(::check)` — ran the stdlib's `check` and died with
  `IllegalStateException: Check failed.` A file's own top-level and local
  functions now take precedence whenever their parameter list accepts the
  call, as Kotlin resolves its own package before the default imports.
- **A member reached through an untracked smart cast** — `fun act(a: Animal) =
  when (a) { is Dog -> a.fetch() … }`, or `if (x is Dog) x.fetch()` with `x:
  Any` — was `unresolved reference: fetch on Dog` at run time: the receiver's
  static class (`Animal`, `Any`) lacked the member, so no candidate was tried.
  It now dispatches over the subtypes that declare it.
- **`Comparator { a, b -> … }`** was `unresolved reference: Comparator`.

## Round 20 — object expressions close over their function, and the gaps round 19 measured

Measured on `kotlinc` 2.4.20 / JRE 21.0.12.1.

- **An object expression capturing a local** — `val r = 2.0; object : Shape {
  override fun area() = r * r }` — was `unresolved reference: r`: the
  expression is hoisted to a top-level class and saw nothing of its function.
  Each local the body names now travels as a hidden constructor property, typed
  as the construction site saw it. A `var` written on either side
  (`var n = 0; object { fun inc() { n++ } }`, a counter returned from a
  factory) is passed as its heap cell, the same sharing a lambda's write gets,
  so the frame and the object see each other's writes. An object built inside
  another object's member forwards what it needs through the outer one's
  captures, and one built in a method copies the class's read-only properties.
- **`xs.map { v -> object : F { … } }`** was a parse error: an unnamed `object`
  at the start of a statement was read as a declaration.
- **A user `Comparator`** — `object : Comparator<String> { override fun
  compare(a: String, b: String) = … }`, a named `class ByLen : Comparator<…>`
  or an `object ByAge : Comparator<P>` — was `unresolved supertype Comparator`.
  `Comparator` is now an accepted supertype whose abstract member is
  `compare(a, b)`, each implementing class publishes that member to the
  runtime, and every comparator consumer (`sortedWith`, `sortWith`,
  `maxWith`/`minWith`, `sortedSetOf(cmp, …)`, `PriorityQueue(cmp)`) calls it.
- **`cmp.thenBy { … }` over a comparator that is not a `compareBy` chain** —
  `Comparator { x, y -> x.length - y.length }.thenBy { it }` — silently DROPPED
  the base and sorted by the tiebreak key alone (`[a, ab, bb, c]` for the
  reference's `[a, c, ab, bb]`). A whole comparator (a two-parameter lambda, a
  user `Comparator`, a chain) is now a step of the chain that compares the pair
  itself.
- **`Comparator` composition** — `reversed()`, `then(cmp)`,
  `thenComparing(cmp)` and `thenDescending(cmp)` were `unresolved reference`
  on every comparator. Each answers a new comparator; a reversed step swaps the
  operands, as the JDK's `Collections.reverseOrder(cmp)` does. `nullsFirst` /
  `nullsLast` (with and without a comparator) are ported from the stdlib's
  `when` over `===`/`null`, and `Comparator.naturalOrder()` /
  `Comparator.reverseOrder()` resolve to the top-level functions.
- **The `NavigableMap`/`NavigableSet` members of a sorted collection** —
  `sortedMapOf(…).firstKey()`, `TreeMap.floorKey`/`headMap`/`pollFirstEntry`,
  `TreeSet.floor`/`headSet`/`pollFirst` and the rest — were `unresolved
  reference … on Map`/`on Set`. They are ported from `java.util.TreeMap` and
  `TreeSet`, under the collection's comparator when it has one: `firstKey` on
  an empty map raises `NoSuchElementException`, the `Entry`/`floor` forms
  answer `null`, `subMap(from > to)` raises `IllegalArgumentException:
  fromKey > toKey`, and the range members honour their inclusive flags. The
  ranges are snapshots, as every collection view here is (see BUGS).
- **`java.util.TreeMap<K, V>()`** and the other JDK collection constructors in
  their fully qualified spelling were `unresolved reference: java`.
- **A user method named like a collection extension, called on a call's
  result** — `Box(3).map { … }.map { … }` on a `class Box` declaring `map` —
  was `unresolved reference: map on Box`: the static class of the first call
  was taken from the stdlib's `map` (a `List`), so the second call never looked
  at `Box`. A member the receiver's own class declares now decides the result's
  class, as it already decided the call.
- **A lazy sequence's `forEach` ran after the whole pipeline** —
  `asSequence().map { println("map $it") }.forEach { println("each $it") }`
  printed every `map` line before the first `each`, where Kotlin pushes each
  element through to the terminal before pulling the next. `forEach`,
  `forEachIndexed`, `count { … }`, `all` and `none` now consume elements as
  they leave the pipeline (`all`/`none` stopping at the deciding one), and
  `onEach` is the lazy stage Kotlin's `Sequence.onEach` is rather than a
  materializing one.
- **`toHashSet()` iterated in insertion order** — `listOf("banana", "apple",
  "cherry", "zebra").toHashSet()` printed `[banana, apple, cherry, zebra]` where
  the reference's `java.util.HashSet` prints `[banana, zebra, apple, cherry]`.
  It now carries the bucket order, from the capacity the stdlib requests for
  each receiver kind (`mapCapacity(size)` for a collection or array,
  `mapCapacity(12)` for a range, the default for a `Sequence`).
- **`java.util.stream` pipelines** — `listOf(1, 2, 3).stream()…` was
  `unresolved reference: stream on List`. A pipeline from `coll.stream()`,
  `Stream.of(…)` or `IntStream.range`/`rangeClosed` is lowered onto the lazy
  sequence (the Java names renamed to the sequence operations they are), so
  stage side effects interleave exactly as the JDK's do. Two JDK behaviours are
  ported rather than inherited from the sequence: `count()` over a pipeline
  that stays `SIZED` (`map`, `peek`, `sorted`, `limit`, `skip`, …) answers from
  the source size without running a stage, and `DoubleStream.sum()` is
  `Collectors.sumWithCompensation` + `computeFinalSum` (`0.6` for
  `0.1 + 0.2 + 0.3`, where a plain fold gives `0.6000000000000001`). The
  `Optional`-returning terminals are refused, because the sequence member of
  the same name would answer the bare value.
- **`is Number` / `is Comparable<*>`** answered `false` for every value
  (`(3.0 as Any) is Number`). `Number` now holds for the numeric types and
  `Comparable` for the numbers, `String`, `Char`, `Boolean`, enum constants,
  `StringBuilder` and a user class declaring it (directly or through an
  ancestor); `as Number` casts accordingly. **`(1..2) is Collection<*>`**
  answered `true` — a range is an `Iterable` only. A star-projected call type
  argument (`filterIsInstance<Comparable<*>>()`) was a parse error.
